//! Implementation of `stig init`.
//!
//! Bootstraps a new project:
//! - Writes `stig.toml` with default values (or overwrites with `--force`).
//! - Creates the migrations directory.
//! - Creates the backups directory tree (`snapshots/`, `resets/`) with a
//!   `.gitignore` that excludes everything.
//! - Opens (or creates) the database and ensures `schema_migrations` exists.

use anyhow::Context as _;

use crate::config::Config;
use crate::context::{ConfigSource, RuntimeContext};
use crate::db::Db;
use crate::errors::CliError;

/// Run `stig init`.
///
/// - `ctx`: the fully-resolved runtime context built by `main`. `init` uses
///   `ctx.config_path` as the write target (falling back to `<cwd>/stig.toml`)
///   and `ctx.config_source` to determine whether an existing file is present.
/// - `force`: when `true`, overwrite an existing config file; when `false`,
///   exit with code 2 if the target file already exists.
pub fn run(ctx: &RuntimeContext, force: bool) -> anyhow::Result<()> {
    // Determine the write target.
    let toml_path = ctx.config_path.clone().unwrap_or_else(|| {
        std::env::current_dir()
            .unwrap_or_default()
            .join("stig.toml")
    });

    // Guard: refuse to overwrite an existing config unless --force was passed.
    if matches!(ctx.config_source, ConfigSource::File) && !force {
        return Err(CliError::Usage(format!(
            "{} already exists; run with --force to overwrite",
            toml_path.display()
        ))
        .into());
    }

    // Derive the project root from the write target path.
    let project_root = toml_path
        .parent()
        .map(|p| {
            if p == std::path::Path::new("") {
                std::env::current_dir().unwrap_or_else(|_| p.to_path_buf())
            } else {
                p.to_path_buf()
            }
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    // 1. Write stig.toml with defaults.
    // Always write Config::default() so neither env-var overrides nor values
    // from a previously existing file are persisted to disk.
    let written_config = Config {
        project_root: project_root.clone(),
        ..Config::default()
    };
    written_config
        .write(&toml_path)
        .with_context(|| format!("failed to write {}", toml_path.display()))?;
    println!("✓ wrote stig.toml");

    // Build the effective config for artifact creation: start from the
    // freshly written defaults (correct paths) and layer runtime env overrides
    // from ctx.config on top. ctx.config already has env overrides applied by
    // RuntimeContext::build, so we borrow its runtime-only fields.
    let effective = Config {
        project_root: project_root.clone(),
        database_path: ctx.config.database_path.clone(),
        migrations_dir: ctx.config.migrations_dir.clone(),
        backups_dir: ctx.config.backups_dir.clone(),
        auto_snapshot: ctx.config.auto_snapshot,
        checksum_check: ctx.config.checksum_check,
        ..written_config.clone()
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
