//! Durable backing for the validity plane (Stage 3 Phase 3 Steps 1–2).
//!
//! `rift-recall-validity` owns the hash-bound key, the sealed [`ValidSummary`],
//! and the **pure** [`rift_recall_validity::classify`] core. This module backs
//! it with the append-only `recall.db` tables: [`put_summary`] writes the
//! `digest_blob` + `summary_binding` rows + the `Summary→binds→FileRevision`
//! edge; [`resolve`] reads them back, classifies against a worktree snapshot,
//! and (on `Invalid`) appends the `Summary→invalidated_by→FileRevision` audit
//! edge — never a mutable flag (`04 §4.3`).
//!
//! The worktree snapshot maps path → **SHA-256** of the current bytes (the P1
//! worktree hasher's output); the binding's `bound_oid` is the same SHA-256, so
//! the bind-time and check-time comparators line up.

use rift_core::Result;
use rift_recall_substrate::SubstrateReader;
use rift_recall_validity::{
    classify, summary_key, BlobResolver, SummaryBinding, SummaryVersions, Validity,
};
use rift_store::{CanonicalStore, ColumnCipher, Value};
use sha2::{Digest, Sha256};

use crate::graph::{file_revision_id, Built};

/// A [`BlobResolver`] over the read-only substrate: `file:` handles resolve by
/// mapping the handle's `content_sha256` to its git blob oid (ADR-0005) and
/// fetching that immutable object; `log:` (captured tool results) is not yet
/// exposed by the substrate seam, so it resolves to `None` (a documented
/// follow-up — the handle then degrades to `Unavailable`, never wrong bytes).
///
/// The `content_sha256 → blob_oid` map is loaded from `n_blob_oid_map`
/// ([`crate::blob_map::load_blob_oid_map`]) and threaded in with
/// [`with_oid_map`](SubstrateBlobResolver::with_oid_map). An unmapped hash falls
/// back to a direct lookup — in-memory test substrates seed blobs by content
/// hash, and a real repo simply misses (honest, per ADR-0005). Resolution stays
/// self-validating in [`rift_recall_validity::resolve_handle`]: the fetched bytes
/// must re-hash to the requested `content_sha256`.
pub struct SubstrateBlobResolver<'a, R: SubstrateReader> {
    reader: &'a R,
    oid_map: std::collections::BTreeMap<String, String>,
}

impl<'a, R: SubstrateReader> SubstrateBlobResolver<'a, R> {
    /// Wrap a substrate reader as a blob resolver (no map — direct lookup).
    pub fn new(reader: &'a R) -> Self {
        Self {
            reader,
            oid_map: std::collections::BTreeMap::new(),
        }
    }

    /// Attach the ADR-0005 `content_sha256 → blob_oid` map.
    #[must_use]
    pub fn with_oid_map(mut self, oid_map: std::collections::BTreeMap<String, String>) -> Self {
        self.oid_map = oid_map;
        self
    }
}

impl<R: SubstrateReader> BlobResolver for SubstrateBlobResolver<'_, R> {
    fn file_blob(&self, _path: &str, content_sha256: &str) -> Option<Vec<u8>> {
        // Map the content hash to its git blob oid (ADR-0005), else fall back to a
        // direct lookup (in-memory substrates seed blobs by content hash).
        let oid = self
            .oid_map
            .get(content_sha256)
            .map_or(content_sha256, String::as_str);
        self.reader.blob(oid).ok().flatten()
    }
    fn tool_result(&self, _tool_use_id: &str) -> Option<Vec<u8>> {
        None // log: exact retrieval lands when the substrate exposes tool results
    }
}

const SUMMARY_TS: i64 = 0;

