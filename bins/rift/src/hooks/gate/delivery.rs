//! What a result says about its own delivery, and what the window journal
//! does about the difference.
//!
//! Separate because it is the one place a client's own statement is read: the
//! fields and the sentence a family publishes about how much of a file it
//! handed over, and the one rule that moves a window's coverage to match.

/// The field a Claude Code `Read` result states its own truncation in, and the
/// two fields that say how much of the file it handed over.
///
/// Pinned by observation O5 of
/// `crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md`:
/// four recorded results across two clients, every one of them carrying the
/// boolean, and only the older client's *content block* carrying the sentence
/// as well. A reader that looked for the sentence alone would have called the
/// newer client's truncated reads complete.
pub(super) const TRUNCATED_BY_TOKEN_CAP: &str = "truncatedByTokenCap";

/// The first line handed over, when the result names it. Absent, the read's
/// own first line is what the delivery started at.
pub(super) const DELIVERY_START_LINE: &str = "startLine";

/// How many lines followed [`DELIVERY_START_LINE`].
pub(super) const DELIVERY_NUM_LINES: &str = "numLines";

/// The same fact as a sentence, for a result that reaches a hook as text
/// rather than as the object O5 pins. Quoted from the same observation, up to
/// the point where the wording differs between clients.
pub(super) const TRUNCATION_NOTICE: &str = "[Truncated: PARTIAL view";

/// The clause of that sentence carrying the numbers, spelled `A-B`.
pub(super) const TRUNCATION_NOTICE_LINES: &str = "showing lines ";

/// The field a command result names the file its output was kept in, when the
/// client kept it in one rather than handing it over.
///
/// Pinned by observation O6 of the same fixture: thirty recorded results, every
/// one of them a command result, none of them smaller than the threshold the
/// published table names, and no command result past that threshold without
/// it. The model gets a pointer and a two-kilobyte preview, so whatever the
/// command line asked for, the lines are not what arrived.
pub(super) const PERSISTED_OUTPUT_PATH: &str = "persistedOutputPath";

/// The same fact as the envelope the model reads, for a result that reaches a
/// hook as text rather than as the object O6 pins. Quoted from the same
/// observation; no recorded result carries it in `tool_response`, and it is
/// read only at the front of a text result, so a command that prints the word
/// cannot be mistaken for the client having said it.
pub(super) const PERSISTED_OUTPUT_MARKER: &str = "<persisted-output>";

/// How many lines the file has, as the client counts them.
///
/// Pinned by O7 of the same fixture, which records both what it settles and
/// what it costs: a result whose `numLines` reaches this number delivered the
/// file, and the number is one larger than Rift's own count wherever the bytes
/// end in a newline. The second half is why it is only ever read *with* the
/// delivered run and never instead of it.
pub(super) const DELIVERY_TOTAL_LINES: &str = "totalLines";

/// What this result states about the delivery it answered, in the one shape a
/// recorded run has pinned.
///
/// One family only, because one family's result shape is pinned. Antigravity
/// and Codex publish no such statement that any recorded run has shown, and a
/// missing statement is never read as a complete delivery: it is silence, and
/// silence leaves the recording rule that bounds what a slice may be credited
/// with (`max_delivered_bytes`) as the only guard.
///
/// **Three statements, from two fields and a sentence.**
///
/// *Cut short* is the flag set, or — for a result that reaches a hook as text
/// rather than as the object O5 pins — the notice in the text. It names the
/// run the tool got to, and where nothing names one the delivery is unstated.
/// This is the backstop the size rule cannot see past: text denser than the
/// density the rule is derived at, and a command whose output the client kept
/// in a file when the slice it was asked for looked small enough to survive.
///
/// *Answered in full* is the flag set to `false`, or — the shape O7 records,
/// where an untruncated result carries no flag at all — a run that reaches the
/// client's own `totalLines`. Either way it is the client's own word about its
/// own delivery, and it is the only evidence there is for a slice too large
/// for density to vouch for. A run that stops short of `totalLines` with no
/// flag beside it says nothing: it is neither, and this returns `None` rather
/// than reading an absence as a fact.
pub(super) fn stated_delivery(
    agent: &rift_core::AgentId,
    payload: &serde_json::Value,
) -> Option<rift_agent::window_journal::StatedDelivery> {
    use rift_agent::window_journal::StatedDelivery;
    if *agent != rift_core::AgentId::CLAUDE_CODE {
        return None;
    }
    let response = payload.get("tool_response")?;
    // A command's output is not cut short in place the way a read is: past a
    // threshold in characters the client writes the whole of it to a file and
    // hands the model the path. Nothing then says how far the output got,
    // because none of it arrived as output — so the delivery is unstated and
    // the read keeps the line it started on.
    let persisted = response
        .get(PERSISTED_OUTPUT_PATH)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|path| !path.is_empty())
        || response
            .as_str()
            .is_some_and(|text| text.starts_with(PERSISTED_OUTPUT_MARKER));
    if persisted {
        return Some(StatedDelivery::Unstated);
    }
    let file = response.get("file");
    let stated = |file: &serde_json::Value, name: &str| -> Option<u32> {
        u32::try_from(file.get(name)?.as_u64()?).ok()
    };
    let flag = file
        .and_then(|file| file.get(TRUNCATED_BY_TOKEN_CAP))
        .and_then(serde_json::Value::as_bool);
    let notice = response
        .as_str()
        .or_else(|| response.get("content")?.as_str())
        .filter(|text| text.contains(TRUNCATION_NOTICE));
    let from_object = file.and_then(|file| {
        let lines = stated(file, DELIVERY_NUM_LINES)?;
        let lo = stated(file, DELIVERY_START_LINE).unwrap_or(1);
        rift_agent::window_journal::Interval::new(lo, lo.checked_add(lines.checked_sub(1)?)?)
    });
    // The cut-short reading comes first, so a result that both sets the flag
    // and carries a contradictory count is read the way that can only cost a
    // saving.
    if flag == Some(true) || notice.is_some() {
        let run = from_object.or_else(|| notice.and_then(notice_lines));
        return Some(run.map_or(StatedDelivery::Unstated, StatedDelivery::Truncated));
    }
    let run = from_object?;
    let reaches_the_end = file
        .and_then(|file| stated(file, DELIVERY_TOTAL_LINES))
        .is_some_and(|total| run.hi >= total);
    (flag == Some(false) || reaches_the_end).then_some(StatedDelivery::Complete(run))
}

