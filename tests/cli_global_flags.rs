//! Integration tests for global CLI flags.

mod common;

use predicates::prelude::*;
use rusqlite::Connection;
use tempfile::TempDir;

use common::{stig_cmd, write_migration};

// --database-path overrides the default database location.
#[test]
fn global_database_path_overrides_default() {
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
        .arg("--database-path")
        .arg("other.db")
        .assert()
        .success();

    let other_db = dir.path().join("other.db");
    assert!(
        other_db.exists(),
        "expected --database-path target to be created"
    );

    let conn = Connection::open(&other_db).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

// A CLI --database-path flag beats the STIG_DATABASE_PATH env var.
#[test]
fn global_database_path_beats_env_var() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    stig_cmd(&dir)
        .env("STIG_DATABASE_PATH", "env.db")
        .arg("migrate")
        .arg("--database-path")
        .arg("cli.db")
        .assert()
        .success();

    let cli_db = dir.path().join("cli.db");
    let env_db = dir.path().join("env.db");
    assert!(cli_db.exists(), "expected CLI --database-path to win");
    assert!(
        !env_db.exists(),
        "did not expect env var database to be used"
    );
}

// --migrations-dir overrides the configured migrations directory.
#[test]
fn global_migrations_dir_overrides_default() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    let custom_migrations = dir.path().join("custom/migrations");
    std::fs::create_dir_all(&custom_migrations).unwrap();
    std::fs::write(
        custom_migrations.join("20240101000000_create_users.sql"),
        "CREATE TABLE users (id INTEGER);",
    )
    .unwrap();

    stig_cmd(&dir)
        .arg("migrate")
        .arg("--migrations-dir")
        .arg("custom/migrations")
        .assert()
        .success();

    let db_path = dir.path().join("app.db");
    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}

// --config loads an alternate configuration file.
#[test]
fn global_config_path_loads_alternate_config() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    let custom_dir = dir.path().join("custom");
    std::fs::create_dir_all(custom_dir.join("migrations")).unwrap();
    std::fs::create_dir_all(custom_dir.join("db/snapshots")).unwrap();
    std::fs::write(
        custom_dir.join("stig.toml"),
        indoc::indoc! {r#"
            database_path = "custom.db"
            migrations_dir = "migrations"
        "#},
    )
    .unwrap();
    std::fs::write(
        custom_dir.join("migrations/20240101000000_create_users.sql"),
        "CREATE TABLE users (id INTEGER);",
    )
    .unwrap();

    stig_cmd(&dir)
        .arg("migrate")
        .arg("--config")
        .arg("custom/stig.toml")
        .assert()
        .success();

    assert!(
        custom_dir.join("custom.db").exists(),
        "expected database relative to alternate config"
    );
}

// --config on the CLI beats the STIG_CONFIG env var.
#[test]
fn global_config_path_beats_env_var() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );

    let a_config = dir.path().join("a.toml");
    let b_config = dir.path().join("b.toml");

    std::fs::write(
        &a_config,
        indoc::indoc! {r#"
            database_path = "a.db"
            migrations_dir = "db/migrations"
        "#},
    )
    .unwrap();
    std::fs::write(
        &b_config,
        indoc::indoc! {r#"
            database_path = "b.db"
            migrations_dir = "db/migrations"
        "#},
    )
    .unwrap();

    stig_cmd(&dir)
        .env("STIG_CONFIG", a_config.to_str().unwrap())
        .arg("migrate")
        .arg("--config")
        .arg(b_config.to_str().unwrap())
        .assert()
        .success();

    assert!(dir.path().join("b.db").exists(), "expected --config to win");
    assert!(
        !dir.path().join("a.db").exists(),
        "did not expect STIG_CONFIG db"
    );
}

// --no-checksum disables checksum drift detection.
#[test]
fn global_no_checksum_disables_drift_detection() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER);",
    );
    stig_cmd(&dir).arg("migrate").assert().success();

    // Edit the migration so the checksum drifts.
    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (id INTEGER, name TEXT);",
    );

    stig_cmd(&dir)
        .arg("status")
        .arg("--no-checksum")
        .assert()
        .success()
        .stdout(predicate::str::contains("checksum check: off"))
        .stdout(predicate::str::contains("1 applied, 0 pending"));
}

// `stig init --database-path` writes the overridden value to stig.toml.
#[test]
fn init_database_path_global_flag_writes_config() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir)
        .arg("init")
        .arg("--database-path")
        .arg("other.db")
        .assert()
        .success();

    let toml = std::fs::read_to_string(dir.path().join("stig.toml")).unwrap();
    assert!(toml.contains("database_path = \"other.db\""));

    let db_path = dir.path().join("other.db");
    assert!(db_path.exists(), "expected init to bootstrap overridden DB");
}

// `stig init --config <path>` creates the config file at the given path.
#[test]
fn init_config_global_flag_creates_file_at_path() {
    let dir = TempDir::new().unwrap();

    stig_cmd(&dir)
        .arg("init")
        .arg("--config")
        .arg("custom/stig.toml")
        .assert()
        .success();

    let config_path = dir.path().join("custom/stig.toml");
    assert!(config_path.is_file());

    // Scaffolding should be relative to the config file's directory.
    assert!(dir.path().join("custom/db/migrations").is_dir());
    assert!(dir.path().join("custom/db/snapshots").is_dir());
    assert!(dir.path().join("custom/db/resets").is_dir());

    // Database should be at the default path relative to the project root.
    assert!(dir.path().join("custom/app.db").exists());
}
