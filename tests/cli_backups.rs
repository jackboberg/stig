//! Integration tests for `stig backups`.

mod common;

use predicates::prelude::*;
use tempfile::TempDir;

use common::{stig_cmd, write_migration};

// List with no backups
#[test]
fn backups_list_empty() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    let output = stig_cmd(&dir).arg("backups").arg("list").assert().success();

    output
        .stdout(predicate::str::contains("snapshots (0 of max 5):"))
        .stdout(predicate::str::contains("resets (0 of max 3):"));
}

// List after migrate shows snapshots
#[test]
fn backups_list_populated() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_foo",
        "CREATE TABLE foo (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    let output = stig_cmd(&dir).arg("backups").arg("list").assert().success();

    output
        .stdout(predicate::str::contains("snapshots (1 of max 5):"))
        .stdout(predicate::str::contains("pre-20240101000000_create_foo.db"))
        .stdout(predicate::str::contains("KiB"))
        .stdout(predicate::str::contains("ago"));
}

// List after reset shows resets
#[test]
fn backups_list_after_reset() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_foo",
        "CREATE TABLE foo (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();
    stig_cmd(&dir).arg("reset").arg("--yes").assert().success();

    let output = stig_cmd(&dir).arg("backups").arg("list").assert().success();

    output
        .stdout(predicate::str::contains("resets (1 of max 3):"))
        .stdout(predicate::str::contains("reset-"))
        .stdout(predicate::str::contains(".db"));
}

// Prune with --yes removes old snapshots
#[test]
fn backups_prune_yes() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "first",
        "CREATE TABLE first (id INTEGER);",
    );
    write_migration(
        &dir,
        "20240102000000",
        "second",
        "CREATE TABLE second (id INTEGER);",
    );
    write_migration(
        &dir,
        "20240103000000",
        "third",
        "CREATE TABLE third (id INTEGER);",
    );

    // Migrate with default snapshot_keep (5) — all 3 snapshots are kept.
    stig_cmd(&dir).arg("migrate").assert().success();

    let snapshots_dir = dir.path().join("db/snapshots");
    assert_eq!(
        count_db_files(&snapshots_dir),
        3,
        "expected 3 snapshots before prune"
    );

    // Lower keep policy and prune.
    let config_path = dir.path().join("stig.toml");
    std::fs::write(&config_path, "snapshot_keep = 2\n").unwrap();

    stig_cmd(&dir)
        .arg("backups")
        .arg("prune")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ backups pruned"));

    assert_eq!(
        count_db_files(&snapshots_dir),
        2,
        "expected 2 snapshots after prune"
    );
}

// Prune without --yes, declined via stdin
#[test]
fn backups_prune_declined() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_foo",
        "CREATE TABLE foo (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    let snapshots_dir = dir.path().join("db/snapshots");
    assert_eq!(count_db_files(&snapshots_dir), 1);

    stig_cmd(&dir)
        .arg("backups")
        .arg("prune")
        .write_stdin("n\n")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("cancelled"));

    // Snapshot untouched.
    assert_eq!(count_db_files(&snapshots_dir), 1);
}

// Helpers

/// Count `.db` files in a directory.
fn count_db_files(dir: &std::path::Path) -> usize {
    if !dir.exists() {
        return 0;
    }
    std::fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".db"))
        .count()
}

// List shows correct counts after migrate then reset
#[test]
fn backups_list_counts_after_migrate_and_reset() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_foo",
        "CREATE TABLE foo (id INTEGER);",
    );

    // After migrate: 1 snapshot, 0 resets.
    stig_cmd(&dir).arg("migrate").assert().success();
    stig_cmd(&dir)
        .arg("backups")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("snapshots (1 of max 5):"))
        .stdout(predicate::str::contains("resets (0 of max 3):"));

    // After reset: 1 snapshot, 1 reset.
    stig_cmd(&dir).arg("reset").arg("--yes").assert().success();
    stig_cmd(&dir)
        .arg("backups")
        .arg("list")
        .assert()
        .success()
        .stdout(predicate::str::contains("snapshots (1 of max 5):"))
        .stdout(predicate::str::contains("resets (1 of max 3):"));
}
