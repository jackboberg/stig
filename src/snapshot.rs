//! Snapshot and reset-backup management for `stig`.
//!
//! **Snapshots** are per-migration filesystem copies of the live SQLite
//! database (plus WAL/SHM sidecars) taken automatically before each migration
//! is applied.  They live in `<backups_dir>/snapshots/` and are named
//! `pre-<version>.db{,-wal,-shm}`.
//!
//! **Reset backups** are created by the explicit `reset` command.  They live
//! in `<backups_dir>/resets/` and are named `reset-<UTC-timestamp>.db{,-wal,-shm}`.
//!
//! # Sidecar handling
//!
//! SQLite WAL-mode databases may have `-wal` and `-shm` sidecar files
//! alongside the main `.db` file.  All snapshot/restore/move operations
//! attempt to include these files.  A missing sidecar (ENOENT) is silently
//! skipped; any other I/O error is surfaced and triggers rollback.
//!
//! # Caller responsibility
//!
//! The caller must run a WAL checkpoint (`Db::checkpoint`) **before** calling
//! [`take_snapshot`] or [`take_reset_backup`] to ensure the snapshot is
//! internally consistent (all WAL data flushed into the main file).

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Copy `db_path` (plus `-wal`/`-shm` sidecars) to
/// `snapshots_dir/pre-<version>.db{,-wal,-shm}`.
///
/// The caller is responsible for running a WAL checkpoint before calling this
/// function so the snapshot is internally consistent.
///
/// On partial failure (e.g. the sidecar copy errors after the main file has
/// been written) all destination files that were already written are deleted
/// before the error is returned.
pub fn take_snapshot(version: &str, db_path: &Path, snapshots_dir: &Path) -> Result<()> {
    let dest_base = snapshots_dir.join(format!("pre-{version}.db"));
    let mut written: Vec<PathBuf> = Vec::new();

    let result = (|| -> Result<()> {
        copy_file(db_path, &dest_base)?;
        written.push(dest_base.clone());

        for ext in ["-wal", "-shm"] {
            let src = sidecar(db_path, ext);
            let dst = sidecar(&dest_base, ext);
            match copy_file_if_exists(&src, &dst) {
                Ok(true) => written.push(dst),
                Ok(false) => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    })();

    if result.is_err() {
        for f in &written {
            let _ = std::fs::remove_file(f);
        }
    }

    result
}

/// Copy `snapshots_dir/pre-<version>.db{,-wal,-shm}` back to `db_path`.
///
/// **Rollback safety (Option A):** before overwriting the live database the
/// live files are moved to a temporary location.  If the restore fails
/// partway through, the originals are moved back.
///
/// Returns an error if the snapshot does not exist.
pub fn restore_snapshot(version: &str, db_path: &Path, snapshots_dir: &Path) -> Result<()> {
    let snap_base = snapshots_dir.join(format!("pre-{version}.db"));
    if !snap_base.exists() {
        bail!(
            "snapshot pre-{version}.db not found in {}",
            snapshots_dir.display()
        );
    }

    let mut pairs: Vec<(Option<PathBuf>, PathBuf)> =
        vec![(Some(snap_base.clone()), db_path.to_path_buf())];
    for ext in ["-wal", "-shm"] {
        let src = sidecar(&snap_base, ext);
        let src_opt = if src.exists() { Some(src) } else { None };
        pairs.push((src_opt, sidecar(db_path, ext)));
    }

    atomic_replace_with_rollback(&pairs)
        .context("failed to restore snapshot; original database restored")
}

/// Return `true` if `snapshots_dir/pre-<version>.db` exists.
pub fn snapshot_exists(version: &str, snapshots_dir: &Path) -> bool {
    snapshots_dir.join(format!("pre-{version}.db")).exists()
}

/// Metadata for a single backup file (snapshot or reset).
pub struct BackupEntry {
    pub filename: String,
    pub size_bytes: u64,
    pub age: Duration,
}

/// List all `<prefix>*.db` files in `dir`, returning metadata sorted
/// oldest-first.  Returns an empty vec if `dir` does not exist.
pub fn list_backups(dir: &Path, prefix: &str) -> Result<Vec<BackupEntry>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }

    let now = std::time::SystemTime::now();

    let mut entries: Vec<BackupEntry> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(prefix) && name.ends_with(".db") {
                let meta = entry.metadata().ok()?;
                let mtime = meta.modified().ok()?;
                let age = now.duration_since(mtime).unwrap_or(Duration::ZERO);
                Some(BackupEntry {
                    filename: name.into_owned(),
                    size_bytes: meta.len(),
                    age,
                })
            } else {
                None
            }
        })
        .collect();

    entries.sort_by_key(|b| std::cmp::Reverse(b.age));
    Ok(entries)
}

