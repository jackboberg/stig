use clap::{Parser, Subcommand};
use stig::errors::CliError;

#[derive(Debug, Parser)]
#[command(name = "stig", about = "A SQLite migration and schema CLI", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
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
    /// Generate code from the current schema.
    Generate,
    /// Manage database backups/snapshots.
    Backups,
}

fn main() {
    let cli = Cli::parse();

    let result: Result<(), anyhow::Error> = match cli.command {
        Command::Init => stig::cli::init::run(),
        Command::New {
            description,
            no_edit,
        } => stig::cli::new::run(description, no_edit),
        Command::Migrate { dry_run } => stig::cli::migrate::run(dry_run),
        Command::Status => stig::cli::status::run(),
        Command::Redo { version, yes } => stig::cli::redo::run(version, yes),
        Command::Reset { yes } => stig::cli::reset::run(yes),
        Command::Generate => stig::cli::generate::run(),
        Command::Backups => stig::cli::backups::run(),
    };

    if let Err(e) = result {
        let cli_err = e.downcast::<CliError>().unwrap_or_else(CliError::Generic);
        eprintln!("{cli_err}");
        std::process::exit(cli_err.exit_code());
    }
}
