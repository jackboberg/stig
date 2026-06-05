#![allow(dead_code)]

use assert_cmd::Command;
use tempfile::TempDir;

/// Known `STIG_*` env vars that could leak from the developer's shell and
/// corrupt test assertions. Cleared on every subprocess.
pub const STIG_ENV_KEYS: &[&str] = &[
    "STIG_CONFIG",
    "STIG_DATABASE_PATH",
    "DATABASE_PATH",
    "STIG_MIGRATIONS_DIR",
    "STIG_BACKUPS_DIR",
    "STIG_NO_SNAPSHOT",
    "STIG_NO_CHECKSUM",
    "STIG_SCHEMA_PATH",
];

/// Return a `stig` [`Command`] with CWD set to `dir` and all known `STIG_*`
/// env vars removed so ambient shell variables cannot affect assertions.
pub fn stig_cmd(dir: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("stig").unwrap();
    cmd.current_dir(dir.path());
    for key in STIG_ENV_KEYS {
        cmd.env_remove(key);
    }
    cmd
}

/// Write a migration `.sql` file into `dir/db/migrations/`.
pub fn write_migration(dir: &TempDir, timestamp: &str, slug: &str, content: &str) {
    let migrations_dir = dir.path().join("db/migrations");
    std::fs::create_dir_all(&migrations_dir).unwrap();
    let filename = format!("{timestamp}_{slug}.sql");
    let path = migrations_dir.join(filename);
    std::fs::write(&path, content).unwrap();
}
