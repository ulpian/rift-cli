//! One call is several reads: decided against one snapshot of the journal,
//! then recorded.
//!
//! Separate because the two phases must not be written as one — a member
//! recorded while the others are still being decided writes declines the call
//! as a whole then has to take back.

use std::path::Path;

use super::answer::{gate_answer, journal_decision, spared_lines};
use super::delivery::{gate_delivery_is_actionable, reconcile_stated_delivery};
use super::file::{gate_resolve, GateFile};
use super::{gate_edit_echo, gate_mode, GateStep};

/// The facts every member of one call is decided and recorded against.
///
/// Gathered once because they are properties of the *call*, not of the member:
/// the step it arrived on, the journal snapshot every member is judged against,
/// and the moment it is all being written at. Passing them as one value is
/// also what keeps the two halves below from drifting apart — a member decided
/// against one snapshot and recorded against another would be a decision about
/// a window that had already moved.
pub(super) struct GatePass<'a> {
    /// Which step of the gate this call is at.
    pub(super) step: GateStep,
    /// The window's lines as they stood when the call arrived.
    pub(super) held: &'a rift_agent::window_journal::Journal,
    /// The journal those lines are appended to.
    pub(super) journal: &'a crate::window_journal::WindowJournal,
    /// The window journal's key of that journal's window, which every
    /// accounting row this call journals is attributed to. Computed only once
    /// a call has reads to decide, so an idle hook pays nothing for it.
    pub(super) window_key: &'a str,
    /// What the result said about its own delivery, on the step that has a
    /// result at all.
    pub(super) stated: Option<rift_agent::window_journal::StatedDelivery>,
    /// The policy ceilings in force.
    pub(super) limits: rift_agent::window_journal::Limits,
    /// The turn every line written here carries.
    pub(super) turn: u32,
    /// The moment it carries.
    pub(super) at_ms: u64,
}

/// One read of a call that may perform several, decided and not yet recorded.
///
/// The phase split is the whole reason this type exists: a member's decision
/// is reached against the journal as it stood when the call arrived, and only
/// once every member has one does the call as a whole get to say whether a
/// decline travels. Recording inside the first pass would write declines that
/// the second pass then has to take back.
pub(super) struct GateMember {
    /// The file this member named, as this checkout spells it.
    pub(super) file: GateFile,
    /// The member's request resolved against that file.
    pub(super) read: rift_agent::window_journal::ResolvedRead,
    /// How many bytes the run of lines it asked for holds.
    pub(super) slice_bytes: usize,
    /// What was decided about it.
    pub(super) decision: rift_agent::window_journal::Decision,
    /// The rendered sentence that decision would be answered with, where
    /// there is one.
    pub(super) frame: Option<String>,
}

/// Phase one: every member of the call, resolved and decided, and nothing
/// written.
///
/// A member missing from the answer is a part of the call nobody here can
/// answer for — a path this checkout has no name for, a file past the inline
/// hash cap, a slice larger than the client's own result channel, or a
/// cross-checked read with no decline behind it. The caller counts what comes
/// back against what went in, which is how one unanswerable member keeps the
/// whole line from being declined.
pub(super) fn gate_decide_members(
    raw: &rift_shadow::hooks::RawHook,
    work_tree: &Path,
    cwd: Option<&Path>,
    effective: &rift_recall_policy::Effective,
    pass: &GatePass<'_>,
    requests: &[rift_agent::ReadRequest],
) -> Vec<GateMember> {
    let mut members: Vec<GateMember> = Vec::new();
    for request in requests {
        let Some((file, read)) = gate_resolve(work_tree, request.clone(), cwd, effective) else {
            continue;
        };
        let slice_bytes = file.slice_bytes(read.range);
        // A request larger than the client's verified inline result channel —
        // including one pathological long line — runs untouched and creates no
        // coverage. A structural replacement belongs to the response planner;
        // this line gate must not repeatedly paginate or invent delivery.
        if matches!(pass.step, GateStep::Decide)
            && !gate_delivery_is_actionable(&read, slice_bytes, pass.limits)
        {
            continue;
        }
        let decision = if matches!(pass.step, GateStep::CrossCheck) {
            // The result of a call this window declined. The bytes reached the
            // model however the decision was answered, so the honest record is
            // that the window holds them now; the decline standing just before
            // that read is the pair a diagnostic counts.
            if !crate::window_journal::deny_stands(pass.held, &read.deny_key()) {
                // No decline to contradict, so there is no decision to reach
                // and nothing to journal — but a result that states its own
                // delivery still moves this window's coverage: back, where it
                // says the call was cut short, and forward, where it says the
                // call was answered in full and the slice was too large for the
                // size rule to have credited it before the call ran.
                reconcile_stated_delivery(
                    pass.journal,
                    &read,
                    pass.stated,
                    slice_bytes,
                    pass.limits,
                    pass.turn,
                    pass.at_ms,
                );
                continue;
            }
            rift_agent::window_journal::Decision::Allow
        } else {
            rift_agent::window_journal::decide(
                &read,
                pass.held,
                gate_mode(effective.read_deny),
                deny_channel(&raw.agent, &raw.verb),
                gate_edit_echo(effective),
                slice_bytes,
                pass.limits,
            )
        };
        let (decision, frame, _) =
            gate_answer(pass.held, &read, decision, pass.limits.context_lines);
        members.push(GateMember {
            file,
            read,
            slice_bytes,
            decision,
            frame,
        });
    }
    members
}

