//! Rift's cryptographic core.
//!
//! Provides authenticated encryption ([`seal`]/[`open`], ChaCha20-Poly1305), an
//! envelope key hierarchy (KEK → org DEK → repo DEK → purpose subkeys via HKDF),
//! and the secret-hygiene types that make it *impossible* to accidentally log or
//! serialize a key.
//!
//! Security-as-types (`05-security` §5.4/§5.7): [`DataKey`] and
//! [`SensitiveBytes`] derive **no** `Debug`/`Display`/`Serialize` and are
//! zeroized on drop, so `format!("{key:?}")`, `tracing::info!(?key)`, and
//! `serde_json::to_string(&key)` simply do not compile. [`RiftError::Crypto`]
//! carries a content-free category — never key or plaintext bytes.
//!
//! Nonce discipline: 96-bit nonces, unique per `(key, message)`. [`Nonce::random`]
//! draws from the OS CSPRNG; a fixed nonce is catastrophic for ChaCha20-Poly1305
//! and the API never encourages one.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce as CcNonce};
use hkdf::Hkdf;
use rift_core::{RepoId, Result, RiftError};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// The symmetric key size for ChaCha20-Poly1305 (256-bit).
pub const KEY_LEN: usize = 32;
/// The nonce size for ChaCha20-Poly1305 (96-bit).
pub const NONCE_LEN: usize = 12;

/// A symmetric data key. **No** `Debug`/`Display`/`Serialize`; zeroized on drop.
/// Read raw bytes only through [`DataKey::expose`] (a greppable, audited seam).
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DataKey([u8; KEY_LEN]);

impl DataKey {
    /// Construct from raw key bytes.
    #[must_use]
    pub fn from_bytes(bytes: [u8; KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// Generate a fresh random key from the OS CSPRNG.
    ///
    /// # Errors
    /// [`RiftError::Crypto`] if the OS RNG is unavailable.
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; KEY_LEN];
        getrandom::getrandom(&mut bytes).map_err(|_| RiftError::crypto("os rng unavailable"))?;
        Ok(Self(bytes))
    }

    /// Expose the raw key bytes. The single audited place keys are read; every
    /// call site is reviewed.
    #[must_use]
    pub fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

/// Plaintext that must be wiped after use (returned by [`open`]). No `Debug`.
#[derive(Zeroize, ZeroizeOnDrop, Clone, PartialEq, Eq)]
pub struct SensitiveBytes(Vec<u8>);

impl SensitiveBytes {
    /// Wrap bytes that should be zeroized on drop.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrow the plaintext bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

/// A 96-bit AEAD nonce.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Nonce([u8; NONCE_LEN]);

impl Nonce {
    /// Construct from raw nonce bytes (caller guarantees uniqueness per key).
    #[must_use]
    pub fn from_bytes(bytes: [u8; NONCE_LEN]) -> Self {
        Self(bytes)
    }

    /// The raw nonce bytes (stored alongside ciphertext).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8; NONCE_LEN] {
        &self.0
    }

    /// A fresh random nonce from the OS CSPRNG.
    ///
    /// # Errors
    /// [`RiftError::Crypto`] if the OS RNG is unavailable.
    pub fn random() -> Result<Self> {
        let mut bytes = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut bytes).map_err(|_| RiftError::crypto("os rng unavailable"))?;
        Ok(Self(bytes))
    }
}

/// A purpose label for a derived subkey (domain separation).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Purpose {
    /// Per-column database encryption.
    DbColumn,
    /// Hash-bound summary body encryption.
    SummaryBody,
    /// A caller-named purpose.
    Custom(&'static str),
}

impl Purpose {
    fn label(self) -> &'static str {
        match self {
            Self::DbColumn => "rift:purpose:db-column",
            Self::SummaryBody => "rift:purpose:summary-body",
            Self::Custom(s) => s,
        }
    }
}

fn cipher(key: &DataKey) -> ChaCha20Poly1305 {
    ChaCha20Poly1305::new(Key::from_slice(key.expose()))
}

/// Encrypt `plaintext` with ChaCha20-Poly1305. `aad` binds the ciphertext to its
/// context (e.g. `table/column/row_id`) so it cannot be relocated. Returns
/// `ciphertext ‖ tag`.
///
/// # Errors
/// [`RiftError::Crypto`] if encryption fails.
pub fn seal(key: &DataKey, nonce: &Nonce, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    cipher(key)
        .encrypt(
            CcNonce::from_slice(nonce.as_bytes()),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| RiftError::crypto("aead seal failed"))
}

/// Decrypt and authenticate. Verifies the tag **and** the AAD; any tamper, wrong
/// key/nonce, or AAD mismatch fails closed.
///
/// # Errors
/// [`RiftError::Crypto`] on any authentication failure (carries no bytes).
pub fn open(key: &DataKey, nonce: &Nonce, ciphertext: &[u8], aad: &[u8]) -> Result<SensitiveBytes> {
    let pt = cipher(key)
        .decrypt(
            CcNonce::from_slice(nonce.as_bytes()),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| RiftError::crypto("aead open failed: tag or aad mismatch"))?;
    Ok(SensitiveBytes::new(pt))
}

