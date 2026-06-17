//! Cross-command integration tests — multi-step workflows that exercise
//! several `stig` commands in sequence.

mod common;

use predicates::prelude::*;
use rusqlite::Connection;
use tempfile::TempDir;

use common::{stig_cmd, write_migration};

// Helpers

fn count_schema_migrations(dir: &TempDir) -> i64 {
    let conn = Connection::open(dir.path().join("app.db")).unwrap();
    conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap()
}

fn table_exists(dir: &TempDir, name: &str) -> bool {
    let conn = Connection::open(dir.path().join("app.db")).unwrap();
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |r| r.get(0),
        )
        .unwrap();
    count > 0
}

fn count_reset_files(dir: &TempDir) -> usize {
    let resets_dir = dir.path().join("db/resets");
    if !resets_dir.exists() {
        return 0;
    }
    std::fs::read_dir(&resets_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".db"))
        .count()
}

// DATABASE_PATH fallback env var
#[test]
fn env_database_path_fallback() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    let custom_db = dir.path().join("custom.db");
    stig_cmd(&dir)
        .env("DATABASE_PATH", custom_db.to_str().unwrap())
        .arg("migrate")
        .assert()
        .success();

    // The custom DB should have been used (schema_migrations exists).
    let conn = Connection::open(&custom_db).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 0, "fresh DB should have no migrations");
}

// Config upward search from subdirectory
#[test]
fn config_upward_search_from_subdir() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();

    // Create a subdirectory and run status from there.
    let subdir = dir.path().join("src").join("db");
    std::fs::create_dir_all(&subdir).unwrap();

    stig_cmd(&dir)
        .current_dir(&subdir)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("1 applied, 0 pending, 0 drifted"));
}

// Full dev-iteration cycle: init → migrate → drift → redo → generate
#[test]
fn full_dev_iteration_cycle() {
    let dir = TempDir::new().unwrap();

    // Step 1: init
    stig_cmd(&dir).arg("init").assert().success();

    // Step 2: new migration (via file, simulating $EDITOR)
    write_migration(
        &dir,
        "20240101000000",
        "add_widgets",
        "CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT);",
    );

    // Step 3: migrate
    stig_cmd(&dir).arg("migrate").assert().success();
    assert!(table_exists(&dir, "widgets"));
    assert_eq!(count_schema_migrations(&dir), 1);

    // Step 4: realize schema is wrong, edit migration
    write_migration(
        &dir,
        "20240101000000",
        "add_widgets",
        "CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT NOT NULL, price REAL);",
    );

    // Step 5: migrate detects drift
    stig_cmd(&dir)
        .arg("migrate")
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("run: stig redo"));

    // Step 6: redo restores and re-applies
    stig_cmd(&dir)
        .arg("redo")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ redo complete"));

    // Verify the new schema is active.
    assert!(table_exists(&dir, "widgets"));
    let conn = Connection::open(dir.path().join("app.db")).unwrap();
    let has_price: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pragma_table_info('widgets') WHERE name='price'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(has_price, 1, "price column should exist after redo");

    // Step 7: generate types
    let toml = indoc::indoc! {r#"
        migrations_dir = "db/migrations"
        database_path  = "app.db"

        [[generate]]
        kind = "typescript"
        path = "types.ts"
    "#};
    std::fs::write(dir.path().join("stig.toml"), toml).unwrap();
    stig_cmd(&dir).arg("generate").assert().success();

    let ts_path = dir.path().join("types.ts");
    assert!(ts_path.exists());
    let ts = std::fs::read_to_string(&ts_path).unwrap();
    assert!(ts.contains("\"widgets\""));
    assert!(ts.contains("\"price\": number"));
}

// Reset and re-migrate cycle: migrate → insert data → reset → verify
#[test]
fn reset_and_re_migrate_cycle() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_items",
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT);",
    );
    write_migration(
        &dir,
        "20240102000000",
        "create_tags",
        "CREATE TABLE tags (id INTEGER PRIMARY KEY, label TEXT);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();
    assert_eq!(count_schema_migrations(&dir), 2);

    // Insert data that should be destroyed by reset.
    {
        let conn = Connection::open(dir.path().join("app.db")).unwrap();
        conn.execute("INSERT INTO items (name) VALUES ('ephemeral')", [])
            .unwrap();
    }

    // Reset.
    stig_cmd(&dir)
        .arg("reset")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ reset complete"));

    // Schema rebuilt.
    assert!(table_exists(&dir, "items"));
    assert!(table_exists(&dir, "tags"));
    assert_eq!(count_schema_migrations(&dir), 2);

    // Data is gone.
    let count: i64 = {
        let conn = Connection::open(dir.path().join("app.db")).unwrap();
        conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count, 0, "data should be gone after reset");

    // Reset artifact exists.
    assert_eq!(count_reset_files(&dir), 1);
}

