//! Integration tests for `stig generate`.

mod common;

use common::{stig_cmd, write_migration};
use tempfile::TempDir;

#[test]
fn generate_creates_types_file() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_users",
        "CREATE TABLE users (
            id    INTEGER PRIMARY KEY,
            name  TEXT NOT NULL,
            email TEXT
        );",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    // Write a stig.toml with a generate target.
    let toml = r#"
migrations_dir = "db/migrations"
database_path  = "app.db"

[[generate]]
kind = "typescript"
path = "lib/database/types.ts"
"#
    .to_string();
    std::fs::write(dir.path().join("stig.toml"), toml).unwrap();

    let output = stig_cmd(&dir).arg("generate").assert().success();
    let stdout = std::str::from_utf8(&output.get_output().stdout).unwrap();
    assert!(stdout.contains("lib/database/types.ts"), "stdout: {stdout}");

    let ts_path = dir.path().join("lib/database/types.ts");
    assert!(ts_path.exists(), "types.ts should exist");

    let contents = std::fs::read_to_string(&ts_path).unwrap();
    assert!(contents.contains("export type Tables"));
    assert!(contents.contains("\"users\""));
    assert!(contents.contains("\"id\": number | null"));
    assert!(contents.contains("\"name\": string"));
}

#[test]
fn generate_with_target_name() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_items",
        "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT NOT NULL);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    let toml = r#"
migrations_dir = "db/migrations"
database_path  = "app.db"

[[generate]]
kind = "typescript"
path = "types.ts"
"#;
    std::fs::write(dir.path().join("stig.toml"), toml).unwrap();

    stig_cmd(&dir)
        .args(["generate", "typescript"])
        .assert()
        .success();

    let ts_path = dir.path().join("types.ts");
    assert!(ts_path.exists());
}

#[test]
fn generate_no_targets_configured() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    // No [[generate]] entries — should succeed silently.
    stig_cmd(&dir).arg("generate").assert().success();
}

#[test]
fn generate_unknown_target_exits_4() {
    let dir = TempDir::new().unwrap();
    stig_cmd(&dir).arg("init").assert().success();

    write_migration(
        &dir,
        "20240101000000",
        "create_items",
        "CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT NOT NULL);",
    );

    stig_cmd(&dir).arg("migrate").assert().success();

    let toml = r#"
migrations_dir = "db/migrations"
database_path  = "app.db"

[[generate]]
kind = "typescript"
path = "types.ts"
"#;
    std::fs::write(dir.path().join("stig.toml"), toml).unwrap();

    stig_cmd(&dir)
        .args(["generate", "nonexistent"])
        .assert()
        .failure()
        .code(4);
}
