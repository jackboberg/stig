//! SQLite connection layer for `stig`.
//!
//! Opens a [`rusqlite::Connection`] at the path specified in [`Runtime`],
//! applies PRAGMAs from config, and exposes [`Db::checkpoint`] and
//! [`Db::close`] helpers used by snapshot and reset operations.
//!
//! # `:memory:` support
//!
//! Passing `":memory:"` as `database_path` opens an in-memory database.
//! **Only the exact string `":memory:"` is recognised as in-memory.** All
//! other strings — including URI forms such as `"file::memory:?cache=shared"`
//! — are passed directly to [`rusqlite::Connection::open`] as filesystem
//! paths.  URI mode is not enabled, so such strings are treated as literal
//! path names and will likely produce an error.
//!
//! When running in-memory mode, `PRAGMA journal_mode = WAL` is intentionally
//! skipped (WAL is incompatible with in-memory databases) and a warning is
//! emitted.  Snapshot and reset operations are not supported in this mode;
//! higher-level commands are responsible for checking [`Db::is_memory`] and
//! declining to proceed.

use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use rusqlite::params;
use tracing::warn;

use crate::config::Runtime;
use crate::migrate::plan::PlannedMigration;
use crate::snapshot;

// ---------------------------------------------------------------------------
// Public struct
// ---------------------------------------------------------------------------

/// A managed SQLite connection with PRAGMAs applied.
pub struct Db {
    conn: Connection,
    is_memory: bool,
}

impl Db {
    /// Open a connection to the database described by `config`.
    ///
    /// - For file databases: applies `PRAGMA journal_mode` and
    ///   `PRAGMA foreign_keys` from config.  Warns if `journal_mode` does not
    ///   settle to the requested value (e.g. WAL is unsupported on the
    ///   underlying filesystem).
    /// - For `:memory:`: skips `journal_mode` (WAL-incompatible) and emits a
    ///   warning that snapshot/reset operations are not supported in this mode.  `foreign_keys` is
    ///   still applied.
    pub fn open(config: &Runtime) -> Result<Self> {
        let is_memory = config.is_memory_db();
        let resolved = config.db_path();

        let conn = if is_memory {
            warn!(
                "database_path is \":memory:\": snapshots and resets are not supported in this mode"
            );
            Connection::open_in_memory().context("failed to open in-memory SQLite database")?
        } else {
            Connection::open(&resolved)
                .with_context(|| format!("failed to open SQLite database at {:?}", resolved))?
        };

        let db = Self { conn, is_memory };

        db.apply_pragmas(&config.file.pragmas)?;

        Ok(db)
    }

    /// Return an immutable reference to the underlying [`Connection`].
    ///
    /// Used by other modules (migrations, codegen) that need direct access to
    /// execute queries.
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Return whether this connection is to an in-memory database.
    pub fn is_memory(&self) -> bool {
        self.is_memory
    }

    /// Run a WAL checkpoint (`PRAGMA wal_checkpoint(TRUNCATE)`).
    ///
    /// Should be called before taking a snapshot to ensure the checkpoint file
    /// is flushed into the main database file.  Is a no-op (returns `Ok`) on
    /// in-memory databases.
    pub fn checkpoint(&self) -> Result<()> {
        if self.is_memory {
            return Ok(());
        }
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
            .context("WAL checkpoint failed")
    }

