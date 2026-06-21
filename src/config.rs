//! Configuration loader for `stig`.
//!
//! Loads `stig.toml` from the project root (upward search from CWD, or an
//! explicit path), applies environment-variable overrides, and exposes a
//! [`Runtime`] struct (on-disk [`ConfigFile`] plus resolved `project_root`)
//! to the rest of the crate.
//!
//! Precedence (highest to lowest):
//! 1. CLI flags — applied via [`Runtime::apply_cli_overrides`]
//! 2. Environment variables — applied inside [`Runtime::load`]
//! 3. `stig.toml` values
//! 4. Built-in defaults ([`Default`] impl)

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::errors::CliError;

// ---------------------------------------------------------------------------
// Environment source abstraction
// ---------------------------------------------------------------------------

/// Sealed trait for environment-variable sources. Prevents downstream crates
/// from implementing their own variants — only [`ProcessEnv`] and [`MapEnv`]
/// are valid.
pub mod env_source {
    use std::collections::HashMap;

    mod sealed {
        pub trait Sealed {}
    }

    /// Abstraction over environment-variable lookups.
    pub trait EnvSource: sealed::Sealed {
        /// Load `.env` file if applicable (no-op for map-backed sources).
        fn load_dotenv(&self);

        /// Look up a single key.
        fn get_var(&self, key: &str) -> Option<String>;
    }

    /// Production source: reads the real process environment and loads `.env`.
    #[derive(Debug, Clone, Copy, Default)]
    pub struct ProcessEnv;

    impl sealed::Sealed for ProcessEnv {}

    impl EnvSource for ProcessEnv {
        fn load_dotenv(&self) {
            dotenvy::dotenv().ok();
        }

        fn get_var(&self, key: &str) -> Option<String> {
            std::env::var(key).ok()
        }
    }

    /// Reads from an injected [`HashMap`] only. Never touches the real process
    /// environment, making the hermetic contract compile-time enforced.
    #[derive(Debug, Clone, Default)]
    pub struct MapEnv(pub HashMap<String, String>);

    impl sealed::Sealed for MapEnv {}

    impl EnvSource for MapEnv {
        fn load_dotenv(&self) {
            // No-op: tests must not depend on .env files.
        }

        fn get_var(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }
}

use env_source::{EnvSource, ProcessEnv};

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

    /// Optional human-friendly name. Used by `stig generate <name>` to select
    /// a single target. Falls back to `kind` when not set.
    #[serde(default)]
    pub name: Option<String>,

    /// Table-name glob patterns to exclude from codegen output.
    ///
    /// When dispatching through [`crate::codegen::run_targets`], internal
    /// tables (`sqlite_%` and `schema_migrations`) are automatically added
    /// to the effective exclude list.
    #[serde(default)]
    pub exclude: Vec<String>,

    /// Kind-specific options captured from unknown TOML keys.
    ///
    /// Note: `flatten` silently absorbs misspelled top-level keys (e.g.
    /// `excude` instead of `exclude`). This is acceptable because unknown
    /// keys are expected to be kind-specific; a future strict mode could
    /// reject unrecognized keys if needed.
    #[serde(flatten)]
    pub extra: toml::Table,
}

// ---------------------------------------------------------------------------
// CLI overrides
// ---------------------------------------------------------------------------

/// Optional overrides supplied via CLI flags. Each field is `None` unless the
/// corresponding flag was explicitly passed.
#[derive(Debug, Default, Clone)]
pub struct ConfigOverrides {
    pub database_path: Option<String>,
    pub migrations_dir: Option<String>,
    pub backups_dir: Option<String>,
    pub auto_snapshot: Option<bool>,
    pub checksum_check: Option<bool>,
    pub schema_path: Option<String>,
}

/// Runtime context derived from the parsed CLI invocation.
///
/// Carries the explicit `--config` path and the parsed CLI overrides so that
/// individual commands can load configuration with the correct precedence
/// (CLI flags > environment variables > `stig.toml` > defaults) from a single
/// call site.
#[derive(Debug, Default, Clone)]
pub struct RunContext {
    /// Explicit config file path from `--config`.
    pub config_path: Option<PathBuf>,
    /// Parsed CLI overrides.
    pub overrides: ConfigOverrides,
}

