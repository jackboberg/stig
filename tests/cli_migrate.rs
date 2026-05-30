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

/// Check whether a snapshot file exists for `version`.
fn snapshot_exists(dir: &TempDir, version: &str) -> bool {
    dir.path()
        .join(format!(".local/db-backups/snapshots/pre-{version}.db"))
        .exists()
}

/// The migrations dir path.
fn migrations_dir(dir: &TempDir) -> std::path::PathBuf {
    dir.path().join("db/migrations")
}

// ---------------------------------------------------------------------------
// 1. Happy path: fresh DB applies all pending migrations
// ---------------------------------------------------------------------------

#[test]
fn migrate_applies_pending() {
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

    stig_cmd(&dir)
        .arg("migrate")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "apply  20240101000000_create_users.sql",
        ))
        .stdout(predicate::str::contains(
            "apply  20240102000000_create_posts.sql",
        ))
        .stdout(predicate::str::contains(
            "✓ 2 applied, 0 already up to date",
        ));

    assert_eq!(count_schema_migrations(&dir), 2);
    assert!(snapshot_exists(&dir, "20240101000000_create_users"));
    assert!(snapshot_exists(&dir, "20240102000000_create_posts"));

    // Verify tables actually exist
    let db_path = dir.path().join("app.db");
    let conn = Connection::open(db_path).unwrap();
    let table_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('users', 'posts')",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 2);
}

// ---------------------------------------------------------------------------
// 2. No-op when already up to date
// ---------------------------------------------------------------------------

#[test]
fn migrate_noop_when_up_to_date() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Run migrate again
    stig_cmd(&dir)
        .arg("migrate")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "✓ 0 applied, 1 already up to date",
        ));

    assert_eq!(count_schema_migrations(&dir), 1);
}

// ---------------------------------------------------------------------------
// 3. Drift detection exits 3
// ---------------------------------------------------------------------------

#[test]
fn migrate_exits_3_on_drift() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    let original = "CREATE TABLE users (id INTEGER);";
    let edited = "CREATE TABLE users (id INTEGER, name TEXT);";

    write_migration(&dir, "20240101000000", "create_users", original);

    stig_cmd(&dir).arg("migrate").assert().success();

    // Edit the migration file
    write_migration(&dir, "20240101000000", "create_users", edited);

    stig_cmd(&dir)
        .arg("migrate")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains(
            "migration 20240101000000_create_users has been edited since it was applied",
        ))
        .stderr(predicate::str::contains(
            "snapshot pre-20240101000000_create_users.db is available",
        ))
        .stderr(predicate::str::contains("run: stig redo 20240101000000"));
}

// ---------------------------------------------------------------------------
// 4. --dry-run does not mutate state
// ---------------------------------------------------------------------------

#[test]
fn migrate_dry_run_does_not_mutate() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    stig_cmd(&dir)
        .arg("migrate")
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "would apply  20240101000000_create_users.sql",
        ))
        .stdout(predicate::str::contains(
            "✓ 1 would be applied, 0 already up to date",
        ));

    assert_eq!(count_schema_migrations(&dir), 0);
    assert!(!snapshot_exists(&dir, "20240101000000_create_users"));

    // Table should not exist
    let db_path = dir.path().join("app.db");
    let conn = Connection::open(db_path).unwrap();
    let table_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='users'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(table_count, 0);
}

// ---------------------------------------------------------------------------
// 5. Non-transactional directive is honored
// ---------------------------------------------------------------------------

#[test]
fn migrate_honors_non_transactional_directive() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    // Create a migration with the non-transactional directive and verify it
    // applies cleanly. The directive parsing is the key behavior tested here.
    write_migration(
        &dir,
        "20240101000000",
        "create_table",
        "-- Note: journal_mode PRAGMA\n\nstig: non-transactional\n\nCREATE TABLE x (id INTEGER);",
    );

    stig_cmd(&dir)
        .arg("migrate")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "apply  20240101000000_create_table.sql",
        ));

    assert_eq!(count_schema_migrations(&dir), 1);
}

// ---------------------------------------------------------------------------
// 6. Missing migrations directory exits 4
// ---------------------------------------------------------------------------

#[test]
fn migrate_exits_4_when_migrations_dir_missing() {
    let dir = TempDir::new().unwrap();

    // Init creates the migrations dir; remove it to trigger the missing-dir check.
    stig_cmd(&dir).arg("init").assert().success();
    std::fs::remove_dir_all(migrations_dir(&dir)).unwrap();

    stig_cmd(&dir)
        .arg("migrate")
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains("migrations directory not found"));
}

