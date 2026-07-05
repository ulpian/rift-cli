//! Worktree hashing for the avoidable-context metric (Stage 3 Phase 1 Step 4,
//! scoped to what the ledger needs).
//!
//! The ledger's `avoidable_context_rate` compares each read's
//! `content_sha256` against the current worktree's hash for that path. The hash
//! here MUST therefore be a **plain SHA-256 of the file bytes** — the same
//! function capture records at read time — not the domain-separated
//! `rift-hashchain` leaf and not blake3. We only hash the paths that were
//! actually read (cheap, and all the metric needs); whole-worktree hashing for
//! the code graph is a later concern.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use sha2::{Digest, Sha256};

/// Plain lowercase-hex SHA-256 of `bytes` (matches read-time `content_sha256`).
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for b in digest {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

/// The git blob object id of `bytes`: SHA-1 over the loose-object framing
/// `"blob <len>\0" ‖ bytes` — exactly what `git hash-object` produces (ADR-0005).
/// This is git's **default** (SHA-1) object hash; a SHA-256-format repo's oids
/// differ, so a `file:` handle then simply misses (honest, per the ADR), rather
/// than serving wrong bytes. The lookup is always self-validated against the
/// requested `content_sha256` before serving.
#[must_use]
pub fn git_blob_oid(bytes: &[u8]) -> String {
    // sha1::Sha1 implements the same `digest::Digest` trait re-exported by sha2,
    // already in scope — no second trait import needed.
    let mut h = sha1::Sha1::new();
    h.update(format!("blob {}\0", bytes.len()).as_bytes());
    h.update(bytes);
    let out = h.finalize();
    let mut s = String::with_capacity(40);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// One worktree file's dual identity: the plain SHA-256 content hash (the
/// validity comparator, what capture records at read time) and the git blob
/// object id (the substrate lookup key). The ADR-0005 map is built from these.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeBlob {
    /// Repo-relative path.
    pub path: String,
    /// Plain SHA-256 of the current bytes.
    pub content_sha256: String,
    /// Git blob object id (SHA-1 framing) of the current bytes.
    pub blob_oid: String,
}

/// Hash the current worktree content for `paths`, returning each existing file's
/// (`content_sha256`, `blob_oid`) — the input to both the avoidable-context
/// metric and the ADR-0005 content→blob-oid map. Same skip rules as
/// [`hash_paths`] (missing/unsafe/dir/oversize paths are omitted, never a panic).
#[must_use]
pub fn hash_blobs(repo_root: &Path, paths: &BTreeSet<String>) -> Vec<WorktreeBlob> {
    let mut out = Vec::new();
    for path in paths {
        if !path_is_safe(path) {
            continue;
        }
        let full = repo_root.join(path);
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        if !meta.is_file() {
            continue;
        }
        if meta.len() > crate::parse::MAX_ARTIFACT_BYTES as u64 {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&full) {
            out.push(WorktreeBlob {
                path: path.clone(),
                content_sha256: sha256_hex(&bytes),
                blob_oid: git_blob_oid(&bytes),
            });
        }
    }
    out
}

/// A repo-relative path is safe to join under the repo root: non-empty, not
/// absolute, no `..` segment, no NUL.
fn path_is_safe(p: &str) -> bool {
    !p.is_empty()
        && !p.starts_with('/')
        && !p.contains('\0')
        && !p.split('/').any(|seg| seg == "..")
}

/// Hash the current worktree content for exactly `paths` (repo-relative), keyed
/// by path. A path that is missing, unsafe, a directory, or over the size cap is
/// simply omitted (it then counts as "changed/absent" in the metric — never a
/// panic). The returned map is the `worktree` argument to the ledger metrics.
#[must_use]
pub fn hash_paths(repo_root: &Path, paths: &BTreeSet<String>) -> BTreeMap<String, String> {
    hash_blobs(repo_root, paths)
        .into_iter()
        .map(|b| (b.path, b.content_sha256))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_matches_known_vector() {
        // SHA-256("abc")
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hashes_only_requested_existing_paths() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello").unwrap();
        let paths: BTreeSet<String> = ["a.txt", "missing.txt"]
            .into_iter()
            .map(String::from)
            .collect();
        let map = hash_paths(dir.path(), &paths);
        assert_eq!(map.get("a.txt"), Some(&sha256_hex(b"hello")));
        assert!(!map.contains_key("missing.txt"), "absent paths are omitted");
    }

    #[test]
    fn rejects_unsafe_paths() {
        let dir = tempfile::tempdir().unwrap();
        let paths: BTreeSet<String> = ["../escape", "/abs", ""]
            .into_iter()
            .map(String::from)
            .collect();
        assert!(hash_paths(dir.path(), &paths).is_empty());
    }

    #[test]
    fn git_blob_oid_matches_git_hash_object() {
        // `printf '' | git hash-object --stdin`
        assert_eq!(
            git_blob_oid(b""),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
        // `printf 'hello\n' | git hash-object --stdin`
        assert_eq!(
            git_blob_oid(b"hello\n"),
            "ce013625030ba8dba906f756967f9e9ca394464a"
        );
        // the content hash and the blob oid are DISTINCT identities (ADR-0005).
        assert_ne!(git_blob_oid(b"hello\n"), sha256_hex(b"hello\n"));
    }

    #[test]
    fn hash_blobs_returns_both_identities() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"hello\n").unwrap();
        let paths: BTreeSet<String> = ["a.txt"].into_iter().map(String::from).collect();
        let blobs = hash_blobs(dir.path(), &paths);
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs[0].content_sha256, sha256_hex(b"hello\n"));
        assert_eq!(blobs[0].blob_oid, git_blob_oid(b"hello\n"));
    }
}
