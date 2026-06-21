use std::path::Path;

use anyhow::Context;

use crate::config::Runtime;
use crate::db::{Db, ensure_schema_migrations};
use crate::errors::CliError;
use crate::migrate::apply;
use crate::migrate::discover::discover;
use crate::migrate::plan::Plan;
use crate::schema;
use crate::snapshot;

/// Run `stig reset [--yes]`.
pub fn run(yes: bool, config: &Runtime) -> anyhow::Result<()> {
    let migrations_dir = config.migrations_path();
    if !migrations_dir.is_dir() {
        return Err(CliError::Prerequisite(format!(
            "migrations directory not found: {}",
            migrations_dir.display()
        ))
        .into());
    }

    confirm_or_abort(yes)?;

    let resets_dir = config.resets_path();
    std::fs::create_dir_all(&resets_dir).with_context(|| {
        format!(
            "failed to create resets directory: {}",
            resets_dir.display()
        )
    })?;

    let db = Db::open(config)
        .with_context(|| format!("failed to open database at {}", config.file.database_path))?;

    ensure_schema_migrations(db.connection())?;

    db.checkpoint()?;
    db.close()?;

    let db_path = config.db_path();

    println!("moving database to resets/");
    let backup_path = snapshot::take_reset_backup(&db_path, &resets_dir)
        .context("failed to create reset backup")?;

    if let Err(e) = reapply_pending(config, &migrations_dir) {
        eprintln!("reset failed; restoring database from resets/");
        if let Err(restore_err) = snapshot::restore_reset_backup_from_path(&backup_path, &db_path) {
            return Err(anyhow::anyhow!(
                "reapply failed: {e}\n\
                 also failed to restore reset backup from {}: {restore_err}\n\
                 the reset backup remains in resets/ for manual recovery",
                backup_path.display()
            ));
        }
        return Err(e);
    }

    println!("✓ reset complete");

    snapshot::prune_resets(&resets_dir, config.file.reset_keep)
        .context("failed to prune reset backups")?;

    Ok(())
}

/// Prompt for confirmation unless `--yes` was passed. Returns `Ok(())` to
/// proceed, or `Err(CliError::Declined)` if the user declines or stdin is
/// not interactive (e.g. piped).
fn confirm_or_abort(yes: bool) -> anyhow::Result<()> {
    if yes {
        return Ok(());
    }
    match dialoguer::Confirm::new()
        .with_prompt("this will destroy the current database and re-migrate from empty. Continue?")
        .default(false)
        .interact()
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(CliError::Declined.into()),
        // stdin is not a TTY (piped) — treat as decline.
        Err(_) => Err(CliError::Declined.into()),
    }
}

/// Open a fresh database and reapply all migrations. Uses the schema manifest
/// if available and up to date for a fast reset; otherwise replays all
/// migrations individually.
fn reapply_pending(config: &Runtime, migrations_dir: &Path) -> anyhow::Result<()> {
    let db = Db::open(config)
        .with_context(|| format!("failed to open database at {}", config.file.database_path))?;

    ensure_schema_migrations(db.connection())?;

    let files = discover(migrations_dir).context("failed to discover migration files")?;

    if schema::schema_has_content(config) && schema::schema_is_fresh(config, &files) {
        let n = schema::apply_schema_manifest(&db, config)
            .context("failed to apply schema manifest")?;
        println!(
            "✓ applied {} ({n} migrations marked as applied)",
            config.file.schema_path
        );
    } else {
        let plan = Plan::build(&files, db.connection())?;
        apply::apply_pending(&db, &plan, config, false)?;
    }

    Ok(())
}
