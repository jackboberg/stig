//! Snapshot-level tests for the TypeScript codegen target.
//!
//! Each test creates a schema in an in-memory SQLite database, runs the
//! TypeScript target via `codegen::run_targets`, and asserts the output
//! against an `insta` golden snapshot.

use rusqlite::Connection;

use stig::codegen;
use stig::config::GenerateTarget;

/// Return a `GenerateTarget` entry for the TypeScript kind writing to `path`.
fn ts_target(path: &str) -> GenerateTarget {
    GenerateTarget {
        kind: "typescript".to_string(),
        path: path.to_string(),
        name: None,
        exclude: vec![],
        extra: toml::Table::new(),
    }
}

/// Run the TypeScript target against `conn` with default exclusions,
/// returning the generated content as a String.
fn generate_ts(conn: &Connection, out_path: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(out_path);

    let targets = vec![ts_target(path.to_str().unwrap())];
    let outputs = codegen::run_targets(conn, &targets, dir.path(), None).unwrap();
    assert_eq!(outputs.len(), 1);
    std::fs::read_to_string(&outputs[0].path).unwrap()
}

// Simple table with all affinities
#[test]
fn simple_table_all_affinities() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE all_types (
            col_integer  INTEGER,
            col_int      INT,
            col_bigint   BIGINT,
            col_smallint SMALLINT,
            col_tinyint  TINYINT,
            col_medint   MEDIUMINT,
            col_real     REAL,
            col_double   DOUBLE,
            col_float    FLOAT,
            col_numeric  NUMERIC,
            col_decimal  DECIMAL,
            col_text     TEXT,
            col_varchar  VARCHAR(255),
            col_char     CHAR(10),
            col_clob     CLOB,
            col_date     DATE,
            col_datetime DATETIME,
            col_time     TIME,
            col_blob     BLOB
        );",
    )
    .unwrap();

    let output = generate_ts(&conn, "types.ts");
    insta::assert_snapshot!("all_affinities", output);
}

// Nullable columns and columns with defaults
#[test]
fn nullable_and_default_columns() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE items (
            id         INTEGER PRIMARY KEY,
            name       TEXT NOT NULL,
            email      TEXT,
            score      REAL DEFAULT 0.0,
            status     TEXT NOT NULL DEFAULT 'active'
        );",
    )
    .unwrap();

    let output = generate_ts(&conn, "types.ts");
    insta::assert_snapshot!("nullable_and_defaults", output);
}

// INTEGER PRIMARY KEY rowid alias
#[test]
fn integer_primary_key_rowid_alias() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE users (
            id         INTEGER PRIMARY KEY,
            name       TEXT NOT NULL,
            email      TEXT NOT NULL
        );",
    )
    .unwrap();

    let output = generate_ts(&conn, "types.ts");
    insta::assert_snapshot!("rowid_alias", output);
}

// WITHOUT ROWID — should NOT be treated as rowid alias
#[test]
fn without_rowid_not_alias() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE kv (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        ) WITHOUT ROWID;",
    )
    .unwrap();

    let output = generate_ts(&conn, "types.ts");
    insta::assert_snapshot!("without_rowid", output);
}

// CHECK-IN enum extraction
#[test]
fn check_in_enum() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE posts (
            id        INTEGER PRIMARY KEY,
            title     TEXT NOT NULL,
            type      TEXT NOT NULL CHECK (type IN ('entry', 'event')),
            subtype   TEXT CHECK (subtype IN ('note', 'article', 'photo'))
        );",
    )
    .unwrap();

    let output = generate_ts(&conn, "types.ts");
    insta::assert_snapshot!("check_in_enum", output);
}

// Reserved-word table name
#[test]
fn reserved_word_table_name() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE \"references\" (
            id   INTEGER PRIMARY KEY,
            url  TEXT NOT NULL
        );",
    )
    .unwrap();

    let output = generate_ts(&conn, "types.ts");
    insta::assert_snapshot!("reserved_word_table_name", output);
}

// Exclude glob pattern
#[test]
fn exclude_glob_pattern() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);
         CREATE TABLE excluded_cache (key TEXT PRIMARY KEY, value TEXT);
         CREATE TABLE excluded_log (id INTEGER PRIMARY KEY, msg TEXT);",
    )
    .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("types.ts");

    let mut target = ts_target(path.to_str().unwrap());
    target.exclude = vec!["excluded_%".to_string()];

    let outputs = codegen::run_targets(&conn, &[target], dir.path(), None).unwrap();
    let output = std::fs::read_to_string(&outputs[0].path).unwrap();
    insta::assert_snapshot!("exclude_glob", output);
}

// schema_migrations excluded by default
#[test]
fn schema_migrations_excluded_by_default() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE schema_migrations (
            version    TEXT NOT NULL PRIMARY KEY,
            checksum   TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );
        CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
    )
    .unwrap();

    let output = generate_ts(&conn, "types.ts");
    insta::assert_snapshot!("schema_migrations_excluded", output);
}

// Multiple tables — ordering and mixed features
#[test]
fn multiple_tables_mixed() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE categories (
            id   INTEGER PRIMARY KEY,
            name TEXT NOT NULL
        );
        CREATE TABLE posts (
            id          INTEGER PRIMARY KEY,
            category_id INTEGER,
            title       TEXT NOT NULL,
            body        TEXT,
            status      TEXT NOT NULL CHECK (status IN ('draft', 'published', 'archived'))
        );
        CREATE TABLE comments (
            id      INTEGER PRIMARY KEY,
            post_id INTEGER NOT NULL,
            author  TEXT,
            body    TEXT NOT NULL
        );",
    )
    .unwrap();

    let output = generate_ts(&conn, "types.ts");
    insta::assert_snapshot!("multiple_tables", output);
}

// TEXT primary key (not a rowid alias)
#[test]
fn text_primary_key() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE tags (
            slug TEXT PRIMARY KEY,
            name TEXT NOT NULL
        );",
    )
    .unwrap();

    let output = generate_ts(&conn, "types.ts");
    insta::assert_snapshot!("text_primary_key", output);
}
