//! OS-keychain-backed envelope key custody (the "Layer 1" KEK seam).
//!
//! The per-repo **KEK** lives in the OS keychain (never on disk); the per-repo
//! **DEK** used to encrypt sensitive columns ([`crate::crypto`]) is HKDF-derived
//! from it via [`rift_crypto::derive_repo_dek`]. So the envelope round-trips
//! through the keychain: store KEK → reload KEK → derive DEK → encrypt/decrypt.
//!
//! The keychain itself is behind the [`KeychainBackend`] seam — a
//! [`MemoryKeychain`] for tests and headless/CI (no keychain available), and the
//! real [`OsKeychain`] behind the `keyring` feature. The round-trip logic is
//! therefore fully testable without a real keychain, and the OS engine plugs in
//! without touching callers (the project's seam discipline).

use std::collections::HashMap;
use std::sync::Mutex;

use rift_core::{RepoId, Result, RiftError};
use rift_crypto::{derive_repo_dek, DataKey, KEY_LEN};

use crate::crypto::KeyStore;

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

/// A [`KeyStore`] whose per-repo KEK is custodied by a [`KeychainBackend`]. The
/// KEK is created once (random) and reloaded thereafter; the DEK is derived from
/// it, so the key material round-trips through the keychain and never hits disk.
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
}

impl<B: KeychainBackend> KeyStore for KeyringKeyStore<B> {
    fn repo_dek(&self, repo_id: &RepoId) -> Result<DataKey> {
        let kek = self.load_or_create_kek(repo_id)?;
        derive_repo_dek(&kek, repo_id)
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

    #[test]
    fn kek_loaded_from_keyring_not_disk() {
        let ks = KeyringKeyStore::new(MemoryKeychain::new());
        let repo = RepoId::new("repo-1").unwrap();

        // First call creates + stores the KEK; second reloads it. Equal DEKs prove
        // the KEK was persisted in the keychain and re-derived identically — not
        // regenerated (which would yield a different key).
        let dek1 = ks.repo_dek(&repo).unwrap();
        let dek2 = ks.repo_dek(&repo).unwrap();
        assert_eq!(dek1.expose(), dek2.expose());

        // The derived DEK actually encrypts and decrypts a column (full envelope
        // round-trip through the keychain-custodied KEK).
        let ct = encrypt_column(&dek1, "cost_fact", "amount_micros", "r1", b"secret$$$").unwrap();
        let pt = decrypt_column(&dek2, "cost_fact", "amount_micros", "r1", &ct).unwrap();
        assert_eq!(pt.as_slice(), b"secret$$$");
    }

    #[test]
    fn different_repos_get_different_keys() {
        let ks = KeyringKeyStore::new(MemoryKeychain::new());
        let a = ks.repo_dek(&RepoId::new("repo-a").unwrap()).unwrap();
        let b = ks.repo_dek(&RepoId::new("repo-b").unwrap()).unwrap();
        assert_ne!(a.expose(), b.expose());
    }

    #[test]
    fn malformed_kek_in_keychain_is_rejected() {
        let backend = MemoryKeychain::new();
        backend.set("rift", "repo-x", b"too-short").unwrap();
        let ks = KeyringKeyStore::new(backend);
        // DataKey has no Debug (key hygiene), so don't format the Result — just match.
        assert!(matches!(
            ks.repo_dek(&RepoId::new("repo-x").unwrap()),
            Err(RiftError::Crypto(_))
        ));
    }
}
