//! Integration tests for `stig restore`.

mod common;

use predicates::prelude::*;
use rusqlite::Connection;
use tempfile::TempDir;

use common::{stig_cmd, write_migration};

/// Count `.db` files in the resets directory.
fn count_reset_files(dir: &TempDir) -> usize {
    let resets_dir = dir.path().join("db/resets");
    std::fs::read_dir(&resets_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".db"))
        .count()
}

/// Read a single string value from the project DB.
fn query_value(dir: &TempDir, sql: &str) -> String {
    let db_path = dir.path().join("app.db");
    let conn = Connection::open(db_path).unwrap();
    conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

// Restore the most recent reset backup.
#[test]
fn restore_restores_most_recent_backup() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Populate the database.
    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("INSERT INTO users (id, name) VALUES (1, 'original')", [])
            .unwrap();
    }

    // Reset to create a backup and leave a fresh database.
    stig_cmd(&dir).arg("reset").arg("--yes").assert().success();

    // Fresh DB should have no data.
    {
        let conn = Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "expected empty database after reset");
    }

    // Restore the most recent backup.
    stig_cmd(&dir)
        .arg("restore")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ restored database from reset-"));

    // Original data should be back.
    let name: String = query_value(&dir, "SELECT name FROM users WHERE id = 1");
    assert_eq!(name, "original");

    // Backup artifact still exists.
    assert_eq!(count_reset_files(&dir), 1);
}

// Restore a specific backup by timestamp.
#[test]
fn restore_restores_specific_backup_by_timestamp() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    let db_path = dir.path().join("app.db");
    let resets_dir = dir.path().join("db/resets");
    std::fs::create_dir_all(&resets_dir).unwrap();

    // Create two manual reset backups with known timestamps.
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("INSERT INTO users (id, name) VALUES (1, 'first')", [])
            .unwrap();
    }
    std::fs::copy(&db_path, resets_dir.join("reset-20240101T000000Z.db")).unwrap();

    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("UPDATE users SET name = 'second' WHERE id = 1", [])
            .unwrap();
    }
    std::fs::copy(&db_path, resets_dir.join("reset-20240102T000000Z.db")).unwrap();

    // Restore the older backup by timestamp.
    stig_cmd(&dir)
        .arg("restore")
        .arg("20240101T000000Z")
        .arg("--yes")
        .assert()
        .success();

    let name: String = query_value(&dir, "SELECT name FROM users WHERE id = 1");
    assert_eq!(name, "first", "expected older backup to be restored");
}

// Missing backup errors cleanly.
#[test]
fn restore_errors_when_backup_missing() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    stig_cmd(&dir)
        .arg("restore")
        .arg("20240101T000000Z")
        .arg("--yes")
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("reset backup not found"));
}

// Invalid timestamp format is rejected as a usage error (also prevents path
// traversal in the timestamp argument).
#[test]
fn restore_errors_on_invalid_timestamp_format() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    for invalid in ["../etc/passwd", "2024-01-01", "20240101", "not-a-timestamp"] {
        stig_cmd(&dir)
            .arg("restore")
            .arg(invalid)
            .arg("--yes")
            .assert()
            .failure()
            .code(2)
            .stderr(predicate::str::contains("invalid timestamp format"));
    }
}

// No backups at all errors cleanly.
#[test]
fn restore_errors_when_no_backups_exist() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    stig_cmd(&dir)
        .arg("restore")
        .arg("--yes")
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("no reset backups found"));
}

// Declining the prompt exits 2 without changes.
#[test]
fn restore_declined_exits_2() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_foo",
        "CREATE TABLE foo (id INTEGER);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();

    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("INSERT INTO foo (id) VALUES (1)", []).unwrap();
    }

    stig_cmd(&dir).arg("reset").arg("--yes").assert().success();

    stig_cmd(&dir)
        .arg("restore")
        .write_stdin("n\n")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("operation cancelled"));

    // Database should still be in the post-reset empty state.
    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM foo", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0);
}

// --yes flag runs without prompt.
#[test]
fn restore_with_yes_flag_runs_without_prompt() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();

    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("INSERT INTO users (id, name) VALUES (1, 'test')", [])
            .unwrap();
    }

    stig_cmd(&dir).arg("reset").arg("--yes").assert().success();
    stig_cmd(&dir)
        .arg("restore")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ restored database"));

    let name: String = query_value(&dir, "SELECT name FROM users WHERE id = 1");
    assert_eq!(name, "test");
}

// In-memory database is rejected before any backup lookup.
#[test]
fn restore_exits_2_with_in_memory_database() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    stig_cmd(&dir)
        .env("STIG_DATABASE_PATH", ":memory:")
        .arg("restore")
        .arg("--yes")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains(
            "cannot restore an in-memory database",
        ));

    assert!(
        !dir.path().join(":memory:").exists(),
        "in-memory database should not create a file on disk"
    );
}
