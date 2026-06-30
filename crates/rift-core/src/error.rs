//! The single shared error type and result alias.
//!
//! Variants are added per subsystem. Each carries enough context to be
//! actionable without leaking secrets (see the security model,
//! `build-plan/00-overview/05-security-model-and-threat-model.md` §5.4/§5.7).
//! Variants never embed key material, plaintext byte buffers, or transcript
//! contents — only identifiers, paths, and content-free reason strings — so the
//! error type can never become an exfiltration channel. The enum is
//! `#[non_exhaustive]` so new variants are not a breaking change.

/// Crate-wide result alias.
pub type Result<T, E = RiftError> = core::result::Result<T, E>;

/// The shared Rift error type.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RiftError {
    /// An append-only/immutability invariant was violated (e.g. an attempted
    /// `UPDATE`/`DELETE` on a canonical table).
    #[error("immutability violation: {0}")]
    Immutability(String),

    /// A validity or hash-binding check failed (e.g. a malformed identifier).
    #[error("validity error: {0}")]
    Validity(String),

    /// A policy gate denied the operation.
    #[error("policy denied: {0}")]
    PolicyDenied(String),

    /// A storage-layer error (the underlying engine reported a failure).
    #[error("storage error: {0}")]
    Storage(String),

    /// A substrate-reader error (reading checkpoint/shadow refs or objects from
    /// the canonical Git record). Carries only a content-free reason — never
    /// repository object bytes or transcript fragments.
    #[error("substrate error: {0}")]
    Substrate(String),

    /// A schema/migration error.
    #[error("migration error: {0}")]
    Migration(String),

    /// A configuration / policy-loading error (`rift-config`). Carries only the
    /// offending key/path/reason — never file contents.
    #[error("config error: {0}")]
    Config(String),

    /// A cryptographic failure (AEAD tamper, AAD mismatch, key derivation).
    /// **Never** embeds key bytes or plaintext — only a content-free category.
    #[error("crypto error: {0}")]
    Crypto(String),

    /// A hash-chain / Merkle / tamper-evidence mismatch. Names the offending
    /// checkpoint/index only, never leaf or payload bytes.
    #[error("integrity error: {0}")]
    Integrity(String),

    /// A requested entity was not found.
    #[error("not found: {0}")]
    NotFound(String),

    /// An I/O error from the standard library.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A (de)serialization error. Carries the parser's message, not the input.
    #[error("serialize error: {0}")]
    Serde(String),

    /// Catch-all with a human-readable message.
    #[error("{0}")]
    Other(String),
}

impl RiftError {
    /// Convenience constructor for [`RiftError::Immutability`].
    #[must_use]
    pub fn immutability(msg: impl Into<String>) -> Self {
        Self::Immutability(msg.into())
    }

    /// Convenience constructor for [`RiftError::Validity`].
    #[must_use]
    pub fn validity(msg: impl Into<String>) -> Self {
        Self::Validity(msg.into())
    }

    /// Convenience constructor for [`RiftError::Storage`].
    #[must_use]
    pub fn storage(msg: impl Into<String>) -> Self {
        Self::Storage(msg.into())
    }

    /// Convenience constructor for [`RiftError::Substrate`].
    #[must_use]
    pub fn substrate(msg: impl Into<String>) -> Self {
        Self::Substrate(msg.into())
    }

    /// Convenience constructor for [`RiftError::Config`].
    #[must_use]
    pub fn config(msg: impl Into<String>) -> Self {
        Self::Config(msg.into())
    }

    /// Convenience constructor for [`RiftError::Crypto`].
    #[must_use]
    pub fn crypto(msg: impl Into<String>) -> Self {
        Self::Crypto(msg.into())
    }

    /// Convenience constructor for [`RiftError::Integrity`].
    #[must_use]
    pub fn integrity(msg: impl Into<String>) -> Self {
        Self::Integrity(msg.into())
    }

    /// Convenience constructor for [`RiftError::PolicyDenied`].
    #[must_use]
    pub fn policy_denied(msg: impl Into<String>) -> Self {
        Self::PolicyDenied(msg.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn displays_carry_context() {
        assert_eq!(
            RiftError::immutability("no update on checkpoint").to_string(),
            "immutability violation: no update on checkpoint"
        );
        assert_eq!(RiftError::Other("x".into()).to_string(), "x");
        assert_eq!(
            RiftError::integrity("checkpoint ckpt_3 diverged").to_string(),
            "integrity error: checkpoint ckpt_3 diverged"
        );
    }

    /// The error type must never echo a "secret" sentinel as a *byte payload*;
    /// only identifiers/reasons are allowed in constructors, and crypto failures
    /// carry a content-free category (see 05 §5.4/§5.7).
    #[test]
    fn crypto_error_is_redaction_safe() {
        let e = RiftError::crypto("aead tag verification failed");
        let shown = e.to_string();
        assert!(shown.contains("aead tag verification failed"));
        assert!(!shown.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn io_error_maps_via_from() {
        let io = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let e: RiftError = io.into();
        assert!(matches!(e, RiftError::Io(_)));
    }

    #[test]
    fn is_std_error() {
        fn assert_error<E: std::error::Error>() {}
        assert_error::<RiftError>();
    }
}
