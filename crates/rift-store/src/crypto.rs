//! Application-layer column encryption (defense in depth, "Layer 2").
//!
//! Whole-database encryption ("Layer 1", Turso's native option keyed by an
//! OS-keychain KEK) is a documented seam pending the production Turso store. This
//! module implements the application-layer envelope encryption of the
//! highest-sensitivity columns via `rift-crypto`'s AEAD, so a plaintext leak of
//! the DB file (a backup, a Turso-encryption gap) still does not expose them.
//!
//! Each ciphertext is `nonce ‖ AEAD(plaintext)` with `aad = table/column/row_id`,
//! binding it to its location so a relocated/copied ciphertext fails
//! authentication (anti-relocation). Keys come from the [`KeyStore`] seam — a
//! [`MemoryKeyStore`] for tests/headless, an OS-keychain-backed store in
//! production (the `keyring`-backed impl is the swappable production variant).

use rift_core::{RepoId, Result, RiftError};
use rift_crypto::{open, seal, DataKey, Nonce, SensitiveBytes, NONCE_LEN};

/// Custody seam for the per-repo data key used to encrypt sensitive columns.
pub trait KeyStore {
    /// The per-repo data key (derived/unwrapped from the envelope hierarchy).
    ///
    /// # Errors
    /// Implementation-defined (e.g. keychain unavailable).
    fn repo_dek(&self, repo_id: &RepoId) -> Result<DataKey>;
}

/// An in-memory key store for tests and headless/CI environments where no OS
/// keychain exists. Never writes a plaintext key to disk.
pub struct MemoryKeyStore {
    dek: DataKey,
}

impl MemoryKeyStore {
    /// Wrap an existing key.
    #[must_use]
    pub fn new(dek: DataKey) -> Self {
        Self { dek }
    }

    /// Generate a fresh random key.
    ///
    /// # Errors
    /// [`RiftError::Crypto`] if the OS RNG is unavailable.
    pub fn generate() -> Result<Self> {
        Ok(Self {
            dek: DataKey::generate()?,
        })
    }
}

impl KeyStore for MemoryKeyStore {
    fn repo_dek(&self, _repo_id: &RepoId) -> Result<DataKey> {
        Ok(self.dek.clone())
    }
}

fn aad(table: &str, column: &str, row_id: &str) -> Vec<u8> {
    format!("{table}/{column}/{row_id}").into_bytes()
}

/// Encrypt a sensitive column value: returns `nonce ‖ ciphertext`, bound to its
/// `(table, column, row_id)` location via the AAD.
///
/// # Errors
/// [`RiftError::Crypto`] on RNG or AEAD failure.
pub fn encrypt_column(
    key: &DataKey,
    table: &str,
    column: &str,
    row_id: &str,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let nonce = Nonce::random()?;
    let mut out = nonce.as_bytes().to_vec();
    out.extend(seal(key, &nonce, plaintext, &aad(table, column, row_id))?);
    Ok(out)
}

/// Decrypt a sensitive column value produced by [`encrypt_column`]. Fails if the
/// ciphertext is moved to a different `(table, column, row_id)`.
///
/// # Errors
/// [`RiftError::Crypto`] on a too-short blob, tamper, or AAD/location mismatch.
pub fn decrypt_column(
    key: &DataKey,
    table: &str,
    column: &str,
    row_id: &str,
    blob: &[u8],
) -> Result<SensitiveBytes> {
    if blob.len() < NONCE_LEN {
        return Err(RiftError::crypto("ciphertext too short"));
    }
    let (n, ct) = blob.split_at(NONCE_LEN);
    let mut nb = [0u8; NONCE_LEN];
    nb.copy_from_slice(n);
    open(key, &Nonce::from_bytes(nb), ct, &aad(table, column, row_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dek() -> DataKey {
        DataKey::from_bytes([3u8; 32])
    }

    #[test]
    fn encrypts_and_decrypts_sensitive_column_roundtrip() {
        let k = dek();
        let ct = encrypt_column(&k, "cost_fact", "amount_micros", "row1", b"sensitive$$$").unwrap();
        let pt = decrypt_column(&k, "cost_fact", "amount_micros", "row1", &ct).unwrap();
        assert_eq!(pt.as_slice(), b"sensitive$$$");
    }

    #[test]
    fn db_bytes_do_not_contain_plaintext_sentinel() {
        let sentinel = b"SUPER-SECRET-SENTINEL";
        let ct = encrypt_column(&dek(), "t", "c", "r", sentinel).unwrap();
        assert!(
            !ct.windows(sentinel.len()).any(|w| w == sentinel),
            "ciphertext must not contain the plaintext sentinel"
        );
    }

    #[test]
    fn aad_mismatch_fails_decrypt() {
        let k = dek();
        let ct = encrypt_column(&k, "t", "c", "rowA", b"x").unwrap();
        // relocation to rowB must fail
        assert!(matches!(
            decrypt_column(&k, "t", "c", "rowB", &ct),
            Err(RiftError::Crypto(_))
        ));
        // short blob
        assert!(matches!(
            decrypt_column(&k, "t", "c", "rowA", b"short"),
            Err(RiftError::Crypto(_))
        ));
    }

    #[test]
    fn memory_keystore_returns_repo_dek() {
        let ks = MemoryKeyStore::new(dek());
        let repo = RepoId::new("repo-1").unwrap();
        let k = ks.repo_dek(&repo).unwrap();
        assert_eq!(k.expose(), dek().expose());
    }
}
