//! Built-in TypeScript codegen target.
//!
//! Introspects the live SQLite schema and emits a `.ts` file with
//! `Enums`, `Tables`, and utility types matching the Supabase
//! `gen types typescript` convention (SPEC §7).

use std::collections::BTreeMap;
use std::fs;

use rusqlite::Connection;
use sqlparser::ast::{Expr, Statement, TableConstraint, Value};
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;
use tracing::warn;

use super::{CodegenConfig, CodegenError, CodegenTarget, GenerateOutput};

// ---------------------------------------------------------------------------
// Public target
// ---------------------------------------------------------------------------

pub struct TypeScriptTarget;

impl CodegenTarget for TypeScriptTarget {
    fn kind(&self) -> &'static str {
        "typescript"
    }

    fn generate(
        &self,
        conn: &Connection,
        config: &CodegenConfig,
    ) -> Result<GenerateOutput, CodegenError> {
        let raw_tables = list_tables(conn)?;
        let tables: Vec<_> = raw_tables
            .into_iter()
            .filter(|t| !is_excluded(&t.name, &config.exclude))
            .collect();

        let mut table_infos = Vec::with_capacity(tables.len());
        let mut all_enums: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for table in &tables {
            let columns = get_columns(conn, &table.name)?;
            let parsed = parse_create_table(table.sql.as_deref());
            let rowid_alias = detect_rowid_alias(&columns, parsed.without_rowid);

            for (col_name, values) in &parsed.enums {
                all_enums.insert(format!("{}_{}", table.name, col_name), values.clone());
            }

            table_infos.push(TableInfo {
                name: table.name.clone(),
                columns,
                rowid_alias,
            });
        }

        let output = render(&table_infos, &all_enums);

        if let Some(parent) = config.path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&config.path, &output)?;

        Ok(GenerateOutput {
            path: config.path.clone(),
            bytes_written: output.len() as u64,
        })
    }
}

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

struct TableEntry {
    name: String,
    sql: Option<String>,
}

struct ColumnInfo {
    name: String,
    declared_type: String,
    notnull: bool,
    default_value: Option<String>,
    pk: i32,
}

struct TableInfo {
    name: String,
    columns: Vec<ColumnInfo>,
    rowid_alias: Option<String>,
}

// ---------------------------------------------------------------------------
// Introspection
// ---------------------------------------------------------------------------

fn list_tables(conn: &Connection) -> Result<Vec<TableEntry>, CodegenError> {
    let mut stmt = conn
        .prepare("SELECT name, sql FROM sqlite_master WHERE type = 'table' ORDER BY name")
        .map_err(|e| CodegenError::Target(e.to_string()))?;

    let entries = stmt
        .query_map([], |row| {
            Ok(TableEntry {
                name: row.get(0)?,
                sql: row.get(1)?,
            })
        })
        .map_err(|e| CodegenError::Target(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CodegenError::Target(e.to_string()))?;

    Ok(entries)
}

fn get_columns(conn: &Connection, table: &str) -> Result<Vec<ColumnInfo>, CodegenError> {
    let mut stmt = conn
        .prepare("SELECT * FROM pragma_table_info(?)")
        .map_err(|e| CodegenError::Target(format!("failed to query columns for {table}: {e}")))?;

    let columns = stmt
        .query_map([table], |row| {
            Ok(ColumnInfo {
                name: row.get(1)?,
                declared_type: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                notnull: row.get::<_, i32>(3)? != 0,
                default_value: row.get(4)?,
                pk: row.get(5)?,
            })
        })
        .map_err(|e| CodegenError::Target(format!("failed to read columns for {table}: {e}")))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CodegenError::Target(format!("failed to read column row for {table}: {e}")))?;

    Ok(columns)
}

// ---------------------------------------------------------------------------
// CHECK-IN enum extraction
// ---------------------------------------------------------------------------

/// Parsed information from a CREATE TABLE statement.
struct ParsedCreateTable {
    enums: BTreeMap<String, Vec<String>>,
    without_rowid: bool,
}