/// The run of lines a truncation notice names, from the `showing lines A-B`
/// clause of the sentence O5 quotes.
///
/// Anything it cannot read outright is no run at all rather than a guess: the
/// caller's answer to that is the read's own first line, which is the smallest
/// claim there is.
fn notice_lines(notice: &str) -> Option<rift_agent::window_journal::Interval> {
    let clause = notice.split_once(TRUNCATION_NOTICE_LINES)?.1;
    let digits = |text: &str| text.trim().parse::<u32>().ok();
    let (lo, rest) = clause.split_once('-')?;
    let hi = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    rift_agent::window_journal::Interval::new(digits(lo)?, digits(hi)?)
}

/// The same reconciliation against what this call was credited with before its
/// result arrived, and nothing at all where the result stated nothing.
pub(super) fn reconcile_stated_delivery(
    journal: &crate::window_journal::WindowJournal,
    read: &rift_agent::window_journal::ResolvedRead,
    stated: Option<rift_agent::window_journal::StatedDelivery>,
    slice_bytes: usize,
    limits: rift_agent::window_journal::Limits,
    turn: u32,
    at_ms: u64,
) {
    if let Some(delivered) = stated {
        reconcile_delivered_lines(
            journal,
            read,
            read.range_delivered(slice_bytes, limits),
            delivered,
            turn,
            at_ms,
        );
    }
}

/// Move this window's coverage to what a result says it was handed — in
/// whichever direction the statement points.
///
/// Both directions are the same rule, decided by
/// [`rift_agent::window_journal::reconcile_delivery`], and only the writing is
/// here:
///
/// - **credit**, where the result states more than the journal was able to
///   claim. Coverage is a union of `read` intervals, so the addition is one
///   append and costs the window nothing it already had. This is the only
///   route by which a delivery too large for the size rule to vouch for ever
///   becomes coverage, and it arrives one hook after the call;
/// - **take back**, where the result states less. The correction is an
///   eviction and a re-record, because a union has no subtraction in it: the
///   window loses the savings its other reads had earned and is credited
///   again, immediately, with exactly what this result states arrived. It is
///   the coarse instrument on purpose — the alternative, rewriting the
///   window's live lines with one of them clipped, trades a rare and cheap
///   loss of savings for a rewriting path whose own failure mode is a false
///   claim of coverage, which is the expensive direction.
///
/// Nothing is written where the record and the statement already agree, so the
/// ordinary read — delivered whole, recorded whole before it ever ran — costs a
/// lookup and no line at all.
///
/// Returns whether it wrote anything.
fn reconcile_delivered_lines(
    journal: &crate::window_journal::WindowJournal,
    read: &rift_agent::window_journal::ResolvedRead,
    recorded: rift_agent::window_journal::Interval,
    delivered: rift_agent::window_journal::StatedDelivery,
    turn: u32,
    at_ms: u64,
) -> bool {
    use rift_agent::window_journal::{reconcile_delivery, EvictReason, Line, Reconcile};
    let credit = |range| {
        let _ = journal.append(&Line::Read {
            turn,
            path: read.path.clone(),
            hash: read.hash,
            range,
            total_lines: read.total_lines,
            at_ms,
        });
    };
    match reconcile_delivery(read, recorded, delivered) {
        Reconcile::Unchanged => false,
        Reconcile::Credit(range) => {
            credit(range);
            true
        }
        Reconcile::TakeBack(kept) => {
            let _ = journal.append(&Line::Evict {
                turn,
                reason: EvictReason::Truncate,
                at_ms,
            });
            if let Some(range) = kept {
                credit(range);
            }
            true
        }
    }
}

/// Whether a pre-call slice fits every verified interception boundary.
///
/// **Unchanged by the client's own statement about a delivery, because at this
/// boundary there is no delivery to state.** A call that has not run has
/// answered nothing, so a slice past the size density can vouch for still runs
/// untouched and still creates no coverage here; the coverage arrives one hook
/// later, from the result, through
/// [`rift_agent::window_journal::reconcile_delivery`]. What that costs is a
/// parallel identical read issued in the same turn: both calls reach this
/// boundary before either result does, so neither can be declined on the
/// other. That is a saving not taken, which is the direction every rule on
/// this path leans.
pub(super) fn gate_delivery_is_actionable(
    read: &rift_agent::window_journal::ResolvedRead,
    slice_bytes: usize,
    limits: rift_agent::window_journal::Limits,
) -> bool {
    slice_bytes <= limits.max_slice_bytes && read.delivery_is_complete(slice_bytes, limits)
}
