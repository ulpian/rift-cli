//! Reading the two directories a collection walks.
//!
//! Its own module because every one of these is a rule about what a sweep may
//! touch, and the rules are the same from whichever side a collection comes:
//! a name this store did not mint is not its to remove, and a file whose age
//! the filesystem will not report is not old.

use std::path::{Path, PathBuf};

use super::key::is_sha256;
use super::store::SIDECAR_EXT;
use super::window::WINDOW_DIR;

/// The `(key, path)` of every sidecar in `dir` — and only the sidecars: a name
/// this module did not mint is not one, whatever its extension.
pub(super) fn sidecars(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            let key = name.strip_suffix(&format!(".{SIDECAR_EXT}"))?;
            is_sha256(key).then(|| (key.to_owned(), path.clone()))
        })
        .collect()
}

/// Whether `path` was last modified before `cutoff` (epoch ms). A time the
/// filesystem will not report is **not** old.
pub(super) fn older_than(path: &Path, cutoff_ms: i64) -> bool {
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
        return false;
    };
    let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    i64::try_from(since.as_millis()).unwrap_or(i64::MAX) < cutoff_ms
}

/// Remove the temp files of writes that died before their rename.
pub(super) fn sweep_tmps(dir: &Path, cutoff_ms: i64, dry_run: bool) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let marker = format!(".{SIDECAR_EXT}.tmp");
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_tmp = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.contains(&marker));
        if !is_tmp || !older_than(&path, cutoff_ms) {
            continue;
        }
        removed += 1;
        if !dry_run {
            let _ = std::fs::remove_file(&path);
        }
    }
    removed
}

/// Remove the window directories of sessions that ended longer ago than the
/// TTL, and say how many went.
///
/// A window's markers are the memory of what one conversation was told, and a
/// conversation that has been over for a month is not going to be told
/// anything more. Directories only, named the way
/// [`crate::window_journal::key`] names one, and a name this module did not
/// write is left alone.
pub(super) fn sweep_windows(dir: &Path, cutoff_ms: i64, dry_run: bool) -> usize {
    let Ok(entries) = std::fs::read_dir(dir.join(WINDOW_DIR)) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let named = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(is_sha256);
        if !named || !path.is_dir() || !older_than(&path, cutoff_ms) {
            continue;
        }
        removed += 1;
        if !dry_run {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
    removed
}
