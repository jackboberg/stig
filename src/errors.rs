use thiserror::Error;

/// Exit-code-bearing errors for the `stig` CLI.
///
/// Each variant maps to a specific exit code per SPEC §8.3.
#[derive(Debug, Error)]
pub enum CliError {
    /// Exit code 1 — generic failure (SQL error, IO error, unexpected state).
    #[error("error: {0}")]
    Generic(#[from] anyhow::Error),

    /// Exit code 2 — usage / config error.
    #[error("config error: {0}")]
    Usage(String),

    /// Exit code 2 — user declined a confirmation prompt.
    #[error("operation cancelled")]
    Declined,

    /// Exit code 3 — drift detected between applied migrations and on-disk files.
    #[error("drift detected: {0}")]
    Drift(String),

    /// Exit code 4 — prerequisite missing (snapshot gone, target unknown, etc.).
    #[error("prerequisite missing: {0}")]
    Prerequisite(String),

    /// Exit code 5 — database locked or otherwise unavailable.
    #[error("database unavailable: {0}")]
    Locked(String),
}

impl CliError {
    /// Returns the process exit code for this error variant.
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::Generic(_) => 1,
            CliError::Usage(_) => 2,
            CliError::Declined => 2,
            CliError::Drift(_) => 3,
            CliError::Prerequisite(_) => 4,
            CliError::Locked(_) => 5,
        }
    }
}
