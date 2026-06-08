//! Schema diff: compare a live database against the migration baseline and
//! generate a migration SQL file that bridges the gap.
//!
//! The diff covers three categories:
//! - **Added objects**: present in the live DB but not in the baseline → `CREATE`
//! - **Removed objects**: present in the baseline but not in the live DB → `DROP`
//! - **Modified tables**: present in both but with different definitions → table
//!   recreation SQL (SQLite's limited `ALTER TABLE` requires rebuilding the table)

use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::Connection;
use sqlparser::ast::Statement;
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;

use crate::migrate::discover::MigrationFile;

// ---------------------------------------------------------------------------
// Schema representation
// ---------------------------------------------------------------------------

/// A single schema object discovered from `sqlite_master`.
#[derive(Debug, Clone)]
struct SchemaObject {
    obj_type: String,
    name: String,
    sql: String,
}

/// Key used to identify a schema object in the comparison map.
type SchemaKey = (String, String); // (type, name)

/// A table that exists in both schemas but with a modified definition.
#[derive(Debug, Clone)]
struct ModifiedTable {
    name: String,
    migration_sql: String,
}

/// The complete diff between two schema states.
#[derive(Debug)]
struct SchemaDiff {
    added: Vec<SchemaObject>,
    removed: Vec<SchemaObject>,
    modified: Vec<ModifiedTable>,
}

// ---------------------------------------------------------------------------
// Schema discovery
// ---------------------------------------------------------------------------

/// Dump the complete schema from a connection as a map of (type, name) → sql.
///
/// Excludes internal SQLite objects (`sqlite_%`), the `schema_migrations`
/// tracking table, and auto-generated indexes.
fn dump_schema(conn: &Connection) -> Result<HashMap<SchemaKey, SchemaObject>> {
    let mut stmt = conn
        .prepare(
            "SELECT type, name, sql FROM sqlite_master \
             WHERE sql IS NOT NULL \
               AND type IN ('table', 'index', 'trigger', 'view') \
               AND name NOT LIKE 'sqlite_%' \
               AND name != 'schema_migrations' \
             ORDER BY name",
        )
        .context("failed to prepare schema query")?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .context("failed to query sqlite_master")?;

    let mut map = HashMap::new();
    for row in rows {
        let (obj_type, name, sql) = row.context("failed to read schema row")?;
        map.insert(
            (obj_type.clone(), name.clone()),
            SchemaObject {
                obj_type,
                name,
                sql,
            },
        );
    }

    Ok(map)
}

// ---------------------------------------------------------------------------
// Baseline construction
// ---------------------------------------------------------------------------

/// Build the baseline schema by applying all migrations to an in-memory database.
fn build_baseline(files: &[MigrationFile]) -> Result<HashMap<SchemaKey, SchemaObject>> {
    let conn = Connection::open_in_memory().context("failed to open in-memory database")?;

    for file in files {
        let content = std::fs::read_to_string(&file.path)
            .with_context(|| format!("failed to read migration file: {}", file.path.display()))?;
        conn.execute_batch(&content)
            .with_context(|| format!("failed to apply migration: {}", file.path.display()))?;
    }

    dump_schema(&conn)
}

// ---------------------------------------------------------------------------
// SQL canonicalization
// ---------------------------------------------------------------------------

/// Canonicalize a SQL statement by parsing and re-rendering it.
///
/// This eliminates whitespace, quoting, and casing differences so that
/// semantically equivalent DDL compares as equal. Falls back to whitespace
/// normalization if parsing fails.
fn canonicalize_sql(sql: &str) -> String {
    match Parser::parse_sql(&SQLiteDialect {}, sql) {
        Ok(stmts) if !stmts.is_empty() => stmts
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .join("; "),
        _ => normalize_whitespace(sql),
    }
}

/// Normalize whitespace: collapse runs of whitespace to a single space and trim.
fn normalize_whitespace(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut prev_was_space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_was_space {
                result.push(' ');
                prev_was_space = true;
            }
        } else {
            result.push(c);
            prev_was_space = false;
        }
    }
    result.trim().to_string()
}

// ---------------------------------------------------------------------------
// Diff computation
// ---------------------------------------------------------------------------

