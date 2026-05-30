//! Integration tests for `stig redo`.

mod common;

use predicates::prelude::*;
use rusqlite::Connection;
use tempfile::TempDir;

use common::{stig_cmd, write_migration};

/// Query the number of rows in `schema_migrations` from the project DB.
fn count_schema_migrations(dir: &TempDir) -> i64 {
    let db_path = dir.path().join("app.db");
    let conn = Connection::open(db_path).unwrap();
    conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap()
}

/// Check whether a table exists in the project DB.
fn table_exists(dir: &TempDir, name: &str) -> bool {
    let db_path = dir.path().join("app.db");
    let conn = Connection::open(db_path).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |r| r.get(0),
        )
        .unwrap();
    count > 0
}

/// Query all values from a text column in a table.
fn query_column_values(dir: &TempDir, table: &str, column: &str) -> Vec<String> {
    let db_path = dir.path().join("app.db");
    let conn = Connection::open(db_path).unwrap();
    let mut stmt = conn
        .prepare(&format!("SELECT {column} FROM {table} ORDER BY {column}"))
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
}

// ---------------------------------------------------------------------------
// 1. Core acceptance: apply two, edit second, redo (no args)
// ---------------------------------------------------------------------------

#[test]
fn redo_reapplies_edited_migration() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
    );
    write_migration(
        &dir,
        "20240102000000",
        "create_posts",
        "CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Both tables should exist.
    assert!(table_exists(&dir, "users"));
    assert!(table_exists(&dir, "posts"));
    assert_eq!(count_schema_migrations(&dir), 2);

    // Edit the second migration to create a different table.
    write_migration(
        &dir,
        "20240102000000",
        "create_posts",
        "CREATE TABLE articles (id INTEGER PRIMARY KEY, body TEXT);",
    );

    stig_cmd(&dir)
        .arg("redo")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("restoring pre-20240102000000"))
        .stdout(predicate::str::contains("re-applying"))
        .stdout(predicate::str::contains("✓ redo complete"));

    // The old table (posts) should be gone; the new table (articles) should exist.
    assert!(table_exists(&dir, "users"));
    assert!(!table_exists(&dir, "posts"));
    assert!(table_exists(&dir, "articles"));
    assert_eq!(count_schema_migrations(&dir), 2);
}

// ---------------------------------------------------------------------------
// 2. redo <version> re-applies from that version forward
// ---------------------------------------------------------------------------

#[test]
fn redo_from_specific_version() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );
    write_migration(
        &dir,
        "20240102000000",
        "create_posts",
        "CREATE TABLE posts (id INTEGER);",
    );
    write_migration(
        &dir,
        "20240103000000",
        "create_tags",
        "CREATE TABLE tags (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();
    assert_eq!(count_schema_migrations(&dir), 3);

    // Edit the second migration.
    write_migration(
        &dir,
        "20240102000000",
        "create_posts",
        "CREATE TABLE articles (id INTEGER);",
    );

    stig_cmd(&dir)
        .arg("redo")
        .arg("20240102000000_create_posts")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ redo complete"));

    // First table untouched.
    assert!(table_exists(&dir, "users"));
    // Old table gone, new table present.
    assert!(!table_exists(&dir, "posts"));
    assert!(table_exists(&dir, "articles"));
    // Third migration re-applied.
    assert!(table_exists(&dir, "tags"));
    assert_eq!(count_schema_migrations(&dir), 3);
}

// ---------------------------------------------------------------------------
// 3. Missing snapshot exits 4
// ---------------------------------------------------------------------------

#[test]
fn redo_exits_4_when_snapshot_missing() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    // Migrate with snapshots disabled.
    stig_cmd(&dir)
        .env("STIG_NO_SNAPSHOT", "1")
        .arg("migrate")
        .assert()
        .success();

    stig_cmd(&dir)
        .arg("redo")
        .arg("--yes")
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains(
            "snapshot pre-20240101000000_create_users.db not found",
        ))
        .stderr(predicate::str::contains("no redo-eligible versions"));
}

// ---------------------------------------------------------------------------
// 4. Data added after snapshot is discarded
// ---------------------------------------------------------------------------

#[test]
fn redo_discards_data_added_after_snapshot() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_items",
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Insert data after migration.
    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO items (id, name) VALUES (1, 'added_after_migrate')",
            [],
        )
        .unwrap();
    }

    // Redo should restore the snapshot (which has no data) and re-apply.
    stig_cmd(&dir)
        .arg("redo")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ redo complete"));

    // The manually-inserted data should be gone.
    let values = query_column_values(&dir, "items", "name");
    assert!(
        values.is_empty(),
        "expected data to be discarded, got: {values:?}"
    );
}

// ---------------------------------------------------------------------------
// 5. No applied migrations exits 4
// ---------------------------------------------------------------------------

#[test]
fn redo_exits_4_when_no_applied_migrations() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    // Don't migrate — no applied migrations.
    stig_cmd(&dir)
        .arg("redo")
        .arg("--yes")
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("no applied migrations to redo"));
}

// ---------------------------------------------------------------------------
// 6. Explicit --yes skips confirmation prompt
// ---------------------------------------------------------------------------

#[test]
fn redo_with_yes_flag_runs_without_prompt() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // --yes should run without hanging on a prompt.
    stig_cmd(&dir)
        .arg("redo")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ redo complete"));
}
