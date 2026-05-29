//! Migration planner: diff applied (from `schema_migrations`) vs on-disk files
//! and detect checksum drift.
//!
//! The planner does **not** apply or modify anything — it only reads.

use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::Connection;
use tracing::warn;

use crate::sha256_hex;

use super::discover::MigrationFile;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The status of a single migration version as determined by the planner.
#[derive(Debug, Clone, PartialEq)]
pub enum MigrationStatus {
    /// The migration file exists on disk but has no row in `schema_migrations`.
    Pending,

    /// The migration has been applied.
    ///
    /// `checksum` is the value stored in `schema_migrations`.
    /// `drifted` is `true` when the file's current SHA-256 no longer matches
    /// the stored checksum, indicating the file was edited after it was
    /// applied.
    Applied { checksum: String, drifted: bool },

    /// A row exists in `schema_migrations` but no corresponding file was found
    /// in the migrations directory.  This is a warning, not a fatal error.
    ///
    /// `checksum` is the value stored in `schema_migrations`.
    OrphanApplied { checksum: String },
}

/// A single entry in the migration plan, combining a (possibly absent) file
/// with its computed status.
#[derive(Debug, Clone)]
pub struct PlannedMigration {
    /// The version string, e.g. `"20240528123045_create_users"`.
    pub version: String,
    /// The corresponding on-disk file.  `None` for [`MigrationStatus::OrphanApplied`].
    pub file: Option<MigrationFile>,
    /// The computed status.
    pub status: MigrationStatus,
}

/// The full migration plan produced by [`Plan::build`].
#[derive(Debug)]
pub struct Plan {
    /// All known migrations, sorted by version (lexicographic).
    /// On-disk files appear first in their sorted order; orphan DB rows are
    /// appended at the end, also sorted.
    pub entries: Vec<PlannedMigration>,
}

impl Plan {
    /// Build the migration plan from the discovered files and the current DB
    /// state.
    ///
    /// # Errors
    ///
    /// Returns an error if the `schema_migrations` query fails or if a
    /// migration file cannot be read from disk.
    pub fn build(files: &[MigrationFile], conn: &Connection) -> Result<Self> {
        // Load all applied rows from the DB into a map: version -> checksum.
        let applied = query_applied(conn)?;

        // Track which DB versions we've matched to a file so we can find orphans.
        let mut matched: std::collections::HashSet<String> = std::collections::HashSet::new();

        let mut entries: Vec<PlannedMigration> = Vec::new();

        for file in files {
            let version = file.version();

            let status = match applied.get(&version) {
                None => MigrationStatus::Pending,
                Some(stored_checksum) => {
                    matched.insert(version.clone());
                    let bytes = std::fs::read(&file.path).with_context(|| {
                        format!("failed to read migration file: {}", file.path.display())
                    })?;
                    let current_checksum = sha256_hex(&bytes);
                    let drifted = current_checksum != *stored_checksum;
                    MigrationStatus::Applied {
                        checksum: stored_checksum.clone(),
                        drifted,
                    }
                }
            };

            entries.push(PlannedMigration {
                version,
                file: Some(file.clone()),
                status,
            });
        }

        // Append orphan DB rows — versions in the DB that have no on-disk file.
        let mut orphans: Vec<(&String, &String)> = applied
            .iter()
            .filter(|(v, _)| !matched.contains(*v))
            .collect();
        orphans.sort_by_key(|(v, _)| *v);

        for (version, checksum) in orphans {
            warn!(
                version = %version,
                "migration version is recorded in schema_migrations but \
                 no corresponding file was found on disk"
            );
            entries.push(PlannedMigration {
                version: version.clone(),
                file: None,
                status: MigrationStatus::OrphanApplied {
                    checksum: checksum.clone(),
                },
            });
        }

        Ok(Self { entries })
    }

    /// Return only the entries whose status is [`MigrationStatus::Pending`],
    /// in plan order.
    pub fn pending(&self) -> Vec<&PlannedMigration> {
        self.entries
            .iter()
            .filter(|e| e.status == MigrationStatus::Pending)
            .collect()
    }

