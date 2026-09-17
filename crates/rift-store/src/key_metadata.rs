//! Encryption-at-rest key metadata + rotation (insert-only, bitemporal-ish).
//!
//! The `key_metadata` table (registered + trigger-guarded in [`crate::schema`])
//! records, per `(repo, generation)`, the wrapped DEK and the keychain KEK
//! reference. It is **insert-only**: a key *rotation* inserts a new generation row
//! whose `supersedes_id` points at the prior one; the old row stays so data
//! encrypted under an old generation can still be decrypted (corrections
//! supersede, never overwrite). Losing the keychain KEK degrades to a rebuild from
//! git, not data loss ([`crate::rebuild`](fn@crate::rebuild)).
//!
//! Determinism: `generation`/`recorded_at` are injected by the caller (no wall
//! clock); `key_id` is content-addressed over the row's identifying fields.

use rift_core::{Immutable, Result};
use sha2::{Digest, Sha256};

use crate::{CanonicalRow, CanonicalStore, Value};

const SELECT: &str = "SELECT key_id, repo_id, wrapped_dek, kek_ref, cipher, \
     generation, recorded_at, supersedes_id, schema_version FROM key_metadata";

/// A key-metadata generation row (insert-only; rotation supersedes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyMetadataRow {
    /// Content-addressed id over `(repo_id, generation, wrapped_dek)`.
    pub key_id: String,
    /// The repo this key generation belongs to.
    pub repo_id: String,
    /// The DEK sealed under the KEK (ciphertext), stored as a `BLOB`.
    pub wrapped_dek: Vec<u8>,
    /// Keychain reference for the KEK (service/account), never the KEK itself.
    pub kek_ref: String,
    /// Cipher/scheme identifier (e.g. `chacha20poly1305`).
    pub cipher: String,
    /// Monotonic generation; rotation inserts `generation + 1`.
    pub generation: i64,
    /// Transaction-time (epoch ms) — injected, never the wall clock.
    pub recorded_at: i64,
    /// The generation this supersedes (`None` for the first).
    pub supersedes_id: Option<String>,
    /// Row schema version.
    pub schema_version: i64,
}

impl KeyMetadataRow {
    /// The content-addressed `key_id` for `(repo_id, generation, wrapped_dek)` —
    /// deterministic, so the same generation re-records idempotently.
    #[must_use]
    pub fn derive_key_id(repo_id: &str, generation: i64, wrapped_dek: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(repo_id.as_bytes());
        h.update([0]);
        h.update(generation.to_le_bytes());
        h.update([0]);
        h.update(wrapped_dek);
        let out = h.finalize();
        let mut s = String::with_capacity(out.len() * 2);
        use core::fmt::Write as _;
        for b in out {
            let _ = write!(s, "{b:02x}");
        }
        s
    }
}

impl Immutable for KeyMetadataRow {}

impl CanonicalRow for KeyMetadataRow {
    const TABLE: &'static str = "key_metadata";
    const COLUMNS: &'static [&'static str] = &[
        "key_id",
        "repo_id",
        "wrapped_dek",
        "kek_ref",
        "cipher",
        "generation",
        "recorded_at",
        "supersedes_id",
        "schema_version",
    ];

    fn values(&self) -> Vec<Value> {
        vec![
            Value::text(self.key_id.clone()),
            Value::text(self.repo_id.clone()),
            Value::Blob(self.wrapped_dek.clone()),
            Value::text(self.kek_ref.clone()),
            Value::text(self.cipher.clone()),
            Value::Int(self.generation),
            Value::Int(self.recorded_at),
            self.supersedes_id.clone().map_or(Value::Null, Value::text),
            Value::Int(self.schema_version),
        ]
    }
}

/// Record a key-metadata generation (insert-only).
///
/// # Errors
/// [`rift_core::RiftError::Storage`]/[`rift_core::RiftError::Immutability`] on failure.
pub async fn record_key_metadata(
    store: &(impl CanonicalStore + Sized),
    r: &KeyMetadataRow,
) -> Result<()> {
    store.insert_row(r).await
}

