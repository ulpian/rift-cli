//! What folding the runs of alike lines in a terminal result would spare,
//! booked as a row of its own, and folded when the lever enforces.
//!
//! Apart from the held-output step because the two decide different things:
//! that one asks whether the window already holds a file's lines, this one
//! only whether the result says the same thing many times. Neither touches
//! the result while its lever observes; the rows let the class be sized on
//! live sessions before anything is folded. When the lever enforces, the
//! folded result goes out only after its original is spooled under the fold
//! form, and only when no held substitution rewrote the result first: one
//! spool record names one form, so a result carrying both could not be
//! checked against its original.
//!
//! A hook's work only: the result in hand and one queue append. No store, no
//! tokenizer.

use std::path::Path;

use rift_shadow::backend::rewrite::{render_folded, LineRange, ResultText, RewriteForm, Stream};
use serde_json::Value;

use super::rewrite_spool::{spool_rewrite, with_stdout, ResultCall};
use super::AUDIT_IDS;

/// The call a fold is decided for.
pub(super) struct FoldCall<'a> {
    pub(super) registry: &'a rift_shadow::hooks::AdapterRegistry,
    pub(super) raw: &'a rift_shadow::hooks::RawHook,
    pub(super) git_dir: &'a Path,
    pub(super) work_tree: &'a Path,
    /// Whether a held substitution already rewrote this result.
    pub(super) held_applied: bool,
}

/// A Claude Code `Bash` result in the tool's object shape.
struct BashResult<'a> {
    command: &'a str,
    stdout: &'a str,
    stderr: &'a str,
    /// The subagent whose window ran it, `None` for the session's own.
    child: Option<String>,
}

/// Book what folding a Claude Code `Bash` result would spare under the
/// resolved lever `fold_command_output`. Says whether a row was queued.
#[cfg(test)]
pub(super) fn fold_output_record(
    registry: &rift_shadow::hooks::AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) -> bool {
    let call = FoldCall {
        registry,
        raw,
        git_dir,
        work_tree,
        held_applied: false,
    };
    fold_output_step(&call, effective).0
}

/// `fold_output_record`, also handing back the folded result object when the
/// lever enforces, no held substitution stood in, and the original spooled.
pub(super) fn fold_output_step(
    call: &FoldCall<'_>,
    effective: &rift_recall_policy::Effective,
) -> (bool, Option<Value>) {
    if effective.fold_command_output == rift_recall_policy::Mode::Off || call.raw.session.is_empty()
    {
        return (false, None);
    }
    let Some(result) = bash_result(call.registry, call.raw) else {
        return (false, None);
    };
    let plan = rift_agent::fold::fold_plan(result.command, result.stdout);
    if plan.ranges.is_empty() {
        return (false, None);
    }
    let frames: usize = plan
        .ranges
        .iter()
        .map(|&(lo, hi)| rift_recall_serve::render_fold_frame(hi - lo).len() + 1)
        .sum();
    let credit = plan.folded_bytes.saturating_sub(frames);
    let enforce = super::gate_mode(effective.fold_command_output)
        == rift_agent::window_journal::Mode::Enforce;
    let folded = (enforce && !call.held_applied)
        .then(|| fold_result(call, &result, &plan.ranges))
        .flatten();
    let booked = enqueue_folded(call, credit, folded.is_some());
    (booked, folded)
}

/// The result with the plan's ranges folded, once its original is spooled
/// under the fold form; `None` leaves it as the tool wrote it.
fn fold_result(
    call: &FoldCall<'_>,
    result: &BashResult<'_>,
    ranges: &[(u32, u32)],
) -> Option<Value> {
    let ranges: Vec<LineRange> = ranges
        .iter()
        .map(|&(start, end)| LineRange {
            stream: Stream::Stdout,
            start,
            end,
        })
        .collect();
    let original = ResultText {
        stdout: result.stdout,
        stderr: result.stderr,
    };
    let (stdout, _) = render_folded(original, &ranges, rift_recall_serve::render_fold_frame)?;
    let window = crate::window_journal::key(&call.raw.session, result.child.as_deref());
    let site = ResultCall {
        raw: call.raw,
        work_tree: call.work_tree,
        window: &window,
        agent: result.child.as_deref(),
    };
    if !spool_rewrite(&site, RewriteForm::FoldedOutput, &ranges, &stdout) {
        return None;
    }
    with_stdout(call.raw, stdout)
}

/// The command and streams of a Claude Code `Bash` result in the tool's
/// object shape; stdout is the stream folded, so the one sized.
fn bash_result<'a>(
    registry: &rift_shadow::hooks::AdapterRegistry,
    raw: &'a rift_shadow::hooks::RawHook,
) -> Option<BashResult<'a>> {
    if raw.agent != rift_core::AgentId::CLAUDE_CODE || raw.verb != "PostToolUse" {
        return None;
    }
    let facts = registry.get(&raw.agent)?.facts(&raw.verb, &raw.payload);
    if facts.tool_name.as_deref() != Some("Bash") {
        return None;
    }
    let response = raw.payload.get("tool_response")?;
    Some(BashResult {
        command: raw.payload.get("tool_input")?.get("command")?.as_str()?,
        stdout: response.get("stdout").and_then(Value::as_str)?,
        stderr: response
            .get("stderr")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        child: facts.child_session,
    })
}

/// Queue the fold row for the worker, as acted when `folded`; says whether
/// it landed.
fn enqueue_folded(call: &FoldCall<'_>, credit: usize, folded: bool) -> bool {
    let raw = call.raw;
    use crate::agent_queue::{AgentJob, JobKind, ServeAudit, ServeAuditKind};
    let audit = ServeAudit {
        call_id: {
            use rift_core::UlidSource as _;
            AUDIT_IDS.next_ulid().to_hex()
        },
        decision: if folded {
            ServeAuditKind::ToolOutputFoldApplied
        } else {
            ServeAuditKind::ToolOutputFolded
        },
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
        work_tree: call.work_tree.to_path_buf(),
        queued_at_ms: raw.occurred_at_ms,
        attempts: 0,
        kind: JobKind::ServeAudit(audit),
    };
    crate::agent_queue::enqueue(call.git_dir, &job).is_ok()
}
