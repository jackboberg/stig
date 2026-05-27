use std::path::PathBuf;

use clap::{Parser, Subcommand};
use stig::errors::CliError;

#[derive(Debug, Parser)]
#[command(name = "stig", about = "A SQLite migration and schema CLI", version)]
struct Cli {
    /// Path to the stig.toml config file (default: walk upward from CWD).
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize a new stig project in the current directory.
    Init {
        /// Overwrite an existing stig.toml config file.
        #[arg(long)]
        force: bool,
    },
    /// Create a new migration file.
    New,
    /// Apply pending migrations.
    Migrate,
    /// Show migration status.
    Status,
    /// Roll back the last migration and re-apply it.
    Redo,
    /// Reset the database to a snapshot.
    Reset,
    /// Generate code from the current schema.
    Generate,
    /// Manage database backups/snapshots.
    Backups,
}

fn main() {
    let cli = Cli::parse();

    let result: Result<(), anyhow::Error> = match cli.command {
        Command::Init { force } => stig::cli::init::run(cli.config, force),
        Command::New => stig::cli::new::run(),
        Command::Migrate => stig::cli::migrate::run(),
        Command::Status => stig::cli::status::run(),
        Command::Redo => stig::cli::redo::run(),
        Command::Reset => stig::cli::reset::run(),
        Command::Generate => stig::cli::generate::run(),
        Command::Backups => stig::cli::backups::run(),
    };

    if let Err(e) = result {
        // Try to downcast to a typed CliError (which carries a specific exit
        // code) before falling back to the generic exit-1 wrapper.
        let cli_err = e.downcast::<CliError>().unwrap_or_else(CliError::Generic);
        eprintln!("{cli_err}");
        std::process::exit(cli_err.exit_code());
    }
}
