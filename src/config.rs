//! Configuration loader for `stig`.
//!
//! Loads `stig.toml` from the project root (upward search from CWD, or an
//! explicit path), applies environment-variable overrides, and exposes a
//! [`Config`] struct to the rest of the crate.
//!
//! Precedence (highest to lowest):
//! 1. CLI flags — applied via [`Config::apply_cli_overrides`]
//! 2. Environment variables — applied inside [`Config::load`]
//! 3. `stig.toml` values
//! 4. Built-in defaults ([`Default`] impl)

use std::collections::HashMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::CliError;

// ---------------------------------------------------------------------------
// Sub-structs
// ---------------------------------------------------------------------------

/// SQLite PRAGMAs applied on every connection open (`[pragmas]` table).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Pragmas {
    /// `PRAGMA journal_mode`. Defaults to `"WAL"`.
    #[serde(default = "default_journal_mode")]
    pub journal_mode: String,

    /// `PRAGMA foreign_keys`. Defaults to `"ON"`.
    #[serde(default = "default_foreign_keys")]
    pub foreign_keys: String,
}

impl Default for Pragmas {
    fn default() -> Self {
        Self {
            journal_mode: default_journal_mode(),
            foreign_keys: default_foreign_keys(),
        }
    }
}

fn default_journal_mode() -> String {
    "WAL".to_string()
}

fn default_foreign_keys() -> String {
    "ON".to_string()
}

/// A single codegen target entry (`[[generate]]` array).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct GenerateTarget {
    /// Target kind. Currently only `"typescript"` is supported.
    pub kind: String,

    /// Output file path (relative to project root, or absolute).
    pub path: String,

    /// Optional post-generation format command. `{path}` is substituted with
    /// the output path.
    #[serde(default)]
    pub format: Option<String>,

    /// Table-name glob patterns to exclude from codegen output.
    /// Defaults to `["sqlite_%", "schema_migrations"]`.
    #[serde(default = "default_exclude")]
    pub exclude: Vec<String>,
}

fn default_exclude() -> Vec<String> {
    vec!["sqlite_%".to_string(), "schema_migrations".to_string()]
}

// ---------------------------------------------------------------------------
// CLI overrides
// ---------------------------------------------------------------------------

/// Optional overrides supplied via CLI flags. Each field is `None` unless the
/// corresponding flag was explicitly passed.
#[derive(Debug, Default, Clone)]
pub struct CliOverrides {
    pub database_path: Option<String>,
    pub migrations_dir: Option<String>,
    pub backups_dir: Option<String>,
    pub auto_snapshot: Option<bool>,
    pub checksum_check: Option<bool>,
}

// ---------------------------------------------------------------------------
// Main Config struct
// ---------------------------------------------------------------------------

/// Resolved configuration for a `stig` invocation.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Config {
    /// The project root directory: the parent of the `stig.toml` file when
    /// one is found, or `start_dir` when supplied to [`Config::load`], or the
    /// process CWD as a last resort when no config file is found and no
    /// `start_dir` is provided. All relative paths in the config
    /// (`database_path`, `migrations_dir`, `backups_dir`,
    /// `[[generate]].path`) should be resolved against this directory.
    ///
    /// Not serialized — populated by [`Config::load`].
    #[serde(skip)]
    pub project_root: PathBuf,

    /// Path to the live SQLite database.
    #[serde(default = "default_database_path")]
    pub database_path: String,

    /// Directory that contains migration `.sql` files.
    #[serde(default = "default_migrations_dir")]
    pub migrations_dir: String,

    /// Directory for snapshots and reset backups.
    #[serde(default = "default_backups_dir")]
    pub backups_dir: String,

    /// Number of pre-migration snapshots to retain.
    #[serde(default = "default_snapshot_keep")]
    pub snapshot_keep: u32,

    /// Number of reset backups to retain.
    #[serde(default = "default_reset_keep")]
    pub reset_keep: u32,

    /// Whether to snapshot automatically before applying migrations.
    #[serde(default = "default_auto_snapshot")]
    pub auto_snapshot: bool,

    /// Whether to verify migration checksums on `migrate` / `status`.
    #[serde(default = "default_checksum_check")]
    pub checksum_check: bool,

    /// SQLite PRAGMAs applied on every connection open.
    #[serde(default)]
    pub pragmas: Pragmas,

    /// Codegen targets.
    #[serde(default, rename = "generate")]
    pub generate: Vec<GenerateTarget>,
}

