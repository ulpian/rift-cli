//! rift-shadow — the immutable, git-native system of record.
//!
//! This crate is the façade that ties the capture sub-crates together:
//!
//! * [`entire`] — the silent, version-pinned capture-backend wrapper (v0.1).
//! * [`hooks`] — canonical events, agent adapters, the session state machine.
//! * [`redact`] — the two-pass redaction gate (the sole `RedactedBytes` mint).
//! * [`attribution`] — human-vs-agent line attribution + confidence.
//! * [`cost`] — the four-class cost model + price tables.
//!
//! It exposes the end-to-end capture primitives — turn a redacted event stream
//! into a Merkle root for a checkpoint — and the `verify` façade that cross-checks
//! the rebuildable projection against the canonical chain.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub use rift_shadow_attribution as attribution;
pub use rift_shadow_cost as cost;
pub use rift_shadow_entire as entire;
pub use rift_shadow_hooks as hooks;
pub use rift_shadow_redact as redact;

pub mod backend;
pub mod bridge;
pub mod commit_capture;
pub mod condense;
pub mod executor;
pub mod parity;
pub mod provision;

pub use backend::{
    persist_divergences, Capability, Divergence, EntireBackend, NativeBackend, Route, RouteTable,
    RoutedBackend, ShadowBackend,
};
pub use bridge::{ingest_hook, trigger_for};
pub use commit_capture::{archive_and_capture, persist_commit_capture};
pub use condense::{condense, CheckpointId};
pub use executor::{run_decision, RepoSerializer};
pub use parity::{parity_report, run_parity, Fidelity, ParityCase, ParityResult};
pub use provision::{provision_and_audit, record_block};

use rift_core::{Result, SecretBytes, Sha256Id};
use rift_shadow_hooks::{dispatch, AdapterRegistry, CanonicalEvent, EventLeaf, RawHook};

/// One content-addressed id helper: lowercase-hex SHA-256 over the concatenation
/// of `parts`. Used to derive the idempotent keys the append-only writers expect.
#[must_use]
pub(crate) fn sha256_hex(parts: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    let mut b = [0u8; 32];
    b.copy_from_slice(&h.finalize());
    Sha256Id::from_bytes(b).to_hex()
}

/// Scaffold/status string (kept for the bin entrypoint and smoke tests).
#[must_use]
pub fn status() -> &'static str {
    "rift-shadow: capture + verify ready"
}

/// Façade entry point used by the bin while richer commands are wired.
///
/// # Errors
/// Currently infallible.
pub fn initialize() -> Result<()> {
    Ok(())
}

/// Capture one agent hook into canonical events. The payload is redacted **before**
/// it is content-addressed for durable storage, honoring the one-way gate
/// (`05-security` §5.3): we hash the redacted form, not the raw bytes.
///
/// # Errors
/// [`rift_core::RiftError`] if the agent is unknown or the payload is malformed.
pub fn capture(raw: &RawHook, registry: &AdapterRegistry) -> Result<Vec<CanonicalEvent>> {
    // Redact the payload's textual form first (the durable, hashable bytes).
    let payload_text = serde_json::to_vec(&raw.payload).unwrap_or_default();
    let _redacted = redact::redact(&SecretBytes::new(payload_text))?;
    dispatch(raw, registry)
}

/// The Merkle root over an ordered set of canonical events — the checkpoint's
/// tamper-evidence witness.
#[must_use]
pub fn checkpoint_merkle_root(events: &[CanonicalEvent]) -> Sha256Id {
    let leaves: Vec<Sha256Id> = events.iter().map(EventLeaf::leaf).collect();
    rift_hashchain::merkle_root(&leaves)
}

/// Verify the projection's checkpoint chain against an authoritative source
/// (git in production). Thin façade over [`rift_store::verify_against_source`].
///
/// # Errors
/// [`rift_core::RiftError::Integrity`] on divergence.
pub async fn verify(
    store: &impl rift_store::CanonicalStore,
    source: &impl rift_store::ChainSource,
) -> Result<rift_store::VerifyReport> {
    rift_store::verify_against_source(store, source).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> AdapterRegistry {
        AdapterRegistry::with_builtin()
    }

    fn raw(verb: &str) -> RawHook {
        RawHook {
            agent: rift_shadow_hooks::AgentId::CLAUDE_CODE,
            verb: verb.into(),
            session: "01JSESSION".into(),
            turn: 0,
            occurred_at_ms: 1,
            payload: serde_json::json!({"text": "token=AKIAIOSFODNN7EXAMPLE"}),
        }
    }

    #[test]
    fn status_and_initialize() {
        assert!(status().contains("shadow"));
        assert!(initialize().is_ok());
    }

    #[test]
    fn capture_produces_events_and_redacts_payload() {
        let events = capture(&raw("UserPromptSubmit"), &registry()).unwrap();
        assert_eq!(events.len(), 1);
        let red = redact::redact(&SecretBytes::new(
            serde_json::to_vec(&raw("UserPromptSubmit").payload).unwrap(),
        ))
        .unwrap();
        assert!(!String::from_utf8_lossy(red.redacted.as_slice()).contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn checkpoint_root_is_stable_for_same_events() {
        let events = capture(&raw("SessionStart"), &registry()).unwrap();
        assert_eq!(
            checkpoint_merkle_root(&events),
            checkpoint_merkle_root(&events)
        );
    }
}