// Status accuracy across multiple states
#[test]
fn status_reports_correctly_after_each_state() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    // State 1: empty migrations dir.
    let output = stig_cmd(&dir).arg("status").output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("0 applied, 0 pending"));

    // State 2: one migration, not yet applied (pending).
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );
    let output = stig_cmd(&dir).arg("status").output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("0 applied, 1 pending"));

    // State 3: applied.
    stig_cmd(&dir).arg("migrate").assert().success();
    let output = stig_cmd(&dir).arg("status").output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("1 applied, 0 pending, 0 drifted"));

    // State 4: drift.
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER, name TEXT);",
    );
    stig_cmd(&dir).arg("status").assert().failure().code(3);

    // State 5: orphan (applied row with no file).
    // Fix the drift first.
    stig_cmd(&dir).arg("redo").arg("--yes").assert().success();
    {
        let conn = Connection::open(dir.path().join("app.db")).unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, checksum) VALUES ('20240102000000_orphan', 'deadbeef')",
            [],
        )
        .unwrap();
    }
    let output = stig_cmd(&dir).arg("status").output().unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("2 applied"));
}

// Generate reflects schema changes across migrations
#[test]
fn generate_reflects_schema_changes() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    let toml = indoc::indoc! {r#"
        migrations_dir = "db/migrations"
        database_path  = "app.db"

        [[generate]]
        kind = "typescript"
        path = "types.ts"
    "#};
    std::fs::write(dir.path().join("stig.toml"), toml).unwrap();

    // First migration: users table.
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();
    stig_cmd(&dir).arg("generate").assert().success();

    let ts1 = std::fs::read_to_string(dir.path().join("types.ts")).unwrap();
    assert!(ts1.contains("\"users\""));
    assert!(!ts1.contains("\"posts\""));

    // Second migration: posts table.
    write_migration(
        &dir,
        "20240102000000",
        "create_posts",
        "CREATE TABLE posts (id INTEGER PRIMARY KEY, title TEXT NOT NULL);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();
    stig_cmd(&dir).arg("generate").assert().success();

    let ts2 = std::fs::read_to_string(dir.path().join("types.ts")).unwrap();
    assert!(ts2.contains("\"users\""));
    assert!(ts2.contains("\"posts\""));
}

// Redo re-applies from middle of chain
#[test]
fn redo_reapplies_from_middle_of_chain() {
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
    write_migration(
        &dir,
        "20240103000000",
        "create_tags",
        "CREATE TABLE tags (id INTEGER);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();
    assert_eq!(count_schema_migrations(&dir), 3);

    // Edit the second migration.
    write_migration(
        &dir,
        "20240102000000",
        "create_posts",
        "CREATE TABLE articles (id INTEGER);",
    );

    // Redo from second migration.
    stig_cmd(&dir)
        .arg("redo")
        .arg("20240102000000_create_posts")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ redo complete"));

    // First table untouched.
    assert!(table_exists(&dir, "users"));
    // Old table gone, new table present.
    assert!(!table_exists(&dir, "posts"));
    assert!(table_exists(&dir, "articles"));
    // Third migration re-applied.
    assert!(table_exists(&dir, "tags"));
    assert_eq!(count_schema_migrations(&dir), 3);
}

// Locked DB fails with exit code 5.
#[test]
fn locked_db_exits_5() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    // Switch to DELETE journal mode so reads block on an exclusive writer lock
    // (the default WAL mode allows concurrent reads while a writer holds the
    // database lock, which would let `stig status` succeed).
    std::fs::write(
        dir.path().join("stig.toml"),
        "[pragmas]\njournal_mode = \"DELETE\"\n",
    )
    .unwrap();

    // Open the DB exclusively to lock it.
    let db_path = dir.path().join("app.db");
    let _lock = Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_FULL_MUTEX,
    )
    .unwrap();

    // Start a write transaction to hold the lock.
    _lock
        .execute_batch("BEGIN EXCLUSIVE; CREATE TABLE IF NOT EXISTS lock_holder (id INTEGER);")
        .unwrap();

    // Both migrate and status should surface the SQLite lock as exit code 5.
    stig_cmd(&dir).arg("migrate").assert().code(5);
    stig_cmd(&dir).arg("status").assert().code(5);

    // Clean up: rollback so TempDir cleanup can delete the file.
    _lock.execute_batch("ROLLBACK;").ok();
}

