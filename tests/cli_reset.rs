//! Integration tests for `stig reset`.

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

/// Count `.db` files in the resets directory.
fn count_reset_files(dir: &TempDir) -> usize {
    let resets_dir = dir.path().join(".local/db-backups/resets");
    std::fs::read_dir(&resets_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".db"))
        .count()
}

// Core acceptance: populated DB, reset --yes, fresh migrations applied
#[test]
fn reset_reapplies_all_migrations() {
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

    assert!(table_exists(&dir, "users"));
    assert!(table_exists(&dir, "posts"));
    assert_eq!(count_schema_migrations(&dir), 2);

    // Insert data that should be lost after reset.
    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("INSERT INTO users (id, name) VALUES (1, 'ephemeral')", [])
            .unwrap();
    }

    stig_cmd(&dir)
        .arg("reset")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ reset complete"));

    // Tables re-created by fresh migrations.
    assert!(table_exists(&dir, "users"));
    assert!(table_exists(&dir, "posts"));
    assert_eq!(count_schema_migrations(&dir), 2);

    // Data is gone.
    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "expected user data to be discarded after reset");

    // Reset artifact exists.
    assert_eq!(count_reset_files(&dir), 1);
}

// Reset backup artifact is created in resets/
#[test]
fn reset_creates_backup_artifact() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_foo",
        "CREATE TABLE foo (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    let resets_dir = dir.path().join(".local/db-backups/resets");

    stig_cmd(&dir).arg("reset").arg("--yes").assert().success();

    // The resets dir should contain exactly one .db file.
    let db_files: Vec<String> = std::fs::read_dir(&resets_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with("reset-") && n.ends_with(".db"))
        .collect();

    assert_eq!(
        db_files.len(),
        1,
        "expected exactly one reset backup: {db_files:?}"
    );
    assert!(
        db_files[0].starts_with("reset-") && db_files[0].ends_with("Z.db"),
        "unexpected backup name: {}",
        db_files[0]
    );
}

// --yes flag runs without prompt
#[test]
fn reset_with_yes_flag_runs_without_prompt() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_foo",
        "CREATE TABLE foo (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // --yes should run without hanging on a prompt.
    stig_cmd(&dir)
        .arg("reset")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ reset complete"));
}

// Declining the prompt exits 2 without changes
#[test]
fn reset_declined_exits_2() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_foo",
        "CREATE TABLE foo (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Pipe "n" to stdin to decline the prompt.
    stig_cmd(&dir)
        .arg("reset")
        .write_stdin("n\n")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cancelled"));

    // Database must be untouched.
    assert_eq!(count_schema_migrations(&dir), 1);
    assert!(table_exists(&dir, "foo"));
}

// Creates resets dir if missing
#[test]
fn reset_creates_resets_dir_if_missing() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_foo",
        "CREATE TABLE foo (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Remove the resets dir to verify reset recreates it.
    let resets_dir = dir.path().join(".local/db-backups/resets");
    std::fs::remove_dir(&resets_dir).unwrap();
    assert!(!resets_dir.exists());

    stig_cmd(&dir).arg("reset").arg("--yes").assert().success();

    assert!(resets_dir.exists());
    assert_eq!(count_reset_files(&dir), 1);
}

// Decline then accept — full round-trip
#[test]
fn reset_declined_then_accepted() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_foo",
        "CREATE TABLE foo (id INTEGER);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();

    // Decline.
    stig_cmd(&dir)
        .arg("reset")
        .write_stdin("n\n")
        .assert()
        .failure()
        .code(2);

    // State preserved.
    assert_eq!(count_schema_migrations(&dir), 1);
    assert!(table_exists(&dir, "foo"));

    // Accept.
    stig_cmd(&dir).arg("reset").arg("--yes").assert().success();

    // Schema rebuilt.
    assert_eq!(count_schema_migrations(&dir), 1);
    assert!(table_exists(&dir, "foo"));
    assert_eq!(count_reset_files(&dir), 1);
}
