use std::path::PathBuf;

use clap::{Parser, Subcommand};
use stig::cli::{BackupsCommand, SchemaCommand};
use stig::config::{ConfigOverrides, RunContext};
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

/// Build a [`ConfigOverrides`] value from the parsed global flags.
fn config_overrides(cli: &Cli) -> ConfigOverrides {
    ConfigOverrides {
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
    let ctx = RunContext {
        config_path: cli.config.clone(),
        overrides: config_overrides(&cli),
    };

    let result: Result<(), anyhow::Error> = (|| {
        if matches!(cli.command, Command::Init) {
            return stig::cli::init::run(&ctx);
        }

        // Load config only for non-init commands: `stig init --config <path>`
        // targets a file that does not exist yet, so Runtime::load would fail
        // before init has a chance to create it.
        let config = ctx.load_config()?;
        match cli.command {
            Command::Init => unreachable!("init is handled above"),
            Command::New {
                description,
                no_edit,
            } => stig::cli::new::run(description, no_edit, &config),
            Command::Migrate { dry_run } => stig::cli::migrate::run(dry_run, &config),
            Command::Status => stig::cli::status::run(&config),
            Command::Redo { version, yes } => stig::cli::redo::run(version, yes, &config),
            Command::Reset { yes } => stig::cli::reset::run(yes, &config),
            Command::Restore { timestamp, yes } => stig::cli::restore::run(timestamp, yes, &config),
            Command::Generate { target_name } => stig::cli::generate::run(target_name, &config),
            Command::Backups { command } => stig::cli::backups::run(command, &config),
            Command::Schema { command } => stig::cli::schema::run(command, &config),
        }
    })();

    if let Err(e) = result {
        let cli_err = e.downcast::<CliError>().unwrap_or_else(CliError::classify);
        eprintln!("{cli_err}");
        std::process::exit(cli_err.exit_code());
    }
}
