//! Integration tests for `stig init`.

use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use tempfile::TempDir;

/// Helper: run `stig init [args]` with the given working directory.
fn stig_init(dir: &TempDir, extra_args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = Command::cargo_bin("stig").unwrap();
    cmd.current_dir(dir.path()).arg("init");
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.assert()
}

// ---------------------------------------------------------------------------
// 1. Happy path: empty temp dir produces all expected artifacts
// ---------------------------------------------------------------------------

#[test]
fn init_creates_all_expected_artifacts() {
    let dir = TempDir::new().unwrap();

    stig_init(&dir, &[]).success();

    // stig.toml was written.
    assert!(dir.path().join("stig.toml").is_file(), "stig.toml missing");

    // Migrations directory was created.
    assert!(
        dir.path().join("db/migrations").is_dir(),
        "db/migrations missing"
    );

    // Backups subdirectories were created.
    assert!(
        dir.path().join(".local/db-backups/snapshots").is_dir(),
        ".local/db-backups/snapshots missing"
    );
    assert!(
        dir.path().join(".local/db-backups/resets").is_dir(),
        ".local/db-backups/resets missing"
    );

    // .gitignore was written with a wildcard.
    let gitignore = std::fs::read_to_string(dir.path().join(".local/db-backups/.gitignore"))
        .expect(".gitignore missing");
    assert!(gitignore.contains('*'), ".gitignore should contain '*'");

    // schema_migrations table is queryable in the default database.
    let db_path = dir.path().join("app.db");
    assert!(db_path.is_file(), "app.db missing");
    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .expect("schema_migrations should be queryable");
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// 2. Re-run without --force exits 2 and leaves files unchanged
// ---------------------------------------------------------------------------

#[test]
fn init_without_force_exits_2_when_config_exists() {
    let dir = TempDir::new().unwrap();

    // First run: must succeed.
    stig_init(&dir, &[]).success();

    // Capture the content of stig.toml before the second run.
    let toml_before = std::fs::read_to_string(dir.path().join("stig.toml")).unwrap();

    // Second run without --force: must exit 2 with a useful message.
    stig_init(&dir, &[])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("already exists"));

    // stig.toml content must be unchanged.
    let toml_after = std::fs::read_to_string(dir.path().join("stig.toml")).unwrap();
    assert_eq!(toml_before, toml_after, "stig.toml should not be modified");
}

// ---------------------------------------------------------------------------
// 3. --force overwrites stig.toml
// ---------------------------------------------------------------------------

#[test]
fn init_force_overwrites_config() {
    let dir = TempDir::new().unwrap();

    // First run to produce the initial config.
    stig_init(&dir, &[]).success();

    // Mutate stig.toml to a non-default value.
    let toml_path = dir.path().join("stig.toml");
    let modified = std::fs::read_to_string(&toml_path)
        .unwrap()
        .replace("app.db", "mutated.db");
    std::fs::write(&toml_path, &modified).unwrap();

    // Confirm the mutation took effect.
    assert!(modified.contains("mutated.db"));

    // Run with --force: must succeed.
    stig_init(&dir, &["--force"]).success();

    // stig.toml should be back to defaults (no "mutated.db").
    let toml_after = std::fs::read_to_string(&toml_path).unwrap();
    assert!(
        !toml_after.contains("mutated.db"),
        "stig.toml should be reset to defaults after --force"
    );
    assert!(
        toml_after.contains("app.db"),
        "stig.toml should contain the default database_path after --force"
    );
}
