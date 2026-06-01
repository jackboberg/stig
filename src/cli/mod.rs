pub mod backups;
pub mod generate;
pub mod init;
pub mod migrate;
pub mod new;
pub mod redo;
pub mod reset;
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
