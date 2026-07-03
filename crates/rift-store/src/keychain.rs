//! OS-keychain-backed envelope key custody (the "Layer 1" KEK seam).
//!
//! The envelope (phase-03 step-05 / phase-04 step-01): the per-repo **KEK**
//! lives in the OS keychain (never on disk); the per-repo **DEK** is generated
//! randomly once, AEAD-**wrapped** under the KEK, and persisted (wrapped) in the
//! insert-only `key_metadata` table ([`crate::key_metadata`]). Opening custody
//! ([`KeyringKeyStore::provision_repo_dek`]) unwraps the latest generation;
//! rotating the KEK ([`KeyringKeyStore::rotate_kek`]) re-wraps the *same* DEK
//! under a fresh KEK as a new superseding generation — no data re-encryption,
//! and old rows stay for audit (`04 §4.3`).
//!
//! The keychain itself is behind the [`KeychainBackend`] seam — a
//! [`MemoryKeychain`] for tests and headless/CI (no keychain available), and the
//! real [`OsKeychain`] behind the `keyring` feature. The custody logic is
//! therefore fully testable without a real keychain, and the OS engine plugs in
//! without touching callers (the project's seam discipline).

use std::collections::HashMap;
use std::sync::Mutex;

use rift_core::{RepoId, Result, RiftError};
use rift_crypto::{unwrap_dek, wrap_dek, DataKey, KEY_LEN};

use crate::key_metadata::{latest_key_metadata, record_key_metadata, KeyMetadataRow};
use crate::CanonicalStore;

/// Custody seam for a secret keyed by `(service, account)`. The secret bytes are
/// the raw KEK; an implementation must never persist them in plaintext on disk
/// (the OS keychain stores them under the user's credential store).
pub trait KeychainBackend {
    /// Fetch the secret for `(service, account)`, or `None` if absent.
    ///
    /// # Errors
    /// Backend-specific (e.g. the keychain is locked or unavailable).
    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>>;

    /// Store (replacing any existing) the secret for `(service, account)`.
    ///
    /// # Errors
    /// Backend-specific.
    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<()>;
}

impl<T: KeychainBackend + ?Sized> KeychainBackend for &T {
    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>> {
        (**self).get(service, account)
    }
    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<()> {
        (**self).set(service, account, secret)
    }
}

/// In-memory keychain for tests and headless/CI. Holds secrets in process memory
/// only — never writes them to disk.
#[derive(Default)]
pub struct MemoryKeychain {
    store: Mutex<HashMap<(String, String), Vec<u8>>>,
}

impl MemoryKeychain {
    /// A fresh, empty in-memory keychain.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl KeychainBackend for MemoryKeychain {
    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>> {
        let store = self
            .store
            .lock()
            .map_err(|_| RiftError::crypto("keychain mutex poisoned"))?;
        Ok(store
            .get(&(service.to_string(), account.to_string()))
            .cloned())
    }

    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<()> {
        let mut store = self
            .store
            .lock()
            .map_err(|_| RiftError::crypto("keychain mutex poisoned"))?;
        store.insert((service.to_string(), account.to_string()), secret.to_vec());
        Ok(())
    }
}

/// Envelope custody over a [`KeychainBackend`] (KEK) + the `key_metadata` table
/// (wrapped DEK). [`Self::provision_repo_dek`] is the production entry point;
/// the returned [`DataKey`] is held in memory (e.g. via
/// [`crate::MemoryKeyStore::new`]) for the process lifetime.
pub struct KeyringKeyStore<B: KeychainBackend> {
    backend: B,
    service: String,
}

impl<B: KeychainBackend> KeyringKeyStore<B> {
    /// Use `backend` under the default service name (`rift`).
    pub fn new(backend: B) -> Self {
        Self::with_service(backend, "rift")
    }

    /// Use `backend` under an explicit keychain service name (e.g. a test scope).
    pub fn with_service(backend: B, service: impl Into<String>) -> Self {
        Self {
            backend,
            service: service.into(),
        }
    }

    /// The keychain reference recorded in `key_metadata.kek_ref` — a locator
    /// (`service/account`), never key material.
    fn kek_ref(&self, repo_id: &RepoId) -> String {
        format!("{}/{}", self.service, repo_id.as_str())
    }

    /// The AAD binding a wrapped DEK to its `(repo, generation)` slot.
    fn wrap_aad(repo_id: &RepoId, generation: i64) -> Vec<u8> {
        format!("rift:wrapped-dek:{}:{generation}", repo_id.as_str()).into_bytes()
    }

