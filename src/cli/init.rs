//! Implementation of `stig init`.
//!
//! Bootstraps a new project:
//! - Writes `stig.toml` capturing the fully-resolved config (defaults merged
//!   with env-var and CLI overrides) so that the initialized file reflects the
//!   user's explicit intent.
//! - Creates the migrations directory.
//! - Creates the backups directory tree (`snapshots/`, `resets/`) with a
//!   `.gitignore` that excludes everything.
//! - Opens (or creates) the database and ensures `schema_migrations` exists.

use anyhow::Context as _;

use crate::context::{ConfigSource, RuntimeContext};
use crate::db::Db;
use crate::errors::CliError;

/// Run `stig init`.
///
/// - `ctx`: the fully-resolved runtime context built by `main`. `init` uses
///   `ctx.config_path` as the write target (falling back to `<cwd>/stig.toml`),
///   `ctx.config_source` to determine whether an existing file is present, and
///   `ctx.config` as the content to write — so env-var and CLI overrides are
///   persisted to `stig.toml` rather than being silently discarded.
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

    // 1. Write stig.toml with the fully-resolved config.
    //
    // ctx.config already has env-var overrides applied by RuntimeContext::build.
    // Writing it directly means the initialized file captures the user's explicit
    // intent (env vars, future CLI flags) rather than silently discarding them.
    // project_root is #[serde(skip)] so it is never written to the file.
    let config_to_write = crate::config::Config {
        project_root: project_root.clone(),
        ..ctx.config.clone()
    };
    config_to_write
        .write(&toml_path)
        .with_context(|| format!("failed to write {}", toml_path.display()))?;
    println!("✓ wrote stig.toml");

    // Use the same config for artifact creation — file and artifacts are
    // always consistent with each other.
    let effective = config_to_write;

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
