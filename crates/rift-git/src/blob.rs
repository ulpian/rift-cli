//! Read-side git object access for diff-capture.
//!
//! Attribution needs to read file bytes at commit/turn boundaries **without a
//! checkout** — the pure diff engine in `rift-shadow-attribution` consumes byte
//! pairs, and this is the seam that produces them. [`BlobReader`] exposes exactly
//! three operations (read a blob by oid, list a commit's tree, find a commit's
//! first parent); `gix` types never cross it — ids leave as hex `String`s, blobs
//! as `Vec<u8>`. The production [`GixBlobReader`] (behind the `gix` feature) opens
//! a repo for object access only; [`MemoryBlobReader`] is the offline test double
//! so the producer can be unit-tested with no repository.

use rift_core::Result;

/// One `(path, blob_oid)` entry in a commit's tree (blobs only; full path from the
/// tree root, `/`-joined).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeEntry {
    /// Path from the tree root (e.g. `src/lib.rs`).
    pub path: String,
    /// The blob object id (lowercase hex).
    pub blob_oid: String,
}

/// Read-only git object access. The boundary between "read blobs via gix" and
/// "diff the bytes" — implemented over a real repo ([`GixBlobReader`]) or in memory
/// ([`MemoryBlobReader`]) so attribution capture stays testable offline.
pub trait BlobReader {
    /// Fetch an immutable blob by hex object id. `None` if absent or not a blob.
    ///
    /// # Errors
    /// [`rift_core::RiftError`] on a read failure.
    fn read_blob(&self, oid_hex: &str) -> Result<Option<Vec<u8>>>;

    /// Every blob `(path, oid)` reachable from the commit's tree, in path order.
    /// An unknown/invalid commit id yields an empty list (never fatal).
    ///
    /// # Errors
    /// [`rift_core::RiftError`] on a read failure.
    fn tree_entries_at(&self, commit_hex: &str) -> Result<Vec<TreeEntry>>;

    /// The blob entries of the commit's tree at exactly `paths`, in path
    /// order; a path the tree lacks, or names a tree or a submodule at, is left
    /// out. The answer equals [`BlobReader::tree_entries_at`] filtered to
    /// `paths`, which is how a reader without a cheaper lookup gives it; the
    /// production reader walks one path at a time instead of listing the whole
    /// tree, since a turn boundary asks about a handful of files.
    ///
    /// # Errors
    /// [`rift_core::RiftError`] on a read failure.
    fn tree_entries_for(&self, commit_hex: &str, paths: &[String]) -> Result<Vec<TreeEntry>> {
        let wanted: std::collections::BTreeSet<&str> = paths.iter().map(String::as_str).collect();
        let mut entries = self.tree_entries_at(commit_hex)?;
        entries.retain(|e| wanted.contains(e.path.as_str()));
        Ok(entries)
    }

    /// The first-parent commit oid (hex) of `commit_hex`, if any (a root commit
    /// has none).
    ///
    /// # Errors
    /// [`rift_core::RiftError`] on a read failure.
    fn parent_of(&self, commit_hex: &str) -> Result<Option<String>>;
}

// --- in-memory double (always available, no gix) ----------------------------

/// An in-memory [`BlobReader`] for tests: a fixed blob/tree/parent graph. Lets the
/// diff-capture producer be exercised without a real repository.
#[derive(Default)]
pub struct MemoryBlobReader {
    blobs: std::collections::HashMap<String, Vec<u8>>,
    trees: std::collections::HashMap<String, Vec<TreeEntry>>,
    parents: std::collections::HashMap<String, String>,
}

impl MemoryBlobReader {
    /// An empty reader.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a blob by its hex oid.
    #[must_use]
    pub fn with_blob(mut self, oid_hex: &str, bytes: &[u8]) -> Self {
        self.blobs.insert(oid_hex.to_owned(), bytes.to_vec());
        self
    }

    /// Register a commit's tree (its `(path, blob_oid)` entries).
    #[must_use]
    pub fn with_tree(mut self, commit_hex: &str, entries: Vec<TreeEntry>) -> Self {
        self.trees.insert(commit_hex.to_owned(), entries);
        self
    }

    /// Register a commit's first parent.
    #[must_use]
    pub fn with_parent(mut self, commit_hex: &str, parent_hex: &str) -> Self {
        self.parents
            .insert(commit_hex.to_owned(), parent_hex.to_owned());
        self
    }
}

impl BlobReader for MemoryBlobReader {
    fn read_blob(&self, oid_hex: &str) -> Result<Option<Vec<u8>>> {
        Ok(self.blobs.get(oid_hex).cloned())
    }