/// The latest (highest-generation) key metadata for a repo — the read path used to
/// unwrap the current DEK.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on query failure.
pub async fn latest_key_metadata(
    store: &impl CanonicalStore,
    repo_id: &str,
) -> Result<Option<KeyMetadataRow>> {
    let rows = store
        .query(
            &format!("{SELECT} WHERE repo_id = ? ORDER BY generation DESC LIMIT 1"),
            &[Value::text(repo_id)],
        )
        .await?;
    match rows.first() {
        Some(r) => Ok(Some(row_to_key_metadata(r)?)),
        None => Ok(None),
    }
}

/// A specific generation's key metadata — so data encrypted under an old key can
/// still be unwrapped during rotation.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on query failure.
pub async fn key_metadata_for_generation(
    store: &impl CanonicalStore,
    repo_id: &str,
    generation: i64,
) -> Result<Option<KeyMetadataRow>> {
    let rows = store
        .query(
            &format!("{SELECT} WHERE repo_id = ? AND generation = ? LIMIT 1"),
            &[Value::text(repo_id), Value::Int(generation)],
        )
        .await?;
    match rows.first() {
        Some(r) => Ok(Some(row_to_key_metadata(r)?)),
        None => Ok(None),
    }
}

fn row_to_key_metadata(r: &crate::Row) -> Result<KeyMetadataRow> {
    Ok(KeyMetadataRow {
        key_id: r.get_text(0)?,
        repo_id: r.get_text(1)?,
        wrapped_dek: r.get_blob(2)?,
        kek_ref: r.get_text(3)?,
        cipher: r.get_text(4)?,
        generation: r.get_i64(5)?,
        recorded_at: r.get_i64(6)?,
        supersedes_id: r.get_opt_text(7)?,
        schema_version: r.get_i64(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteStore;
    use rift_core::RiftError;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    fn gen_row(repo: &str, generation: i64, supersedes: Option<&str>) -> KeyMetadataRow {
        let wrapped = vec![u8::try_from(generation).unwrap_or(0); 32];
        KeyMetadataRow {
            key_id: KeyMetadataRow::derive_key_id(repo, generation, &wrapped),
            repo_id: repo.to_string(),
            wrapped_dek: wrapped,
            kek_ref: format!("kek/{repo}"),
            cipher: "chacha20poly1305".to_string(),
            generation,
            recorded_at: generation, // injected (no wall clock)
            supersedes_id: supersedes.map(ToString::to_string),
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn key_rotation_inserts_new_generation_and_keeps_old() {
        let s = store();
        let g1 = gen_row("repo-1", 1, None);
        record_key_metadata(&s, &g1).await.unwrap();
        let g2 = gen_row("repo-1", 2, Some(&g1.key_id));
        record_key_metadata(&s, &g2).await.unwrap();

        // Both generations persist (immutable; rotation supersedes, never overwrites).
        assert_eq!(s.count("key_metadata").await.unwrap(), 2);
        // Latest is generation 2.
        let latest = latest_key_metadata(&s, "repo-1").await.unwrap().unwrap();
        assert_eq!(latest.generation, 2);
        assert_eq!(latest.supersedes_id.as_deref(), Some(g1.key_id.as_str()));
        // The old generation is still retrievable to decrypt old-key data.
        let old = key_metadata_for_generation(&s, "repo-1", 1)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(old, g1);
    }

    #[tokio::test]
    async fn latest_and_specific_are_none_for_unknown() {
        let s = store();
        assert!(latest_key_metadata(&s, "nope").await.unwrap().is_none());
        assert!(key_metadata_for_generation(&s, "nope", 1)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn key_id_is_deterministic() {
        let a = KeyMetadataRow::derive_key_id("repo-1", 1, b"abc");
        let b = KeyMetadataRow::derive_key_id("repo-1", 1, b"abc");
        let c = KeyMetadataRow::derive_key_id("repo-1", 2, b"abc");
        assert_eq!(a, b);
        assert_ne!(a, c);
    }

    #[tokio::test]
    async fn rotation_record_cannot_be_updated_or_deleted() {
        let s = store();
        record_key_metadata(&s, &gen_row("repo-1", 1, None))
            .await
            .unwrap();
        // The SQL-layer append-only guard rejects mutation even via raw SQL.
        assert!(matches!(
            s.execute_raw("UPDATE key_metadata SET generation = 9"),
            Err(RiftError::Immutability(_))
        ));
        assert!(matches!(
            s.execute_raw("DELETE FROM key_metadata"),
            Err(RiftError::Immutability(_))
        ));
        assert_eq!(s.count("key_metadata").await.unwrap(), 1);
    }
}
