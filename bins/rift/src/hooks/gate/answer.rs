//! What a decision is answered with: the sentence a declined call carries
//! back, and the accounting row the detached worker is left.
//!
//! Separate because both are about what the model and the ledger were
//! actually told — a frame nobody read and a row nobody owes are the two ways
//! this accounting could flatter itself.

use std::path::Path;

/// What one hook's read gate concluded: the answer the agent is owed, and
/// whether the detached worker was left something to write.
#[derive(Debug, Default, PartialEq, Eq)]
pub(in crate::hooks) struct GateOutcome {
    /// An accounting row was journaled, so the worker is spawned for it too.
    pub(super) journaled: bool,
    /// The rendered explanation this call is declined with — `None` for the
    /// overwhelming majority of calls, which run exactly as they were written.
    pub(super) deny_reason: Option<String>,
}

/// A source of per-decision accounting ids, process-wide for the reason
/// [`TURN_IDS`](crate::hooks::manifest::TURN_IDS) is: a fresh source inside one
/// millisecond re-emits the same id, and two decisions that book identical
/// figures are still two decisions.
pub(crate) static AUDIT_IDS: std::sync::LazyLock<rift_core::MonotonicUlidSource> =
    std::sync::LazyLock::new(rift_core::MonotonicUlidSource::new);

/// The decision as it is finally answered, the sentence carrying it, and the
/// reason to send back — which is the sentence only where one is actually sent.
///
/// The sentence is written by the one renderer allowed to write one. Composed
/// only where a decline travels: a decision that was merely recorded cost the
/// model nothing, and a row claiming otherwise would put a frame nobody read
/// on the paying side of the honesty gate.
///
/// A shape with nothing exact left to say has no frame, and a decline with no
/// frame is an unexplained tool failure — so the call runs instead, and the
/// journal records the lines it will have delivered.
pub(super) fn gate_answer(
    held: &rift_agent::window_journal::Journal,
    read: &rift_agent::window_journal::ResolvedRead,
    decision: rift_agent::window_journal::Decision,
    context_lines: u32,
) -> (
    rift_agent::window_journal::Decision,
    Option<String>,
    Option<String>,
) {
    use rift_agent::window_journal::Decision;
    let frame = gate_frame(held, read, &decision, context_lines);
    let decision = match (&decision, &frame) {
        (Decision::Deny(_), None) => Decision::Allow,
        _ => decision,
    };
    let deny_reason = match (&decision, &frame) {
        (Decision::Deny(_), Some(frame)) => Some(frame.clone()),
        _ => None,
    };
    (decision, frame, deny_reason)
}

/// The explanation a declined call is answered with, or `None` when a decline
/// has nothing exact left to say and so must not be made.
///
/// Every frame comes from the sealed renderers beside the manifest's: this
/// function chooses which and supplies the numbers, and composes no text of
/// its own. A narrowing names the turn the window last read any of its held
/// run in. The delta renderer clamps the changed places to the file's own
/// length and derives the window it suggests, which is why the shape's own
/// window is not passed on — one clamp, in the place that also does the
/// rendering.
///
/// Only a decline has a frame. A decision that was recorded and not acted on
/// sent the model nothing, and a rendered sentence nobody read is not a cost
/// the accounting may book against it.
fn gate_frame(
    held: &rift_agent::window_journal::Journal,
    read: &rift_agent::window_journal::ResolvedRead,
    decision: &rift_agent::window_journal::Decision,
    context_lines: u32,
) -> Option<String> {
    use rift_agent::window_journal::{Decision, DenyShape};
    let Decision::Deny(shape) = decision else {
        return None;
    };
    let hash = read.hash.to_hex();
    let since = held_since(held, read);
    match shape {
        DenyShape::Plain => Some(rift_recall_serve::render_deny_frame(
            &read.path,
            &hash,
            read.range.lo,
            read.range.hi,
            since,
        )),
        DenyShape::Delta { sites, shift, .. } => {
            let sites: Vec<(u32, u32)> = sites.iter().map(|site| (site.lo, site.hi)).collect();
            rift_recall_serve::render_delta_frame(
                &read.path,
                &hash,
                &sites,
                *shift,
                read.total_lines,
                context_lines,
                since,
            )
        }
        DenyShape::Narrow {
            held: run,
            read: rest,
        } => Some(rift_recall_serve::render_narrow_frame(
            &read.path,
            &hash,
            (run.lo, run.hi),
            (rest.lo, rest.hi),
            held.read_turn(&read.path, read.hash, *run).map(u64::from),
        )),
    }
}

