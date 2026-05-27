//! Integration tests for `stig init`.

use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use tempfile::TempDir;

/// All environment variables that `stig` reads. Removing these from every
/// subprocess prevents ambient shell variables set by the developer (or a
/// previous test run) from corrupting assertions.
const STIG_ENV_KEYS: &[&str] = &[
    "STIG_CONFIG",
    "STIG_DATABASE_PATH",
    "DATABASE_PATH",
    "STIG_MIGRATIONS_DIR",
    "STIG_BACKUPS_DIR",
    "STIG_NO_SNAPSHOT",
    "STIG_NO_CHECKSUM",
];

/// Return a `stig` [`Command`] with all known `STIG_*` env vars removed and
/// `current_dir` set to `dir`. Callers can add `.arg()` / `.env()` on top.
fn stig_cmd(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("stig").unwrap();
    cmd.current_dir(dir.path());
    for key in STIG_ENV_KEYS {
        cmd.env_remove(key);
    }
    cmd
}

/// Helper: run `stig init [args]` with a clean environment.
fn stig_init(dir: &TempDir, extra_args: &[&str]) -> assert_cmd::assert::Assert {
    let mut cmd = stig_cmd(dir);
    cmd.arg("init");
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
// 3. --force succeeds even when stig.toml exists; preserves resolved values
// ---------------------------------------------------------------------------

#[test]
fn init_force_succeeds_when_config_exists() {
    let dir = TempDir::new().unwrap();

    // First run to produce the initial config.
    stig_init(&dir, &[]).success();

    // Mutate stig.toml to a non-default value.
    let toml_path = dir.path().join("stig.toml");
    let modified = std::fs::read_to_string(&toml_path)
        .unwrap()
        .replace("app.db", "mutated.db");
    std::fs::write(&toml_path, &modified).unwrap();

    // Run with --force: must succeed (file is loaded, mutated value is resolved).
    stig_init(&dir, &["--force"]).success();

    // The file is rewritten with the resolved config, which was loaded from
    // the mutated stig.toml — so the mutated value is preserved.
    let toml_after = std::fs::read_to_string(&toml_path).unwrap();
    assert!(
        toml_after.contains("mutated.db"),
        "--force rewrites the file with the resolved config (mutated value preserved)"
    );
}

// ---------------------------------------------------------------------------
// 4. Env-var overrides ARE persisted to stig.toml and applied to artifacts
// ---------------------------------------------------------------------------

#[test]
fn init_persists_env_var_overrides_to_toml() {
    let dir = TempDir::new().unwrap();

    // Run init with STIG_DATABASE_PATH set in the environment.
    let mut cmd = stig_cmd(&dir);
    cmd.arg("init").env("STIG_DATABASE_PATH", "from_env.db");
    cmd.assert().success();

    // The written stig.toml must capture the env-var value — init expresses
    // intent, so env overrides are persisted rather than discarded.
    let toml = std::fs::read_to_string(dir.path().join("stig.toml")).unwrap();
    assert!(
        toml.contains("from_env.db"),
        "stig.toml should contain env-var value STIG_DATABASE_PATH=from_env.db"
    );

    // Artifacts use the same value — file and artifacts are always consistent.
    assert!(
        dir.path().join("from_env.db").is_file(),
        "from_env.db should be created (consistent with written config)"
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
    let mut cmd = stig_cmd(&dir);
    cmd.args(["--config", custom_toml.to_str().unwrap(), "init"]);
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
// 6. --force with env override: env value is written to file and used for artifacts
// ---------------------------------------------------------------------------

#[test]
fn init_force_with_env_override_writes_env_value() {
    let dir = TempDir::new().unwrap();

    // First run to produce the initial config.
    stig_init(&dir, &[]).success();

    // Run --force with an env override: the env value should be written.
    let mut cmd = stig_cmd(&dir);
    cmd.args(["init", "--force"])
        .env("STIG_MIGRATIONS_DIR", "custom/migrations");
    cmd.assert().success();

    // The rewritten config captures the env override.
    let toml_after = std::fs::read_to_string(dir.path().join("stig.toml")).unwrap();
    assert!(
        toml_after.contains("custom/migrations"),
        "--force with env override should write the env value to stig.toml"
    );

    // The artifact dir matches the written config.
    assert!(dir.path().join("custom/migrations").is_dir());
}

// ---------------------------------------------------------------------------
// 7. Invalid stig.toml always errors before any command runs
// ---------------------------------------------------------------------------

#[test]
fn init_invalid_toml_always_errors() {
    let dir = TempDir::new().unwrap();

    // Write a deliberately broken stig.toml.
    let toml_path = dir.path().join("stig.toml");
    std::fs::write(&toml_path, "this is not valid toml ][[[").unwrap();

    // Without --force: exits 2 with a TOML parse error message.
    stig_init(&dir, &[])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("invalid config"));

    // With --force: still exits 2 — invalid TOML is always an error.
    // Users must fix or delete stig.toml manually before init --force can run.
    stig_init(&dir, &["--force"])
        .failure()
        .code(2)
        .stderr(predicate::str::contains("invalid config"));
}

// ---------------------------------------------------------------------------
// 8. schema_migrations checksum column has no default (SPEC §5)
// ---------------------------------------------------------------------------

#[test]
fn schema_migrations_checksum_has_no_default() {
    let dir = TempDir::new().unwrap();
    stig_init(&dir, &[]).success();

    let conn = Connection::open(dir.path().join("app.db")).unwrap();

    // Inserting a row without supplying checksum must fail (NOT NULL, no default).
    let result = conn.execute(
        "INSERT INTO schema_migrations (version, applied_at) VALUES ('20260101000000_test', datetime('now'))",
        [],
    );
    assert!(
        result.is_err(),
        "INSERT without checksum should fail due to NOT NULL constraint"
    );

    // Supplying checksum explicitly must succeed.
    conn.execute(
        "INSERT INTO schema_migrations (version, checksum) VALUES ('20260101000000_test', 'abc123')",
        [],
    )
    .expect("INSERT with explicit checksum should succeed");
}
