use std::path::PathBuf;

use clap::{Parser, Subcommand};
use stig::context::RuntimeContext;
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

    // Build the fully-resolved runtime context from CLI args + process env.
    // This is the single point where dotenvy, env-var overrides, and config
    // file loading all happen — no command module touches std::env or Config::load*.
    let ctx = match RuntimeContext::build(cli.config) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(e.exit_code());
        }
    };

    let result: Result<(), anyhow::Error> = match cli.command {
        Command::Init { force } => stig::cli::init::run(&ctx, force),
        Command::New => stig::cli::new::run(&ctx),
        Command::Migrate => stig::cli::migrate::run(&ctx),
        Command::Status => stig::cli::status::run(&ctx),
        Command::Redo => stig::cli::redo::run(&ctx),
        Command::Reset => stig::cli::reset::run(&ctx),
        Command::Generate => stig::cli::generate::run(&ctx),
        Command::Backups => stig::cli::backups::run(&ctx),
    };

    if let Err(e) = result {
        let cli_err = e.downcast::<CliError>().unwrap_or_else(CliError::Generic);
        eprintln!("{cli_err}");
        std::process::exit(cli_err.exit_code());
    }
}