/// Compare the current schema against the baseline and return the diff.
///
/// Propagates errors from table recreation generation so that failures are
/// surfaced rather than silently dropped.
fn compute_diff(
    current: &HashMap<SchemaKey, SchemaObject>,
    baseline: &HashMap<SchemaKey, SchemaObject>,
) -> Result<SchemaDiff> {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut modified = Vec::new();

    // Collect dependent objects for each table (indexes, triggers)
    let table_dependents = collect_table_dependents(current);

    for (key, obj) in current {
        if let Some(baseline_obj) = baseline.get(key) {
            let current_canonical = canonicalize_sql(&obj.sql);
            let baseline_canonical = canonicalize_sql(&baseline_obj.sql);
            if current_canonical != baseline_canonical {
                if key.0 == "table" {
                    let migration_sql = generate_table_recreation(
                        &baseline_obj.sql,
                        &obj.sql,
                        &obj.name,
                        table_dependents.get(&obj.name).map(|v| v.as_slice()),
                    )?;
                    modified.push(ModifiedTable {
                        name: obj.name.clone(),
                        migration_sql,
                    });
                } else {
                    let drop_sql = format!(
                        "DROP {} IF EXISTS {};",
                        key.0.to_uppercase(),
                        quote_name(&key.1)
                    );
                    let create_sql = ensure_semicolon(&obj.sql);
                    modified.push(ModifiedTable {
                        name: obj.name.clone(),
                        migration_sql: format!("{drop_sql}\n\n{create_sql}"),
                    });
                }
            }
        } else {
            added.push(obj.clone());
        }
    }

    for (key, obj) in baseline {
        if !current.contains_key(key) {
            removed.push(obj.clone());
        }
    }

    // Sort for deterministic output
    added.sort_by(|a, b| (&a.obj_type, &a.name).cmp(&(&b.obj_type, &b.name)));
    removed.sort_by(|a, b| (&a.obj_type, &a.name).cmp(&(&b.obj_type, &b.name)));
    modified.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(SchemaDiff {
        added,
        removed,
        modified,
    })
}

/// Build a map from table name → list of dependent (index, trigger) objects.
fn collect_table_dependents(
    schema: &HashMap<SchemaKey, SchemaObject>,
) -> HashMap<String, Vec<SchemaObject>> {
    let mut dependents: HashMap<String, Vec<SchemaObject>> = HashMap::new();

    for ((obj_type, _name), obj) in schema {
        if obj_type != "index" && obj_type != "trigger" {
            continue;
        }
        // Parse the SQL to find which table this object references
        if let Some(table_name) = extract_referenced_table(&obj.sql, obj_type) {
            dependents.entry(table_name).or_default().push(obj.clone());
        }
    }

    dependents
}