impl RunContext {
    /// Load configuration using the process environment and then apply any
    /// CLI overrides carried by this context.
    pub fn load_config(&self) -> Result<Runtime, CliError> {
        let mut runtime = Runtime::load(self.config_path.as_deref(), &ProcessEnv, None)?;
        runtime.apply_cli_overrides(&self.overrides);
        Ok(runtime)
    }
}

// ---------------------------------------------------------------------------
// On-disk config file (TOML mirror)
// ---------------------------------------------------------------------------

/// On-disk shape of `stig.toml`.
///
/// Mirrors the serialised TOML structure exactly: no `project_root`, all
/// fields expressed as the strings/values users wrote. Use [`Runtime`] for
/// the resolved runtime configuration (which carries `project_root` and the
/// path-resolution accessors).
///
/// Kept `pub(crate)` so the on-disk format remains an implementation detail
/// of the crate — the public consumer surface is [`Runtime`].
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub(crate) struct ConfigFile {
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

    /// Path to the schema manifest file.
    #[serde(default = "default_schema_path")]
    pub schema_path: String,
}

// Default value fns used by serde and the Default impl.
fn default_database_path() -> String {
    "app.db".to_string()
}
fn default_migrations_dir() -> String {
    "db/migrations".to_string()
}
fn default_backups_dir() -> String {
    "db".to_string()
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
fn default_schema_path() -> String {
    "db/schema.sql".to_string()
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self {
            database_path: default_database_path(),
            migrations_dir: default_migrations_dir(),
            backups_dir: default_backups_dir(),
            snapshot_keep: default_snapshot_keep(),
            reset_keep: default_reset_keep(),
            auto_snapshot: default_auto_snapshot(),
            checksum_check: default_checksum_check(),
            pragmas: Pragmas::default(),
            generate: Vec::new(),
            schema_path: default_schema_path(),
        }
    }
}

// ---------------------------------------------------------------------------
// Runtime (resolved configuration)
// ---------------------------------------------------------------------------

/// Resolved configuration for a `stig` invocation.
///
/// Pairs an on-disk [`ConfigFile`] with the `project_root` the file was
/// resolved against. Path-resolution accessors ([`Self::db_path`],
/// [`Self::migrations_path`], etc.) live here so callers stop reaching for
/// `project_root` and the raw string fields directly.
///
/// Built by [`Runtime::load`] or by [`RunContext::load_config`] (which
/// additionally applies CLI overrides on top of file + env).
#[derive(Debug, Clone, PartialEq)]
pub struct Runtime {
    /// The project root directory: the parent of the `stig.toml` file when
    /// one is found, or `start_dir` when supplied to [`Runtime::load`], or
    /// the process CWD as a last resort when no config file is found and no
    /// `start_dir` is provided. All relative paths in the on-disk file
    /// (`database_path`, `migrations_dir`, `backups_dir`,
    /// `[[generate]].path`) are resolved against this directory.
    pub project_root: PathBuf,