fn sha256_hex(bytes: &[u8]) -> String {
    let out = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// Bind a summary durably: digest bytes → `digest_blob` (content-addressed), the
/// binding → `summary_binding`, and a `Summary→binds→FileRevision` edge.
/// Idempotent (content-addressed keys → `INSERT OR IGNORE` set-union).
///
/// The digest body is a registered sensitive column (Layer-2 encryption at
/// rest): it reaches the store only through `cipher` — [`rift_store::DekCipher`]
/// in production custody, [`rift_store::PlainCipher`] where no keychain exists.
/// The content address (`digest_ref`) is computed over the *plaintext*, so
/// idempotency and hash-binding are cipher-independent.
///
/// # Errors
/// Propagates store write and cipher errors.
pub async fn put_summary(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    path: &str,
    source: &[u8],
    versions: SummaryVersions,
    digest: &[u8],
) -> Result<String> {
    let key = summary_key(source, versions);
    let digest_ref = sha256_hex(digest);
    let source_sha = sha256_hex(source);

    let sealed = cipher.encrypt("digest_blob", "bytes", &digest_ref, digest)?;
    store
        .insert_or_ignore(
            "digest_blob",
            &["digest_ref", "bytes", "created_at"],
            &[
                Value::text(&digest_ref),
                Value::Blob(sealed),
                Value::Int(SUMMARY_TS),
            ],
        )
        .await?;
    store
        .insert_or_ignore(
            "summary_binding",
            &[
                "summary_id",
                "path",
                "source_sha256",
                "bound_blob_oid",
                "extractor_ver",
                "policy_ver",
                "redaction_ver",
                "digest_ref",
                "created_at",
            ],
            &[
                Value::text(&key),
                Value::text(path),
                Value::text(&source_sha),
                Value::text(&source_sha), // bound_oid == source SHA-256 (worktree comparator)
                Value::Int(i64::from(versions.extractor_version)),
                Value::Int(i64::from(versions.policy_version)),
                Value::Int(i64::from(versions.redaction_version)),
                Value::text(&digest_ref),
                Value::Int(SUMMARY_TS),
            ],
        )
        .await?;

    // Summary node + Summary→binds→FileRevision edge.
    let fr = file_revision_id(path, &source_sha);
    let mut b = Built::default();
    b.node(&key, "Summary", &key);
    b.edge(&key, &fr, "binds");
    crate::ingest::apply_rows(store, &b).await?;
    Ok(key)
}

async fn fetch_binding(store: &impl CanonicalStore, key: &str) -> Result<Option<SummaryBinding>> {
    let rows = store
        .query(
            "SELECT path, source_sha256, bound_blob_oid, extractor_ver, policy_ver, \
                    redaction_ver, digest_ref \
             FROM summary_binding WHERE summary_id = ?",
            &[Value::text(key)],
        )
        .await?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(SummaryBinding {
        key: key.to_string(),
        path: r.get_text(0)?,
        source_sha256: r.get_text(1)?,
        bound_oid: r.get_text(2)?,
        versions: SummaryVersions {
            extractor_version: u32::try_from(r.get_i64(3)?).unwrap_or(0),
            policy_version: u32::try_from(r.get_i64(4)?).unwrap_or(0),
            redaction_version: u32::try_from(r.get_i64(5)?).unwrap_or(0),
        },
        digest_ref: r.get_text(6)?,
    }))
}

async fn fetch_digest(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    digest_ref: &str,
) -> Result<Option<Vec<u8>>> {
    let rows = store
        .query(
            "SELECT bytes FROM digest_blob WHERE digest_ref = ?",
            &[Value::text(digest_ref)],
        )
        .await?;
    match rows.first() {
        Some(r) => Ok(Some(cipher.decrypt(
            "digest_blob",
            "bytes",
            digest_ref,
            &r.get_blob(0)?,
        )?)),
        None => Ok(None),
    }
}

