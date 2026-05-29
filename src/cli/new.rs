//! Implementation of `stig new`.
//!
//! Creates a new migration file in the configured migrations directory:
//! - Slugifies the description per §3.2.
//! - Generates a UTC timestamp filename `<yyyyMMddHHmmss>_<slug>.sql`.
//! - Errors (exit 2) if a file with the same timestamp already exists
//!   (same-second collision is treated as a hard error for MVP; wait a
//!   second and retry).
//! - Writes the standard migration template atomically via `create_new`.
//! - Opens `$EDITOR` unless `--no-edit` is passed or `$EDITOR` is unset.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use chrono::{DateTime, Utc};

use crate::config::Config;
use crate::errors::CliError;

/// Run `stig new <description> [--no-edit]`.
pub fn run(description: String, no_edit: bool) -> anyhow::Result<()> {
    let config = Config::load(None, None, None)?;
    let migrations_dir = config.project_root.join(&config.migrations_dir);

    if !migrations_dir.is_dir() {
        return Err(CliError::Usage(
            "migrations directory not found — run `stig init` first".into(),
        )
        .into());
    }

    let slug = slugify(&description)?;
    let now = Utc::now();
    let path = build_path(&migrations_dir, &slug, now)?;

    write_template(&path, &description, now)?;

    // Print relative path when possible, otherwise absolute.
    let display = path
        .strip_prefix(&config.project_root)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| path.clone());
    println!("✓ {}", display.display());

    if !no_edit
        && let Ok(editor) = std::env::var("EDITOR")
        && !editor.is_empty()
    {
        println!("  opening in {} ...", editor);
        let status = std::process::Command::new(&editor)
            .arg(&path)
            .status()
            .with_context(|| format!("failed to launch editor `{editor}`"))?;
        if !status.success() {
            return Err(CliError::Generic(anyhow::anyhow!(
                "editor `{editor}` exited with status {status}"
            ))
            .into());
        }
    }

    Ok(())
}

/// Slugify a migration description per §3.2.
///
/// - Lowercase
/// - Replace non-`[a-z0-9]` characters with `_`
/// - Collapse consecutive underscores to a single `_`
/// - Strip leading/trailing underscores
/// - Truncate to 60 characters
/// - Return `Err(CliError::Usage)` if the result is empty
pub fn slugify(description: &str) -> anyhow::Result<String> {
    let lower = description.to_lowercase();

    // Replace non-alphanumeric with '_'
    let replaced: String = lower
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();

    // Collapse consecutive underscores
    let mut slug = String::with_capacity(replaced.len());
    let mut prev_under = false;
    for c in replaced.chars() {
        if c == '_' {
            if !prev_under {
                slug.push(c);
            }
            prev_under = true;
        } else {
            slug.push(c);
            prev_under = false;
        }
    }

    // Strip leading/trailing underscores
    let slug = slug.trim_matches('_');

    // Truncate to 60 chars (char boundary safe for ASCII)
    let slug = if slug.len() > 60 { &slug[..60] } else { slug };
    // After truncation, trim any trailing underscore that may have been exposed.
    let slug = slug.trim_end_matches('_');

    if slug.is_empty() {
        return Err(CliError::Usage(
            "description produces an empty slug — provide a non-empty description".into(),
        )
        .into());
    }

    Ok(slug.to_string())
}

/// Compute the target file path for a new migration.
///
/// Returns `Err(CliError::Usage)` if a file with the same timestamp already
/// exists (indicates a real collision, not just a retry situation).
pub fn build_path(migrations_dir: &Path, slug: &str, ts: DateTime<Utc>) -> anyhow::Result<PathBuf> {
    let ts_str = ts.format("%Y%m%d%H%M%S").to_string();
    let filename = format!("{ts_str}_{slug}.sql");
    let path = migrations_dir.join(&filename);

    if path.exists() {
        return Err(CliError::Usage(format!(
            "a migration with timestamp {ts_str} already exists — wait a second and retry"
        ))
        .into());
    }

    Ok(path)
}

