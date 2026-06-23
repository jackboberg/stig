use std::path::{Path, PathBuf};

use anyhow::Context;
use chrono::NaiveDateTime;

use crate::config::Runtime;
use crate::errors::CliError;
use crate::snapshot;

/// Run `stig restore [timestamp] [--yes]`.
///
/// Restores the database from a reset backup. With no timestamp, the most
/// recent reset backup is used. With a timestamp, the matching
/// `reset-<timestamp>.db` file is used.
pub fn run(timestamp: Option<String>, yes: bool, config: &Runtime) -> anyhow::Result<()> {
    super::guards::require_persistent_db(config, super::guards::PersistentDbCommand::Restore)?;

    let db_path = config.db_path();
    let resets_dir = config.resets_path();

    let backup_path = resolve_backup(&resets_dir, timestamp.as_deref())?;

    let name = backup_path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_else(|| backup_path.display().to_string().into());
    let prompt = format!(
        "this will replace the current database with {}. Continue?",
        name
    );
    super::prompt::confirm_or_abort(yes, &prompt)?;

    snapshot::restore_reset_backup_from_path(&backup_path, &db_path)
        .with_context(|| format!("failed to restore {}", backup_path.display()))?;

    println!(
        "✓ restored database from {}",
        backup_path.file_name().unwrap().to_string_lossy()
    );

    Ok(())
}

/// Resolve the reset backup path from an optional timestamp.
///
/// - `Some(ts)` -> `resets_dir/reset-<ts>.db`
/// - `None` -> most recent `reset-*.db` in `resets_dir`
fn resolve_backup(resets_dir: &Path, timestamp: Option<&str>) -> anyhow::Result<PathBuf> {
    match timestamp {
        Some(ts) => {
            validate_timestamp(ts)?;
            let path = resets_dir.join(format!("reset-{ts}.db"));
            if !path.is_file() {
                return Err(CliError::Prerequisite(format!(
                    "reset backup not found: {}",
                    path.display()
                ))
                .into());
            }
            Ok(path)
        }
        None => Ok(snapshot::most_recent_reset(resets_dir).map_err(|e| {
            CliError::Prerequisite(format!("failed to locate a reset backup to restore: {e}"))
        })?),
    }
}

/// Validate that `ts` matches the reset backup timestamp format
/// `%Y%m%dT%H%M%SZ` and represents a real datetime.
fn validate_timestamp(ts: &str) -> anyhow::Result<()> {
    if NaiveDateTime::parse_from_str(ts, "%Y%m%dT%H%M%SZ").is_err() {
        return Err(CliError::Usage(format!("invalid timestamp format: {ts}")).into());
    }
    Ok(())
}
