//! The five phases one repository-content call goes through, and the one
//! bucket it comes out in.
//!
//! The order mirrors the binary's own: decide every member against one
//! snapshot of the journal, settle the call as a whole, record, reconcile
//! against the client's statement, and only then count.

use rift_agent::window_journal::{
    self, Decision, DenyShape, EditEcho, EvictReason, Interval, Journal, Limits, Line, Mode,
    Narrowing, Reconcile, ResolvedRead, StatedDelivery,
};
use rift_agent::ReadRequest;

use super::bucket::Bucket;
use super::census::{
    count_first_sight_size, count_machine_format, count_machine_format_first_sight, MachineFormat,
};
use super::delivery::{stated_delivery, PERSISTED_OUTPUT};
use super::replay::{revision_hash, Ctx, Replay};
use super::report::{land, Report};
use super::transcript::Call;
use super::worktree::{path_key, FileFacts};

/// The sentences Claude Code's own identical-repeat guard answers a read with.
///
/// A result carrying one of these is a read the client itself declined, so the
/// tokens were never spent and there is nothing left for Rift to save. They are
/// counted on their own line rather than folded into the declinable pool, which
/// would credit Rift with a saving another mechanism made.
const CLIENT_DEDUP_MARKERS: [&str; 2] = ["Wasted call", "File unchanged since last read"];

/// What resolving one read against this checkout established, before any
/// decision is taken about it.
struct Resolved {
    /// The file, in the one spelling coverage is keyed by.
    key: String,
    /// The synthetic revision the decision was keyed by.
    revision: u32,
    /// The request resolved against that file.
    read: ResolvedRead,
    /// How many bytes the run of lines it asked for holds.
    slice_bytes: usize,
    /// How many bytes the whole file holds, as the replay resolved it.
    file_bytes: u64,
    /// What the pre-call step credited before any result arrived.
    recorded: Interval,
}

/// One read of a call that may perform several: resolved, and decided where
/// the gate reaches a decision at all.
///
/// The shape mirrors the binary's `GateMember`, and for the same reason: a
/// member's decision is reached against the journal as it stood when the call
/// arrived, and only once every member has one does the call as a whole get
/// to say whether a decline travels.
struct Member {
    /// What this checkout could say about the read.
    resolved: Resolved,
    /// What was decided, or `None` where the slice was past the ceiling and
    /// the gate reached no decision and wrote nothing.
    decision: Option<Decision>,
    /// The sentence that decision would be answered with.
    frame: Option<String>,
    /// Whether a decline here rests on the window's own whole-file write.
    rests_on_a_write: bool,
    /// The bucket this member puts the call in, were it the first to block a
    /// decline.
    bucket: Bucket,
    /// How the request stands against what the window holds of it.
    narrowing: NarrowClass,
    /// What the shipped decision declines of it with narrowing enforced.
    if_enforced: Option<Spared>,
}

/// How a repeat that is not wholly held stands against what is: the split the
/// two repeat pools are published under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum NarrowClass {
    /// One held run at one end, and one unheld run beside it.
    Narrowable,
    /// Held lines, or the lines left to read, in two or more runs.
    Multi,
    /// Nothing of it held at this revision.
    NotNarrowable,
}

/// What enforcing narrowing would have declined of one member: the lines it
/// spares, the lines asked for, and the frame it travels with.
struct Spared {
    /// The lines the decline answers — the held run of a narrowing, the
    /// whole request otherwise.
    lines: u32,
    /// The lines the member asked for.
    requested: u32,
    /// What the frame costs.
    frame_tokens: u64,
    /// Whether it is a narrowing rather than a plain or delta decline.
    narrowed: bool,
}

/// One slot of a call, in the order the command line performs it.
enum Slot {
    /// This checkout could say nothing about the member at all.
    Blocked(Bucket),
    /// The member resolved.
    Member(Box<Member>),
}

impl NarrowClass {
    /// Every class, in the order the table prints them.
    pub(super) const ALL: [Self; 3] = [Self::Narrowable, Self::Multi, Self::NotNarrowable];

    /// The class a classification falls in.
    const fn of(narrowing: Narrowing) -> Self {
        match narrowing {
            Narrowing::Narrow { .. } => Self::Narrowable,
            Narrowing::Multi => Self::Multi,
            Narrowing::NotNarrowable => Self::NotNarrowable,
        }
    }