/// Delete the oldest `pre-*.db` snapshots (plus their sidecars) in
/// `snapshots_dir`, retaining only the `keep` most-recently-modified ones.
///
/// Sidecars (`-wal`, `-shm`) are deleted alongside the main file; a missing
/// sidecar is silently ignored.
pub fn prune_snapshots(snapshots_dir: &Path, keep: u32) -> Result<()> {
    prune_dir(snapshots_dir, "pre-", keep)
}

/// Move `db_path` (plus `-wal`/`-shm` sidecars) to
/// `resets_dir/reset-<UTC-timestamp>.db{,-wal,-shm}`.
///
/// Tries `fs::rename` first (fast, same-filesystem); falls back to
/// copy-then-delete if the rename crosses filesystem boundaries.
///
/// Returns the path of the created reset backup (the `.db` file).
///
/// Returns an error if a reset backup with the same timestamp already exists
/// (collision within one second); the caller should retry.
///
/// On partial failure all destination files are removed and the source files
/// are restored before the error is returned.
///
/// **Caller responsibility:** run a WAL checkpoint before calling this
/// function to ensure the reset backup is internally consistent.
pub fn take_reset_backup(db_path: &Path, resets_dir: &Path) -> Result<PathBuf> {
    take_reset_backup_with_clock(db_path, resets_dir, Utc::now)
}

/// Injectable-clock variant of [`take_reset_backup`].
///
/// The `now` closure is called exactly once.  Sidecar handling, rollback
/// behaviour, and return value are identical to [`take_reset_backup`].
///
/// **Caller responsibility:** run a WAL checkpoint before calling this
/// function to ensure the reset backup is internally consistent.
fn take_reset_backup_with_clock(
    db_path: &Path,
    resets_dir: &Path,
    now: impl FnOnce() -> DateTime<Utc>,
) -> Result<PathBuf> {
    let ts = now().format("%Y%m%dT%H%M%SZ");
    let dest_base = resets_dir.join(format!("reset-{ts}.db"));

    if dest_base.exists() {
        anyhow::bail!(
            "reset backup already exists: {}; wait a second and retry",
            dest_base.display()
        );
    }

    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new(); // (src, dst)

    let result = (|| -> Result<()> {
        move_file(db_path, &dest_base)?;
        moved.push((db_path.to_path_buf(), dest_base.clone()));

        for ext in ["-wal", "-shm"] {
            let src = sidecar(db_path, ext);
            let dst = sidecar(&dest_base, ext);
            if src.exists() {
                move_file(&src, &dst)?;
                moved.push((src, dst));
            }
        }
        Ok(())
    })();

    if let Err(e) = result {
        // Undo: move destination files back to their source paths.
        for (src, dst) in moved.iter().rev() {
            if dst.exists() {
                let _ = move_file(dst, src);
            }
        }
        return Err(e);
    }

    Ok(dest_base)
}

/// Delete the oldest `reset-*.db` backups (plus their sidecars) in
/// `resets_dir`, retaining only the `keep` most-recently-modified ones.
pub fn prune_resets(resets_dir: &Path, keep: u32) -> Result<()> {
    prune_dir(resets_dir, "reset-", keep)
}