    /// Return only the entries that have drifted, in plan order.
    pub fn drifted(&self) -> Vec<&PlannedMigration> {
        self.entries
            .iter()
            .filter(|e| matches!(e.status, MigrationStatus::Applied { drifted: true, .. }))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Query `schema_migrations` and return a map of `version -> checksum`.
fn query_applied(conn: &Connection) -> Result<HashMap<String, String>> {
    let mut stmt = conn
        .prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")
        .context("failed to prepare schema_migrations query")?;

    let rows = stmt
        .query_map([], |row| {
            let version: String = row.get(0)?;
            let checksum: String = row.get(1)?;
            Ok((version, checksum))
        })
        .context("failed to query schema_migrations")?;

    let mut map = HashMap::new();
    for row in rows {
        let (version, checksum) = row.context("failed to read schema_migrations row")?;
        map.insert(version, checksum);
    }
    Ok(map)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use rusqlite::Connection;
    use tempfile::TempDir;

    use super::*;
    use crate::migrate::discover::MigrationFile;

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Open an in-memory DB with the `schema_migrations` table created.
    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version    TEXT NOT NULL PRIMARY KEY,
                checksum   TEXT NOT NULL,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();
        conn
    }

    /// Insert a row into `schema_migrations`.
    fn insert_applied(conn: &Connection, version: &str, checksum: &str) {
        conn.execute(
            "INSERT INTO schema_migrations (version, checksum) VALUES (?1, ?2)",
            rusqlite::params![version, checksum],
        )
        .unwrap();
    }

    /// Write a `.sql` file with `content` into `dir` and return a
    /// `MigrationFile` pointing at it.
    fn write_migration(dir: &TempDir, timestamp: &str, slug: &str, content: &str) -> MigrationFile {
        let filename = format!("{timestamp}_{slug}.sql");
        let path = dir.path().join(&filename);
        std::fs::write(&path, content).unwrap();
        MigrationFile {
            timestamp: timestamp.to_string(),
            slug: slug.to_string(),
            path,
        }
    }

    fn checksum_of(content: &str) -> String {
        sha256_hex(content.as_bytes())
    }

    // -----------------------------------------------------------------------
    // Tests
    // -----------------------------------------------------------------------

    #[test]
    fn all_pending_when_db_empty() {
        let conn = setup_db();
        let dir = TempDir::new().unwrap();
        let files = vec![
            write_migration(
                &dir,
                "20240101000000",
                "alpha",
                "CREATE TABLE a (id INTEGER);",
            ),
            write_migration(
                &dir,
                "20240102000000",
                "beta",
                "CREATE TABLE b (id INTEGER);",
            ),
        ];

        let plan = Plan::build(&files, &conn).unwrap();

        assert_eq!(plan.entries.len(), 2);
        assert_eq!(plan.entries[0].status, MigrationStatus::Pending);
        assert_eq!(plan.entries[1].status, MigrationStatus::Pending);
        assert_eq!(plan.pending().len(), 2);
        assert_eq!(plan.drifted().len(), 0);
    }

    #[test]
    fn all_applied_no_drift() {
        let conn = setup_db();
        let dir = TempDir::new().unwrap();

        let content_a = "CREATE TABLE a (id INTEGER);";
        let content_b = "CREATE TABLE b (id INTEGER);";
        let files = vec![
            write_migration(&dir, "20240101000000", "alpha", content_a),
            write_migration(&dir, "20240102000000", "beta", content_b),
        ];

        insert_applied(&conn, "20240101000000_alpha", &checksum_of(content_a));
        insert_applied(&conn, "20240102000000_beta", &checksum_of(content_b));

        let plan = Plan::build(&files, &conn).unwrap();

        assert_eq!(plan.entries.len(), 2);
        for entry in &plan.entries {
            assert!(
                matches!(
                    entry.status,
                    MigrationStatus::Applied { drifted: false, .. }
                ),
                "expected Applied {{ drifted: false }}, got {:?}",
                entry.status
            );
        }
        assert_eq!(plan.pending().len(), 0);
        assert_eq!(plan.drifted().len(), 0);
    }

    #[test]
    fn drift_detected_when_file_changed() {
        let conn = setup_db();
        let dir = TempDir::new().unwrap();

        let original = "CREATE TABLE a (id INTEGER);";
        let modified = "CREATE TABLE a (id INTEGER, name TEXT);";

        // Write the file with modified content (simulating an edit after apply).
        let file = write_migration(&dir, "20240101000000", "alpha", modified);

        // DB has the checksum of the original content.
        insert_applied(&conn, "20240101000000_alpha", &checksum_of(original));

        let plan = Plan::build(&[file], &conn).unwrap();

        assert_eq!(plan.entries.len(), 1);
        assert!(
            matches!(
                plan.entries[0].status,
                MigrationStatus::Applied { drifted: true, .. }
            ),
            "expected drift, got {:?}",
            plan.entries[0].status
        );
        assert_eq!(plan.drifted().len(), 1);
    }

    #[test]
    fn no_drift_when_checksum_matches() {
        let conn = setup_db();
        let dir = TempDir::new().unwrap();

        let content = "CREATE TABLE a (id INTEGER);";
        let file = write_migration(&dir, "20240101000000", "alpha", content);
        insert_applied(&conn, "20240101000000_alpha", &checksum_of(content));

        let plan = Plan::build(&[file], &conn).unwrap();

        assert!(matches!(
            plan.entries[0].status,
            MigrationStatus::Applied { drifted: false, .. }
        ));
    }

    #[test]
    fn orphan_applied_when_db_row_has_no_file() {
        let conn = setup_db();
        // Insert a DB row for a version that has no file on disk.
        insert_applied(&conn, "20240101000000_ghost", "deadbeef");

        let plan = Plan::build(&[], &conn).unwrap();

        assert_eq!(plan.entries.len(), 1);
        assert!(
            matches!(
                &plan.entries[0].status,
                MigrationStatus::OrphanApplied { checksum } if checksum == "deadbeef"
            ),
            "expected OrphanApplied, got {:?}",
            plan.entries[0].status
        );
    }

    #[test]
    fn mixed_pending_applied_drifted_orphan() {
        let conn = setup_db();
        let dir = TempDir::new().unwrap();

        let content_applied = "CREATE TABLE applied (id INTEGER);";
        let content_original = "CREATE TABLE original (id INTEGER);";
        let content_drifted = "CREATE TABLE original (id INTEGER, extra TEXT);";

        let files = vec![
            // Applied, no drift.
            write_migration(&dir, "20240101000000", "applied", content_applied),
            // Applied, but file was edited → drifted.
            write_migration(&dir, "20240102000000", "drifted", content_drifted),
            // Pending — no DB row.
            write_migration(
                &dir,
                "20240103000000",
                "pending",
                "CREATE TABLE pending (id INTEGER);",
            ),
        ];

        insert_applied(
            &conn,
            "20240101000000_applied",
            &checksum_of(content_applied),
        );
        insert_applied(
            &conn,
            "20240102000000_drifted",
            &checksum_of(content_original),
        );
        // Orphan — in DB but no file on disk.
        insert_applied(&conn, "20240104000000_orphan", "orphanchecksum");

        let plan = Plan::build(&files, &conn).unwrap();

        // 3 files + 1 orphan = 4 entries total.
        assert_eq!(plan.entries.len(), 4);

        // Verify individual statuses by version.
        let by_version: HashMap<_, _> = plan
            .entries
            .iter()
            .map(|e| (e.version.as_str(), &e.status))
            .collect();

        assert!(matches!(
            by_version["20240101000000_applied"],
            MigrationStatus::Applied { drifted: false, .. }
        ));
        assert!(matches!(
            by_version["20240102000000_drifted"],
            MigrationStatus::Applied { drifted: true, .. }
        ));
        assert_eq!(
            by_version["20240103000000_pending"],
            &MigrationStatus::Pending
        );
        assert!(matches!(
            by_version["20240104000000_orphan"],
            MigrationStatus::OrphanApplied { .. }
        ));

        assert_eq!(plan.pending().len(), 1);
        assert_eq!(plan.drifted().len(), 1);
    }

    #[test]
    fn entries_files_have_correct_paths() {
        let conn = setup_db();
        let dir = TempDir::new().unwrap();
        let file = write_migration(&dir, "20240101000000", "alpha", "SELECT 1;");

        let plan = Plan::build(std::slice::from_ref(&file), &conn).unwrap();

        assert_eq!(plan.entries[0].file.as_ref().unwrap().path, file.path);
    }

    #[test]
    fn orphan_entries_have_no_file() {
        let conn = setup_db();
        insert_applied(&conn, "20240101000000_ghost", "abc");

        let plan = Plan::build(&[], &conn).unwrap();

        assert!(plan.entries[0].file.is_none());
    }

    #[test]
    fn pending_helper_returns_only_pending() {
        let conn = setup_db();
        let dir = TempDir::new().unwrap();
        let content = "SELECT 1;";
        let files = vec![
            write_migration(&dir, "20240101000000", "applied", content),
            write_migration(&dir, "20240102000000", "pending", content),
        ];
        insert_applied(&conn, "20240101000000_applied", &checksum_of(content));

        let plan = Plan::build(&files, &conn).unwrap();
        let pending = plan.pending();

        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].version, "20240102000000_pending");
    }

    #[test]
    fn drifted_helper_returns_only_drifted() {
        let conn = setup_db();
        let dir = TempDir::new().unwrap();
        let files = vec![
            write_migration(&dir, "20240101000000", "clean", "SELECT 1;"),
            write_migration(&dir, "20240102000000", "dirty", "SELECT 2;"),
        ];
        insert_applied(&conn, "20240101000000_clean", &checksum_of("SELECT 1;"));
        // Store wrong checksum to trigger drift.
        insert_applied(&conn, "20240102000000_dirty", "wrongchecksum");

        let plan = Plan::build(&files, &conn).unwrap();
        let drifted = plan.drifted();

        assert_eq!(drifted.len(), 1);
        assert_eq!(drifted[0].version, "20240102000000_dirty");
    }

    #[test]
    fn empty_files_and_empty_db_produces_empty_plan() {
        let conn = setup_db();
        let plan = Plan::build(&[], &conn).unwrap();
        assert!(plan.entries.is_empty());
    }

    #[test]
    fn applied_checksum_stored_in_status() {
        let conn = setup_db();
        let dir = TempDir::new().unwrap();
        let content = "CREATE TABLE x (id INTEGER);";
        let stored = checksum_of(content);
        let file = write_migration(&dir, "20240101000000", "x", content);
        insert_applied(&conn, "20240101000000_x", &stored);

        let plan = Plan::build(&[file], &conn).unwrap();

        match &plan.entries[0].status {
            MigrationStatus::Applied { checksum, .. } => assert_eq!(checksum, &stored),
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    #[test]
    fn version_field_matches_file_version() {
        let conn = setup_db();
        let dir = TempDir::new().unwrap();
        let file = write_migration(&dir, "20240528123045", "create_users", "SELECT 1;");

        let plan = Plan::build(&[file], &conn).unwrap();

        assert_eq!(plan.entries[0].version, "20240528123045_create_users");
    }
}
