use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::params;
use sqlparser::ast::Statement;
use sqlparser::dialect::SQLiteDialect;
use sqlparser::parser::Parser;
use tracing::warn;

use crate::config::Config;
use crate::db::Db;
use crate::errors::CliError;
use crate::sha256_hex;
use crate::snapshot;

use super::plan::{Plan, PlannedMigration};

fn resolve_db_path(config: &Config) -> PathBuf {
    config.resolve_path(&config.database_path)
}

/// Check whether the migration file contains a `stig: non-transactional`
/// directive.
///
/// The directive must appear as the first meaningful line — the first line
/// that is not blank and not a SQL comment (`-- ...` or `/* ... */`).  This
/// allows any number of leading comments or blank lines before the directive.
pub fn has_non_transactional_directive(content: &str) -> bool {
    let mut in_block_comment = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if in_block_comment {
            if let Some(end) = trimmed.find("*/") {
                in_block_comment = false;
                let after = trimmed[end + 2..].trim();
                if after.is_empty() {
                    continue;
                }
                return after.eq_ignore_ascii_case("stig: non-transactional");
            }
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("--") {
            continue;
        }
        if let Some(start) = trimmed.find("/*") {
            if let Some(end) = trimmed[start + 2..].find("*/") {
                // Single-line block comment: /* ... */
                let after = trimmed[start + 2 + end + 2..].trim();
                if after.is_empty() {
                    continue;
                }
                return after.eq_ignore_ascii_case("stig: non-transactional");
            } else {
                // Block comment starts but doesn't end on this line
                in_block_comment = true;
                continue;
            }
        }
        return trimmed.eq_ignore_ascii_case("stig: non-transactional");
    }
    false
}

/// Remove the `stig: non-transactional` directive line from `content`.
///
/// The directive sits on its own line and must be stripped before the content
/// is passed to SQLite, since it is not valid SQL.
pub fn strip_directive(content: &str) -> String {
    let mut directive_found = false;
    let mut in_block_comment = false;
    let mut result = String::with_capacity(content.len());

    for line in content.lines() {
        if directive_found {
            result.push_str(line);
            result.push('\n');
            continue;
        }

        let trimmed = line.trim();
        if in_block_comment {
            result.push_str(line);
            result.push('\n');
            if let Some(end) = trimmed.find("*/") {
                in_block_comment = false;
                let after = trimmed[end + 2..].trim();
                if after.eq_ignore_ascii_case("stig: non-transactional") {
                    directive_found = true;
                    // Remove the trailing newline we just added for this line
                    // since the directive part should be stripped.
                    // But we need to keep the block comment part.
                    // Reconstruct: keep everything up to and including */, strip directive.
                    let prefix_end = line.find("*/").unwrap() + 2;
                    let prefix = &line[..prefix_end];
                    // Remove the line we just added and add just the prefix
                    let trim_len = result.len() - line.len() - 1;
                    result.truncate(trim_len);
                    result.push_str(prefix);
                    result.push('\n');
                }
            }
            continue;
        }
        if trimmed.is_empty() {
            result.push_str(line);
            result.push('\n');
            continue;
        }
        if trimmed.starts_with("--") {
            result.push_str(line);
            result.push('\n');
            continue;
        }
        if let Some(start) = trimmed.find("/*") {
            if let Some(end) = trimmed[start + 2..].find("*/") {
                // Single-line block comment: /* ... */
                let after = trimmed[start + 2 + end + 2..].trim();
                if after.eq_ignore_ascii_case("stig: non-transactional") {
                    directive_found = true;
                    // Keep the comment part, strip the directive
                    let comment_end = start + 2 + end + 2;
                    let prefix = &trimmed[..comment_end];
                    if !prefix.trim().is_empty() {
                        result.push_str(prefix);
                        result.push('\n');
                    }
                    continue;
                }
            } else {
                // Block comment starts but doesn't end on this line
                in_block_comment = true;
                result.push_str(line);
                result.push('\n');
                continue;
            }
        }
        if trimmed.eq_ignore_ascii_case("stig: non-transactional") {
            directive_found = true;
            continue;
        }
        result.push_str(line);
        result.push('\n');
    }

    // If the original content didn't end with a newline, don't add a trailing one
    if !content.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    result
}

/// Check whether `sql` contains explicit transaction statements
/// (`BEGIN` or `COMMIT`).
///
/// This is used to warn when a non-transactional migration contains
/// explicit transaction control, which is likely a mistake.
fn has_explicit_transaction(sql: &str) -> bool {
    for line in sql.lines() {
        let trimmed = line.trim().to_uppercase();
        if trimmed.starts_with("BEGIN") || trimmed.starts_with("COMMIT") {
            return true;
        }
    }
    false
}

