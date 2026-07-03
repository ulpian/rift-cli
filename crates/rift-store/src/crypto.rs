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

/// An in-memory key store: holds an already-provisioned DEK for the process
/// lifetime. Used for tests/headless environments AND as the production holder
/// after [`crate::KeyringKeyStore::provision_repo_dek`] unwraps the envelope.
/// Never writes a plaintext key to disk.
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

/// Framing byte for a plaintext (uncustodied) sensitive value.
const FRAME_PLAIN: u8 = 0x00;
/// Framing byte for an AEAD-encrypted sensitive value.
const FRAME_AEAD: u8 = 0x01;

/// Value-level cipher seam for sensitive columns whose write/read paths live in
/// crates that must stay key-blind (e.g. the recall ledger's digest bodies).
/// The stored form is self-describing (`frame ‖ payload`) so plain rows written
/// before key custody was available and encrypted rows coexist; which frames a
/// given implementation *accepts* is its policy.
pub trait ColumnCipher: Send + Sync {
    /// Seal `plaintext` for the `(table, column, row_id)` slot into its stored form.
    ///
    /// # Errors
    /// Implementation-defined (RNG/AEAD/key custody failure).
    fn encrypt(&self, table: &str, column: &str, row_id: &str, plaintext: &[u8])
        -> Result<Vec<u8>>;

    /// Recover the plaintext of a stored value from its slot.
    ///
    /// # Errors
    /// Implementation-defined (tamper, relocation, missing key custody).
    fn decrypt(&self, table: &str, column: &str, row_id: &str, stored: &[u8]) -> Result<Vec<u8>>;
}

/// The no-custody cipher: stores plaintext (framed). For headless environments
/// with no OS keychain — the documented residual-exposure path — and tests. It
/// refuses to read encrypted values (no key), failing closed rather than
/// returning ciphertext as data.
pub struct PlainCipher;

impl ColumnCipher for PlainCipher {
    fn encrypt(
        &self,
        _table: &str,
        _column: &str,
        _row_id: &str,
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let mut out = Vec::with_capacity(1 + plaintext.len());
        out.push(FRAME_PLAIN);
        out.extend_from_slice(plaintext);
        Ok(out)
    }

    fn decrypt(&self, table: &str, column: &str, _row_id: &str, stored: &[u8]) -> Result<Vec<u8>> {
        match stored.split_first() {
            Some((&FRAME_PLAIN, rest)) => Ok(rest.to_vec()),
            Some((&FRAME_AEAD, _)) => Err(RiftError::crypto(format!(
                "{table}.{column} is encrypted but no key custody is available \
                 (OS keychain required)"
            ))),
            _ => Err(RiftError::crypto(format!(
                "{table}.{column} has an unrecognized storage frame"
            ))),
        }
    }
}

/// The AEAD cipher over a provisioned per-repo DEK: encrypts on write, decrypts
/// on read, and transparently passes through plain-framed rows written before
/// key custody was available (the projection is rebuildable, so re-ingesting
/// upgrades them).
pub struct DekCipher<K: KeyStore> {
    keys: K,
    repo_id: RepoId,
}

impl<K: KeyStore> DekCipher<K> {
    /// A cipher keyed for `repo_id` via `keys`.
    pub fn new(keys: K, repo_id: RepoId) -> Self {
        Self { keys, repo_id }
    }
}

impl<K: KeyStore + Send + Sync> ColumnCipher for DekCipher<K> {
    fn encrypt(
        &self,
        table: &str,
        column: &str,
        row_id: &str,
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let dek = self.keys.repo_dek(&self.repo_id)?;
        let mut out = Vec::with_capacity(1 + NONCE_LEN + plaintext.len() + 16);
        out.push(FRAME_AEAD);
        out.extend(encrypt_column(&dek, table, column, row_id, plaintext)?);
        Ok(out)
    }

    fn decrypt(&self, table: &str, column: &str, row_id: &str, stored: &[u8]) -> Result<Vec<u8>> {
        match stored.split_first() {
            Some((&FRAME_AEAD, rest)) => {
                let dek = self.keys.repo_dek(&self.repo_id)?;
                Ok(decrypt_column(&dek, table, column, row_id, rest)?
                    .as_slice()
                    .to_vec())
            }
            Some((&FRAME_PLAIN, rest)) => Ok(rest.to_vec()),
            _ => Err(RiftError::crypto(format!(
                "{table}.{column} has an unrecognized storage frame"
            ))),
        }
    }
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
    fn cipher_seam_roundtrips_and_frames_interoperate() {
        let enc = DekCipher::new(MemoryKeyStore::new(dek()), RepoId::new("r").unwrap());
        let plain = PlainCipher;

        // AEAD round-trip; stored form is framed ciphertext.
        let stored = enc
            .encrypt("digest_blob", "bytes", "ref1", b"payload")
            .unwrap();
        assert_eq!(stored[0], FRAME_AEAD);
        assert!(!stored.windows(7).any(|w| w == b"payload"));
        assert_eq!(
            enc.decrypt("digest_blob", "bytes", "ref1", &stored)
                .unwrap(),
            b"payload"
        );

        // Relocation fails (AAD binds the slot).
        assert!(enc
            .decrypt("digest_blob", "bytes", "ref2", &stored)
            .is_err());

        // Plain frame: written by PlainCipher, readable by both.
        let plain_stored = plain
            .encrypt("digest_blob", "bytes", "ref1", b"payload")
            .unwrap();
        assert_eq!(plain_stored[0], FRAME_PLAIN);
        assert_eq!(
            plain
                .decrypt("digest_blob", "bytes", "ref1", &plain_stored)
                .unwrap(),
            b"payload"
        );
        assert_eq!(
            enc.decrypt("digest_blob", "bytes", "ref1", &plain_stored)
                .unwrap(),
            b"payload"
        );

        // Encrypted frame without custody fails closed; junk frames fail everywhere.
        assert!(plain
            .decrypt("digest_blob", "bytes", "ref1", &stored)
            .is_err());
        assert!(plain.decrypt("t", "c", "r", &[0x7f, 1, 2]).is_err());
        assert!(enc.decrypt("t", "c", "r", &[]).is_err());
    }

    #[test]
    fn memory_keystore_returns_repo_dek() {
        let ks = MemoryKeyStore::new(dek());
        let repo = RepoId::new("repo-1").unwrap();
        let k = ks.repo_dek(&repo).unwrap();
        assert_eq!(k.expose(), dek().expose());
    }

    #[test]
    fn generated_memory_keystore_roundtrips_a_column() {
        let ks = MemoryKeyStore::generate().unwrap();
        let repo = RepoId::new("repo-x").unwrap();
        let k = ks.repo_dek(&repo).unwrap();
        let ct = encrypt_column(&k, "t", "c", "r", b"hi").unwrap();
        let pt = decrypt_column(&k, "t", "c", "r", &ct).unwrap();
        assert_eq!(pt.as_slice(), b"hi");
    }
}
