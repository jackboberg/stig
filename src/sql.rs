//! Shared SQL parsing helpers.

use sqlparser::ast::Statement;

/// Check whether a parsed SQL statement is explicit transaction control.
pub(crate) fn is_transaction_control(stmt: &Statement) -> bool {
    matches!(
        stmt,
        Statement::StartTransaction { .. }
            | Statement::Commit { .. }
            | Statement::Rollback { .. }
            | Statement::Savepoint { .. }
            | Statement::ReleaseSavepoint { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlparser::dialect::SQLiteDialect;
    use sqlparser::parser::Parser;

    fn parse_one(sql: &str) -> Statement {
        Parser::parse_sql(&SQLiteDialect {}, sql)
            .unwrap()
            .into_iter()
            .next()
            .unwrap()
    }

    #[test]
    fn detects_begin() {
        assert!(is_transaction_control(&parse_one("BEGIN;")));
        assert!(is_transaction_control(&parse_one("BEGIN TRANSACTION;")));
    }

    #[test]
    fn detects_commit() {
        assert!(is_transaction_control(&parse_one("COMMIT;")));
        assert!(is_transaction_control(&parse_one("COMMIT TRANSACTION;")));
    }

    #[test]
    fn detects_rollback() {
        assert!(is_transaction_control(&parse_one("ROLLBACK;")));
    }

    #[test]
    fn detects_savepoints() {
        assert!(is_transaction_control(&parse_one("SAVEPOINT sp;")));
        assert!(is_transaction_control(&parse_one("RELEASE SAVEPOINT sp;")));
    }

    #[test]
    fn returns_false_for_non_transaction_statements() {
        assert!(!is_transaction_control(&parse_one(
            "CREATE TABLE foo (id INTEGER PRIMARY KEY);"
        )));
        assert!(!is_transaction_control(&parse_one(
            "INSERT INTO foo VALUES (1);"
        )));
        assert!(!is_transaction_control(&parse_one("SELECT 1;")));
    }
}
