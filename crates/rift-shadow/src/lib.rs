//! rift-shadow — the immutable, git-native system of record.
//!
//! This crate is the façade that ties the capture sub-crates together:
//!
//! * [`hooks`] — canonical events, agent adapters, the session state machine.
//! * [`redact`] — the two-pass redaction gate (the sole `RedactedBytes` mint).
//! * [`attribution`] — human-vs-agent line attribution + confidence.
//! * [`cost`] — the four-class cost model + price tables.
//!
//! It exposes the end-to-end capture primitives — turn a redacted event stream
//! into a Merkle root for a checkpoint — and the `verify` façade that cross-checks
//! the rebuildable projection against the canonical chain.
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

pub use rift_shadow_attribution as attribution;
pub use rift_shadow_cost as cost;
pub use rift_shadow_hooks as hooks;
pub use rift_shadow_redact as redact;

pub mod artifact_bridge;
pub mod artifact_registry;
pub mod backend;
pub mod bridge;
pub mod commit_capture;
pub mod condense;
pub mod digest;
pub mod executor;
pub mod ids;
pub mod inclusion;
pub mod legacy_import;
pub mod parity;
pub mod project;
pub mod shard_fixture;
pub mod soak;
pub mod transcript;
pub mod turn_boundary;

pub use artifact_bridge::{
    ingest_dialect, ingest_from_substrate, BridgeReport, RefusedShard, CHECKPOINT_BRANCH,
    CREATED_AT_UNKNOWN,
};
pub use backend::{
    ArchivedEvent, CheckpointArchive, NativeBackend, RedactionReportView, ShadowBackend,
    WorktreeLabel, NATIVE_CANON_VERSION, NATIVE_CHECKPOINT_REF, ROOT_BOUND_ID_MIN_CANON_VERSION,
};
pub use bridge::{ingest_hook, trigger_for, HookReceipt, IngestOutcome, RecordedRead};
pub use commit_capture::{archive_and_capture, archive_and_capture_with, persist_commit_capture};
pub use condense::{
    condense, predict_redaction, predicted_checkpoint_id, predicted_checkpoint_id_from,
    CheckpointId, SessionFacts,
};
pub use executor::{run_decision, RepoSerializer};
pub use parity::{
    all_match, anchors_from_fixture, anchors_root, fixture_archive, fixture_names, parity_report,
    render_report, run_behavior, AnchorSource, Fidelity, GoldenAnchors, ParityBehavior,
    ParityResult,
};
pub use project::{ReadContent, TokenEstimate};
pub use soak::{SoakEvidence, SOAK_MIN_CONSECUTIVE_CLEAN};
pub use transcript::{
    archived_transcript, compact_jsonl, sealed_spill, CompactTranscript, COMPACT_SCHEMA_VERSION,
    MAX_INLINE_CHARS,
};
pub use turn_boundary::{
    attribution_preview, boundary_collection_time, capture_boundary, capture_boundary_with_clock,
    enrich_attribution, Boundary,
};

use rift_core::{Result, SecretBytes, Sha256Id};
use rift_shadow_hooks::{dispatch, AdapterRegistry, CanonicalEvent, EventLeaf, RawHook};

/// One content-addressed id helper: lowercase-hex SHA-256 over the concatenation
/// of `parts`. Used to derive the idempotent keys the append-only writers expect,
/// and — public since the provenance verbs landed — the same convention they
/// use to mint
/// `file:<path>@sha256:<hex>` handles (identical to the attribution capture's
/// `head_content_sha256`, so a handle minted here resolves in the recall plane).
#[must_use]
pub fn sha256_hex(parts: &[&[u8]]) -> String {
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
/// it is content-addressed for durable storage, honoring the one-way redaction
/// gate: we hash the redacted form, not the raw bytes.
///
/// # Errors
/// [`rift_core::RiftError`] if the agent is unknown or the payload is malformed.
pub fn capture(raw: &RawHook, registry: &AdapterRegistry) -> Result<Vec<CanonicalEvent>> {
    // Redact the payload's textual form first (the durable, hashable bytes).
    let payload_text = serde_json::to_vec(&raw.payload).unwrap_or_default();
    let _redacted = redact::redact(&SecretBytes::new(payload_text))?;
    dispatch(raw, registry)
}

/// The Merkle root over an ordered set of canonical events.
///
/// **This is not a native checkpoint's root.** It was, and that is exactly how
/// the vacuous era happened: every caller passed an empty event set — nothing
/// threads a per-turn stream into `condense` — so every archived shard recorded
/// the same `empty_root()` constant. A native shard's root is now
/// [`backend::CheckpointArchive::merkle_root`], derived from the shard's own
/// artifacts and impossible to supply from outside.
///
/// What remains here is the derivation the LEGACY dialect used, kept because
/// [`legacy_import`] must re-derive an imported shard's *claimed* root the way
/// its writer computed it, over the events that shard actually carries.
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
            turn: Some(0),
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