/// Find the most recent `reset-*.db` backup in `resets_dir` and copy it
/// (plus `-wal`/`-shm` sidecars) back to `db_path`.
///
/// Uses the same rollback-safety pattern as [`restore_snapshot`]: before
/// overwriting the live database, any partial files at the destination are
/// moved to a temporary location and restored on failure.
///
/// Returns an error if no reset backup exists.
pub fn restore_reset_backup(db_path: &Path, resets_dir: &Path) -> Result<()> {
    let backup = most_recent_reset(resets_dir)?;

    let mut pairs: Vec<(Option<PathBuf>, PathBuf)> =
        vec![(Some(backup.clone()), db_path.to_path_buf())];
    for ext in ["-wal", "-shm"] {
        let src = sidecar(&backup, ext);
        let src_opt = if src.exists() { Some(src) } else { None };
        pairs.push((src_opt, sidecar(db_path, ext)));
    }

    atomic_replace_with_rollback(&pairs)
        .context("failed to restore reset backup; original state restored")
}

/// Return the path of the most recently modified `reset-*.db` file in
/// `resets_dir`.
fn most_recent_reset(resets_dir: &Path) -> Result<PathBuf> {
    if !resets_dir.is_dir() {
        anyhow::bail!("resets directory does not exist: {}", resets_dir.display());
    }

    let mut entries: Vec<(PathBuf, std::time::SystemTime)> = std::fs::read_dir(resets_dir)
        .with_context(|| format!("failed to read directory {}", resets_dir.display()))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("reset-") && name.ends_with(".db") {
                let mtime = entry.metadata().ok()?.modified().ok()?;
                Some((entry.path(), mtime))
            } else {
                None
            }
        })
        .collect();

    if entries.is_empty() {
        anyhow::bail!("no reset backups found in {}", resets_dir.display());
    }

    // Sort by mtime descending, take the newest.
    entries.sort_by_key(|(_, mtime)| std::cmp::Reverse(*mtime));
    Ok(entries.into_iter().next().unwrap().0)
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Return the path of a sidecar file (e.g. `app.db-wal`) for a given base.
pub(crate) fn sidecar(base: &Path, ext: &str) -> PathBuf {
    let mut s = base.as_os_str().to_os_string();
    s.push(ext);
    PathBuf::from(s)
}

/// Copy `src` to `dst`, creating or overwriting `dst`.
fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    std::fs::copy(src, dst)
        .with_context(|| format!("failed to copy {} to {}", src.display(), dst.display()))?;
    Ok(())
}

/// Copy `src` to `dst` if `src` exists.  Returns `Ok(true)` if the copy was
/// performed, `Ok(false)` if `src` was absent (ENOENT), or `Err` on other
/// failures.
fn copy_file_if_exists(src: &Path, dst: &Path) -> Result<bool> {
    match std::fs::copy(src, dst) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => {
            Err(e).with_context(|| format!("failed to copy {} to {}", src.display(), dst.display()))
        }
    }
}

/// Move a file, trying `rename` first and falling back to copy+delete.
fn move_file(src: &Path, dst: &Path) -> Result<()> {
    if std::fs::rename(src, dst).is_ok() {
        return Ok(());
    }
    // Cross-filesystem fallback.
    copy_file(src, dst)?;
    std::fs::remove_file(src)
        .with_context(|| format!("failed to remove source file after copy: {}", src.display()))
}

/// Move `path` to a sibling temp file and record `(original, temp)` in `saved`.
fn move_to_temp(path: &Path, saved: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    let tmp = path.with_extension(format!(
        "{}.stig-tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("db")
    ));
    move_file(path, &tmp)
        .with_context(|| format!("failed to move {} to temp location", path.display()))?;
    saved.push((path.to_path_buf(), tmp));
    Ok(())
}

