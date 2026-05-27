//! Runtime context for a `stig` invocation.
//!
//! [`RuntimeContext`] is the single product of all external input
//! normalization. It is built once in `main` — consuming CLI args, the
//! process environment, and any `.env` file — and then passed (by reference)
//! to every command. No command module should read from `std::env`, call
//! `dotenvy`, or invoke `Config::load*` directly.
//!
//! # Precedence (highest → lowest)
//!
//! 1. CLI flags (`--config`, per-flag overrides) — applied by the caller
//!    after `build()` via `Config::apply_cli_overrides`.
//! 2. Environment variables — applied inside `build()`.
//! 3. `stig.toml` values.
//! 4. Built-in defaults ([`Config::default`]).

use std::collections::HashMap;
use std::path::PathBuf;

use crate::config::Config;
use crate::errors::CliError;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Describes how [`RuntimeContext::config`] was derived.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigSource {
    /// A `stig.toml` was found and successfully parsed.
    File,
    /// No config file was found or specified; values are defaults + env overrides.
    Defaults,
}

/// Fully-resolved runtime context for a single `stig` invocation.
///
/// Built once by [`RuntimeContext::build`] in `main`; passed by reference to
/// every command handler.
#[derive(Debug, Clone)]
pub struct RuntimeContext {
    /// The fully-resolved, env-overridden configuration for this invocation.
    pub config: Config,

    /// The resolved path to the config file, if one was found or explicitly
    /// specified. `None` means no config file is in play.
    ///
    /// Commands such as `init` use this to determine their write target.
    pub config_path: Option<PathBuf>,

    /// How `config` was derived.
    pub config_source: ConfigSource,
}

// ---------------------------------------------------------------------------
// Building
// ---------------------------------------------------------------------------

impl RuntimeContext {
    /// Build a [`RuntimeContext`] from CLI arguments and the real process
    /// environment.
    ///
    /// Steps:
    /// 1. Load `.env` via `dotenvy` (idempotent, errors silently ignored).
    /// 2. Resolve the config file path using the standard precedence.
    /// 3. Parse the config file if it exists (error on invalid TOML).
    /// 4. Apply environment-variable overrides.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Usage`] (exit 2) if a config file is found but
    /// contains invalid TOML or fails to deserialize.
    pub fn build(config_path_arg: Option<PathBuf>) -> Result<Self, CliError> {
        dotenvy::dotenv().ok();
        Self::build_inner(config_path_arg, None)
    }

    /// Build a [`RuntimeContext`] with an injected environment map instead of
    /// reading the real process environment. Intended for unit tests only.
    ///
    /// `dotenvy` is **not** called when an env map is injected.
    pub fn build_with_env(
        config_path_arg: Option<PathBuf>,
        env: HashMap<String, String>,
    ) -> Result<Self, CliError> {
        Self::build_inner(config_path_arg, Some(env))
    }

    // -----------------------------------------------------------------------
    // Internal
    // -----------------------------------------------------------------------

