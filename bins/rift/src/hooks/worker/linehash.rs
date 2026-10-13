//! Building the line-hash cache the read gate matches terminal output
//! against, and collecting it.
//!
//! Apart from the gate because this is the writing half: the detached worker
//! writes a revision's line hashes after a read and catches up at condense,
//! and the hook only ever reads them. Every file is a copy keyed by the bytes'
//! own SHA-256, so a write is idempotent and a collected file costs one hash
//! pass on the next hook that wants it.

use std::path::Path;

use rift_agent::window_journal::output_match::{
    encode_line_hashes, file_line_hashes, MAX_LINE_HASH_FILE_BYTES,
};
use rift_agent::window_journal::Line;

use crate::hooks::gate::{linehash_cache_dir, linehash_cache_name};

/// The largest revision cached: the gate's own inline-hash ceiling, past
/// which a hook never hashes a file and so never matches against it.
const MAX_CACHED_SOURCE_BYTES: usize = 1024 * 1024;

/// The most revisions one condense catches up on, so a long session costs a
/// bounded pass.
const MAX_CATCH_UP: usize = 256;

/// How long a cache file is kept after it was last written.
pub(in crate::hooks) const LINEHASH_TTL_MS: i64 = 7 * 86_400_000;

/// The most cache files a family keeps; the oldest beyond it are collected
/// first.
const MAX_CACHE_FILES: usize = 8192;

/// Write the line hashes of `bytes`, the revision `content_sha256`, unless a
/// file already holds them or the bytes are not lines a hook would match.
/// Says whether a file was written.
///
/// The caller vouches that the bytes hash to `content_sha256`: the name is
/// the claim, and a reader checks only the counts against it.
///
/// # Errors
/// An I/O failure creating the directory or writing the file.
pub(in crate::hooks) fn cache_line_hashes(
    common_dir: &Path,
    content_sha256: &str,
    bytes: &[u8],
) -> std::io::Result<bool> {
    if bytes.len() > MAX_CACHED_SOURCE_BYTES || bytes.contains(&0) {
        return Ok(false);
    }
    let dir = linehash_cache_dir(common_dir);
    let file = dir.join(linehash_cache_name(content_sha256));
    if file.is_file() {
        return Ok(false);
    }
    let byte_count = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    let encoded = encode_line_hashes(&file_line_hashes(bytes), byte_count);
    if encoded.len() > MAX_LINE_HASH_FILE_BYTES {
        return Ok(false);
    }
    std::fs::create_dir_all(&dir)?;
    let staged = dir.join(format!(".{content_sha256}.{}.tmp", std::process::id()));
    std::fs::write(&staged, encoded)?;
    std::fs::rename(&staged, &file)?;
    Ok(true)
}

/// Cache the line hashes of every revision the session's own window was
/// shown or read whose bytes are still on disk: the catch-up at condense for
/// the files no read job saw, which are the ones a terminal result carried.
///
/// Bounded by [`MAX_CATCH_UP`] revisions; each one is read once and kept
/// only where it still hashes to the revision the journal names. Returns how
/// many files were written.
pub(in crate::hooks) fn cache_window_line_hashes(
    work_tree: &Path,
    common_dir: &Path,
    session: &str,
) -> usize {
    let loaded = crate::window_journal::WindowJournal::open(work_tree, session, None).load();
    let mut seen = std::collections::BTreeSet::new();
    let mut written = 0usize;
    for line in loaded.journal.lines() {
        let (path, hash) = match line {
            Line::Matched { path, hash, .. } | Line::Read { path, hash, .. } => (path, hash),
            _ => continue,
        };
        if seen.len() >= MAX_CATCH_UP {
            break;
        }
        if !seen.insert((path.clone(), hash.to_hex())) {
            continue;
        }
        let Some(bytes) = revision_on_disk(work_tree, path, *hash) else {
            continue;
        };
        if cache_line_hashes(common_dir, &hash.to_hex(), &bytes).unwrap_or(false) {
            written = written.saturating_add(1);
        }
    }
    written
}

/// The bytes at `rel_path` under the work tree, only while they still hash
/// to `hash` and are small enough to cache.
fn revision_on_disk(
    work_tree: &Path,
    rel_path: &str,
    hash: rift_core::Sha256Id,
) -> Option<Vec<u8>> {
    let file = work_tree.join(rel_path);
    let meta = std::fs::symlink_metadata(&file).ok()?;
    if !meta.is_file() || meta.len() > MAX_CACHED_SOURCE_BYTES as u64 {
        return None;
    }
    let bytes = std::fs::read(&file).ok()?;
    (rift_agent::content_sha256(&bytes) == hash).then_some(bytes)
}