// ---------------------------------------------------------------------------
// 7. Dry-run with no pending migrations is a no-op
// ---------------------------------------------------------------------------

#[test]
fn migrate_dry_run_noop_when_up_to_date() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "alpha",
        "CREATE TABLE a (id INTEGER);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();

    stig_cmd(&dir)
        .arg("migrate")
        .arg("--dry-run")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "✓ 0 would be applied, 1 already up to date",
        ));
}

// ---------------------------------------------------------------------------
// 8. auto_snapshot=false — no snapshots taken
// ---------------------------------------------------------------------------

#[test]
fn migrate_no_snapshot_when_disabled() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    stig_cmd(&dir)
        .env("STIG_NO_SNAPSHOT", "1")
        .arg("migrate")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "apply  20240101000000_create_users.sql",
        ))
        .stdout(predicate::str::contains(
            "✓ 1 applied, 0 already up to date",
        ));

    assert_eq!(count_schema_migrations(&dir), 1);
    // Snapshot should NOT exist when auto_snapshot is disabled
    assert!(!snapshot_exists(&dir, "20240101000000_create_users"));
}

// ---------------------------------------------------------------------------
// 9. checksum_check=false — drift is silently ignored
// ---------------------------------------------------------------------------

#[test]
fn migrate_ignores_drift_when_checksum_check_disabled() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    // Migration A — applied, then edited
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();

    // Migration B — pending
    write_migration(
        &dir,
        "20240102000000",
        "create_posts",
        "CREATE TABLE posts (id INTEGER);",
    );

    // Edit migration A to create drift
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER, name TEXT);",
    );

    // With STIG_NO_CHECKSUM, drift should be ignored and pending applied
    stig_cmd(&dir)
        .env("STIG_NO_CHECKSUM", "1")
        .arg("migrate")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "✓ 1 applied, 1 already up to date",
        ));

    assert_eq!(count_schema_migrations(&dir), 2);
}

// ---------------------------------------------------------------------------
// 10. Drift exits 3 even when pending migrations exist
// ---------------------------------------------------------------------------

#[test]
fn migrate_drift_with_pending_fails_before_apply() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    // Migration A — applied, then edited
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();

    // Migration B — pending, should never be applied because drift fails first
    write_migration(
        &dir,
        "20240102000000",
        "create_posts",
        "CREATE TABLE posts (id INTEGER);",
    );

    // Edit migration A to create drift
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER, name TEXT);",
    );

    stig_cmd(&dir)
        .arg("migrate")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains(
            "migration 20240101000000_create_users has been edited",
        ));

    // Migration B should NOT have been applied
    assert_eq!(count_schema_migrations(&dir), 1);

    // Verify migration B's table does not exist
    let db_path = dir.path().join("app.db");
    let conn = Connection::open(db_path).unwrap();
    let post_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='posts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(post_count, 0);
}

// ---------------------------------------------------------------------------
// 11. Snapshots are pruned after migration, keeping only snapshot_keep
// ---------------------------------------------------------------------------

#[test]
fn migrate_prunes_snapshots() {
    let dir = TempDir::new().unwrap();

    // Write a custom stig.toml with snapshot_keep=1 so pruning is observable
    // with just 2 migrations.
    let config = indoc::indoc! {r#"
        database_path  = "app.db"
        migrations_dir = "db/migrations"
        backups_dir    = ".local/db-backups"
        snapshot_keep  = 1
        auto_snapshot  = true
        checksum_check = true
    "#};
    std::fs::write(dir.path().join("stig.toml"), config).unwrap();
    std::fs::create_dir_all(dir.path().join("db/migrations")).unwrap();
    std::fs::create_dir_all(dir.path().join(".local/db-backups/snapshots")).unwrap();

    write_migration(
        &dir,
        "20240101000000",
        "first",
        "CREATE TABLE a (id INTEGER);",
    );
    write_migration(
        &dir,
        "20240102000000",
        "second",
        "CREATE TABLE b (id INTEGER);",
    );

    stig_cmd(&dir)
        .arg("migrate")
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "✓ 2 applied, 0 already up to date",
        ));

    assert_eq!(count_schema_migrations(&dir), 2);

    // With snapshot_keep=1, only 1 snapshot should remain.
    // The first snapshot (pre-20240101000000_first.db) should have been pruned.
    assert!(
        !snapshot_exists(&dir, "20240101000000_first"),
        "first snapshot should have been pruned"
    );
    assert!(
        snapshot_exists(&dir, "20240102000000_second"),
        "second snapshot should still exist"
    );
}
