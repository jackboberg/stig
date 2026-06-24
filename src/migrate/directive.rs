/// Result of parsing a migration file for the `stig: non-transactional`
/// directive.
#[derive(Debug, Clone, PartialEq)]
pub struct DirectiveResult {
    /// Whether the directive was found as the first meaningful line.
    pub is_non_transactional: bool,
    /// The SQL content with the directive stripped (if found).
    pub sql: String,
}

/// Parse migration content for `stig: non-transactional` directive.
///
/// Returns both the detection result and the stripped SQL in a single pass,
/// eliminating duplication between detection and stripping.
///
/// The directive must appear as the first meaningful line — the first line
/// that is not blank and not a SQL comment (`-- ...` or `/* ... */`).
pub fn parse_directive(content: &str) -> DirectiveResult {
    let mut in_block_comment = false;
    let mut directive_found = false;
    let mut result = String::with_capacity(content.len());

    for line in content.split_inclusive('\n') {
        let trimmed = if let Some(rest) = line.strip_suffix("\r\n") {
            rest
        } else if let Some(rest) = line.strip_suffix('\n') {
            rest
        } else {
            line
        };

        let trimmed_no_ws = trimmed.trim();

        if directive_found {
            result.push_str(line);
            continue;
        }

        if in_block_comment {
            if let Some(end) = trimmed.find("*/") {
                in_block_comment = false;
                let after = trimmed[end + 2..].trim();
                if after.eq_ignore_ascii_case("stig: non-transactional") {
                    directive_found = true;
                    result.push_str(&trimmed[..end + 2]);
                    result.push('\n');
                    continue;
                }
            }
            result.push_str(line);
            continue;
        }

        if trimmed_no_ws.is_empty() {
            result.push_str(line);
            continue;
        }

        if trimmed_no_ws.starts_with("--") {
            result.push_str(line);
            continue;
        }

        if let Some(start) = trimmed_no_ws.find("/*") {
            if let Some(end) = trimmed_no_ws[start + 2..].find("*/") {
                let after = trimmed_no_ws[start + 2 + end + 2..].trim();
                if after.eq_ignore_ascii_case("stig: non-transactional") {
                    directive_found = true;
                    let comment_end = start + 2 + end + 2;
                    let prefix = &trimmed_no_ws[..comment_end];
                    if !prefix.trim().is_empty() {
                        result.push_str(prefix);
                        result.push('\n');
                    }
                    continue;
                }
            } else {
                in_block_comment = true;
                result.push_str(line);
                continue;
            }
        }

        if trimmed_no_ws.eq_ignore_ascii_case("stig: non-transactional") {
            directive_found = true;
            continue;
        }

        result.push_str(line);
    }

    if !content.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    DirectiveResult {
        is_non_transactional: directive_found,
        sql: result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directive_as_first_meaningful_line() {
        let result = parse_directive("stig: non-transactional\nSELECT 1;\n");
        assert!(result.is_non_transactional);
    }

    #[test]
    fn directive_with_trailing_whitespace() {
        let result = parse_directive("  stig: non-transactional   \nSELECT 1;\n");
        assert!(result.is_non_transactional);
    }

    #[test]
    fn directive_case_insensitive() {
        let result = parse_directive("STIG: NON-TRANSACTIONAL\nSELECT 1;\n");
        assert!(result.is_non_transactional);
    }

    #[test]
    fn leading_comments_then_directive() {
        let result = parse_directive("-- My migration\nstig: non-transactional\nVACUUM;\n");
        assert!(result.is_non_transactional);
    }

    #[test]
    fn sql_as_first_meaningful_line_returns_false() {
        let result = parse_directive("SELECT 1;\n");
        assert!(!result.is_non_transactional);
    }

    #[test]
    fn directive_in_comment_is_skipped() {
        let result = parse_directive("-- stig: non-transactional\nSELECT 1;\n");
        assert!(!result.is_non_transactional);
    }

    #[test]
    fn directive_in_single_line_block_comment_is_skipped() {
        let result = parse_directive("/* stig: non-transactional */\nSELECT 1;\n");
        assert!(!result.is_non_transactional);
    }

    #[test]
    fn directive_after_single_line_block_comment_is_detected() {
        let result =
            parse_directive("/* migration header */\nstig: non-transactional\nSELECT 1;\n");
        assert!(result.is_non_transactional);
    }

    #[test]
    fn directive_in_multi_line_block_comment_is_skipped() {
        let result = parse_directive("/*\n * stig: non-transactional\n */\nSELECT 1;\n");
        assert!(!result.is_non_transactional);
    }

    #[test]
    fn directive_after_multi_line_block_comment_is_detected() {
        let result = parse_directive(
            "/*\n * Migration description\n */\nstig: non-transactional\nSELECT 1;\n",
        );
        assert!(result.is_non_transactional);
    }

    #[test]
    fn directive_on_same_line_after_block_comment_end() {
        let result = parse_directive("/* comment */ stig: non-transactional\nSELECT 1;\n");
        assert!(result.is_non_transactional);
    }

    #[test]
    fn block_comment_with_no_closing_treated_as_unclosed() {
        let result = parse_directive("/* stig: non-transactional\nSELECT 1;\n");
        assert!(!result.is_non_transactional);
    }

    #[test]
    fn strip_directive_removes_bare_token() {
        let result = parse_directive("stig: non-transactional\nSELECT 1;\n");
        assert_eq!(result.sql, "SELECT 1;\n");
    }

    #[test]
    fn strip_directive_preserves_leading_comments() {
        let result = parse_directive("-- comment\nstig: non-transactional\nSELECT 1;\n");
        assert_eq!(result.sql, "-- comment\nSELECT 1;\n");
    }

    #[test]
    fn strip_directive_noop_when_no_directive() {
        let result = parse_directive("SELECT 1;\n");
        assert_eq!(result.sql, "SELECT 1;\n");
    }

    #[test]
    fn strip_directive_preserves_single_line_block_comment() {
        let result =
            parse_directive("/* migration header */\nstig: non-transactional\nSELECT 1;\n");
        assert_eq!(result.sql, "/* migration header */\nSELECT 1;\n");
    }

    #[test]
    fn strip_directive_preserves_multi_line_block_comment() {
        let result = parse_directive(
            "/*\n * Migration description\n */\nstig: non-transactional\nSELECT 1;\n",
        );
        assert_eq!(result.sql, "/*\n * Migration description\n */\nSELECT 1;\n");
    }

    #[test]
    fn strip_directive_preserves_block_comment_with_directive_inside() {
        let result = parse_directive("/* stig: non-transactional */\nSELECT 1;\n");
        assert_eq!(result.sql, "/* stig: non-transactional */\nSELECT 1;\n");
    }

    #[test]
    fn strip_directive_handles_directive_on_same_line_after_block_comment() {
        let result = parse_directive("/* comment */ stig: non-transactional\nSELECT 1;\n");
        assert_eq!(result.sql, "/* comment */\nSELECT 1;\n");
    }

    #[test]
    fn strip_directive_empty_input() {
        let result = parse_directive("");
        assert_eq!(result.sql, "");
    }

    #[test]
    fn strip_directive_only_directive() {
        let result = parse_directive("stig: non-transactional\n");
        assert_eq!(result.sql, "");
    }

    #[test]
    fn strip_directive_preserves_trailing_newline_format() {
        let result = parse_directive("stig: non-transactional\nVACUUM;\n");
        assert_eq!(result.sql, "VACUUM;\n");
    }

    #[test]
    fn strip_directive_no_trailing_newline_in_input() {
        let result = parse_directive("stig: non-transactional\nSELECT 1");
        assert_eq!(result.sql, "SELECT 1");
    }
}
