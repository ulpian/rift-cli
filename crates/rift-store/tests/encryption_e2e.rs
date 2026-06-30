//! End-to-end encryption-at-rest (Layer 2), proven on the always-available
//! rusqlite engine — no Turso required.
//!
//! Drives the full custody chain — keychain → KEK → HKDF DEK → AEAD column
//! encryption → on-disk BLOB → readback → decrypt — against a **file-backed**
//! store so the raw database bytes can be inspected. This is the step-05 keystone:
//! `db_file_bytes_do_not_contain_plaintext_sensitive_value`, plus the
//! anti-relocation (AAD) check and the lost-keychain-degrades-to-rebuild path.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use rift_core::{RepoId, RiftError};
use rift_store::{
    rebuild, CanonicalStore, EncryptingStore, KeyringKeyStore, MemoryKeychain, SqliteStore, Value,
};

const KM_COLS: [&str; 9] = [
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

fn km_row(key_id: &str, wrapped: Vec<u8>) -> [Value; 9] {
    [
        Value::text(key_id),
        Value::text("repo-1"),
        Value::Blob(wrapped),
        Value::text("kek/repo-1"),
        Value::text("chacha20poly1305"),
        Value::Int(1),
        Value::Int(1),
        Value::Null,
        Value::Int(1),
    ]
}

/// The keystone: a sensitive value written through [`EncryptingStore`] is
/// ciphertext on disk (the raw `.db` bytes never contain the plaintext sentinel),
/// and reads back to the original plaintext through the full keychain→KEK→DEK chain.
#[tokio::test]
async fn db_file_bytes_do_not_contain_plaintext_sensitive_value() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("projection.db");
    let repo = RepoId::new("repo-1").unwrap();
    let sentinel: &[u8] = b"SUPER-SECRET-SENTINEL-9f3a";

    {
        let store = SqliteStore::open(&db).unwrap();
        store.create_canonical_schema().unwrap();
        // Full custody chain: keychain → KEK → HKDF DEK (not the bare MemoryKeyStore).
        let keys = KeyringKeyStore::new(MemoryKeychain::new());
        let enc = EncryptingStore::new(&store, &keys, repo.clone());

        enc.insert_encrypted(
            "key_metadata",
            "kid-1",
            &KM_COLS,
            &km_row("kid-1", sentinel.to_vec()),
        )
        .await
        .unwrap();

        // Read the stored ciphertext back and decrypt it.
        let rows = store
            .query(
                "SELECT wrapped_dek FROM key_metadata WHERE key_id = ?",
                &[Value::text("kid-1")],
            )
            .await
            .unwrap();
        let ct = rows[0].get_blob(0).unwrap();
        let pt = enc
            .decrypt_value("key_metadata", "wrapped_dek", "kid-1", &ct)
            .unwrap();
        assert_eq!(pt.as_slice(), sentinel, "decrypt recovers the plaintext");

        // Anti-relocation: the AAD binds ciphertext to (table, column, row_id).
        assert!(matches!(
            enc.decrypt_value("key_metadata", "wrapped_dek", "kid-OTHER", &ct),
            Err(RiftError::Crypto(_))
        ));
    }

    // The raw on-disk database (and the WAL sidecar, if present) must NOT contain
    // the plaintext sentinel.
    assert_sentinel_absent(&db, sentinel);
    assert_sentinel_absent(&db.with_extension("db-wal"), sentinel);
}

fn assert_sentinel_absent(path: &std::path::Path, sentinel: &[u8]) {
    if let Ok(bytes) = std::fs::read(path) {
        assert!(
            !bytes.windows(sentinel.len()).any(|w| w == sentinel),
            "plaintext sentinel leaked into {}",
            path.display()
        );
    }
}

/// Losing the OS-keychain entry degrades to a rebuild, not data loss: a fresh
/// keychain yields a different KEK/DEK that cannot decrypt the old ciphertext, and
/// the response is to re-create the (rebuildable) projection from zero.
#[tokio::test]
async fn lost_keychain_entry_triggers_rebuild_not_dataloss() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("projection.db");
    let repo = RepoId::new("repo-1").unwrap();

    let ct = {
        let store = SqliteStore::open(&db).unwrap();
        store.create_canonical_schema().unwrap();

        // Encrypt + store under the KEK custodied by keychain A.
        let keys_a = KeyringKeyStore::new(MemoryKeychain::new());
        let enc_a = EncryptingStore::new(&store, &keys_a, repo.clone());
        enc_a
            .insert_encrypted(
                "key_metadata",
                "kid-1",
                &KM_COLS,
                &km_row("kid-1", b"old-secret".to_vec()),
            )
            .await
            .unwrap();
        let rows = store
            .query("SELECT wrapped_dek FROM key_metadata", &[])
            .await
            .unwrap();
        let ct = rows[0].get_blob(0).unwrap();

        // Keychain entry lost → a fresh (empty) keychain mints a *different* KEK,
        // so the derived DEK cannot decrypt the old ciphertext.
        let keys_b = KeyringKeyStore::new(MemoryKeychain::new());
        let enc_b = EncryptingStore::new(&store, &keys_b, repo.clone());
        assert!(
            matches!(
                enc_b.decrypt_value("key_metadata", "wrapped_dek", "kid-1", &ct),
                Err(RiftError::Crypto(_))
            ),
            "a new key must not decrypt old-key data"
        );
        ct
    };

    // Degrade to rebuild: delete + re-migrate. Not data loss (git is canonical).
    let fresh = rebuild(&db).unwrap();
    assert_eq!(fresh.count("key_metadata").await.unwrap(), 0);
    assert!(db.exists());
    // The old ciphertext is gone with the rebuilt file; the new projection is usable.
    drop(ct);
}
