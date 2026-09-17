//! Rift's cryptographic core.
//!
//! Provides authenticated encryption ([`aead_seal`]/[`aead_open`], ChaCha20-Poly1305), an
//! envelope key hierarchy (KEK → org DEK → repo DEK → purpose subkeys via HKDF),
//! and the secret-hygiene types that make it *impossible* to accidentally log or
//! serialize a key.
//!
//! Security-as-types, the way the security model asks for: [`DataKey`] and
//! [`SensitiveBytes`] derive **no** `Debug`/`Display`/`Serialize` and are
//! zeroized on drop, so `format!("{key:?}")`, `tracing::info!(?key)`, and
//! `serde_json::to_string(&key)` simply do not compile. [`RiftError::Crypto`]
//! carries a content-free category — never key or plaintext bytes.
//!
//! Nonce discipline: 96-bit nonces, unique per `(key, message)`. [`Nonce::random`]
//! draws from the OS CSPRNG; a fixed nonce is catastrophic for ChaCha20-Poly1305
//! and the API never encourages one.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

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
///
/// The hygiene is compiler-enforced — formatting or serializing a key does not
/// compile (the `key_type_cannot_be_debug_or_serialized` proof):
///
/// ```compile_fail
/// let key = rift_crypto::DataKey::from_bytes([0u8; rift_crypto::KEY_LEN]);
/// let _ = format!("{key:?}"); // no Debug — keys never reach logs
/// ```
///
/// ```compile_fail
/// let key = rift_crypto::DataKey::from_bytes([0u8; rift_crypto::KEY_LEN]);
/// let _ = format!("{key}"); // no Display either
/// ```
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

