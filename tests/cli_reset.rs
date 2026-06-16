//! Integration tests for `stig reset`.

mod common;

use predicates::prelude::*;
use rusqlite::Connection;
use tempfile::TempDir;

use common::{stig_cmd, write_migration};
use filetime::FileTime;

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
    let resets_dir = dir.path().join("db/resets");
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

    let resets_dir = dir.path().join("db/resets");

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

// Prune respects reset_keep — pre-create old reset files to avoid sleeps
#[test]
fn reset_prunes_resets_beyond_keep() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_foo",
        "CREATE TABLE foo (id INTEGER);",
    );

    // Set reset_keep = 2 via config file.
    let config_path = dir.path().join("stig.toml");
    std::fs::write(&config_path, "reset_keep = 2\n").unwrap();

    // Pre-create 3 old reset backup files with distinct mtimes.
    // These simulate prior reset runs without needing to invoke the CLI
    // multiple times or sleep between them.
    let resets_dir = dir.path().join("db/resets");
    std::fs::create_dir_all(&resets_dir).unwrap();
    for i in 1u8..=3 {
        let path = resets_dir.join(format!("reset-synth-{i:03}.db"));
        std::fs::write(&path, [i]).unwrap();
        filetime::set_file_mtime(&path, FileTime::from_unix_time(1_700_000_000 + i as i64, 0))
            .unwrap();
    }

    // Run reset once; should create a 4th file and prune the 2 oldest.
    stig_cmd(&dir).arg("migrate").assert().success();
    stig_cmd(&dir).arg("reset").arg("--yes").assert().success();

    assert_eq!(
        count_reset_files(&dir),
        2,
        "expected exactly 2 reset files after reset with keep=2"
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
    let resets_dir = dir.path().join("db/resets");
    std::fs::remove_dir_all(&resets_dir).unwrap();
    assert!(!resets_dir.exists());

    stig_cmd(&dir).arg("reset").arg("--yes").assert().success();

    assert!(resets_dir.exists());
    assert_eq!(count_reset_files(&dir), 1);
}

// If reapply fails partway through, the original database is restored from
// the reset backup.
#[test]
fn reset_restores_backup_on_reapply_failure() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    // First migration: creates a table.
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Insert data that should survive a failed reset.
    let db_path = dir.path().join("app.db");
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute("INSERT INTO users (id, name) VALUES (1, 'survivor')", [])
            .unwrap();
    }

    // Add a second migration that will fail at runtime during reset reapply.
    write_migration(
        &dir,
        "20240102000000",
        "bad_insert",
        "INSERT INTO nonexistent_table VALUES (1);",
    );

    // Reset should fail because the second migration errors.
    stig_cmd(&dir).arg("reset").arg("--yes").assert().failure();

    // Original database must be restored: table exists with data.
    assert!(table_exists(&dir, "users"));
    assert_eq!(count_schema_migrations(&dir), 1);

    let conn = Connection::open(&db_path).unwrap();
    let name: String = conn
        .query_row("SELECT name FROM users WHERE id = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(name, "survivor", "expected original data to be restored");

    // Reset backup artifact still exists.
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