// Default value fns used by serde and the Default impl.
fn default_database_path() -> String {
    "app.db".to_string()
}
fn default_migrations_dir() -> String {
    "db/migrations".to_string()
}
fn default_backups_dir() -> String {
    ".local/db-backups".to_string()
}
fn default_snapshot_keep() -> u32 {
    5
}
fn default_reset_keep() -> u32 {
    3
}
fn default_auto_snapshot() -> bool {
    true
}
fn default_checksum_check() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self {
            project_root: PathBuf::new(),
            database_path: default_database_path(),
            migrations_dir: default_migrations_dir(),
            backups_dir: default_backups_dir(),
            snapshot_keep: default_snapshot_keep(),
            reset_keep: default_reset_keep(),
            auto_snapshot: default_auto_snapshot(),
            checksum_check: default_checksum_check(),
            pragmas: Pragmas::default(),
            generate: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Loading logic
// ---------------------------------------------------------------------------

impl Config {
    /// Load configuration from a pre-resolved path (or defaults when `None`),
    /// then apply real process environment overrides.
    ///
    /// Unlike [`Config::load`], this method does **not** run the
    /// `STIG_CONFIG` / `override_path` / upward-search resolution — the caller
    /// is responsible for resolving the path first (e.g. via
    /// [`Config::resolve_path`]). This avoids double-resolution in commands
    /// like `init` that need to know the target path before loading.
    ///
    /// - `resolved_path`: the config file to read, or `None` to use defaults.
    ///   If the path exists it is read; if it does not exist (e.g. on first
    ///   `init`) defaults are used with `start_dir` as the project root.
    /// - `start_dir`: fallback project root when `resolved_path` is absent.
    pub fn load_from(
        resolved_path: Option<&Path>,
        start_dir: Option<&Path>,
    ) -> Result<Self, CliError> {
        dotenvy::dotenv().ok();

        let mut config: Config = match resolved_path.filter(|p| p.exists()) {
            Some(path) => {
                let raw = std::fs::read_to_string(path).map_err(|e| {
                    CliError::Usage(format!("cannot read config file {}: {}", path.display(), e))
                })?;
                let mut cfg: Config = toml::from_str(&raw).map_err(|e| {
                    CliError::Usage(format!("invalid config in {}: {}", path.display(), e))
                })?;
                let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                cfg.project_root = canonical_path
                    .parent()
                    .map(|p| {
                        if p == std::path::Path::new("") {
                            std::env::current_dir().unwrap_or_else(|_| p.to_path_buf())
                        } else {
                            p.to_path_buf()
                        }
                    })
                    .unwrap_or_else(|| canonical_path.clone());
                cfg
            }
            None => {
                let project_root = start_dir
                    .map(|d| d.to_path_buf())
                    .or_else(|| {
                        // If the caller provided a (non-existent) resolved path,
                        // use its parent as the project root.
                        resolved_path
                            .and_then(|p| p.parent())
                            .map(|p| p.to_path_buf())
                    })
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_default();
                Config {
                    project_root,
                    ..Config::default()
                }
            }
        };

        config.apply_env_overrides(None);
        Ok(config)
    }

    /// Load configuration, applying the following precedence:
    ///
    /// 1. Environment variables (from `env`)
    /// 2. `stig.toml` values
    /// 3. Built-in defaults
    ///
    /// # Parameters
    ///
    /// - `override_path`: explicit config file path (e.g. from `--config`).
    ///   Ignored if `STIG_CONFIG` is set in `env`.
    /// - `env`: environment variable map. Pass `None` to use the real process
    ///   environment; pass `Some(&map)` in tests for full isolation.
    /// - `start_dir`: starting directory for the upward config-file search.
    ///   Pass `None` to use the real CWD; pass `Some(path)` in tests to avoid
    ///   touching process state. Note: `start_dir` only controls where the
    ///   upward search begins — if `override_path` or `STIG_CONFIG` is a
    ///   relative path, it is still resolved against the process CWD via
    ///   [`std::fs::canonicalize`], not against `start_dir`.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Usage`] (exit 2) if the config file is found but
    /// contains invalid TOML or fails to deserialise into [`Config`] (reported
    /// as "invalid config").
    ///
    /// A missing config file is **not** an error — defaults are used.
    pub fn load(
        override_path: Option<&Path>,
        env: Option<&HashMap<String, String>>,
        start_dir: Option<&Path>,
    ) -> Result<Self, CliError> {
        // Load .env when using the real process environment. Skip when an
        // injected env map is provided so tests remain fully hermetic.
        if env.is_none() {
            dotenvy::dotenv().ok();
        }

        // Resolve the config file path.
        let config_path = Self::resolve_config_path(override_path, env, start_dir);

        // Parse the file, or start from defaults if there is no file.
        let mut config: Config = match config_path {
            Some(ref path) => {
                let raw = std::fs::read_to_string(path).map_err(|e| {
                    CliError::Usage(format!("cannot read config file {}: {}", path.display(), e))
                })?;
                let mut cfg: Config = toml::from_str(&raw).map_err(|e| {
                    CliError::Usage(format!("invalid config in {}: {}", path.display(), e))
                })?;
                // Set project_root to the directory containing the config file
                // so callers can resolve relative paths correctly regardless of
                // where the process CWD is.
                //
                // Canonicalize the full config file path first so that a bare
                // filename like "stig.toml" (where path.parent() == Some(""))
                // is resolved against CWD before we strip the filename component.
                let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                cfg.project_root = canonical_path
                    .parent()
                    .map(|p| {
                        if p == std::path::Path::new("") {
                            std::env::current_dir().unwrap_or_else(|_| p.to_path_buf())
                        } else {
                            p.to_path_buf()
                        }
                    })
                    .unwrap_or_else(|| canonical_path.clone());
                cfg
            }
            None => {
                // No config file found — use start_dir or CWD as the project root.
                let project_root = start_dir
                    .map(|d| d.to_path_buf())
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_default();
                Config {
                    project_root,
                    ..Config::default()
                }
            }
        };

        // Apply environment-variable overrides.
        config.apply_env_overrides(env);

        Ok(config)
    }

    /// Apply CLI flag overrides on top of the loaded config (step 1 in the
    /// precedence chain). Called by individual commands after [`Config::load`].
    pub fn apply_cli_overrides(&mut self, overrides: &CliOverrides) {
        if let Some(v) = &overrides.database_path {
            self.database_path = v.clone();
        }
        if let Some(v) = &overrides.migrations_dir {
            self.migrations_dir = v.clone();
        }
        if let Some(v) = &overrides.backups_dir {
            self.backups_dir = v.clone();
        }
        if let Some(v) = overrides.auto_snapshot {
            self.auto_snapshot = v;
        }
        if let Some(v) = overrides.checksum_check {
            self.checksum_check = v;
        }
    }

    /// Serialize this config to TOML and write it to `path`.
    ///
    /// `project_root` is skipped during serialization (it has `#[serde(skip)]`)
    /// so the written file only contains the portable, transferable fields.
    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        let toml_str = toml::to_string(self)
            .map_err(|e| anyhow::anyhow!("failed to serialize config: {e}"))?;
        let mut file = std::fs::File::create(path)
            .map_err(|e| anyhow::anyhow!("failed to create {}: {e}", path.display()))?;
        file.write_all(toml_str.as_bytes())
            .map_err(|e| anyhow::anyhow!("failed to write {}: {e}", path.display()))?;
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Resolve the path to the config file using the precedence:
    /// 1. `STIG_CONFIG` env var
    /// 2. `override_path` argument
    /// 3. Upward search from `start_dir` (or CWD) for `stig.toml`
    ///
    /// Returns `None` if no config file is found (not an error).
    ///
    /// This is public so callers (e.g. `stig init`) can determine the
    /// canonical config path for write-target purposes without duplicating
    /// the precedence logic.
    pub fn resolve_path(
        override_path: Option<&Path>,
        env: Option<&HashMap<String, String>>,
        start_dir: Option<&Path>,
    ) -> Option<PathBuf> {
        Self::resolve_config_path(override_path, env, start_dir)
    }

    /// Resolve the path to the config file using the precedence:
    /// 1. `STIG_CONFIG` env var
    /// 2. `override_path` argument
    /// 3. Upward search from `start_dir` (or CWD) for `stig.toml`
    ///
    /// Returns `None` if no config file is found (not an error).
    fn resolve_config_path(
        override_path: Option<&Path>,
        env: Option<&HashMap<String, String>>,
        start_dir: Option<&Path>,
    ) -> Option<PathBuf> {
        // 1. STIG_CONFIG env var — always passed through so the caller gets a
        // clear IO error if the file doesn't exist rather than silently falling
        // back to defaults.
        if let Some(p) = Self::env_get(env, "STIG_CONFIG") {
            return Some(PathBuf::from(p));
        }

        // 2. Explicit override path.
        if let Some(p) = override_path {
            return Some(p.to_path_buf());
        }

        // 3. Upward search from start_dir (or CWD).
        let cwd;
        let search_root: &Path = match start_dir {
            Some(d) => d,
            None => {
                cwd = std::env::current_dir().ok()?;
                &cwd
            }
        };
        Self::find_upward(search_root, "stig.toml")
    }

    /// Walk up the directory tree from `start` looking for `filename`.
    fn find_upward(start: &Path, filename: &str) -> Option<PathBuf> {
        let mut current = start.to_path_buf();
        loop {
            let candidate = current.join(filename);
            if candidate.is_file() {
                return Some(candidate);
            }
            if !current.pop() {
                return None;
            }
        }
    }

    /// Look up an environment variable, using the provided map when in test
    /// mode or falling back to the real process environment.
    fn env_get(env: Option<&HashMap<String, String>>, key: &str) -> Option<String> {
        match env {
            Some(map) => map.get(key).cloned(),
            None => std::env::var(key).ok(),
        }
    }

    /// Apply all environment-variable overrides to `self`.
    fn apply_env_overrides(&mut self, env: Option<&HashMap<String, String>>) {
        // STIG_DATABASE_PATH or DATABASE_PATH
        if let Some(v) =
            Self::env_get(env, "STIG_DATABASE_PATH").or_else(|| Self::env_get(env, "DATABASE_PATH"))
        {
            self.database_path = v;
        }

        // STIG_MIGRATIONS_DIR
        if let Some(v) = Self::env_get(env, "STIG_MIGRATIONS_DIR") {
            self.migrations_dir = v;
        }

        // STIG_BACKUPS_DIR
        if let Some(v) = Self::env_get(env, "STIG_BACKUPS_DIR") {
            self.backups_dir = v;
        }

        // STIG_NO_SNAPSHOT — any non-empty value disables snapshots
        if let Some(v) = Self::env_get(env, "STIG_NO_SNAPSHOT")
            && !v.is_empty()
        {
            self.auto_snapshot = false;
        }

        // STIG_NO_CHECKSUM — any non-empty value disables checksum verification
        if let Some(v) = Self::env_get(env, "STIG_NO_CHECKSUM")
            && !v.is_empty()
        {
            self.checksum_check = false;
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    /// RAII guard that sets an environment variable on construction and removes
    /// it on drop — including on panic/unwind.  This avoids leaving sentinel
    /// values in the process environment if a test fails mid-way.
    ///
    /// `set_var` / `remove_var` are still `unsafe` on stable Rust; mark the
    /// test `#[serial_test::serial]` to serialise it against other tests that
    /// read or write the same key, since the process environment is global state.
    struct EnvGuard {
        key: &'static str,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            // SAFETY: caller must hold #[serial_test::serial] to prevent
            // concurrent access to the process environment from other tests.
            unsafe { std::env::set_var(key, value) };
            EnvGuard { key }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: same requirement as set().
            unsafe { std::env::remove_var(self.key) };
        }
    }

    fn empty_env() -> HashMap<String, String> {
        HashMap::new()
    }

    /// Write `contents` to a fresh [`NamedTempFile`] and return it.
    /// The file stays open (and therefore alive) for the duration of the test.
    fn temp_toml(contents: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    // -----------------------------------------------------------------------
    // 1. Empty / missing config file — all defaults
    // -----------------------------------------------------------------------

    #[test]
    fn upward_search_finds_no_file_returns_defaults() {
        // Use a temp dir that is guaranteed to contain no stig.toml.
        let dir = TempDir::new().unwrap();
        let cfg = Config::load(None, Some(&empty_env()), Some(dir.path())).unwrap();
        // Config values should all be defaults.
        let defaults = Config::default();
        assert_eq!(cfg.database_path, defaults.database_path);
        assert_eq!(cfg.migrations_dir, defaults.migrations_dir);
        assert_eq!(cfg.snapshot_keep, defaults.snapshot_keep);
        assert_eq!(cfg.pragmas, defaults.pragmas);
        // project_root should be set to the start_dir we passed.
        assert_eq!(cfg.project_root, dir.path());
    }

    #[test]
    fn explicit_config_path_not_found_returns_error() {
        // An explicit path that does not exist should produce a CliError::Usage,
        // not silently fall back to defaults. Use a TempDir to guarantee the
        // path is absent without relying on a hard-coded system path.
        let dir = TempDir::new().unwrap();
        let absent = dir.path().join("no_such_stig.toml");
        let result = Config::load(Some(&absent), Some(&empty_env()), None);
        assert!(matches!(result, Err(CliError::Usage(_))));
    }

    #[test]
    fn empty_config_file_returns_defaults() {
        let f = temp_toml("");
        let cfg = Config::load(Some(f.path()), Some(&empty_env()), None).unwrap();
        let defaults = Config::default();
        assert_eq!(cfg.database_path, defaults.database_path);
        assert_eq!(cfg.migrations_dir, defaults.migrations_dir);
        assert_eq!(cfg.snapshot_keep, defaults.snapshot_keep);
        assert_eq!(cfg.pragmas, defaults.pragmas);
        assert_eq!(cfg.generate, defaults.generate);
        // project_root should be the parent directory of the temp file.
        assert_eq!(
            cfg.project_root,
            f.path().parent().unwrap().canonicalize().unwrap()
        );
    }

    // -----------------------------------------------------------------------
    // 2. Partial config — only some keys set, rest use defaults
    // -----------------------------------------------------------------------

    #[test]
    fn partial_config_uses_defaults_for_missing_keys() {
        let f = temp_toml(indoc! {r#"
            database_path = "my.db"
        "#});
        let cfg = Config::load(Some(f.path()), Some(&empty_env()), None).unwrap();
        assert_eq!(cfg.database_path, "my.db");
        assert_eq!(cfg.migrations_dir, "db/migrations"); // default
        assert_eq!(cfg.snapshot_keep, 5); // default
        assert_eq!(cfg.pragmas, Pragmas::default());
    }

    // -----------------------------------------------------------------------
    // 3. Full config — all keys set
    // -----------------------------------------------------------------------

    #[test]
    fn full_config_parsed_correctly() {
        let f = temp_toml(indoc! {r#"
            database_path  = "prod.db"
            migrations_dir = "migrations"
            backups_dir    = "bk"
            snapshot_keep  = 10
            reset_keep     = 2
            auto_snapshot  = false
            checksum_check = false

            [pragmas]
            journal_mode = "DELETE"
            foreign_keys = "OFF"

            [[generate]]
            kind    = "typescript"
            path    = "types.ts"
            format  = "deno fmt {path}"
            exclude = ["sqlite_%"]
        "#});

        let cfg = Config::load(Some(f.path()), Some(&empty_env()), None).unwrap();
        assert_eq!(cfg.database_path, "prod.db");
        assert_eq!(cfg.migrations_dir, "migrations");
        assert_eq!(cfg.backups_dir, "bk");
        assert_eq!(cfg.snapshot_keep, 10);
        assert_eq!(cfg.reset_keep, 2);
        assert!(!cfg.auto_snapshot);
        assert!(!cfg.checksum_check);
        assert_eq!(cfg.pragmas.journal_mode, "DELETE");
        assert_eq!(cfg.pragmas.foreign_keys, "OFF");
        assert_eq!(cfg.generate.len(), 1);
        assert_eq!(cfg.generate[0].kind, "typescript");
        assert_eq!(cfg.generate[0].path, "types.ts");
        assert_eq!(cfg.generate[0].exclude, vec!["sqlite_%".to_string()]);
        assert_eq!(
            cfg.project_root,
            f.path().parent().unwrap().canonicalize().unwrap()
        );
    }

    // -----------------------------------------------------------------------
    // 4. Env var overrides
    // -----------------------------------------------------------------------

    #[test]
    fn env_stig_database_path_overrides_file() {
        let f = temp_toml(indoc! {r#"
            database_path = "file.db"
        "#});

        let mut env = empty_env();
        env.insert("STIG_DATABASE_PATH".into(), "env.db".into());

        let cfg = Config::load(Some(f.path()), Some(&env), None).unwrap();
        assert_eq!(cfg.database_path, "env.db");
    }

    #[test]
    fn env_database_path_fallback_overrides_file() {
        let f = temp_toml(indoc! {r#"
            database_path = "file.db"
        "#});

        let mut env = empty_env();
        env.insert("DATABASE_PATH".into(), "fallback.db".into());

        let cfg = Config::load(Some(f.path()), Some(&env), None).unwrap();
        assert_eq!(cfg.database_path, "fallback.db");
    }

    #[test]
    fn env_stig_database_path_takes_priority_over_database_path() {
        let f = temp_toml("");

        let mut env = empty_env();
        env.insert("STIG_DATABASE_PATH".into(), "stig.db".into());
        env.insert("DATABASE_PATH".into(), "other.db".into());

        let cfg = Config::load(Some(f.path()), Some(&env), None).unwrap();
        assert_eq!(cfg.database_path, "stig.db");
    }

    #[test]
    fn env_stig_migrations_dir_overrides_file() {
        let f = temp_toml("");

        let mut env = empty_env();
        env.insert("STIG_MIGRATIONS_DIR".into(), "custom/migrations".into());

        let cfg = Config::load(Some(f.path()), Some(&env), None).unwrap();
        assert_eq!(cfg.migrations_dir, "custom/migrations");
    }

    #[test]
    fn env_stig_backups_dir_overrides_file() {
        let f = temp_toml("");

        let mut env = empty_env();
        env.insert("STIG_BACKUPS_DIR".into(), "custom/backups".into());

        let cfg = Config::load(Some(f.path()), Some(&env), None).unwrap();
        assert_eq!(cfg.backups_dir, "custom/backups");
    }

    #[test]
    fn env_stig_no_snapshot_disables_auto_snapshot() {
        let f = temp_toml("auto_snapshot = true");

        let mut env = empty_env();
        env.insert("STIG_NO_SNAPSHOT".into(), "1".into());

        let cfg = Config::load(Some(f.path()), Some(&env), None).unwrap();
        assert!(!cfg.auto_snapshot);
    }

    #[test]
    fn env_stig_no_snapshot_empty_string_does_not_disable() {
        let f = temp_toml("auto_snapshot = true");

        let mut env = empty_env();
        env.insert("STIG_NO_SNAPSHOT".into(), "".into());

        let cfg = Config::load(Some(f.path()), Some(&env), None).unwrap();
        assert!(cfg.auto_snapshot);
    }

    #[test]
    fn env_stig_no_checksum_disables_checksum_check() {
        let f = temp_toml("checksum_check = true");

        let mut env = empty_env();
        env.insert("STIG_NO_CHECKSUM".into(), "true".into());

        let cfg = Config::load(Some(f.path()), Some(&env), None).unwrap();
        assert!(!cfg.checksum_check);
    }

    // -----------------------------------------------------------------------
    // 5. CLI overrides
    // -----------------------------------------------------------------------

    #[test]
    fn cli_overrides_applied_on_top_of_env_and_file() {
        let f = temp_toml(indoc! {r#"
            database_path = "file.db"
        "#});

        let mut env = empty_env();
        env.insert("STIG_DATABASE_PATH".into(), "env.db".into());

        let mut cfg = Config::load(Some(f.path()), Some(&env), None).unwrap();
        assert_eq!(cfg.database_path, "env.db"); // env beat file

        let overrides = CliOverrides {
            database_path: Some("cli.db".into()),
            ..Default::default()
        };
        cfg.apply_cli_overrides(&overrides);
        assert_eq!(cfg.database_path, "cli.db"); // CLI beats env
    }

    #[test]
    fn cli_overrides_partial_leaves_other_fields_unchanged() {
        let f = temp_toml("");

        let mut cfg = Config::load(Some(f.path()), Some(&empty_env()), None).unwrap();
        let original_migrations_dir = cfg.migrations_dir.clone();

        let overrides = CliOverrides {
            database_path: Some("cli.db".into()),
            ..Default::default()
        };
        cfg.apply_cli_overrides(&overrides);
        assert_eq!(cfg.database_path, "cli.db");
        assert_eq!(cfg.migrations_dir, original_migrations_dir); // untouched
    }

    #[test]
    fn cli_override_auto_snapshot_false() {
        let f = temp_toml("auto_snapshot = true");

        let mut cfg = Config::load(Some(f.path()), Some(&empty_env()), None).unwrap();
        assert!(cfg.auto_snapshot);

        let overrides = CliOverrides {
            auto_snapshot: Some(false),
            ..Default::default()
        };
        cfg.apply_cli_overrides(&overrides);
        assert!(!cfg.auto_snapshot);
    }

    // -----------------------------------------------------------------------
    // 6. Invalid TOML — should return CliError::Usage (exit 2)
    // -----------------------------------------------------------------------

    #[test]
    fn invalid_toml_returns_usage_error() {
        let f = temp_toml("this is not : valid = toml [[[");

        let result = Config::load(Some(f.path()), Some(&empty_env()), None);
        assert!(result.is_err());
        assert!(
            matches!(result.unwrap_err(), CliError::Usage(_)),
            "expected CliError::Usage"
        );
    }

    #[test]
    fn wrong_type_in_toml_returns_usage_error() {
        // snapshot_keep expects u32, not a string
        let f = temp_toml(r#"snapshot_keep = "five""#);

        let result = Config::load(Some(f.path()), Some(&empty_env()), None);
        assert!(matches!(result, Err(CliError::Usage(_))));
    }

    // -----------------------------------------------------------------------
    // 7. STIG_CONFIG env var respected
    // -----------------------------------------------------------------------

    #[test]
    fn stig_config_env_var_used_over_override_path() {
        // File A: via STIG_CONFIG
        let fa = temp_toml(r#"database_path = "from_stig_config.db""#);

        // File B: via override_path argument (should be ignored)
        let fb = temp_toml(r#"database_path = "from_override.db""#);

        let mut env = empty_env();
        env.insert(
            "STIG_CONFIG".into(),
            fa.path().to_str().unwrap().to_string(),
        );

        let cfg = Config::load(Some(fb.path()), Some(&env), None).unwrap();
        assert_eq!(cfg.database_path, "from_stig_config.db");
    }

    // -----------------------------------------------------------------------
    // 8. project_root edge cases
    // -----------------------------------------------------------------------

    /// Verifies that `project_root` is set to the directory containing the
    /// config file when the path is supplied as an absolute path via
    /// `STIG_CONFIG`.  This exercises the `canonicalize`-before-`parent`
    /// logic added to avoid an empty `project_root` for paths with no
    /// meaningful parent component (e.g. a bare filename resolved by the OS).
    ///
    /// Note: truly testing a bare filename (e.g. `STIG_CONFIG=stig.toml`)
    /// requires a `set_current_dir` + mutex guard to avoid cross-test
    /// interference; that is tracked as a follow-up.  This test confirms the
    /// core correctness property: `project_root` equals the file's parent.
    #[test]
    fn project_root_equals_config_file_parent_when_set_via_stig_config_env() {
        // Write a minimal config inside a fresh temp dir.
        let dir = TempDir::new().unwrap();
        let config_file = dir.path().join("stig.toml");
        std::fs::write(&config_file, r#"database_path = "app.db""#).unwrap();

        let mut env = empty_env();
        env.insert(
            "STIG_CONFIG".into(),
            config_file.to_str().unwrap().to_string(),
        );

        let cfg = Config::load(None, Some(&env), None).unwrap();
        assert_eq!(cfg.database_path, "app.db");
        assert_eq!(cfg.project_root, dir.path().canonicalize().unwrap());
        assert_ne!(cfg.project_root, std::path::PathBuf::new());
        assert_ne!(cfg.project_root, std::path::Path::new(""));
    }

    /// project_root should equal the directory that contains the config file
    /// regardless of what start_dir is supplied.
    #[test]
    fn project_root_is_config_file_parent_not_start_dir() {
        let config_dir = TempDir::new().unwrap();
        let start_dir = TempDir::new().unwrap();

        let config_file = config_dir.path().join("stig.toml");
        std::fs::write(&config_file, r#"database_path = "app.db""#).unwrap();

        // Pass a different start_dir — project_root must still come from the
        // config file's location, not from start_dir.
        let cfg = Config::load(
            Some(&config_file),
            Some(&empty_env()),
            Some(start_dir.path()),
        )
        .unwrap();

        assert_eq!(cfg.project_root, config_dir.path().canonicalize().unwrap());
        assert_ne!(cfg.project_root, start_dir.path().canonicalize().unwrap());
    }

    // -----------------------------------------------------------------------
    // 9. Hermetic env injection
    // -----------------------------------------------------------------------

    /// When an env map is injected, `load()` must NOT read the real process
    /// environment — even if a relevant env var is set in the process env.
    ///
    /// This test sets `STIG_DATABASE_PATH` in the real process environment for
    /// the duration of the test via an [`EnvGuard`] (which removes the var on
    /// drop, even on panic), then calls `load()` with an injected empty map.
    /// The resulting `database_path` must be the default, proving that
    /// `load()` consulted the injected map rather than `std::env::var`.
    ///
    /// Marked `#[serial]` because it mutates the process environment, which is
    /// global state that could interfere with other tests running concurrently.
    #[test]
    #[serial_test::serial]
    fn injected_env_map_prevents_process_env_leaking_into_load() {
        const KEY: &str = "STIG_DATABASE_PATH";
        const SENTINEL: &str = "__stig_test_sentinel__.db";

        // Guard removes KEY from the env on drop — including on panic.
        let _guard = EnvGuard::set(KEY, SENTINEL);

        let f = temp_toml("");
        let cfg = Config::load(Some(f.path()), Some(&empty_env()), None).unwrap();

        // The process env had STIG_DATABASE_PATH=SENTINEL, but the injected
        // empty map should have been used instead.
        assert_ne!(
            cfg.database_path, SENTINEL,
            "load() must not read the real process env when an injected map is provided"
        );
        assert_eq!(cfg.database_path, Config::default().database_path);
    }

    // -----------------------------------------------------------------------
    // 10. Config::write round-trips through Config::load
    // -----------------------------------------------------------------------

    #[test]
    fn write_round_trips_default_config() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("stig.toml");

        let original = Config {
            project_root: dir.path().to_path_buf(),
            ..Config::default()
        };
        original.write(&config_path).unwrap();

        // The file must exist and be valid TOML that round-trips cleanly.
        let loaded = Config::load(Some(&config_path), Some(&empty_env()), None).unwrap();
        assert_eq!(loaded.database_path, original.database_path);
        assert_eq!(loaded.migrations_dir, original.migrations_dir);
        assert_eq!(loaded.backups_dir, original.backups_dir);
        assert_eq!(loaded.snapshot_keep, original.snapshot_keep);
        assert_eq!(loaded.reset_keep, original.reset_keep);
        assert_eq!(loaded.auto_snapshot, original.auto_snapshot);
        assert_eq!(loaded.checksum_check, original.checksum_check);
        assert_eq!(loaded.pragmas, original.pragmas);
        assert_eq!(loaded.generate, original.generate);
    }

    #[test]
    fn write_non_default_values_round_trip() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("stig.toml");

        let original = Config {
            project_root: dir.path().to_path_buf(),
            database_path: "custom.db".to_string(),
            migrations_dir: "schema/migrations".to_string(),
            snapshot_keep: 10,
            auto_snapshot: false,
            pragmas: Pragmas {
                journal_mode: "DELETE".to_string(),
                foreign_keys: "ON".to_string(),
            },
            ..Config::default()
        };
        original.write(&config_path).unwrap();

        let loaded = Config::load(Some(&config_path), Some(&empty_env()), None).unwrap();
        assert_eq!(loaded.database_path, "custom.db");
        assert_eq!(loaded.migrations_dir, "schema/migrations");
        assert_eq!(loaded.snapshot_keep, 10);
        assert!(!loaded.auto_snapshot);
        assert_eq!(loaded.pragmas.journal_mode, "DELETE");
    }
}
