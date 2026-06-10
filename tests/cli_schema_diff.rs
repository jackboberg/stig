//! Integration tests for `stig schema diff`.

mod common;

use predicates::prelude::*;
use rusqlite::Connection;
use tempfile::TempDir;

use common::{stig_cmd, write_migration};

#[test]
fn no_diff_when_schemas_match() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Diff should report no differences
    stig_cmd(&dir)
        .arg("schema")
        .arg("diff")
        .assert()
        .success()
        .stdout(predicate::str::contains("no schema differences detected"));
}

#[test]
fn detects_new_table() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Add a table directly to the DB (not through migrations)
    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch("CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT);")
            .unwrap();
    }

    stig_cmd(&dir)
        .arg("schema")
        .arg("diff")
        .assert()
        .success()
        .stdout(predicate::str::contains("-- NEW OBJECTS"))
        .stdout(predicate::str::contains("CREATE TABLE posts"));
}

#[test]
fn detects_removed_table() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY);",
    );
    write_migration(
        &dir,
        "20240102000000",
        "create_posts",
        "CREATE TABLE posts (id INTEGER PRIMARY KEY);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Drop a table directly
    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("DROP TABLE posts", []).unwrap();
    }

    stig_cmd(&dir)
        .arg("schema")
        .arg("diff")
        .assert()
        .success()
        .stdout(predicate::str::contains("-- REMOVED OBJECTS"))
        .stdout(predicate::str::contains("DROP TABLE IF EXISTS"));
}

#[test]
fn detects_modified_table() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Modify the table directly (recreate with extra column)
    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
             BEGIN TRANSACTION;
             CREATE TABLE _tmp (id INTEGER PRIMARY KEY, name TEXT, email TEXT);
             INSERT INTO _tmp (id, name) SELECT id, name FROM users;
             DROP TABLE users;
             ALTER TABLE _tmp RENAME TO users;
             COMMIT;
             PRAGMA foreign_keys=ON;",
        )
        .unwrap();
    }

    stig_cmd(&dir)
        .arg("schema")
        .arg("diff")
        .assert()
        .success()
        .stdout(predicate::str::contains("-- MODIFIED OBJECTS"))
        .stdout(predicate::str::contains("PRAGMA foreign_keys=OFF"))
        .stdout(predicate::str::contains("SAVEPOINT sp"))
        .stdout(predicate::str::contains("\"_stig_new_users\""))
        .stdout(predicate::str::contains("INSERT INTO"))
        .stdout(predicate::str::contains("FROM"))
        .stdout(predicate::str::contains("DROP TABLE"))
        .stdout(predicate::str::contains("RENAME TO"))
        .stdout(predicate::str::contains("RELEASE sp"))
        .stdout(predicate::str::contains("PRAGMA foreign_keys=ON"));
}

#[test]
fn multiple_changes_combined() {
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
    write_migration(
        &dir,
        "20240103000000",
        "create_drafts",
        "CREATE TABLE drafts (id INTEGER PRIMARY KEY);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Add a new table, remove one, modify another
    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("DROP TABLE drafts", []).unwrap();
        conn.execute(
            "CREATE TABLE comments (id INTEGER PRIMARY KEY, body TEXT)",
            [],
        )
        .unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
             BEGIN TRANSACTION;
             CREATE TABLE _tmp (id INTEGER PRIMARY KEY, name TEXT, email TEXT);
             INSERT INTO _tmp (id, name) SELECT id, name FROM users;
             DROP TABLE users;
             ALTER TABLE _tmp RENAME TO users;
             COMMIT;
             PRAGMA foreign_keys=ON;",
        )
        .unwrap();
    }

    stig_cmd(&dir)
        .arg("schema")
        .arg("diff")
        .assert()
        .success()
        .stdout(predicate::str::contains("-- NEW OBJECTS"))
        .stdout(predicate::str::contains("comments"))
        .stdout(predicate::str::contains("-- REMOVED OBJECTS"))
        .stdout(predicate::str::contains("drafts"))
        .stdout(predicate::str::contains("-- MODIFIED OBJECTS"))
        .stdout(predicate::str::contains("users"));
}

#[test]
fn output_flag_writes_to_file() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Add a table
    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("CREATE TABLE posts (id INTEGER PRIMARY KEY);", [])
            .unwrap();
    }

    stig_cmd(&dir)
        .arg("schema")
        .arg("diff")
        .arg("--output")
        .arg("db/migrations/20240201000000_schema_diff.sql")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "db/migrations/20240201000000_schema_diff.sql",
        ));

    let migration_path = dir
        .path()
        .join("db/migrations/20240201000000_schema_diff.sql");
    assert!(migration_path.exists());
    let content = std::fs::read_to_string(&migration_path).unwrap();
    assert!(content.contains("CREATE TABLE posts"));

    // Verify the generated migration is valid SQL by applying it to a fresh
    // in-memory database (the diff migration takes a baseline DB to the current state)
    let fresh_conn = Connection::open_in_memory().unwrap();
    // Apply the baseline migration first
    fresh_conn
        .execute_batch("CREATE TABLE users (id INTEGER PRIMARY KEY);")
        .unwrap();
    fresh_conn
        .execute_batch(
            "CREATE TABLE schema_migrations (version TEXT NOT NULL PRIMARY KEY, checksum TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT (datetime('now')));",
        )
        .unwrap();
    // Now apply the diff migration (strip the directive and comments)
    let diff_sql = content
        .lines()
        .filter(|l| !l.starts_with("stig: non-transactional"))
        .collect::<Vec<_>>()
        .join("\n");
    fresh_conn.execute_batch(&diff_sql).unwrap();
    // Verify the posts table was created
    let count: i64 = fresh_conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='posts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1, "diff migration should create the posts table");
}

#[test]
fn excludes_internal_objects() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Add an index (should be detected but not sqlite_ objects)
    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("CREATE INDEX idx_users ON users(id);", [])
            .unwrap();
    }

    stig_cmd(&dir)
        .arg("schema")
        .arg("diff")
        .assert()
        .success()
        .stdout(predicate::str::contains("idx_users"))
        .stdout(predicate::str::contains("sqlite_").not());
}
