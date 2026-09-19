//! What a read-shaped call can have delivered: the request resolved against
//! the file's own length, the byte ceilings a delivery is held to, and the
//! reconciliation with what the client's result states about itself.
//!
//! A separate module because crediting is the one place a recorded range can
//! over-claim, and the rules that bound it are read together.

use rift_core::Sha256Id;

use super::{DenyKey, Interval, Limits};
use crate::{ReadChannel, ReadRequest};

/// A read-shaped call with the two facts an adapter cannot supply — the
/// revision the file is at, and how many lines it has — filled in by whoever
/// had the bytes in hand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRead {
    /// The file, in the one spelling coverage is keyed by.
    pub path: String,
    /// The revision the bytes hash to now.
    pub hash: Sha256Id,
    /// The lines the tool can have delivered, clamped by the file's length.
    pub range: Interval,
    /// How many lines the file has.
    pub total_lines: u32,
    /// Which channel carried the answer, carried through from the request
    /// because the ceiling a delivery is held to depends on it.
    pub channel: ReadChannel,
}

impl ResolvedRead {
    /// A request resolved against the file it names, or `None` when the range
    /// it asks for is not a range of that file: an empty file, a start past
    /// the end, a start before line one. Those cost nothing to allow and
    /// there is nothing to record about them.
    ///
    /// This is also the one place a window counted from the *bottom* becomes
    /// line numbers. [`crate::ReadSpan::LastLines`] names a count and not a start,
    /// because the start is `total - count + 1` and only here is the total
    /// known — the same division of labour [`crate::HiKind::END_OF_FILE`] already
    /// has, read the other way up. A count of nothing is not a range and is
    /// refused rather than rounded to one line.
    #[must_use]
    pub fn new(request: &ReadRequest, hash: Sha256Id, total_lines: u32) -> Option<Self> {
        if total_lines == 0 {
            return None;
        }
        let (lo, hi) = match request.span {
            crate::ReadSpan::LastLines { count } if count > 0 => {
                (total_lines.saturating_sub(count - 1).max(1), total_lines)
            }
            crate::ReadSpan::LastLines { .. } => return None,
            _ => request.conservative_lines()?,
        };
        if lo == 0 || lo > total_lines {
            return None;
        }
        let hi = hi.min(total_lines);
        Some(Self {
            path: request.path.clone(),
            hash,
            range: Interval::new(lo, hi)?,
            total_lines,
            channel: request.channel,
        })
    }

    /// The run of lines this read may be credited with, given how many bytes
    /// the slice it asked for holds.
    ///
    /// The whole run when the slice is small enough that the client's own cap
    /// cannot have cut it short; otherwise the single line it started on. The
    /// collapse is the same rule an unpinned default window already gets, and
    /// for the same reason: what a record must never do is answer for lines
    /// the model was not handed.
    ///
    /// Which ceiling applies is the read's channel. A terminal answer is held
    /// to the tighter of the two, because a client that spills terminal output
    /// to a file has handed the window a pointer, and a pointer is not the
    /// lines however precisely the command named them.
    #[must_use]
    pub fn delivery_is_complete(&self, slice_bytes: usize, limits: Limits) -> bool {
        // The two ceilings are compared differently because they are
        // different KINDS of number. The tool bound is arithmetic — a measured
        // token cap divided by a measured density — so the value itself is
        // inside what was measured and the whole run at exactly that size is
        // vouchable. The terminal bound is a boundary nobody has stood on: the
        // largest output ever watched arriving inline is 29,999 characters,
        // and results of exactly 30,000 were recorded carrying the client's
        // persisted-output field. So a terminal result is credited in full
        // only STRICTLY under its bound — one character wide, and on the side
        // the record supports.
        match self.channel {
            ReadChannel::Tool => slice_bytes <= limits.max_delivered_bytes,
            ReadChannel::Shell => {
                slice_bytes < limits.max_shell_delivered_bytes
                    && slice_bytes <= limits.max_delivered_bytes
            }
        }
    }

    /// The line range that can be credited under the legacy journal API.
    ///
    /// Hook callers now reject incomplete delivery before recording at all;
    /// the single-line fallback remains for replay compatibility with older
    /// journal records and direct users of this pure helper.
    #[must_use]
    pub fn range_delivered(&self, slice_bytes: usize, limits: Limits) -> Interval {
        if self.delivery_is_complete(slice_bytes, limits) {
            return self.range;
        }
        Interval {
            lo: self.range.lo,
            hi: self.range.lo,
        }
    }

