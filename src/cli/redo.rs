use std::path::Path;

use anyhow::Context;

use crate::config::Config;
use crate::db::{Db, delete_from_version, ensure_schema_migrations};
use crate::errors::CliError;
use crate::migrate::apply;
use crate::migrate::discover::discover;
use crate::migrate::plan::{MigrationStatus, Plan};
use crate::snapshot;

/// Run `stig redo [<version>] [--yes]`.
pub fn run(version: Option<String>, yes: bool) -> anyhow::Result<()> {
    let config = Config::load(None, None, None)?;

    let migrations_dir = config.project_root.join(&config.migrations_dir);
    if !migrations_dir.is_dir() {
        return Err(CliError::Prerequisite(format!(
            "migrations directory not found: {}",
            migrations_dir.display()
        ))
        .into());
    }

    let snapshots_dir = config
        .project_root
        .join(&config.backups_dir)
        .join("snapshots");

    let db = Db::open(&config)
        .with_context(|| format!("failed to open database at {}", config.database_path))?;

    ensure_schema_migrations(db.connection())?;

    let files = discover(&migrations_dir).context("failed to discover migration files")?;
    let plan = Plan::build(&files, db.connection())?;

    let target = resolve_target(&plan, version)?;
    require_snapshot(&target, &plan, &snapshots_dir)?;
    confirm_or_abort(&target, yes)?;

    restore_and_clear(db, &config, &target, &snapshots_dir)?;
    reapply_pending(&config, &migrations_dir)?;

    println!("✓ redo complete");

    Ok(())
}

/// Find the target version: use the explicit argument, or the most recent
/// applied migration from the plan.
fn resolve_target(plan: &Plan, version: Option<String>) -> anyhow::Result<String> {
    match version {
        Some(v) => {
            let is_applied = plan
                .entries
                .iter()
                .any(|e| matches!(e.status, MigrationStatus::Applied { .. }) && e.version == v);
            if is_applied {
                Ok(v)
            } else {
                Err(
                    CliError::Prerequisite(format!("version not found in applied migrations: {v}"))
                        .into(),
                )
            }
        }
        None => plan
            .entries
            .iter()
            .rfind(|e| matches!(e.status, MigrationStatus::Applied { .. }))
            .map(|e| e.version.clone())
            .ok_or_else(|| {
                CliError::Prerequisite("no applied migrations to redo".to_string()).into()
            }),
    }
}

/// Ensure the snapshot for `target` exists. On failure, include the list of
/// redo-eligible versions (those with existing snapshots).
fn require_snapshot(target: &str, plan: &Plan, snapshots_dir: &Path) -> anyhow::Result<()> {
    if snapshot::snapshot_exists(target, snapshots_dir) {
        return Ok(());
    }

    let eligible: Vec<String> = plan
        .entries
        .iter()
        .filter(|e| matches!(e.status, MigrationStatus::Applied { .. }))
        .filter(|e| snapshot::snapshot_exists(&e.version, snapshots_dir))
        .map(|e| e.version.clone())
        .collect();

    let mut msg = format!("snapshot pre-{target}.db not found");
    if eligible.is_empty() {
        msg.push_str("\nno redo-eligible versions (all snapshots pruned)");
    } else {
        msg.push_str("\nredo-eligible versions:");
        for v in &eligible {
            msg.push_str(&format!("\n  {v}"));
        }
    }
    Err(CliError::Prerequisite(msg).into())
}

/// Prompt for confirmation unless `--yes` was passed. Returns `Ok(())` to
/// proceed, or `Err(CliError::Declined)` if the user declines.
fn confirm_or_abort(target: &str, yes: bool) -> anyhow::Result<()> {
    if yes {
        return Ok(());
    }
    let prompt = format!("this will discard local data added since version {target}. Continue?");
    if !dialoguer::Confirm::new()
        .with_prompt(&prompt)
        .default(false)
        .interact()
        .context("failed to read confirmation")?
    {
        // User declined — return a sentinel the caller can detect.
        return Err(CliError::Declined.into());
    }
    Ok(())
}

/// Checkpoint, close the connection, restore the snapshot, then delete stale
/// `schema_migrations` rows.
fn restore_and_clear(
    db: Db,
    config: &Config,
    target: &str,
    snapshots_dir: &Path,
) -> anyhow::Result<()> {
    db.checkpoint()?;
    db.close()?;

    let db_path = config.resolve_path(&config.database_path);

    println!("restoring pre-{target}.db");
    snapshot::restore_snapshot(target, &db_path, snapshots_dir)
        .context("failed to restore snapshot")?;

    // Re-open with a raw connection to delete rows, then close again.
    let conn = rusqlite::Connection::open(&db_path)
        .with_context(|| format!("failed to reopen database at {}", db_path.display()))?;
    ensure_schema_migrations(&conn)?;
    delete_from_version(&conn, target)?;
    conn.close()
        .map_err(|(_, e)| e)
        .context("failed to close database")?;

    Ok(())
}

/// Re-open the database, discover migrations, build a plan, and apply all
/// pending migrations.
fn reapply_pending(config: &Config, migrations_dir: &Path) -> anyhow::Result<()> {
    let db = Db::open(config)
        .with_context(|| format!("failed to open database at {}", config.database_path))?;

    ensure_schema_migrations(db.connection())?;

    let files = discover(migrations_dir).context("failed to discover migration files")?;
    let plan = Plan::build(&files, db.connection())?;

    apply::apply_pending(&db, &plan, config, false)?;

    Ok(())
}
