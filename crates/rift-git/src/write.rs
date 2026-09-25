//! The additive object/ref write seam and its in-memory double.

use rift_core::{RedactedBytes, Result, RiftError, SecretBytes};
use sha2::{Digest, Sha256};

use crate::plumbing::non_anchor_entries;
use crate::to_hex;

/// A git tree entry's object type + file mode. Only the two the checkpoint layout
/// uses: a `100644` regular-file blob, and a `040000` subtree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileMode {
    /// A regular file (mode `100644`, type `blob`).
    Blob,
    /// A subtree (mode `040000`, type `tree`).
    Tree,
}

impl GitFileMode {
    /// The octal mode token git's `mktree` stdin format expects.
    #[must_use]
    pub fn mode_token(self) -> &'static str {
        match self {
            Self::Blob => "100644",
            Self::Tree => "040000",
        }
    }

    /// The object-type token (`blob` / `tree`).
    #[must_use]
    pub fn type_token(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
        }
    }
}

/// One entry in a tree written by [`NativeGitWrite::write_tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeWriteEntry {
    /// The object mode/type.
    pub mode: GitFileMode,
    /// The entry name (a single path component — never contains `/`).
    pub name: String,
    /// The referenced object id (hex).
    pub oid: String,
}

/// A git author/committer identity for a deterministic checkpoint commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIdent {
    /// The identity name.
    pub name: String,
    /// The identity email.
    pub email: String,
}

impl Default for GitIdent {
    fn default() -> Self {
        // A stable, brand-free identity so checkpoint commits are byte-reproducible
        // and never leak a developer's own name/email onto the shadow branch.
        Self {
            name: "rift".to_owned(),
            email: "rift@localhost".to_owned(),
        }
    }
}

/// The spec for grafting one checkpoint (leaf) tree onto an orphan checkpoint
/// branch: merge it into the branch's current tip tree at `shard_path`, commit
/// with a deterministic identity/date, and advance the ref.
#[derive(Debug, Clone)]
pub struct OrphanCommit<'a> {
    /// The fully-qualified branch ref (e.g. `refs/heads/rift/checkpoints/v1`).
    pub branch: &'a str,
    /// The `<id[:2]>/<id[2:]>` path the checkpoint tree is grafted at (no trailing `/`).
    pub shard_path: &'a str,
    /// The checkpoint (leaf) tree oid to graft.
    pub tree_oid: &'a str,
    /// The commit message (brand-free).
    pub message: &'a str,
    /// The author/committer identity.
    pub ident: &'a GitIdent,
    /// The author/committer date, epoch **seconds** — caller-supplied for
    /// byte-reproducibility (never the wall clock).
    pub date_epoch: i64,
}

/// Additive git object/ref writes. The **durable** writer accepts only
/// [`RedactedBytes`]; the **local-only** writer accepts [`SecretBytes`] and may
/// only target a local shadow ref — the plumbing cannot durably persist raw bytes
/// (it does not compile). This is the redaction gate surviving into the write
/// layer.
///
/// The checkpoint-archive methods ([`write_structural_blob`](NativeGitWrite::write_structural_blob),
/// [`write_tree`](NativeGitWrite::write_tree),
/// [`commit_on_orphan`](NativeGitWrite::commit_on_orphan)) build the on-branch
/// shard. They never widen the redaction gate: `write_tree`/`commit_on_orphan`
/// take **object ids**, not bytes, and `write_structural_blob` accepts only
/// Rift-authored *structural* documents (checkpoint metadata / redaction report /
/// event index — counts, ids, and content-addresses, provably never captured
/// payload bytes). Captured content still enters durable storage exclusively
/// through [`write_durable_blob`](NativeGitWrite::write_durable_blob).
pub trait NativeGitWrite {
    /// Write a durable blob from already-redacted bytes; returns its object id (hex).
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn write_durable_blob(&self, body: &RedactedBytes) -> Result<String>;

    /// Write raw bytes to the **local-only** shadow ref (never a durable branch).
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn write_shadow_raw(&self, body: &SecretBytes) -> Result<()>;

    /// Write a **structural** checkpoint artifact (Rift-authored metadata / report /
    /// event index — never captured payload bytes) as a blob; returns its oid.
    /// Written byte-verbatim: these documents carry only counts, ids, and
    /// content-addresses, and are the LAYOUT/BYTE-parity surface, so they must not
    /// pass back through the entropy redactor (which would mask their hex hashes).
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn write_structural_blob(&self, bytes: &[u8]) -> Result<String>;

    /// Point a ref at an object id.
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn update_ref(&self, name: &str, oid: &str) -> Result<()>;

