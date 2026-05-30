use anyhow::{Context, Result};

use crate::config::Config;
use crate::db::Db;
use crate::errors::CliError;
use crate::migrate::apply;
use crate::migrate::discover::discover;
use crate::migrate::plan::Plan;
use crate::snapshot;

/// Run `stig migrate`.
pub fn run(dry_run: bool) -> anyhow::Result<()> {
    let config = Config::load(None, None, None)?;
    let project_root = &config.project_root;

    let migrations_dir = project_root.join(&config.migrations_dir);
    if !migrations_dir.is_dir() {
        return Err(CliError::Prerequisite(format!(
            "migrations directory not found: {}",
            migrations_dir.display()
        ))
        .into());
    }

    let db = Db::open(&config)
        .with_context(|| format!("failed to open database at {}", config.database_path))?;

    ensure_schema_migrations(db.connection())?;

    let files = discover(&migrations_dir).context("failed to discover migration files")?;

    let plan = Plan::build(&files, db.connection())?;

    if config.checksum_check {
        let drifted = plan.drifted();
        if !drifted.is_empty() {
            let snapshots_dir = project_root.join(&config.backups_dir).join("snapshots");
            let mut msg = String::new();
            for entry in &drifted {
                let version = &entry.version;
                let snapshot_path = format!("pre-{version}.db");
                let available = snapshot::snapshot_exists(version, &snapshots_dir);
                if available {
                    msg.push_str(&format!(
                        "migration {version} has been edited since it was applied\n\
                         snapshot {snapshot_path} is available\n\
                         → run: stig redo {version}\n"
                    ));
                } else {
                    msg.push_str(&format!(
                        "migration {version} has been edited since it was applied\n\
                         snapshot {snapshot_path} has been pruned\n\
                         → revert the edit or run: stig reset\n"
                    ));
                }
            }
            return Err(CliError::Drift(msg.trim().to_string()).into());
        }
    }

    apply::apply_pending(&db, &plan, &config, dry_run)?;

    let n_pending = plan.pending().len();
    let n_current = plan.entries.len() - n_pending;
    if dry_run {
        println!("✓ {n_pending} would be applied, {n_current} already up to date");
    } else {
        println!("✓ {n_pending} applied, {n_current} already up to date");
    }

    Ok(())
}

/// Ensure the `schema_migrations` table exists (created by `init`, but
/// `migrate` must also handle a DB that was created externally).
fn ensure_schema_migrations(conn: &rusqlite::Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    TEXT NOT NULL PRIMARY KEY,
            checksum   TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )
    .context("failed to ensure schema_migrations table")?;
    Ok(())
}
