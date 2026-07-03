//! Application-layer column encryption on the write/read path (Layer 2).
//!
//! [`EncryptingStore`] wraps any [`CanonicalStore`] engine (rusqlite reference or
//! Turso) and a [`KeyStore`]: on insert it AEAD-encrypts every column registered
//! sensitive ([`crate::sensitive`]) before the row reaches the engine, and on read
//! it decrypts. The ciphertext (`nonce ‖ AEAD`) is bound to its `(table, column,
//! row_id)` slot via the AAD ([`crate::crypto::encrypt_column`]), so a relocated or
//! copied ciphertext fails authentication (anti-relocation).
//!
//! It is store-agnostic and needs no Turso: the end-to-end envelope round-trip is
//! provable on the always-available rusqlite engine (`tests/encryption_e2e.rs`),
//! which is the standing confidentiality guarantee while Layer-1 whole-DB
//! encryption is unavailable on the pinned Turso BETA.

use rift_core::{RepoId, Result, RiftError};
use rift_crypto::SensitiveBytes;

use crate::crypto::{decrypt_column, encrypt_column, KeyStore};
use crate::sensitive::is_sensitive_column;
use crate::{CanonicalStore, Value};

/// A write/read facade that AEAD-encrypts the enumerated sensitive columns.
pub struct EncryptingStore<'a, S: CanonicalStore + ?Sized, K: KeyStore + ?Sized> {
    inner: &'a S,
    keys: &'a K,
    repo_id: RepoId,
}

impl<'a, S: CanonicalStore + ?Sized, K: KeyStore + ?Sized> EncryptingStore<'a, S, K> {
    /// Wrap `inner` with column encryption keyed for `repo_id` via `keys`.
    pub fn new(inner: &'a S, keys: &'a K, repo_id: RepoId) -> Self {
        Self {
            inner,
            keys,
            repo_id,
        }
    }

    /// Insert a row, AEAD-encrypting any column registered sensitive. `row_id` is
    /// the row's primary-key string — the AAD anchor binding ciphertext to its slot.
    ///
    /// # Errors
    /// [`RiftError::Crypto`] on key/AEAD failure or a non-encryptable sensitive
    /// value; [`RiftError::Storage`]/[`RiftError::Immutability`] from the engine.
    pub async fn insert_encrypted(
        &self,
        table: &str,
        row_id: &str,
        columns: &[&str],
        values: &[Value],
    ) -> Result<()> {
        if columns.len() != values.len() {
            return Err(RiftError::storage(format!(
                "insert into {table}: {} columns but {} values",
                columns.len(),
                values.len()
            )));
        }
        let dek = self.keys.repo_dek(&self.repo_id)?;
        let mut sealed: Vec<Value> = Vec::with_capacity(values.len());
        for (col, val) in columns.iter().zip(values.iter()) {
            if is_sensitive_column(table, col) {
                let plaintext = plaintext_bytes(val)?;
                let ct = encrypt_column(&dek, table, col, row_id, &plaintext)?;
                sealed.push(Value::Blob(ct));
            } else {
                sealed.push(val.clone());
            }
        }
        self.inner.insert(table, columns, &sealed).await
    }

    /// Decrypt a sensitive column value read back from the engine.
    ///
    /// # Errors
    /// [`RiftError::Crypto`] on tamper, AAD/location mismatch, or key failure.
    pub fn decrypt_value(
        &self,
        table: &str,
        column: &str,
        row_id: &str,
        blob: &[u8],
    ) -> Result<SensitiveBytes> {
        let dek = self.keys.repo_dek(&self.repo_id)?;
        decrypt_column(&dek, table, column, row_id, blob)
    }
}

/// The plaintext bytes of a value to encrypt. Sensitive columns are `BLOB`
/// (binary payloads) or `TEXT`; numeric/null values are not app-encryptable.
fn plaintext_bytes(v: &Value) -> Result<Vec<u8>> {
    match v {
        Value::Blob(b) => Ok(b.clone()),
        Value::Text(s) => Ok(s.clone().into_bytes()),
        Value::Null | Value::Int(_) => Err(RiftError::crypto(
            "sensitive column value must be a blob or text payload",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::MemoryKeyStore;
    use crate::{apply_migrations, recall_migrations, SqliteStore};
    use rift_crypto::DataKey;

    const DIGEST_COLS: [&str; 3] = ["digest_ref", "bytes", "created_at"];

    fn digest_row(digest_ref: &str, bytes: Vec<u8>) -> [Value; 3] {
        [Value::text(digest_ref), Value::Blob(bytes), Value::Int(1)]
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    #[tokio::test]
    async fn sensitive_column_is_ciphertext_and_decrypts() {
        let store = recall_store();
        let keys = MemoryKeyStore::new(DataKey::from_bytes([5u8; 32]));
        let enc = EncryptingStore::new(&store, &keys, RepoId::new("repo-1").unwrap());

        let sentinel = b"PLAINTEXT-DIGEST-SENTINEL".to_vec();
        enc.insert_encrypted(
            "digest_blob",
            "ref-1",
            &DIGEST_COLS,
            &digest_row("ref-1", sentinel.clone()),
        )
        .await
        .unwrap();

        let rows = store
            .query(
                "SELECT bytes FROM digest_blob WHERE digest_ref = ?",
                &[Value::text("ref-1")],
            )
            .await
            .unwrap();
        let stored = rows[0].get_blob(0).unwrap();
        assert_ne!(stored, sentinel, "the stored blob must be ciphertext");
        assert!(
            !stored
                .windows(sentinel.len())
                .any(|w| w == sentinel.as_slice()),
            "ciphertext must not contain the plaintext sentinel"
        );

        let pt = enc
            .decrypt_value("digest_blob", "bytes", "ref-1", &stored)
            .unwrap();
        assert_eq!(pt.as_slice(), sentinel.as_slice());
    }

    #[tokio::test]
    async fn relocated_ciphertext_fails_to_decrypt() {
        let store = recall_store();
        let keys = MemoryKeyStore::new(DataKey::from_bytes([6u8; 32]));
        let enc = EncryptingStore::new(&store, &keys, RepoId::new("repo-1").unwrap());

        enc.insert_encrypted(
            "digest_blob",
            "ref-1",
            &DIGEST_COLS,
            &digest_row("ref-1", b"secret".to_vec()),
        )
        .await
        .unwrap();
        let rows = store
            .query("SELECT bytes FROM digest_blob", &[])
            .await
            .unwrap();
        let stored = rows[0].get_blob(0).unwrap();
        // Decrypting under a different row_id (AAD mismatch) must fail.
        assert!(matches!(
            enc.decrypt_value("digest_blob", "bytes", "ref-OTHER", &stored),
            Err(RiftError::Crypto(_))
        ));
    }

    #[tokio::test]
    async fn non_encryptable_sensitive_value_is_rejected() {
        let store = recall_store();
        let keys = MemoryKeyStore::new(DataKey::from_bytes([7u8; 32]));
        let enc = EncryptingStore::new(&store, &keys, RepoId::new("repo-1").unwrap());
        // bytes (sensitive) given as an Int → not an encryptable payload.
        let mut row = digest_row("ref-1", vec![]);
        row[1] = Value::Int(99);
        let err = enc
            .insert_encrypted("digest_blob", "ref-1", &DIGEST_COLS, &row)
            .await;
        assert!(matches!(err, Err(RiftError::Crypto(_))), "got {err:?}");
    }
}