    /// Write a (flat) tree from its entries; returns the tree oid.
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn write_tree(&self, entries: &[TreeWriteEntry]) -> Result<String>;

    /// Graft a checkpoint tree onto the orphan checkpoint branch: merge it into the
    /// branch's current tip tree at `spec.shard_path`, commit, and advance the ref.
    /// Returns the resulting branch-tip commit oid. **Idempotent:** if the shard
    /// path is already present on the branch, this is a no-op that returns the
    /// existing tip.
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn commit_on_orphan(&self, spec: &OrphanCommit<'_>) -> Result<String>;
}

/// An in-memory [`NativeGitWrite`] for tests: records durable + structural blobs,
/// trees, and refs; keeps local-only raw writes strictly separate from durable
/// storage; and models the orphan-branch graft deterministically so callers can
/// assert layout + idempotency without a real git repo.
#[derive(Default)]
pub struct MemoryGitWrite {
    durable: std::cell::RefCell<Vec<(String, Vec<u8>)>>,
    structural: std::cell::RefCell<Vec<(String, Vec<u8>)>>,
    trees: std::cell::RefCell<Vec<(String, Vec<TreeWriteEntry>)>>,
    refs: std::cell::RefCell<Vec<(String, String)>>,
    local_raw: std::cell::RefCell<usize>,
    /// Shard paths grafted onto the branch → checkpoint tree oid (for idempotency).
    shards: std::cell::RefCell<std::collections::BTreeMap<String, String>>,
    /// The current branch tip commit oid, if the branch has been created.
    tip: std::cell::RefCell<Option<String>>,
}

impl MemoryGitWrite {
    /// Number of durable blobs written.
    #[must_use]
    pub fn durable_count(&self) -> usize {
        self.durable.borrow().len()
    }

    /// Number of structural artifact blobs written.
    #[must_use]
    pub fn structural_count(&self) -> usize {
        self.structural.borrow().len()
    }

    /// Number of trees written.
    #[must_use]
    pub fn tree_count(&self) -> usize {
        self.trees.borrow().len()
    }

    /// Number of local-only raw writes (never durable).
    #[must_use]
    pub fn local_raw_count(&self) -> usize {
        *self.local_raw.borrow()
    }

    /// The shard paths grafted onto the branch, in insertion order.
    #[must_use]
    pub fn shard_paths(&self) -> Vec<String> {
        self.shards.borrow().keys().cloned().collect()
    }

    /// The current branch tip commit oid, if any.
    #[must_use]
    pub fn branch_tip(&self) -> Option<String> {
        self.tip.borrow().clone()
    }
}

/// The deterministic content oid a memory double assigns (sha256 of bytes) — a
/// stand-in for git's own object hashing (the real oids come from the plumbing
/// impl; the double only needs stable, collision-resistant ids for assertions).
fn mem_oid(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    to_hex(&h.finalize())
}

impl NativeGitWrite for MemoryGitWrite {
    fn write_durable_blob(&self, body: &RedactedBytes) -> Result<String> {
        let bytes = body.as_slice().to_vec();
        let oid = mem_oid(&[&bytes]);
        self.durable.borrow_mut().push((oid.clone(), bytes));
        Ok(oid)
    }

    fn write_shadow_raw(&self, body: &SecretBytes) -> Result<()> {
        let _ = body.as_slice().len(); // local-only; never enters durable storage
        *self.local_raw.borrow_mut() += 1;
        Ok(())
    }

    fn write_structural_blob(&self, bytes: &[u8]) -> Result<String> {
        let oid = mem_oid(&[bytes]);
        self.structural
            .borrow_mut()
            .push((oid.clone(), bytes.to_vec()));
        Ok(oid)
    }

    fn update_ref(&self, name: &str, oid: &str) -> Result<()> {
        self.refs
            .borrow_mut()
            .push((name.to_owned(), oid.to_owned()));
        Ok(())
    }

    fn write_tree(&self, entries: &[TreeWriteEntry]) -> Result<String> {
        // A tree's oid is content-addressed over its (mode,name,oid) entries in the
        // exact order given (git sorts; the caller passes canonical order).
        let mut buf = Vec::new();
        for e in entries {
            buf.extend_from_slice(e.mode.mode_token().as_bytes());
            buf.push(b' ');
            buf.extend_from_slice(e.name.as_bytes());
            buf.push(0);
            buf.extend_from_slice(e.oid.as_bytes());
        }
        let oid = mem_oid(&[b"tree", &buf]);
        self.trees
            .borrow_mut()
            .push((oid.clone(), entries.to_vec()));
        Ok(oid)
    }