    /// Where this class sits in [`NarrowClass::ALL`].
    pub(super) const fn index(self) -> usize {
        match self {
            Self::Narrowable => 0,
            Self::Multi => 1,
            Self::NotNarrowable => 2,
        }
    }

    /// The key this class is published under.
    pub(super) const fn key(self) -> &'static str {
        match self {
            Self::Narrowable => "narrowable",
            Self::Multi => "multi",
            Self::NotNarrowable => "not_narrowable",
        }
    }

    /// The sentence the table prints for this class.
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Narrowable => "  narrowable: one unheld run",
            Self::Multi => "  multi: two or more runs",
            Self::NotNarrowable => "  not narrowable",
        }
    }
}

impl Slot {
    /// The bucket this slot puts the call in when it is the first blocker.
    fn bucket(&self) -> Bucket {
        match self {
            Self::Blocked(bucket) => *bucket,
            Self::Member(member) => member.bucket,
        }
    }

    /// Whether this slot would let the call be declined as a whole.
    fn declines(&self) -> bool {
        matches!(
            self,
            Self::Member(member) if matches!(member.decision, Some(Decision::Deny(_)))
        )
    }
}

/// Take every read one repository-content call performs through the shipped
/// decision, and then through the client's own statement about what it
/// delivered.
///
/// Five phases, the first three of them the binary's own: resolve and decide
/// every member against ONE snapshot of the journal; settle whether the call as
/// a whole is declined, which it is only when declining every part of it would
/// be right; record what each member decided; reconcile each member that was
/// not declined against what the result states it was handed; and only then
/// choose the one bucket the call is counted in.
pub(super) fn classify_reads(
    ctx: &mut Ctx<'_>,
    call: &Call,
    requests: &[ReadRequest],
    text: &str,
    tokens: u64,
) {
    // A call none of whose reads this checkout has a name for is out of scope,
    // the way scope containment takes an out-of-tree read out of every other
    // figure. One member inside the tree makes the call repository content.
    if requests
        .iter()
        .all(|request| path_key(&request.path, call.cwd.as_deref()).is_none())
    {
        ctx.report.outside_work_tree.add(tokens);
        return;
    }
    count_the_members(ctx, call, requests, tokens);
    let format = count_machine_format(
        ctx.report,
        in_tree_paths(call, requests).iter().map(String::as_str),
        tokens,
    );
    if call.result.is_none() {
        land(ctx.report, Bucket::NoResultRecorded, tokens);
        return;
    }
    // The client answered the call itself, so no bytes arrived and there is
    // nothing to credit a window with. Checked before the journal is touched:
    // recording a delivery that never happened is the one error a decline
    // cannot survive.
    if CLIENT_DEDUP_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
    {
        land(ctx.report, Bucket::ClientDedup, tokens);
        return;
    }
    // Phase one.
    let mut slots: Vec<Slot> = requests
        .iter()
        .map(|request| resolve_member(ctx, call, request, text))
        .collect();
    // Phase two: the call is one act, so it is declined only when declining
    // every part of it would be right. One member the window does not hold —
    // or one nobody here could resolve at all — and the whole line runs.
    let wholly_declined = !slots.is_empty() && slots.iter().all(Slot::declines);
    // Which member stopped the call being declined, taken before the downgrade
    // below turns the held members into allowed ones — afterwards nothing on
    // the call declines and the question has no answer.
    let blocker = slots.iter().position(|slot| !slot.declines());
    if !wholly_declined {
        downgrade_the_declines(ctx.report, &mut slots);
    }
    record_the_decisions(ctx, &slots);
    reconcile_with_the_statement(ctx, call, &mut slots);
    // Phase five: the one bucket this call is counted in.
    let blocker = blocker.and_then(|at| slots.get(at));
    let bucket = bucket_for_the_call(wholly_declined, text, blocker);
    if bucket == Bucket::Declined {
        ctx.report.declined.add(tokens);
        charge_the_reason_frames(ctx.report, &slots);
    }
    if bucket.declinable() {
        ctx.report.declinable.add(tokens);
    }
    note_the_narrowing(ctx.report, bucket, blocker, &slots, tokens);
    if matches!(bucket, Bucket::FirstSight | Bucket::CreditedFromStatement) {
        note_a_first_sight(ctx, blocker, text, tokens);
    }
    if bucket == Bucket::FirstSight {
        note_what_a_first_sight_was_of(ctx.report, format, blocker, tokens);
    }
    land(ctx.report, bucket, tokens);
}