/// Write the standard migration template to `path`.
///
/// Uses `create_new` so the operation is atomic — it will fail if the file
/// already exists, providing a last-line-of-defence against clobbering an
/// existing migration even if the collision check in `build_path` races.
fn write_template(path: &Path, description: &str, ts: DateTime<Utc>) -> anyhow::Result<()> {
    let created = ts.to_rfc3339();
    let content = format!(
        "-- Migration: {description}\n\
         -- Created:   {created}\n\
         --\n\
         -- To make this migration apply outside a transaction (e.g. to run\n\
         -- PRAGMA or FTS5 rebuild statements that don't allow transactions),\n\
         -- uncomment the directive on the next line:\n\
         -- stig: non-transactional\n\
         \n\
         \n"
    );
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("failed to create migration file {}", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write migration file {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ── slugify ──────────────────────────────────────────────────────────────

    #[test]
    fn slugify_basic() {
        assert_eq!(slugify("Add Widgets!!!").unwrap(), "add_widgets");
    }

    #[test]
    fn slugify_already_clean() {
        assert_eq!(slugify("create_users").unwrap(), "create_users");
    }

    #[test]
    fn slugify_numbers_preserved() {
        assert_eq!(slugify("v2 schema").unwrap(), "v2_schema");
    }

    #[test]
    fn slugify_collapses_consecutive_separators() {
        assert_eq!(slugify("foo---bar").unwrap(), "foo_bar");
    }

    #[test]
    fn slugify_strips_leading_trailing() {
        assert_eq!(slugify("!!!hello!!!").unwrap(), "hello");
    }

    #[test]
    fn slugify_truncates_at_60() {
        let long = "a".repeat(80);
        assert_eq!(slugify(&long).unwrap().len(), 60);
    }

    #[test]
    fn slugify_truncate_strips_trailing_underscore() {
        // 59 'a's + separator + more chars — truncation at 60 lands on '_'
        let input = format!("{}_extra", "a".repeat(59));
        let result = slugify(&input).unwrap();
        assert!(!result.ends_with('_'));
    }

    #[test]
    fn slugify_empty_string_is_error() {
        assert!(slugify("").is_err());
    }

    #[test]
    fn slugify_whitespace_only_is_error() {
        assert!(slugify("   ").is_err());
    }

    #[test]
    fn slugify_punctuation_only_is_error() {
        assert!(slugify("!!!###").is_err());
    }

    // ── build_path ───────────────────────────────────────────────────────────

    #[test]
    fn build_path_returns_correct_filename() {
        let dir = TempDir::new().unwrap();
        let ts = "2026-05-29T10:30:00Z".parse::<DateTime<Utc>>().unwrap();
        let path = build_path(dir.path(), "add_widgets", ts).unwrap();
        assert_eq!(
            path.file_name().unwrap().to_str().unwrap(),
            "20260529103000_add_widgets.sql"
        );
    }

    #[test]
    fn build_path_errors_on_collision() {
        let dir = TempDir::new().unwrap();
        let ts = "2026-05-29T10:30:00Z".parse::<DateTime<Utc>>().unwrap();
        // Pre-create the file to simulate a collision.
        std::fs::write(dir.path().join("20260529103000_add_widgets.sql"), "").unwrap();
        let err = build_path(dir.path(), "add_widgets", ts).unwrap_err();
        assert!(err.to_string().contains("already exists"));
    }

    // ── write_template ───────────────────────────────────────────────────────

    #[test]
    fn write_template_contains_expected_lines() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.sql");
        let ts = "2026-05-29T10:30:00Z".parse::<DateTime<Utc>>().unwrap();
        write_template(&path, "Add Widgets!!!", ts).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("-- Migration: Add Widgets!!!"));
        assert!(content.contains("-- Created:   2026-05-29T10:30:00+00:00"));
        assert!(content.contains("-- stig: non-transactional"));
        assert!(content.ends_with("\n\n"));
    }
}
