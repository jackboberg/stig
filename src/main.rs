use std::path::PathBuf;

use clap::{Parser, Subcommand};
use stig::cli::{BackupsCommand, SchemaCommand};
use stig::config::{CliContext, CliOverrides};
use stig::errors::CliError;

#[derive(Debug, Parser)]
#[command(name = "stig", about = "A SQLite migration and schema CLI", version)]
struct Cli {
    /// Path to the configuration file
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Path to the SQLite database
    #[arg(long, global = true)]
    database_path: Option<String>,

    /// Directory containing migration files
    #[arg(long, global = true)]
    migrations_dir: Option<String>,

    /// Directory for snapshots and reset backups
    #[arg(long, global = true)]
    backups_dir: Option<String>,

    /// Path to the schema manifest file
    #[arg(long, global = true)]
    schema_path: Option<String>,

    /// Disable automatic pre-migration snapshots
    #[arg(long, global = true)]
    no_snapshot: bool,

    /// Skip checksum drift detection
    #[arg(long, global = true)]
    no_checksum: bool,

    #[command(subcommand)]
    command: Command,
}

/// Build a [`CliOverrides`] value from the parsed global flags.
fn cli_overrides(cli: &Cli) -> CliOverrides {
    CliOverrides {
        database_path: cli.database_path.clone(),
        migrations_dir: cli.migrations_dir.clone(),
        backups_dir: cli.backups_dir.clone(),
        auto_snapshot: cli.no_snapshot.then_some(false),
        checksum_check: cli.no_checksum.then_some(false),
        schema_path: cli.schema_path.clone(),
    }
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize a new stig project in the current directory.
    Init,
    /// Create a new migration file.
    New {
        /// Description for the new migration (will be slugified)
        description: String,
        /// Skip opening $EDITOR after creating the file
        #[arg(long)]
        no_edit: bool,
    },
    /// Apply pending migrations.
    Migrate {
        /// Preview what would be applied without mutating state.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show migration status.
    Status,
    /// Restore a snapshot and re-apply migrations from that version forward.
    Redo {
        /// Version to redo from (defaults to most recent applied migration)
        version: Option<String>,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Reset the database to a snapshot.
    Reset {
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Restore the database from a reset backup.
    Restore {
        /// Timestamp of the reset backup to restore (defaults to most recent)
        timestamp: Option<String>,
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
    /// Generate code from the current schema.
    Generate {
        /// Name or kind of target to generate (runs all if omitted)
        target_name: Option<String>,
    },
    /// Manage database backups/snapshots.
    Backups {
        #[command(subcommand)]
        command: BackupsCommand,
    },
    /// Compare database schema to migration baseline.
    Schema {
        #[command(subcommand)]
        command: SchemaCommand,
    },
}

fn main() {
    let cli = Cli::parse();
    let ctx = CliContext {
        config_path: cli.config.clone(),
        overrides: cli_overrides(&cli),
    };

    let result: Result<(), anyhow::Error> = (|| match cli.command {
        Command::Init => stig::cli::init::run(&ctx),
        Command::New {
            description,
            no_edit,
        } => {
            let config = ctx.load_config()?;
            stig::cli::new::run(description, no_edit, &config)
        }
        Command::Migrate { dry_run } => {
            let config = ctx.load_config()?;
            stig::cli::migrate::run(dry_run, &config)
        }
        Command::Status => {
            let config = ctx.load_config()?;
            stig::cli::status::run(&config)
        }
        Command::Redo { version, yes } => {
            let config = ctx.load_config()?;
            stig::cli::redo::run(version, yes, &config)
        }
        Command::Reset { yes } => {
            let config = ctx.load_config()?;
            stig::cli::reset::run(yes, &config)
        }
        Command::Restore { timestamp, yes } => {
            let config = ctx.load_config()?;
            stig::cli::restore::run(timestamp, yes, &config)
        }
        Command::Generate { target_name } => {
            let config = ctx.load_config()?;
            stig::cli::generate::run(target_name, &config)
        }
        Command::Backups { command } => {
            let config = ctx.load_config()?;
            stig::cli::backups::run(command, &config)
        }
        Command::Schema { command } => {
            let config = ctx.load_config()?;
            stig::cli::schema::run(command, &config)
        }
    })();

    if let Err(e) = result {
        let cli_err = e.downcast::<CliError>().unwrap_or_else(CliError::classify);
        eprintln!("{cli_err}");
        std::process::exit(cli_err.exit_code());
    }
}
