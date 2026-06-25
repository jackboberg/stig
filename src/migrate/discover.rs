use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tracing::warn;

/// A parsed migration file found on disk.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct MigrationFile {
    /// 14-digit UTC timestamp string, e.g. `"20240528123045"`.
    pub timestamp: String,
    /// Slug portion of the filename, e.g. `"create_users"`.
    pub slug: String,
    /// Full path to the `.sql` file.
    pub path: PathBuf,
}

impl MigrationFile {
    /// Return the version string as stored in `schema_migrations`: the
    /// filename stem without the `.sql` suffix, i.e. `<timestamp>_<slug>`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use std::path::PathBuf;
    /// # use stig::migrate::discover::MigrationFile;
    /// let m = MigrationFile {
    ///     timestamp: "20240528123045".to_string(),
    ///     slug: "create_users".to_string(),
    ///     path: PathBuf::from("db/migrations/20240528123045_create_users.sql"),
    /// };
    /// assert_eq!(m.version(), "20240528123045_create_users");
    /// ```
    pub fn version(&self) -> String {
        format!("{}_{}", self.timestamp, self.slug)
    }
}

/// Scan `migrations_dir` for `.sql` files, parse filenames of the form
/// `<14-digit timestamp>_<slug>.sql`, and return them sorted lexicographically.
///
/// Files that do not match the expected pattern are skipped; a `warn!` is
/// emitted for each so it surfaces under `-v`.
///
/// Returns an error if `migrations_dir` does not exist or if two files share
/// the same timestamp.
pub fn discover(migrations_dir: &Path) -> Result<Vec<MigrationFile>> {
    let mut by_timestamp: HashMap<String, PathBuf> = HashMap::new();
    let mut migrations: Vec<MigrationFile> = Vec::new();

    let mut entries: Vec<_> = migrations_dir
        .read_dir()
        .with_context(|| {
            format!(
                "failed to read migrations directory: {}",
                migrations_dir.display()
            )
        })?
        .collect::<Result<Vec<_>, _>>()
        .with_context(|| {
            format!(
                "failed to read migrations directory: {}",
                migrations_dir.display()
            )
        })?;

    // Sort entries by filename for deterministic processing order.
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let path = entry.path();

        // Only process regular files with a `.sql` extension.
        if !path.is_file() {
            continue;
        }
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("sql") => {}
            _ => continue,
        }

        let filename = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_owned(),
            None => {
                warn!("skipping file with non-UTF-8 name: {}", path.display());
                continue;
            }
        };

        // Strip `.sql` suffix — safe because we confirmed the extension above.
        let stem = &filename[..filename.len() - 4];

        match parse_stem(stem) {
            Ok((timestamp, slug)) => {
                // Duplicate-timestamp check.
                if let Some(existing) = by_timestamp.get(&timestamp) {
                    bail!(
                        "duplicate migration timestamp {}: {} and {}",
                        timestamp,
                        existing.display(),
                        path.display()
                    );
                }
                by_timestamp.insert(timestamp.clone(), path.clone());
                migrations.push(MigrationFile {
                    timestamp,
                    slug,
                    path,
                });
            }
            Err(reason) => {
                warn!("skipping {}: {}", filename, reason);
            }
        }
    }

    // Return in lexicographic (timestamp) order.
    migrations.sort();
    Ok(migrations)
}

