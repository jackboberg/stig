pub mod apply;
pub mod directive;
pub mod discover;
pub mod plan;

use std::path::Path;

use anyhow::Context;

use crate::config::Runtime;
use crate::db::Db;

use self::discover::discover;
use self::plan::Plan;

/// Discover migration files, build a plan, and apply all pending migrations.
///
/// The caller provides an already-open [`Db`]; this function handles
/// discovery, planning, and application.
///
/// This is the shared implementation used by both `stig redo` and `stig reset`.
/// Note: the schema-manifest fast path is intentionally NOT used here — it is
/// only safe when the database is known to be completely empty (as in `reset`).
/// After a snapshot restore (`redo`), the database may already contain schema
/// from prior migrations, so applying the full manifest would fail.
pub fn reapply_pending(db: &Db, config: &Runtime, migrations_dir: &Path) -> anyhow::Result<()> {
    let files = discover(migrations_dir).context("failed to discover migration files")?;
    let plan = Plan::build(&files, db.connection())?;

    apply::apply_pending(db, &plan, config, false)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigFile, Runtime};
    use crate::db::Db;
    use tempfile::TempDir;

    fn make_runtime(dir: &TempDir) -> Runtime {
        Runtime {
            project_root: dir.path().to_path_buf(),
            file: ConfigFile {
                database_path: "app.db".to_string(),
                ..ConfigFile::default()
            },
        }
    }

    #[test]
    fn reapply_pending_applies_discovered_migrations() {
        let dir = TempDir::new().unwrap();
        let config = make_runtime(&dir);

        let migrations_dir = dir.path().join("db/migrations");
        std::fs::create_dir_all(&migrations_dir).unwrap();
        std::fs::write(
            migrations_dir.join("20240101000000_create_foo.sql"),
            "CREATE TABLE foo (id INTEGER PRIMARY KEY);",
        )
        .unwrap();

        let snapshots_dir = dir.path().join("db/snapshots");
        std::fs::create_dir_all(&snapshots_dir).unwrap();

        let db = Db::open(&config).unwrap();
        reapply_pending(&db, &config, &migrations_dir).unwrap();

        let count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM schema_migrations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let table_count: i64 = db
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='foo'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(table_count, 1);
    }
}
