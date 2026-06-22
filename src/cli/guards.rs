//! Precondition guards shared across CLI subcommands.

use std::path::PathBuf;

use crate::config::Runtime;
use crate::errors::CliError;

/// Return the resolved migrations directory path, or exit 4 if it is missing.
///
/// Every subcommand that needs to read migration files calls this before
/// opening the database or invoking `discover()`. Using a single helper
/// keeps the error message, exit code, and wording consistent across the CLI.
pub fn require_migrations_dir(config: &Runtime) -> anyhow::Result<PathBuf> {
    let dir = config.migrations_path();
    if !dir.is_dir() {
        return Err(CliError::Prerequisite(format!(
            "migrations directory not found: {} — run `stig init` first",
            dir.display()
        ))
        .into());
    }
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigFile, Runtime};
    use tempfile::TempDir;

    fn runtime_with_migrations_dir(dir: &std::path::Path) -> Runtime {
        Runtime {
            project_root: dir.to_path_buf(),
            file: ConfigFile {
                database_path: "app.db".to_string(),
                migrations_dir: "db/migrations".to_string(),
                ..ConfigFile::default()
            },
        }
    }

    #[test]
    fn existing_directory_returns_path() {
        let tmp = TempDir::new().unwrap();
        let migrations_dir = tmp.path().join("db/migrations");
        std::fs::create_dir_all(&migrations_dir).unwrap();

        let config = runtime_with_migrations_dir(tmp.path());
        let result = require_migrations_dir(&config).unwrap();
        assert_eq!(result, migrations_dir);
    }

    #[test]
    fn missing_directory_returns_prerequisite() {
        let tmp = TempDir::new().unwrap();
        let config = runtime_with_migrations_dir(tmp.path());

        let err = require_migrations_dir(&config).unwrap_err();
        let cli_err = err.downcast_ref::<CliError>().expect("should be CliError");
        assert!(matches!(cli_err, CliError::Prerequisite(_)));
        assert_eq!(cli_err.exit_code(), 4);
        assert!(
            cli_err
                .to_string()
                .contains("migrations directory not found")
        );
        assert!(cli_err.to_string().contains("run `stig init` first"));
    }

    #[test]
    fn file_instead_of_directory_returns_prerequisite() {
        let tmp = TempDir::new().unwrap();
        let migrations_dir = tmp.path().join("db/migrations");
        std::fs::create_dir_all(migrations_dir.parent().unwrap()).unwrap();
        std::fs::write(&migrations_dir, "not a directory").unwrap();

        let config = runtime_with_migrations_dir(tmp.path());
        let err = require_migrations_dir(&config).unwrap_err();
        let cli_err = err.downcast_ref::<CliError>().expect("should be CliError");
        assert!(matches!(cli_err, CliError::Prerequisite(_)));
    }
}
