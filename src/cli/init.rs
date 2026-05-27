//! Implementation of `stig init`.
//!
//! Bootstraps a new project:
//! - Writes `stig.toml` with default values (or overwrites with `--force`).
//! - Creates the migrations directory.
//! - Creates the backups directory tree (`snapshots/`, `resets/`) with a
//!   `.gitignore` that excludes everything.
//! - Opens (or creates) the database and ensures `schema_migrations` exists.

use std::path::PathBuf;

use anyhow::Context as _;

use crate::config::Config;
use crate::db::Db;
use crate::errors::CliError;

/// Run `stig init`.
///
/// - `config_path`: optional explicit path to `stig.toml` supplied via
///   `--config`.  When `None` the default upward-search logic in
///   [`Config::load`] applies.
/// - `force`: when `true`, overwrite an existing `stig.toml`; when `false`,
///   exit with code 2 if the file already exists.
pub fn run(config_path: Option<PathBuf>, force: bool) -> anyhow::Result<()> {
    // Load config (or defaults when no file exists yet). We pass start_dir =
    // None so Config::load uses the real CWD as the project root — which is
    // exactly where `init` should write its files.
    let config =
        Config::load(config_path.as_deref(), None, None).map_err(|e| anyhow::anyhow!("{e}"))?;

    let toml_path = config.project_root.join("stig.toml");

    // Guard: refuse to overwrite an existing config unless --force was passed.
    if toml_path.exists() && !force {
        return Err(CliError::Usage(format!(
            "{} already exists; run with --force to overwrite",
            toml_path.display()
        ))
        .into());
    }

    // 1. Write stig.toml.
    // When --force is active, always write a fresh default config (not the
    // loaded one, which may have been read from the existing mutated file).
    let config_to_write = if force && toml_path.exists() {
        Config {
            project_root: config.project_root.clone(),
            ..Config::default()
        }
    } else {
        config.clone()
    };
    config_to_write
        .write(&toml_path)
        .with_context(|| format!("failed to write {}", toml_path.display()))?;
    println!("✓ wrote stig.toml");

    // Use config_to_write for all subsequent path resolution so the paths
    // we create match the config we actually wrote to disk.
    let config = config_to_write;

    // 2. Create migrations directory.
    let migrations_dir = config.project_root.join(&config.migrations_dir);
    std::fs::create_dir_all(&migrations_dir)
        .with_context(|| format!("failed to create {}", migrations_dir.display()))?;
    println!("✓ created {}/", config.migrations_dir);

    // 3. Create backups directory tree + .gitignore.
    let backups_dir = config.project_root.join(&config.backups_dir);
    let snapshots_dir = backups_dir.join("snapshots");
    let resets_dir = backups_dir.join("resets");
    std::fs::create_dir_all(&snapshots_dir)
        .with_context(|| format!("failed to create {}", snapshots_dir.display()))?;
    std::fs::create_dir_all(&resets_dir)
        .with_context(|| format!("failed to create {}", resets_dir.display()))?;

    let gitignore_path = backups_dir.join(".gitignore");
    // Write .gitignore only if absent (idempotent; --force only affects toml).
    if !gitignore_path.exists() {
        std::fs::write(&gitignore_path, "*\n")
            .with_context(|| format!("failed to write {}", gitignore_path.display()))?;
    }
    println!(
        "✓ created {}/{{snapshots,resets}}/ (gitignored)",
        config.backups_dir
    );

    // 4. Open (or create) the database and ensure schema_migrations exists.
    let db = Db::open(&config)
        .with_context(|| format!("failed to open database at {}", config.database_path))?;

    db.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    TEXT NOT NULL PRIMARY KEY,
            checksum   TEXT NOT NULL DEFAULT '',
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    println!("✓ created schema_migrations in {}", config.database_path);

    Ok(())
}
