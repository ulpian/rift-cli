//! What folding the runs of alike lines in a terminal result would spare,
//! booked as a row of its own while the lever only observes.
//!
//! Apart from the held-output step because the two decide different things:
//! that one asks whether the window already holds a file's lines, this one
//! only whether the result says the same thing many times. Neither touches
//! the result while its lever observes; the rows let the class be sized on
//! live sessions before anything is folded.
//!
//! A hook's work only: the result in hand and one queue append. No store, no
//! tokenizer.

use std::path::Path;

use serde_json::Value;

use super::AUDIT_IDS;

/// Book what folding a Claude Code `Bash` result would spare under the
/// resolved lever `fold_command_output`. Says whether a row was queued.
pub(super) fn fold_output_record(
    registry: &rift_shadow::hooks::AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) -> bool {
    if effective.fold_command_output == rift_recall_policy::Mode::Off || raw.session.is_empty() {
        return false;
    }
    let Some((command, stdout)) = bash_stdout(registry, raw) else {
        return false;
    };
    let plan = rift_agent::fold::fold_plan(command, stdout);
    if plan.ranges.is_empty() {
        return false;
    }
    let frames: usize = plan
        .ranges
        .iter()
        .map(|&(lo, hi)| rift_recall_serve::render_fold_frame(hi - lo).len() + 1)
        .sum();
    let credit = plan.folded_bytes.saturating_sub(frames);
    enqueue_folded(raw, git_dir, work_tree, credit)
}

/// The command and the `stdout` text of a Claude Code `Bash` result in the
/// tool's object shape; the stream slice 2 would replace, so the one sized.
fn bash_stdout<'a>(
    registry: &rift_shadow::hooks::AdapterRegistry,
    raw: &'a rift_shadow::hooks::RawHook,
) -> Option<(&'a str, &'a str)> {
    if raw.agent != rift_core::AgentId::CLAUDE_CODE || raw.verb != "PostToolUse" {
        return None;
    }
    let facts = registry.get(&raw.agent)?.facts(&raw.verb, &raw.payload);
    if facts.tool_name.as_deref() != Some("Bash") {
        return None;
    }
    let command = raw.payload.get("tool_input")?.get("command")?.as_str()?;
    let stdout = raw
        .payload
        .get("tool_response")?
        .get("stdout")
        .and_then(Value::as_str)?;
    Some((command, stdout))
}

/// Queue the observed fold row for the worker; says whether it landed.
fn enqueue_folded(
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    credit: usize,
) -> bool {
    use crate::agent_queue::{AgentJob, JobKind, ServeAudit, ServeAuditKind};
    let audit = ServeAudit {
        call_id: {
            use rift_core::UlidSource as _;
            AUDIT_IDS.next_ulid().to_hex()
        },
        decision: ServeAuditKind::ToolOutputFolded,
        agent: raw.agent.as_str().to_owned(),
        // A fold names no file: the result itself is what it would shorten.
        basis: None,
        frame: String::new(),
        tag: None,
        total: Some(u32::try_from(credit).unwrap_or(u32::MAX)),
        declared_avoided: None,
        shape: None,
        session: Some(raw.session.clone()),
        window: None,
    };
    let job = AgentJob {
        schema_version: 1,
        work_tree: work_tree.to_path_buf(),
        queued_at_ms: raw.occurred_at_ms,
        attempts: 0,
        kind: JobKind::ServeAudit(audit),
    };
    crate::agent_queue::enqueue(git_dir, &job).is_ok()
}
