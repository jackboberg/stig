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

// Core acceptance: apply two, edit second, redo (no args)
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
        .stdout(predicate::str::contains("apply"))
        .stdout(predicate::str::contains("✓ redo complete"));

    // The old table (posts) should be gone; the new table (articles) should exist.
    assert!(table_exists(&dir, "users"));
    assert!(!table_exists(&dir, "posts"));
    assert!(table_exists(&dir, "articles"));
    assert_eq!(count_schema_migrations(&dir), 2);
}

// redo <version> re-applies from that version forward
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

// Missing snapshot exits 4
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

// Data added after snapshot is discarded
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

// No applied migrations exits 4
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

// Explicit --yes skips confirmation prompt
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

// Invalid version exits 4 with clear message
#[test]
fn redo_exits_4_when_version_not_applied() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    stig_cmd(&dir)
        .arg("redo")
        .arg("20240102000000_nonexistent")
        .arg("--yes")
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains(
            "version not found in applied migrations",
        ));
}

// Non-TTY confirmation (empty stdin) is treated as decline and exits 2.
#[test]
fn redo_non_tty_declined_exits_2() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Pipe empty stdin to simulate a non-TTY / CI environment.
    stig_cmd(&dir)
        .arg("redo")
        .write_stdin("")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("operation cancelled"));

    // Database must be untouched.
    assert_eq!(count_schema_migrations(&dir), 1);
    assert!(table_exists(&dir, "users"));
}

// Even when a fresh schema manifest exists, redo must replay migrations
// individually (not use the manifest fast path) because the snapshot restore
// leaves prior schema in place.
#[test]
fn redo_does_not_use_schema_manifest_fast_path() {
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

    // First migrate to generate schema.sql
    stig_cmd(&dir).arg("migrate").assert().success();

    // schema.sql should now exist and be fresh
    assert!(dir.path().join("db/schema.sql").exists());

    // Edit the second migration
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
        .stdout(predicate::str::contains("apply"))
        .stdout(predicate::str::contains("applied db/schema.sql").not())
        .stdout(predicate::str::contains("✓ redo complete"));

    assert!(table_exists(&dir, "users"));
    assert!(!table_exists(&dir, "posts"));
    assert!(table_exists(&dir, "articles"));
    assert_eq!(count_schema_migrations(&dir), 2);
}

// In-memory database is rejected before any snapshot/db work.
#[test]
fn redo_exits_2_with_in_memory_database() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    stig_cmd(&dir)
        .env("STIG_DATABASE_PATH", ":memory:")
        .arg("redo")
        .arg("--yes")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "cannot redo an in-memory database",
        ));
}