    /// Fetch the per-repo KEK from the keychain, creating + storing a fresh random
    /// one on first use.
    fn load_or_create_kek(&self, repo_id: &RepoId) -> Result<DataKey> {
        if let Some(bytes) = self.backend.get(&self.service, repo_id.as_str())? {
            let arr: [u8; KEY_LEN] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| RiftError::crypto("malformed KEK in keychain"))?;
            return Ok(DataKey::from_bytes(arr));
        }
        let kek = DataKey::generate()?;
        self.backend
            .set(&self.service, repo_id.as_str(), kek.expose())?;
        Ok(kek)
    }

    /// Provision the per-repo DEK through the full envelope: load (or mint) the
    /// KEK from the keychain, then unwrap the latest `key_metadata` generation —
    /// or, on first use, generate a random DEK, wrap it under the KEK, and
    /// record generation 1. `now_ms` is injected (no wall clock).
    ///
    /// Fails **closed**: if a wrapped DEK exists but the keychain KEK cannot
    /// unwrap it (lost/replaced keychain entry), this is an error — the remedy
    /// is a projection rebuild ([`crate::rebuild`]), never a silent new key over
    /// undecryptable data.
    ///
    /// # Errors
    /// [`RiftError::Crypto`] on keychain, RNG, or unwrap failure;
    /// [`RiftError::Storage`] on `key_metadata` I/O.
    pub async fn provision_repo_dek(
        &self,
        store: &impl CanonicalStore,
        repo_id: &RepoId,
        now_ms: i64,
    ) -> Result<DataKey> {
        let kek = self.load_or_create_kek(repo_id)?;
        if let Some(row) = latest_key_metadata(store, repo_id.as_str()).await? {
            return unwrap_dek(&kek, &row.wrapped_dek, &Self::wrap_aad(repo_id, row.generation))
                .map_err(|_| {
                    RiftError::crypto(
                        "keychain KEK cannot unwrap the stored DEK (keychain entry \
                         lost or replaced) — rebuild the projection",
                    )
                });
        }
        let dek = DataKey::generate()?;
        let wrapped = wrap_dek(&kek, &dek, &Self::wrap_aad(repo_id, 1))?;
        record_key_metadata(
            store,
            &KeyMetadataRow {
                key_id: KeyMetadataRow::derive_key_id(repo_id.as_str(), 1, &wrapped),
                repo_id: repo_id.as_str().to_string(),
                wrapped_dek: wrapped,
                kek_ref: self.kek_ref(repo_id),
                cipher: "chacha20poly1305".to_string(),
                generation: 1,
                recorded_at: now_ms,
                supersedes_id: None,
                schema_version: 1,
            },
        )
        .await?;
        Ok(dek)
    }

    /// Rotate the KEK: mint a fresh keychain KEK and re-wrap the *current* DEK
    /// under it as a new superseding `key_metadata` generation. Cheap by design
    /// (phase-03 step-05): no column data is re-encrypted, and previously
    /// written ciphertext stays decryptable because the DEK is unchanged.
    /// Returns the new generation number.
    ///
    /// # Errors
    /// [`RiftError::Crypto`] if no custody exists yet for `repo_id`, or on
    /// keychain/unwrap failure; [`RiftError::Storage`] on `key_metadata` I/O.
    pub async fn rotate_kek(
        &self,
        store: &impl CanonicalStore,
        repo_id: &RepoId,
        now_ms: i64,
    ) -> Result<i64> {
        let Some(row) = latest_key_metadata(store, repo_id.as_str()).await? else {
            return Err(RiftError::crypto(
                "no key metadata to rotate — provision the repo DEK first",
            ));
        };
        let old_kek = self.load_or_create_kek(repo_id)?;
        let dek = unwrap_dek(
            &old_kek,
            &row.wrapped_dek,
            &Self::wrap_aad(repo_id, row.generation),
        )?;

        let new_kek = DataKey::generate()?;
        self.backend
            .set(&self.service, repo_id.as_str(), new_kek.expose())?;

        let generation = row.generation + 1;
        let wrapped = wrap_dek(&new_kek, &dek, &Self::wrap_aad(repo_id, generation))?;
        record_key_metadata(
            store,
            &KeyMetadataRow {
                key_id: KeyMetadataRow::derive_key_id(repo_id.as_str(), generation, &wrapped),
                repo_id: repo_id.as_str().to_string(),
                wrapped_dek: wrapped,
                kek_ref: self.kek_ref(repo_id),
                cipher: "chacha20poly1305".to_string(),
                generation,
                recorded_at: now_ms,
                supersedes_id: Some(row.key_id),
                schema_version: 1,
            },
        )
        .await?;
        Ok(generation)
    }
}

