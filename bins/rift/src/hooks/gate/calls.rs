//! The planned, emitted and applied outcomes of one declined native call.
//!
//! The gate writes "planned" when it declines a call, the hook writes
//! "emitted" once it knows whether its response reached stdout, and a result
//! for the same call writes "applied". All three are keyed by the id the
//! client gave the call, so a later diagnostic can tell a response that never
//! left the hook from one the client received and did not act on. Nothing
//! here opens a store: each fact is one append to the window's call sidecar,
//! beside its journal and never in it, since the journal models only what
//! the window holds.

use std::path::{Path, PathBuf};

use rift_agent::window_journal::{CallFact, Line};

use crate::window_journal::CallSidecar;

/// What the gate concluded about a call, handed back to the hook that
/// answers it.
#[derive(Debug, Default)]
pub(in crate::hooks) struct Decline {
    /// The sentence the call is declined with, when it is declined.
    pub(in crate::hooks) reason: Option<String>,
    /// Where the emission of that decline is to be recorded.
    pub(in crate::hooks) receipt: Option<CallReceipt>,
}

/// The window and call a decline was planned for, so the hook can record
/// what it emitted against the same key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::hooks) struct CallReceipt {
    work_tree: PathBuf,
    session: String,
    child: Option<String>,
    call: Option<String>,
    turn: u32,
    at_ms: u64,
}

/// The window a receipt is for, named the way the gate opened it.
pub(super) struct Window<'a> {
    pub(super) work_tree: &'a Path,
    pub(super) session: &'a str,
    pub(super) child: Option<&'a str>,
    pub(super) turn: u32,
    pub(super) at_ms: u64,
}

impl Window<'_> {
    /// This window's call sidecar.
    fn sidecar(&self) -> CallSidecar {
        CallSidecar::open(self.work_tree, self.session, self.child)
    }
}

/// The id the client gave this call, or `None` when it sent none.
///
/// Claude Code and Codex both name a tool call `tool_use_id` on the pre and
/// post events. An absent or empty id is kept absent: pairing by time or by
/// path instead would be a guess.
pub(super) fn native_call_id(payload: &serde_json::Value) -> Option<String> {
    let id = payload.get("tool_use_id")?.as_str()?;
    (!id.is_empty()).then(|| id.to_owned())
}

/// Record that the gate declined this call, and hand back the receipt the
/// hook records its emission against.
pub(super) fn plan_decline(window: &Window<'_>, payload: &serde_json::Value) -> CallReceipt {
    let call = native_call_id(payload);
    let _ = window.sidecar().append(&Line::Call {
        turn: window.turn,
        call: call.clone(),
        fact: CallFact::Planned,
        at_ms: window.at_ms,
    });
    CallReceipt {
        work_tree: window.work_tree.to_path_buf(),
        session: window.session.to_owned(),
        child: window.child.map(str::to_owned),
        call,
        turn: window.turn,
        at_ms: window.at_ms,
    }
}

/// Record that a result arrived for a call this window planned to decline.
///
/// Only a call with a planned decline under the same id is written, so an
/// ordinary result costs no row. A result with no id is not paired at all.
pub(super) fn record_applied(window: &Window<'_>, payload: &serde_json::Value) {
    let Some(call) = native_call_id(payload) else {
        return;
    };
    let sidecar = window.sidecar();
    let held = sidecar.load();
    let fact_of = |wanted: CallFact| {
        held.lines().iter().any(|line| {
            matches!(line, Line::Call { call: Some(id), fact, .. } if *id == call && *fact == wanted)
        })
    };
    // A second result for the same call says nothing the first did not.
    if !fact_of(CallFact::Planned) || fact_of(CallFact::Applied) {
        return;
    }
    let _ = sidecar.append(&Line::Call {
        turn: window.turn,
        call: Some(call),
        fact: CallFact::Applied,
        at_ms: window.at_ms,
    });
}

/// Record what the hook did with the response to a declined call: whether it
/// was written and flushed, and the status the hook exits with.
///
/// Stamped with the millisecond the decline was planned at, because the hook
/// reads no clock of its own and the two happen in one process.
pub(in crate::hooks) fn record_emission(receipt: &CallReceipt, wrote: bool, exit: i32) {
    let sidecar = CallSidecar::open(
        &receipt.work_tree,
        &receipt.session,
        receipt.child.as_deref(),
    );
    let _ = sidecar.append(&Line::Call {
        turn: receipt.turn,
        call: receipt.call.clone(),
        fact: CallFact::Emitted { wrote, exit },
        at_ms: receipt.at_ms,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_native_call_id_is_read_from_the_claude_code_and_codex_shapes() {
        let claude = serde_json::json!({
            "hook_event_name": "PreToolUse", "tool_name": "Read",
            "tool_use_id": "toolu_01A", "tool_input": { "file_path": "/r/a.rs" }
        });
        let codex = serde_json::json!({
            "hook_event_name": "PostToolUse", "tool_name": "shell",
            "tool_use_id": "call_9x", "tool_input": { "command": ["cat", "a.rs"] }
        });
        assert_eq!(native_call_id(&claude).as_deref(), Some("toolu_01A"));
        assert_eq!(native_call_id(&codex).as_deref(), Some("call_9x"));
    }

    #[test]
    fn a_call_with_no_id_or_an_empty_one_is_uncorrelatable() {
        for payload in [
            serde_json::json!({ "tool_name": "Read" }),
            serde_json::json!({ "tool_use_id": "" }),
            serde_json::json!({ "tool_use_id": 7 }),
        ] {
            assert_eq!(native_call_id(&payload), None, "{payload}");
        }
    }
}
