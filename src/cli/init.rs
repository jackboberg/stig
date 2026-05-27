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
///   source if it already exists and `--force` is not passed). When `None`,
///   the `STIG_CONFIG` env var is checked (after loading `.env`), then an
///   upward search from CWD is tried; if neither yields a file the write
///   target defaults to `<cwd>/stig.toml`.
/// - `force`: when `true`, overwrite an existing config file without reading
///   it (so invalid TOML is also recoverable); when `false`, exit with code 2
///   if the target file already exists.
pub fn run(config_path: Option<PathBuf>, force: bool) -> anyhow::Result<()> {
    // Load .env first so that STIG_CONFIG defined there is visible to
    // resolve_path. Config::load_from also calls this; dotenvy is idempotent.
    dotenvy::dotenv().ok();

    // Determine the canonical config path: STIG_CONFIG > --config > upward
    // search. We need this before loading so we know the exact write target.
    let resolved_path = Config::resolve_path(config_path.as_deref(), None, None);

    // Derive the write target from the resolved path, falling back to
    // <cwd>/stig.toml when no existing file was found.
    let toml_path = resolved_path.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_default()
            .join("stig.toml")
    });

    // Guard: refuse to overwrite an existing config unless --force was passed.
    // This check happens before any file I/O so that --force can recover from
    // an invalid (unreadable/unparseable) existing config.
    if toml_path.exists() && !force {
        return Err(CliError::Usage(format!(
            "{} already exists; run with --force to overwrite",
            toml_path.display()
        ))
        .into());
    }

    // project_root is the directory that will contain stig.toml.
    let project_root = toml_path
        .parent()
        .map(|p| {
            // canonicalize so relative paths (e.g. bare "stig.toml") resolve
            // against CWD rather than producing an empty component.
            if p == std::path::Path::new("") {
                std::env::current_dir().unwrap_or_else(|_| p.to_path_buf())
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    // 1. Write stig.toml with defaults.
    // Always write Config::default() so neither env-var overrides nor values
    // from a previously existing (possibly mutated or invalid) file are
    // persisted to disk.
    let written_config = Config {
        project_root: project_root.clone(),
        ..Config::default()
    };
    written_config
        .write(&toml_path)
        .with_context(|| format!("failed to write {}", toml_path.display()))?;
    println!("✓ wrote stig.toml");

    // Build the effective config for artifact creation: start from the
    // freshly written defaults (correct paths) and layer real process env
    // overrides on top (e.g. STIG_DATABASE_PATH). This ensures artifacts are
    // consistent with the written config while still honouring runtime env
    // vars that are intentionally not persisted.
    let effective = {
        let mut c = written_config.clone();
        c.apply_env_overrides(None);
        c
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

    // Schema per SPEC §5: checksum has no DEFAULT so every applied migration
    // must explicitly supply its SHA-256.
    db.connection().execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    TEXT NOT NULL PRIMARY KEY,
            checksum   TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )?;

    println!("✓ created schema_migrations in {}", effective.database_path);

    Ok(())
}