/// The paths of a call's reads that name something inside the work tree, in
/// the one spelling coverage is keyed by.
fn in_tree_paths(call: &Call, requests: &[ReadRequest]) -> Vec<String> {
    requests
        .iter()
        .filter_map(|request| path_key(&request.path, call.cwd.as_deref()))
        .collect()
}

/// Count a first sight in its machine-format class and by the size of the
/// file it read — the file of the member that made the call a first sight.
fn note_what_a_first_sight_was_of(
    report: &mut Report,
    format: Option<MachineFormat>,
    blocker: Option<&Slot>,
    tokens: u64,
) {
    if let Some(format) = format {
        count_machine_format_first_sight(report, format, tokens);
    }
    let Some(Slot::Member(member)) = blocker else {
        return;
    };
    let resolved = &member.resolved;
    count_first_sight_size(
        &mut report.first_sight_sizes,
        &resolved.key,
        u64::from(resolved.read.total_lines),
        resolved.file_bytes,
        tokens,
    );
}

/// Count what the call is made of, and whether it is the read that follows an
/// edit this harness declines to model as coverage.
fn count_the_members(ctx: &mut Ctx<'_>, call: &Call, requests: &[ReadRequest], tokens: u64) {
    if requests.len() > 1 {
        ctx.report.calls_with_several_reads = ctx.report.calls_with_several_reads.saturating_add(1);
    }
    ctx.report.read_members = ctx
        .report
        .read_members
        .saturating_add(requests.len().try_into().unwrap_or(u64::MAX));
    let mut verified = false;
    for request in requests {
        if let Some(key) = path_key(&request.path, call.cwd.as_deref()) {
            verified |= ctx.state.awaiting_verification.remove(&key);
        }
    }
    if verified {
        ctx.report.verification_after_edit.add(tokens);
    }
}

/// Turn every decline on a call that is not declined as a whole back into an
/// allow.
///
/// What the all-or-nothing rule costs, counted rather than absorbed: the agent
/// would lose the reads that were never in question.
fn downgrade_the_declines(report: &mut Report, slots: &mut [Slot]) {
    for slot in slots {
        let Slot::Member(member) = slot else { continue };
        if matches!(member.decision, Some(Decision::Deny(_))) {
            member.decision = Some(Decision::Allow);
            member.frame = None;
            report.declines_lost_to_a_sibling = report.declines_lost_to_a_sibling.saturating_add(1);
        }
    }
}

/// Phase three: write each member's decision into the window's journal.
fn record_the_decisions(ctx: &mut Ctx<'_>, slots: &[Slot]) {
    for slot in slots {
        let Slot::Member(member) = slot else { continue };
        let Some(decision) = &member.decision else {
            continue;
        };
        ctx.state.journal.push(window_journal::record(
            &member.resolved.read,
            decision,
            member.resolved.slice_bytes,
            ctx.replay.limits,
            ctx.state.turn,
            0,
        ));
    }
}

/// Phase four: the result's own statement, which is the only route by which a
/// delivery too large for the size rule to vouch for ever becomes coverage —
/// and the only one that takes coverage back.
fn reconcile_with_the_statement(ctx: &mut Ctx<'_>, call: &Call, slots: &mut [Slot]) {
    let stated = call.response.as_ref().and_then(stated_delivery);
    if call.response.is_some() {
        ctx.report.results_with_a_response = ctx.report.results_with_a_response.saturating_add(1);
    }
    let Some(stated) = stated else { return };
    ctx.report.statements_read = ctx.report.statements_read.saturating_add(1);
    for slot in slots {
        let Slot::Member(member) = slot else { continue };
        if matches!(member.decision, Some(Decision::Deny(_))) {
            // A decline stands over these lines, so the result is the bytes
            // arriving anyway rather than a statement about a delivery this
            // window is owed coverage for.
            continue;
        }
        apply_the_statement(ctx, member, stated);
    }
}