/// Parse a CREATE TABLE statement to extract CHECK-IN constraints and
/// the `WITHOUT ROWID` flag.
fn parse_create_table(sql: Option<&str>) -> ParsedCreateTable {
    let sql = match sql {
        Some(s) => s,
        None => {
            return ParsedCreateTable {
                enums: BTreeMap::new(),
                without_rowid: false,
            };
        }
    };

    let stmts = match Parser::parse_sql(&SQLiteDialect {}, sql) {
        Ok(stmts) => stmts,
        Err(_) => {
            // Fallback: if sqlparser can't parse the DDL, preserve the old
            // string-based WITHOUT ROWID detection to avoid regressions on
            // SQLite syntax the parser doesn't understand.
            let without_rowid = sql.to_ascii_uppercase().contains("WITHOUT ROWID");
            return ParsedCreateTable {
                enums: BTreeMap::new(),
                without_rowid,
            };
        }
    };

    let mut enums = BTreeMap::new();
    let mut without_rowid = false;

    for stmt in &stmts {
        let Statement::CreateTable(create) = stmt else {
            continue;
        };

        without_rowid |= create.without_rowid;

        // Check table-level constraints.
        for constraint in &create.constraints {
            if let Some((col, values)) = extract_in_list_from_check(constraint)
                && !values.is_empty()
            {
                enums.insert(col, values);
            }
        }

        // Check column-level constraints.
        for col_def in &create.columns {
            for opt_def in &col_def.options {
                if let sqlparser::ast::ColumnOption::Check(check) = &opt_def.option
                    && let Some((col, values)) = extract_in_list_values(check)
                    && !values.is_empty()
                {
                    enums.insert(col, values);
                }
            }
        }
    }

    ParsedCreateTable {
        enums,
        without_rowid,
    }
}

/// Try to extract an IN-list from a table-level CHECK constraint.
fn extract_in_list_from_check(constraint: &TableConstraint) -> Option<(String, Vec<String>)> {
    let TableConstraint::Check(check) = constraint else {
        return None;
    };
    extract_in_list_values(check)
}

