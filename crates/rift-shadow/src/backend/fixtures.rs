//! The archive fixtures the backend's test modules share: a redacted capture
//! minted through the one sanctioned gate, a transcript pair built the way
//! condense builds one, and a minimal valid archive over a real 64-hex id.

use std::collections::BTreeMap;

use rift_core::{RedactedBytes, SecretBytes};

use super::{ArchivedTask, ArchivedTranscript, CheckpointArchive, RedactionReportView};

/// A redacted (empty) capture minted through the sole sanctioned gate.
pub(super) fn redacted() -> RedactedBytes {
    rift_shadow_redact::redact(&SecretBytes::new(Vec::new()))
        .unwrap()
        .redacted
}

/// Redacted bytes over real content — same gate, non-empty.
pub(super) fn redacted_of(bytes: &[u8]) -> RedactedBytes {
    rift_shadow_redact::redact(&SecretBytes::new(bytes.to_vec()))
        .unwrap()
        .redacted
}

/// An [`ArchivedTranscript`] over the given raw transcript bytes, built the
/// way condense builds one: compact on the RAW side, then the gate once per
/// artifact.
pub(super) fn transcript_of(raw: &[u8]) -> ArchivedTranscript {
    let compacted = crate::transcript::compact_jsonl(&SecretBytes::new(raw.to_vec()), 0);
    ArchivedTranscript {
        full: redacted_of(raw),
        compact: redacted_of(compacted.compact().as_slice()),
        compact_start: compacted.compact_transcript_start,
    }
}

/// A minimal, valid checkpoint archive over a real 64-hex id.
pub(super) fn archive() -> CheckpointArchive {
    CheckpointArchive {
        repo: std::path::PathBuf::from("/tmp/r"),
        commit: "abc123".to_owned(),
        checkpoint_id: "ab".to_string() + &"cd".repeat(31),
        sessions: vec!["native-1".to_owned()],
        strategy: "manual-commit".to_owned(),
        prev_hash: None,
        events: Vec::new(),
        read_facts: Vec::new(),
        serve_audit: None,
        content: redacted(),
        report: RedactionReportView {
            total_spans: 0,
            bytes_masked: 0,
            allowlisted: 0,
            rule_pack_versions: vec!["builtin@2026.06.0".to_owned()],
            input_sha256: "c".repeat(64),
            by_detector: BTreeMap::new(),
            by_class: BTreeMap::new(),
        },
        transcript: None,
        tasks: Vec::new(),
        // `Some`, so every roundtrip/idempotence/root-invariance test in
        // this module runs over a metadata document that CARRIES the
        // recorded time — including the proof just below that the time is
        // not a Merkle leaf.
        created_at_ms: Some(1_700_000_000_000),
        // A hand-built archive is in no repository: the member is ABSENT, which
        // is what keeps every frozen anchor and golden byte-identical.
        worktree: None,
        session_worktrees: std::collections::BTreeMap::new(),
    }
}

/// A subagent task with metadata only — the minimal member of the class.
pub(super) fn task(id: &str) -> ArchivedTask {
    ArchivedTask {
        tool_use_id: id.to_owned(),
        agent_id: "explore".to_owned(),
        started_at_ms: 5,
        ended_at_ms: 9,
        input_tokens: None,
        output_tokens: None,
        transcript: None,
    }
}