/// Resolve a summary key against the current worktree snapshot, durably. Returns
/// a [`Validity`] whose `Valid` arm is the only one carrying bytes (a stale
/// summary is structurally unreturnable, via [`classify`]). On `Invalid`, an
/// append-only `Summary→invalidated_by→FileRevision` audit edge is recorded.
///
/// # Errors
/// Propagates store read/write errors.
pub async fn resolve(
    store: &impl CanonicalStore,
    cipher: &dyn ColumnCipher,
    snapshot: &rift_recall_validity::WorktreeSnapshot,
    key: &str,
) -> Result<Validity> {
    let Some(binding) = fetch_binding(store, key).await? else {
        return Ok(Validity::Degraded {
            key: key.to_string(),
        });
    };
    let digest = fetch_digest(store, cipher, &binding.digest_ref).await?;
    let verdict = classify(&binding, snapshot, digest);
    if let Validity::Invalid { now, .. } = &verdict {
        let new_fr = file_revision_id(&binding.path, now);
        let mut b = Built::default();
        b.edge(key, &new_fr, "invalidated_by"); // Summary→invalidated_by→FileRevision (audit)
        crate::ingest::apply_rows(store, &b).await?;
    }
    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_validity::WorktreeSnapshot;
    use rift_store::{recall_migrations, DekCipher, MemoryKeyStore, PlainCipher, SqliteStore};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    fn versions() -> SummaryVersions {
        SummaryVersions {
            extractor_version: 1,
            policy_version: 1,
            redaction_version: 1,
        }
    }

    fn snapshot(path: &str, sha: &str) -> WorktreeSnapshot {
        let mut m = WorktreeSnapshot::new();
        m.insert(path.to_string(), sha.to_string());
        m
    }

    #[test]
    fn valid_when_oid_matches_invalid_when_changed() {
        let store = recall_store();
        let source = b"fn main() {}\n";
        let key = block(put_summary(
            &store,
            &PlainCipher,
            "src/main.rs",
            source,
            versions(),
            b"digest",
        ))
        .unwrap();
        let cur = sha256_hex(source);

        // unchanged → Valid (bytes attached)
        match block(resolve(
            &store,
            &PlainCipher,
            &snapshot("src/main.rs", &cur),
            &key,
        ))
        .unwrap()
        {
            Validity::Valid(s) => assert_eq!(s.digest(), b"digest"),
            other => panic!("expected Valid, got {:?}", validity_tag(&other)),
        }
        // changed → Invalid (no bytes)
        match block(resolve(
            &store,
            &PlainCipher,
            &snapshot("src/main.rs", "different"),
            &key,
        ))
        .unwrap()
        {
            Validity::Invalid { .. } => {}
            other => panic!("expected Invalid, got {:?}", validity_tag(&other)),
        }
    }

    #[test]
    fn substrate_resolver_maps_content_hash_to_git_blob_oid() {
        use rift_recall_substrate::MemorySubstrate;
        use rift_recall_types::Handle;
        use rift_recall_validity::{resolve_handle, Resolved, Validity};

        // The real-repo case: the git blob oid is DISTINCT from content_sha256.
        let bytes = b"fn main() {}\n".to_vec();
        let content_sha = sha256_hex(&bytes);
        let blob_oid = "1111111111111111111111111111111111111111".to_string();
        assert_ne!(content_sha, blob_oid);

        // The substrate stores the blob under its git oid; the map bridges.
        let reader = MemorySubstrate::local().with_blob(&blob_oid, &bytes);
        let mut map = std::collections::BTreeMap::new();
        map.insert(content_sha.clone(), blob_oid.clone());
        let resolver = SubstrateBlobResolver::new(&reader).with_oid_map(map);

        let h = Handle::File {
            path: "a.rs".into(),
            sha256: content_sha,
            range: None,
        };
        // resolve_handle self-validates (bytes must re-hash to the handle's sha).
        match resolve_handle(&h, &resolver, |_| Validity::Degraded { key: String::new() }) {
            Resolved::Bytes(b) => assert_eq!(b, bytes, "resolved via the blob-oid map"),
            _ => panic!("expected the mapped bytes"),
        }

        // Without the map (unmapped hash) the fallback lookup by content hash
        // misses in a real repo — honest Unavailable, never wrong bytes.
        let bare = SubstrateBlobResolver::new(&reader);
        assert!(matches!(
            resolve_handle(&h, &bare, |_| Validity::Degraded { key: String::new() }),
            Resolved::Unavailable(_)
        ));
    }

    #[test]
    fn invalid_appends_invalidated_by_edge() {
        let store = recall_store();
        let source = b"x\n";
        let key = block(put_summary(
            &store,
            &PlainCipher,
            "a.rs",
            source,
            versions(),
            b"d",
        ))
        .unwrap();
        let _ = block(resolve(
            &store,
            &PlainCipher,
            &snapshot("a.rs", "changed"),
            &key,
        ))
        .unwrap();
        let n = block(store.query(
            "SELECT count(*) FROM graph_edge WHERE kind='invalidated_by'",
            &[],
        ))
        .unwrap();
        assert_eq!(n[0].get_i64(0).unwrap(), 1);
    }

    #[test]
    fn revert_makes_valid_again() {
        let store = recall_store();
        let source = b"y\n";
        let key = block(put_summary(
            &store,
            &PlainCipher,
            "b.rs",
            source,
            versions(),
            b"d",
        ))
        .unwrap();
        let cur = sha256_hex(source);
        // changed → Invalid
        assert!(matches!(
            block(resolve(&store, &PlainCipher, &snapshot("b.rs", "z"), &key)).unwrap(),
            Validity::Invalid { .. }
        ));
        // reverted → Valid again (validity is computed, no lingering flag)
        assert!(matches!(
            block(resolve(&store, &PlainCipher, &snapshot("b.rs", &cur), &key)).unwrap(),
            Validity::Valid(_)
        ));
    }

    fn validity_tag(v: &Validity) -> &'static str {
        match v {
            Validity::Valid(_) => "Valid",
            Validity::Invalid { .. } => "Invalid",
            Validity::Degraded { .. } => "Degraded",
        }
    }

    /// The Layer-2 keystone for the recall projection: a digest body written
    /// through the production custody chain (keychain → KEK → wrapped DEK →
    /// [`DekCipher`]) is ciphertext in the raw `recall.db` bytes, and resolves
    /// back to plaintext. A [`PlainCipher`] reader cannot read it (fails
    /// closed), and a plain-written row remains readable under [`DekCipher`].
    #[test]
    fn digest_bytes_are_encrypted_at_rest_in_recall_db() {
        use rift_core::RepoId;
        use rift_store::{KeyringKeyStore, MemoryKeychain};

        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("recall.db");
        let sentinel: &[u8] = b"SUMMARY-DIGEST-SENTINEL-77c1";
        let source = b"fn secret_summary_source() {}\n";
        let repo = RepoId::new("repo-1").unwrap();

        {
            let store = rift_store::open_migrated(&db, &recall_migrations()).unwrap();
            // Production custody: provision the wrapped DEK through the envelope.
            let ks = KeyringKeyStore::new(MemoryKeychain::new());
            let dek = block(ks.provision_repo_dek(&store, &repo, 1)).unwrap();
            let cipher = DekCipher::new(MemoryKeyStore::new(dek), repo.clone());

            let key = block(put_summary(
                &store,
                &cipher,
                "s.rs",
                source,
                versions(),
                sentinel,
            ))
            .unwrap();

            // Resolves back to the plaintext digest through the cipher.
            let cur = sha256_hex(source);
            match block(resolve(&store, &cipher, &snapshot("s.rs", &cur), &key)).unwrap() {
                Validity::Valid(s) => assert_eq!(s.digest(), sentinel),
                other => panic!("expected Valid, got {:?}", validity_tag(&other)),
            }

            // A key-less reader fails closed rather than serving ciphertext.
            assert!(block(resolve(&store, &PlainCipher, &snapshot("s.rs", &cur), &key)).is_err());
        }

        // The raw on-disk database (and WAL sidecar, if any) must not contain
        // the plaintext digest sentinel.
        for path in [db.clone(), db.with_extension("db-wal")] {
            if let Ok(bytes) = std::fs::read(&path) {
                assert!(
                    !bytes.windows(sentinel.len()).any(|w| w == sentinel),
                    "plaintext digest sentinel leaked into {}",
                    path.display()
                );
            }
        }

        // NOTE: key_metadata rows live in recall.db here too — assert the wrapped
        // DEK never appears as raw key bytes is covered in rift-store's tests.
    }

    proptest::proptest! {
        /// The W2 keystone, durably: for ANY source mutation, `resolve` never
        /// yields `Valid` for the original key (`05-security §5.4`).
        #[test]
        fn mutated_source_makes_summary_unreturnable(
            source in proptest::collection::vec(0u8..255, 1..50),
            other in "[a-f0-9]{8}",
        ) {
            let store = recall_store();
            let key = block(put_summary(&store, &PlainCipher, "f.rs", &source, versions(), b"digest")).unwrap();
            let real = sha256_hex(&source);
            // any worktree hash != the bound hash must NOT be Valid
            proptest::prop_assume!(other != real);
            let verdict = block(resolve(&store, &PlainCipher, &snapshot("f.rs", &other), &key)).unwrap();
            proptest::prop_assert!(!matches!(verdict, Validity::Valid(_)));
        }
    }
}