// Custom paths via config
#[test]
fn custom_paths_via_config() {
    let dir = TempDir::new().unwrap();

    let config = indoc::indoc! {r#"
        database_path  = "data/my-app.db"
        migrations_dir = "schema/migrations"
        backups_dir    = "backups"
        snapshot_keep  = 5
        auto_snapshot  = true
        checksum_check = true
    "#};
    std::fs::write(dir.path().join("stig.toml"), config).unwrap();
    std::fs::create_dir_all(dir.path().join("schema/migrations")).unwrap();
    std::fs::create_dir_all(dir.path().join("data")).unwrap();
    std::fs::create_dir_all(dir.path().join("backups/snapshots")).unwrap();

    // Write migration to the custom migrations directory.
    std::fs::write(
        dir.path()
            .join("schema/migrations/20240101000000_create_users.sql"),
        "CREATE TABLE users (id INTEGER);",
    )
    .unwrap();

    stig_cmd(&dir).arg("migrate").assert().success();

    // DB created at custom path.
    assert!(dir.path().join("data/my-app.db").exists());

    // Snapshot created at custom path.
    let snaps = dir.path().join("backups/snapshots");
    let snap_entries: Vec<String> = std::fs::read_dir(&snaps)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        snap_entries
            .iter()
            .any(|n| n.starts_with("pre-") && n.ends_with(".db")),
        "expected a snapshot in backups/snapshots, found: {snap_entries:?}"
    );

    // Status works with custom paths.
    stig_cmd(&dir)
        .arg("status")
        .assert()
        .success()
        .stdout(predicate::str::contains("1 applied, 0 pending"));
}

// Redo after pruned snapshot lists eligible versions
#[test]
fn redo_after_pruned_snapshot_lists_eligible() {
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
    stig_cmd(&dir).arg("migrate").assert().success();

    // Delete the first snapshot (simulate pruning).
    let snaps_dir = dir.path().join("db/snapshots");
    std::fs::remove_file(snaps_dir.join("pre-20240101000000_first.db")).unwrap();

    // Try to redo from the first migration — snapshot is gone.
    stig_cmd(&dir)
        .arg("redo")
        .arg("20240101000000_first")
        .arg("--yes")
        .assert()
        .failure()
        .code(4)
        .stderr(predicate::str::contains(
            "snapshot pre-20240101000000_first.db not found",
        ))
        .stderr(predicate::str::contains("redo-eligible versions:"))
        .stderr(predicate::str::contains("20240102000000_second"));
}

// Migrate → reset → redo compose
#[test]
fn migrate_reset_redo_compose() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_items",
        "CREATE TABLE items (id INTEGER PRIMARY KEY, name TEXT);",
    );

    // Migrate.
    stig_cmd(&dir).arg("migrate").assert().success();
    assert!(table_exists(&dir, "items"));

    // Insert data.
    {
        let conn = Connection::open(dir.path().join("app.db")).unwrap();
        conn.execute("INSERT INTO items (name) VALUES ('post-migrate')", [])
            .unwrap();
    }

    // Reset.
    stig_cmd(&dir).arg("reset").arg("--yes").assert().success();
    assert!(table_exists(&dir, "items"));
    let count: i64 = {
        let conn = Connection::open(dir.path().join("app.db")).unwrap();
        conn.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count, 0, "data should be gone after reset");

    // Redo (should restore snapshot from reset's re-migrate and re-apply).
    stig_cmd(&dir)
        .arg("redo")
        .arg("--yes")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ redo complete"));

    assert!(table_exists(&dir, "items"));
    assert_eq!(count_schema_migrations(&dir), 1);
}
