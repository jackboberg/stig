use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::params;
use sqlparser::ast::Statement;
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;
use tracing::warn;

use crate::config::Runtime;
use crate::db::Db;
use crate::errors::CliError;
use crate::sha256_hex;
use crate::snapshot;

use super::directive::parse_directive;
use super::plan::{Plan, PlannedMigration};

/// Check whether the migration file contains a `stig: non-transactional`
/// directive as the first meaningful line (after any blank lines or comments).
pub fn has_non_transactional_directive(content: &str) -> bool {
    parse_directive(content).is_non_transactional
}

/// Remove the `stig: non-transactional` directive line from `content`.
///
/// The directive must appear as the first meaningful line. If found, the
/// directive line is removed and the remaining content is returned.
pub fn strip_directive(content: &str) -> String {
    parse_directive(content).sql
}

/// Check whether `sql` contains explicit transaction statements
/// (`BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`, or `RELEASE SAVEPOINT`).
///
/// This is a conservative line-based fallback used only when `sqlparser`
/// fails to parse the migration. It may false-positive on identifiers that
/// start with these keywords (e.g. `begin_table`), which is acceptable for a
/// warning-only code path.
///
/// This is used to warn when a non-transactional migration contains
/// explicit transaction control, which is likely a mistake.
fn has_explicit_transaction(sql: &str) -> bool {
    for line in sql.lines() {
        let trimmed = line.trim().to_uppercase();
        if trimmed.starts_with("BEGIN")
            || trimmed.starts_with("COMMIT")
            || trimmed.starts_with("ROLLBACK")
            || trimmed.starts_with("SAVEPOINT")
            || trimmed.starts_with("RELEASE")
        {
            return true;
        }
    }
    false
}

/// Execute a non-transactional migration, restoring the pre-migration
/// snapshot on failure when snapshots are available.
fn execute_non_transactional(
    db: &Db,
    filename: &str,
    version: &str,
    sql: &str,
    db_path: &Path,
    snapshots_dir: &Path,
    can_snapshot: bool,
) -> Result<()> {
    let exec_result = db
        .connection()
        .execute_batch(sql)
        .with_context(|| format!("failed to execute {filename} ({version})"));

    if let Err(e) = exec_result {
        if can_snapshot && snapshot::snapshot_exists(version, snapshots_dir) {
            if let Err(restore_err) = snapshot::restore_snapshot(version, db_path, snapshots_dir) {
                return Err(anyhow::anyhow!(
                    "migration {filename} ({version}) failed; \
                     attempted to restore pre-migration snapshot but also failed: {restore_err}\nCaused by: {e}"
                ));
            }
            return Err(anyhow::anyhow!(
                "migration {filename} ({version}) failed; database restored to pre-migration state\nCaused by: {e}"
            ));
        }
        return Err(e);
    }
    Ok(())
}

/// Execute a transactional migration wrapped in `BEGIN/COMMIT`.
fn execute_transactional(db: &Db, filename: &str, version: &str, content: &str) -> Result<()> {
    let sql = format!("BEGIN TRANSACTION;\n{content}\nCOMMIT;");
    db.connection()
        .execute_batch(&sql)
        .with_context(|| format!("failed to execute {filename} ({version})"))
}

/// Record a successfully applied migration in `schema_migrations`.
fn record_migration(db: &Db, version: &str, checksum: &str, filename: &str) -> Result<()> {
    db.connection()
        .execute(
            "INSERT INTO schema_migrations (version, checksum) VALUES (?1, ?2)",
            params![version, checksum],
        )
        .with_context(|| format!("failed to record {filename} ({version}) in schema_migrations"))
        .map(|_| ())
}

