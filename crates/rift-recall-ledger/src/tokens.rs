//! Fallback token estimates for read facts whose shard froze no figure.
//!
//! Shards written before token capture landed carry `{id, path, sha256, tool}`
//! lines only, so their reads would ingest as 0 tokens and the reread-report
//! would be structurally hollow. This module resolves what can still be
//! resolved, **verifying every candidate against the recorded hash** before
//! tokenizing — measured or absent, never a guess:
//!
//! 1. a **sibling fact's inline figure** — another shard froze a measurement
//!    for the same `content_sha256` (pure over the substrate, so a rebuild
//!    reproduces it byte-for-byte);
//! 2. the **worktree** — the file at the recorded path still hashes to the
//!    read-time value;
//! 3. a **recorded blob oid** (the recall content→blob-oid map, then the capture
//!    projection's `file_revision`) whose object-store bytes verify.
//!
//! Sources 2–3 read the ingest-time environment, so two rebuilds may differ in
//! **coverage** (a hash resolved on one run and not the other) — but never in
//! value: a hash only ever maps to the one count of its verified bytes. The
//! projection stays honest under rebuild; it simply knows as much as is still
//! reachable.

use std::collections::BTreeMap;
use std::path::Path;

use rift_recall_substrate::{worktree, SubstrateReader};
use rift_store::CanonicalStore;

use crate::FileRead;

/// Build the `content_sha256 → input_tokens` fallback map for
/// [`crate::ingest::IngestOptions::token_estimate_by_hash`]. `capture` is the
/// capture projection when it exists (best-effort context — recall runs
/// without it); `reader` serves object-store bytes for recorded oids.
pub async fn fallback_token_estimates(
    reads: &[FileRead],
    recall: &impl CanonicalStore,
    capture: Option<&impl CanonicalStore>,
    reader: &impl SubstrateReader,
    repo_root: &Path,
) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    // 1) Sibling inline figures: substrate-pure, so they win and reproduce
    //    identically on every rebuild (first occurrence in walk order).
    for r in reads {
        if r.input_tokens > 0 {
            out.entry(r.content_sha256.clone())
                .or_insert(r.input_tokens);
        }
    }
    // Environment legs measure with the real BPE only ("measured or absent"):
    // the chars/4 fallback would freeze crude figures whose basis differs
    // between runs, so a BPE-less build resolves nothing beyond siblings.
    if rift_tokens::provenance() != rift_tokens::TOKENIZER {
        return out;
    }
    // Oid maps are loaded once — per-hash queries would scan per read.
    let recall_oids = crate::blob_map::load_blob_oid_map(recall)
        .await
        .unwrap_or_default();
    let capture_oids = match capture {
        Some(c) => rift_store::content_blob_oid_map(c)
            .await
            .unwrap_or_default(),
        None => BTreeMap::new(),
    };
    for r in reads {
        if r.input_tokens > 0 || out.contains_key(&r.content_sha256) {
            continue;
        }
        // 2) The worktree still holds the exact bytes (single read+hash — the
        //    same bytes that verify are the bytes tokenized).
        let bytes = worktree::hash_blob_bytes(repo_root, &r.path)
            .filter(|(blob, _)| blob.content_sha256 == r.content_sha256)
            .map(|(_, bytes)| bytes)
            // 3) A recorded oid whose object-store bytes verify.
            .or_else(|| {
                recall_oids
                    .get(&r.content_sha256)
                    .or_else(|| capture_oids.get(&r.content_sha256))
                    .and_then(|oid| reader.blob(oid).ok().flatten())
                    .filter(|bytes| worktree::sha256_hex(bytes) == r.content_sha256)
            });
        if let Some(bytes) = bytes {
            out.insert(
                r.content_sha256.clone(),
                rift_tokens::count(&String::from_utf8_lossy(&bytes)),
            );
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_substrate::MemorySubstrate;
    use rift_store::{recall_migrations, SqliteStore};

    fn read(path: &str, hash: &str, tokens: u32) -> FileRead {
        FileRead {
            path: path.into(),
            content_sha256: hash.into(),
            input_tokens: tokens,
        }
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    /// Sibling inline figures resolve factless reads of the same hash without
    /// touching any environment — pure over the substrate.
    #[test]
    fn sibling_inline_figures_win_and_need_no_environment() {
        let dir = tempfile::tempdir().unwrap();
        let reads = vec![read("a.rs", "h1", 70), read("a.rs", "h1", 0)];
        let store = recall_store();
        let sub = MemorySubstrate::local();
        let map = block(fallback_token_estimates(
            &reads,
            &store,
            None::<&SqliteStore>,
            &sub,
            dir.path(),
        ));
        assert_eq!(map.get("h1"), Some(&70));
    }

    /// A worktree file that still hashes to the read-time value is tokenized;
    /// one that drifted resolves nothing (never a guess).
    #[test]
    fn worktree_bytes_resolve_only_on_hash_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), b"fn main() {}\n").unwrap();
        let h = worktree::sha256_hex(b"fn main() {}\n");
        let reads = vec![read("a.rs", &h, 0), read("a.rs", "not-the-hash", 0)];
        let store = recall_store();
        let sub = MemorySubstrate::local();
        let map = block(fallback_token_estimates(
            &reads,
            &store,
            None::<&SqliteStore>,
            &sub,
            dir.path(),
        ));
        assert_eq!(map.get(&h), Some(&rift_tokens::count("fn main() {}\n")));
        assert_eq!(map.get("not-the-hash"), None);
    }

    /// A recorded blob oid resolves through the reader, and bytes that fail
    /// hash verification are refused even when the oid map names them.
    #[test]
    fn recorded_oid_bytes_must_verify() {
        let dir = tempfile::tempdir().unwrap();
        let content = b"pub fn f() {}\n";
        let h = worktree::sha256_hex(content);
        let store = recall_store();
        block(crate::blob_map::persist_blob_oid_map(
            &store,
            &[
                worktree::WorktreeBlob {
                    path: "gone.rs".into(),
                    content_sha256: h.clone(),
                    blob_oid: "oid-good".into(),
                },
                worktree::WorktreeBlob {
                    path: "liar.rs".into(),
                    content_sha256: "h-liar".into(),
                    blob_oid: "oid-liar".into(),
                },
            ],
            0,
        ))
        .unwrap();
        let sub = MemorySubstrate::local()
            .with_blob("oid-good", content)
            .with_blob("oid-liar", b"different bytes");
        let reads = vec![read("gone.rs", &h, 0), read("liar.rs", "h-liar", 0)];
        let map = block(fallback_token_estimates(
            &reads,
            &store,
            None::<&SqliteStore>,
            &sub,
            dir.path(),
        ));
        assert_eq!(
            map.get(&h),
            Some(&rift_tokens::count(&String::from_utf8_lossy(content)))
        );
        assert_eq!(map.get("h-liar"), None, "unverified bytes never count");
    }
}
