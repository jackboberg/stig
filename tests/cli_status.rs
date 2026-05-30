mod common;

use predicates::prelude::*;
use tempfile::TempDir;

use common::{stig_cmd, write_migration};

fn status_output(dir: &TempDir) -> String {
    let output = stig_cmd(dir).arg("status").output().unwrap();
    String::from_utf8(output.stdout).unwrap()
}

// ---------------------------------------------------------------------------
// 1. All applied — no pending, no drift
// ---------------------------------------------------------------------------

#[test]
fn status_all_applied() {
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

    stig_cmd(&dir).arg("migrate").assert().success();

    let output = status_output(&dir);
    insta::assert_snapshot!("all_applied", output);
}

// ---------------------------------------------------------------------------
// 2. Pending present — some not yet applied
// ---------------------------------------------------------------------------

#[test]
fn status_pending_present() {
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

    // Apply only the first migration by using a second DB that skips the second
    // Actually, stig migrate applies all pending. We need a different approach.
    // Run migrate, then add a new file so it shows as pending.
    stig_cmd(&dir).arg("migrate").assert().success();

    write_migration(
        &dir,
        "20240103000000",
        "add_comments",
        "CREATE TABLE comments (id INTEGER);",
    );

    let output = status_output(&dir);
    insta::assert_snapshot!("pending_present", output);
}

// ---------------------------------------------------------------------------
// 3. Drift present — exits 3
// ---------------------------------------------------------------------------

#[test]
fn status_drift_exits_3() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Edit the migration file to cause drift
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER, name TEXT);",
    );

    stig_cmd(&dir)
        .arg("status")
        .assert()
        .failure()
        .code(predicate::eq(3));
}

// ---------------------------------------------------------------------------
// 4. Orphan-applied — DB row with no file on disk
// ---------------------------------------------------------------------------

#[test]
fn status_orphan_applied() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Insert an orphan row directly into the DB
    let db_path = dir.path().join("app.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute(
        "INSERT INTO schema_migrations (version, checksum) VALUES (?1, ?2)",
        rusqlite::params!["20240102000000_orphan", "deadbeef"],
    )
    .unwrap();
    drop(conn);

    // Remove the file for the first migration so it also becomes an orphan
    // Actually, let's keep it simpler: just add an orphan row, the original
    // migration still has its file so it shows as applied normally.
    let output = status_output(&dir);
    insta::assert_snapshot!("orphan_applied", output);
}
