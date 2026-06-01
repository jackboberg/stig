//! Built-in TypeScript codegen target.
//!
//! Introspects the live SQLite schema and emits a `.ts` file with
//! `Enums`, `Tables`, and utility types matching the Supabase
//! `gen types typescript` convention (SPEC §7).

use std::collections::BTreeMap;
use std::fs;

use rusqlite::Connection;
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
            let enums = extract_enums(table.sql.as_deref());
            let rowid_alias = detect_rowid_alias(&columns, table.sql.as_deref());

            for (col_name, values) in &enums {
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
            formatted: false,
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
        .map_err(|e| CodegenError::Target(e.to_string()))?;

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
        .map_err(|e| CodegenError::Target(e.to_string()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| CodegenError::Target(e.to_string()))?;

    Ok(columns)
}

// ---------------------------------------------------------------------------
// CHECK-IN enum extraction
// ---------------------------------------------------------------------------

/// Extract `CHECK (<col> IN ('a','b',...))` constraints from a CREATE TABLE
/// statement. Returns a map of column name → list of string literal values.
fn extract_enums(sql: Option<&str>) -> BTreeMap<String, Vec<String>> {
    let sql = match sql {
        Some(s) => s,
        None => return BTreeMap::new(),
    };

    let normalized = normalize_whitespace(sql);
    let mut enums = BTreeMap::new();
    let bytes = normalized.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Look for "CHECK" (case-insensitive)
        if i + 5 <= len
            && bytes[i].eq_ignore_ascii_case(&b'C')
            && bytes[i + 1].eq_ignore_ascii_case(&b'H')
            && bytes[i + 2].eq_ignore_ascii_case(&b'E')
            && bytes[i + 3].eq_ignore_ascii_case(&b'C')
            && bytes[i + 4].eq_ignore_ascii_case(&b'K')
        {
            i += 5;

            // Skip whitespace and '('
            i = skip_ws(bytes, i);
            if i >= len || bytes[i] != b'(' {
                continue;
            }
            i += 1;
            i = skip_ws(bytes, i);

            // Read column name (possibly quoted with " or `)
            let col = if i < len && (bytes[i] == b'"' || bytes[i] == b'`') {
                let quote = bytes[i];
                i += 1;
                let start = i;
                while i < len && bytes[i] != quote {
                    i += 1;
                }
                let c = normalized[start..i].to_string();
                if i < len {
                    i += 1;
                }
                c
            } else {
                let start = i;
                while i < len && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                    i += 1;
                }
                normalized[start..i].to_string()
            };

            i = skip_ws(bytes, i);

            // Expect "IN"
            if i + 2 > len
                || !bytes[i].eq_ignore_ascii_case(&b'I')
                || !bytes[i + 1].eq_ignore_ascii_case(&b'N')
            {
                continue;
            }
            i += 2;
            i = skip_ws(bytes, i);

            // Expect '('
            if i >= len || bytes[i] != b'(' {
                continue;
            }
            i += 1;

            // Read values until ')'
            let val_start = i;
            while i < len && bytes[i] != b')' {
                i += 1;
            }
            let val_str = &normalized[val_start..i];

            let values = extract_string_literals(val_str);
            if !values.is_empty() {
                enums.insert(col, values);
            }
        } else {
            i += 1;
        }
    }

    enums
}

/// Extract single-quoted string literal contents from `s`.
///
/// Handles SQL escaped single quotes (`''` → `'`).
fn extract_string_literals(s: &str) -> Vec<String> {
    let mut values = Vec::new();
    let bytes = s.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if bytes[i] == b'\'' {
            i += 1;
            let start = i;
            while i < len {
                if bytes[i] == b'\'' {
                    if i + 1 < len && bytes[i + 1] == b'\'' {
                        i += 2; // skip escaped quote pair
                    } else {
                        break; // end of string literal
                    }
                } else {
                    i += 1;
                }
            }
            values.push(s[start..i].replace("''", "'"));
            if i < len {
                i += 1;
            }
        } else {
            i += 1;
        }
    }

    values
}