/// What one member's coverage does with the statement: nothing, a credit for
/// what the size rule refused, or an emptying and a re-record.
fn apply_the_statement(ctx: &mut Ctx<'_>, member: &mut Member, stated: StatedDelivery) {
    let read = &member.resolved.read;
    match window_journal::reconcile_delivery(read, member.resolved.recorded, stated) {
        Reconcile::Unchanged => (),
        Reconcile::Credit(range) => {
            // The whole of what this lever is worth, in one line: a read the
            // ceiling refused to credit now leaves coverage behind, so the
            // NEXT read of these lines has something to be decided against.
            if member.bucket == Bucket::OverCreditBound {
                member.bucket = Bucket::CreditedFromStatement;
            }
            ctx.report.deliveries_credited = ctx.report.deliveries_credited.saturating_add(1);
            credit_line(&mut ctx.state.journal, read, range, ctx.state.turn);
        }
        Reconcile::TakeBack(kept) => {
            ctx.report.deliveries_taken_back = ctx.report.deliveries_taken_back.saturating_add(1);
            ctx.state.journal.push(Line::Evict {
                turn: ctx.state.turn,
                reason: EvictReason::Truncate,
                at_ms: 0,
            });
            if let Some(range) = kept {
                credit_line(&mut ctx.state.journal, read, range, ctx.state.turn);
            }
        }
    }
}

/// The one bucket a whole call is counted in: what it was decided as, what
/// became of its result, or what stopped it being declined.
fn bucket_for_the_call(wholly_declined: bool, text: &str, blocker: Option<&Slot>) -> Bucket {
    if wholly_declined {
        return Bucket::Declined;
    }
    // The client wrote the result to a file; the window holds a pointer and a
    // preview, never the lines.
    if text.contains(PERSISTED_OUTPUT) {
        return Bucket::SpilledToAFile;
    }
    blocker.map_or(Bucket::NoLineCount, Slot::bucket)
}

/// Split a repeat that is not wholly held by how it stands against what is,
/// and size what enforcing narrowing would have spared of the call.
fn note_the_narrowing(
    report: &mut Report,
    bucket: Bucket,
    blocker: Option<&Slot>,
    slots: &[Slot],
    tokens: u64,
) {
    let pool = match bucket {
        Bucket::RepeatOverlapping => 0,
        Bucket::RepeatDisjoint => 1,
        _ => return,
    };
    if let Some(Slot::Member(member)) = blocker {
        let cell = report
            .narrowing_split
            .get_mut(pool)
            .and_then(|split| split.get_mut(member.narrowing.index()));
        if let Some(tally) = cell {
            tally.add(tokens);
        }
    }
    size_the_narrowing(report, slots, tokens);
}

/// What enforcing narrowing would have spared of one call, booked where it
/// would have been declined at all.
///
/// All-or-nothing, as a decline is: the call is narrowed only when every
/// member of it would be declined and one of them is a narrowing. What it
/// spares is the declined lines' share of the result's own tokens, net of
/// every frame the call would carry — the held lines of a narrowing, and none
/// of the lines it still reads.
fn size_the_narrowing(report: &mut Report, slots: &[Slot], tokens: u64) {
    let mut spared = Vec::with_capacity(slots.len());
    for slot in slots {
        let Slot::Member(member) = slot else { return };
        let Some(member) = &member.if_enforced else {
            return;
        };
        spared.push(member);
    }
    if !spared.iter().any(|member| member.narrowed) {
        return;
    }
    let lines: u64 = spared.iter().map(|member| u64::from(member.lines)).sum();
    let requested: u64 = spared
        .iter()
        .map(|member| u64::from(member.requested))
        .sum();
    let frames: u64 = spared.iter().map(|member| member.frame_tokens).sum();
    let share = u128::from(tokens) * u128::from(lines) / u128::from(requested.max(1));
    let share = u64::try_from(share).unwrap_or(u64::MAX);
    report
        .narrowed_if_enforced
        .add(share.saturating_sub(frames));
    report.narrowed_frame_tokens = report.narrowed_frame_tokens.saturating_add(frames);
}

/// What this call's declines cost, and how many of them rest on the window's
/// own whole-file write rather than on a read.
fn charge_the_reason_frames(report: &mut Report, slots: &[Slot]) {
    for slot in slots {
        let Slot::Member(member) = slot else { continue };
        if let Some(frame) = &member.frame {
            report.frame_tokens = report
                .frame_tokens
                .saturating_add(u64::from(rift_tokens::count(frame)));
        }
        if member.rests_on_a_write {
            report.declines_resting_on_a_write =
                report.declines_resting_on_a_write.saturating_add(1);
        }
    }
}

