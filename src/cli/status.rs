use anyhow::{Context, Result};

use crate::config::Config;
use crate::db::{Db, ensure_schema_migrations};
use crate::errors::CliError;
use crate::migrate::discover::discover;
use crate::migrate::plan::{MigrationStatus, Plan};
use crate::snapshot;

/// Run `stig status`.
pub fn run() -> Result<()> {
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

    let snapshots_dir = project_root.join(&config.backups_dir).join("snapshots");

    // Header
    println!("database: {}", config.database_path);
    println!("migrations dir: {}", config.migrations_dir);
    println!(
        "checksum check: {}",
        if config.checksum_check { "on" } else { "off" }
    );
    println!();

    // Table header
    println!(
        "  {:<9} {:<9} {:<10} {:<34} file",
        "applied", "drifted", "snapshot", "version"
    );
    println!(
        "  {:<9} {:<9} {:<10} {:<34} -----------------------------------------",
        "-------", "-------", "--------", "--------------------------------"
    );

    // Rows
    let mut n_applied = 0u32;
    let mut n_pending = 0u32;
    let mut n_drifted = 0u32;

    for entry in &plan.entries {
        let (applied, drifted, snapshot_status) = match &entry.status {
            MigrationStatus::Pending => {
                n_pending += 1;
                ("no", "\u{2014}", "\u{2014}")
            }
            MigrationStatus::Applied { drifted, .. } => {
                n_applied += 1;
                let drift_display = if config.checksum_check {
                    if *drifted {
                        n_drifted += 1;
                        "yes"
                    } else {
                        "no"
                    }
                } else {
                    "\u{2014}"
                };
                let snap = if snapshot::snapshot_exists(&entry.version, &snapshots_dir) {
                    "yes"
                } else {
                    "pruned"
                };
                ("yes", drift_display, snap)
            }
            MigrationStatus::OrphanApplied { .. } => {
                n_applied += 1;
                ("yes", "\u{2014}", "\u{2014}")
            }
        };

        let file_name = match &entry.file {
            Some(f) => f.path.file_name().and_then(|n| n.to_str()).unwrap_or(""),
            None => "\u{2014}",
        };

        println!(
            "  {:<9} {:<9} {:<10} {:<34} {}",
            applied, drifted, snapshot_status, entry.version, file_name
        );
    }

    // Summary
    println!();
    if config.checksum_check {
        println!("summary: {n_applied} applied, {n_pending} pending, {n_drifted} drifted");
    } else {
        println!("summary: {n_applied} applied, {n_pending} pending");
    }

    // Exit 3 on drift
    if config.checksum_check && n_drifted > 0 {
        let mut msg = String::new();
        for entry in plan.drifted() {
            let version = &entry.version;
            let available = snapshot::snapshot_exists(version, &snapshots_dir);
            if available {
                msg.push_str(&format!(
                    "migration {version} has been edited since it was applied\n\
                     snapshot pre-{version}.db is available\n\
                     \u{2192} run: stig redo {version}\n"
                ));
            } else {
                msg.push_str(&format!(
                    "migration {version} has been edited since it was applied\n\
                     snapshot pre-{version}.db has been pruned\n\
                     \u{2192} revert the edit or run: stig reset\n"
                ));
            }
        }
        return Err(CliError::Drift(msg.trim().to_string()).into());
    }

    Ok(())
}
