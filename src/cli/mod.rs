pub mod backups;
pub mod generate;
pub mod guards;
pub mod init;
pub mod migrate;
pub mod new;
pub mod prompt;
pub mod redo;
pub mod reset;
pub mod restore;
pub mod schema;
pub mod status;

use clap::Subcommand;

/// Subcommands for `stig backups`.
#[derive(Debug, Subcommand)]
pub enum BackupsCommand {
    /// List database backups and snapshots.
    List,
    /// Remove old backups according to keep policies.
    Prune {
        /// Skip confirmation prompt
        #[arg(long)]
        yes: bool,
    },
}

/// Subcommands for `stig schema`.
#[derive(Debug, Subcommand)]
pub enum SchemaCommand {
    /// Generate a migration from the difference between the current database and the migration baseline.
    Diff {
        /// Write the generated migration to a file instead of stdout
        #[arg(long)]
        output: Option<String>,
    },
}