/// Plaintext that must be wiped after use (returned by [`aead_open`]). No `Debug`.
///
/// ```compile_fail
/// let pt = rift_crypto::SensitiveBytes::new(vec![1, 2, 3]);
/// let _ = format!("{pt:?}"); // no Debug — decrypted plaintext never reaches logs
/// ```
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
pub fn aead_seal(key: &DataKey, nonce: &Nonce, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
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
pub fn aead_open(
    key: &DataKey,
    nonce: &Nonce,
    ciphertext: &[u8],
    aad: &[u8],
) -> Result<SensitiveBytes> {
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

/// Length of a wrapped key blob: `nonce ‖ AEAD(key) ‖ tag`.
pub const WRAPPED_KEY_LEN: usize = NONCE_LEN + KEY_LEN + 16;

/// Wrap (AEAD-seal) a DEK under a parent key (the envelope: KEK wraps DEK).
/// Returns `nonce ‖ ciphertext ‖ tag` ([`WRAPPED_KEY_LEN`] bytes); the wrapped
/// blob is safe to persist. `aad` binds the wrap to its context (repo id +
/// generation) so a wrapped key cannot be relocated.
///
/// # Errors
/// [`RiftError::Crypto`] on RNG or AEAD failure.
pub fn wrap_dek(parent: &DataKey, dek: &DataKey, aad: &[u8]) -> Result<Vec<u8>> {
    let nonce = Nonce::random()?;
    let mut out = nonce.as_bytes().to_vec();
    out.extend(aead_seal(parent, &nonce, dek.expose(), aad)?);
    Ok(out)
}

/// Unwrap a DEK produced by [`wrap_dek`]. Fails closed on tamper, a wrong
/// parent key, or an AAD (context) mismatch.
///
/// # Errors
/// [`RiftError::Crypto`] on length or authentication failure (carries no bytes).
pub fn unwrap_dek(parent: &DataKey, wrapped: &[u8], aad: &[u8]) -> Result<DataKey> {
    if wrapped.len() != WRAPPED_KEY_LEN {
        return Err(RiftError::crypto("wrapped key has wrong length"));
    }
    let (n, ct) = wrapped.split_at(NONCE_LEN);
    let mut nb = [0u8; NONCE_LEN];
    nb.copy_from_slice(n);
    let pt = aead_open(parent, &Nonce::from_bytes(nb), ct, aad)?;
    let mut bytes = [0u8; KEY_LEN];
    bytes.copy_from_slice(pt.as_slice());
    let dek = DataKey::from_bytes(bytes);
    bytes.zeroize();
    Ok(dek)
}

/// Derive a per-repo DEK from an org DEK: `HKDF-SHA256(salt=repo_id, ikm=org_dek)`.
///
/// Deterministic and domain-separated; org scope is forward-looking for a
/// future cloud sync surface, repo scope dominates locally.
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

/// A 32-byte tag that binds a cached derivation to the install salt it was
/// made under: `HKDF-SHA256(salt=install_salt, ikm=label, info=label)`.
///
/// For a cache whose *content* is already a salted digest (the machine id)
/// and whose *validity* depends on the salt not having moved: the tag names
/// the salt without being the salt, so a reader can refuse a cache made
/// under another install without ever holding the salt itself. One-way and
/// domain-separated by `label`; two labels never collide, two salts never
/// agree.
#[must_use]
pub fn salt_tag(install_salt: &[u8; 32], label: &str) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(install_salt), label.as_bytes());
    let mut okm = [0u8; 32];
    // A 32-byte expand from SHA-256 cannot fail (one block); the `Result` is
    // the generic API's, not a reachable arm.
    if hk.expand(label.as_bytes(), &mut okm).is_err() {
        return [0u8; 32];
    }
    okm
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cache tag is a pure function of (salt, label), one-way, and
    /// separated on both inputs.
    #[test]
    fn salt_tag_is_deterministic_and_separated_on_salt_and_label() {
        let a = salt_tag(&[1u8; 32], "machine-id-cache");
        assert_eq!(a, salt_tag(&[1u8; 32], "machine-id-cache"));
        assert_ne!(
            a,
            salt_tag(&[2u8; 32], "machine-id-cache"),
            "another install"
        );
        assert_ne!(a, salt_tag(&[1u8; 32], "other-cache"), "another purpose");
        assert_ne!(a, [0u8; 32]);
        assert_ne!(&a[..], &[1u8; 32][..], "never the salt itself");
    }

    fn key() -> DataKey {
        DataKey::from_bytes([7u8; KEY_LEN])
    }

    #[test]
    fn seal_open_roundtrip() {
        let n = Nonce::from_bytes([1u8; NONCE_LEN]);
        let aad = b"table:summary/col:body/row:42";
        let ct = aead_seal(&key(), &n, b"hello world", aad).unwrap();
        assert_ne!(ct.as_slice(), b"hello world");
        let pt = aead_open(&key(), &n, &ct, aad).unwrap();
        assert_eq!(pt.as_slice(), b"hello world");
    }

    #[test]
    fn tamper_or_aad_mismatch_fails_open() {
        let n = Nonce::from_bytes([1u8; NONCE_LEN]);
        let aad = b"row:A";
        let mut ct = aead_seal(&key(), &n, b"secret", aad).unwrap();

        // wrong AAD (relocation attempt)
        assert!(matches!(
            aead_open(&key(), &n, &ct, b"row:B"),
            Err(RiftError::Crypto(_))
        ));
        // wrong key
        let other = DataKey::from_bytes([8u8; KEY_LEN]);
        assert!(matches!(
            aead_open(&other, &n, &ct, aad),
            Err(RiftError::Crypto(_))
        ));
        // wrong nonce
        let n2 = Nonce::from_bytes([2u8; NONCE_LEN]);
        assert!(matches!(
            aead_open(&key(), &n2, &ct, aad),
            Err(RiftError::Crypto(_))
        ));
        // flipped ciphertext byte
        ct[0] ^= 0xFF;
        assert!(matches!(
            aead_open(&key(), &n, &ct, aad),
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
    fn wrap_unwrap_roundtrip_and_fail_closed() {
        let kek = DataKey::from_bytes([9u8; KEY_LEN]);
        let dek = DataKey::from_bytes([4u8; KEY_LEN]);
        let aad = b"rift:wrapped-dek:repo-1:1";
        let wrapped = wrap_dek(&kek, &dek, aad).unwrap();
        assert_eq!(wrapped.len(), WRAPPED_KEY_LEN);
        // the wrapped blob never contains the raw DEK bytes
        assert!(!wrapped.windows(KEY_LEN).any(|w| w == dek.expose()));

        let back = unwrap_dek(&kek, &wrapped, aad).unwrap();
        assert_eq!(back.expose(), dek.expose());

        // wrong parent key
        let other = DataKey::from_bytes([1u8; KEY_LEN]);
        assert!(matches!(
            unwrap_dek(&other, &wrapped, aad),
            Err(RiftError::Crypto(_))
        ));
        // relocated context (different repo/generation)
        assert!(matches!(
            unwrap_dek(&kek, &wrapped, b"rift:wrapped-dek:repo-2:1"),
            Err(RiftError::Crypto(_))
        ));
        // wrong length / truncation
        assert!(matches!(
            unwrap_dek(&kek, &wrapped[..WRAPPED_KEY_LEN - 1], aad),
            Err(RiftError::Crypto(_))
        ));
        // bit flip
        let mut tampered = wrapped;
        tampered[NONCE_LEN] ^= 0xFF;
        assert!(matches!(
            unwrap_dek(&kek, &tampered, aad),
            Err(RiftError::Crypto(_))
        ));
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
        /// Confidentiality+integrity primitive: aead_open(aead_seal(x)) == x for any input.
        #[test]
        fn aead_roundtrip_any_plaintext(
            plaintext in proptest::collection::vec(proptest::num::u8::ANY, 0..512),
            aad in proptest::collection::vec(proptest::num::u8::ANY, 0..64),
            nonce_byte in proptest::num::u8::ANY,
        ) {
            let n = Nonce::from_bytes([nonce_byte; NONCE_LEN]);
            let ct = aead_seal(&key(), &n, &plaintext, &aad).unwrap();
            let pt = aead_open(&key(), &n, &ct, &aad).unwrap();
            proptest::prop_assert_eq!(pt.as_slice(), plaintext.as_slice());
        }
    }

    /// RFC 8439 §2.8.2 known-answer vector: pins the ChaCha20-Poly1305 wiring
    /// (key/nonce/AAD layout) to the IETF reference — a transposed argument or
    /// swapped parameter produces a different ciphertext and fails here.
    #[test]
    fn aead_matches_rfc8439_vector() {
        let key_bytes: [u8; KEY_LEN] = (0x80..=0x9f).collect::<Vec<u8>>().try_into().unwrap();
        let key = DataKey::from_bytes(key_bytes);
        let nonce = Nonce::from_bytes([
            0x07, 0x00, 0x00, 0x00, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
        ]);
        let aad = [
            0x50u8, 0x51, 0x52, 0x53, 0xc0, 0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7,
        ];
        let plaintext = b"Ladies and Gentlemen of the class of '99: \
If I could offer you only one tip for the future, sunscreen would be it.";
        let ct = aead_seal(&key, &nonce, plaintext, &aad).unwrap();
        let hex: String = ct.iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        });
        assert_eq!(
            hex,
            // ciphertext ‖ tag from RFC 8439 §2.8.2
            "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6\
3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36\
92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc\
3ff4def08e4b7a9de576d26586cec64b6116\
1ae10b594f09e26a7e902ecbd0600691"
        );
        let pt = aead_open(&key, &nonce, &ct, &aad).unwrap();
        assert_eq!(pt.as_slice(), plaintext.as_slice());
    }

    /// RFC 5869 A.1 (HKDF-SHA256 test case 1), driven through the same
    /// `Hkdf::new` + `expand` sequence the derive functions use — pins the
    /// salt-vs-ikm-vs-info parameter layout.
    #[test]
    fn hkdf_matches_rfc5869_vector() {
        let ikm = [0x0bu8; 22];
        let salt: Vec<u8> = (0x00..=0x0c).collect();
        let info: Vec<u8> = (0xf0..=0xf9).collect();
        let hk = Hkdf::<Sha256>::new(Some(&salt), &ikm);
        let mut okm = [0u8; 42];
        hk.expand(&info, &mut okm).unwrap();
        let hex: String = okm.iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        });
        assert_eq!(
            hex,
            "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf\
34007208d5b887185865"
        );
    }

    /// Freezes the derivation layout of [`derive_repo_dek`] (salt = repo id,
    /// info = "rift:repo-dek"): swapping salt and info would pass every
    /// self-referential test but changes this pinned output.
    #[test]
    fn derive_repo_dek_layout_is_pinned() {
        let org = DataKey::from_bytes([7u8; KEY_LEN]);
        let repo = RepoId::new("repo-a").unwrap();
        let dek = derive_repo_dek(&org, &repo).unwrap();
        let hex: String = dek.expose().iter().fold(String::new(), |mut s, b| {
            use std::fmt::Write as _;
            let _ = write!(s, "{b:02x}");
            s
        });
        assert_eq!(
            hex,
            "d5c755c6a187503a06d1369e5276654603161c41c6dde4a66ad6c975ca012aeb"
        );
    }

    /// Random nonces never collide over a large batch (the spec's
    /// `random_nonces_are_unique` requirement; 96-bit space, 10k draws).
    #[test]
    fn random_nonces_are_unique() {
        let mut seen = std::collections::HashSet::with_capacity(10_000);
        for _ in 0..10_000 {
            assert!(
                seen.insert(*Nonce::random().unwrap().as_bytes()),
                "nonce collision — CSPRNG output repeated"
            );
        }
    }

    /// The zeroize contract is real, not just derived: wiping a clone zeroes
    /// every byte (drop-order wiping itself is exercised under miri in CI).
    #[test]
    fn key_and_plaintext_zeroize_to_all_zero() {
        let mut key = DataKey::from_bytes([0xAAu8; KEY_LEN]);
        key.zeroize();
        assert_eq!(key.expose(), &[0u8; KEY_LEN]);

        let mut pt = SensitiveBytes::new(vec![0xBBu8; 64]);
        pt.zeroize();
        assert!(pt.as_slice().is_empty() || pt.as_slice().iter().all(|&b| b == 0));
    }
}
