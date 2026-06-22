use anyhow::Context;

use crate::cli::SchemaCommand;
use crate::config::Runtime;
use crate::db::Db;
use crate::errors::CliError;
use crate::migrate::discover::discover;
use crate::schema::diff;

/// Run `stig schema diff`.
pub fn run(command: SchemaCommand, config: &Runtime) -> anyhow::Result<()> {
    let SchemaCommand::Diff { output } = command;

    let migrations_dir = config.migrations_path();
    if !migrations_dir.is_dir() {
        return Err(CliError::Prerequisite(format!(
            "migrations directory not found: {}",
            migrations_dir.display()
        ))
        .into());
    }

    let db = Db::open(config)
        .with_context(|| format!("failed to open database at {}", config.file.database_path))?;

    let files = discover(&migrations_dir).context("failed to discover migration files")?;

    let migration_sql = diff::generate_migration(db.connection(), &files, &config.file.pragmas)
        .context("failed to generate schema diff")?;

    match migration_sql {
        Some(sql) => {
            if let Some(output_path) = &output {
                let path = config.resolve_path(output_path);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).with_context(|| {
                        format!("failed to create directory: {}", parent.display())
                    })?;
                }
                std::fs::write(&path, &sql)
                    .with_context(|| format!("failed to write migration to {}", path.display()))?;
                let display_path = path.strip_prefix(&config.project_root).unwrap_or(&path);
                println!("\u{2713} {}", display_path.display());
            } else {
                print!("{sql}");
            }
        }
        None => {
            println!("no schema differences detected");
        }
    }

    Ok(())
}
