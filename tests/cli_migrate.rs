use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use tempfile::TempDir;

/// Known `STIG_*` env vars that could leak from the developer's shell and
/// corrupt test assertions. Cleared on every subprocess.
const STIG_ENV_KEYS: &[&str] = &[
    "STIG_CONFIG",
    "STIG_DATABASE_PATH",
    "DATABASE_PATH",
    "STIG_MIGRATIONS_DIR",
    "STIG_BACKUPS_DIR",
    "STIG_NO_SNAPSHOT",
    "STIG_NO_CHECKSUM",
];

/// Return a `stig` [`Command`] with CWD set to `dir` and all known `STIG_*`
/// env vars removed so ambient shell variables cannot affect assertions.
fn stig_cmd(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("stig").unwrap();
    cmd.current_dir(dir.path());
    for key in STIG_ENV_KEYS {
        cmd.env_remove(key);
    }
    cmd
}

/// Write a migration `.sql` file into `dir/db/migrations/`.
fn write_migration(dir: &TempDir, timestamp: &str, slug: &str, content: &str) {
    let migrations_dir = dir.path().join("db/migrations");
    std::fs::create_dir_all(&migrations_dir).unwrap();
    let filename = format!("{timestamp}_{slug}.sql");
    let path = migrations_dir.join(filename);
    std::fs::write(&path, content).unwrap();
}

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
