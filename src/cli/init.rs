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
///   `--config`.  When `Some`, this is honoured as the write target (and read
///   source if it already exists). When `None`, the `STIG_CONFIG` env var is
///   checked, then an upward search from CWD is tried; if neither yields a
///   file the write target defaults to `<project_root>/stig.toml`.
/// - `force`: when `true`, overwrite an existing config file; when `false`,
///   exit with code 2 if the target file already exists.
pub fn run(config_path: Option<PathBuf>, force: bool) -> anyhow::Result<()> {
    // Determine the canonical config path using the same precedence as
    // Config::load (STIG_CONFIG > --config > upward search).  We do this
    // before loading so we know the exact write target even for the
    // STIG_CONFIG case where config_path is None.
    //
    // For a genuinely new project none of these will find a file, so
    // resolved_path will be None and we fall back to writing stig.toml in the
    // project_root determined by Config::load below.
    let resolved_path = Config::resolve_path(config_path.as_deref(), None, None);

    // Load the effective config using the pre-resolved path, bypassing any
    // further STIG_CONFIG / upward-search resolution. This prevents
    // double-resolution where STIG_CONFIG points to a non-existent file and
    // Config::load would error trying to read it.
    let config = Config::load_from(resolved_path.as_deref(), None).map_err(anyhow::Error::from)?;

    // Determine the write target:
    //   1. Explicit --config path  (already in resolved_path via resolve_path)
    //   2. STIG_CONFIG-resolved path (also in resolved_path)
    //   3. <project_root>/stig.toml as a last resort
    let toml_path = resolved_path
        .clone()
        .unwrap_or_else(|| config.project_root.join("stig.toml"));

    // Guard: refuse to overwrite an existing config unless --force was passed.
    if toml_path.exists() && !force {
        return Err(CliError::Usage(format!(
            "{} already exists; run with --force to overwrite",
            toml_path.display()
        ))
        .into());
    }

    // 1. Write stig.toml.
    // Always write a fresh default config so env-var overrides and values from
    // any previously existing file are never persisted to disk.
    // project_root is derived from the write target's parent directory.
    let written_project_root = toml_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| config.project_root.clone());
    let written_config = Config {
        project_root: written_project_root,
        ..Config::default()
    };
    written_config
        .write(&toml_path)
        .with_context(|| format!("failed to write {}", toml_path.display()))?;
    println!("✓ wrote stig.toml");

    // For artifact creation (dirs + DB) use the *loaded* config so that
    // runtime overrides like STIG_DATABASE_PATH are respected: env vars are
    // not persisted to the config file, but they do control which paths are
    // initialized in this invocation.
    //
    // Use written_config's project_root to ensure relative paths are resolved
    // against the directory where the config file was written.
    let effective = Config {
        project_root: written_config.project_root.clone(),
        ..config
    };

    // 2. Create migrations directory.
    let migrations_dir = effective.project_root.join(&effective.migrations_dir);
    std::fs::create_dir_all(&migrations_dir)
        .with_context(|| format!("failed to create {}", migrations_dir.display()))?;
    println!("✓ created {}/", effective.migrations_dir);

    // 3. Create backups directory tree + .gitignore.
    let backups_dir = effective.project_root.join(&effective.backups_dir);
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
        effective.backups_dir
    );

    // 4. Open (or create) the database and ensure schema_migrations exists.
    let db = Db::open(&effective)
        .with_context(|| format!("failed to open database at {}", effective.database_path))?;

    db.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    TEXT NOT NULL PRIMARY KEY,
            checksum   TEXT NOT NULL DEFAULT '',
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    println!("✓ created schema_migrations in {}", effective.database_path);

    Ok(())
}