/// Extract the table name referenced by an index or trigger CREATE statement.
fn extract_referenced_table(sql: &str, obj_type: &str) -> Option<String> {
    let stmts = Parser::parse_sql(&SQLiteDialect {}, sql).ok()?;
    let stmt = stmts.first()?;
    match (obj_type, stmt) {
        ("index", Statement::CreateIndex(ci)) => Some(
            ci.table_name
                .0
                .iter()
                .filter_map(|p| p.as_ident())
                .map(|i| i.value.clone())
                .collect(),
        ),
        ("trigger", Statement::CreateTrigger(ct)) => Some(
            ct.table_name
                .0
                .iter()
                .filter_map(|p| p.as_ident())
                .map(|i| i.value.clone())
                .collect(),
        ),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Table recreation generation
// ---------------------------------------------------------------------------

/// Generate the SQL needed to recreate a table with a new definition while
/// preserving data in columns that exist in both the old and new schemas.
///
/// The generated SQL follows this pattern:
/// 1. Rename the existing table aside
/// 2. Run the new CREATE TABLE (creates table under original name)
/// 3. Copy common columns from the renamed table
/// 4. Drop the renamed table
/// 5. Recreate dependent indexes/triggers
fn generate_table_recreation(
    old_sql: &str,
    new_sql: &str,
    table_name: &str,
    dependents: Option<&[SchemaObject]>,
) -> Result<String> {
    let old_cols = extract_columns(old_sql)?;
    let new_cols = extract_columns(new_sql)?;

    let old_names: Vec<&str> = old_cols.iter().map(|c| c.name.as_str()).collect();
    let new_names: Vec<&str> = new_cols.iter().map(|c| c.name.as_str()).collect();

    let common: Vec<&str> = old_names
        .iter()
        .filter(|c| new_names.contains(c))
        .copied()
        .collect();

    let temp_name = format!("_stig_old_{table_name}");

    let mut parts = Vec::new();
    parts.push("PRAGMA foreign_keys=OFF;".to_string());
    parts.push("BEGIN TRANSACTION;".to_string());

    // Step 1: Rename existing table aside
    parts.push(format!("ALTER TABLE {table_name} RENAME TO {temp_name};"));

    // Step 2: Create the new table (under the original name)
    parts.push(format!("\n-- New table definition\n{new_sql}"));

    // Step 3: Copy data from the renamed table
    if !common.is_empty() {
        let col_list = common
            .iter()
            .map(|c| quote_name(c))
            .collect::<Vec<_>>()
            .join(", ");
        parts.push(format!(
            "INSERT INTO {table_name} ({col_list}) SELECT {col_list} FROM {temp_name};"
        ));
    }

    // Step 4: Drop the renamed table
    parts.push(format!("DROP TABLE {temp_name};"));

    // Step 5: Recreate dependent indexes and triggers
    if let Some(deps) = dependents
        && !deps.is_empty()
    {
        parts.push("\n-- Recreate dependent objects".to_string());
        for dep in deps {
            parts.push(format!("-- {}", dep.obj_type));
            parts.push(ensure_semicolon(&dep.sql));
            parts.push(String::new());
        }
    }

    parts.push("COMMIT;".to_string());
    parts.push("PRAGMA foreign_keys=ON;".to_string());

    Ok(parts.join("\n"))
}

/// Extract column definitions from a CREATE TABLE statement.
fn extract_columns(sql: &str) -> Result<Vec<ColumnInfo>> {
    let stmts = Parser::parse_sql(&SQLiteDialect {}, sql)
        .context("failed to parse CREATE TABLE statement")?;

    let Statement::CreateTable(create) = stmts.first().context("expected CREATE TABLE")? else {
        anyhow::bail!("expected CREATE TABLE statement");
    };

    let mut columns = Vec::new();
    for col in &create.columns {
        columns.push(ColumnInfo {
            name: col.name.to_string(),
        });
    }

    Ok(columns)
}

#[derive(Debug, Clone)]
struct ColumnInfo {
    name: String,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn ensure_semicolon(sql: &str) -> String {
    let trimmed = sql.trim_end();
    if trimmed.ends_with(';') {
        trimmed.to_string()
    } else {
        format!("{trimmed};")
    }
}

/// Quote an identifier for use in SQL, escaping embedded double quotes.
fn quote_name(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

// ---------------------------------------------------------------------------
// Migration output formatting
// ---------------------------------------------------------------------------

/// Format the diff into a migration SQL string.
fn format_migration(diff: &SchemaDiff) -> Option<String> {
    if diff.added.is_empty() && diff.removed.is_empty() && diff.modified.is_empty() {
        return None;
    }

    let timestamp = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let mut parts = vec![
        "-- stig schema diff migration".to_string(),
        format!("-- Generated: {timestamp}"),
        String::new(),
    ];

    if !diff.added.is_empty() {
        parts.push("-- NEW OBJECTS".to_string());
        for obj in &diff.added {
            parts.push(format!("-- {}", obj.obj_type));
            parts.push(ensure_semicolon(&obj.sql));
            parts.push(String::new());
        }
    }

    if !diff.removed.is_empty() {
        parts.push("-- REMOVED OBJECTS".to_string());
        for obj in &diff.removed {
            let drop_type = obj.obj_type.to_uppercase();
            parts.push(format!(
                "DROP {drop_type} IF EXISTS {};",
                quote_name(&obj.name)
            ));
            parts.push(String::new());
        }
    }

    if !diff.modified.is_empty() {
        parts.push("-- MODIFIED OBJECTS".to_string());
        for mt in &diff.modified {
            parts.push(format!("-- Table: {}", mt.name));
            parts.push(mt.migration_sql.clone());
            parts.push(String::new());
        }
    }

    Some(parts.join("\n"))
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate a migration SQL string that bridges the gap between the current
/// database schema and the migration baseline.
///
/// Returns `None` if the schemas are identical.
pub fn generate_migration(conn: &Connection, files: &[MigrationFile]) -> Result<Option<String>> {
    let current = dump_schema(conn).context("failed to dump current schema")?;
    let baseline = build_baseline(files).context("failed to build baseline schema")?;

    let diff = compute_diff(&current, &baseline)?;
    Ok(format_migration(&diff))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_migration_file(
        dir: &TempDir,
        timestamp: &str,
        slug: &str,
        content: &str,
    ) -> MigrationFile {
        let migrations_dir = dir.path().join("db/migrations");
        std::fs::create_dir_all(&migrations_dir).unwrap();
        let filename = format!("{timestamp}_{slug}.sql");
        let path = migrations_dir.join(&filename);
        std::fs::write(&path, content).unwrap();
        MigrationFile {
            timestamp: timestamp.to_string(),
            slug: slug.to_string(),
            path,
        }
    }

    #[test]
    fn dump_schema_excludes_internal_objects() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE users (id INTEGER PRIMARY KEY);
             CREATE TABLE schema_migrations (version TEXT);
             CREATE INDEX idx_users ON users(id);",
        )
        .unwrap();

        let schema = dump_schema(&conn).unwrap();
        assert!(schema.contains_key(&("table".to_string(), "users".to_string())));
        assert!(!schema.contains_key(&("table".to_string(), "schema_migrations".to_string())));
        assert!(!schema.keys().any(|(_, n)| n.starts_with("sqlite_")));
    }

    #[test]
    fn build_baseline_applies_migrations() {
        let dir = TempDir::new().unwrap();
        let files = vec![
            write_migration_file(
                &dir,
                "20240101000000",
                "create_users",
                "CREATE TABLE users (id INTEGER PRIMARY KEY);",
            ),
            write_migration_file(
                &dir,
                "20240102000000",
                "create_posts",
                "CREATE TABLE posts (id INTEGER PRIMARY KEY, user_id INTEGER);",
            ),
        ];

        let baseline = build_baseline(&files).unwrap();
        assert!(baseline.contains_key(&("table".to_string(), "users".to_string())));
        assert!(baseline.contains_key(&("table".to_string(), "posts".to_string())));
    }

    #[test]
    fn compute_diff_detects_added_table() {
        let dir = TempDir::new().unwrap();
        let baseline = build_baseline(&[write_migration_file(
            &dir,
            "20240101000000",
            "create_users",
            "CREATE TABLE users (id INTEGER PRIMARY KEY);",
        )])
        .unwrap();

        let current_conn = Connection::open_in_memory().unwrap();
        current_conn
            .execute_batch(
                "CREATE TABLE users (id INTEGER PRIMARY KEY);
             CREATE TABLE posts (id INTEGER PRIMARY KEY);",
            )
            .unwrap();
        let current = dump_schema(&current_conn).unwrap();

        let diff = compute_diff(&current, &baseline).unwrap();
        assert_eq!(diff.added.len(), 1);
        assert_eq!(diff.added[0].name, "posts");
        assert!(diff.removed.is_empty());
        assert!(diff.modified.is_empty());
    }

    #[test]
    fn compute_diff_detects_removed_table() {
        let dir = TempDir::new().unwrap();
        let baseline = build_baseline(&[
            write_migration_file(
                &dir,
                "20240101000000",
                "create_users",
                "CREATE TABLE users (id INTEGER PRIMARY KEY);",
            ),
            write_migration_file(
                &dir,
                "20240102000000",
                "create_posts",
                "CREATE TABLE posts (id INTEGER PRIMARY KEY);",
            ),
        ])
        .unwrap();

        let current_conn = Connection::open_in_memory().unwrap();
        current_conn
            .execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY);")
            .unwrap();
        let current = dump_schema(&current_conn).unwrap();

        let diff = compute_diff(&current, &baseline).unwrap();
        assert_eq!(diff.removed.len(), 1);
        assert_eq!(diff.removed[0].name, "posts");
        assert!(diff.added.is_empty());
        assert!(diff.modified.is_empty());
    }

    #[test]
    fn compute_diff_detects_modified_table() {
        let dir = TempDir::new().unwrap();
        let baseline = build_baseline(&[write_migration_file(
            &dir,
            "20240101000000",
            "create_users",
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
        )])
        .unwrap();

        let current_conn = Connection::open_in_memory().unwrap();
        current_conn
            .execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT);")
            .unwrap();
        let current = dump_schema(&current_conn).unwrap();

        let diff = compute_diff(&current, &baseline).unwrap();
        assert_eq!(diff.modified.len(), 1);
        assert_eq!(diff.modified[0].name, "users");
        let sql = &diff.modified[0].migration_sql;
        assert!(sql.contains("PRAGMA foreign_keys=OFF"));
        assert!(sql.contains("BEGIN TRANSACTION"));
        assert!(sql.contains("ALTER TABLE users RENAME TO _stig_old_users"));
        assert!(sql.contains("CREATE TABLE users"));
        assert!(sql.contains("INSERT INTO users"));
        assert!(sql.contains("SELECT"));
        assert!(sql.contains("FROM _stig_old_users"));
        assert!(sql.contains("DROP TABLE _stig_old_users"));
        assert!(sql.contains("COMMIT"));
        assert!(sql.contains("PRAGMA foreign_keys=ON"));
    }

    #[test]
    fn compute_diff_empty_when_identical() {
        let dir = TempDir::new().unwrap();
        let baseline = build_baseline(&[write_migration_file(
            &dir,
            "20240101000000",
            "create_users",
            "CREATE TABLE users (id INTEGER PRIMARY KEY);",
        )])
        .unwrap();

        let current_conn = Connection::open_in_memory().unwrap();
        current_conn
            .execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY);")
            .unwrap();
        let current = dump_schema(&current_conn).unwrap();

        let diff = compute_diff(&current, &baseline).unwrap();
        assert!(diff.added.is_empty());
        assert!(diff.removed.is_empty());
        assert!(diff.modified.is_empty());
    }

    #[test]
    fn compute_diff_ignores_whitespace_differences() {
        let dir = TempDir::new().unwrap();
        let baseline = build_baseline(&[write_migration_file(
            &dir,
            "20240101000000",
            "create_users",
            "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
        )])
        .unwrap();

        // Same table with different whitespace
        let current_conn = Connection::open_in_memory().unwrap();
        current_conn
            .execute_batch(
                "CREATE TABLE users (
                    id INTEGER PRIMARY KEY,
                    name TEXT
                );",
            )
            .unwrap();
        let current = dump_schema(&current_conn).unwrap();

        let diff = compute_diff(&current, &baseline).unwrap();
        assert!(
            diff.modified.is_empty(),
            "whitespace-only differences should not trigger a modification"
        );
    }

    #[test]
    fn format_migration_returns_none_when_empty() {
        let diff = SchemaDiff {
            added: vec![],
            removed: vec![],
            modified: vec![],
        };
        assert!(format_migration(&diff).is_none());
    }

    #[test]
    fn format_migration_includes_sections() {
        let diff = SchemaDiff {
            added: vec![SchemaObject {
                obj_type: "table".to_string(),
                name: "posts".to_string(),
                sql: "CREATE TABLE posts (id INTEGER PRIMARY KEY)".to_string(),
            }],
            removed: vec![SchemaObject {
                obj_type: "table".to_string(),
                name: "drafts".to_string(),
                sql: "CREATE TABLE drafts (id INTEGER PRIMARY KEY)".to_string(),
            }],
            modified: vec![],
        };
        let output = format_migration(&diff).unwrap();
        assert!(output.contains("-- NEW OBJECTS"));
        assert!(output.contains("-- REMOVED OBJECTS"));
        assert!(output.contains("CREATE TABLE posts"));
        assert!(output.contains("DROP TABLE IF EXISTS"));
    }

    #[test]
    fn extract_columns_parses_create_table() {
        let sql = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT)";
        let cols = extract_columns(sql).unwrap();
        assert_eq!(cols.len(), 3);
        assert_eq!(cols[0].name, "id");
        assert_eq!(cols[1].name, "name");
        assert_eq!(cols[2].name, "email");
    }

    #[test]
    fn generate_migration_none_when_identical() {
        let dir = TempDir::new().unwrap();
        let files = vec![write_migration_file(
            &dir,
            "20240101000000",
            "create_users",
            "CREATE TABLE users (id INTEGER PRIMARY KEY);",
        )];

        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY);")
            .unwrap();

        let result = generate_migration(&conn, &files).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn quote_name_escapes_embedded_quotes() {
        assert_eq!(quote_name("foo"), "\"foo\"");
        assert_eq!(quote_name("foo\"bar"), "\"foo\"\"bar\"");
        assert_eq!(quote_name("\"\""), "\"\"\"\"\"\"");
    }

    #[test]
    fn canonicalize_sql_normalizes_whitespace() {
        let a = "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)";
        let b = "CREATE  TABLE  users  ( id  INTEGER  PRIMARY KEY ,  name  TEXT )";
        assert_eq!(canonicalize_sql(a), canonicalize_sql(b));
    }

    #[test]
    fn collect_table_dependents_finds_indexes_and_triggers() {
        let mut schema = HashMap::new();
        schema.insert(
            ("table".to_string(), "users".to_string()),
            SchemaObject {
                obj_type: "table".to_string(),
                name: "users".to_string(),
                sql: "CREATE TABLE users (id INTEGER PRIMARY KEY)".to_string(),
            },
        );
        schema.insert(
            ("index".to_string(), "idx_users_name".to_string()),
            SchemaObject {
                obj_type: "index".to_string(),
                name: "idx_users_name".to_string(),
                sql: "CREATE INDEX idx_users_name ON users(name)".to_string(),
            },
        );
        schema.insert(
            ("trigger".to_string(), "trig_users".to_string()),
            SchemaObject {
                obj_type: "trigger".to_string(),
                name: "trig_users".to_string(),
                sql: "CREATE TRIGGER trig_users AFTER INSERT ON users BEGIN SELECT 1; END"
                    .to_string(),
            },
        );

        let dependents = collect_table_dependents(&schema);
        let users_deps = dependents.get("users").unwrap();
        assert_eq!(users_deps.len(), 2);
    }
}