/// Remove the cache files written before `now_ms - ttl_ms`, then the oldest
/// past [`MAX_CACHE_FILES`], and say how many went.
///
/// Age and count are safe bounds because every file is a copy the next hook
/// can rebuild from the bytes it already holds. Only names this module
/// writes are touched — a cache file or a staged temporary — and only in the
/// line-hash directory; a file whose age cannot be read is kept.
pub(in crate::hooks) fn sweep_linehash_cache(common_dir: &Path, now_ms: i64, ttl_ms: i64) -> usize {
    let cutoff = now_ms.saturating_sub(ttl_ms);
    let Ok(entries) = std::fs::read_dir(linehash_cache_dir(common_dir)) else {
        return 0;
    };
    let mut kept: Vec<(i64, std::path::PathBuf)> = Vec::new();
    let mut removed = 0usize;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_cache_file_name(&name) {
            continue;
        }
        let path = entry.path();
        let Some(modified_ms) = modified_ms(&path) else {
            continue;
        };
        if modified_ms < cutoff {
            if std::fs::remove_file(&path).is_ok() {
                removed = removed.saturating_add(1);
            }
            continue;
        }
        kept.push((modified_ms, path));
    }
    if kept.len() > MAX_CACHE_FILES {
        kept.sort();
        let excess = kept.len() - MAX_CACHE_FILES;
        for (_, path) in kept.iter().take(excess) {
            if std::fs::remove_file(path).is_ok() {
                removed = removed.saturating_add(1);
            }
        }
    }
    removed
}

/// When a file was last written, in milliseconds, or `None` when that cannot
/// be read or the name is not a plain file.
fn modified_ms(path: &Path) -> Option<i64> {
    std::fs::symlink_metadata(path)
        .ok()
        .filter(std::fs::Metadata::is_file)
        .and_then(|meta| meta.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_millis()).ok())
}

/// Whether a name is one this module writes: `<sha256>.lh`, or a staged
/// `.<sha256>.<pid>.tmp`.
fn is_cache_file_name(name: &str) -> bool {
    let is_hex = |s: &str| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit());
    if let Some(staged) = name.strip_prefix('.').and_then(|n| n.strip_suffix(".tmp")) {
        return staged
            .split_once('.')
            .is_some_and(|(sha, pid)| is_hex(sha) && pid.parse::<u32>().is_ok());
    }
    name.strip_suffix(".lh").is_some_and(is_hex)
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::output_match::decode_line_hashes;

    use super::*;

    /// ★ A revision's line hashes are written once under its content
    /// address and read back whole; bytes that are not lines are not cached.
    #[test]
    fn a_revision_is_cached_once_under_its_content_address() {
        let tmp = tempfile::tempdir().unwrap();
        let bytes = b"fn alpha() {}\nfn beta() {}\n";
        let sha = rift_agent::content_sha256(bytes).to_hex();
        assert!(cache_line_hashes(tmp.path(), &sha, bytes).unwrap());
        assert!(!cache_line_hashes(tmp.path(), &sha, bytes).unwrap());
        let file = linehash_cache_dir(tmp.path()).join(linehash_cache_name(&sha));
        let read = std::fs::read(file).unwrap();
        assert_eq!(
            decode_line_hashes(&read, 2, 27),
            Some(file_line_hashes(bytes))
        );
        let binary_sha = rift_agent::content_sha256(b"a\0b").to_hex();
        assert!(!cache_line_hashes(tmp.path(), &binary_sha, b"a\0b").unwrap());
    }

    /// ★ The condense catch-up caches what the window was shown while the
    /// bytes still hash to it, and skips a file that has moved on.
    #[test]
    fn the_catch_up_caches_only_revisions_still_on_disk() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        rift_git::test_support::fake_repo(root).unwrap();
        std::fs::write(root.join("kept.rs"), "one\ntwo\n").unwrap();
        std::fs::write(root.join("moved.rs"), "now\n").unwrap();
        let kept = rift_agent::content_sha256(b"one\ntwo\n");
        let moved = rift_agent::content_sha256(b"then\n");
        let journal = crate::window_journal::WindowJournal::open(root, "s-1", None);
        for (path, hash) in [("kept.rs", kept), ("moved.rs", moved)] {
            journal
                .append(&Line::Matched {
                    turn: 1,
                    path: path.to_owned(),
                    hash,
                    runs: vec![rift_agent::window_journal::Interval::new(1, 1).unwrap()],
                    total_lines: 2,
                    member: 0,
                    at_ms: 1,
                })
                .unwrap();
        }
        let common = root.join(".git");
        assert_eq!(cache_window_line_hashes(root, &common, "s-1"), 1);
        let dir = linehash_cache_dir(&common);
        assert!(dir.join(linehash_cache_name(&kept.to_hex())).is_file());
        assert!(!dir.join(linehash_cache_name(&moved.to_hex())).exists());
    }

    /// The sweep collects old cache files and nothing it did not write.
    #[test]
    fn the_sweep_collects_old_cache_files_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        let sha = rift_agent::content_sha256(b"x\n").to_hex();
        assert!(cache_line_hashes(tmp.path(), &sha, b"x\n").unwrap());
        let dir = linehash_cache_dir(tmp.path());
        std::fs::write(dir.join("notes.txt"), "keep").unwrap();
        let now = modified_ms(&dir.join(linehash_cache_name(&sha))).unwrap();
        assert_eq!(sweep_linehash_cache(tmp.path(), now, LINEHASH_TTL_MS), 0);
        assert_eq!(
            sweep_linehash_cache(tmp.path(), now + LINEHASH_TTL_MS + 1, LINEHASH_TTL_MS),
            1
        );
        assert!(dir.join("notes.txt").is_file());
        assert!(is_cache_file_name(&format!(".{sha}.42.tmp")));
        assert!(!is_cache_file_name("abc.lh"));
    }
}
