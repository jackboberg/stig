//! Implementation of `stig init`.
//!
//! Bootstraps a new project:
//! - Writes `stig.toml` with default values.
//! - Creates the migrations directory.
//! - Creates the backups directory tree (`snapshots/`, `resets/`) with a
//!   `.gitignore` that excludes everything.
//! - Creates an initial empty `schema.sql` manifest.
//! - Opens (or creates) the database and ensures `schema_migrations` exists.

use std::path::{Path, PathBuf};

use anyhow::Context as _;

use crate::config::Config;
use crate::config::env_source::ProcessEnv;
use crate::db::Db;
use crate::errors::CliError;
use crate::schema;

/// Run `stig init`.
///
/// Exits with code 2 if a `stig.toml` already exists (found via upward search
/// from CWD). Otherwise writes a default `stig.toml` to CWD, creates
/// directory scaffolding, and bootstraps the database.
pub fn run() -> anyhow::Result<()> {
    guard_no_existing_config()?;

    let project_root = current_dir()?;
    let config = Config {
        project_root: project_root.clone(),
        ..Config::default()
    };

    write_config(&config, &project_root)?;
    create_migrations_dir(&config, &project_root)?;
    create_backups_dir(&config, &project_root)?;
    create_schema_manifest(&config)?;
    bootstrap_database(&config)?;

    Ok(())
}

/// Return an error (exit 2) if a `stig.toml` already exists anywhere in the
/// upward search path from CWD.
fn guard_no_existing_config() -> anyhow::Result<()> {
    if let Some(existing) = Config::resolve_config_path(None, &ProcessEnv, None) {
        return Err(CliError::Usage(format!("{} already exists", existing.display())).into());
    }
    Ok(())
}

/// Return the current working directory, or a usage error if it cannot be
/// determined.
fn current_dir() -> anyhow::Result<PathBuf> {
    std::env::current_dir()
        .map_err(|e| CliError::Usage(format!("cannot determine current directory: {e}")).into())
}

/// Serialise `config` to `<project_root>/stig.toml`.
fn write_config(config: &Config, project_root: &Path) -> anyhow::Result<()> {
    let toml_path = project_root.join("stig.toml");
    config.write(&toml_path)?;
    println!("✓ wrote stig.toml");
    Ok(())
}

/// Create the migrations directory.
fn create_migrations_dir(config: &Config, project_root: &Path) -> anyhow::Result<()> {
    let dir = project_root.join(&config.migrations_dir);
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    println!("✓ created {}/", config.migrations_dir);
    Ok(())
}

/// Create the backups directory tree (`snapshots/`, `resets/`) and write a
/// `.gitignore` inside each subdirectory to exclude its contents.
fn create_backups_dir(config: &Config, project_root: &Path) -> anyhow::Result<()> {
    let base = project_root.join(&config.backups_dir);
    for sub in ["snapshots", "resets"] {
        let dir = base.join(sub);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create {}", dir.display()))?;
        let gitignore = dir.join(".gitignore");
        if !gitignore.exists() {
            std::fs::write(&gitignore, "*\n")
                .with_context(|| format!("failed to write {}", gitignore.display()))?;
        }
    }
    println!(
        "✓ created {}/{{snapshots,resets}}/ (gitignored)",
        config.backups_dir
    );
    Ok(())
}

/// Create an initial empty schema manifest file if it does not already exist.
///
/// Returns a usage error if something other than a regular file exists at the
/// target path (e.g. a directory), since later operations would fail with a
/// less clear error.
fn create_schema_manifest(config: &Config) -> anyhow::Result<()> {
    let path = schema::schema_path(config);
    if path.is_file() {
        return Ok(());
    }
    if path.exists() {
        return Err(CliError::Usage(format!(
            "schema path exists but is not a regular file: {}",
            path.display()
        ))
        .into());
    }
    schema::write_schema_sql(config, "")
        .with_context(|| "failed to create schema manifest".to_string())?;
    println!("✓ created {}", config.schema_path);
    Ok(())
}

/// Open (or create) the database and ensure the `schema_migrations` table
/// exists.
///
/// Per SPEC §5: `checksum` has no DEFAULT so every applied migration must
/// explicitly record its SHA-256.
fn bootstrap_database(config: &Config) -> anyhow::Result<()> {
    let db = Db::open(config)
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