    fn tree_entries_at(&self, commit_hex: &str) -> Result<Vec<TreeEntry>> {
        let mut out = self.trees.get(commit_hex).cloned().unwrap_or_default();
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    fn parent_of(&self, commit_hex: &str) -> Result<Option<String>> {
        Ok(self.parents.get(commit_hex).cloned())
    }
}

// --- production gix-backed reader --------------------------------------------

#[cfg(feature = "gix")]
pub use gix_reader::GixBlobReader;

#[cfg(feature = "gix")]
mod gix_reader {
    use std::path::Path;

    use rift_core::{Result, RiftError};

    use super::{BlobReader, TreeEntry};

    /// The production [`BlobReader`]: opens a repository for **object access only**
    /// (no index, worktree, checkout, or network). `gix` types stay inside.
    pub struct GixBlobReader {
        repo: gix::Repository,
    }

    impl GixBlobReader {
        /// Open the repo at `repo_root` for read-only object access.
        ///
        /// # Errors
        /// [`RiftError::Substrate`] if the path is not a readable repository.
        pub fn open(repo_root: &Path) -> Result<Self> {
            let repo = gix::open(repo_root)
                .map_err(|e| RiftError::substrate(format!("open repo: {e}")))?;
            Ok(Self { repo })
        }

        /// The current `HEAD` commit oid (hex), if the repo has any commit.
        ///
        /// # Errors
        /// [`RiftError::Substrate`] on a read failure other than an unborn HEAD.
        pub fn head_hex(&self) -> Result<Option<String>> {
            match self.repo.head_commit() {
                Ok(c) => Ok(Some(c.id().to_string())),
                Err(_) => Ok(None), // unborn/detached-empty HEAD: nothing to capture
            }
        }

        fn commit(&self, hex: &str) -> Result<Option<gix::Commit<'_>>> {
            let Ok(oid) = gix::ObjectId::from_hex(hex.as_bytes()) else {
                return Ok(None);
            };
            match self.repo.find_object(oid) {
                Ok(obj) => obj
                    .try_into_commit()
                    .map(Some)
                    .map_err(|e| RiftError::substrate(format!("not a commit: {e}"))),
                Err(_) => Ok(None),
            }
        }
    }

    /// Recursively collect every blob `(path, oid)` under `tree`, prefixing names.
    fn collect(
        repo: &gix::Repository,
        tree: &gix::Tree<'_>,
        prefix: &str,
        out: &mut Vec<TreeEntry>,
    ) -> Result<()> {
        let decoded = tree
            .decode()
            .map_err(|e| RiftError::substrate(format!("decode tree: {e}")))?;
        for entry in &decoded.entries {
            let name = entry.filename.to_string();
            let path = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.mode.is_tree() {
                let sub = repo
                    .find_object(entry.oid)
                    .map_err(|e| RiftError::substrate(format!("find subtree: {e}")))?
                    .try_into_tree()
                    .map_err(|e| RiftError::substrate(format!("not a tree: {e}")))?;
                collect(repo, &sub, &path, out)?;
            } else if entry.mode.is_blob() {
                out.push(TreeEntry {
                    path,
                    blob_oid: entry.oid.to_string(),
                });
            }
        }
        Ok(())
    }

    impl BlobReader for GixBlobReader {
        fn read_blob(&self, oid_hex: &str) -> Result<Option<Vec<u8>>> {
            let Ok(oid) = gix::ObjectId::from_hex(oid_hex.as_bytes()) else {
                return Ok(None);
            };
            match self.repo.find_object(oid) {
                Ok(obj) => Ok(Some(obj.data.clone())),
                Err(_) => Ok(None),
            }
        }

        fn tree_entries_at(&self, commit_hex: &str) -> Result<Vec<TreeEntry>> {
            let Some(commit) = self.commit(commit_hex)? else {
                return Ok(Vec::new());
            };
            let tree = commit
                .tree()
                .map_err(|e| RiftError::substrate(format!("read tree: {e}")))?;
            let mut out = Vec::new();
            collect(&self.repo, &tree, "", &mut out)?;
            out.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(out)
        }

        fn tree_entries_for(&self, commit_hex: &str, paths: &[String]) -> Result<Vec<TreeEntry>> {
            let Some(commit) = self.commit(commit_hex)? else {
                return Ok(Vec::new());
            };
            let tree = commit
                .tree()
                .map_err(|e| RiftError::substrate(format!("read tree: {e}")))?;
            let mut out = Vec::new();
            for path in paths {
                let entry = tree
                    .lookup_entry(path.split('/').map(str::as_bytes))
                    .map_err(|e| RiftError::substrate(format!("look up tree entry: {e}")))?;
                let Some(entry) = entry else {
                    continue;
                };
                if entry.mode().is_blob() {
                    out.push(TreeEntry {
                        path: path.clone(),
                        blob_oid: entry.object_id().to_string(),
                    });
                }
            }
            out.sort_by(|a, b| a.path.cmp(&b.path));
            Ok(out)
        }

