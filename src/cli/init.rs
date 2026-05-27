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
///   `--config`.  When `Some`, this is used as both the guard/read source and
///   the write target. When `None`, the default upward-search logic in
///   [`Config::load`] applies and the write target falls back to
///   `<project_root>/stig.toml`.
/// - `force`: when `true`, overwrite an existing `stig.toml`; when `false`,
///   exit with code 2 if the file already exists.
pub fn run(config_path: Option<PathBuf>, force: bool) -> anyhow::Result<()> {
    // Determine the write target for stig.toml up front.
    // When --config names an existing file, honour it as the read source and
    // write target. When it names a not-yet-existing file, treat that path as
    // the write target (don't try to load it). When --config is absent, use
    // the upward-search logic and fall back to <project_root>/stig.toml.
    let explicit_path_exists = config_path.as_deref().is_some_and(|p| p.exists());

    // Load config from the file only when it already exists.
    let load_path: Option<&std::path::Path> = if explicit_path_exists {
        config_path.as_deref()
    } else {
        None
    };

    // start_dir: when an explicit (but absent) config path is given, use its
    // parent so Config::load sets project_root correctly even without a file.
    let explicit_parent: Option<PathBuf> = if config_path.is_some() && !explicit_path_exists {
        config_path
            .as_deref()
            .and_then(|p| p.parent())
            .map(|p| p.to_path_buf())
    } else {
        None
    };

    let config =
        Config::load(load_path, None, explicit_parent.as_deref()).map_err(anyhow::Error::from)?;

    // Resolve the final write target.
    let toml_path = config_path
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
    // any previously existing file are never persisted. `project_root` is set
    // to the parent of the target path so relative paths within the written
    // config resolve correctly regardless of CWD.
    let project_root = toml_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| config.project_root.clone());
    let written_config = Config {
        project_root,
        ..Config::default()
    };
    written_config
        .write(&toml_path)
        .with_context(|| format!("failed to write {}", toml_path.display()))?;
    println!("✓ wrote stig.toml");

    // All subsequent artifact creation uses `written_config` so the paths on
    // disk are consistent with the config file that was just written.

    // 2. Create migrations directory.
    let migrations_dir = written_config
        .project_root
        .join(&written_config.migrations_dir);
    std::fs::create_dir_all(&migrations_dir)
        .with_context(|| format!("failed to create {}", migrations_dir.display()))?;
    println!("✓ created {}/", written_config.migrations_dir);

    // 3. Create backups directory tree + .gitignore.
    let backups_dir = written_config
        .project_root
        .join(&written_config.backups_dir);
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
        written_config.backups_dir
    );

    // 4. Open (or create) the database and ensure schema_migrations exists.
    let db = Db::open(&written_config).with_context(|| {
        format!(
            "failed to open database at {}",
            written_config.database_path
        )
    })?;

    db.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    TEXT NOT NULL PRIMARY KEY,
            checksum   TEXT NOT NULL DEFAULT '',
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    println!(
        "✓ created schema_migrations in {}",
        written_config.database_path
    );

    Ok(())
}
