//! The ADR-0005 content→blob-oid map: persist and load.
//!
//! Recall handles bind content by `content_sha256`, but git addresses blobs by
//! object id. This module writes the bridge — `n_blob_oid_map` rows computed by
//! the substrate worktree hasher (both hashes over the same bytes, one pass) —
//! and loads it back for the durable `file:`/exact resolvers. Append-only and
//! content-addressed (PK `content_sha256`), so re-ingest is a set-union no-op and
//! the map grows monotonically.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_recall_substrate::worktree::WorktreeBlob;
use rift_store::{CanonicalStore, Value};

/// Persist a batch of worktree `(content_sha256, blob_oid, path)` rows into
/// `n_blob_oid_map`. Idempotent (`INSERT OR IGNORE` on the content-addressed PK),
/// so a hash already mapped keeps its first-seen path/oid.
///
/// # Errors
/// Propagates store write errors.
pub async fn persist_blob_oid_map(
    store: &impl CanonicalStore,
    blobs: &[WorktreeBlob],
    now: i64,
) -> Result<usize> {
    let mut written = 0usize;
    for b in blobs {
        store
            .insert_or_ignore(
                "n_blob_oid_map",
                &["content_sha256", "blob_oid", "path", "first_seen"],
                &[
                    Value::text(&b.content_sha256),
                    Value::text(&b.blob_oid),
                    Value::text(&b.path),
                    Value::Int(now),
                ],
            )
            .await?;
        written += 1;
    }
    Ok(written)
}

/// Load the whole `content_sha256 → blob_oid` map for the durable resolvers.
///
/// # Errors
/// Propagates store read errors.
pub async fn load_blob_oid_map(store: &impl CanonicalStore) -> Result<BTreeMap<String, String>> {
    let rows = store
        .query("SELECT content_sha256, blob_oid FROM n_blob_oid_map", &[])
        .await?;
    let mut map = BTreeMap::new();
    for r in &rows {
        map.insert(r.get_text(0)?, r.get_text(1)?);
    }
    Ok(map)
}

/// Look up one `content_sha256`'s git blob oid (the exact-retrieval hot path,
/// avoiding loading the whole map).
///
/// # Errors
/// Propagates store read errors.
pub async fn blob_oid_for(
    store: &impl CanonicalStore,
    content_sha256: &str,
) -> Result<Option<String>> {
    let rows = store
        .query(
            "SELECT blob_oid FROM n_blob_oid_map WHERE content_sha256 = ?",
            &[Value::text(content_sha256)],
        )
        .await?;
    Ok(rows.first().and_then(|r| r.get_text(0).ok()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_substrate::worktree::{git_blob_oid, sha256_hex};
    use rift_store::{recall_migrations, SqliteStore};

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

    #[test]
    fn persist_and_load_round_trip_is_idempotent() {
        let store = recall_store();
        let bytes = b"fn main() {}\n";
        let blob = WorktreeBlob {
            path: "src/main.rs".into(),
            content_sha256: sha256_hex(bytes),
            blob_oid: git_blob_oid(bytes),
        };
        let n = block(persist_blob_oid_map(&store, std::slice::from_ref(&blob), 0)).unwrap();
        assert_eq!(n, 1);
        // re-persisting the same content is a set-union no-op (one row).
        block(persist_blob_oid_map(&store, std::slice::from_ref(&blob), 5)).unwrap();
        assert_eq!(block(store.count("n_blob_oid_map")).unwrap(), 1);

        let map = block(load_blob_oid_map(&store)).unwrap();
        assert_eq!(map.get(&blob.content_sha256), Some(&blob.blob_oid));
        assert_eq!(
            block(blob_oid_for(&store, &blob.content_sha256)).unwrap(),
            Some(blob.blob_oid.clone())
        );
        // a hash not in the map resolves to None (ADR-0005: honest miss).
        assert_eq!(block(blob_oid_for(&store, "deadbeef")).unwrap(), None);
    }
}