    /// On-disk fields parsed from `stig.toml` (or defaults). Held as the
    /// single source of truth for serialisable values; in-crate callers
    /// should prefer the accessor methods over reaching into `file`
    /// directly. Kept `pub(crate)` because the on-disk shape is an
    /// implementation detail.
    pub(crate) file: ConfigFile,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            project_root: PathBuf::new(),
            file: ConfigFile::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// Loading logic
// ---------------------------------------------------------------------------

impl Runtime {
    /// Load configuration, applying the following precedence:
    ///
    /// 1. Environment variables (from `env`)
    /// 2. `stig.toml` values
    /// 3. Built-in defaults
    ///
    /// CLI flag overrides are applied separately by callers via
    /// [`Runtime::apply_cli_overrides`] (or through [`RunContext::load_config`]).
    ///
    /// # Parameters
    ///
    /// - `override_path`: explicit config file path (e.g. from `--config`).
    ///   Takes precedence over `STIG_CONFIG` when both are set.
    /// - `env`: environment-variable source. Use [`env_source::ProcessEnv`] in production
    ///   to read the real process environment; use [`env_source::MapEnv`] in tests for
    ///   full isolation (structurally cannot read `std::env`).
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
    /// contains invalid TOML or fails to deserialise into [`ConfigFile`]
    /// (reported as "invalid config").
    ///
    /// A missing config file is **not** an error — defaults are used.
    pub fn load<E: EnvSource>(
        override_path: Option<&Path>,
        env: &E,
        start_dir: Option<&Path>,
    ) -> Result<Self, CliError> {
        env.load_dotenv();

        // Resolve the config file path.
        let config_path = Self::resolve_config_path(override_path, env, start_dir);

        // Parse the file, or start from defaults if there is no file.
        let mut runtime: Runtime = match config_path {
            Some(ref path) => {
                let raw = std::fs::read_to_string(path).map_err(|e| {
                    CliError::Usage(format!("cannot read config file {}: {}", path.display(), e))
                })?;
                let file: ConfigFile = toml::from_str(&raw).map_err(|e| {
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
                let project_root = canonical_path
                    .parent()
                    .map(|p| {
                        if p == std::path::Path::new("") {
                            std::env::current_dir().unwrap_or_else(|_| p.to_path_buf())
                        } else {
                            p.to_path_buf()
                        }
                    })
                    .unwrap_or_else(|| canonical_path.clone());
                Runtime { project_root, file }
            }
            None => {
                // No config file found — use start_dir or CWD as the project root.
                let project_root = start_dir
                    .map(|d| d.to_path_buf())
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_default();
                Runtime {
                    project_root,
                    file: ConfigFile::default(),
                }
            }
        };

        // Apply environment-variable overrides.
        runtime.apply_env_overrides(env);

        Ok(runtime)
    }

    /// Apply CLI flag overrides on top of the loaded config (step 1 in the
    /// precedence chain). Called by individual commands after [`Runtime::load`].
    pub fn apply_cli_overrides(&mut self, overrides: &ConfigOverrides) {
        if let Some(v) = &overrides.database_path {
            self.file.database_path = v.clone();
        }
        if let Some(v) = &overrides.migrations_dir {
            self.file.migrations_dir = v.clone();
        }
        if let Some(v) = &overrides.backups_dir {
            self.file.backups_dir = v.clone();
        }
        if let Some(v) = overrides.auto_snapshot {
            self.file.auto_snapshot = v;
        }
        if let Some(v) = overrides.checksum_check {
            self.file.checksum_check = v;
        }
        if let Some(v) = &overrides.schema_path {
            self.file.schema_path = v.clone();
        }
    }

    /// Resolve a relative config path against `project_root`.
    ///
    /// Absolute paths and the special token `":memory:"` are returned as-is.
    /// Relative paths are joined to `project_root`.
    pub fn resolve_path(&self, path: &str) -> PathBuf {
        if path == ":memory:" || Path::new(path).is_absolute() {
            PathBuf::from(path)
        } else {
            self.project_root.join(path)
        }
    }

    // -----------------------------------------------------------------------
    // Path accessors
    //
    // Centralised so callers stop building paths from `project_root` + the
    // raw string fields. Each accessor delegates to [`Self::resolve_path`],
    // which preserves the `:memory:` token and absolute paths.
    // -----------------------------------------------------------------------

    /// Resolved path to the SQLite database.
    ///
    /// For the literal `":memory:"` this returns `PathBuf::from(":memory:")`
    /// rather than a filesystem path. Callers that need to branch on the
    /// in-memory case should use [`Self::is_memory_db`].
    pub fn db_path(&self) -> PathBuf {
        self.resolve_path(&self.file.database_path)
    }

    /// Whether [`ConfigFile::database_path`] is the literal `":memory:"` token.
    pub fn is_memory_db(&self) -> bool {
        self.file.database_path == ":memory:"
    }

    /// Resolved path to the migrations directory.
    pub fn migrations_path(&self) -> PathBuf {
        self.resolve_path(&self.file.migrations_dir)
    }

    /// Resolved path to the backups directory.
    pub fn backups_path(&self) -> PathBuf {
        self.resolve_path(&self.file.backups_dir)
    }

    /// Resolved path to the snapshots directory (`<backups>/snapshots`).
    pub fn snapshots_path(&self) -> PathBuf {
        self.backups_path().join("snapshots")
    }

    /// Resolved path to the reset-backups directory (`<backups>/resets`).
    pub fn resets_path(&self) -> PathBuf {
        self.backups_path().join("resets")
    }

    /// Resolved path to the schema manifest file.
    pub fn schema_file_path(&self) -> PathBuf {
        self.resolve_path(&self.file.schema_path)
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Resolve the path to the config file using the precedence:
    /// 1. `override_path` argument (e.g. `--config`)
    /// 2. `STIG_CONFIG` env var
    /// 3. Upward search from `start_dir` (or CWD) for `stig.toml`
    ///
    /// Returns `None` if no config file is found (not an error).
    pub(crate) fn resolve_config_path<E: EnvSource>(
        override_path: Option<&Path>,
        env: &E,
        start_dir: Option<&Path>,
    ) -> Option<PathBuf> {
        // 1. Explicit override path — CLI flags beat environment variables.
        if let Some(p) = override_path {
            return Some(p.to_path_buf());
        }

        // 2. STIG_CONFIG env var — always passed through so the caller gets a
        // clear IO error if the file doesn't exist rather than silently falling
        // back to defaults.
        if let Some(p) = env.get_var("STIG_CONFIG") {
            return Some(PathBuf::from(p));
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

    /// Apply all environment-variable overrides to `self`.
    fn apply_env_overrides<E: EnvSource>(&mut self, env: &E) {
        // STIG_DATABASE_PATH or DATABASE_PATH
        if let Some(v) = env
            .get_var("STIG_DATABASE_PATH")
            .or_else(|| env.get_var("DATABASE_PATH"))
        {
            self.file.database_path = v;
        }

        // STIG_MIGRATIONS_DIR
        if let Some(v) = env.get_var("STIG_MIGRATIONS_DIR") {
            self.file.migrations_dir = v;
        }

        // STIG_BACKUPS_DIR
        if let Some(v) = env.get_var("STIG_BACKUPS_DIR") {
            self.file.backups_dir = v;
        }

        // STIG_NO_SNAPSHOT — any non-empty value disables snapshots
        if let Some(v) = env.get_var("STIG_NO_SNAPSHOT")
            && !v.is_empty()
        {
            self.file.auto_snapshot = false;
        }

        // STIG_NO_CHECKSUM — any non-empty value disables checksum verification
        if let Some(v) = env.get_var("STIG_NO_CHECKSUM")
            && !v.is_empty()
        {
            self.file.checksum_check = false;
        }

        // STIG_SCHEMA_PATH
        if let Some(v) = env.get_var("STIG_SCHEMA_PATH") {
            self.file.schema_path = v;
        }
    }

    /// Serialize the on-disk fields of `self` to TOML and write them to `path`.
    ///
    /// Only the [`ConfigFile`] portion is serialised — `project_root` is a
    /// runtime-only value derived from the file's location and is never
    /// written.
    ///
    /// # Errors
    ///
    /// Returns [`CliError::Usage`] (exit 2) if serialization or the file write
    /// fails.
    pub fn write(&self, path: &Path) -> Result<(), CliError> {
        let contents = toml::to_string_pretty(&self.file)
            .map_err(|e| CliError::Usage(format!("failed to serialize config: {e}")))?;
        std::fs::write(path, contents)
            .map_err(|e| CliError::Usage(format!("failed to write {}: {e}", path.display())))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use env_source::MapEnv;
    use indoc::indoc;
    use std::collections::HashMap;
    use std::io::Write;
    use tempfile::{NamedTempFile, TempDir};

    fn empty_env() -> MapEnv {
        MapEnv(HashMap::new())
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
        let cfg = Runtime::load(None, &empty_env(), Some(dir.path())).unwrap();
        // Runtime values should all match the on-disk defaults.
        let defaults = ConfigFile::default();
        assert_eq!(cfg.file.database_path, defaults.database_path);
        assert_eq!(cfg.file.migrations_dir, defaults.migrations_dir);
        assert_eq!(cfg.file.snapshot_keep, defaults.snapshot_keep);
        assert_eq!(cfg.file.pragmas, defaults.pragmas);
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
        let result = Runtime::load(Some(&absent), &empty_env(), None);
        assert!(matches!(result, Err(CliError::Usage(_))));
    }

    #[test]
    fn empty_config_file_returns_defaults() {
        let f = temp_toml("");
        let cfg = Runtime::load(Some(f.path()), &empty_env(), None).unwrap();
        let defaults = ConfigFile::default();
        assert_eq!(cfg.file.database_path, defaults.database_path);
        assert_eq!(cfg.file.migrations_dir, defaults.migrations_dir);
        assert_eq!(cfg.file.snapshot_keep, defaults.snapshot_keep);
        assert_eq!(cfg.file.pragmas, defaults.pragmas);
        assert_eq!(cfg.file.generate, defaults.generate);
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
        let cfg = Runtime::load(Some(f.path()), &empty_env(), None).unwrap();
        assert_eq!(cfg.file.database_path, "my.db");
        assert_eq!(cfg.file.migrations_dir, "db/migrations"); // default
        assert_eq!(cfg.file.snapshot_keep, 5); // default
        assert_eq!(cfg.file.pragmas, Pragmas::default());
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
            exclude = ["sqlite_%"]
        "#});

        let cfg = Runtime::load(Some(f.path()), &empty_env(), None).unwrap();
        assert_eq!(cfg.file.database_path, "prod.db");
        assert_eq!(cfg.file.migrations_dir, "migrations");
        assert_eq!(cfg.file.backups_dir, "bk");
        assert_eq!(cfg.file.snapshot_keep, 10);
        assert_eq!(cfg.file.reset_keep, 2);
        assert!(!cfg.file.auto_snapshot);
        assert!(!cfg.file.checksum_check);
        assert_eq!(cfg.file.pragmas.journal_mode, "DELETE");
        assert_eq!(cfg.file.pragmas.foreign_keys, "OFF");
        assert_eq!(cfg.file.generate.len(), 1);
        assert_eq!(cfg.file.generate[0].kind, "typescript");
        assert_eq!(cfg.file.generate[0].path, "types.ts");
        assert_eq!(cfg.file.generate[0].name, None);
        assert_eq!(cfg.file.generate[0].exclude, vec!["sqlite_%".to_string()]);
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

        let env = MapEnv([("STIG_DATABASE_PATH".into(), "env.db".into())].into());

        let cfg = Runtime::load(Some(f.path()), &env, None).unwrap();
        assert_eq!(cfg.file.database_path, "env.db");
    }

    #[test]
    fn env_database_path_fallback_overrides_file() {
        let f = temp_toml(indoc! {r#"
            database_path = "file.db"
        "#});

        let env = MapEnv([("DATABASE_PATH".into(), "fallback.db".into())].into());

        let cfg = Runtime::load(Some(f.path()), &env, None).unwrap();
        assert_eq!(cfg.file.database_path, "fallback.db");
    }

    #[test]
    fn env_stig_database_path_takes_priority_over_database_path() {
        let f = temp_toml("");

        let env = MapEnv(
            [
                ("STIG_DATABASE_PATH".into(), "stig.db".into()),
                ("DATABASE_PATH".into(), "other.db".into()),
            ]
            .into(),
        );

        let cfg = Runtime::load(Some(f.path()), &env, None).unwrap();
        assert_eq!(cfg.file.database_path, "stig.db");
    }

    #[test]
    fn env_stig_migrations_dir_overrides_file() {
        let f = temp_toml("");

        let env = MapEnv([("STIG_MIGRATIONS_DIR".into(), "custom/migrations".into())].into());

        let cfg = Runtime::load(Some(f.path()), &env, None).unwrap();
        assert_eq!(cfg.file.migrations_dir, "custom/migrations");
    }

    #[test]
    fn env_stig_backups_dir_overrides_file() {
        let f = temp_toml("");

        let env = MapEnv([("STIG_BACKUPS_DIR".into(), "custom/backups".into())].into());

        let cfg = Runtime::load(Some(f.path()), &env, None).unwrap();
        assert_eq!(cfg.file.backups_dir, "custom/backups");
    }

    #[test]
    fn env_stig_no_snapshot_disables_auto_snapshot() {
        let f = temp_toml("auto_snapshot = true");

        let env = MapEnv([("STIG_NO_SNAPSHOT".into(), "1".into())].into());

        let cfg = Runtime::load(Some(f.path()), &env, None).unwrap();
        assert!(!cfg.file.auto_snapshot);
    }

    #[test]
    fn env_stig_no_snapshot_empty_string_does_not_disable() {
        let f = temp_toml("auto_snapshot = true");

        let env = MapEnv([("STIG_NO_SNAPSHOT".into(), "".into())].into());

        let cfg = Runtime::load(Some(f.path()), &env, None).unwrap();
        assert!(cfg.file.auto_snapshot);
    }

    #[test]
    fn env_stig_no_checksum_disables_checksum_check() {
        let f = temp_toml("checksum_check = true");

        let env = MapEnv([("STIG_NO_CHECKSUM".into(), "true".into())].into());

        let cfg = Runtime::load(Some(f.path()), &env, None).unwrap();
        assert!(!cfg.file.checksum_check);
    }

    #[test]
    fn env_stig_schema_path_overrides_file() {
        let f = temp_toml("");

        let env = MapEnv([("STIG_SCHEMA_PATH".into(), "custom/schema.sql".into())].into());

        let cfg = Runtime::load(Some(f.path()), &env, None).unwrap();
        assert_eq!(cfg.file.schema_path, "custom/schema.sql");
    }

    // -----------------------------------------------------------------------
    // 5. CLI overrides
    // -----------------------------------------------------------------------

    #[test]
    fn cli_overrides_applied_on_top_of_env_and_file() {
        let f = temp_toml(indoc! {r#"
            database_path = "file.db"
        "#});

        let env = MapEnv([("STIG_DATABASE_PATH".into(), "env.db".into())].into());

        let mut cfg = Runtime::load(Some(f.path()), &env, None).unwrap();
        assert_eq!(cfg.file.database_path, "env.db"); // env beat file

        let overrides = ConfigOverrides {
            database_path: Some("cli.db".into()),
            ..Default::default()
        };
        cfg.apply_cli_overrides(&overrides);
        assert_eq!(cfg.file.database_path, "cli.db"); // CLI beats env
    }

    #[test]
    fn cli_overrides_partial_leaves_other_fields_unchanged() {
        let f = temp_toml("");

        let mut cfg = Runtime::load(Some(f.path()), &empty_env(), None).unwrap();
        let original_migrations_dir = cfg.file.migrations_dir.clone();

        let overrides = ConfigOverrides {
            database_path: Some("cli.db".into()),
            ..Default::default()
        };
        cfg.apply_cli_overrides(&overrides);
        assert_eq!(cfg.file.database_path, "cli.db");
        assert_eq!(cfg.file.migrations_dir, original_migrations_dir); // untouched
    }

    #[test]
    fn cli_override_auto_snapshot_false() {
        let f = temp_toml("auto_snapshot = true");

        let mut cfg = Runtime::load(Some(f.path()), &empty_env(), None).unwrap();
        assert!(cfg.file.auto_snapshot);

        let overrides = ConfigOverrides {
            auto_snapshot: Some(false),
            ..Default::default()
        };
        cfg.apply_cli_overrides(&overrides);
        assert!(!cfg.file.auto_snapshot);
    }

    #[test]
    fn cli_override_schema_path() {
        let f = temp_toml("");

        let mut cfg = Runtime::load(Some(f.path()), &empty_env(), None).unwrap();
        assert_eq!(cfg.file.schema_path, "db/schema.sql");

        let overrides = ConfigOverrides {
            schema_path: Some("custom/schema.sql".into()),
            ..Default::default()
        };
        cfg.apply_cli_overrides(&overrides);
        assert_eq!(cfg.file.schema_path, "custom/schema.sql");
    }

    // -----------------------------------------------------------------------
    // 6. Invalid TOML — should return CliError::Usage (exit 2)
    // -----------------------------------------------------------------------

    #[test]
    fn invalid_toml_returns_usage_error() {
        let f = temp_toml("this is not : valid = toml [[[");

        let result = Runtime::load(Some(f.path()), &empty_env(), None);
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

        let result = Runtime::load(Some(f.path()), &empty_env(), None);
        assert!(matches!(result, Err(CliError::Usage(_))));
    }

    // -----------------------------------------------------------------------
    // 7. STIG_CONFIG env var respected
    // -----------------------------------------------------------------------

    #[test]
    fn override_path_takes_priority_over_stig_config_env() {
        // File A: via STIG_CONFIG
        let fa = temp_toml(r#"database_path = "from_stig_config.db""#);

        // File B: via override_path argument (should win)
        let fb = temp_toml(r#"database_path = "from_override.db""#);

        let env = MapEnv(
            [(
                "STIG_CONFIG".into(),
                fa.path().to_str().unwrap().to_string(),
            )]
            .into(),
        );

        let cfg = Runtime::load(Some(fb.path()), &env, None).unwrap();
        assert_eq!(cfg.file.database_path, "from_override.db");
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

        let env = MapEnv(
            [(
                "STIG_CONFIG".into(),
                config_file.to_str().unwrap().to_string(),
            )]
            .into(),
        );

        let cfg = Runtime::load(None, &env, None).unwrap();
        assert_eq!(cfg.file.database_path, "app.db");
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
        let cfg = Runtime::load(Some(&config_file), &empty_env(), Some(start_dir.path())).unwrap();

        assert_eq!(cfg.project_root, config_dir.path().canonicalize().unwrap());
        assert_ne!(cfg.project_root, start_dir.path().canonicalize().unwrap());
    }

    // -----------------------------------------------------------------------
    // 9. Hermetic env injection
    // -----------------------------------------------------------------------

    /// When a [`MapEnv`] is injected, `load()` consults only the injected map.
    /// Because [`MapEnv`] is structurally incapable of calling `std::env::var`,
    /// the hermetic contract is enforced at compile time — no runtime assertion
    /// against the real process environment is needed.
    #[test]
    fn injected_env_map_is_used_exclusively() {
        let env = empty_env();
        let f = temp_toml("");
        let cfg = Runtime::load(Some(f.path()), &env, None).unwrap();

        // With an empty injected map, no env overrides should apply.
        assert_eq!(cfg.file.database_path, ConfigFile::default().database_path);
    }

    // -----------------------------------------------------------------------
    // Runtime::write round-trip tests
    // -----------------------------------------------------------------------

    #[test]
    fn write_round_trips_default_config() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("stig.toml");

        let original = Runtime {
            project_root: dir.path().to_path_buf(),
            file: ConfigFile::default(),
        };
        original.write(&path).unwrap();

        let loaded = Runtime::load(Some(&path), &empty_env(), None).unwrap();
        assert_eq!(loaded.file.database_path, original.file.database_path);
        assert_eq!(loaded.file.migrations_dir, original.file.migrations_dir);
        assert_eq!(loaded.file.backups_dir, original.file.backups_dir);
        assert_eq!(loaded.file.snapshot_keep, original.file.snapshot_keep);
        assert_eq!(loaded.file.reset_keep, original.file.reset_keep);
        assert_eq!(loaded.file.auto_snapshot, original.file.auto_snapshot);
        assert_eq!(loaded.file.checksum_check, original.file.checksum_check);
        assert_eq!(loaded.file.pragmas, original.file.pragmas);
        assert_eq!(loaded.file.generate, original.file.generate);
        assert_eq!(loaded.file.schema_path, original.file.schema_path);
    }

    #[test]
    fn write_round_trips_non_default_values() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("stig.toml");

        let original = Runtime {
            project_root: dir.path().to_path_buf(),
            file: ConfigFile {
                database_path: "prod.db".to_string(),
                migrations_dir: "schema/migrations".to_string(),
                snapshot_keep: 10,
                auto_snapshot: false,
                pragmas: Pragmas {
                    journal_mode: "DELETE".to_string(),
                    foreign_keys: "ON".to_string(),
                },
                ..ConfigFile::default()
            },
        };
        original.write(&path).unwrap();

        let loaded = Runtime::load(Some(&path), &empty_env(), None).unwrap();
        assert_eq!(loaded.file.database_path, "prod.db");
        assert_eq!(loaded.file.migrations_dir, "schema/migrations");
        assert_eq!(loaded.file.snapshot_keep, 10);
        assert!(!loaded.file.auto_snapshot);
        assert_eq!(loaded.file.pragmas.journal_mode, "DELETE");
    }

    #[test]
    fn write_does_not_include_project_root() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("stig.toml");

        let cfg = Runtime {
            project_root: dir.path().to_path_buf(),
            file: ConfigFile::default(),
        };
        cfg.write(&path).unwrap();

        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(
            !contents.contains("project_root"),
            "project_root must not appear in written TOML"
        );
    }

    #[test]
    fn write_round_trips_generate_target_extra_fields() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("stig.toml");

        let mut extra = toml::Table::new();
        extra.insert("indent".to_string(), toml::Value::Integer(4));
        extra.insert(
            "header".to_string(),
            toml::Value::String("// auto-generated".to_string()),
        );

        let original = Runtime {
            project_root: dir.path().to_path_buf(),
            file: ConfigFile {
                generate: vec![GenerateTarget {
                    kind: "typescript".to_string(),
                    path: "types.ts".to_string(),
                    name: Some("my-types".to_string()),
                    exclude: vec!["sqlite_%".to_string()],
                    extra,
                }],
                ..ConfigFile::default()
            },
        };
        original.write(&path).unwrap();

        let loaded = Runtime::load(Some(&path), &empty_env(), None).unwrap();
        assert_eq!(loaded.file.generate.len(), 1);
        assert_eq!(
            loaded.file.generate[0].extra.get("indent"),
            Some(&toml::Value::Integer(4))
        );
        assert_eq!(
            loaded.file.generate[0].extra.get("header"),
            Some(&toml::Value::String("// auto-generated".to_string()))
        );
    }

    // -----------------------------------------------------------------------
    // Path accessors
    // -----------------------------------------------------------------------

    #[test]
    fn path_accessors_resolve_relative_paths_against_project_root() {
        let dir = TempDir::new().unwrap();
        let cfg = Runtime {
            project_root: dir.path().to_path_buf(),
            file: ConfigFile::default(),
        };

        assert_eq!(cfg.db_path(), dir.path().join("app.db"));
        assert_eq!(cfg.migrations_path(), dir.path().join("db/migrations"));
        assert_eq!(cfg.backups_path(), dir.path().join("db"));
        assert_eq!(
            cfg.snapshots_path(),
            dir.path().join("db").join("snapshots")
        );
        assert_eq!(cfg.resets_path(), dir.path().join("db").join("resets"));
        assert_eq!(cfg.schema_file_path(), dir.path().join("db/schema.sql"));
        assert!(!cfg.is_memory_db());
    }

    #[test]
    fn db_path_preserves_memory_token_and_is_memory_db_detects_it() {
        let dir = TempDir::new().unwrap();
        let cfg = Runtime {
            project_root: dir.path().to_path_buf(),
            file: ConfigFile {
                database_path: ":memory:".to_string(),
                ..ConfigFile::default()
            },
        };

        assert_eq!(cfg.db_path(), PathBuf::from(":memory:"));
        assert!(cfg.is_memory_db());
    }

    #[test]
    fn path_accessors_preserve_absolute_paths() {
        let dir = TempDir::new().unwrap();
        let cfg = Runtime {
            project_root: dir.path().to_path_buf(),
            file: ConfigFile {
                database_path: "/var/lib/app/data.db".to_string(),
                migrations_dir: "/etc/app/migrations".to_string(),
                backups_dir: "/srv/backups".to_string(),
                schema_path: "/etc/app/schema.sql".to_string(),
                ..ConfigFile::default()
            },
        };

        assert_eq!(cfg.db_path(), PathBuf::from("/var/lib/app/data.db"));
        assert_eq!(cfg.migrations_path(), PathBuf::from("/etc/app/migrations"));
        assert_eq!(cfg.backups_path(), PathBuf::from("/srv/backups"));
        assert_eq!(
            cfg.snapshots_path(),
            PathBuf::from("/srv/backups/snapshots")
        );
        assert_eq!(cfg.resets_path(), PathBuf::from("/srv/backups/resets"));
        assert_eq!(cfg.schema_file_path(), PathBuf::from("/etc/app/schema.sql"));
        assert!(!cfg.is_memory_db());
    }
}
