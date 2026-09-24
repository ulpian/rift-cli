//! The hash-bound summary key, the append-only binding and digest stores, and
//! the validity check that is the only mint of a returnable [`ValidSummary`].
//! Kept apart from serving so the seal on that type is visible in one file.

use std::cell::RefCell;
use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The version triple that binds a summary. Any change orphans the prior summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SummaryVersions {
    /// Bumped on any extractor output change.
    pub extractor_version: u32,
    /// Bumped on a summary-policy change.
    pub policy_version: u32,
    /// Bumped on a redaction-rules change.
    pub redaction_version: u32,
}

/// Compute the summary key: domain-separated, length-prefixed SHA-256 over the
/// source content and the three versions. Changes the instant **any** component
/// changes (so a stale summary is unlookupable).
#[must_use]
pub fn summary_key(source_content: &[u8], v: SummaryVersions) -> String {
    let mut h = Sha256::new();
    h.update(b"rift.summary.v1\x00");
    h.update((source_content.len() as u64).to_le_bytes());
    h.update(source_content);
    h.update(v.extractor_version.to_le_bytes());
    h.update(v.policy_version.to_le_bytes());
    h.update(v.redaction_version.to_le_bytes());
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Content-addressed digest blob store.
pub trait DigestStore {
    /// Store bytes, returning their content-addressed ref (idempotent).
    fn put(&self, bytes: &[u8]) -> String;
    /// Fetch bytes by ref.
    fn get(&self, digest_ref: &str) -> Option<Vec<u8>>;
}

/// An in-memory digest store (tests / headless).
#[derive(Default)]
pub struct MemoryDigestStore {
    map: RefCell<BTreeMap<String, Vec<u8>>>,
}

impl DigestStore for MemoryDigestStore {
    fn put(&self, bytes: &[u8]) -> String {
        let r = sha256_hex(bytes);
        self.map.borrow_mut().insert(r.clone(), bytes.to_vec());
        r
    }
    fn get(&self, digest_ref: &str) -> Option<Vec<u8>> {
        self.map.borrow().get(digest_ref).cloned()
    }
}

/// An append-only summary binding row.
#[derive(Clone, Debug)]
pub struct SummaryBinding {
    /// The summary key (= summary id).
    pub key: String,
    /// Repo-relative path the summary is bound to.
    pub path: String,
    /// The content hash the summary is bound to.
    pub source_sha256: String,
    /// The git blob oid (here: equal to `source_sha256` for the reference store).
    pub bound_oid: String,
    /// The version triple.
    pub versions: SummaryVersions,
    /// Ref into the digest store.
    pub digest_ref: String,
}

/// An in-memory append-only summary binding store.
#[derive(Default)]
pub struct SummaryStore {
    bindings: RefCell<BTreeMap<String, SummaryBinding>>,
}

impl SummaryStore {
    /// A fresh store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind a summary: store the digest content-addressed and append the binding
    /// (idempotent — re-insertion of the same key is a no-op).
    pub fn put_summary(
        &self,
        dg: &dyn DigestStore,
        path: &str,
        source: &[u8],
        versions: SummaryVersions,
        digest: &[u8],
    ) -> String {
        let key = summary_key(source, versions);
        let digest_ref = dg.put(digest);
        let oid = sha256_hex(source);
        self.bindings
            .borrow_mut()
            .entry(key.clone())
            .or_insert(SummaryBinding {
                key: key.clone(),
                path: path.to_string(),
                source_sha256: oid.clone(),
                bound_oid: oid,
                versions,
                digest_ref,
            });
        key
    }

    fn get(&self, key: &str) -> Option<SummaryBinding> {
        self.bindings.borrow().get(key).cloned()
    }
}

/// The single returnable summary value. Its constructor is sealed (`pub(crate)`),
/// so no external code can fabricate a "valid" summary — only [`resolve`] mints
/// one, and only after re-checking the bound hash against the current worktree.
pub struct ValidSummary {
    key: String,
    digest: Vec<u8>,
    bound_oid: String,
}

impl ValidSummary {
    fn sealed(key: String, digest: Vec<u8>, bound_oid: String) -> Self {
        Self {
            key,
            digest,
            bound_oid,
        }
    }
    /// The summary key.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }
    /// The summary digest bytes (proven current).
    #[must_use]
    pub fn digest(&self) -> &[u8] {
        &self.digest
    }
    /// The bound content hash.
    #[must_use]
    pub fn bound_oid(&self) -> &str {
        &self.bound_oid
    }
}

/// The result of a validity check.
pub enum Validity {
    /// Returnable; bytes attached.
    Valid(ValidSummary),
    /// Source changed → no bytes.
    Invalid {
        /// The key checked.
        key: String,
        /// The current content hash.
        now: String,
    },
    /// Source gone / digest lost → handle-only.
    Degraded {
        /// The key checked.
        key: String,
    },
}

/// A snapshot of the current worktree: path → current content hash (git OID).
pub type WorktreeSnapshot = BTreeMap<String, String>;

/// Classify a binding against the current worktree — the **pure** validity core,
/// shared by the in-memory [`resolve`] and the durable recall.db resolver
/// (`rift-recall-ledger`). `digest` is the already-fetched summary bytes (only
/// needed for the `Valid` arm). A stale summary can never exit as `Valid`: the
/// bound-hash re-check is structural, and [`ValidSummary`]'s constructor is
/// sealed, so this is the single mint of a returnable summary.
#[must_use]
pub fn classify(
    binding: &SummaryBinding,
    snapshot: &WorktreeSnapshot,
    digest: Option<Vec<u8>>,
) -> Validity {
    let Some(current_oid) = snapshot.get(&binding.path) else {
        return Validity::Degraded {
            key: binding.key.clone(),
        };
    };
    if current_oid != &binding.bound_oid {
        return Validity::Invalid {
            key: binding.key.clone(),
            now: current_oid.clone(),
        };
    }
    match digest {
        Some(bytes) => Validity::Valid(ValidSummary::sealed(
            binding.key.clone(),
            bytes,
            binding.bound_oid.clone(),
        )),
        None => Validity::Degraded {
            key: binding.key.clone(),
        },
    }
}

/// Resolve a summary key against the current worktree (in-memory reference store).
/// A stale summary can never be returned as `Valid` — see [`classify`].
#[must_use]
pub fn resolve(
    store: &SummaryStore,
    dg: &dyn DigestStore,
    snapshot: &WorktreeSnapshot,
    key: &str,
) -> Validity {
    let Some(binding) = store.get(key) else {
        return Validity::Degraded {
            key: key.to_string(),
        };
    };
    let digest = dg.get(&binding.digest_ref);
    classify(&binding, snapshot, digest)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn versions() -> SummaryVersions {
        SummaryVersions {
            extractor_version: 1,
            policy_version: 1,
            redaction_version: 1,
        }
    }

    #[test]
    fn key_changes_when_any_component_changes() {
        let base = summary_key(b"hello", versions());
        assert_eq!(base, summary_key(b"hello", versions()), "deterministic");
        assert_ne!(base, summary_key(b"hellp", versions()), "source change");
        let mut v = versions();
        v.extractor_version = 2;
        assert_ne!(base, summary_key(b"hello", v), "extractor change");
        v = versions();
        v.policy_version = 2;
        assert_ne!(base, summary_key(b"hello", v), "policy change");
        v = versions();
        v.redaction_version = 2;
        assert_ne!(base, summary_key(b"hello", v), "redaction change");
    }

    #[test]
    fn put_summary_is_idempotent_and_digest_content_addressed() {
        let dg = MemoryDigestStore::default();
        let store = SummaryStore::new();
        let k1 = store.put_summary(&dg, "a.rs", b"src", versions(), b"3 hunks");
        let k2 = store.put_summary(&dg, "a.rs", b"src", versions(), b"3 hunks");
        assert_eq!(k1, k2);
        assert_eq!(dg.get(&sha256_hex(b"3 hunks")), Some(b"3 hunks".to_vec()));
    }

    #[test]
    fn valid_when_oid_matches_invalid_when_changed() {
        let dg = MemoryDigestStore::default();
        let store = SummaryStore::new();
        let key = store.put_summary(&dg, "a.rs", b"v1 source", versions(), b"digest-v1");
        let oid_v1 = sha256_hex(b"v1 source");

        // worktree still at v1 → Valid
        let mut snap = WorktreeSnapshot::new();
        snap.insert("a.rs".into(), oid_v1.clone());
        assert!(matches!(
            resolve(&store, &dg, &snap, &key),
            Validity::Valid(_)
        ));

        // worktree changed → Invalid (no bytes)
        snap.insert("a.rs".into(), sha256_hex(b"v2 source"));
        assert!(matches!(
            resolve(&store, &dg, &snap, &key),
            Validity::Invalid { .. }
        ));

        // path gone → Degraded
        snap.remove("a.rs");
        assert!(matches!(
            resolve(&store, &dg, &snap, &key),
            Validity::Degraded { .. }
        ));

        // unknown key → Degraded
        assert!(matches!(
            resolve(&store, &dg, &snap, "deadbeef"),
            Validity::Degraded { .. }
        ));
    }

    #[test]
    fn revert_makes_valid_again() {
        let dg = MemoryDigestStore::default();
        let store = SummaryStore::new();
        let key = store.put_summary(&dg, "a.rs", b"orig", versions(), b"d");
        let orig = sha256_hex(b"orig");
        let mut snap = WorktreeSnapshot::new();
        snap.insert("a.rs".into(), sha256_hex(b"changed"));
        assert!(matches!(
            resolve(&store, &dg, &snap, &key),
            Validity::Invalid { .. }
        ));
        snap.insert("a.rs".into(), orig); // revert
        assert!(matches!(
            resolve(&store, &dg, &snap, &key),
            Validity::Valid(_)
        ));
    }

    proptest::proptest! {
        /// The keystone invariant: a mutated source can never resolve to Valid for
        /// the original key (`05` §5.5).
        ///
        /// `(?s)` is the first half; the NEAR-MISS is the second. Two
        /// independent noise draws differ almost everywhere, which is the easy
        /// case — the source that would actually break the guard is one that differs
        /// from the bound bytes by a single newline or a trailing space, because
        /// that is what a normalizing hash would erase. `edit` assembles exactly
        /// those, so the draw reaches the class the assertion is about.
        #[test]
        fn mutated_source_makes_summary_unreturnable(
            orig in "(?s).{1,40}",
            edit in 0_u8..6,
            elsewhere in "(?s).{1,40}",
        ) {
            let mutated = match edit {
                0 => format!("{orig}\n"),
                1 => format!("\n{orig}"),
                2 => format!("{orig} "),
                3 => orig.replace('\n', ""),
                4 => orig.replace('\n', "\r\n"),
                _ => elsewhere,
            };
            proptest::prop_assume!(orig != mutated);
            let dg = MemoryDigestStore::default();
            let store = SummaryStore::new();
            let key = store.put_summary(&dg, "f", orig.as_bytes(), versions(), b"d");
            let mut snap = WorktreeSnapshot::new();
            snap.insert("f".into(), sha256_hex(mutated.as_bytes()));
            proptest::prop_assert!(!matches!(resolve(&store, &dg, &snap, &key), Validity::Valid(_)));
        }
    }
}
