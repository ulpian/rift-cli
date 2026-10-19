//! What standing a frame in for the held lines of a terminal result would
//! save, booked as a row of its own while the lever only observes.
//!
//! Separate from the matcher because the matcher records what a result
//! showed, and this step decides about it: the matched stretches are checked
//! against the window's own reads at the same revision, and every stretch the
//! window provably holds is priced at the frame the one renderer would write
//! for it. While the lever observes nothing here touches the result, and the
//! rows let the class be sized on live sessions. When it enforces, each held
//! stretch the result carries byte for byte is replaced by that frame, every
//! other byte is kept, and the stretches replaced are booked as acted rows.
//!
//! A hook's work only: the journal already in hand, the file bytes already
//! hashed, one queue append per held stretch. No store, no tokenizer.

use std::path::Path;

use rift_agent::window_journal::output_match::Booking;
use rift_agent::window_journal::{
    held_output, substitute_held_lines, Delivered, HeldRun, Journal, Mode,
};
use serde_json::Value;

use super::file::GateFile;
use super::AUDIT_IDS;

/// The call a held stretch is booked against.
pub(super) struct HeldCall<'a> {
    pub(super) git_dir: &'a Path,
    pub(super) work_tree: &'a Path,
    pub(super) raw: &'a rift_shadow::hooks::RawHook,
    /// The window journal's key of the window that ran the command.
    pub(super) window: &'a str,
    /// The resolved `substitute_held_output` lever.
    pub(super) mode: Mode,
}

/// Book every held stretch of the matched deliveries; returns the replaced
/// result object when the lever enforces and some stretch was stood in for.
pub(super) fn held_output_record(
    call: &HeldCall<'_>,
    journal: &Journal,
    files: &[GateFile],
    bookings: &[Booking],
) -> Option<Value> {
    let mut stdout = result_stdout(call.raw)?.to_owned();
    let mut replaced = false;
    for booking in bookings {
        let Some(file) = files.get(booking.file) else {
            continue;
        };
        let sizes = line_sizes(&file.bytes);
        let line_bytes = |line: u32| {
            let at = usize::try_from(line.saturating_sub(1)).unwrap_or(usize::MAX);
            sizes.get(at).copied().unwrap_or_default()
        };
        let delivered = Delivered {
            path: &file.rel_path,
            hash: file.hash,
            runs: &booking.runs,
            line_bytes: &line_bytes,
        };
        let Some(decision) = held_output(journal, &delivered, call.mode, &frame_bytes) else {
            continue;
        };
        let acted = if decision.mode == Mode::Enforce {
            substitute_runs(&mut stdout, &file.bytes, &decision.runs)
        } else {
            vec![false; decision.runs.len()]
        };
        replaced |= acted.contains(&true);
        for (held, done) in decision.runs.iter().zip(acted) {
            enqueue_held(call, held, done);
        }
    }
    if !replaced {
        return None;
    }
    let mut output = call.raw.payload.get("tool_response")?.clone();
    *output.get_mut("stdout")? = Value::from(stdout);
    Some(output)
}

/// The `stdout` text of the result object, or `None` when the result is not
/// the tool's object shape (a bare string is never replaced).
fn result_stdout(raw: &rift_shadow::hooks::RawHook) -> Option<&str> {
    raw.payload.get("tool_response")?.get("stdout")?.as_str()
}

/// Stand each held run in for by its frame, one run at a time so a run the
/// result does not carry verbatim is told apart from one it does; says, per
/// run, whether it was replaced.
fn substitute_runs(stdout: &mut String, file: &[u8], runs: &[HeldRun]) -> Vec<bool> {
    runs.iter()
        .map(|held| {
            let stretch = [(held.run, frame_for(held))];
            match substitute_held_lines(stdout, file, &stretch) {
                Some(next) => {
                    *stdout = next;
                    true
                }
                None => false,
            }
        })
        .collect()
}

/// The bytes of each line of the file, newline included.
///
/// The file's own lines and not the result's: a printed prefix (`12:` from
/// `grep -n`) is not known per line here, so the delivered size is counted
/// low, and a credit counted low is the safe side of a figure nobody has
/// claimed yet.
fn line_sizes(bytes: &[u8]) -> Vec<usize> {
    bytes
        .split_inclusive(|byte| *byte == b'\n')
        .map(<[u8]>::len)
        .collect()
}

/// What the frame standing for `held` would cost, its own line break
/// included, as the one renderer writes it.
fn frame_bytes(held: &HeldRun) -> usize {
    frame_for(held).len() + 1
}

/// The frame the renderer writes for one held stretch.
fn frame_for(held: &HeldRun) -> String {
    rift_recall_serve::render_held_output_frame(
        &held.path,
        &held.hash.to_hex(),
        held.run.lo,
        held.run.hi,
        held.since_turn,
    )
}

/// Queue one held stretch for the worker, as acted when `substituted`;
/// says whether it landed.
fn enqueue_held(call: &HeldCall<'_>, held: &HeldRun, substituted: bool) -> bool {
    use crate::agent_queue::{
        AgentJob, AuditBasis, AuditLines, JobKind, ServeAudit, ServeAuditKind,
    };
    let credit = held.delivered_bytes.saturating_sub(held.frame_bytes);
    let audit = ServeAudit {
        call_id: {
            use rift_core::UlidSource as _;
            AUDIT_IDS.next_ulid().to_hex()
        },
        decision: if substituted {
            ServeAuditKind::ToolOutputSubstituted
        } else {
            ServeAuditKind::ToolOutputHeld
        },
        agent: call.raw.agent.as_str().to_owned(),
        basis: Some(AuditBasis {
            rel_path: held.path.clone(),
            content_sha256: held.hash.to_hex(),
            // The matcher hashed the bytes on disk and computed no object id:
            // the row names the revision by its content hash alone.
            blob_oid: String::new(),
            lines: Some(AuditLines {
                lo: held.run.lo,
                hi: held.run.hi,
            }),
        }),
        // The frame the model read, only when one stood in for the lines.
        frame: if substituted {
            frame_for(held)
        } else {
            String::new()
        },
        tag: None,
        total: Some(u32::try_from(credit).unwrap_or(u32::MAX)),
        declared_avoided: None,
        shape: None,
        session: Some(call.raw.session.clone()),
        window: Some(call.window.to_owned()),
    };
    let job = AgentJob {
        schema_version: 1,
        work_tree: call.work_tree.to_path_buf(),
        queued_at_ms: call.raw.occurred_at_ms,
        attempts: 0,
        kind: JobKind::ServeAudit(audit),
    };
    crate::agent_queue::enqueue(call.git_dir, &job).is_ok()
}
