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

// ---------------------------------------------------------------------------
// 4. Env-var overrides: not persisted to stig.toml, but applied for artifacts
// ---------------------------------------------------------------------------

#[test]
fn init_does_not_persist_env_var_overrides_to_toml() {
    let dir = TempDir::new().unwrap();

    // Run init with STIG_DATABASE_PATH set in the environment.
    let mut cmd = Command::cargo_bin("stig").unwrap();
    cmd.current_dir(dir.path())
        .arg("init")
        .env("STIG_DATABASE_PATH", "from_env.db");
    cmd.assert().success();

    // The written stig.toml must contain the default database_path, not the
    // env-var value — env overrides are runtime-only and must not be baked
    // into the config file.
    let toml = std::fs::read_to_string(dir.path().join("stig.toml")).unwrap();
    assert!(
        !toml.contains("from_env.db"),
        "stig.toml must not contain env-var value STIG_DATABASE_PATH=from_env.db"
    );
    assert!(
        toml.contains("app.db"),
        "stig.toml should contain the default database_path"
    );

    // The env-var override IS applied for artifact creation: schema_migrations
    // is created in from_env.db (the runtime DB), not app.db.
    assert!(
        dir.path().join("from_env.db").is_file(),
        "from_env.db should be created (env override applied to artifact creation)"
    );
    let conn = Connection::open(dir.path().join("from_env.db")).unwrap();
    conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
        row.get::<_, i64>(0)
    })
    .expect("schema_migrations should exist in from_env.db");
}

// ---------------------------------------------------------------------------
// 5. --config writes to the explicit path, not project_root/stig.toml
// ---------------------------------------------------------------------------

#[test]
fn init_with_explicit_config_path_writes_to_that_path() {
    let dir = TempDir::new().unwrap();
    let custom_toml = dir.path().join("custom.toml");

    // Run init with an explicit --config path that does not exist yet.
    let mut cmd = Command::cargo_bin("stig").unwrap();
    cmd.current_dir(dir.path())
        .args(["--config", custom_toml.to_str().unwrap(), "init"]);
    cmd.assert().success();

    // The config must be written to the explicit path.
    assert!(custom_toml.is_file(), "custom.toml should be written");

    // The default stig.toml must NOT be created (we used an explicit path).
    assert!(
        !dir.path().join("stig.toml").exists(),
        "stig.toml should not be created when --config is an explicit path"
    );

    // Artifacts should be created relative to the config file's parent
    // (which is dir.path() here).
    assert!(dir.path().join("db/migrations").is_dir());
    assert!(dir.path().join(".local/db-backups/snapshots").is_dir());
    assert!(dir.path().join("app.db").is_file());
}

// ---------------------------------------------------------------------------
// 6. --force uses written config's paths for all artifacts
// ---------------------------------------------------------------------------

#[test]
fn init_force_creates_artifacts_matching_written_config() {
    let dir = TempDir::new().unwrap();

    // First run to produce the initial config.
    stig_init(&dir, &[]).success();

    // Mutate stig.toml so migrations_dir points somewhere non-default.
    let toml_path = dir.path().join("stig.toml");
    let original = std::fs::read_to_string(&toml_path).unwrap();
    let modified = original.replace("db/migrations", "custom/migrations");
    std::fs::write(&toml_path, &modified).unwrap();

    // Run --force: should succeed and write a default config.
    stig_init(&dir, &["--force"]).success();

    // The written config should reference the default migrations_dir.
    let toml_after = std::fs::read_to_string(&toml_path).unwrap();
    assert!(toml_after.contains("db/migrations"));

    // The default artifacts must exist (not the mutated paths).
    assert!(dir.path().join("db/migrations").is_dir());
    // schema_migrations must be in the default app.db, not a custom path.
    let conn = Connection::open(dir.path().join("app.db")).unwrap();
    conn.query_row("SELECT COUNT(*) FROM schema_migrations", [], |row| {
        row.get::<_, i64>(0)
    })
    .expect("schema_migrations should exist in app.db after --force");
}

// ---------------------------------------------------------------------------
// 7. STIG_CONFIG env var controls the write target
// ---------------------------------------------------------------------------

#[test]
fn init_stig_config_env_var_controls_write_target() {
    let dir = TempDir::new().unwrap();
    let custom_toml = dir.path().join("custom.toml");

    // Run init with STIG_CONFIG pointing to a non-existent file.
    let mut cmd = Command::cargo_bin("stig").unwrap();
    cmd.current_dir(dir.path())
        .arg("init")
        .env("STIG_CONFIG", custom_toml.to_str().unwrap());
    cmd.assert().success();

    // The config must be written to the STIG_CONFIG path.
    assert!(
        custom_toml.is_file(),
        "custom.toml should be written via STIG_CONFIG"
    );

    // The default stig.toml must NOT be created.
    assert!(
        !dir.path().join("stig.toml").exists(),
        "stig.toml should not be created when STIG_CONFIG is set"
    );
}
