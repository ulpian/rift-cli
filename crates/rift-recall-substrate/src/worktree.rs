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
    let mut out = BTreeMap::new();
    for path in paths {
        if !path_is_safe(path) {
            continue;
        }
        let full = repo_root.join(path);
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        // Regular files only: never follow a symlink out of the worktree.
        if !meta.is_file() {
            continue;
        }
        if meta.len() > crate::parse::MAX_ARTIFACT_BYTES as u64 {
            continue;
        }
        if let Ok(bytes) = std::fs::read(&full) {
            out.insert(path.clone(), sha256_hex(&bytes));
        }
    }
    out
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
}