/// Phase two: the call is one act, so it is declined only when declining
/// every part of it would be right.
///
/// One member the window does not hold — or one nobody here could resolve at
/// all, which is why the count of members is compared with the count of
/// requests — and the whole call runs, because a decline answers the whole
/// command line and the agent would lose the reads that were never in
/// question. Every decline is turned back into an allowed read in that case,
/// so phase three books nothing the model was never told.
///
/// Returns the sentence the call is answered with, where one travels: each
/// member's own frame, from the one renderer allowed to write one, joined by
/// a line break. Nothing is composed here.
pub(super) fn settle_whole_call(members: &mut [GateMember], requests: usize) -> Option<String> {
    use rift_agent::window_journal::Decision;
    let wholly_declined = members.len() == requests
        && members
            .iter()
            .all(|member| matches!(member.decision, Decision::Deny(_)));
    if !wholly_declined {
        for member in members.iter_mut() {
            if matches!(member.decision, Decision::Deny(_)) {
                member.decision = Decision::Allow;
                member.frame = None;
            }
        }
        return None;
    }
    let frames: Vec<String> = members
        .iter()
        .filter_map(|member| member.frame.clone())
        .collect();
    if frames.len() != members.len() {
        return None;
    }
    Some(frames.join("\n"))
}

/// Phase three: write each member's decision into the window's journal and
/// journal the accounting it owes, and say whether the worker was left
/// anything at all.
///
/// A decline that lost its frame in phase two is an allowed read by the time
/// it arrives here, so nothing books an avoidance the model was never told
/// about.
pub(super) fn gate_record_members(
    git_dir: &Path,
    work_tree: &Path,
    raw: &rift_shadow::hooks::RawHook,
    members: &[GateMember],
    pass: &GatePass<'_>,
) -> bool {
    let mut journaled = false;
    for member in members {
        // Whether this window has already booked any of these bytes as a read
        // that never happened. Asked of the journal this gate already has open
        // — no second file, no store, no tokenizer — and only where a decline
        // is actually being made, so the ordinary allowed call pays nothing
        // for it.
        //
        // Any of them, not all of them: this decline books its whole run or
        // none of it, so a booking that already holds part of the run is a
        // booking this claim would double.
        let booked = matches!(
            member.decision,
            rift_agent::window_journal::Decision::Deny(_)
        ) && pass.held.prevented_intersects(
            &member.read.path,
            member.read.hash,
            spared_lines(&member.read, &member.decision),
        );
        let _ = pass.journal.append(&rift_agent::window_journal::record(
            &member.read,
            &member.decision,
            member.slice_bytes,
            pass.limits,
            pass.turn,
            pass.at_ms,
        ));
        // A cross-checked read whose own result states its delivery: the line
        // just written credits the window with what the size rule could vouch
        // for, and this moves it to what the client says actually arrived.
        reconcile_stated_delivery(
            pass.journal,
            &member.read,
            pass.stated,
            member.slice_bytes,
            pass.limits,
            pass.turn,
            pass.at_ms,
        );
        // The decision is recorded in the window's own journal either way; what
        // is journaled here is the accounting the worker owes for it, the
        // frame included — a row that books what a decline avoided and says
        // nothing about what its reason cost is a row that flatters itself.
        journaled |= journal_decision(
            git_dir,
            work_tree,
            raw,
            &member.read,
            &member.file.blob_oid,
            &member.decision,
            member.frame.as_deref().unwrap_or_default(),
            booked,
            pass.window_key,
        );
    }
    journaled
}

/// The channel a decline of this `(family, verb)` would travel on, if the
/// family documents one and a recorded run has shown the model receiving it.
///
/// Read straight off the capability table, which is the one place a channel
/// is stated, so a family whose row is unconfirmed cannot be declined on by
/// this path however the rest of it is configured.
pub(super) fn deny_channel(
    agent: &rift_core::AgentId,
    verb: &str,
) -> Option<rift_agent::respond::Channel> {
    rift_agent::respond::CAPABILITIES
        .iter()
        .find(|(family, on, channel)| {
            family == agent
                && *on == verb
                && matches!(channel, rift_agent::respond::Channel::DenyWithReason { .. })
        })
        .map(|(_, _, channel)| *channel)
}
