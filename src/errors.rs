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
    /// Classify an unhandled `anyhow::Error` into the appropriate `CliError`
    /// variant.
    ///
    /// This is the single entry point for mapping raw `anyhow::Error`s to
    /// `CliError` exit-code variants. Currently only SQLite busy/locked failures
    /// are special-cased (to `CliError::Locked`); everything else becomes
    /// `CliError::Generic`.
    pub fn classify(err: anyhow::Error) -> Self {
        if let Some(sqlite_err) = err.downcast_ref::<rusqlite::Error>()
            && is_sqlite_locked(sqlite_err)
        {
            return CliError::Locked(err.to_string());
        }
        CliError::Generic(err)
    }

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

/// Returns `true` if `err` is a SQLite busy or locked failure.
fn is_sqlite_locked(err: &rusqlite::Error) -> bool {
    matches!(
        err,
        rusqlite::Error::SqliteFailure(code, _)
            if code.extended_code == rusqlite::ffi::SQLITE_BUSY
                || code.extended_code == rusqlite::ffi::SQLITE_LOCKED
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sqlite_error(code: rusqlite::ErrorCode, extended_code: i32) -> anyhow::Error {
        let sqlite_err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error {
                code,
                extended_code,
            },
            Some("database is locked".to_string()),
        );
        anyhow::Error::new(sqlite_err)
    }

    #[test]
    fn maps_sqlite_busy_to_locked() {
        let err = sqlite_error(
            rusqlite::ErrorCode::DatabaseBusy,
            rusqlite::ffi::SQLITE_BUSY,
        );
        assert!(matches!(CliError::classify(err), CliError::Locked(_)));
    }

    #[test]
    fn maps_sqlite_locked_to_locked() {
        let err = sqlite_error(
            rusqlite::ErrorCode::DatabaseLocked,
            rusqlite::ffi::SQLITE_LOCKED,
        );
        assert!(matches!(CliError::classify(err), CliError::Locked(_)));
    }

    #[test]
    fn maps_wrapped_sqlite_busy_to_locked() {
        let err = sqlite_error(
            rusqlite::ErrorCode::DatabaseBusy,
            rusqlite::ffi::SQLITE_BUSY,
        )
        .context("failed to open database");
        assert!(matches!(CliError::classify(err), CliError::Locked(_)));
    }

    #[test]
    fn maps_other_errors_to_generic() {
        let err = anyhow::anyhow!("some other failure");
        assert!(matches!(CliError::classify(err), CliError::Generic(_)));
    }
}
