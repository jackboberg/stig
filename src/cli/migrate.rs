use anyhow::Context;

use crate::config::Config;
use crate::config::env_source::ProcessEnv;
use crate::db::{Db, ensure_schema_migrations, format_drift_messages};
use crate::errors::CliError;
use crate::migrate::apply;
use crate::migrate::discover::discover;
use crate::migrate::plan::Plan;
use crate::schema;

/// Run `stig migrate`.
pub fn run(dry_run: bool) -> anyhow::Result<()> {
    let config = Config::load(None, &ProcessEnv, None)?;
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
            let msg = format_drift_messages(&drifted, &snapshots_dir);
            return Err(CliError::Drift(msg).into());
        }
    }

    let n_current = plan.entries.len();
    let n_pending = plan.pending().len();
    let n_already = n_current - n_pending;

    if !dry_run && n_pending == n_current && schema::schema_has_content(&config) {
        let n_applied = schema::apply_schema_manifest(&db, &config, &files)
            .context("failed to apply schema manifest")?;
        println!(
            "✓ applied {} ({n_applied} migrations marked as applied)",
            config.schema_path
        );
    } else {
        apply::apply_pending(&db, &plan, &config, dry_run)?;

        if dry_run {
            println!("✓ {n_pending} would be applied, {n_already} already up to date");
        } else {
            println!("✓ {n_pending} applied, {n_already} already up to date");

            if n_pending > 0 {
                let plan_after = Plan::build(&files, db.connection())?;
                if plan_after.pending().is_empty() {
                    let sql = schema::generate_schema_sql(db.connection())
                        .context("failed to generate schema manifest")?;
                    schema::write_schema_sql(&config, &sql)
                        .context("failed to write schema manifest")?;
                }
            }
        }
    }

    Ok(())
}