/// Apply a single pending migration.  Returns `true` if the migration was
/// actually applied (not a dry-run).
fn apply_single_migration(
    db: &Db,
    entry: &PlannedMigration,
    db_path: &Path,
    snapshots_dir: &Path,
    can_snapshot: bool,
    dry_run: bool,
) -> Result<bool> {
    let version = &entry.version;
    let file = entry.file.as_ref().context("pending entry has no file")?;

    let filename = file
        .path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| version.clone());

    let content = std::fs::read_to_string(&file.path)
        .with_context(|| format!("failed to read {}", file.path.display()))?;

    let directive_result = parse_directive(&content);
    let sql_without_directive = &directive_result.sql;
    let stmts: Option<Vec<Statement>> =
        Parser::parse_sql(&SQLiteDialect {}, sql_without_directive).ok();
    if stmts.as_ref().is_some_and(Vec::is_empty) {
        return Err(
            CliError::Usage(format!("migration {filename} contains no SQL statements")).into(),
        );
    }

    if can_snapshot && !dry_run {
        db.checkpoint()?;
        snapshot::take_snapshot(version, db_path, snapshots_dir)
            .with_context(|| format!("failed to snapshot before {version}"))?;
    }

    if dry_run {
        if can_snapshot {
            println!("would apply  {filename}  (snapshot: pre-{version}.db)");
        } else {
            println!("would apply  {filename}");
        }
        return Ok(false);
    }

    let checksum = sha256_hex(content.as_bytes());

    if directive_result.is_non_transactional {
        let has_tx = stmts.as_ref().map_or_else(
            || has_explicit_transaction(sql_without_directive),
            |s| s.iter().any(crate::sql::is_transaction_control),
        );
        if has_tx {
            warn!(
                migration = %filename,
                "non-transactional migration contains explicit transaction control statements (BEGIN/COMMIT/ROLLBACK/SAVEPOINT)"
            );
        }
        execute_non_transactional(
            db,
            &filename,
            version,
            sql_without_directive,
            db_path,
            snapshots_dir,
            can_snapshot,
        )?;
    } else {
        execute_transactional(db, &filename, version, &content)?;
    }

    record_migration(db, version, &checksum, &filename)?;

    if can_snapshot {
        println!("apply  {filename}  (snapshot: pre-{version}.db)");
    } else {
        println!("apply  {filename}");
    }

    Ok(true)
}

/// Apply all pending migrations from `plan` against `db`.
///
/// For each pending migration:
/// 1. If `auto_snapshot` is true and not `dry_run`: checkpoint + take snapshot.
/// 2. Read the file content and check for the non-transactional directive.
/// 3. If not `dry_run`: compute checksum, execute SQL, record in
///    `schema_migrations`, prune snapshots.
///
/// When `dry_run` is true, files are read and parsed but no SQL is executed
/// and no snapshots are written.
pub fn apply_pending(db: &Db, plan: &Plan, config: &Runtime, dry_run: bool) -> Result<()> {
    let snapshots_dir = config.snapshots_path();
    let db_path = config.db_path();
    let can_snapshot = config.file.auto_snapshot && !db.is_memory();
    let mut n_applied = 0u32;

    for entry in plan.pending() {
        if apply_single_migration(db, entry, &db_path, &snapshots_dir, can_snapshot, dry_run)? {
            n_applied += 1;
        }
    }

    if !dry_run && can_snapshot && snapshots_dir.exists() && n_applied > 0 {
        snapshot::prune_snapshots(&snapshots_dir, config.file.snapshot_keep)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // has_explicit_transaction tests
    // -----------------------------------------------------------------------

    #[test]
    fn has_explicit_transaction_detects_begin() {
        assert!(has_explicit_transaction("BEGIN;\nSELECT 1;\nCOMMIT;"));
    }

    #[test]
    fn has_explicit_transaction_detects_commit() {
        assert!(has_explicit_transaction("SELECT 1;\nCOMMIT;"));
    }

    #[test]
    fn has_explicit_transaction_case_insensitive() {
        assert!(has_explicit_transaction("begin;\nSELECT 1;\ncommit;"));
    }

    #[test]
    fn has_explicit_transaction_with_whitespace() {
        assert!(has_explicit_transaction("  BEGIN TRANSACTION;\nSELECT 1;"));
    }

    #[test]
    fn has_explicit_transaction_returns_false_for_plain_sql() {
        assert!(!has_explicit_transaction("SELECT 1;\nVACUUM;"));
    }

    #[test]
    fn has_explicit_transaction_returns_false_for_empty() {
        assert!(!has_explicit_transaction(""));
    }

    #[test]
    fn has_explicit_transaction_detects_rollback() {
        assert!(has_explicit_transaction("SELECT 1;\nROLLBACK;"));
    }

    #[test]
    fn has_explicit_transaction_detects_savepoint() {
        assert!(has_explicit_transaction("SAVEPOINT sp1;"));
    }

    #[test]
    fn has_explicit_transaction_detects_release_savepoint() {
        assert!(has_explicit_transaction("RELEASE SAVEPOINT sp1;"));
    }
}
