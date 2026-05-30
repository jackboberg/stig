mod common;

use predicates::prelude::*;
use tempfile::TempDir;

use common::{stig_cmd, write_migration};

fn status_output(dir: &TempDir) -> String {
    let output = stig_cmd(dir).arg("status").output().unwrap();
    String::from_utf8(output.stdout).unwrap()
}

fn migrations_dir(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("db/migrations")
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

// ---------------------------------------------------------------------------
// 5. checksum_check=off — drift column shows "—", no drift in summary
// ---------------------------------------------------------------------------

#[test]
fn status_checksum_check_off_hides_drift() {
    let dir = TempDir::new().unwrap();

    let config = indoc::indoc! {r#"
        database_path  = "app.db"
        migrations_dir = "db/migrations"
        backups_dir    = ".local/db-backups"
        checksum_check = false
    "#};
    std::fs::write(dir.path().join("stig.toml"), config).unwrap();
    std::fs::create_dir_all(dir.path().join("db/migrations")).unwrap();
    std::fs::create_dir_all(dir.path().join(".local/db-backups/snapshots")).unwrap();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();

    // Edit the migration to create drift
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER, name TEXT);",
    );

    // Status should succeed (no exit 3) and show "—" for drifted
    let output = status_output(&dir);
    insta::assert_snapshot!("checksum_check_off", output);
}

// ---------------------------------------------------------------------------
// 6. Empty migrations directory
// ---------------------------------------------------------------------------

#[test]
fn status_empty_migrations_dir() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    let output = status_output(&dir);
    insta::assert_snapshot!("empty_migrations_dir", output);
}

// ---------------------------------------------------------------------------
// 7. Missing migrations directory — exits 4
// ---------------------------------------------------------------------------

#[test]
fn status_exits_4_when_migrations_dir_missing() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();
    std::fs::remove_dir_all(migrations_dir(&dir)).unwrap();

    stig_cmd(&dir)
        .arg("status")
        .assert()
        .failure()
        .code(predicate::eq(4))
        .stderr(predicate::str::contains("migrations directory not found"));
}

// ---------------------------------------------------------------------------
// 8. Snapshot pruned — shows "pruned" after snapshot deletion
// ---------------------------------------------------------------------------

#[test]
fn status_snapshot_pruned() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();

    // Delete the snapshot to simulate pruning
    let snapshots_dir = dir.path().join(".local/db-backups/snapshots");
    for entry in std::fs::read_dir(&snapshots_dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with("pre-") {
            std::fs::remove_file(entry.path()).unwrap();
        }
    }

    let output = status_output(&dir);
    insta::assert_snapshot!("snapshot_pruned", output);
}