    /// The key a decline of this request is remembered under.
    #[must_use]
    pub fn deny_key(&self) -> DenyKey {
        DenyKey {
            path: self.path.clone(),
            hash: self.hash,
            range: self.range,
        }
    }
}

/// What a family's own tool result states about the delivery it answered.
///
/// A range credited from the bytes is arithmetic over a measured density, and
/// [`Limits::max_delivered_bytes`] is where that arithmetic runs out: past it
/// a slice *may* have been cut short, so the window is credited with nothing.
/// This is the other source, and where it exists it is the better one — the
/// client says outright how much of the file it handed over, so a delivery
/// larger than any density argument could vouch for is nonetheless exactly
/// known.
///
/// Three cases, because three are what a pinned result object can say: the
/// call was answered in full, it was cut short at a named line, or it was cut
/// short and no line was named. **Silence is not one of them.** A result that
/// says nothing about itself is *absent* here rather than complete, which is
/// why every call site holds an `Option<StatedDelivery>` and an absent
/// statement leaves the byte ceiling as the only guard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatedDelivery {
    /// The client said the call was answered in full, and named the run it
    /// handed over.
    Complete(Interval),
    /// The client said the call was cut short, and named how far it got.
    Truncated(Interval),
    /// The client said the call was cut short and did not say where, so
    /// nothing past the line the read started on can be vouched for.
    Unstated,
}

/// How a window's coverage has to move to agree with what a result states.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reconcile {
    /// The record and the statement already agree.
    Unchanged,
    /// The statement names more than the call was credited with, and coverage
    /// is a union, so this run is simply added to it. Nothing is lost and no
    /// boundary is written.
    Credit(Interval),
    /// The statement names less. A union has no subtraction in it, so the
    /// window is emptied and credited again with this run — or with nothing,
    /// where the statement leaves no line of this read standing at all.
    TakeBack(Option<Interval>),
}

