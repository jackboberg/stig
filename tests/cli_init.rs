//! Integration tests for `stig init`.

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

// ---------------------------------------------------------------------------
// 1. Happy path: empty directory produces all expected artifacts
// ---------------------------------------------------------------------------

#[test]
fn init_creates_all_expected_artifacts() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir)
        .arg("init")
        .assert()
        .success()
        .stdout(predicate::str::contains("✓ wrote stig.toml"))
        .stdout(predicate::str::contains("✓ created db/migrations/"))
        .stdout(predicate::str::contains(
            "✓ created .local/db-backups/{snapshots,resets}/ (gitignored)",
        ))
        .stdout(predicate::str::contains(
            "✓ created schema_migrations in app.db",
        ));

    // stig.toml written with default values.
    assert!(dir.path().join("stig.toml").is_file());

    // Migrations directory created.
    assert!(dir.path().join("db/migrations").is_dir());

    // Backups directory tree created.
    assert!(dir.path().join(".local/db-backups/snapshots").is_dir());
    assert!(dir.path().join(".local/db-backups/resets").is_dir());

    // .gitignore written inside backups dir.
    let gitignore = dir.path().join(".local/db-backups/.gitignore");
    assert!(gitignore.is_file());
    assert_eq!(std::fs::read_to_string(gitignore).unwrap(), "*\n");

    // Database created and schema_migrations table exists.
    assert!(dir.path().join("app.db").is_file());
    let conn = Connection::open(dir.path().join("app.db")).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .expect("schema_migrations table should exist");
    assert_eq!(count, 0);
}

// ---------------------------------------------------------------------------
// 2. Re-run without --force exits 2 and leaves files unchanged
// ---------------------------------------------------------------------------

#[test]
fn init_exits_2_when_config_exists() {
    let dir = TempDir::new().unwrap();

    // First run: must succeed.
    stig_cmd(&dir).arg("init").assert().success();

    let toml_before = std::fs::read_to_string(dir.path().join("stig.toml")).unwrap();

    // Second run: must exit 2 with a useful message.
    stig_cmd(&dir)
        .arg("init")
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("already exists"));

    // stig.toml must be unchanged.
    let toml_after = std::fs::read_to_string(dir.path().join("stig.toml")).unwrap();
    assert_eq!(toml_before, toml_after);
}

// ---------------------------------------------------------------------------
// 3. schema_migrations checksum column has no DEFAULT (SPEC §5)
// ---------------------------------------------------------------------------

#[test]
fn schema_migrations_checksum_has_no_default() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir).arg("init").assert().success();

    let conn = Connection::open(dir.path().join("app.db")).unwrap();

    // Inserting a row without supplying checksum must fail.
    let result = conn.execute(
        "INSERT INTO schema_migrations (version, applied_at) VALUES ('001', datetime('now'))",
        [],
    );
    assert!(
        result.is_err(),
        "inserting without checksum should fail (no DEFAULT)"
    );

    // Inserting with checksum must succeed.
    conn.execute(
        "INSERT INTO schema_migrations (version, checksum, applied_at) \
         VALUES ('001', 'abc123', datetime('now'))",
        [],
    )
    .expect("inserting with checksum should succeed");
}
