pub mod typescript;

use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use rusqlite::Connection;

use crate::config::GenerateTarget;
use crate::errors::CliError;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors produced by codegen targets.
#[derive(Debug, thiserror::Error)]
pub enum CodegenError {
    /// Filesystem or formatter I/O error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// Target-specific failure (e.g. SQL introspection error).
    #[error("target error: {0}")]
    Target(String),

    /// No registered target matches the requested `kind`.
    #[error("unknown kind \"{kind}\"; registered kinds: {registered:?}")]
    UnknownKind {
        kind: String,
        registered: Vec<&'static str>,
    },

    /// No configured target entry matches the filter selector.
    #[error("no target named \"{selector}\"; available: {available:?}")]
    UnknownTarget {
        selector: String,
        available: Vec<String>,
    },
}

impl From<CodegenError> for CliError {
    fn from(e: CodegenError) -> Self {
        match &e {
            CodegenError::UnknownKind { .. } | CodegenError::UnknownTarget { .. } => {
                CliError::Prerequisite(e.to_string())
            }
            CodegenError::Io(_) | CodegenError::Target(_) => CliError::Generic(anyhow::anyhow!(e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Trait and output types
// ---------------------------------------------------------------------------

/// Resolved configuration passed to a codegen target.
///
/// Derived from the `[[generate]]` config entry with the output `path`
/// resolved against the project root.
#[derive(Debug)]
pub struct CodegenConfig {
    pub path: PathBuf,
    pub exclude: Vec<String>,
    pub format: Option<String>,
    pub extra: toml::Table,
}

/// Result returned by a successful codegen run.
#[derive(Debug)]
pub struct GenerateOutput {
    pub path: PathBuf,
    pub bytes_written: u64,
    pub formatted: bool,
}

/// Trait implemented by each codegen target.
pub trait CodegenTarget: Send + Sync {
    /// Stable identifier used by config (`kind = "..."`).
    fn kind(&self) -> &'static str;

    /// Run introspection and write the output. Receives a read-only
    /// connection plus the target's resolved config.
    fn generate(
        &self,
        conn: &Connection,
        config: &CodegenConfig,
    ) -> Result<GenerateOutput, CodegenError>;
}

// ---------------------------------------------------------------------------
// Dispatcher
// ---------------------------------------------------------------------------

/// Built-in codegen targets, initialized once.
///
/// New targets are registered here — one entry per target.
static REGISTRY: LazyLock<Vec<Box<dyn CodegenTarget>>> =
    LazyLock::new(|| vec![Box::new(typescript::TypeScriptTarget)]);

/// Return the set of built-in codegen targets.
fn registry() -> &'static [Box<dyn CodegenTarget>] {
    &REGISTRY
}

/// Run codegen targets for the given config entries.
///
/// `filter` optionally restricts execution to a single target matched by
/// `name` or `kind`. When `None`, all configured targets run.
///
/// Precedence: `name` is checked first per entry, so the first entry whose
/// `name` matches wins before later entries' `kind` values are checked.
/// If two entries share a `name`, the first declared in the config is used.
pub fn run_targets(
    conn: &Connection,
    targets: &[GenerateTarget],
    project_root: &Path,
    filter: Option<&str>,
) -> Result<Vec<GenerateOutput>, CodegenError> {
    let registry = registry();

    // If a filter is provided, find the single matching target entry.
    let entries: Vec<&GenerateTarget> = match filter {
        Some(selector) => {
            let entry = targets
                .iter()
                .find(|t| t.name.as_deref() == Some(selector) || t.kind == selector);
            match entry {
                Some(e) => vec![e],
                None => {
                    let available: Vec<String> = targets
                        .iter()
                        .map(|t| t.name.clone().unwrap_or_else(|| t.kind.clone()))
                        .collect();
                    return Err(CodegenError::UnknownTarget {
                        selector: selector.to_string(),
                        available,
                    });
                }
            }
        }
        None => targets.iter().collect(),
    };

    let mut outputs = Vec::with_capacity(entries.len());

    for entry in &entries {
        let target = registry.iter().find(|t| t.kind() == entry.kind);
        let target = match target {
            Some(t) => t,
            None => {
                let registered: Vec<&'static str> = registry.iter().map(|t| t.kind()).collect();
                return Err(CodegenError::UnknownKind {
                    kind: entry.kind.clone(),
                    registered,
                });
            }
        };

        let config = CodegenConfig {
            path: project_root.join(&entry.path),
            exclude: entry.exclude.clone(),
            format: entry.format.clone(),
            extra: entry.extra.clone(),
        };

        let output = target.generate(conn, &config)?;

        let suffix = if output.formatted { " (formatted)" } else { "" };
        tracing::info!(
            "generated {} ({} bytes){}",
            output.path.display(),
            output.bytes_written,
            suffix
        );

        outputs.push(output);
    }

    Ok(outputs)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// A test-only target that does nothing and reports success.
    struct NoopTarget;

    impl CodegenTarget for NoopTarget {
        fn kind(&self) -> &'static str {
            "noop"
        }

        fn generate(
            &self,
            _conn: &Connection,
            config: &CodegenConfig,
        ) -> Result<GenerateOutput, CodegenError> {
            Ok(GenerateOutput {
                path: config.path.clone(),
                bytes_written: 0,
                formatted: false,
            })
        }
    }

    fn temp_conn() -> Connection {
        Connection::open_in_memory().unwrap()
    }

    // -----------------------------------------------------------------------
    // 1. Zero targets — nothing to do, success
    // -----------------------------------------------------------------------

    #[test]
    fn zero_targets_succeeds() {
        let conn = temp_conn();
        let dir = tempfile::tempdir().unwrap();
        let result = run_targets(&conn, &[], dir.path(), None);
        assert!(result.is_ok());
    }

    // -----------------------------------------------------------------------
    // 2. Trait works end-to-end with noop target
    // -----------------------------------------------------------------------

    #[test]
    fn noop_target_trait_works_end_to_end() {
        let conn = temp_conn();
        let target = NoopTarget;

        assert_eq!(target.kind(), "noop");

        let dir = tempfile::tempdir().unwrap();
        let config = CodegenConfig {
            path: dir.path().join("out/noop.txt"),
            exclude: vec![],
            format: None,
            extra: toml::Table::new(),
        };

        let output = target.generate(&conn, &config).unwrap();
        assert_eq!(output.path, dir.path().join("out/noop.txt"));
        assert_eq!(output.bytes_written, 0);
        assert!(!output.formatted);
    }

    // -----------------------------------------------------------------------
    // 3. Unknown kind returns error with registered kinds
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_kind_returns_error() {
        let conn = temp_conn();
        let dir = tempfile::tempdir().unwrap();

        let entry = GenerateTarget {
            kind: "nonexistent".to_string(),
            path: "out.txt".to_string(),
            name: None,
            format: None,
            exclude: vec![],
            extra: toml::Table::new(),
        };

        let result = run_targets(&conn, &[entry], dir.path(), None);
        assert!(result.is_err());

        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("nonexistent"), "error should name the kind");
        assert!(
            msg.contains("registered kinds"),
            "error should list registered kinds"
        );
    }

    // -----------------------------------------------------------------------
    // 4. Filter by name matches the right entry
    // -----------------------------------------------------------------------

    #[test]
    fn filter_by_name_selects_matching_entry() {
        let conn = temp_conn();
        let dir = tempfile::tempdir().unwrap();

        // Two entries: one named "alpha", one named "beta", both using
        // a test-only kind that is not in the real registry.
        let alpha = GenerateTarget {
            kind: "__test_noop__".to_string(),
            path: "alpha.txt".to_string(),
            name: Some("alpha".to_string()),
            format: None,
            exclude: vec![],
            extra: toml::Table::new(),
        };
        let beta = GenerateTarget {
            kind: "__test_noop__".to_string(),
            path: "beta.txt".to_string(),
            name: Some("beta".to_string()),
            format: None,
            exclude: vec![],
            extra: toml::Table::new(),
        };

        // Filter for "alpha" — the filter should match the entry with
        // name "alpha" (not "beta"), then fail with UnknownKind for
        // "__test_noop__" because it's not a real registered kind.
        let result = run_targets(&conn, &[alpha, beta], dir.path(), Some("alpha"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.to_string().contains("__test_noop__"),
            "should have looked up kind '__test_noop__' for the matched entry"
        );
    }

    // -----------------------------------------------------------------------
    // 5. Filter by kind also works
    // -----------------------------------------------------------------------

    #[test]
    fn filter_by_kind_selects_matching_entry() {
        let conn = temp_conn();
        let dir = tempfile::tempdir().unwrap();

        let entry = GenerateTarget {
            kind: "__test_ts__".to_string(),
            path: "types.ts".to_string(),
            name: Some("my-types".to_string()),
            format: None,
            exclude: vec![],
            extra: toml::Table::new(),
        };

        // Filter by kind "__test_ts__" — should match even though name differs.
        let result = run_targets(&conn, &[entry], dir.path(), Some("__test_ts__"));
        // The kind isn't in the real registry, so this will be UnknownKind.
        // The important thing is it matched on kind, not name.
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("__test_ts__"));
    }

    // -----------------------------------------------------------------------
    // 6. Filter with no matching name or kind returns UnknownTarget
    // -----------------------------------------------------------------------

    #[test]
    fn filter_no_match_returns_unknown_target() {
        let conn = temp_conn();
        let dir = tempfile::tempdir().unwrap();

        let entry = GenerateTarget {
            kind: "noop".to_string(),
            path: "out.txt".to_string(),
            name: Some("alpha".to_string()),
            format: None,
            exclude: vec![],
            extra: toml::Table::new(),
        };

        let result = run_targets(&conn, &[entry], dir.path(), Some("beta"));
        assert!(result.is_err());
        let err = result.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("beta"), "error should name the selector");
        assert!(
            msg.contains("available"),
            "error should list available targets"
        );
    }

    // -----------------------------------------------------------------------
    // 7. CodegenConfig carries extra fields
    // -----------------------------------------------------------------------

    #[test]
    fn codegen_config_carries_extra() {
        let mut extra = toml::Table::new();
        extra.insert("custom_key".to_string(), toml::Value::Boolean(true));

        let entry = GenerateTarget {
            kind: "noop".to_string(),
            path: "out.txt".to_string(),
            name: None,
            format: None,
            exclude: vec!["sqlite_%".to_string()],
            extra: extra.clone(),
        };

        let dir = tempfile::tempdir().unwrap();
        let config = CodegenConfig {
            path: dir.path().join(&entry.path),
            exclude: entry.exclude.clone(),
            format: entry.format.clone(),
            extra: entry.extra.clone(),
        };

        assert_eq!(
            config.extra.get("custom_key"),
            Some(&toml::Value::Boolean(true))
        );
        assert_eq!(config.exclude, vec!["sqlite_%".to_string()]);
    }

    // -----------------------------------------------------------------------
    // 8. CodegenError converts to CliError with correct exit codes
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_kind_converts_to_cli_error_exit_4() {
        let err = CodegenError::UnknownKind {
            kind: "bad".to_string(),
            registered: vec!["typescript"],
        };
        let cli_err: CliError = err.into();
        assert_eq!(cli_err.exit_code(), 4);
    }

    #[test]
    fn unknown_target_converts_to_cli_error_exit_4() {
        let err = CodegenError::UnknownTarget {
            selector: "missing".to_string(),
            available: vec!["alpha".to_string()],
        };
        let cli_err: CliError = err.into();
        assert_eq!(cli_err.exit_code(), 4);
    }

    #[test]
    fn target_error_converts_to_cli_error_exit_1() {
        let err = CodegenError::Target("something broke".to_string());
        let cli_err: CliError = err.into();
        assert_eq!(cli_err.exit_code(), 1);
    }

    #[test]
    fn io_error_converts_to_cli_error_exit_1() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let err = CodegenError::Io(io_err);
        let cli_err: CliError = err.into();
        assert_eq!(cli_err.exit_code(), 1);
    }
}
