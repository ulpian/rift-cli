//! End-to-end encryption-at-rest (Layer 2), proven on the always-available
//! rusqlite engine — no Turso required.
//!
//! Drives the full custody chain — keychain → KEK → **wrapped DEK**
//! (`key_metadata`, unwrapped on provision) → AEAD column encryption → on-disk
//! BLOB → readback → decrypt — against a **file-backed** store so the raw
//! database bytes can be inspected. This is the step-05 keystone:
//! `db_file_bytes_do_not_contain_plaintext_sensitive_value`, plus the
//! anti-relocation (AAD) check, the KEK-rotation cheap-re-wrap contract, and
//! the lost-keychain-degrades-to-rebuild path.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use rift_core::{RepoId, RiftError};
use rift_store::{
    open_migrated, rebuild, recall_migrations, CanonicalStore, EncryptingStore, KeyringKeyStore,
    MemoryKeyStore, MemoryKeychain, SqliteStore, Value,
};

const DIGEST_COLS: [&str; 3] = ["digest_ref", "bytes", "created_at"];

fn digest_row(digest_ref: &str, bytes: Vec<u8>) -> [Value; 3] {
    [Value::text(digest_ref), Value::Blob(bytes), Value::Int(1)]
}

/// Open a file-backed recall store (the projection whose `digest_blob.bytes`
/// is the registered Layer-2 sensitive column) plus capture custody tables.
fn open_recall(db: &std::path::Path) -> SqliteStore {
    open_migrated(db, &recall_migrations()).unwrap()
}

/// The keystone: a sensitive value written through [`EncryptingStore`] is
/// ciphertext on disk (the raw `.db` bytes never contain the plaintext
/// sentinel), and reads back to the original plaintext through the full
/// keychain → KEK → wrapped-DEK chain.
#[tokio::test]
async fn db_file_bytes_do_not_contain_plaintext_sensitive_value() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("recall.db");
    let repo = RepoId::new("repo-1").unwrap();
    let sentinel: &[u8] = b"SUPER-SECRET-SENTINEL-9f3a";

    {
        let store = open_recall(&db);
        // Full custody chain: keychain → KEK → wrapped DEK (recorded in
        // key_metadata, unwrapped on provision) — not a bare in-memory key.
        let custody = KeyringKeyStore::new(MemoryKeychain::new());
        let dek = custody.provision_repo_dek(&store, &repo, 1).await.unwrap();
        let keys = MemoryKeyStore::new(dek);
        let enc = EncryptingStore::new(&store, &keys, repo.clone());

        enc.insert_encrypted(
            "digest_blob",
            "ref-1",
            &DIGEST_COLS,
            &digest_row("ref-1", sentinel.to_vec()),
        )
        .await
        .unwrap();

        // Read the stored ciphertext back and decrypt it.
        let rows = store
            .query(
                "SELECT bytes FROM digest_blob WHERE digest_ref = ?",
                &[Value::text("ref-1")],
            )
            .await
            .unwrap();
        let ct = rows[0].get_blob(0).unwrap();
        let pt = enc.decrypt_value("digest_blob", "bytes", "ref-1", &ct).unwrap();
        assert_eq!(pt.as_slice(), sentinel, "decrypt recovers the plaintext");

        // Anti-relocation: the AAD binds ciphertext to (table, column, row_id).
        assert!(matches!(
            enc.decrypt_value("digest_blob", "bytes", "ref-OTHER", &ct),
            Err(RiftError::Crypto(_))
        ));

        // The persisted wrapped DEK must not contain the raw DEK bytes either.
        let km = store
            .query("SELECT wrapped_dek FROM key_metadata", &[])
            .await
            .unwrap();
        let wrapped = km[0].get_blob(0).unwrap();
        let dek_again = custody.provision_repo_dek(&store, &repo, 2).await.unwrap();
        assert!(!wrapped
            .windows(dek_again.expose().len())
            .any(|w| w == dek_again.expose()));
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

/// The cheap-rotation contract (phase-03 step-05): rotating the KEK re-wraps
/// the same DEK under a fresh keychain secret — previously written ciphertext
/// stays decryptable with zero data re-encryption, across a store re-open.
#[tokio::test]
async fn kek_rotation_keeps_on_disk_ciphertext_decryptable() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("recall.db");
    let repo = RepoId::new("repo-1").unwrap();
    let keychain = MemoryKeychain::new();
    let sentinel: &[u8] = b"pre-rotation-payload";

    // Write under generation 1.
    {
        let store = open_recall(&db);
        let custody = KeyringKeyStore::new(&keychain);
        let dek = custody.provision_repo_dek(&store, &repo, 1).await.unwrap();
        let keys = MemoryKeyStore::new(dek);
        let enc = EncryptingStore::new(&store, &keys, repo.clone());
        enc.insert_encrypted(
            "digest_blob",
            "ref-1",
            &DIGEST_COLS,
            &digest_row("ref-1", sentinel.to_vec()),
        )
        .await
        .unwrap();
        // Rotate: new KEK in the keychain, superseding generation-2 row.
        assert_eq!(custody.rotate_kek(&store, &repo, 2).await.unwrap(), 2);
    }

    // Re-open: provision unwraps generation 2 under the NEW KEK; old data reads.
    {
        let store = open_recall(&db);
        let custody = KeyringKeyStore::new(&keychain);
        let dek = custody.provision_repo_dek(&store, &repo, 3).await.unwrap();
        let keys = MemoryKeyStore::new(dek);
        let enc = EncryptingStore::new(&store, &keys, repo.clone());
        let rows = store
            .query("SELECT bytes FROM digest_blob", &[])
            .await
            .unwrap();
        let ct = rows[0].get_blob(0).unwrap();
        let pt = enc.decrypt_value("digest_blob", "bytes", "ref-1", &ct).unwrap();
        assert_eq!(pt.as_slice(), sentinel);
        assert_eq!(store.count("key_metadata").await.unwrap(), 2);
    }
}

/// Losing the OS-keychain entry degrades to a rebuild, not data loss: with the
/// wrapped DEK still on disk but the KEK gone, provisioning fails **closed**
/// (never silently mints a new key over undecryptable data), and the response
/// is to re-create the (rebuildable) projection from zero.
#[tokio::test]
async fn lost_keychain_entry_triggers_rebuild_not_dataloss() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("recall.db");
    let repo = RepoId::new("repo-1").unwrap();

    {
        let store = open_recall(&db);
        // Encrypt + store under the KEK custodied by keychain A.
        let custody_a = KeyringKeyStore::new(MemoryKeychain::new());
        let dek = custody_a.provision_repo_dek(&store, &repo, 1).await.unwrap();
        let keys = MemoryKeyStore::new(dek);
        let enc = EncryptingStore::new(&store, &keys, repo.clone());
        enc.insert_encrypted(
            "digest_blob",
            "ref-1",
            &DIGEST_COLS,
            &digest_row("ref-1", b"old-secret".to_vec()),
        )
        .await
        .unwrap();

        // Keychain entry lost → a fresh (empty) keychain mints a *different* KEK,
        // which cannot unwrap the stored DEK: provisioning fails closed.
        let custody_b = KeyringKeyStore::new(MemoryKeychain::new());
        assert!(matches!(
            custody_b.provision_repo_dek(&store, &repo, 2).await,
            Err(RiftError::Crypto(_))
        ));
    }

    // Degrade to rebuild: delete + re-migrate. Not data loss (git is canonical).
    let fresh = rebuild(&db).unwrap();
    assert_eq!(fresh.count("key_metadata").await.unwrap(), 0);
    assert!(db.exists());
}