/// Record a read the window had to be handed, and which `(path, revision)` it
/// was of.
///
/// A first read is a first read whichever ceiling it arrived past: both buckets
/// are reads nothing could have declined, and both are what a sibling window
/// then reads again.
///
/// A spawning window's first read also keeps the text it returned, which is
/// what a paste into the prompt it spawned a sibling with is compared against.
fn note_a_first_sight(ctx: &mut Ctx<'_>, blocker: Option<&Slot>, text: &str, tokens: u64) {
    ctx.report.first_read_floor.add(tokens);
    let Some(Slot::Member(member)) = blocker else {
        return;
    };
    let key = member.resolved.key.clone();
    let revision = member.resolved.revision;
    if ctx.tree.spawners.contains(&ctx.window) {
        ctx.tree
            .first_texts
            .entry((key.clone(), revision, ctx.window))
            .or_insert_with(|| text.to_owned());
    }
    ctx.tree
        .seen
        .entry((key, revision))
        .or_default()
        .push((ctx.window, tokens));
}

/// Append the one line a credit is: a read of exactly the run the statement
/// named.
fn credit_line(journal: &mut Journal, read: &ResolvedRead, range: Interval, turn: u32) {
    journal.push(Line::Read {
        turn,
        path: read.path.clone(),
        hash: read.hash,
        range,
        total_lines: read.total_lines,
        at_ms: 0,
    });
}

/// Resolve one member of a call against this checkout, and decide about it
/// where the gate would have reached a decision at all.
fn resolve_member(ctx: &mut Ctx<'_>, call: &Call, request: &ReadRequest, text: &str) -> Slot {
    let Some(key) = path_key(&request.path, call.cwd.as_deref()) else {
        return Slot::Blocked(Bucket::NoLineCount);
    };
    let revision = ctx.tree.revisions.get(&key).copied().unwrap_or_default();
    let hash = revision_hash(&key, revision);
    let facts = ctx.worktree.facts_at(ctx.state.commit, &key).cloned();
    let total_lines = ctx
        .tree
        .written_lines
        .get(&key)
        .copied()
        .or_else(|| facts.as_ref().map(FileFacts::total_lines));
    let Some(total_lines) = total_lines.filter(|lines| *lines > 0) else {
        return Slot::Blocked(Bucket::NoLineCount);
    };
    let Some(read) = ResolvedRead::new(
        &ReadRequest {
            path: key.clone(),
            ..request.clone()
        },
        hash,
        total_lines,
    ) else {
        // A span no artifact pins, or a start past the end of the file.
        return Slot::Blocked(Bucket::UnknownSpan);
    };
    // The larger of the two readings of the slice: the checkout's own bytes
    // for those lines, and what the result actually returned.
    let slice_bytes = facts
        .as_ref()
        .map_or(0, |facts| facts.slice_bytes(read.range))
        .max(text.len());
    let recorded = read.range_delivered(slice_bytes, ctx.replay.limits);
    let file_bytes = match ctx.tree.written_bytes.get(&key) {
        Some(bytes) => *bytes,
        None => facts.as_ref().map_or(0, FileFacts::total_bytes),
    };
    let resolved = Resolved {
        key,
        revision,
        read,
        slice_bytes,
        file_bytes,
        recorded,
    };
    // Past the size a delivery can be vouched for: the gate reaches no
    // decision and writes nothing, and the member's only route to coverage is
    // the client's own statement, one hook later.
    if slice_bytes > ctx.replay.limits.max_slice_bytes
        || !resolved
            .read
            .delivery_is_complete(slice_bytes, ctx.replay.limits)
    {
        return Slot::Member(Box::new(Member {
            narrowing: NarrowClass::of(window_journal::narrowing(
                &resolved.read,
                &ctx.state.journal,
            )),
            resolved,
            decision: None,
            frame: None,
            rests_on_a_write: false,
            bucket: Bucket::OverCreditBound,
            if_enforced: None,
        }));
    }
    Slot::Member(Box::new(decide_member(
        &ctx.state.journal,
        ctx.replay,
        resolved,
    )))
}