/// Try to extract column name and string literal values from a CHECK constraint
/// containing a simple `col IN ('a', 'b')` expression.
fn extract_in_list_values(
    check: &sqlparser::ast::CheckConstraint,
) -> Option<(String, Vec<String>)> {
    let Expr::InList {
        expr: box_expr,
        list,
        negated: false,
    } = check.expr.as_ref()
    else {
        return None;
    };

    // Extract column name from identifier (possibly quoted).
    let col = match box_expr.as_ref() {
        Expr::Identifier(ident) => ident.value.clone(),
        _ => return None,
    };

    // Extract string literal values.
    let values: Vec<String> = list
        .iter()
        .filter_map(|e| match e {
            Expr::Value(vws) => match &vws.value {
                Value::SingleQuotedString(s) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();

    if values.is_empty() {
        None
    } else {
        Some((col, values))
    }
}

// ---------------------------------------------------------------------------
// Rowid alias detection
// ---------------------------------------------------------------------------

/// Detect whether the table has an `INTEGER PRIMARY KEY` rowid alias.
///
/// Returns the alias column name if:
/// - Exactly one column has `pk = 1` and declared type is exactly `INTEGER`.
/// - The table does NOT use `WITHOUT ROWID`.
fn detect_rowid_alias(columns: &[ColumnInfo], without_rowid: bool) -> Option<String> {
    if without_rowid {
        return None;
    }

    let pk_cols: Vec<&ColumnInfo> = columns.iter().filter(|c| c.pk > 0).collect();
    if pk_cols.len() != 1 {
        return None;
    }

    let col = pk_cols[0];
    if col.declared_type.to_uppercase() == "INTEGER" {
        Some(col.name.clone())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Exclusion (SQL LIKE-style glob matching)
// ---------------------------------------------------------------------------

fn is_excluded(name: &str, exclude: &[String]) -> bool {
    // Internal exclusions per SPEC §7.5: always exclude sqlite_ system
    // tables and the schema_migrations tracking table.
    const INTERNAL_EXCLUDE: &[&str] = &["sqlite_%", "schema_migrations"];
    INTERNAL_EXCLUDE
        .iter()
        .any(|pattern| like_match(pattern, name))
        || exclude.iter().any(|pattern| like_match(pattern, name))
}

/// SQL `LIKE`-style matching with `%` (any sequence) and `_` (single char).
///
/// Case-insensitive for ASCII characters, matching SQLite's default LIKE.
fn like_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    like_inner(&p, &t)
}

fn like_inner(pattern: &[char], text: &[char]) -> bool {
    let mut pi = 0;
    let mut ti = 0;
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0;

    while ti < text.len() {
        if pi < pattern.len() && (pattern[pi] == '_' || pattern[pi].eq_ignore_ascii_case(&text[ti]))
        {
            pi += 1;
            ti += 1;
        } else if pi < pattern.len() && pattern[pi] == '%' {
            star_pi = Some(pi);
            star_ti = ti;
            pi += 1;
        } else if let Some(sp) = star_pi {
            pi = sp + 1;
            star_ti += 1;
            ti = star_ti;
        } else {
            return false;
        }
    }

    while pi < pattern.len() && pattern[pi] == '%' {
        pi += 1;
    }

    pi == pattern.len()
}

// ---------------------------------------------------------------------------
// Affinity → TS type
// ---------------------------------------------------------------------------

fn ts_type_for_column(col: &ColumnInfo, table_name: &str) -> String {
    let base = col
        .declared_type
        .split('(')
        .next()
        .unwrap_or("")
        .trim()
        .to_uppercase();

    match base.as_str() {
        "INTEGER" | "INT" | "BIGINT" | "SMALLINT" | "TINYINT" | "MEDIUMINT" | "INT2" | "INT8"
        | "BOOLEAN" | "BOOL" | "REAL" | "DOUBLE" | "FLOAT" | "NUMERIC" | "DECIMAL" => {
            "number".to_string()
        }

        "TEXT" | "VARCHAR" | "CHAR" | "CLOB" | "DATE" | "DATETIME" | "TIME" => "string".to_string(),

        "BLOB" => "Uint8Array".to_string(),

        "" => {
            warn!(
                column = %col.name,
                table = %table_name,
                "column has no declared type, mapping to string"
            );
            "string".to_string()
        }

        other => {
            warn!(
                column = %col.name,
                table = %table_name,
                type = %other,
                "unknown SQLite type, mapping to string"
            );
            "string".to_string()
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

/// Escape a string for safe emission inside a TypeScript string literal.
///
/// Handles backslashes, double quotes, and ASCII control characters.
fn escape_ts_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn render(table_infos: &[TableInfo], all_enums: &BTreeMap<String, Vec<String>>) -> String {
    let mut out = String::new();

    // Header
    out.push_str("// Generated by stig. Do not edit by hand.\n");
    out.push_str(&format!(
        "// Source: {} tables, {} enums.\n",
        table_infos.len(),
        all_enums.len()
    ));
    out.push('\n');

    // Enums
    if !all_enums.is_empty() {
        out.push_str("export type Enums = {\n");
        for (name, values) in all_enums {
            let union = values
                .iter()
                .map(|v| format!("\"{}\"", escape_ts_literal(v)))
                .collect::<Vec<_>>()
                .join(" | ");
            out.push_str(&format!("  \"{}\": {union};\n", escape_ts_literal(name)));
        }
        out.push_str("};\n\n");
    }

    // Tables
    out.push_str("export type Tables = {\n");
    for table in table_infos {
        out.push_str(&format!("  \"{}\": {{\n", escape_ts_literal(&table.name)));

        // Row
        out.push_str("    Row: {\n");
        for col in &table.columns {
            let base = column_type(col, &table.name, all_enums);
            let nullable = nullable_suffix(col, table);
            out.push_str(&format!(
                "      \"{}\": {base}{nullable};\n",
                escape_ts_literal(&col.name)
            ));
        }
        out.push_str("    };\n");

        // Insert
        out.push_str("    Insert: {\n");
        for col in &table.columns {
            let base = column_type(col, &table.name, all_enums);
            let nullable = nullable_suffix(col, table);
            let optional = insert_optional(col, table);
            out.push_str(&format!(
                "      \"{}\"{optional}: {base}{nullable};\n",
                escape_ts_literal(&col.name)
            ));
        }
        out.push_str("    };\n");

        // Update
        out.push_str("    Update: {\n");
        for col in &table.columns {
            let base = column_type(col, &table.name, all_enums);
            let nullable = nullable_suffix(col, table);
            out.push_str(&format!(
                "      \"{}\"?: {base}{nullable};\n",
                escape_ts_literal(&col.name)
            ));
        }
        out.push_str("    };\n");

        out.push_str("  };\n");
    }
    out.push_str("};\n\n");

    // Utility types
    out.push_str("export type TableName = keyof Tables;\n");
    out.push_str("export type Row<T extends TableName>    = Tables[T][\"Row\"];\n");
    out.push_str("export type Insert<T extends TableName> = Tables[T][\"Insert\"];\n");
    out.push_str("export type Update<T extends TableName> = Tables[T][\"Update\"];\n");

    out
}

/// Return `Enums["<table>_<col>"]` if the column has a CHECK-IN constraint,
/// otherwise the affinity-mapped TS type.
fn column_type(
    col: &ColumnInfo,
    table_name: &str,
    enums: &BTreeMap<String, Vec<String>>,
) -> String {
    let enum_key = format!("{table_name}_{}", col.name);
    if enums.contains_key(&enum_key) {
        return format!("Enums[\"{}\"]", escape_ts_literal(&enum_key));
    }
    ts_type_for_column(col, table_name)
}

/// Whether a column is effectively NOT NULL.
///
/// A column is NOT NULL if:
/// - It has an explicit `NOT NULL` constraint, OR
/// - It is a PRIMARY KEY column (`pk > 0`) that is NOT a rowid alias.
///   In SQLite, all PRIMARY KEY columns (except INTEGER PRIMARY KEY
///   rowid aliases) are implicitly NOT NULL, OR
/// - It is an INTEGER PRIMARY KEY rowid alias — while NULL inserts are
///   allowed (auto-assign), the value is never NULL in query results.
fn is_effectively_notnull(col: &ColumnInfo, table: &TableInfo) -> bool {
    if col.notnull {
        return true;
    }
    // Rowid aliases are never NULL in query results.
    if table.rowid_alias.as_deref() == Some(&col.name) {
        return true;
    }
    // Non-rowid-alias PRIMARY KEY columns are implicitly NOT NULL.
    col.pk > 0
}

fn nullable_suffix(col: &ColumnInfo, table: &TableInfo) -> &'static str {
    if is_effectively_notnull(col, table) {
        ""
    } else {
        " | null"
    }
}

fn insert_optional(col: &ColumnInfo, table: &TableInfo) -> &'static str {
    if col.default_value.is_some()
        || !is_effectively_notnull(col, table)
        || table.rowid_alias.as_deref() == Some(&col.name)
    {
        "?"
    } else {
        ""
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_ts_literal_basic() {
        assert_eq!(escape_ts_literal("hello"), "hello");
        assert_eq!(escape_ts_literal("foo\"bar"), "foo\\\"bar");
        assert_eq!(escape_ts_literal("foo\\bar"), "foo\\\\bar");
    }

    #[test]
    fn escape_ts_literal_control_chars() {
        assert_eq!(escape_ts_literal("a\nb\tc"), "a\\nb\\tc");
        assert_eq!(escape_ts_literal("a\rb"), "a\\rb");
        assert_eq!(escape_ts_literal("\x00"), "\\u0000");
        assert_eq!(escape_ts_literal("\x1f"), "\\u001f");
    }

    #[test]
    fn escape_ts_literal_combined() {
        assert_eq!(escape_ts_literal("say \"hi\"\n"), "say \\\"hi\\\"\\n");
    }

    #[test]
    fn like_match_basic() {
        assert!(like_match("sqlite_%", "sqlite_master"));
        assert!(like_match("sqlite_%", "sqlite_temp_master"));
        assert!(!like_match("sqlite_%", "users"));
        assert!(like_match("%", "anything"));
        assert!(like_match("_", "a"));
        assert!(!like_match("_", "ab"));
        assert!(like_match("schema_migrations", "schema_migrations"));
        assert!(!like_match("schema_migrations", "other"));
    }

    #[test]
    fn like_match_case_insensitive() {
        assert!(like_match("SQLITE_%", "sqlite_master"));
        assert!(like_match("sqlite_%", "SQLITE_MASTER"));
    }

    #[test]
    fn extract_enums_simple() {
        let sql = "CREATE TABLE posts (type TEXT CHECK (type IN ('entry', 'event')))";
        let parsed = parse_create_table(Some(sql));
        assert_eq!(
            parsed.enums.get("type"),
            Some(&vec!["entry".to_string(), "event".to_string()])
        );
        assert!(!parsed.without_rowid);
    }

    #[test]
    fn extract_enums_quoted_column() {
        let sql = "CREATE TABLE t (\"type\" TEXT CHECK (\"type\" IN ('a','b')))";
        let parsed = parse_create_table(Some(sql));
        assert_eq!(
            parsed.enums.get("type"),
            Some(&vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn extract_enums_no_check() {
        let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY)";
        let parsed = parse_create_table(Some(sql));
        assert!(parsed.enums.is_empty());
    }

    #[test]
    fn extract_enums_none_sql() {
        let parsed = parse_create_table(None);
        assert!(parsed.enums.is_empty());
        assert!(!parsed.without_rowid);
    }

    #[test]
    fn extract_enums_escaped_quotes() {
        let sql = "CREATE TABLE t (status TEXT CHECK (status IN ('it''s', 'a''b''c')))";
        let parsed = parse_create_table(Some(sql));
        assert_eq!(
            parsed.enums.get("status"),
            Some(&vec!["it's".to_string(), "a'b'c".to_string()])
        );
    }

    #[test]
    fn detect_rowid_alias_basic() {
        let cols = vec![
            ColumnInfo {
                name: "id".into(),
                declared_type: "INTEGER".into(),
                notnull: true,
                default_value: None,
                pk: 1,
            },
            ColumnInfo {
                name: "name".into(),
                declared_type: "TEXT".into(),
                notnull: false,
                default_value: None,
                pk: 0,
            },
        ];
        assert_eq!(detect_rowid_alias(&cols, false), Some("id".into()));
    }

    #[test]
    fn detect_rowid_alias_without_rowid() {
        let cols = vec![ColumnInfo {
            name: "id".into(),
            declared_type: "INTEGER".into(),
            notnull: true,
            default_value: None,
            pk: 1,
        }];
        assert_eq!(detect_rowid_alias(&cols, true), None);
    }

    #[test]
    fn detect_rowid_alias_composite_pk() {
        let cols = vec![
            ColumnInfo {
                name: "a".into(),
                declared_type: "INTEGER".into(),
                notnull: true,
                default_value: None,
                pk: 1,
            },
            ColumnInfo {
                name: "b".into(),
                declared_type: "INTEGER".into(),
                notnull: true,
                default_value: None,
                pk: 2,
            },
        ];
        assert_eq!(detect_rowid_alias(&cols, false), None);
    }

    #[test]
    fn detect_rowid_alias_text_pk() {
        let cols = vec![ColumnInfo {
            name: "id".into(),
            declared_type: "TEXT".into(),
            notnull: true,
            default_value: None,
            pk: 1,
        }];
        assert_eq!(detect_rowid_alias(&cols, false), None);
    }

    #[test]
    fn parse_create_table_detects_without_rowid() {
        let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY) WITHOUT ROWID";
        let parsed = parse_create_table(Some(sql));
        assert!(parsed.without_rowid);
    }

    #[test]
    fn parse_create_table_without_rowid_is_false_by_default() {
        let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)";
        let parsed = parse_create_table(Some(sql));
        assert!(!parsed.without_rowid);
    }

    #[test]
    fn parse_create_table_fallback_detects_without_rowid() {
        // Unparseable SQL that contains WITHOUT ROWID should still detect it
        // via the fallback string scan.
        let sql = "CREATE TABLE t (weird_syntax!!!) WITHOUT ROWID";
        let parsed = parse_create_table(Some(sql));
        assert!(parsed.without_rowid);
        assert!(parsed.enums.is_empty());
    }

    #[test]
    fn parse_create_table_accumulates_without_rowid() {
        // Multiple statements: first has WITHOUT ROWID, second doesn't.
        // The flag should remain true (|= accumulation).
        let sql = indoc::indoc! {r#"
            CREATE TABLE t1 (id INTEGER) WITHOUT ROWID;
            CREATE TABLE t2 (id INTEGER);
        "#};
        let parsed = parse_create_table(Some(sql));
        assert!(parsed.without_rowid);
    }
}
