//! SQLite connection layer for `stig`.
//!
//! Opens a [`rusqlite::Connection`] at the path specified in [`Config`],
//! applies PRAGMAs from config, and exposes [`Db::checkpoint`] and
//! [`Db::close`] helpers used by snapshot and reset operations.
//!
//! # `:memory:` support
//!
//! Passing `":memory:"` as `database_path` opens an in-memory database.
//! **Only the exact string `":memory:"` is recognised as in-memory** — URI
//! forms such as `"file::memory:?cache=shared"` are treated as regular file
//! paths and will be opened as files.  If you need URI in-memory databases,
//! open the connection manually and apply PRAGMAs yourself.
//!
//! When running in-memory mode, `PRAGMA journal_mode = WAL` is intentionally
//! skipped (WAL is incompatible with in-memory databases) and a warning is
//! emitted.  Snapshot and reset operations are also disabled in this mode.

use anyhow::{Context, Result};
use rusqlite::Connection;
use tracing::warn;

use crate::config::Config;

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
    ///   warning that snapshot/reset features are disabled.  `foreign_keys` is
    ///   still applied.
    pub fn open(config: &Config) -> Result<Self> {
        let path = &config.database_path;
        let is_memory = path == ":memory:";

        let conn = if is_memory {
            warn!("database_path is \":memory:\": snapshots and resets are disabled");
            Connection::open_in_memory().context("failed to open in-memory SQLite database")?
        } else {
            Connection::open(path)
                .with_context(|| format!("failed to open SQLite database at {path:?}"))?
        };

        let db = Self { conn, is_memory };

        db.apply_pragmas(&config.pragmas)?;

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
        self.conn.close().map_err(|(_, e)| e).context("failed to close SQLite connection")
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    fn apply_pragmas(&self, pragmas: &crate::config::Pragmas) -> Result<()> {
        // journal_mode is skipped for :memory: (WAL is incompatible)
        if !self.is_memory {
            let requested = pragmas.journal_mode.to_uppercase();
            let actual: String = self
                .conn
                .query_row(
                    &format!("PRAGMA journal_mode = {}", pragmas.journal_mode),
                    [],
                    |row| row.get(0),
                )
                .context("failed to set PRAGMA journal_mode")?;
            if actual.to_uppercase() != requested {
                warn!(
                    requested = %requested,
                    actual = %actual,
                    "PRAGMA journal_mode did not settle to the requested value; \
                     WAL may be unsupported on this filesystem"
                );
            }
        }

        // foreign_keys applies to all connection types
        self.conn
            .execute_batch(&format!("PRAGMA foreign_keys = {};", pragmas.foreign_keys))
            .context("failed to set PRAGMA foreign_keys")?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::*;
    use crate::config::Config;

    fn file_config(path: &str) -> Config {
        Config { database_path: path.to_string(), ..Config::default() }
    }

    fn memory_config() -> Config {
        Config { database_path: ":memory:".to_string(), ..Config::default() }
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

        let fk: i32 =
            db.conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn open_file_db_respects_configured_journal_mode() {
        let tmp = NamedTempFile::new().unwrap();
        let mut cfg = file_config(tmp.path().to_str().unwrap());
        cfg.pragmas.journal_mode = "DELETE".to_string();
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
        assert_ne!(mode.to_uppercase(), "WAL", "WAL should not be set for :memory:");
    }

    #[test]
    fn open_memory_applies_foreign_keys() {
        let db = Db::open(&memory_config()).expect("open failed");

        let fk: i32 =
            db.conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0)).unwrap();
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
        db.checkpoint().expect("checkpoint should succeed for :memory:");
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
        let n: i32 = db.connection().query_row("SELECT 1", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }
}
