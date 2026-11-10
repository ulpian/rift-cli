//! Detached Ed25519 signature verification.
//!
//! The release tooling signs each download manifest with a key that never
//! leaves the owner's secret store; installed binaries carry only the public
//! key and check the signature before trusting the manifest's checksums. A
//! manifest and its checksums live in the same bucket, so without a signature
//! anyone able to write there could replace both together.

use ed25519_dalek::{Signature, VerifyingKey};

/// The size of an Ed25519 public key.
pub const ED25519_PUBLIC_KEY_LEN: usize = 32;
/// The size of an Ed25519 signature.
pub const ED25519_SIGNATURE_LEN: usize = 64;

/// Whether `signature` is `public_key`'s signature over exactly `message`.
///
/// Strict verification: it also refuses the malleable and small-order forms
/// that plain RFC 8032 verification lets through, so one message has one
/// accepted signature. Any malformed key is simply "not verified".
#[must_use]
pub fn verify_ed25519(
    public_key: &[u8; ED25519_PUBLIC_KEY_LEN],
    message: &[u8],
    signature: &[u8; ED25519_SIGNATURE_LEN],
) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    key.verify_strict(message, &Signature::from_bytes(signature))
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer as _, SigningKey};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    #[test]
    fn a_signature_verifies_only_for_its_own_message_and_key() {
        let signer = key();
        let public = signer.verifying_key().to_bytes();
        let signature = signer.sign(b"rift-download-v1\tv1.2.0-alpha\n").to_bytes();
        assert!(verify_ed25519(
            &public,
            b"rift-download-v1\tv1.2.0-alpha\n",
            &signature
        ));
        assert!(!verify_ed25519(
            &public,
            b"rift-download-v1\tv1.0.0-alpha\n",
            &signature
        ));
        let other = SigningKey::from_bytes(&[8u8; 32])
            .verifying_key()
            .to_bytes();
        assert!(!verify_ed25519(
            &other,
            b"rift-download-v1\tv1.2.0-alpha\n",
            &signature
        ));
    }

    #[test]
    fn a_damaged_signature_or_key_is_not_verified() {
        let signer = key();
        let public = signer.verifying_key().to_bytes();
        let mut signature = signer.sign(b"manifest").to_bytes();
        signature[0] ^= 1;
        assert!(!verify_ed25519(&public, b"manifest", &signature));
        assert!(!verify_ed25519(&[0u8; 32], b"manifest", &[0u8; 64]));
    }
}