/// Decide about one resolved member against the journal as it stands, writing
/// nothing.
fn decide_member(journal: &Journal, replay: &Replay<'_>, resolved: Resolved) -> Member {
    let read = &resolved.read;
    let read_coverage = journal.read_coverage(&resolved.key, read.hash);
    let held_before = journal
        .coverage(&resolved.key, read.hash)
        .contains(read.range);
    let already_declined = journal.denied(&read.deny_key());
    let rests_on_a_write = journal.wrote_in_full(&resolved.key, read.hash).is_some();
    let decision = window_journal::decide(
        read,
        journal,
        Mode::Enforce,
        replay.channel,
        EditEcho::Pinned,
        resolved.slice_bytes,
        replay.limits,
    );
    // The same fallback the shipped gate takes: a decline whose frame has
    // nothing exact left to say is not a decline.
    let frame = frame_for(journal, read, &decision, replay.limits.context_lines);
    let decision = match (&decision, &frame) {
        (Decision::Deny(_), None) => Decision::Allow,
        _ => decision,
    };
    let bucket = bucket_of(
        &decision,
        held_before,
        !read_coverage.is_empty(),
        already_declined,
        &read_coverage,
        read.range,
        replay.limits.min_slice_lines,
    );
    Member {
        narrowing: NarrowClass::of(window_journal::narrowing(read, journal)),
        if_enforced: if_narrowing_enforced(journal, replay, &resolved),
        resolved,
        decision: Some(decision),
        frame,
        rests_on_a_write,
        bucket,
    }
}

/// What the shipped decision declines of one member with narrowing enforced,
/// against the same journal the bucket was decided over, or `None` when it
/// declines nothing.
fn if_narrowing_enforced(
    journal: &Journal,
    replay: &Replay<'_>,
    resolved: &Resolved,
) -> Option<Spared> {
    let read = &resolved.read;
    let limits = Limits {
        narrow_mode: Mode::Enforce,
        ..replay.limits
    };
    let decision = window_journal::decide(
        read,
        journal,
        Mode::Enforce,
        replay.channel,
        EditEcho::Pinned,
        resolved.slice_bytes,
        limits,
    );
    let frame = frame_for(journal, read, &decision, limits.context_lines)?;
    let (lines, narrowed) = match decision {
        Decision::Deny(DenyShape::Narrow { held, .. }) => (held.lines(), true),
        _ => (read.range.lines(), false),
    };
    Some(Spared {
        lines,
        requested: read.range.lines(),
        frame_tokens: u64::from(rift_tokens::count(&frame)),
        narrowed,
    })
}

/// Which bucket a decided call belongs in.
fn bucket_of(
    decision: &Decision,
    held_before: bool,
    read_before: bool,
    already_declined: bool,
    coverage: &window_journal::IntervalSet,
    range: Interval,
    floor: u32,
) -> Bucket {
    match decision {
        Decision::Deny(_) => Bucket::Declined,
        Decision::Observe(_) => {
            if range.lines() < floor {
                Bucket::BelowFloor
            } else {
                Bucket::ObservedNotDeclined
            }
        }
        Decision::Allow => {
            if held_before && already_declined {
                Bucket::RepeatAfterDecline
            } else if held_before {
                Bucket::NoHonestFrame
            } else if !read_before {
                Bucket::FirstSight
            } else if coverage.runs().iter().any(|run| run.intersects(range)) {
                Bucket::RepeatOverlapping
            } else {
                Bucket::RepeatDisjoint
            }
        }
    }
}

/// The sentence a decline would carry, through the one sealed renderer.
fn frame_for(
    held: &Journal,
    read: &ResolvedRead,
    decision: &Decision,
    context_lines: u32,
) -> Option<String> {
    let Decision::Deny(shape) = decision else {
        return None;
    };
    let hash = read.hash.to_hex();
    let since = held_since(held, read);
    match shape {
        DenyShape::Plain => Some(rift_recall_validity::trust::render_deny_frame(
            &read.path,
            &hash,
            read.range.lo,
            read.range.hi,
            since,
        )),
        DenyShape::Delta { sites, shift, .. } => {
            let sites: Vec<(u32, u32)> = sites.iter().map(|site| (site.lo, site.hi)).collect();
            rift_recall_validity::trust::render_delta_frame(
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
        } => Some(rift_recall_validity::trust::render_narrow_frame(
            &read.path,
            &hash,
            (run.lo, run.hi),
            (rest.lo, rest.hi),
            held.read_turn(&read.path, read.hash, *run).map(u64::from),
        )),
    }
}

/// The turn this window was last handed — or last authored — these bytes.
fn held_since(held: &Journal, read: &ResolvedRead) -> Option<u64> {
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
