use anyhow::{Context, Result};
use tracing::info;

use crate::codegen;
use crate::config::Config;
use crate::db::Db;
use crate::errors::CliError;

/// Run `stig generate [target-name]`.
pub fn run(target_name: Option<String>, config: &Config) -> Result<()> {
    if config.generate.is_empty() {
        info!("no codegen targets configured");
        return Ok(());
    }

    let db = Db::open(config)
        .with_context(|| format!("failed to open database at {}", config.database_path))?;

    let filter = target_name.as_deref();
    let outputs = codegen::run_targets(
        db.connection(),
        &config.generate,
        &config.project_root,
        filter,
    )
    .map_err(|e| -> CliError { e.into() })?;

    for output in &outputs {
        let display_path = output
            .path
            .strip_prefix(&config.project_root)
            .unwrap_or(&output.path);
        println!("\u{2713} {}", display_path.display());
    }

    Ok(())
}