/// Restore saved (original, temp) pairs by moving temp back to original.
/// Errors are ignored (best-effort recovery).
fn restore_saved(saved: &[(PathBuf, PathBuf)]) {
    for (orig, tmp) in saved.iter().rev() {
        if tmp.exists() {
            let _ = move_file(tmp, orig);
        }
    }
}

/// Atomically replace destination files with source files, preserving
/// originals for rollback on failure.
///
/// Each `(src, dst)` pair is processed in two phases:
/// 1. Move existing destination files to temp locations.
/// 2. If `src` is `Some`, copy it into the destination position.
///    If `src` is `None`, ensure the destination is absent (no copy).
///
/// If either phase fails, partially-written destination files are removed
/// and originals are restored. On success, temp copies are deleted.
fn atomic_replace_with_rollback(pairs: &[(Option<PathBuf>, PathBuf)]) -> Result<()> {
    // Phase 1: Move existing destination files aside.
    let mut saved: Vec<(PathBuf, PathBuf)> = Vec::new();
    let save_result = (|| -> Result<()> {
        for (_, dst) in pairs {
            if dst.exists() {
                move_to_temp(dst, &mut saved)?;
            }
        }
        Ok(())
    })();

    if let Err(e) = save_result {
        restore_saved(&saved);
        return Err(e.context("failed to move existing files aside"));
    }

    // Phase 2: Copy source files into destination positions, or ensure
    // absence when `src` is `None`.
    let copy_result = (|| -> Result<()> {
        for (src, dst) in pairs {
            if let Some(src) = src {
                copy_file(src, dst)?;
            }
        }
        Ok(())
    })();

    if let Err(e) = copy_result {
        // Remove any partially-written destination files.
        for (_, dst) in pairs {
            let _ = std::fs::remove_file(dst);
        }
        restore_saved(&saved);
        return Err(e.context("copy failed; originals restored"));
    }

    // Success — delete the temp copies.
    for (_, tmp) in &saved {
        let _ = std::fs::remove_file(tmp);
    }

    Ok(())
}

