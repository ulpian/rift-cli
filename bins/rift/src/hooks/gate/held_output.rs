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
use rift_shadow::backend::rewrite::{LineRange, RewriteForm, Stream};
use serde_json::Value;

use super::file::GateFile;
use super::rewrite_spool::{spool_rewrite, with_stdout, DeliveredStreams, ResultCall};
use super::AUDIT_IDS;

/// The form a held substitution is spooled and delivered in.
const HELD: RewriteForm = RewriteForm::HeldOutputSubstitution;

/// The call a held stretch is booked against.
pub(super) struct HeldCall<'a> {
    pub(super) git_dir: &'a Path,
    pub(super) work_tree: &'a Path,
    pub(super) raw: &'a rift_shadow::hooks::RawHook,
    /// The window journal's key of the window that ran the command.
    pub(super) window: &'a str,
    /// The subagent whose window ran it, `None` for the session's own.
    pub(super) agent: Option<&'a str>,
    /// The resolved `substitute_held_output` lever.
    pub(super) mode: Mode,
}

/// Book every held stretch of the matched deliveries; returns the replaced
/// result object when the lever enforces, some stretch was stood in for, and
/// the original was spooled under the held form first.
pub(super) fn held_output_record(
    call: &HeldCall<'_>,
    journal: &Journal,
    files: &[GateFile],
    bookings: &[Booking],
) -> Option<Value> {
    let original = result_stdout(call.raw)?;
    let mut stdout = original.to_owned();
    let mut frames = Vec::new();
    let mut booked = Vec::new();
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
            substitute_runs(&mut stdout, &file.bytes, &decision.runs, &mut frames)
        } else {
            vec![false; decision.runs.len()]
        };
        booked.extend(decision.runs.into_iter().zip(acted));
    }
    // The original is kept before the rewrite may stand: a refused spool
    // delivers the result as the tool wrote it, and every stretch is booked
    // as observed, since none reached the model.
    let ranges = original_ranges(&mut frames);
    let spooled = !ranges.is_empty()
        && spool_rewrite(
            &call.result(),
            HELD,
            &ranges,
            &DeliveredStreams {
                stdout: &stdout,
                stderr: None,
            },
        );
    for (held, done) in &booked {
        enqueue_held(call, held, *done && spooled);
    }
    spooled.then(|| with_stdout(call.raw, stdout)).flatten()
}

impl HeldCall<'_> {
    /// The call as the spool names it.
    fn result(&self) -> ResultCall<'_> {
        ResultCall {
            raw: self.raw,
            work_tree: self.work_tree,
            window: self.window,
            agent: self.agent,
        }
    }
}

/// The `stdout` text of the result object, or `None` when the result is not
/// the tool's object shape (a bare string is never replaced).
fn result_stdout(raw: &rift_shadow::hooks::RawHook) -> Option<&str> {
    raw.payload.get("tool_response")?.get("stdout")?.as_str()
}

/// Stand each held run in for by its frame, one run at a time so a run the
/// result does not carry verbatim is told apart from one it does; says, per
/// run, whether it was replaced. Each frame stood in is noted in `frames` as
/// its line in the current text and the count of lines it replaced.
fn substitute_runs(
    stdout: &mut String,
    file: &[u8],
    runs: &[HeldRun],
    frames: &mut Vec<(usize, usize)>,
) -> Vec<bool> {
    runs.iter()
        .map(|held| {
            let stretch = [(held.run, frame_for(held))];
            let Some(next) = substitute_held_lines(stdout, file, &stretch) else {
                return false;
            };
            let at = first_changed_line(stdout, &next);
            let count = usize::try_from(held.run.hi.saturating_sub(held.run.lo) + 1).unwrap_or(1);
            // Every frame below the new one moves up by the lines it took.
            for frame in frames.iter_mut().filter(|frame| frame.0 > at) {
                frame.0 -= count - 1;
            }
            frames.push((at, count));
            *stdout = next;
            true
        })
        .collect()
}

/// The first line at which `before` and `after` differ.
fn first_changed_line(before: &str, after: &str) -> usize {
    before
        .split('\n')
        .zip(after.split('\n'))
        .take_while(|(old, new)| old == new)
        .count()
}

/// The original's stdout lines each frame replaced, in order: a frame's line
/// in the delivered text plus the lines the frames above it took away.
fn original_ranges(frames: &mut [(usize, usize)]) -> Vec<LineRange> {
    frames.sort_unstable();
    let mut taken = 0usize;
    frames
        .iter()
        .map(|&(at, count)| {
            let start = at + taken;
            taken += count - 1;
            LineRange {
                stream: Stream::Stdout,
                start: u32::try_from(start).unwrap_or(u32::MAX),
                end: u32::try_from(start + count).unwrap_or(u32::MAX),
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
