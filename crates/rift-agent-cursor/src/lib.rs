//! First-party Rift adapter **scaffold** for **Cursor** (P8 S3).
//!
//! Cursor is editor-integrated and surfaces fewer discrete hooks than a CLI
//! agent; this scaffold maps the available points and records the gaps in
//! [`compat_notes`](AgentAdapter::compat_notes). The exact native event names are
//! `(verify)` and pinned by the conformance fixtures.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use rift_agent::{event, AdapterError, AgentAdapter, CompatNotes};
use rift_core::{AgentId, CanonicalEvent, EventKind, Trust};

/// The Cursor adapter (scaffold).
pub struct CursorAdapter;

impl CursorAdapter {
    /// The agent id this adapter serves.
    pub const AGENT_ID: AgentId = AgentId::CURSOR;
}

impl AgentAdapter for CursorAdapter {
    fn agent_id(&self) -> AgentId {
        Self::AGENT_ID
    }

    fn map(
        &self,
        verb: &str,
        _native: &serde_json::Value,
    ) -> Result<Vec<CanonicalEvent>, AdapterError> {
        use EventKind::{FileEdit, SessionEnd, SessionStart};
        use Trust::{CodeFact, GitFact};
        Ok(match verb {
            "session_start" => vec![event(SessionStart, GitFact)],
            "edit" => vec![event(FileEdit, CodeFact)],
            "session_end" => vec![event(SessionEnd, GitFact)],
            other => return Err(AdapterError::UnknownVerb(other.to_owned())),
        })
    }

    fn verbs(&self) -> &'static [&'static str] {
        &["session_start", "edit", "session_end"]
    }

    fn compat_notes(&self) -> CompatNotes {
        CompatNotes {
            agent: Self::AGENT_ID,
            lifecycle_gaps: &[
                "no discrete tool-call/turn boundaries",
                "no provider-reported token usage",
            ],
            surfaces_token_usage: false,
            best_effort: true,
            note: "Editor-integrated; maps available edit/session points, marks gaps.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_agent::conformance::{check, Case};
    use rift_core::EventKind::{FileEdit, SessionEnd, SessionStart};

    #[test]
    fn conformance_kit_passes() {
        let cases = vec![
            Case::new("session_start", serde_json::json!({}), &[SessionStart]),
            Case::new("edit", serde_json::json!({}), &[FileEdit]),
            Case::new("session_end", serde_json::json!({}), &[SessionEnd]),
        ];
        check(&CursorAdapter, &cases).unwrap();
    }
}