        fn parent_of(&self, commit_hex: &str) -> Result<Option<String>> {
            let Some(commit) = self.commit(commit_hex)? else {
                return Ok(None);
            };
            let parent = commit.parent_ids().next().map(|p| p.detach().to_string());
            Ok(parent)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_reader_round_trips_blobs_trees_and_parents() {
        let r = MemoryBlobReader::new()
            .with_blob("aa", b"hello")
            .with_tree(
                "c1",
                vec![
                    TreeEntry {
                        path: "src/main.rs".into(),
                        blob_oid: "aa".into(),
                    },
                    TreeEntry {
                        path: "README.md".into(),
                        blob_oid: "bb".into(),
                    },
                ],
            )
            .with_parent("c1", "c0");

        assert_eq!(r.read_blob("aa").unwrap().as_deref(), Some(&b"hello"[..]));
        assert_eq!(r.read_blob("missing").unwrap(), None);
        // entries come back in path order
        let entries = r.tree_entries_at("c1").unwrap();
        assert_eq!(entries[0].path, "README.md");
        assert_eq!(entries[1].path, "src/main.rs");
        assert_eq!(r.parent_of("c1").unwrap().as_deref(), Some("c0"));
        assert_eq!(r.parent_of("c0").unwrap(), None);
        // unknown commit → empty tree, never fatal
        assert!(r.tree_entries_at("nope").unwrap().is_empty());
    }

    /// The family plane stores canonical paths, and on Windows those carry
    /// the verbatim prefix; the seam must open a repository by that spelling
    /// too, since every caller hands it a member's stored work tree.
    #[cfg(feature = "gix")]
    #[test]
    fn gix_reader_opens_the_canonical_spelling_of_a_repository() {
        let dir = tempfile::tempdir().unwrap();
        let ok = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap()
            .success();
        assert!(ok);
        let canonical = std::fs::canonicalize(dir.path()).unwrap();
        let reader = GixBlobReader::open(&canonical).unwrap();
        assert_eq!(
            reader.head_hex().unwrap(),
            None,
            "an unborn HEAD is no commit"
        );
    }
    #[cfg(feature = "gix")]
    #[test]
    fn gix_reader_reads_a_real_commit_tree() {
        // Build a tiny real repo with two commits via the user's git, then read its
        // HEAD tree + parent + a blob through the gix seam.
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let ok = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap()
                .status
                .success();
            assert!(ok, "git {args:?}");
        };
        run(&["init", "-q"]);
        std::fs::write(dir.path().join("a.txt"), b"one\ntwo\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "c0"]);
        std::fs::write(dir.path().join("a.txt"), b"one\nTWO\n").unwrap();
        run(&["add", "."]);
        run(&["commit", "-qm", "c1"]);

        let reader = GixBlobReader::open(dir.path()).unwrap();
        let head = reader.head_hex().unwrap().expect("HEAD commit");
        let parent = reader.parent_of(&head).unwrap().expect("parent");
        assert_ne!(head, parent);

        let head_tree = reader.tree_entries_at(&head).unwrap();
        let a = head_tree
            .iter()
            .find(|e| e.path == "a.txt")
            .expect("a.txt in tree");
        assert_eq!(
            reader.read_blob(&a.blob_oid).unwrap().unwrap(),
            b"one\nTWO\n"
        );

        // The parent's a.txt differs (the pre-edit content).
        let parent_tree = reader.tree_entries_at(&parent).unwrap();
        let pa = parent_tree.iter().find(|e| e.path == "a.txt").unwrap();
        assert_eq!(
            reader.read_blob(&pa.blob_oid).unwrap().unwrap(),
            b"one\ntwo\n"
        );
    }

    #[cfg(feature = "gix")]
    #[test]
    fn looking_up_paths_one_at_a_time_agrees_with_listing_the_whole_tree() {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@e")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@e")
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}");
        };
        run(&["init", "-q"]);
        for path in ["a.txt", "src/lib.rs", "src/deep/mod.rs", "src/deep/z.rs"] {
            let file = dir.path().join(path);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, path.as_bytes()).unwrap();
        }
        run(&["add", "."]);
        run(&["commit", "-qm", "c0"]);
        let reader = GixBlobReader::open(dir.path()).unwrap();
        let head = reader.head_hex().unwrap().unwrap();
        let paths: Vec<String> = [
            "src/deep/z.rs",
            "a.txt",
            "src/deep",
            "a.txt/x",
            "missing.rs",
        ]
        .map(String::from)
        .to_vec();

        let looked_up = reader.tree_entries_for(&head, &paths).unwrap();

        let mut listed = reader.tree_entries_at(&head).unwrap();
        listed.retain(|e| paths.contains(&e.path));
        assert_eq!(looked_up, listed);
        assert_eq!(
            looked_up.len(),
            2,
            "a directory or a missing path is no blob"
        );
        let memory = MemoryBlobReader::new().with_tree("c", listed.clone());
        assert_eq!(memory.tree_entries_for("c", &paths).unwrap(), listed);
    }
}