/// Parse SQL statements from migration content.
///
/// Strips the `stig: non-transactional` directive before parsing.
/// Returns `None` if the parser fails — we let SQLite's `execute_batch`
/// surface the real error rather than rejecting possibly-valid SQL that
/// `sqlparser` does not understand.
fn parse_sql_statements(content: &str) -> Option<Vec<Statement>> {
    let sql = strip_directive(content);
    Parser::parse_sql(&SQLiteDialect {}, &sql).ok()
}

/// Check whether the parsed statements contain explicit transaction control
/// (`BEGIN`/`COMMIT`).
fn has_explicit_transaction_stmts(stmts: &[Statement]) -> bool {
    stmts.iter().any(|s| {
        matches!(
            s,
            Statement::StartTransaction { .. } | Statement::Commit { .. }
        )
    })
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
                     attempted to restore pre-migration snapshot but also failed: {restore_err}"
                ));
            }
            return Err(anyhow::anyhow!(
                "migration {filename} ({version}) failed; database restored to pre-migration state"
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

    let stmts = parse_sql_statements(&content);
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

    let is_non_tx = has_non_transactional_directive(&content);

    if dry_run {
        if can_snapshot {
            println!("would apply  {filename}  (snapshot: pre-{version}.db)");
        } else {
            println!("would apply  {filename}");
        }
        return Ok(false);
    }

    let checksum = sha256_hex(content.as_bytes());

    if is_non_tx {
        let sql = strip_directive(&content);
        let has_tx = stmts.as_ref().map_or_else(
            || has_explicit_transaction(&sql),
            |s| has_explicit_transaction_stmts(s),
        );
        if has_tx {
            warn!(
                migration = %filename,
                "non-transactional migration contains explicit BEGIN/COMMIT statements"
            );
        }
        execute_non_transactional(
            db,
            &filename,
            version,
            &sql,
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
pub fn apply_pending(db: &Db, plan: &Plan, config: &Config, dry_run: bool) -> Result<()> {
    let project_root = &config.project_root;
    let snapshots_dir = project_root.join(&config.backups_dir).join("snapshots");
    let db_path = resolve_db_path(config);
    let can_snapshot = config.auto_snapshot && !db.is_memory();
    let mut n_applied = 0u32;

    for entry in plan.pending() {
        if apply_single_migration(db, entry, &db_path, &snapshots_dir, can_snapshot, dry_run)? {
            n_applied += 1;
        }
    }

    if !dry_run && can_snapshot && snapshots_dir.exists() && n_applied > 0 {
        snapshot::prune_snapshots(&snapshots_dir, config.snapshot_keep)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_as_first_meaningful_line() {
        let content = "stig: non-transactional\nSELECT 1;\n";
        assert!(has_non_transactional_directive(content));
    }

    #[test]
    fn directive_with_trailing_whitespace() {
        let content = "  stig: non-transactional   \nSELECT 1;\n";
        assert!(has_non_transactional_directive(content));
    }

    #[test]
    fn directive_case_insensitive() {
        let content = "STIG: NON-TRANSACTIONAL\nSELECT 1;\n";
        assert!(has_non_transactional_directive(content));
    }

    #[test]
    fn leading_comments_then_directive() {
        let content = "-- My migration\nstig: non-transactional\nVACUUM;\n";
        assert!(has_non_transactional_directive(content));
    }

    #[test]
    fn comments_then_blanks_then_directive() {
        let content = "-- header\n\n\nstig: non-transactional\nSELECT 1;\n";
        assert!(has_non_transactional_directive(content));
    }

    #[test]
    fn many_leading_comments_and_blanks_then_directive() {
        let content = format!(
            "{}\nstig: non-transactional\nSELECT 1;\n",
            (0..100).map(|_| "-- comment\n\n").collect::<String>()
        );
        assert!(has_non_transactional_directive(&content));
    }

    #[test]
    fn sql_as_first_meaningful_line_returns_false() {
        let content = "SELECT 1;\n";
        assert!(!has_non_transactional_directive(content));
    }

    #[test]
    fn directive_in_comment_is_skipped() {
        let content = "-- stig: non-transactional\nSELECT 1;\n";
        assert!(!has_non_transactional_directive(content));
    }

    #[test]
    fn directive_after_comment_and_sql_is_not_detected() {
        let content = "-- comment\nSELECT 1;\nstig: non-transactional\n";
        assert!(!has_non_transactional_directive(content));
    }

    #[test]
    fn empty_content_returns_false() {
        assert!(!has_non_transactional_directive(""));
    }

    #[test]
    fn only_blank_lines_returns_false() {
        assert!(!has_non_transactional_directive("\n\n\n"));
    }

    #[test]
    fn only_comments_returns_false() {
        assert!(!has_non_transactional_directive("-- a\n-- b\n-- c\n"));
    }

    #[test]
    fn directive_in_single_line_block_comment_is_skipped() {
        let content = "/* stig: non-transactional */\nSELECT 1;\n";
        assert!(!has_non_transactional_directive(content));
    }

    #[test]
    fn directive_after_single_line_block_comment_is_detected() {
        let content = "/* migration header */\nstig: non-transactional\nSELECT 1;\n";
        assert!(has_non_transactional_directive(content));
    }

    #[test]
    fn directive_in_multi_line_block_comment_is_skipped() {
        let content = "/*\n * stig: non-transactional\n */\nSELECT 1;\n";
        assert!(!has_non_transactional_directive(content));
    }

    #[test]
    fn directive_after_multi_line_block_comment_is_detected() {
        let content = "/*\n * Migration description\n */\nstig: non-transactional\nSELECT 1;\n";
        assert!(has_non_transactional_directive(content));
    }

    #[test]
    fn directive_on_same_line_after_block_comment_end() {
        let content = "/* comment */ stig: non-transactional\nSELECT 1;\n";
        assert!(has_non_transactional_directive(content));
    }

    #[test]
    fn block_comment_with_no_closing_treated_as_unclosed() {
        let content = "/* stig: non-transactional\nSELECT 1;\n";
        assert!(!has_non_transactional_directive(content));
    }

    // -----------------------------------------------------------------------
    // strip_directive tests
    // -----------------------------------------------------------------------

    #[test]
    fn strip_directive_removes_bare_token() {
        let input = "stig: non-transactional\nSELECT 1;\n";
        assert_eq!(strip_directive(input), "SELECT 1;\n");
    }

    #[test]
    fn strip_directive_preserves_leading_comments() {
        let input = "-- comment\nstig: non-transactional\nSELECT 1;\n";
        assert_eq!(strip_directive(input), "-- comment\nSELECT 1;\n");
    }

    #[test]
    fn strip_directive_preserves_leading_blanks() {
        let input = "\n\nstig: non-transactional\nSELECT 1;\n";
        assert_eq!(strip_directive(input), "\n\nSELECT 1;\n");
    }

    #[test]
    fn strip_directive_noop_when_no_directive() {
        let input = "SELECT 1;\n";
        assert_eq!(strip_directive(input), "SELECT 1;\n");
    }

    #[test]
    fn strip_directive_strips_only_first_meaningful_line() {
        let input = "-- hdr\nstig: non-transactional\nSELECT 1;\nstig: non-transactional\n";
        assert_eq!(
            strip_directive(input),
            "-- hdr\nSELECT 1;\nstig: non-transactional\n"
        );
    }

    #[test]
    fn strip_directive_preserves_trailing_newline() {
        let input = "stig: non-transactional\nVACUUM;\n";
        assert_eq!(strip_directive(input), "VACUUM;\n");
    }

    #[test]
    fn strip_directive_no_trailing_newline_in_input() {
        let input = "stig: non-transactional\nSELECT 1";
        assert_eq!(strip_directive(input), "SELECT 1");
    }

    #[test]
    fn strip_directive_leading_whitespace_on_directive() {
        let input = "  stig: non-transactional\nSELECT 1;\n";
        assert_eq!(strip_directive(input), "SELECT 1;\n");
    }

    #[test]
    fn strip_directive_empty_input() {
        assert_eq!(strip_directive(""), "");
    }

    #[test]
    fn strip_directive_only_directive() {
        assert_eq!(strip_directive("stig: non-transactional\n"), "");
    }

    #[test]
    fn strip_directive_comments_blanks_directive_sql() {
        let input = "-- header\n\nstig: non-transactional\n\nCREATE TABLE x (id INTEGER);\n";
        assert_eq!(
            strip_directive(input),
            "-- header\n\n\nCREATE TABLE x (id INTEGER);\n"
        );
    }

    #[test]
    fn strip_directive_preserves_single_line_block_comment() {
        let input = "/* migration header */\nstig: non-transactional\nSELECT 1;\n";
        assert_eq!(
            strip_directive(input),
            "/* migration header */\nSELECT 1;\n"
        );
    }

    #[test]
    fn strip_directive_preserves_multi_line_block_comment() {
        let input = "/*\n * Migration description\n */\nstig: non-transactional\nSELECT 1;\n";
        assert_eq!(
            strip_directive(input),
            "/*\n * Migration description\n */\nSELECT 1;\n"
        );
    }

    #[test]
    fn strip_directive_preserves_block_comment_with_directive_inside() {
        let input = "/* stig: non-transactional */\nSELECT 1;\n";
        assert_eq!(
            strip_directive(input),
            "/* stig: non-transactional */\nSELECT 1;\n"
        );
    }

    #[test]
    fn strip_directive_handles_directive_after_multi_line_block_comment() {
        let input = "/*\n * Comment\n */\nstig: non-transactional\nVACUUM;\n";
        assert_eq!(strip_directive(input), "/*\n * Comment\n */\nVACUUM;\n");
    }

    #[test]
    fn strip_directive_handles_directive_on_same_line_after_block_comment() {
        let input = "/* comment */ stig: non-transactional\nSELECT 1;\n";
        assert_eq!(strip_directive(input), "/* comment */\nSELECT 1;\n");
    }

    #[test]
    fn strip_directive_handles_unclosed_block_comment() {
        let input = "/* stig: non-transactional\nSELECT 1;\n";
        assert_eq!(
            strip_directive(input),
            "/* stig: non-transactional\nSELECT 1;\n"
        );
    }

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

    // -----------------------------------------------------------------------
    // parse_sql_statements tests
    // -----------------------------------------------------------------------

    fn is_empty(content: &str) -> bool {
        parse_sql_statements(content).is_some_and(|s| s.is_empty())
    }

    #[test]
    fn parse_empty_string() {
        assert!(is_empty(""));
    }

    #[test]
    fn parse_whitespace_only() {
        assert!(is_empty("   \n\n  \t  \n"));
    }

    #[test]
    fn parse_comments_only() {
        assert!(is_empty("-- just a comment\n-- another comment\n"));
    }

    #[test]
    fn parse_block_comments_only() {
        assert!(is_empty("/* block comment */\n/* another */\n"));
    }

    #[test]
    fn parse_directive_only() {
        assert!(is_empty("stig: non-transactional\n"));
    }

    #[test]
    fn parse_directive_and_comments() {
        assert!(is_empty("-- header\nstig: non-transactional\n\n"));
    }

    #[test]
    fn parse_with_select() {
        assert!(!is_empty("SELECT 1;"));
    }

    #[test]
    fn parse_with_create_table() {
        assert!(!is_empty("CREATE TABLE x (id INTEGER PRIMARY KEY);"));
    }

    #[test]
    fn parse_with_insert() {
        assert!(!is_empty("INSERT INTO x VALUES (1);"));
    }

    #[test]
    fn parse_with_pragma() {
        assert!(!is_empty("PRAGMA journal_mode = WAL;"));
    }

    #[test]
    fn parse_with_vacuum() {
        assert!(!is_empty("VACUUM;"));
    }

    #[test]
    fn parse_non_tx_with_sql() {
        assert!(!is_empty(
            "stig: non-transactional\nCREATE TABLE x (id INTEGER);\n"
        ));
    }

    #[test]
    fn parse_invalid_sql_returns_none() {
        // sqlparser may not understand all SQLite syntax; we defer to SQLite.
        assert!(parse_sql_statements("this is not valid sql at all").is_none());
    }

    // -----------------------------------------------------------------------
    // has_explicit_transaction_stmts tests
    // -----------------------------------------------------------------------

    #[test]
    fn has_explicit_transaction_stmts_detects_begin() {
        let stmts = parse_sql_statements("BEGIN; SELECT 1; COMMIT;").unwrap();
        assert!(has_explicit_transaction_stmts(&stmts));
    }

    #[test]
    fn has_explicit_transaction_stmts_detects_commit_only() {
        let stmts = parse_sql_statements("SELECT 1; COMMIT;").unwrap();
        assert!(has_explicit_transaction_stmts(&stmts));
    }

    #[test]
    fn has_explicit_transaction_stmts_no_transaction() {
        let stmts = parse_sql_statements("SELECT 1; VACUUM;").unwrap();
        assert!(!has_explicit_transaction_stmts(&stmts));
    }

    #[test]
    fn has_explicit_transaction_stmts_empty() {
        let stmts = parse_sql_statements("").unwrap();
        assert!(stmts.is_empty());
        assert!(!has_explicit_transaction_stmts(&stmts));
    }
}
