use anyhow::{Context, Result};

use crate::codegen;
use crate::config::Runtime;
use crate::db::Db;
use crate::errors::CliError;

/// Run `stig generate [target-name]`.
pub fn run(target_name: Option<String>, config: &Runtime) -> Result<()> {
    if config.file.generate.is_empty() {
        println!("no codegen targets configured");
        return Ok(());
    }

    let db = Db::open(config)
        .with_context(|| format!("failed to open database at {}", config.file.database_path))?;

    let filter = target_name.as_deref();
    let outputs = codegen::run_targets(
        db.connection(),
        &config.file.generate,
        &config.project_root,
        filter,
    )
    .map_err(|e| -> CliError { e.into() })?;

    for output in &outputs {
        let display_path = output
            .path
            .strip_prefix(&config.project_root)
            .unwrap_or(&output.path);
        crate::success!("{}", display_path.display());
    }

    Ok(())
}
