use std::path::Path;

use anyhow::Context;

use crate::config::Runtime;
use crate::db::{Db, delete_from_version, ensure_schema_migrations};
use crate::errors::CliError;
use crate::migrate;
use crate::migrate::discover::discover;
use crate::migrate::plan::{MigrationStatus, Plan};
use crate::snapshot;

/// Run `stig redo [<version>] [--yes]`.
pub fn run(version: Option<String>, yes: bool, config: &Runtime) -> anyhow::Result<()> {
    super::guards::require_persistent_db(config, "redo")?;
    let migrations_dir = super::guards::require_migrations_dir(config)?;

    let snapshots_dir = config.snapshots_path();

    let db = Db::open(config)
        .with_context(|| format!("failed to open database at {}", config.file.database_path))?;

    let files = discover(&migrations_dir).context("failed to discover migration files")?;
    let plan = Plan::build(&files, db.connection())?;

    let target = resolve_target(&plan, version)?;
    require_snapshot(&target, &plan, &snapshots_dir)?;
    let prompt = format!("this will discard local data added since version {target}. Continue?");
    super::prompt::confirm_or_abort(yes, &prompt)?;

    restore_and_clear(db, config, &target, &snapshots_dir)?;
    // Re-open the database and reapply pending migrations.
    // The schema-manifest fast path is intentionally NOT used here: after a
    // snapshot restore the database already contains schema from prior
    // migrations, so applying the full manifest would fail with "table already
    // exists". Only `reset` (which starts from a completely empty database)
    // can safely use the fast path.
    let db = Db::open(config)
        .with_context(|| format!("failed to open database at {}", config.file.database_path))?;
    migrate::reapply_pending(&db, config, &migrations_dir)?;

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

/// Checkpoint, close the connection, restore the snapshot, then delete stale
/// `schema_migrations` rows.
fn restore_and_clear(
    db: Db,
    config: &Runtime,
    target: &str,
    snapshots_dir: &Path,
) -> anyhow::Result<()> {
    db.checkpoint()?;
    db.close()?;

    let db_path = config.db_path();

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