/// The lines a decision is about: what a decline spares the window, and what
/// its accounting row is priced over.
///
/// The request itself for a plain or a delta decline, which answer all of it.
/// A narrowing answers only its **held** run: the rest of the request is a
/// read the agent still makes, and it is counted as a read when it happens,
/// so a row priced over the whole request would book those lines twice. The
/// same run sizes an observed narrowing, so the pool the observe window
/// publishes is the one enforcing would book.
pub(super) fn spared_lines(
    read: &rift_agent::window_journal::ResolvedRead,
    decision: &rift_agent::window_journal::Decision,
) -> rift_agent::window_journal::Interval {
    use rift_agent::window_journal::{Decision, DenyShape};
    match decision {
        Decision::Deny(DenyShape::Narrow { held, .. })
        | Decision::Observe(DenyShape::Narrow { held, .. }) => *held,
        Decision::Allow | Decision::Deny(_) | Decision::Observe(_) | Decision::Inherited { .. } => {
            read.range
        }
    }
}
/// The turn this window was last handed — or last authored — the bytes it is
/// now asking for again.
///
/// The frame says "unchanged since turn N", and N is that moment, not the turn
/// the repeat is being made in. Read off the live lines rather than tracked
/// separately, because the journal is already the record of when a window was
/// given what.
fn held_since(
    held: &rift_agent::window_journal::Journal,
    read: &rift_agent::window_journal::ResolvedRead,
) -> Option<u64> {
    use rift_agent::window_journal::Line;
    held.live()
        .iter()
        .rev()
        .find_map(|line| match line {
            Line::Read {
                turn, path, hash, ..
            } if path == &read.path && *hash == read.hash => Some(*turn),
            Line::Edit {
                turn,
                path,
                hash_new,
                ..
            } if path == &read.path && *hash_new == read.hash => Some(*turn),
            _ => None,
        })
        .map(u64::from)
}

/// Journal the accounting one decision owes to the detached worker, and say
/// whether it left the worker anything at all.
///
/// Best-effort by design: a journal that cannot be written costs a row and
/// never the agent's call, which has already been answered by the time this
/// runs.
pub(super) fn journal_decision(
    git_dir: &Path,
    work_tree: &Path,
    raw: &rift_shadow::hooks::RawHook,
    read: &rift_agent::window_journal::ResolvedRead,
    blob_oid: &str,
    decision: &rift_agent::window_journal::Decision,
    frame: &str,
    booked: bool,
    window_key: &str,
) -> bool {
    let Some(audit) = read_decision_audit(&raw.agent, read, blob_oid, decision, frame, booked, {
        use rift_core::UlidSource as _;
        AUDIT_IDS.next_ulid().to_hex()
    }) else {
        return false;
    };
    crate::agent_queue::enqueue(
        git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: raw.occurred_at_ms,
            attempts: 0,
            kind: crate::agent_queue::JobKind::ServeAudit(crate::agent_queue::ServeAudit {
                session: Some(raw.session.clone()),
                window: Some(window_key.to_owned()),
                ..audit
            }),
        },
    )
    .is_ok()
}

/// The accounting row a read decision owes, or `None` when it owes none.
///
/// An allowed read owes nothing: nothing was decided, and the journal line is
/// the whole record. A decline owes the row that carries what it saved and
/// what its reason cost; a decline that was only observed owes the row that
/// sizes the pool without claiming any of it. `frame` is the text the model
/// was handed, which is empty until a family is being declined on. The lines
/// named are [`spared_lines`]: a narrowing is priced over its held run.
///
/// `booked` says that a **prevented** row of this same window has already
/// taken this slice. The decline itself is unchanged — the model is still
/// declined, still pointed at what it holds, and the reason it was sent is
/// still charged — but the slice is worth one read, so this row declares a
/// zero under the tag that says which row has it. The mirror order needs
/// nothing: a decline answers the announcement, so no booking follows it.
pub(super) fn read_decision_audit(
    agent: &rift_core::AgentId,
    read: &rift_agent::window_journal::ResolvedRead,
    blob_oid: &str,
    decision: &rift_agent::window_journal::Decision,
    frame: &str,
    booked: bool,
    call_id: String,
) -> Option<crate::agent_queue::ServeAudit> {
    use crate::agent_queue::{AuditBasis, AuditLines, ServeAudit, ServeAuditKind};
    use rift_agent::window_journal::Decision;
    let lines = spared_lines(read, decision);
    let declined = matches!(decision, Decision::Deny(_));
    // An inherited observation is an observed repeat like any other, under the
    // tag of its class, so the relation can size each class apart.
    let class_tag = match decision {
        Decision::Inherited { class, .. } => Some(class.audit_tag().to_owned()),
        _ => None,
    };
    let decision = match decision {
        Decision::Allow => return None,
        Decision::Deny(_) => ServeAuditKind::ReadIntercepted,
        Decision::Observe(_) | Decision::Inherited { .. } => ServeAuditKind::ReadRepeatObserved,
    };
    let twice = declined && booked;
    Some(ServeAudit {
        call_id,
        decision,
        agent: agent.as_str().to_owned(),
        basis: Some(AuditBasis {
            rel_path: read.path.clone(),
            content_sha256: read.hash.to_hex(),
            blob_oid: blob_oid.to_owned(),
            lines: Some(AuditLines {
                lo: lines.lo,
                hi: lines.hi,
            }),
        }),
        frame: frame.to_owned(),
        tag: twice
            .then(|| rift_recall_serve::TAG_PREVENTED_FIRST.to_owned())
            .or(class_tag),
        total: None,
        declared_avoided: twice.then_some(0),
        // Not a name search: nothing here stood in for a listing.
        shape: None,
        session: None,
        window: None,
    })
}
