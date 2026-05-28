//! Implementation of `stig init`.
//!
//! Bootstraps a new project:
//! - Writes `stig.toml` with default values.
//! - Creates the migrations directory.
//! - Creates the backups directory tree (`snapshots/`, `resets/`) with a
//!   `.gitignore` that excludes everything.
//! - Opens (or creates) the database and ensures `schema_migrations` exists.

use anyhow::Context as _;

use crate::config::Config;
use crate::db::Db;
use crate::errors::CliError;

/// Run `stig init`.
///
/// Exits with code 2 if a `stig.toml` already exists (found via upward search
/// from CWD). Otherwise writes a default `stig.toml` to CWD, creates
/// directory scaffolding, and bootstraps the database.
pub fn run() -> anyhow::Result<()> {
    // Check whether a stig.toml already exists anywhere in the upward search
    // path. If so, refuse to overwrite it.
    if let Some(existing) = Config::resolve_config_path(None, None, None) {
        return Err(CliError::Usage(format!("{} already exists", existing.display())).into());
    }

    // Determine the project root (CWD) and the write target.
    let project_root = std::env::current_dir()
        .map_err(|e| CliError::Usage(format!("cannot determine current directory: {e}")))?;
    let toml_path = project_root.join("stig.toml");

    // Build a default config rooted at the project root and write it.
    let config = Config {
        project_root: project_root.clone(),
        ..Config::default()
    };
    config
        .write(&toml_path)
        .with_context(|| format!("failed to write {}", toml_path.display()))?;
    println!("✓ wrote stig.toml");

    // Create migrations directory.
    let migrations_dir = project_root.join(&config.migrations_dir);
    std::fs::create_dir_all(&migrations_dir)
        .with_context(|| format!("failed to create {}", migrations_dir.display()))?;
    println!("✓ created {}/", config.migrations_dir);

    // Create backups directory tree + .gitignore.
    let backups_dir = project_root.join(&config.backups_dir);
    std::fs::create_dir_all(backups_dir.join("snapshots"))
        .with_context(|| format!("failed to create {}/snapshots", config.backups_dir))?;
    std::fs::create_dir_all(backups_dir.join("resets"))
        .with_context(|| format!("failed to create {}/resets", config.backups_dir))?;
    let gitignore = backups_dir.join(".gitignore");
    if !gitignore.exists() {
        std::fs::write(&gitignore, "*\n")
            .with_context(|| format!("failed to write {}", gitignore.display()))?;
    }
    println!(
        "✓ created {}/{{snapshots,resets}}/ (gitignored)",
        config.backups_dir
    );

    // Open (or create) the database and ensure schema_migrations exists.
    //
    // Per SPEC §5: checksum has no DEFAULT so every applied migration must
    // explicitly record its SHA-256.
    let db = Db::open(&config)
        .with_context(|| format!("failed to open database at {}", config.database_path))?;
    db.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    TEXT NOT NULL PRIMARY KEY,
            checksum   TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;
    println!("✓ created schema_migrations in {}", config.database_path);

    Ok(())
}