    fn commit_on_orphan(&self, spec: &OrphanCommit<'_>) -> Result<String> {
        // Content-aware idempotency (mirrors the plumbing impl): an identical
        // re-graft is a no-op; so is anchor-only drift (the checkpoint id is
        // content-addressed over the capture and deliberately EXCLUDES the
        // anchoring commit — the trailer-prediction contract — so a re-archive
        // of unchanged capture at a later commit differs only in metadata.json
        // + the content_hash.txt digest over it, and keeps the FIRST anchor).
        // Divergent capture content at the same shard path must surface.
        let existing = self.shards.borrow().get(spec.shard_path).cloned();
        if let Some(existing) = existing {
            let same = existing == spec.tree_oid || {
                let trees = self.trees.borrow();
                let entries_of = |oid: &str| {
                    trees
                        .iter()
                        .find(|(t, _)| t == oid)
                        .map(|(_, e)| non_anchor_entries(e))
                };
                entries_of(&existing).is_some_and(|a| entries_of(spec.tree_oid) == Some(a))
            };
            if same {
                return self
                    .tip
                    .borrow()
                    .clone()
                    .ok_or_else(|| RiftError::validity("branch tip missing for existing shard"));
            }
            return Err(RiftError::integrity(
                "checkpoint shard path already holds different content",
            ));
        }
        let prev = self.tip.borrow().clone();
        self.shards
            .borrow_mut()
            .insert(spec.shard_path.to_owned(), spec.tree_oid.to_owned());
        let commit = mem_oid(&[
            b"commit",
            prev.as_deref().unwrap_or("").as_bytes(),
            spec.shard_path.as_bytes(),
            spec.tree_oid.as_bytes(),
            &spec.date_epoch.to_le_bytes(),
        ]);
        *self.tip.borrow_mut() = Some(commit.clone());
        self.refs
            .borrow_mut()
            .push((spec.branch.to_owned(), commit.clone()));
        Ok(commit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durable_writer_separates_from_local_raw() {
        let w = MemoryGitWrite::default();
        // A RedactedBytes only via the sanctioned redaction gate (its sole constructor).
        let redacted = rift_shadow_redact::redact(&SecretBytes::new(b"durable bytes".to_vec()))
            .unwrap()
            .redacted;
        let oid = w.write_durable_blob(&redacted).unwrap();
        assert_eq!(oid.len(), 64);
        assert_eq!(w.durable_count(), 1);
        w.write_shadow_raw(&SecretBytes::new(b"raw local".to_vec()))
            .unwrap();
        assert_eq!(w.local_raw_count(), 1);
        // local-only writes never become durable blobs.
        assert_eq!(w.durable_count(), 1);
    }

    #[test]
    fn memory_orphan_graft_is_ordered_and_idempotent() {
        let w = MemoryGitWrite::default();
        let a = w.write_structural_blob(b"metadata-a").unwrap();
        let tree_a = w
            .write_tree(&[TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "metadata.json".into(),
                oid: a,
            }])
            .unwrap();
        let ident = GitIdent::default();
        let c1 = w
            .commit_on_orphan(&OrphanCommit {
                branch: "refs/heads/rift/checkpoints/v1",
                shard_path: "ab/cd",
                tree_oid: &tree_a,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 100,
            })
            .unwrap();
        assert_eq!(w.branch_tip().as_deref(), Some(c1.as_str()));
        assert_eq!(w.shard_paths(), vec!["ab/cd".to_string()]);
        // Re-grafting the SAME shard path is a no-op returning the same tip.
        let c1_again = w
            .commit_on_orphan(&OrphanCommit {
                branch: "refs/heads/rift/checkpoints/v1",
                shard_path: "ab/cd",
                tree_oid: &tree_a,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 100,
            })
            .unwrap();
        assert_eq!(c1_again, c1, "same shard path → idempotent no-op");
        assert_eq!(w.shard_paths().len(), 1);
    }

    #[test]
    fn memory_orphan_graft_rejects_conflicting_content() {
        let w = MemoryGitWrite::default();
        let ident = GitIdent::default();
        let graft = |tree: &str| {
            w.commit_on_orphan(&OrphanCommit {
                branch: "refs/heads/rift/checkpoints/v1",
                shard_path: "ab/cd",
                tree_oid: tree,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 1,
            })
        };
        graft("tree-a").unwrap();
        // Same path, same content → idempotent no-op.
        assert!(graft("tree-a").is_ok());
        // Same path, DIFFERENT content → a conflict, never a silent drop.
        assert!(
            graft("tree-b").is_err(),
            "conflicting shard content must error"
        );
    }
}