    /// Close the database connection, consuming `self`.
    ///
    /// [`rusqlite::Connection::close`] returns `Err((conn, e))` on failure;
    /// we discard the re-returned connection and surface just the error.
    pub fn close(self) -> Result<()> {
        self.conn
            .close()
            .map_err(|(_, e)| e)
            .context("failed to close SQLite connection")
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn apply_pragmas(&self, pragmas: &crate::config::Pragmas) -> Result<()> {
        // journal_mode is skipped for :memory: (WAL is incompatible)
        if !self.is_memory {
            let requested = validate_journal_mode(&pragmas.journal_mode)?;
            let actual: String = self
                .conn
                .query_row(&format!("PRAGMA journal_mode = {requested}"), [], |row| {
                    row.get(0)
                })
                .context("failed to set PRAGMA journal_mode")?;
            if actual.to_uppercase() != requested {
                warn!(
                    requested = %requested,
                    actual = %actual,
                    "PRAGMA journal_mode did not settle to the requested value; \
                     the mode may be unsupported in this environment"
                );
            }
        }

        // foreign_keys applies to all connection types
        let fk = validate_foreign_keys(&pragmas.foreign_keys)?;
        self.conn
            .execute_batch(&format!("PRAGMA foreign_keys = {fk};"))
            .context("failed to set PRAGMA foreign_keys")?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Schema helpers
// ---------------------------------------------------------------------------

/// Ensure the `schema_migrations` table exists (created by `init`, but
/// other commands must also handle a DB that was created externally).
pub fn ensure_schema_migrations(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version    TEXT NOT NULL PRIMARY KEY,
            checksum   TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )
    .context("failed to ensure schema_migrations table")?;
    Ok(())
}

/// Delete rows from `schema_migrations` where `version >= from_version`.
///
/// Returns the number of rows deleted. Used by `redo` to clear stale entries
/// after restoring a snapshot.
pub fn delete_from_version(conn: &Connection, from_version: &str) -> Result<usize> {
    let n = conn
        .execute(
            "DELETE FROM schema_migrations WHERE version >= ?1",
            params![from_version],
        )
        .context("failed to delete schema_migrations rows")?;
    Ok(n)
}

/// Format a user-facing drift error message for the given entries.
///
/// Shared by `status` and `migrate` commands to keep messages consistent.
pub fn format_drift_messages(entries: &[&PlannedMigration], snapshots_dir: &Path) -> String {
    let mut msg = String::new();
    for entry in entries {
        let version = &entry.version;
        let available = snapshot::snapshot_exists(version, snapshots_dir);
        if available {
            msg.push_str(&format!(
                "migration {version} has been edited since it was applied\n\
                 snapshot pre-{version}.db is available\n\
                 \u{2192} run: stig redo {version}\n"
            ));
        } else {
            msg.push_str(&format!(
                "migration {version} has been edited since it was applied\n\
                 snapshot pre-{version}.db has been pruned\n\
                 \u{2192} revert the edit or run: stig reset\n"
            ));
        }
    }
    msg.trim().to_string()
}

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

/// Validate and normalise a `journal_mode` value.
///
/// Accepts any casing of the six modes SQLite recognises: DELETE, TRUNCATE,
/// PERSIST, MEMORY, WAL, OFF.  Returns the upper-cased token so it can be
/// safely interpolated into a PRAGMA statement without risk of injection.
fn validate_journal_mode(value: &str) -> Result<String> {
    const ALLOWED: &[&str] = &["DELETE", "TRUNCATE", "PERSIST", "MEMORY", "WAL", "OFF"];
    let upper = value.trim().to_uppercase();
    if ALLOWED.contains(&upper.as_str()) {
        Ok(upper)
    } else {
        anyhow::bail!(
            "invalid journal_mode {:?}; must be one of: {}",
            value,
            ALLOWED.join(", ")
        )
    }
}

/// Validate and normalise a `foreign_keys` value.
///
/// Accepts ON/OFF (any case) and the numeric equivalents 1/0.  Returns the
/// upper-cased token so it can be safely interpolated into a PRAGMA statement.
fn validate_foreign_keys(value: &str) -> Result<String> {
    match value.trim().to_uppercase().as_str() {
        "ON" | "1" => Ok("ON".to_string()),
        "OFF" | "0" => Ok("OFF".to_string()),
        _ => anyhow::bail!("invalid foreign_keys {:?}; must be ON, OFF, 1, or 0", value),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::*;
    use crate::config::{ConfigFile, Runtime};

    fn file_config(path: &str) -> Runtime {
        Runtime {
            project_root: std::path::PathBuf::new(),
            file: ConfigFile {
                database_path: path.to_string(),
                ..ConfigFile::default()
            },
        }
    }

    fn memory_config() -> Runtime {
        Runtime {
            project_root: std::path::PathBuf::new(),
            file: ConfigFile {
                database_path: ":memory:".to_string(),
                ..ConfigFile::default()
            },
        }
    }

    // -- file DB -------------------------------------------------------------

    #[test]
    fn open_file_db_applies_wal() {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = file_config(tmp.path().to_str().unwrap());
        let db = Db::open(&cfg).expect("open failed");

        let mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_uppercase(), "WAL");
    }

    #[test]
    fn open_file_db_applies_foreign_keys() {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = file_config(tmp.path().to_str().unwrap());
        let db = Db::open(&cfg).expect("open failed");

        let fk: i32 = db
            .conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn open_file_db_respects_configured_journal_mode() {
        let tmp = NamedTempFile::new().unwrap();
        let mut cfg = file_config(tmp.path().to_str().unwrap());
        cfg.file.pragmas.journal_mode = "DELETE".to_string();
        let db = Db::open(&cfg).expect("open failed");

        let mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode.to_uppercase(), "DELETE");
    }

    // -- :memory: DB ---------------------------------------------------------

    #[test]
    fn open_memory_skips_wal() {
        let db = Db::open(&memory_config()).expect("open failed");

        let mode: String = db
            .conn
            .query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .unwrap();
        assert_ne!(
            mode.to_uppercase(),
            "WAL",
            "WAL should not be set for :memory:"
        );
    }

    #[test]
    fn open_memory_applies_foreign_keys() {
        let db = Db::open(&memory_config()).expect("open failed");

        let fk: i32 = db
            .conn
            .query_row("PRAGMA foreign_keys", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn memory_is_memory_flag() {
        let db = Db::open(&memory_config()).unwrap();
        assert!(db.is_memory());

        let tmp = NamedTempFile::new().unwrap();
        let db = Db::open(&file_config(tmp.path().to_str().unwrap())).unwrap();
        assert!(!db.is_memory());
    }

    // -- helpers -------------------------------------------------------------

    #[test]
    fn checkpoint_succeeds_on_file_db() {
        let tmp = NamedTempFile::new().unwrap();
        let db = Db::open(&file_config(tmp.path().to_str().unwrap())).unwrap();
        db.checkpoint().expect("checkpoint failed");
    }

    #[test]
    fn checkpoint_is_noop_on_memory_db() {
        let db = Db::open(&memory_config()).unwrap();
        db.checkpoint()
            .expect("checkpoint should succeed for :memory:");
    }

    #[test]
    fn close_succeeds() {
        let tmp = NamedTempFile::new().unwrap();
        let db = Db::open(&file_config(tmp.path().to_str().unwrap())).unwrap();
        db.close().expect("close failed");
    }

    #[test]
    fn connection_accessor_returns_conn() {
        let db = Db::open(&memory_config()).unwrap();
        // sanity: can execute a query through the accessor
        let n: i32 = db
            .connection()
            .query_row("SELECT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    // -- validation helpers --------------------------------------------------

    #[test]
    fn validate_journal_mode_accepts_all_valid_modes() {
        for mode in &[
            "WAL", "wal", "DELETE", "TRUNCATE", "PERSIST", "MEMORY", "OFF",
        ] {
            assert!(
                validate_journal_mode(mode).is_ok(),
                "expected {mode:?} to be valid"
            );
        }
    }

    #[test]
    fn validate_journal_mode_normalises_to_uppercase() {
        assert_eq!(validate_journal_mode("wal").unwrap(), "WAL");
        assert_eq!(validate_journal_mode("delete").unwrap(), "DELETE");
    }

    #[test]
    fn validate_journal_mode_rejects_invalid() {
        assert!(validate_journal_mode("WAL; DROP TABLE foo").is_err());
        assert!(validate_journal_mode("").is_err());
        assert!(validate_journal_mode("UNKNOWN").is_err());
    }

    #[test]
    fn validate_foreign_keys_accepts_valid_values() {
        assert_eq!(validate_foreign_keys("ON").unwrap(), "ON");
        assert_eq!(validate_foreign_keys("on").unwrap(), "ON");
        assert_eq!(validate_foreign_keys("1").unwrap(), "ON");
        assert_eq!(validate_foreign_keys("OFF").unwrap(), "OFF");
        assert_eq!(validate_foreign_keys("off").unwrap(), "OFF");
        assert_eq!(validate_foreign_keys("0").unwrap(), "OFF");
    }

    #[test]
    fn validate_foreign_keys_rejects_invalid() {
        assert!(validate_foreign_keys("ON; DROP TABLE foo").is_err());
        assert!(validate_foreign_keys("").is_err());
        assert!(validate_foreign_keys("TRUE").is_err());
    }

    #[test]
    fn open_rejects_invalid_journal_mode() {
        let tmp = NamedTempFile::new().unwrap();
        let mut cfg = file_config(tmp.path().to_str().unwrap());
        cfg.file.pragmas.journal_mode = "INVALID; --".to_string();
        assert!(Db::open(&cfg).is_err());
    }

    #[test]
    fn open_rejects_invalid_foreign_keys() {
        let tmp = NamedTempFile::new().unwrap();
        let mut cfg = file_config(tmp.path().to_str().unwrap());
        cfg.file.pragmas.foreign_keys = "ON; DROP TABLE x".to_string();
        assert!(Db::open(&cfg).is_err());
    }

    // -- path resolution -----------------------------------------------------

    #[test]
    fn open_resolves_relative_path_against_project_root() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Runtime {
            project_root: dir.path().to_path_buf(),
            file: ConfigFile {
                database_path: "relative.db".to_string(),
                ..ConfigFile::default()
            },
        };
        let db = Db::open(&cfg).expect("should open relative path against project_root");
        // Verify the file was created inside project_root, not CWD.
        assert!(dir.path().join("relative.db").exists());
        drop(db);
    }

    #[test]
    fn open_uses_absolute_path_as_is() {
        let tmp = NamedTempFile::new().unwrap();
        let cfg = Runtime {
            // project_root is irrelevant when path is absolute
            project_root: std::path::PathBuf::from("/nonexistent"),
            file: ConfigFile {
                database_path: tmp.path().to_str().unwrap().to_string(),
                ..ConfigFile::default()
            },
        };
        assert!(Db::open(&cfg).is_ok());
    }

    // -- delete_from_version -------------------------------------------------

    #[test]
    fn delete_from_version_removes_matching_and_newer_rows() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version    TEXT NOT NULL PRIMARY KEY,
                checksum   TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();

        conn.execute(
            "INSERT INTO schema_migrations (version, checksum) VALUES (?1, ?2)",
            params!["20240101000000_a", "a"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, checksum) VALUES (?1, ?2)",
            params!["20240102000000_b", "b"],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO schema_migrations (version, checksum) VALUES (?1, ?2)",
            params!["20240103000000_c", "c"],
        )
        .unwrap();

        let n = delete_from_version(&conn, "20240102000000_b").unwrap();
        assert_eq!(n, 2);

        let remaining: Vec<String> = conn
            .prepare("SELECT version FROM schema_migrations ORDER BY version")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(remaining, vec!["20240101000000_a"]);
    }

    #[test]
    fn delete_from_version_returns_zero_when_no_match() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version    TEXT NOT NULL PRIMARY KEY,
                checksum   TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();

        conn.execute(
            "INSERT INTO schema_migrations (version, checksum) VALUES (?1, ?2)",
            params!["20240101000000_a", "a"],
        )
        .unwrap();

        let n = delete_from_version(&conn, "20240102000000_b").unwrap();
        assert_eq!(n, 0);
    }
}
