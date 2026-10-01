//! The single shared error type and result alias.
//!
//! Variants are added per subsystem. Each carries enough context to be
//! actionable without leaking secrets (see the repository's `SECURITY.md`).
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

    /// A projection lease excludes replacement or a concurrent store open.
    #[error("projection busy: {projection}: {reason}")]
    ProjectionBusy {
        /// The guarded projection identifier.
        projection: String,
        /// The content-free reason the lease cannot be acquired.
        reason: String,
    },

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

    /// Every variant's Display/Debug output for a given reason string.
    fn all_variants(reason: &str) -> Vec<RiftError> {
        vec![
            RiftError::Immutability(reason.into()),
            RiftError::Validity(reason.into()),
            RiftError::PolicyDenied(reason.into()),
            RiftError::Storage(reason.into()),
            RiftError::ProjectionBusy {
                projection: "projection".into(),
                reason: reason.into(),
            },
            RiftError::Substrate(reason.into()),
            RiftError::Migration(reason.into()),
            RiftError::Config(reason.into()),
            RiftError::Crypto(reason.into()),
            RiftError::Integrity(reason.into()),
            RiftError::NotFound(reason.into()),
            RiftError::Io(std::io::Error::other(reason.to_string())),
            RiftError::Serde(reason.into()),
            RiftError::Other(reason.into()),
        ]
    }

    /// The `error_display_contains_no_byte_payload` contract, exhaustive
    /// over EVERY variant: the rendered error is exactly
    /// `<prefix><reason>` — the reason a call site passes is the *only* content
    /// channel, so nothing the caller did not deliberately format in (key
    /// bytes, transcript fragments, a secret sentinel from elsewhere) can leak
    /// through the error type.
    #[test]
    fn error_display_contains_no_byte_payload() {
        let reason = "reason-7f3a";
        let secret_sentinel = "SECRET-SENTINEL-NEVER-PASSED-1c9d";
        for e in all_variants(reason) {
            let display = e.to_string();
            let debug = format!("{e:?}");
            // the display is exactly prefix + the passed reason, nothing more
            assert!(
                display.ends_with(reason),
                "display must end with the passed reason: {display}"
            );
            assert!(
                display.len() <= reason.len() + 32,
                "display carries only a short static prefix: {display}"
            );
            // nothing that was never passed can appear in either rendering
            assert!(!display.contains(secret_sentinel));
            assert!(!debug.contains(secret_sentinel));
        }
    }

    /// Contract guard: the exact Display form of every variant, snapshotted so
    /// a changed (or newly leaky) rendering is a reviewable diff.
    #[test]
    fn display_forms_are_stable() {
        let rendered: Vec<String> = all_variants("<reason>")
            .iter()
            .map(ToString::to_string)
            .collect();
        insta::assert_snapshot!("riferror_display_forms", rendered.join("\n"));
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