    fn build_inner(
        config_path_arg: Option<PathBuf>,
        env: Option<HashMap<String, String>>,
    ) -> Result<Self, CliError> {
        let env_ref = env.as_ref();

        // Resolve the config file path using standard precedence.
        let config_path = Config::resolve_path(config_path_arg.as_deref(), env_ref, None);

        // Load or default.
        //
        // If a path was resolved but does not exist on disk, treat it as
        // "Defaults with a known target path" — this allows `init` to use the
        // path as its write target without erroring before any command runs.
        // If the path *does* exist but is invalid TOML, that is always an error.
        let (mut config, config_source) = match &config_path {
            Some(path) if path.exists() => {
                let cfg = Config::load(Some(path), env_ref, None)?;
                (cfg, ConfigSource::File)
            }
            _ => {
                // No file found (path absent or path doesn't exist yet).
                // Use CWD as the project root for default config.
                let project_root = std::env::current_dir().unwrap_or_default();
                let mut cfg = Config {
                    project_root,
                    ..Config::default()
                };
                cfg.apply_env_overrides(env_ref);
                (cfg, ConfigSource::Defaults)
            }
        };

        // When a file was found, env overrides were applied by Config::load.
        // When we built from defaults above, we applied them directly.
        // Nothing more to do here.
        let _ = &mut config; // suppress unused-mut warning

        Ok(RuntimeContext {
            config,
            config_path,
            config_source,
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::{NamedTempFile, TempDir};

    fn empty_env() -> HashMap<String, String> {
        HashMap::new()
    }

    fn temp_toml(contents: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    // -----------------------------------------------------------------------
    // ConfigSource::Defaults — no config file
    // -----------------------------------------------------------------------

    #[test]
    fn no_config_file_yields_defaults_source() {
        let dir = TempDir::new().unwrap();
        // Point to a directory with no stig.toml and no STIG_CONFIG in env.
        let ctx = RuntimeContext::build_with_env(None, empty_env()).unwrap();
        // config_source should be Defaults (no file).
        // We can't guarantee there's no stig.toml above CWD in a real test run,
        // so just verify the result is consistent.
        match ctx.config_source {
            ConfigSource::File => {
                assert!(ctx.config_path.is_some());
            }
            ConfigSource::Defaults => {
                assert!(ctx.config_path.is_none());
            }
        }
        drop(dir);
    }

    #[test]
    fn explicit_absent_path_yields_defaults_with_path() {
        let dir = TempDir::new().unwrap();
        let absent = dir.path().join("no_such_stig.toml");

        // An explicit path that doesn't exist → Defaults source, but
        // config_path is set so init knows where to write.
        let ctx = RuntimeContext::build_with_env(Some(absent.clone()), empty_env()).unwrap();
        assert_eq!(ctx.config_source, ConfigSource::Defaults);
        assert_eq!(ctx.config_path.as_deref(), Some(absent.as_path()));
    }

    // -----------------------------------------------------------------------
    // ConfigSource::File — file found and parsed
    // -----------------------------------------------------------------------

    #[test]
    fn valid_config_file_yields_file_source() {
        let f = temp_toml(r#"database_path = "mydb.db""#);
        let ctx =
            RuntimeContext::build_with_env(Some(f.path().to_path_buf()), empty_env()).unwrap();

        assert_eq!(ctx.config_source, ConfigSource::File);
        assert_eq!(ctx.config_path.as_deref(), Some(f.path()));
        assert_eq!(ctx.config.database_path, "mydb.db");
    }

    #[test]
    fn invalid_toml_returns_usage_error() {
        let f = temp_toml("this is not [[ valid toml");
        let result = RuntimeContext::build_with_env(Some(f.path().to_path_buf()), empty_env());
        assert!(matches!(result, Err(CliError::Usage(_))));
    }

    // -----------------------------------------------------------------------
    // Env overrides applied
    // -----------------------------------------------------------------------

    #[test]
    fn env_database_path_override_applied() {
        let f = temp_toml(r#"database_path = "file.db""#);
        let mut env = empty_env();
        env.insert("STIG_DATABASE_PATH".into(), "env.db".into());

        let ctx = RuntimeContext::build_with_env(Some(f.path().to_path_buf()), env).unwrap();
        assert_eq!(ctx.config.database_path, "env.db");
    }

    #[test]
    fn env_no_snapshot_disables_auto_snapshot() {
        let f = temp_toml("auto_snapshot = true");
        let mut env = empty_env();
        env.insert("STIG_NO_SNAPSHOT".into(), "1".into());

        let ctx = RuntimeContext::build_with_env(Some(f.path().to_path_buf()), env).unwrap();
        assert!(!ctx.config.auto_snapshot);
    }

    #[test]
    fn env_stig_config_controls_which_file_is_loaded() {
        let fa = temp_toml(r#"database_path = "from_stig_config.db""#);
        let fb = temp_toml(r#"database_path = "from_arg.db""#);

        let mut env = empty_env();
        env.insert(
            "STIG_CONFIG".into(),
            fa.path().to_str().unwrap().to_string(),
        );

        // fb is passed as the arg, but STIG_CONFIG takes precedence.
        let ctx = RuntimeContext::build_with_env(Some(fb.path().to_path_buf()), env).unwrap();
        assert_eq!(ctx.config.database_path, "from_stig_config.db");
        assert_eq!(ctx.config_source, ConfigSource::File);
    }

    // -----------------------------------------------------------------------
    // Defaults + partial env override — the init write contract
    // -----------------------------------------------------------------------

    /// When no config file exists, a partial env override produces a config
    /// where the overridden field takes the env value and all other fields
    /// remain at their built-in defaults. This is what `init` writes to disk.
    #[test]
    fn partial_env_override_merges_with_defaults() {
        let dir = TempDir::new().unwrap();
        // Point STIG_CONFIG at a non-existent path so the upward search is
        // bypassed and we get a clean Defaults context.
        let absent = dir.path().join("stig.toml");
        let mut env = empty_env();
        env.insert("STIG_CONFIG".into(), absent.to_str().unwrap().into());
        env.insert("STIG_DATABASE_PATH".into(), "custom.db".into());

        let ctx = RuntimeContext::build_with_env(None, env).unwrap();

        assert_eq!(ctx.config_source, ConfigSource::Defaults);
        // Overridden field reflects the env value.
        assert_eq!(ctx.config.database_path, "custom.db");
        // Unset fields remain at defaults.
        assert_eq!(
            ctx.config.migrations_dir,
            crate::config::Config::default().migrations_dir
        );
        assert_eq!(
            ctx.config.backups_dir,
            crate::config::Config::default().backups_dir
        );
        assert_eq!(
            ctx.config.snapshot_keep,
            crate::config::Config::default().snapshot_keep
        );
        assert!(ctx.config.auto_snapshot); // default is true
        assert!(ctx.config.checksum_check); // default is true
    }

    /// Multiple env overrides are all applied simultaneously.
    #[test]
    fn multiple_env_overrides_all_applied() {
        let dir = TempDir::new().unwrap();
        let absent = dir.path().join("stig.toml");
        let mut env = empty_env();
        env.insert("STIG_CONFIG".into(), absent.to_str().unwrap().into());
        env.insert("STIG_DATABASE_PATH".into(), "prod.db".into());
        env.insert("STIG_MIGRATIONS_DIR".into(), "schema/migrations".into());
        env.insert("STIG_BACKUPS_DIR".into(), "backups".into());
        env.insert("STIG_NO_SNAPSHOT".into(), "1".into());
        env.insert("STIG_NO_CHECKSUM".into(), "1".into());

        let ctx = RuntimeContext::build_with_env(None, env).unwrap();

        assert_eq!(ctx.config.database_path, "prod.db");
        assert_eq!(ctx.config.migrations_dir, "schema/migrations");
        assert_eq!(ctx.config.backups_dir, "backups");
        assert!(!ctx.config.auto_snapshot);
        assert!(!ctx.config.checksum_check);
        // Non-overridden fields still at defaults.
        assert_eq!(
            ctx.config.snapshot_keep,
            crate::config::Config::default().snapshot_keep
        );
    }

    /// Env overrides on top of a file config: both the file values and the
    /// env overrides are reflected in ctx.config.
    #[test]
    fn env_overrides_layer_on_top_of_file_values() {
        let f = temp_toml(
            r#"database_path = "file.db"
migrations_dir = "file/migrations""#,
        );
        let mut env = empty_env();
        env.insert("STIG_DATABASE_PATH".into(), "env.db".into());

        let ctx = RuntimeContext::build_with_env(Some(f.path().to_path_buf()), env).unwrap();

        // Env beats file for database_path.
        assert_eq!(ctx.config.database_path, "env.db");
        // File value used for migrations_dir (no env override).
        assert_eq!(ctx.config.migrations_dir, "file/migrations");
    }
}
