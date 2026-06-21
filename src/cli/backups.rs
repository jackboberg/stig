use std::time::Duration;

use anyhow::Context;

use crate::cli::BackupsCommand;
use crate::config::Config;
use crate::errors::CliError;
use crate::snapshot;

/// Run `stig backups <subcommand>`.
pub fn run(command: BackupsCommand, config: &Config) -> anyhow::Result<()> {
    match command {
        BackupsCommand::List => list(config),
        BackupsCommand::Prune { yes } => prune(yes, config),
    }
}

fn list(config: &Config) -> anyhow::Result<()> {
    let backups_dir = config.project_root.join(&config.backups_dir);
    let snapshots_dir = backups_dir.join("snapshots");
    let resets_dir = backups_dir.join("resets");

    let snapshots = snapshot::list_backups(&snapshots_dir, "pre-")?;
    let resets = snapshot::list_backups(&resets_dir, "reset-")?;

    println!(
        "snapshots ({} of max {}):",
        snapshots.len(),
        config.snapshot_keep
    );
    for entry in &snapshots {
        println!(
            "  {:<36} {:>8}   {} ago",
            entry.filename,
            format_size(entry.size_bytes),
            format_duration(entry.age),
        );
    }

    println!();
    println!("resets ({} of max {}):", resets.len(), config.reset_keep);
    for entry in &resets {
        println!(
            "  {:<36} {:>8}   {} ago",
            entry.filename,
            format_size(entry.size_bytes),
            format_duration(entry.age),
        );
    }

    Ok(())
}

fn prune(yes: bool, config: &Config) -> anyhow::Result<()> {
    confirm_or_abort(yes)?;

    let backups_dir = config.project_root.join(&config.backups_dir);
    let snapshots_dir = backups_dir.join("snapshots");
    let resets_dir = backups_dir.join("resets");

    snapshot::prune_snapshots(&snapshots_dir, config.snapshot_keep)
        .context("failed to prune snapshots")?;
    snapshot::prune_resets(&resets_dir, config.reset_keep)
        .context("failed to prune reset backups")?;

    println!("✓ backups pruned");

    Ok(())
}

fn confirm_or_abort(yes: bool) -> anyhow::Result<()> {
    if yes {
        return Ok(());
    }
    match dialoguer::Confirm::new()
        .with_prompt("this will delete old backups beyond keep limits. Continue?")
        .default(false)
        .interact()
    {
        Ok(true) => Ok(()),
        Ok(false) => Err(CliError::Declined.into()),
        Err(_) => Err(CliError::Declined.into()),
    }
}

fn format_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;

    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{} KiB", bytes / KIB)
    } else {
        format!("{} B", bytes)
    }
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format_duration_value(secs, "second")
    } else if secs < 3600 {
        format_duration_value(secs / 60, "minute")
    } else if secs < 86400 {
        format_duration_value(secs / 3600, "hour")
    } else {
        format_duration_value(secs / 86400, "day")
    }
}

fn format_duration_value(value: u64, unit: &str) -> String {
    if value == 1 {
        format!("1 {unit}")
    } else {
        format!("{value} {unit}s")
    }
}