/// Derive a per-repo DEK from an org DEK: `HKDF-SHA256(salt=repo_id, ikm=org_dek)`.
///
/// Deterministic and domain-separated; org scope is forward-looking for Stage 7
/// sync, repo scope dominates locally.
///
/// # Errors
/// [`RiftError::Crypto`] if HKDF expansion fails.
pub fn derive_repo_dek(org_dek: &DataKey, repo_id: &RepoId) -> Result<DataKey> {
    let hk = Hkdf::<Sha256>::new(Some(repo_id.as_str().as_bytes()), org_dek.expose());
    let mut okm = [0u8; KEY_LEN];
    hk.expand(b"rift:repo-dek", &mut okm)
        .map_err(|_| RiftError::crypto("hkdf expand failed"))?;
    Ok(DataKey::from_bytes(okm))
}

/// Derive a per-purpose subkey from a repo DEK: domain-separated by [`Purpose`].
///
/// # Errors
/// [`RiftError::Crypto`] if HKDF expansion fails.
pub fn derive_purpose_subkey(repo_dek: &DataKey, purpose: Purpose) -> Result<DataKey> {
    let hk = Hkdf::<Sha256>::new(None, repo_dek.expose());
    let mut okm = [0u8; KEY_LEN];
    hk.expand(purpose.label().as_bytes(), &mut okm)
        .map_err(|_| RiftError::crypto("hkdf expand failed"))?;
    Ok(DataKey::from_bytes(okm))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> DataKey {
        DataKey::from_bytes([7u8; KEY_LEN])
    }

    #[test]
    fn seal_open_roundtrip() {
        let n = Nonce::from_bytes([1u8; NONCE_LEN]);
        let aad = b"table:summary/col:body/row:42";
        let ct = seal(&key(), &n, b"hello world", aad).unwrap();
        assert_ne!(ct.as_slice(), b"hello world");
        let pt = open(&key(), &n, &ct, aad).unwrap();
        assert_eq!(pt.as_slice(), b"hello world");
    }

    #[test]
    fn tamper_or_aad_mismatch_fails_open() {
        let n = Nonce::from_bytes([1u8; NONCE_LEN]);
        let aad = b"row:A";
        let mut ct = seal(&key(), &n, b"secret", aad).unwrap();

        // wrong AAD (relocation attempt)
        assert!(matches!(
            open(&key(), &n, &ct, b"row:B"),
            Err(RiftError::Crypto(_))
        ));
        // wrong key
        let other = DataKey::from_bytes([8u8; KEY_LEN]);
        assert!(matches!(
            open(&other, &n, &ct, aad),
            Err(RiftError::Crypto(_))
        ));
        // wrong nonce
        let n2 = Nonce::from_bytes([2u8; NONCE_LEN]);
        assert!(matches!(
            open(&key(), &n2, &ct, aad),
            Err(RiftError::Crypto(_))
        ));
        // flipped ciphertext byte
        ct[0] ^= 0xFF;
        assert!(matches!(
            open(&key(), &n, &ct, aad),
            Err(RiftError::Crypto(_))
        ));
    }

    #[test]
    fn derive_repo_dek_is_deterministic_and_domain_separated() {
        let org = key();
        let repo_a = RepoId::new("repo-a").unwrap();
        let repo_b = RepoId::new("repo-b").unwrap();
        let k1 = derive_repo_dek(&org, &repo_a).unwrap();
        let k2 = derive_repo_dek(&org, &repo_a).unwrap();
        let k3 = derive_repo_dek(&org, &repo_b).unwrap();
        assert_eq!(k1.expose(), k2.expose(), "same inputs → same subkey");
        assert_ne!(
            k1.expose(),
            k3.expose(),
            "different repo → different subkey"
        );

        let col = derive_purpose_subkey(&k1, Purpose::DbColumn).unwrap();
        let sum = derive_purpose_subkey(&k1, Purpose::SummaryBody).unwrap();
        assert_ne!(
            col.expose(),
            sum.expose(),
            "different purpose → different subkey"
        );
        assert_ne!(col.expose(), k1.expose(), "subkey != parent");
    }

    #[test]
    fn generate_produces_distinct_keys_and_nonces() {
        let a = DataKey::generate().unwrap();
        let b = DataKey::generate().unwrap();
        assert_ne!(a.expose(), b.expose());
        let n1 = Nonce::random().unwrap();
        let n2 = Nonce::random().unwrap();
        assert_ne!(n1, n2);
    }

    proptest::proptest! {
        /// Confidentiality+integrity primitive: open(seal(x)) == x for any input.
        #[test]
        fn aead_roundtrip_any_plaintext(
            plaintext in proptest::collection::vec(proptest::num::u8::ANY, 0..512),
            aad in proptest::collection::vec(proptest::num::u8::ANY, 0..64),
            nonce_byte in proptest::num::u8::ANY,
        ) {
            let n = Nonce::from_bytes([nonce_byte; NONCE_LEN]);
            let ct = seal(&key(), &n, &plaintext, &aad).unwrap();
            let pt = open(&key(), &n, &ct, &aad).unwrap();
            proptest::prop_assert_eq!(pt.as_slice(), plaintext.as_slice());
        }
    }
}