/// Collapse runs of whitespace into single spaces and trim.
fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn skip_ws(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

// ---------------------------------------------------------------------------
// Rowid alias detection
// ---------------------------------------------------------------------------

/// Detect whether the table has an `INTEGER PRIMARY KEY` rowid alias.
///
/// Returns the alias column name if:
/// - Exactly one column has `pk = 1` and declared type contains `INT`.
/// - The CREATE TABLE statement does NOT contain `WITHOUT ROWID`.
fn detect_rowid_alias(columns: &[ColumnInfo], sql: Option<&str>) -> Option<String> {
    if let Some(sql) = sql {
        let upper = sql.to_uppercase();
        if upper.contains("WITHOUT ROWID") || upper.contains("WITHOUTROWID") {
            return None;
        }
    }

    let pk_cols: Vec<&ColumnInfo> = columns.iter().filter(|c| c.pk > 0).collect();
    if pk_cols.len() != 1 {
        return None;
    }

    let col = pk_cols[0];
    if col.declared_type.to_uppercase().contains("INT") {
        Some(col.name.clone())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Exclusion (SQL LIKE-style glob matching)
// ---------------------------------------------------------------------------

fn is_excluded(name: &str, exclude: &[String]) -> bool {
    exclude.iter().any(|pattern| like_match(pattern, name))
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
        "INTEGER" | "INT" | "BIGINT" | "SMALLINT" | "TINYINT" | "MEDIUMINT" | "REAL" | "DOUBLE"
        | "FLOAT" | "NUMERIC" | "DECIMAL" => "number".to_string(),

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
                .map(|v| format!("\"{v}\""))
                .collect::<Vec<_>>()
                .join(" | ");
            out.push_str(&format!("  {name}: {union};\n"));
        }
        out.push_str("};\n\n");
    }

    // Tables
    out.push_str("export type Tables = {\n");
    for table in table_infos {
        out.push_str(&format!("  \"{}\": {{\n", table.name));

        // Row
        out.push_str("    Row: {\n");
        for col in &table.columns {
            let base = column_type(col, &table.name, all_enums);
            let nullable = nullable_suffix(col, table);
            out.push_str(&format!("      {}: {base}{nullable};\n", col.name));
        }
        out.push_str("    };\n");

        // Insert
        out.push_str("    Insert: {\n");
        for col in &table.columns {
            let base = column_type(col, &table.name, all_enums);
            let nullable = nullable_suffix(col, table);
            let optional = insert_optional(col, table);
            out.push_str(&format!(
                "      {}{}: {base}{nullable};\n",
                col.name, optional
            ));
        }
        out.push_str("    };\n");

        // Update
        out.push_str("    Update: {\n");
        for col in &table.columns {
            let base = column_type(col, &table.name, all_enums);
            let nullable = nullable_suffix(col, table);
            out.push_str(&format!("      {}?: {base}{nullable};\n", col.name));
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

/// Return `Enums.<table>_<col>` if the column has a CHECK-IN constraint,
/// otherwise the affinity-mapped TS type.
fn column_type(
    col: &ColumnInfo,
    table_name: &str,
    enums: &BTreeMap<String, Vec<String>>,
) -> String {
    let enum_key = format!("{table_name}_{}", col.name);
    if enums.contains_key(&enum_key) {
        return format!("Enums.{enum_key}");
    }
    ts_type_for_column(col, table_name)
}

/// Whether a column is effectively NOT NULL.
///
/// A column is NOT NULL if:
/// - It has an explicit `NOT NULL` constraint, OR
/// - It is a PRIMARY KEY column (`pk > 0`) that is NOT a rowid alias.
///   In SQLite, all PRIMARY KEY columns (except INTEGER PRIMARY KEY
///   rowid aliases) are implicitly NOT NULL.
fn is_effectively_notnull(col: &ColumnInfo, table: &TableInfo) -> bool {
    if col.notnull {
        return true;
    }
    // Non-rowid-alias PRIMARY KEY columns are implicitly NOT NULL.
    col.pk > 0 && table.rowid_alias.as_deref() != Some(&col.name)
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
    fn normalize_whitespace_collapses_runs() {
        assert_eq!(
            normalize_whitespace("  hello   world  \n\t foo  "),
            "hello world foo"
        );
    }

    #[test]
    fn extract_enums_simple() {
        let sql = "CREATE TABLE posts (type TEXT CHECK (type IN ('entry', 'event')))";
        let enums = extract_enums(Some(sql));
        assert_eq!(
            enums.get("type"),
            Some(&vec!["entry".to_string(), "event".to_string()])
        );
    }

    #[test]
    fn extract_enums_quoted_column() {
        let sql = "CREATE TABLE t (\"type\" TEXT CHECK (\"type\" IN ('a','b')))";
        let enums = extract_enums(Some(sql));
        assert_eq!(
            enums.get("type"),
            Some(&vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn extract_enums_no_check() {
        let sql = "CREATE TABLE t (id INTEGER PRIMARY KEY)";
        let enums = extract_enums(Some(sql));
        assert!(enums.is_empty());
    }

    #[test]
    fn extract_enums_none_sql() {
        let enums = extract_enums(None);
        assert!(enums.is_empty());
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
        let sql = Some("CREATE TABLE t (id INTEGER PRIMARY KEY, name TEXT)");
        assert_eq!(detect_rowid_alias(&cols, sql), Some("id".into()));
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
        let sql = Some("CREATE TABLE t (id INTEGER PRIMARY KEY) WITHOUT ROWID");
        assert_eq!(detect_rowid_alias(&cols, sql), None);
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
        let sql = Some("CREATE TABLE t (a INTEGER, b INTEGER, PRIMARY KEY (a, b))");
        assert_eq!(detect_rowid_alias(&cols, sql), None);
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
        let sql = Some("CREATE TABLE t (id TEXT PRIMARY KEY)");
        assert_eq!(detect_rowid_alias(&cols, sql), None);
    }

    #[test]
    fn extract_string_literals_basic() {
        let values = extract_string_literals("'entry', 'event'");
        assert_eq!(values, vec!["entry", "event"]);
    }

    #[test]
    fn extract_string_literals_no_spaces() {
        let values = extract_string_literals("'a','b','c'");
        assert_eq!(values, vec!["a", "b", "c"]);
    }

    #[test]
    fn extract_string_literals_escaped_quotes() {
        let values = extract_string_literals("'it''s', 'a''b''c'");
        assert_eq!(values, vec!["it's", "a'b'c"]);
    }
}