/// What one result's own statement does to the coverage its call was credited
/// with.
///
/// **One rule, in both directions.** A statement that a call was answered in
/// full and one that it was cut short name the same thing — the run the client
/// says the window received — and the only difference between them is which
/// side of `recorded` that run falls on. Naming less than the record claims
/// takes lines back; naming more credits them, and that second direction is
/// the one a byte ceiling can never reach on its own: a slice over
/// [`Limits::max_delivered_bytes`] is credited with nothing because *density*
/// cannot vouch for it, and a client stating its own delivery has vouched for
/// it in the one way that is not a guess.
///
/// `recorded` is what the call was credited with before the result arrived —
/// [`ResolvedRead::range_delivered`] over the slice it asked for. Where the
/// ceiling refused that slice outright and nothing was written at all, it is
/// the read's own first line, and every line the statement names past it is a
/// credit rather than a correction.
///
/// Three clamps keep a statement from over-claiming:
///
/// - the run is intersected with [`ResolvedRead::range`], which is already
///   clamped to the file's own length. A client that counts the empty stretch
///   after a final newline as a line states one line more than the bytes hold,
///   and the intersection spends that line on a saving rather than on a claim;
/// - a run with no line in common with what this read could have delivered is
///   no run at all, and the window loses the claim rather than gaining a
///   strange one;
/// - a **terminal** answer is never credited from a statement. What a family
///   publishes is its read tool's result object; a command's result says only
///   that its output went to a file, so the bound a shell read is held to
///   stays [`Limits::max_shell_delivered_bytes`] whatever else arrives.
#[must_use]
pub fn reconcile_delivery(
    read: &ResolvedRead,
    recorded: Interval,
    stated: StatedDelivery,
) -> Reconcile {
    let run = match stated {
        StatedDelivery::Complete(run) | StatedDelivery::Truncated(run) => {
            Interval::new(read.range.lo.max(run.lo), read.range.hi.min(run.hi))
        }
        StatedDelivery::Unstated => Interval::new(recorded.lo, recorded.lo),
    };
    match run {
        None => Reconcile::TakeBack(None),
        Some(run) if run == recorded => Reconcile::Unchanged,
        Some(run) if run.contains(recorded) && read.channel == ReadChannel::Tool => {
            Reconcile::Credit(run)
        }
        Some(run) if run.contains(recorded) => Reconcile::Unchanged,
        Some(run) => Reconcile::TakeBack(Some(run)),
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{hash, request, resolved, run, whole_file};
    use super::super::{
        decide, record, Decision, DenyShape, EditEcho, Journal, Line, Mode,
        DEFAULT_MAX_DELIVERED_BYTES, DEFAULT_MAX_SHELL_DELIVERED_BYTES,
    };
    use super::*;
    use crate::respond::Channel;
    use crate::{HiKind, ReadSpan};

    /// The one channel a decline may travel on: documented **and** observed
    /// reaching the model.
    const CONFIRMED: Channel = Channel::DenyWithReason {
        reason_field: "permissionDecisionReason",
        confirmed: true,
    };

    /// ★ **A shell read past the persistence threshold is credited with one
    /// line, whatever its command line claimed.**
    ///
    /// `sed -n '1,3000p'` names three thousand lines and the recogniser
    /// records them as an explicit range, which is right about the command
    /// and wrong about the window: past the measured threshold this client
    /// does not hand the model the output at all, it writes it to a file and
    /// hands over the path. The same bytes through a read tool are under the
    /// tool ceiling and stay whole — which is the point of keeping the two
    /// ceilings apart rather than taking the tighter one everywhere and
    /// giving up savings a read tool actually earned.
    #[test]
    fn a_shell_read_past_the_persistence_threshold_grants_one_line() {
        let limits = Limits::default();
        let slice = |channel| {
            ResolvedRead::new(
                &ReadRequest {
                    path: "src/long.rs".to_owned(),
                    span: ReadSpan::lines(1, HiKind::Explicit(3_000)),
                    channel,
                },
                hash(7),
                3_000,
            )
            .expect("a resolved read")
        };
        // A ~35 KB slice: over the shell threshold, under the tool ceiling.
        let bytes = 35_000;
        assert!(bytes > DEFAULT_MAX_SHELL_DELIVERED_BYTES);
        assert!(bytes < DEFAULT_MAX_DELIVERED_BYTES);
        assert_eq!(
            slice(ReadChannel::Shell).range_delivered(bytes, limits),
            run(1, 1),
            "the window holds a filename and a preview, not three thousand lines"
        );
        assert_eq!(
            slice(ReadChannel::Tool).range_delivered(bytes, limits),
            run(1, 3_000),
            "the same bytes inline are the lines they say they are"
        );
        // The recorded line follows, because that is the one kind of line
        // that grants a later decline something to rest on.
        assert_eq!(
            record(
                &slice(ReadChannel::Shell),
                &Decision::Allow,
                bytes,
                limits,
                1,
                1
            ),
            Line::Read {
                turn: 1,
                path: "src/long.rs".to_owned(),
                hash: hash(7),
                range: run(1, 1),
                total_lines: 3_000,
                at_ms: 1,
            }
        );
        // One character under the threshold is the largest output anyone has
        // watched arrive inline, and it is credited whole. AT the threshold
        // nobody has: every recorded result of exactly that size came back as
        // a filename, so that is where the credit stops.
        assert_eq!(
            slice(ReadChannel::Shell)
                .range_delivered(DEFAULT_MAX_SHELL_DELIVERED_BYTES - 1, limits),
            run(1, 3_000)
        );
        assert_eq!(
            slice(ReadChannel::Shell).range_delivered(DEFAULT_MAX_SHELL_DELIVERED_BYTES, limits),
            run(1, 1),
            "the bound itself was never seen delivered whole"
        );
    }

    /// A slice inside the bound is credited with every line it asked for; one
    /// byte over it is credited with the line it started on and nothing after,
    /// whatever the tool was asked for.
    #[test]
    fn a_slice_past_the_delivery_bound_is_credited_with_one_line() {
        let read = ResolvedRead::new(
            &ReadRequest {
                path: "src/big.rs".to_owned(),
                span: ReadSpan::lines(40, HiKind::Explicit(900)),
                channel: ReadChannel::Tool,
            },
            hash(1),
            900,
        )
        .expect("a resolved read");
        let limits = Limits {
            max_delivered_bytes: 1_000,
            ..Limits::default()
        };
        assert_eq!(read.range_delivered(1_000, limits), run(40, 900));
        assert_eq!(read.range_delivered(1_001, limits), run(40, 40));
        // The line the record leaves behind follows the same bound, and only
        // for the kind of line that grants coverage.
        assert_eq!(
            record(&read, &Decision::Allow, 1_001, limits, 1, 1),
            Line::Read {
                turn: 1,
                path: "src/big.rs".to_owned(),
                hash: hash(1),
                range: run(40, 40),
                total_lines: 900,
                at_ms: 1,
            }
        );
        let Line::Deny { range, .. } = record(
            &read,
            &Decision::Deny(DenyShape::Plain),
            1_001,
            limits,
            1,
            1,
        ) else {
            panic!("a declined call is recorded as a decline");
        };
        assert_eq!(
            range,
            run(40, 900),
            "a decline records what was asked for: that is what sizes the pool"
        );
    }

    /// **A window counted from the bottom becomes line numbers exactly once**,
    /// here, where the file's length is already in hand for the hash.
    ///
    /// The three cases are the whole contract: a count inside the file is the
    /// run ending at its last line; a count past the file's end is the whole
    /// file and never a line before its first; and a count of nothing is not a
    /// range at all and is refused rather than rounded up to one line.
    #[test]
    fn a_window_counted_from_the_end_resolves_against_the_files_own_length() {
        let from_end = |count: u32| ReadRequest {
            path: "src/lib.rs".to_owned(),
            span: ReadSpan::LastLines { count },
            channel: ReadChannel::Shell,
        };
        let range = |count: u32, total: u32| {
            ResolvedRead::new(&from_end(count), hash(1), total).map(|read| read.range)
        };
        assert_eq!(range(40, 400).map(|r| (r.lo, r.hi)), Some((361, 400)));
        assert_eq!(range(1, 400).map(|r| (r.lo, r.hi)), Some((400, 400)));
        assert_eq!(range(400, 400).map(|r| (r.lo, r.hi)), Some((1, 400)));
        assert_eq!(range(9_000, 400).map(|r| (r.lo, r.hi)), Some((1, 400)));
        assert_eq!(range(u32::MAX, 400).map(|r| (r.lo, r.hi)), Some((1, 400)));
        assert_eq!(range(0, 400), None);
        assert_eq!(range(40, 0), None);
        // The request on its own still names no lines: the whole point of the
        // variant is that only this step may say which they are.
        assert_eq!(from_end(40).conservative_lines(), None);
    }

    #[test]
    fn a_range_that_is_not_a_range_of_the_file_resolves_to_nothing() {
        assert_eq!(
            ResolvedRead::new(&request("p", 1, HiKind::Explicit(10)), hash(1), 0),
            None,
            "an empty file"
        );
        assert_eq!(
            ResolvedRead::new(&request("p", 40, HiKind::Explicit(50)), hash(1), 30),
            None,
            "a start past the end"
        );
        let clamped = ResolvedRead::new(&request("p", 5, HiKind::Explicit(500)), hash(1), 30)
            .expect("a start inside the file resolves");
        assert_eq!(clamped.range, run(5, 30), "the end is clamped to the file");
    }

    /// **A limit-less read holds the whole file it was delivered.**
    ///
    /// The one place the two halves of the rule meet: an adapter that knows
    /// the delivery runs to the end of the file states that and nothing more,
    /// and this step — the only one with the bytes in hand — turns it into the
    /// file's own last line and then asks whether a delivery that size can be
    /// vouched for at all.
    ///
    /// Derived from the recorded spike behind the Claude Code tool-field pin
    /// (observation O7): six limit-less reads of files from 25,000 to 100,000
    /// bytes each came back with `startLine` 1 and `numLines` equal to the
    /// file's own `totalLines`, and none of them carried the truncation flag
    /// that O5 pins for the reads that stopped short. So the whole file is
    /// what such a call delivers, up to the size at which the client's token
    /// cap could have cut it — and that size, not a line count, is the bound.
    #[test]
    fn a_limit_less_read_holds_the_whole_file_it_was_delivered() {
        let whole = ResolvedRead::new(
            &request("src/lib.rs", 1, HiKind::DefaultWindow(HiKind::END_OF_FILE)),
            hash(3),
            900,
        )
        .expect("the start is inside the file");
        assert_eq!(whole.range, run(1, 900), "the file's own extent");

        // Under the delivery bound, that whole extent is what the window is
        // credited with — the line a later decision is made of.
        let bytes = DEFAULT_MAX_DELIVERED_BYTES;
        let line = record(&whole, &Decision::Allow, bytes, Limits::default(), 0, 1);
        let Line::Read { range, .. } = &line else {
            panic!("an allowed read records a read line, got {line:?}");
        };
        assert_eq!(*range, run(1, 900));

        // And it answers for the repeat that follows, which is the whole point
        // of recording it: the same call again is inside coverage.
        let journal = Journal::from_lines(vec![line]);
        assert!(matches!(
            decide(
                &whole,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                bytes,
                Limits::default()
            ),
            Decision::Deny(_)
        ));

        // An offset with no cap is the same delivery starting later.
        let tail = ResolvedRead::new(
            &request(
                "src/lib.rs",
                700,
                HiKind::DefaultWindow(HiKind::END_OF_FILE),
            ),
            hash(3),
            900,
        )
        .expect("the start is inside the file");
        assert_eq!(tail.range, run(700, 900));
    }

    /// **A limit-less read past the delivery bound holds one line.**
    ///
    /// The other side of the same boundary, and the reason the rule is a size
    /// rather than a flag. A file large enough that the client's token cap
    /// *could* have stopped the read short is credited with the line it began
    /// on, whatever the range asked for — the same collapse an unbounded range
    /// has always had, now reached by measurement instead of by absence.
    ///
    /// One byte decides it, in both directions, so the bound is the bound and
    /// not a neighbourhood of one.
    #[test]
    fn a_limit_less_read_past_the_delivery_bound_holds_one_line() {
        let read = ResolvedRead::new(
            &request("src/lib.rs", 1, HiKind::DefaultWindow(HiKind::END_OF_FILE)),
            hash(3),
            900,
        )
        .expect("the start is inside the file");
        let limits = Limits::default();

        assert_eq!(
            read.range_delivered(DEFAULT_MAX_DELIVERED_BYTES, limits),
            run(1, 900),
            "at the bound the whole delivery is vouchable"
        );
        assert_eq!(
            read.range_delivered(DEFAULT_MAX_DELIVERED_BYTES + 1, limits),
            run(1, 1),
            "one byte past it, only the line it started on"
        );

        // What that costs is a saving, never a wrong decision: the repeat of a
        // read too large to vouch for runs.
        let line = record(
            &read,
            &Decision::Allow,
            DEFAULT_MAX_DELIVERED_BYTES + 1,
            limits,
            0,
            1,
        );
        let journal = Journal::from_lines(vec![line]);
        assert_eq!(
            decide(
                &read,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                DEFAULT_MAX_DELIVERED_BYTES + 1,
                limits
            ),
            Decision::Allow
        );
    }

    /// **The client's own statement credits a delivery no density could vouch
    /// for.**
    ///
    /// The size bound is arithmetic — a measured token cap over a measured
    /// density — and past it the window is credited with one line, so every
    /// later read of a repository's largest and most-read files starts from
    /// coverage that can never satisfy anything. This is the other evidence
    /// and it is not a guess: the result object names `startLine`, `numLines`
    /// and `totalLines`, and a run reaching the file's own length with no
    /// truncation flag beside it is the client saying it handed the file over
    /// (the recorded shape of O7 in the Claude Code tool-field pin). Believed,
    /// whatever the slice weighs.
    #[test]
    fn a_delivery_the_client_states_in_full_is_credited_past_the_size_bound() {
        let read = whole_file(900);
        let limits = Limits::default();
        let recorded = read.range_delivered(DEFAULT_MAX_DELIVERED_BYTES * 2, limits);
        assert_eq!(recorded, run(1, 1), "density vouches for one line of it");
        assert_eq!(
            reconcile_delivery(&read, recorded, StatedDelivery::Complete(run(1, 901))),
            Reconcile::Credit(run(1, 900)),
            "and the client's own word for the rest of it"
        );
    }

    /// **The client counts one line more than the bytes do, and that line is
    /// never credited.**
    ///
    /// O7: every file in the recorded spike ends in a newline and the client
    /// reports one `totalLines` more than it holds newline-terminated lines,
    /// counting the empty stretch after the final newline. A statement is
    /// therefore intersected with the run this read could have delivered —
    /// already clamped to Rift's own count — rather than believed outright.
    /// The direction of the difference is a line of saving forgone, never a
    /// line of coverage invented.
    #[test]
    fn the_line_the_client_counts_past_the_last_newline_is_never_credited() {
        let read = whole_file(250);
        assert_eq!(read.range, run(1, 250), "Rift's own count");
        assert_eq!(
            reconcile_delivery(&read, run(1, 1), StatedDelivery::Complete(run(1, 251))),
            Reconcile::Credit(run(1, 250)),
            "the client's 251st line is not a line of these bytes"
        );
    }

    /// **A statement that the call was cut short and one that it was answered
    /// in full are one rule**, and which way coverage moves is decided by the
    /// record rather than by the statement.
    ///
    /// The same delivery — forty lines, stated by the same result — takes
    /// eight hundred and sixty lines back from a record that claimed the file,
    /// and credits thirty-nine to a record the size bound had already cut to
    /// one. A statement that agrees with the record writes nothing at all.
    #[test]
    fn a_truncation_and_a_completion_are_one_rule_read_from_both_sides() {
        let read = whole_file(900);
        let limits = Limits::default();

        let whole = read.range_delivered(DEFAULT_MAX_DELIVERED_BYTES, limits);
        assert_eq!(whole, run(1, 900));
        assert_eq!(
            reconcile_delivery(&read, whole, StatedDelivery::Truncated(run(1, 40))),
            Reconcile::TakeBack(Some(run(1, 40))),
            "the record claimed more than arrived"
        );

        let one = read.range_delivered(DEFAULT_MAX_DELIVERED_BYTES + 1, limits);
        assert_eq!(one, run(1, 1));
        assert_eq!(
            reconcile_delivery(&read, one, StatedDelivery::Truncated(run(1, 40))),
            Reconcile::Credit(run(1, 40)),
            "and here it claimed less, from the very same sentence"
        );

        assert_eq!(
            reconcile_delivery(&read, whole, StatedDelivery::Complete(run(1, 901))),
            Reconcile::Unchanged,
            "a statement the record already agrees with costs a lookup"
        );
    }

    /// A truncation that names nothing keeps the line the read started on, and
    /// a statement with no line of this read in it leaves no claim standing.
    #[test]
    fn a_statement_that_names_no_line_of_this_read_leaves_nothing_standing() {
        let read = whole_file(900);
        assert_eq!(
            reconcile_delivery(&read, run(1, 900), StatedDelivery::Unstated),
            Reconcile::TakeBack(Some(run(1, 1))),
            "cut short with no line named is the smallest claim there is"
        );
        assert_eq!(
            reconcile_delivery(&read, run(1, 1), StatedDelivery::Unstated),
            Reconcile::Unchanged,
            "and a record already that small has nothing to lose"
        );
        let tail = resolved("src/lib.rs", 3, 700, 900, 900);
        assert_eq!(
            reconcile_delivery(&tail, run(700, 900), StatedDelivery::Complete(run(1, 40))),
            Reconcile::TakeBack(None),
            "a run sharing no line with this read is no run of it at all"
        );
    }

    /// **A terminal answer is never credited from a statement.**
    ///
    /// The fields a statement is read from belong to a read tool's result
    /// object. A command's result says something quite different: past a size
    /// in characters the client writes the output to a file and hands the model a
    /// pointer and a preview (O6), so no sentence a command's result carries
    /// can raise what a shell read is credited with above
    /// [`Limits::max_shell_delivered_bytes`]. The correcting direction still
    /// reaches it, because taking a claim back can only cost a saving.
    #[test]
    fn a_terminal_answer_is_never_credited_from_a_statement() {
        let shell = ResolvedRead::new(
            &ReadRequest {
                path: "src/lib.rs".to_owned(),
                span: ReadSpan::lines(1, HiKind::DefaultWindow(HiKind::END_OF_FILE)),
                channel: ReadChannel::Shell,
            },
            hash(3),
            900,
        )
        .expect("a shell read of a file with lines in it is a range of it");
        assert_eq!(
            reconcile_delivery(&shell, run(1, 1), StatedDelivery::Complete(run(1, 901))),
            Reconcile::Unchanged,
            "the shell bound stands whatever a result says about itself"
        );
        assert_eq!(
            reconcile_delivery(&shell, run(1, 900), StatedDelivery::Unstated),
            Reconcile::TakeBack(Some(run(1, 1))),
            "and the correction still reaches the terminal channel"
        );
    }

    #[test]
    fn an_unknown_or_byte_span_never_resolves_to_line_coverage() {
        for span in [
            ReadSpan::Unknown,
            ReadSpan::VerifiedBytes {
                offset: 7,
                length: 60,
            },
        ] {
            let request = ReadRequest {
                path: "src/lib.rs".to_owned(),
                span,
                channel: ReadChannel::Tool,
            };
            assert_eq!(ResolvedRead::new(&request, hash(3), 900), None);
        }

        // With no fabricated seed line in the journal, a later pinned span is
        // allowed and cannot be observed or denied.
        let journal = Journal::default();
        let wider = resolved("src/lib.rs", 3, 7, 60, 900);
        assert_eq!(
            decide(
                &wider,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                64,
                Limits::default()
            ),
            Decision::Allow
        );
    }
}