// The real OS keychain backend ([`crate::keychain_os::OsKeychain`]) lives behind
// the `keyring` feature in its own file, which is an untestable external-I/O
// boundary (only the `#[ignore]` real-keychain test exercises it) and is excluded
// from the coverage gate. Everything above is the testable seam logic.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{decrypt_column, encrypt_column};
    use crate::SqliteStore;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    #[tokio::test]
    async fn provision_creates_then_unwraps_the_same_dek() {
        let s = store();
        let ks = KeyringKeyStore::new(MemoryKeychain::new());
        let repo = RepoId::new("repo-1").unwrap();

        // First call generates + wraps + records; second unwraps the stored row.
        let dek1 = ks.provision_repo_dek(&s, &repo, 1).await.unwrap();
        let dek2 = ks.provision_repo_dek(&s, &repo, 2).await.unwrap();
        assert_eq!(dek1.expose(), dek2.expose());
        assert_eq!(s.count("key_metadata").await.unwrap(), 1);

        // The persisted wrapped_dek is ciphertext — it never contains the DEK.
        let rows = s
            .query("SELECT wrapped_dek FROM key_metadata", &[])
            .await
            .unwrap();
        let wrapped = rows[0].get_blob(0).unwrap();
        assert!(!wrapped.windows(KEY_LEN).any(|w| w == dek1.expose()));

        // The provisioned DEK drives column encryption end-to-end.
        let ct = encrypt_column(&dek1, "digest_blob", "bytes", "r1", b"secret$$$").unwrap();
        let pt = decrypt_column(&dek2, "digest_blob", "bytes", "r1", &ct).unwrap();
        assert_eq!(pt.as_slice(), b"secret$$$");
    }

    #[tokio::test]
    async fn kek_rotation_keeps_data_decryptable_and_supersedes() {
        let s = store();
        let ks = KeyringKeyStore::new(MemoryKeychain::new());
        let repo = RepoId::new("repo-1").unwrap();

        let dek = ks.provision_repo_dek(&s, &repo, 1).await.unwrap();
        let ct = encrypt_column(&dek, "t", "c", "r", b"pre-rotation").unwrap();

        let generation = ks.rotate_kek(&s, &repo, 2).await.unwrap();
        assert_eq!(generation, 2);

        // Re-provision unwraps under the NEW KEK and yields the SAME DEK, so
        // pre-rotation ciphertext still decrypts — the cheap-re-wrap contract.
        let dek_after = ks.provision_repo_dek(&s, &repo, 3).await.unwrap();
        assert_eq!(dek_after.expose(), dek.expose());
        let pt = decrypt_column(&dek_after, "t", "c", "r", &ct).unwrap();
        assert_eq!(pt.as_slice(), b"pre-rotation");

        // Rotation superseded, never overwrote: both generations persist.
        assert_eq!(s.count("key_metadata").await.unwrap(), 2);
        let latest = crate::latest_key_metadata(&s, "repo-1").await.unwrap().unwrap();
        assert_eq!(latest.generation, 2);
        assert!(latest.supersedes_id.is_some());
    }

    #[tokio::test]
    async fn lost_keychain_entry_fails_closed_not_silent_rekey() {
        let s = store();
        let repo = RepoId::new("repo-1").unwrap();
        let ks_a = KeyringKeyStore::new(MemoryKeychain::new());
        ks_a.provision_repo_dek(&s, &repo, 1).await.unwrap();

        // A fresh keychain (lost entry) mints a different KEK; the stored wrapped
        // DEK must NOT be silently replaced — provisioning errors toward rebuild.
        let ks_b = KeyringKeyStore::new(MemoryKeychain::new());
        assert!(matches!(
            ks_b.provision_repo_dek(&s, &repo, 2).await,
            Err(RiftError::Crypto(_))
        ));
        // No second generation was recorded.
        assert_eq!(s.count("key_metadata").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn different_repos_get_different_deks() {
        let s = store();
        let ks = KeyringKeyStore::new(MemoryKeychain::new());
        let a = ks
            .provision_repo_dek(&s, &RepoId::new("repo-a").unwrap(), 1)
            .await
            .unwrap();
        let b = ks
            .provision_repo_dek(&s, &RepoId::new("repo-b").unwrap(), 1)
            .await
            .unwrap();
        assert_ne!(a.expose(), b.expose());
    }

    #[tokio::test]
    async fn malformed_kek_in_keychain_is_rejected() {
        let s = store();
        let backend = MemoryKeychain::new();
        backend.set("rift", "repo-x", b"too-short").unwrap();
        let ks = KeyringKeyStore::new(backend);
        // DataKey has no Debug (key hygiene), so don't format the Result — just match.
        assert!(matches!(
            ks.provision_repo_dek(&s, &RepoId::new("repo-x").unwrap(), 1)
                .await,
            Err(RiftError::Crypto(_))
        ));
    }

    #[tokio::test]
    async fn rotate_without_provision_is_an_error() {
        let s = store();
        let ks = KeyringKeyStore::new(MemoryKeychain::new());
        assert!(matches!(
            ks.rotate_kek(&s, &RepoId::new("repo-1").unwrap(), 1).await,
            Err(RiftError::Crypto(_))
        ));
    }
}
