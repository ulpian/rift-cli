//! The one-way redaction gate, expressed as types.
//!
//! Nothing reaches durable storage or sync except through the redaction
//! pipeline's *output* ([`RedactedBytes`]). This is the **compile-time** face of
//! the security model's one-way gate (`05-security` §5.3/§5.4):
//!
//! * [`SecretBytes`] — raw, untrusted, **local-only** input. Zeroized on drop,
//!   no `Debug`/`Display`. It is what shadow branches and the redaction pipeline
//!   accept; it can never be handed to a durable-write API.
//! * [`RedactedBytes`] — bytes that provably passed redaction. Its byte buffer is
//!   a **private** field, so it cannot be fabricated by a struct literal, and the
//!   only constructor is [`RedactedBytes::seal`], whose single audited call site
//!   is `rift-shadow-redact::redact`. Durable-write/sync APIs accept only
//!   `&RedactedBytes`, so raw bytes cannot be persisted — the compiler rejects it.

use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::Sha256Id;

/// Raw, untrusted, local-only bytes (a transcript fragment, a prompt). Zeroized
/// on drop; deliberately has no `Debug`/`Display`/`Serialize`, so it cannot leak
/// into a log line, and no API on a durable surface accepts it.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct SecretBytes(Vec<u8>);

impl SecretBytes {
    /// Wrap raw bytes that must be redacted before they can be persisted.
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    /// Borrow the raw bytes (for the redaction pipeline only).
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }

    /// The raw length in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether the buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<&str> for SecretBytes {
    fn from(s: &str) -> Self {
        Self(s.as_bytes().to_vec())
    }
}

/// Provenance recorded with every [`RedactedBytes`]: exactly which rule-pack
/// versions cleared the bytes, the hash of the raw input (for recompute-verify),
/// and the hash of the cleared bytes (chains into the tamper chain). Holds only
/// versions and hashes — never a secret — so deriving `Debug` is safe.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedactionProvenance {
    /// The rule-pack `name@version` strings that ran (provenance).
    pub rule_pack_versions: Vec<String>,
    /// SHA-256 of the **raw** pre-redaction input.
    pub input_sha256: Sha256Id,
    /// SHA-256 of the cleared (redacted) bytes.
    pub redacted_sha256: Sha256Id,
}

/// Bytes that have provably passed the redaction pipeline.
///
/// The inner buffer is private (no struct-literal fabrication) and the only
/// constructor is [`RedactedBytes::seal`]. There is intentionally no
/// `From<Vec<u8>>`/`From<&str>` and no mutable accessor — read-only views only.
/// `Debug` is **not** derived: redacted ≠ non-sensitive, so the bytes never land
/// in a log line by accident.
#[derive(Clone)]
pub struct RedactedBytes {
    bytes: Vec<u8>,
    provenance: RedactionProvenance,
}

impl RedactedBytes {
    /// The sealed constructor. **Call only** from `rift-shadow-redact::redact`
    /// (a single audited call site, CI-enforced) with the pipeline's masked
    /// output and the provenance it computed. The private field makes this the
    /// sole way to mint a `RedactedBytes`.
    #[must_use]
    pub fn seal(bytes: Vec<u8>, provenance: RedactionProvenance) -> Self {
        Self { bytes, provenance }
    }

    /// A read-only view of the cleared bytes (durable writers need to read them).
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    /// The redaction provenance (rule-pack versions + hashes).
    #[must_use]
    pub fn provenance(&self) -> &RedactionProvenance {
        &self.provenance
    }

    /// The SHA-256 of the cleared bytes (as recorded in provenance).
    #[must_use]
    pub fn redacted_sha256(&self) -> Sha256Id {
        self.provenance.redacted_sha256
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_bytes_roundtrip() {
        let s = SecretBytes::from("raw secret");
        assert_eq!(s.as_slice(), b"raw secret");
        assert!(!s.is_empty());
        assert_eq!(s.len(), 10);
    }

    #[test]
    fn redacted_bytes_round_trips_provenance() {
        let prov = RedactionProvenance {
            rule_pack_versions: vec!["builtin@2026.06.0".into()],
            input_sha256: Sha256Id::from_bytes([1; 32]),
            redacted_sha256: Sha256Id::from_bytes([2; 32]),
        };
        let r = RedactedBytes::seal(b"cleared".to_vec(), prov.clone());
        assert_eq!(r.as_slice(), b"cleared");
        assert_eq!(r.provenance(), &prov);
        assert_eq!(r.redacted_sha256(), Sha256Id::from_bytes([2; 32]));
    }
}
