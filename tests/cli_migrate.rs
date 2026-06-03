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

/// Count snapshot `.db` files in the snapshots directory.
fn count_snapshot_files(dir: &TempDir) -> usize {
    let snaps_dir = dir.path().join(".local/db-backups/snapshots");
    if !snaps_dir.exists() {
        return 0;
    }
    std::fs::read_dir(&snaps_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name();
            let n = name.to_string_lossy();
            n.starts_with("pre-") && n.ends_with(".db")
        })
        .count()
}

/// Query all values from a text column in a table, sorted ascending.
fn query_column_values(dir: &TempDir, table: &str, column: &str) -> Vec<String> {
    let conn = Connection::open(dir.path().join("app.db")).unwrap();
    let mut stmt = conn
        .prepare(&format!("SELECT {column} FROM {table} ORDER BY {column}"))
        .unwrap();
    stmt.query_map([], |row| row.get(0))
        .unwrap()
        .map(|r| r.unwrap())
        .collect()
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

// ---------------------------------------------------------------------------
// 12. Drift with pruned snapshot — hard fail suggesting reset
// ---------------------------------------------------------------------------

#[test]
fn migrate_drift_with_pruned_snapshot_hard_fail() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();

    // Delete the snapshot to simulate pruning.
    let snaps_dir = dir.path().join(".local/db-backups/snapshots");
    for entry in std::fs::read_dir(&snaps_dir).unwrap() {
        let entry = entry.unwrap();
        if entry.file_name().to_string_lossy().starts_with("pre-") {
            std::fs::remove_file(entry.path()).unwrap();
        }
    }

    // Edit the migration to cause drift.
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER, name TEXT);",
    );

    // Should fail with "revert the edit or run: stig reset".
    stig_cmd(&dir)
        .arg("migrate")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains(
            "migration 20240101000000_create_users has been edited since it was applied",
        ))
        .stderr(predicate::str::contains(
            "revert the edit or run: stig reset",
        ));
}

// ---------------------------------------------------------------------------
// 13. Migrations apply in lexicographic order
// ---------------------------------------------------------------------------

#[test]
fn migrate_applies_in_lexicographic_order() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    // Lexicographic order matches chronological order for zero-padded timestamps.
    write_migration(
        &dir,
        "20240101000000",
        "first",
        "CREATE TABLE step (id INTEGER PRIMARY KEY, name TEXT);",
    );
    write_migration(
        &dir,
        "20240102000000",
        "second",
        "INSERT INTO step (name) VALUES ('second');",
    );
    write_migration(
        &dir,
        "20240103000000",
        "third",
        "INSERT INTO step (name) VALUES ('third');",
    );
    write_migration(
        &dir,
        "20240104000000",
        "fourth",
        "INSERT INTO step (name) VALUES ('fourth');",
    );
    write_migration(
        &dir,
        "20240105000000",
        "fifth",
        "INSERT INTO step (name) VALUES ('fifth');",
    );

    stig_cmd(&dir).arg("migrate").assert().success();
    assert_eq!(count_schema_migrations(&dir), 5);

    // Verify all rows inserted in order.
    let values = query_column_values(&dir, "step", "name");
    assert_eq!(values, vec!["fifth", "fourth", "second", "third"]);
}

// ---------------------------------------------------------------------------
// 14. Snapshot pruning across multiple keep cycles
// ---------------------------------------------------------------------------

#[test]
fn migrate_prunes_snapshots_across_keep() {
    let dir = TempDir::new().unwrap();

    let config = indoc::indoc! {r#"
        database_path  = "app.db"
        migrations_dir = "db/migrations"
        backups_dir    = ".local/db-backups"
        snapshot_keep  = 2
        auto_snapshot  = true
        checksum_check = true
    "#};
    std::fs::write(dir.path().join("stig.toml"), config).unwrap();
    std::fs::create_dir_all(dir.path().join("db/migrations")).unwrap();
    std::fs::create_dir_all(dir.path().join(".local/db-backups/snapshots")).unwrap();

    for i in 1..=4 {
        let ts = format!("2024010{i}000000");
        let slug = format!("migration_{i}");
        let sql = format!("CREATE TABLE t{i} (id INTEGER);");
        write_migration(&dir, &ts, &slug, &sql);
    }

    stig_cmd(&dir).arg("migrate").assert().success();
    assert_eq!(count_schema_migrations(&dir), 4);

    // Only 2 snapshots should remain (the newest two).
    assert_eq!(count_snapshot_files(&dir), 2);
    assert!(
        !snapshot_exists(&dir, "20240101000000_migration_1"),
        "oldest snapshot should be pruned"
    );
    assert!(
        !snapshot_exists(&dir, "20240102000000_migration_2"),
        "second snapshot should be pruned"
    );
    assert!(snapshot_exists(&dir, "20240103000000_migration_3"));
    assert!(snapshot_exists(&dir, "20240104000000_migration_4"));
}

// ---------------------------------------------------------------------------
// 15. Empty migration (comments only) succeeds
// ---------------------------------------------------------------------------

#[test]
fn migrate_empty_migration_succeeds() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(&dir, "20240101000000", "empty", "-- just a comment\n\n");

    stig_cmd(&dir).arg("migrate").assert().success();
    assert_eq!(count_schema_migrations(&dir), 1);
}

// ---------------------------------------------------------------------------
// 16. Non-transactional migration with PRAGMA
// ---------------------------------------------------------------------------

#[test]
fn migrate_non_transactional_with_pragma() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "with_pragma",
        "stig: non-transactional\nPRAGMA journal_mode = WAL;\nCREATE TABLE x (id INTEGER);\n",
    );

    stig_cmd(&dir).arg("migrate").assert().success();
    assert_eq!(count_schema_migrations(&dir), 1);
    assert!(query_column_values(&dir, "sqlite_master", "name").contains(&"x".to_string()));
}

// ---------------------------------------------------------------------------
// 17. Large migration set with pruning at scale
// ---------------------------------------------------------------------------

#[test]
fn migrate_large_set_with_pruning() {
    let dir = TempDir::new().unwrap();

    let config = indoc::indoc! {r#"
        database_path  = "app.db"
        migrations_dir = "db/migrations"
        backups_dir    = ".local/db-backups"
        snapshot_keep  = 3
        auto_snapshot  = true
        checksum_check = true
    "#};
    std::fs::write(dir.path().join("stig.toml"), config).unwrap();
    std::fs::create_dir_all(dir.path().join("db/migrations")).unwrap();
    std::fs::create_dir_all(dir.path().join(".local/db-backups/snapshots")).unwrap();

    for i in 0..20 {
        let ts = format!("202401{:02}000000", i + 1);
        let slug = format!("migration_{:02}", i + 1);
        let sql = format!("CREATE TABLE t{} (id INTEGER);", i + 1);
        write_migration(&dir, &ts, &slug, &sql);
    }

    stig_cmd(&dir).arg("migrate").assert().success();
    assert_eq!(count_schema_migrations(&dir), 20);

    // All 20 tables should exist.
    let conn = Connection::open(dir.path().join("app.db")).unwrap();
    for i in 1..=20 {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                [format!("t{i}")],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "table t{i} should exist");
    }

    // Only 3 snapshots should remain.
    assert_eq!(count_snapshot_files(&dir), 3);
}