/// Parse a filename stem (without `.sql`) into `(timestamp, slug)`.
///
/// Expected format: `<14 digits>_<slug>` where slug matches `[a-z0-9_]+`
/// and is 1–60 characters long.
fn parse_stem(stem: &str) -> Result<(String, String), String> {
    let (ts, slug) = match stem.split_once('_') {
        Some(parts) => parts,
        None => return Err("missing underscore separator between timestamp and slug".into()),
    };

    // Validate timestamp: exactly 14 ASCII digits.
    if ts.len() != 14 || !ts.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("timestamp must be exactly 14 digits, got {:?}", ts));
    }

    // Validate slug: [a-z0-9_]+, 1–60 chars.
    if slug.is_empty() {
        return Err("slug must not be empty".into());
    }
    if slug.len() > 60 {
        return Err(format!("slug exceeds 60 characters ({})", slug.len()));
    }
    if !slug
        .chars()
        .all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
    {
        return Err(format!(
            "slug contains invalid characters (only [a-z0-9_] allowed): {:?}",
            slug
        ));
    }

    Ok((ts.to_owned(), slug.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_dir() -> TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn touch(dir: &Path, name: &str) {
        fs::write(dir.join(name), "").expect("write");
    }

    // -------------------------------------------------------------------------
    // MigrationFile::version tests
    // -------------------------------------------------------------------------

    #[test]
    fn version_combines_timestamp_and_slug() {
        let m = MigrationFile {
            timestamp: "20240528123045".to_string(),
            slug: "create_users".to_string(),
            path: PathBuf::from("20240528123045_create_users.sql"),
        };
        assert_eq!(m.version(), "20240528123045_create_users");
    }

    #[test]
    fn version_matches_filename_stem() {
        let m = MigrationFile {
            timestamp: "20260524103000".to_string(),
            slug: "add_widgets".to_string(),
            path: PathBuf::from("db/migrations/20260524103000_add_widgets.sql"),
        };
        assert_eq!(m.version(), "20260524103000_add_widgets");
    }

    // -------------------------------------------------------------------------
    // parse_stem unit tests
    // -------------------------------------------------------------------------

    #[test]
    fn parse_stem_valid() {
        let (ts, slug) = parse_stem("20240528123045_create_users").unwrap();
        assert_eq!(ts, "20240528123045");
        assert_eq!(slug, "create_users");
    }

    #[test]
    fn parse_stem_valid_numbers_in_slug() {
        let (ts, slug) = parse_stem("20240528123045_v2_add_index").unwrap();
        assert_eq!(ts, "20240528123045");
        assert_eq!(slug, "v2_add_index");
    }

    #[test]
    fn parse_stem_invalid_timestamp_too_short() {
        assert!(parse_stem("2024052812304_create_users").is_err());
    }

    #[test]
    fn parse_stem_invalid_timestamp_too_long() {
        assert!(parse_stem("202405281230450_create_users").is_err());
    }

    #[test]
    fn parse_stem_invalid_timestamp_non_digit() {
        assert!(parse_stem("2024052812304x_create_users").is_err());
    }

    #[test]
    fn parse_stem_invalid_slug_uppercase() {
        assert!(parse_stem("20240528123045_CreateUsers").is_err());
    }

    #[test]
    fn parse_stem_invalid_slug_hyphen() {
        assert!(parse_stem("20240528123045_create-users").is_err());
    }

    #[test]
    fn parse_stem_invalid_slug_empty() {
        // stem is "20240528123045_" — slug is empty after the underscore
        // but split_once gives ("20240528123045", "") so slug is empty
        assert!(parse_stem("20240528123045_").is_err());
    }

    #[test]
    fn parse_stem_slug_too_long() {
        let long_slug = "a".repeat(61);
        let stem = format!("20240528123045_{}", long_slug);
        assert!(parse_stem(&stem).is_err());
    }

    #[test]
    fn parse_stem_slug_exactly_60_chars() {
        let slug = "a".repeat(60);
        let stem = format!("20240528123045_{}", slug);
        assert!(parse_stem(&stem).is_ok());
    }

    #[test]
    fn parse_stem_no_underscore() {
        assert!(parse_stem("20240528123045").is_err());
    }

    // -------------------------------------------------------------------------
    // discover() integration tests
    // -------------------------------------------------------------------------

    #[test]
    fn discover_missing_dir_returns_error() {
        let tmp = make_dir();
        // Use a child path that was never created — guaranteed absent for the
        // lifetime of the TempDir.
        let missing = tmp.path().join("nonexistent_subdir");
        let err = discover(&missing).unwrap_err();
        assert!(
            err.to_string()
                .contains("failed to read migrations directory")
        );
    }

    #[test]
    fn discover_empty_dir_returns_empty_vec() {
        let tmp = make_dir();
        let result = discover(tmp.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn discover_valid_files_parsed_and_sorted() {
        let tmp = make_dir();
        touch(tmp.path(), "20240528120000_beta.sql");
        touch(tmp.path(), "20240528110000_alpha.sql");
        touch(tmp.path(), "20240528130000_gamma.sql");

        let result = discover(tmp.path()).unwrap();
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].timestamp, "20240528110000");
        assert_eq!(result[0].slug, "alpha");
        assert_eq!(result[1].timestamp, "20240528120000");
        assert_eq!(result[1].slug, "beta");
        assert_eq!(result[2].timestamp, "20240528130000");
        assert_eq!(result[2].slug, "gamma");
    }

    #[test]
    fn discover_non_matching_files_skipped() {
        let tmp = make_dir();
        touch(tmp.path(), "20240528120000_valid.sql");
        touch(tmp.path(), "not_a_migration.sql");
        touch(tmp.path(), "README.md");

        let result = discover(tmp.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].slug, "valid");
    }

    #[test]
    fn discover_duplicate_timestamps_returns_error() {
        let tmp = make_dir();
        touch(tmp.path(), "20240528120000_first.sql");
        touch(tmp.path(), "20240528120000_second.sql");

        let err = discover(tmp.path()).unwrap_err();
        assert!(err.to_string().contains("duplicate migration timestamp"));
    }

    #[test]
    fn discover_non_sql_files_ignored() {
        let tmp = make_dir();
        touch(tmp.path(), "20240528120000_valid.sql");
        touch(tmp.path(), "20240528120001_valid.txt");
        touch(tmp.path(), "20240528120002_valid");

        let result = discover(tmp.path()).unwrap();
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn discover_uppercase_sql_extension_is_recognized() {
        let tmp = make_dir();
        touch(tmp.path(), "20240528120000_init.SQL");

        let result = discover(tmp.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].slug, "init");
    }

    #[test]
    fn discover_mixed_case_sql_extension_is_recognized() {
        let tmp = make_dir();
        touch(tmp.path(), "20240528120000_init.Sql");

        let result = discover(tmp.path()).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].slug, "init");
    }

    #[test]
    fn discover_path_is_set_correctly() {
        let tmp = make_dir();
        touch(tmp.path(), "20240528120000_init.sql");

        let result = discover(tmp.path()).unwrap();
        assert_eq!(result[0].path, tmp.path().join("20240528120000_init.sql"));
    }
}