/// Shared pruning logic: retain the `keep` most-recently-modified `<prefix>*.db`
/// files in `dir`, deleting older ones (plus sidecars).
fn prune_dir(dir: &Path, prefix: &str, keep: u32) -> Result<()> {
    // Collect all matching .db files with their mtime.
    let mut entries: Vec<(PathBuf, std::time::SystemTime)> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read directory {}", dir.display()))?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(prefix) && name.ends_with(".db") {
                let mtime = entry.metadata().ok()?.modified().ok()?;
                Some((entry.path(), mtime))
            } else {
                None
            }
        })
        .collect();

    if entries.len() <= keep as usize {
        return Ok(());
    }

    // Sort oldest first.
    entries.sort_by_key(|(_, mtime)| *mtime);

    let to_delete = entries.len() - keep as usize;
    for (path, _) in entries.iter().take(to_delete) {
        std::fs::remove_file(path)
            .with_context(|| format!("failed to delete {}", path.display()))?;
        for ext in ["-wal", "-shm"] {
            let sidecar_path = sidecar(path, ext);
            match std::fs::remove_file(&sidecar_path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!("failed to delete sidecar {}", sidecar_path.display())
                    });
                }
            }
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use filetime::FileTime;
    use tempfile::TempDir;

    /// Write a file with known content and return its path.
    fn write_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, content).unwrap();
        path
    }

    /// Assert a file exists and has exactly `content`.
    fn assert_file_content(path: &Path, content: &[u8]) {
        assert!(path.exists(), "expected file to exist: {}", path.display());
        let actual = std::fs::read(path).unwrap();
        assert_eq!(actual, content, "content mismatch at {}", path.display());
    }

    /// A fake clock that returns an incrementing timestamp each time.
    /// Each call advances the clock by 1 second.
    fn fake_clock(start_epoch: i64) -> impl Fn() -> DateTime<Utc> {
        let counter = std::cell::Cell::new(start_epoch);
        move || {
            let ts = counter.get();
            counter.set(ts + 1);
            DateTime::<Utc>::from_timestamp(ts, 0).unwrap()
        }
    }

    /// A fake clock that always returns the same timestamp.
    fn frozen_clock(epoch: i64) -> impl Fn() -> DateTime<Utc> {
        move || DateTime::<Utc>::from_timestamp(epoch, 0).unwrap()
    }

    // -----------------------------------------------------------------------
    // take_snapshot
    // -----------------------------------------------------------------------

    #[test]
    fn take_snapshot_copies_main_file() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"dbcontent");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        take_snapshot("20240101000000_alpha", &db, &snaps).unwrap();

        assert_file_content(&snaps.join("pre-20240101000000_alpha.db"), b"dbcontent");
    }

    #[test]
    fn take_snapshot_copies_sidecars_when_present() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"db");
        write_file(dir.path(), "app.db-wal", b"wal");
        write_file(dir.path(), "app.db-shm", b"shm");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        take_snapshot("20240101000000_v", &db, &snaps).unwrap();

        assert_file_content(&snaps.join("pre-20240101000000_v.db"), b"db");
        assert_file_content(&snaps.join("pre-20240101000000_v.db-wal"), b"wal");
        assert_file_content(&snaps.join("pre-20240101000000_v.db-shm"), b"shm");
    }

    #[test]
    fn take_snapshot_succeeds_without_sidecars() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"db");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        // No -wal or -shm files — should not error.
        take_snapshot("20240101000000_v", &db, &snaps).unwrap();

        assert!(snaps.join("pre-20240101000000_v.db").exists());
        assert!(!snaps.join("pre-20240101000000_v.db-wal").exists());
        assert!(!snaps.join("pre-20240101000000_v.db-shm").exists());
    }

    #[test]
    fn take_snapshot_rolls_back_on_failure() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"db");
        // Point snapshots at a non-existent directory to force a failure.
        let snaps = dir.path().join("nonexistent_snapshots");

        let result = take_snapshot("20240101000000_v", &db, &snaps);

        assert!(result.is_err());
        // No partial files should have been left behind.
        assert!(!snaps.join("pre-20240101000000_v.db").exists());
    }

    // -----------------------------------------------------------------------
    // restore_snapshot
    // -----------------------------------------------------------------------

    #[test]
    fn restore_snapshot_round_trips_content() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"original");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        // Take a snapshot, then overwrite the live DB with different content.
        take_snapshot("20240101000000_v", &db, &snaps).unwrap();
        std::fs::write(&db, b"modified").unwrap();

        restore_snapshot("20240101000000_v", &db, &snaps).unwrap();

        assert_file_content(&db, b"original");
    }

    #[test]
    fn restore_snapshot_restores_sidecars() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"db");
        write_file(dir.path(), "app.db-wal", b"wal-before");
        write_file(dir.path(), "app.db-shm", b"shm-before");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        take_snapshot("20240101000000_v", &db, &snaps).unwrap();

        // Overwrite live sidecars.
        std::fs::write(sidecar(&db, "-wal"), b"wal-after").unwrap();
        std::fs::write(sidecar(&db, "-shm"), b"shm-after").unwrap();

        restore_snapshot("20240101000000_v", &db, &snaps).unwrap();

        assert_file_content(&sidecar(&db, "-wal"), b"wal-before");
        assert_file_content(&sidecar(&db, "-shm"), b"shm-before");
    }

    #[test]
    fn restore_snapshot_errors_when_snapshot_missing() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"live");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        let result = restore_snapshot("20240101000000_missing", &db, &snaps);

        assert!(result.is_err());
        // Live DB must be untouched.
        assert_file_content(&db, b"live");
    }

    #[test]
    #[cfg(unix)]
    fn restore_snapshot_rolls_back_live_db_on_copy_failure() {
        // Trigger the rollback path: live DB is moved aside successfully, but
        // the copy of the snapshot .db file fails because we've made it
        // unreadable (mode 0o000).  After the error the live DB must be
        // restored to its original content.
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"safe");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        take_snapshot("20240101000000_v", &db, &snaps).unwrap();

        // Make the snapshot .db unreadable so copy_file fails after
        // move_to_temp has already moved the live DB aside.
        let snap_db = snaps.join("pre-20240101000000_v.db");
        let mut perms = std::fs::metadata(&snap_db).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&snap_db, perms).unwrap();

        let result = restore_snapshot("20240101000000_v", &db, &snaps);

        // Restore permissions so TempDir cleanup can delete the file.
        let mut perms = std::fs::metadata(&snap_db).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&snap_db, perms).unwrap();

        assert!(
            result.is_err(),
            "expected an error due to unreadable snapshot"
        );
        // The live DB must be restored to its original content.
        assert_file_content(&db, b"safe");
    }

    // -----------------------------------------------------------------------
    // snapshot_exists
    // -----------------------------------------------------------------------

    #[test]
    fn snapshot_exists_returns_true_when_present() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"db");
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        take_snapshot("20240101000000_v", &db, &snaps).unwrap();

        assert!(snapshot_exists("20240101000000_v", &snaps));
    }

    #[test]
    fn snapshot_exists_returns_false_when_absent() {
        let dir = TempDir::new().unwrap();
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        assert!(!snapshot_exists("20240101000000_v", &snaps));
    }

    // -----------------------------------------------------------------------
    // prune_snapshots
    // -----------------------------------------------------------------------

    #[test]
    fn prune_snapshots_keeps_newest_n() {
        let dir = TempDir::new().unwrap();
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        for i in 1u8..=5 {
            let path = write_file(&snaps, &format!("pre-2024010100000{i}_v.db"), &[i]);
            filetime::set_file_mtime(&path, FileTime::from_unix_time(1_700_000_000 + i as i64, 0))
                .unwrap();
        }

        prune_snapshots(&snaps, 3).unwrap();

        let remaining: Vec<_> = std::fs::read_dir(&snaps)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".db"))
            .collect();

        assert_eq!(remaining.len(), 3);
    }

    #[test]
    fn prune_snapshots_deletes_sidecars() {
        let dir = TempDir::new().unwrap();
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        // Create 2 snapshots, the older one has sidecars.
        let old = write_file(&snaps, "pre-20240101000001_a.db", b"a");
        write_file(&snaps, "pre-20240101000001_a.db-wal", b"wal");
        write_file(&snaps, "pre-20240101000001_a.db-shm", b"shm");
        filetime::set_file_mtime(&old, FileTime::from_unix_time(1_700_000_000, 0)).unwrap();
        write_file(&snaps, "pre-20240101000002_b.db", b"b");

        prune_snapshots(&snaps, 1).unwrap();

        assert!(!snaps.join("pre-20240101000001_a.db").exists());
        assert!(!snaps.join("pre-20240101000001_a.db-wal").exists());
        assert!(!snaps.join("pre-20240101000001_a.db-shm").exists());
        assert!(snaps.join("pre-20240101000002_b.db").exists());
    }

    #[test]
    fn prune_snapshots_noop_when_count_within_keep() {
        let dir = TempDir::new().unwrap();
        let snaps = dir.path().join("snapshots");
        std::fs::create_dir(&snaps).unwrap();

        write_file(&snaps, "pre-20240101000001_a.db", b"a");
        write_file(&snaps, "pre-20240101000002_b.db", b"b");

        prune_snapshots(&snaps, 5).unwrap();

        assert!(snaps.join("pre-20240101000001_a.db").exists());
        assert!(snaps.join("pre-20240101000002_b.db").exists());
    }

    // -----------------------------------------------------------------------
    // take_reset_backup (with injectable clock)
    // -----------------------------------------------------------------------

    #[test]
    fn take_reset_backup_moves_db_to_resets_dir() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"live");
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        let clock = fake_clock(1_700_000_000);
        take_reset_backup_with_clock(&db, &resets, &clock).unwrap();

        // Source is gone.
        assert!(!db.exists());
        // One reset file created.
        let files: Vec<_> = std::fs::read_dir(&resets)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".db"))
            .collect();
        assert_eq!(files.len(), 1);
        assert_file_content(&files[0].path(), b"live");
    }

    #[test]
    fn take_reset_backup_moves_sidecars() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"db");
        write_file(dir.path(), "app.db-wal", b"wal");
        write_file(dir.path(), "app.db-shm", b"shm");
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        let clock = fake_clock(1_700_000_000);
        take_reset_backup_with_clock(&db, &resets, &clock).unwrap();

        assert!(!db.exists());
        assert!(!dir.path().join("app.db-wal").exists());
        assert!(!dir.path().join("app.db-shm").exists());

        let reset_files: Vec<_> = std::fs::read_dir(&resets)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert!(reset_files.iter().any(|n| n.ends_with(".db-wal")));
        assert!(reset_files.iter().any(|n| n.ends_with(".db-shm")));
    }

    #[test]
    fn take_reset_backup_returns_path_of_reset_file() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"db");
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        let clock = fake_clock(1_700_000_000);
        let result = take_reset_backup_with_clock(&db, &resets, &clock).unwrap();

        assert!(result.exists());
        let name = result.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("reset-"),
            "expected reset- prefix, got {name}"
        );
        assert!(name.ends_with(".db"), "expected .db suffix, got {name}");
    }

    #[test]
    fn take_reset_backup_name_contains_timestamp() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"db");
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        let clock = frozen_clock(1_700_000_000);
        let result = take_reset_backup_with_clock(&db, &resets, &clock).unwrap();
        let name = result.file_name().unwrap().to_string_lossy().to_string();

        // The frozen clock returns a known timestamp; verify it appears in the name.
        let expected_ts = DateTime::<Utc>::from_timestamp(1_700_000_000, 0)
            .unwrap()
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        assert!(
            name.contains(&expected_ts),
            "expected timestamp {expected_ts} in name {name}"
        );
    }

    #[test]
    fn take_reset_backup_errors_on_timestamp_collision() {
        let dir = TempDir::new().unwrap();
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        // Use a frozen clock so both calls produce the same destination path.
        let clock = frozen_clock(1_700_000_000);

        let db1 = write_file(dir.path(), "app1.db", b"first");
        take_reset_backup_with_clock(&db1, &resets, &clock).unwrap();

        let db2 = write_file(dir.path(), "app2.db", b"second");
        let result = take_reset_backup_with_clock(&db2, &resets, &clock);
        assert!(result.is_err(), "expected error on timestamp collision");
        let msg = format!("{:?}", result.unwrap_err());
        assert!(
            msg.contains("already exists"),
            "error should mention collision: {msg}"
        );
        // Source DB must be untouched.
        assert_file_content(&db2, b"second");
    }

    #[test]
    fn take_reset_backup_uses_clock_for_timestamp() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"db");
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        let clock = fake_clock(1_700_000_000);
        let result = take_reset_backup_with_clock(&db, &resets, &clock).unwrap();

        let name = result.file_name().unwrap().to_string_lossy().to_string();
        let expected_ts = DateTime::<Utc>::from_timestamp(1_700_000_000, 0)
            .unwrap()
            .format("%Y%m%dT%H%M%SZ")
            .to_string();
        assert!(
            name == format!("reset-{expected_ts}.db"),
            "expected name reset-{expected_ts}.db, got {name}"
        );
    }

    // -----------------------------------------------------------------------
    // prune_resets
    // -----------------------------------------------------------------------

    #[test]
    fn prune_resets_keeps_newest_n() {
        let dir = TempDir::new().unwrap();
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        for i in 1u8..=4 {
            let path = write_file(&resets, &format!("reset-202401010000{i:02}Z.db"), &[i]);
            filetime::set_file_mtime(&path, FileTime::from_unix_time(1_700_000_000 + i as i64, 0))
                .unwrap();
        }

        prune_resets(&resets, 2).unwrap();

        let remaining: Vec<_> = std::fs::read_dir(&resets)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".db"))
            .collect();
        assert_eq!(remaining.len(), 2);
    }

    // -----------------------------------------------------------------------
    // restore_reset_backup
    // -----------------------------------------------------------------------

    #[test]
    fn restore_reset_backup_round_trips_content() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"original");
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        // Simulate a reset backup: the content that was moved away.
        write_file(&resets, "reset-20240101T000000Z.db", b"original");

        // Simulate a partially-created database at the original path.
        std::fs::write(&db, b"partial").unwrap();

        restore_reset_backup(&db, &resets).unwrap();

        assert_file_content(&db, b"original");
    }

    #[test]
    fn restore_reset_backup_restores_sidecars() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"db");
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        write_file(&resets, "reset-20240101T000000Z.db", b"db-backup");
        write_file(&resets, "reset-20240101T000000Z.db-wal", b"wal-backup");
        write_file(&resets, "reset-20240101T000000Z.db-shm", b"shm-backup");

        // Partial sidecars at destination.
        std::fs::write(&db, b"partial-db").unwrap();
        std::fs::write(sidecar(&db, "-wal"), b"partial-wal").unwrap();

        restore_reset_backup(&db, &resets).unwrap();

        assert_file_content(&db, b"db-backup");
        assert_file_content(&sidecar(&db, "-wal"), b"wal-backup");
        assert_file_content(&sidecar(&db, "-shm"), b"shm-backup");
    }

    #[test]
    fn restore_reset_backup_picks_most_recent() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"partial");
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        // Two reset backups, older one first.
        let old = write_file(&resets, "reset-20240101T000000Z.db", b"old");
        filetime::set_file_mtime(&old, FileTime::from_unix_time(1_700_000_000, 0)).unwrap();
        let new = write_file(&resets, "reset-20240102T000000Z.db", b"new");
        filetime::set_file_mtime(&new, FileTime::from_unix_time(1_700_001_000, 0)).unwrap();

        restore_reset_backup(&db, &resets).unwrap();

        assert_file_content(&db, b"new");
    }

    #[test]
    fn restore_reset_backup_errors_when_no_backups() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"live");
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        let result = restore_reset_backup(&db, &resets);

        assert!(result.is_err());
        assert_file_content(&db, b"live");
    }

    #[test]
    fn restore_reset_backup_errors_when_dir_missing() {
        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"live");
        let resets = dir.path().join("nonexistent_resets");

        let result = restore_reset_backup(&db, &resets);

        assert!(result.is_err());
    }

    #[test]
    #[cfg(unix)]
    fn restore_reset_backup_rolls_back_on_copy_failure() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let db = write_file(dir.path(), "app.db", b"partial");
        let resets = dir.path().join("resets");
        std::fs::create_dir(&resets).unwrap();

        let backup = write_file(&resets, "reset-20240101T000000Z.db", b"backup-content");

        // Make the backup unreadable so copy fails.
        let mut perms = std::fs::metadata(&backup).unwrap().permissions();
        perms.set_mode(0o000);
        std::fs::set_permissions(&backup, perms).unwrap();

        let result = restore_reset_backup(&db, &resets);

        // Restore permissions so TempDir cleanup can delete.
        let mut perms = std::fs::metadata(&backup).unwrap().permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&backup, perms).unwrap();

        assert!(result.is_err());
        // The partial file at db_path should be restored.
        assert_file_content(&db, b"partial");
    }
}
