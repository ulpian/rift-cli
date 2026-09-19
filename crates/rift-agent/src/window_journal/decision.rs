//! The decision taken over a journal: the settings it needs, the shape a
//! decline takes, and the line each decision leaves behind.
//!
//! A separate module because the decision is the part that answers while the
//! agent waits. Everything it consults is a pure query over rows, and this is
//! where those queries meet the mode and the family's channel.

use super::{
    Interval, Journal, Line, ResolvedRead, DEFAULT_MAX_DELIVERED_BYTES,
    DEFAULT_MAX_SHELL_DELIVERED_BYTES,
};
use crate::respond::Channel;

/// How far interception is allowed to go.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Nothing is decided and nothing is recorded. This is where an
    /// unreadable policy lands, because a lever whose settings cannot be read
    /// is a lever nobody is holding.
    Off,
    /// Everything is decided and recorded, and nothing is acted on — the mode
    /// that sizes the pool before anyone's session changes.
    #[default]
    Observe,
    /// A read the journal proves is already held may be declined, once.
    Enforce,
}

/// Whether a family's own edit tool is pinned to tell the model that the file
/// it just wrote is still current in its context.
///
/// A decline that rests on an `edit` line says, in effect, "you wrote these
/// bytes, so you hold them". That is a claim about the *family's* tooling, and
/// it is true only where a committed artifact says the edit result tells the
/// model so. Where it does not, the verification read is the agent's only
/// sight of its own edit, and taking that away buys a saving with the one
/// thing the read exists to provide — so a decline never rests on an edit line
/// there, in any shape.
///
/// [`Default`] is [`Self::Unpinned`], so a family nobody has pinned yet lands
/// on the safe side by omission rather than by anyone remembering to say so.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EditEcho {
    /// A committed pin quotes this family's edit result telling the model the
    /// file it just wrote is current in its context.
    Pinned,
    /// Nothing pinned says what this family's edit tools return.
    #[default]
    Unpinned,
}

/// The numbers the decision needs that are settings rather than facts.
///
/// They arrive as an argument because where they come from — a policy file,
/// a per-family override, a default — is not this module's business, and a
/// pure decision with a hidden source of truth is not a pure decision.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Above this many bytes a slice is recorded but never treated as held:
    /// past the point a read was measured to truncate, what the window kept of
    /// an earlier delivery is a page rather than the range, so a record saying
    /// the range arrived is true of the tool and false of the context.
    pub max_slice_bytes: usize,
    /// Below this many lines a slice is decided and recorded but never
    /// declined: the reason frame the model reads instead has a price of its
    /// own, and under some length it costs more than the range it replaces.
    pub min_slice_lines: u32,
    /// How many lines of surrounding context a suggested window adds on each
    /// side of a changed site.
    pub context_lines: u32,
    /// Above this many bytes a slice is not recorded as held at all: what a
    /// window is credited with collapses to the one line the read started on,
    /// the way an unpinned default window already does.
    ///
    /// Different in kind from [`Limits::max_slice_bytes`], and larger numbers
    /// are not safer here. That one bounds what may be **declined**: a slice
    /// over it is still recorded, and forgetting to record would only cost a
    /// saving. This one bounds what may be **recorded**, and a slice over it
    /// that is recorded anyway is a claim that a context holds lines a client
    /// may have cut off — the one direction of error a decline cannot survive.
    pub max_delivered_bytes: usize,
    /// The same bound for a read whose answer came back through the terminal,
    /// where the client's ceiling is a size in characters rather than a token
    /// cap and sits lower. Whichever of the two is tighter is what a shell
    /// read is held to.
    pub max_shell_delivered_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_slice_bytes: crate::DEFAULT_MAX_SLICE_BYTES,
            min_slice_lines: crate::DEFAULT_MIN_SLICE_LINES,
            context_lines: 3,
            max_delivered_bytes: DEFAULT_MAX_DELIVERED_BYTES,
            max_shell_delivered_bytes: DEFAULT_MAX_SHELL_DELIVERED_BYTES,
        }
    }
}

/// What a decline would say, when there is one to make.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DenyShape {
    /// These lines of this revision are the ones already in the window.
    Plain,
    /// The window holds the previous revision in full and the file changed
    /// only where it says, in the numbering the reader is about to see.
    Delta {
        /// Where the file changed, in the numbering of the current bytes.
        sites: Vec<Interval>,
        /// How many lines the file grew by, negative when it shrank.
        shift: i64,
        /// A range worth reading instead: the changed sites with a few lines
        /// of context around them.
        window: Interval,
    },
}

/// What is to be done about a read-shaped call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Let it run, and record what it will have delivered.
    Allow,
    /// It would have been declined, in this shape, and is not. The window is
    /// untouched; the record sizes what declining would be worth.
    Observe(DenyShape),
    /// Decline it once, with this to say for it.
    Deny(DenyShape),
}

/// Whether a family's channel is one a decline may travel on: an explanation
/// field the family documents **and** a recorded run showing the model
/// receiving it.
///
/// A decline whose reason the model never sees is an unexplained tool
/// failure, and retrying an unexplained failure is reasonable behaviour, so
/// an unconfirmed channel means the decision is recorded and never acted on.
fn reason_reaches_the_model(channel: Option<Channel>) -> bool {
    matches!(
        channel,
        Some(Channel::DenyWithReason {
            confirmed: true,
            ..
        })
    )
}

/// The smallest run holding every changed site.
fn hull(sites: &[Interval]) -> Option<Interval> {
    Interval::new(
        sites.iter().map(|s| s.lo).min()?,
        sites.iter().map(|s| s.hi).max()?,
    )
}

/// A range worth reading instead of the file: the changed sites taken
/// together, widened by the configured context on each side, clamped to the
/// file's own length.
fn suggested_window(hull: Interval, total_lines: u32, context_lines: u32) -> Option<Interval> {
    Interval::new(
        hull.lo.saturating_sub(context_lines).max(1),
        hull.hi.saturating_add(context_lines).min(total_lines),
    )
}

/// Which shape a decline of this read would take, or `None` when it must not
/// be declined at all.
fn shape_of(
    read: &ResolvedRead,
    journal: &Journal,
    limits: Limits,
    echo: EditEcho,
) -> Option<DenyShape> {
    if !journal.read_coverage(&read.path, read.hash).is_empty() {
        return Some(DenyShape::Plain);
    }
    // Nothing of this revision was read, so what the window holds it holds by
    // having written: the file changed since it last read, and the honest
    // thing to say is where.
    let Some(Line::Edit { sites, shift, .. }) = journal.wrote_in_full(&read.path, read.hash) else {
        return None;
    };
    if echo == EditEcho::Unpinned {
        // Unconditional, and deliberately not a mode: a family whose edit
        // result is not pinned to say the model holds what it wrote is never
        // declined on the strength of an edit line — neither with the sites,
        // nor plainly when the edit covered the whole file. The line stays:
        // coverage of the old bytes must still stop being coverage the moment
        // they change. What does not follow from it, here, is a decision.
        return None;
    }
    let span = hull(sites)?;
    if span.lo <= 1 && span.hi >= read.total_lines {
        // The change is the whole file — a write, not an edit. Naming every
        // line as a changed site tells a reader nothing it did not know.
        return Some(DenyShape::Plain);
    }
    let window = suggested_window(span, read.total_lines, limits.context_lines)?;
    if sites.iter().any(|site| site.contains(read.range)) {
        // Reading exactly the lines that changed is the right thing to do.
        return None;
    }
    Some(DenyShape::Delta {
        sites: sites.clone(),
        shift: *shift,
        window,
    })
}

/// What to do about a read-shaped call, given what this context window has
/// already been handed.
///
/// Every gate below is a reason **not** to decline, and they are checked in
/// the order that makes the cheapest and most certain ones first:
///
/// * the lever is off;
/// * the slice is larger than a client keeps inline, so what the window holds
///   of an earlier delivery may be a pointer rather than the bytes;
/// * the request asks for a line the window was not handed;
/// * this exact request was already declined once, so the repeat is honoured
///   — which is what bounds a model that answers a decline by asking again;
/// * the only thing the window holds this revision by is having written it,
///   and this family's edit result is not pinned to say the model holds what
///   it wrote;
/// * there is no honest shape for the decline to take;
/// * the slice is shorter than the length at which a reason frame stops
///   costing more than the range it replaces — the one gate here that still
///   reaches a decision and records it, because the pool a floor forgoes is a
///   figure the window has to be able to publish.
///
/// Only then does the mode and the family's channel decide between acting on
/// the decision and merely recording it. Whether the bytes are text, whether
/// they are small enough to hash inline, and whether the path resolves inside
/// the work tree are the caller's checks: they need the file, and this module
/// never opens one.
#[must_use]
pub fn decide(
    read: &ResolvedRead,
    journal: &Journal,
    mode: Mode,
    channel: Option<Channel>,
    echo: EditEcho,
    slice_bytes: usize,
    limits: Limits,
) -> Decision {
    if mode == Mode::Off || slice_bytes > limits.max_slice_bytes {
        return Decision::Allow;
    }
    if !journal.coverage(&read.path, read.hash).contains(read.range) {
        return Decision::Allow;
    }
    if journal.denied(&read.deny_key()) {
        return Decision::Allow;
    }
    let Some(shape) = shape_of(read, journal, limits, echo) else {
        return Decision::Allow;
    };
    // The floor, last: a slice too short for a decline to pay for itself is
    // still a decision this window made, and recording it is what lets the
    // observe window price the floor before anyone moves it.
    let pays_for_itself = read.range.lines() >= limits.min_slice_lines;
    if mode == Mode::Enforce && pays_for_itself && reason_reaches_the_model(channel) {
        Decision::Deny(shape)
    } else {
        Decision::Observe(shape)
    }
}

/// The line a decision leaves behind.
///
/// Recording happens on every path, including the one where nothing was
/// decided: an allowed read is what a later decision is made of, and a
/// decision that was only observed is what sizes the pool.
///
/// The three lines are not symmetric, which is why the slice's size is an
/// argument here. A `deny` and an `observe` record what was **asked for** —
/// that is what sizes the pool and what a repeat is matched against. A `read`
/// records what the call can have **delivered**, and a slice larger than
/// [`Limits::max_delivered_bytes`] cannot be vouched for past its first line
/// whatever the tool was asked for, so that is all it is credited with.
#[must_use]
pub fn record(
    read: &ResolvedRead,
    decision: &Decision,
    slice_bytes: usize,
    limits: Limits,
    turn: u32,
    at_ms: u64,
) -> Line {
    match decision {
        Decision::Allow => Line::Read {
            turn,
            path: read.path.clone(),
            hash: read.hash,
            range: read.range_delivered(slice_bytes, limits),
            total_lines: read.total_lines,
            at_ms,
        },
        Decision::Observe(_) => Line::Observe {
            turn,
            path: read.path.clone(),
            hash: read.hash,
            range: read.range,
            at_ms,
        },
        Decision::Deny(_) => Line::Deny {
            turn,
            path: read.path.clone(),
            hash: read.hash,
            range: read.range,
            at_ms,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{hash, read_line, resolved, run, whole_file};
    use super::super::{reconcile_delivery, EvictReason, Reconcile, StatedDelivery};
    use super::*;

    /// The one channel a decline may travel on: documented **and** observed
    /// reaching the model.
    const CONFIRMED: Channel = Channel::DenyWithReason {
        reason_field: "permissionDecisionReason",
        confirmed: true,
    };

    /// The same channel before anyone watched a model receive it.
    const UNCONFIRMED: Channel = Channel::DenyWithReason {
        reason_field: "permissionDecisionReason",
        confirmed: false,
    };

    /// ★ **A key already declined is never declined a second time**, however
    /// many times the same call is made.
    ///
    /// A3 §1 is a loop: a model that answers a decline by making the identical
    /// call again must not meet the same decline, or the conversation cannot
    /// leave the turn. The journal's memory of the decline is tested above;
    /// this is the rule that memory exists for — `decide` consulting it — and
    /// it is the test that reddens when that consultation is short-circuited.
    ///
    /// Both directions are asserted, because a guard that answered `Allow` for
    /// everything would satisfy the first half alone: a DIFFERENT key over the
    /// very same coverage is still declined, so what the guard remembers is
    /// one request and not one window.
    #[test]
    fn a_key_already_declined_is_never_declined_a_second_time() {
        let held = |lines: Vec<Line>| Journal::from_lines(lines);
        let decide_now = |journal: &Journal, read: &ResolvedRead| {
            decide(
                read,
                journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                1_000,
                Limits::default(),
            )
        };
        let repeat = resolved("src/lib.rs", 0x3f, 1, 50, 50);
        let narrower = resolved("src/lib.rs", 0x3f, 1, 40, 50);
        let mut lines = vec![read_line(1, "src/lib.rs", 0x3f, 1, 50)];

        // The first repeat is what a decline is for.
        let first = decide_now(&held(lines.clone()), &repeat);
        assert!(
            matches!(first, Decision::Deny(_)),
            "the repeat of a range the window holds is declined: {first:?}"
        );

        // The line that decision leaves behind is the whole mechanism: from
        // here on the same key is answered by the guard, not by coverage.
        lines.push(record(&repeat, &first, 1_000, Limits::default(), 2, 2));
        for again in 1..=3 {
            assert_eq!(
                decide_now(&held(lines.clone()), &repeat),
                Decision::Allow,
                "call {again} after the decline runs; the window is never held \
                 in a loop of its own answer"
            );
        }

        // And the guard is one key wide, not one window wide.
        let other = decide_now(&held(lines.clone()), &narrower);
        assert!(
            matches!(other, Decision::Deny(_)),
            "a different request over the same coverage is its own key: {other:?}"
        );
    }

    /// **What the credit is worth, and the ceiling it does not move.**
    ///
    /// Coverage credited from a statement is coverage like any other: a later
    /// read wholly inside it is declinable under the ordinary rule. What one
    /// decline may cover is still [`Limits::max_slice_bytes`], which is a
    /// separate number about a separate thing — the size at which what a
    /// window kept of an earlier delivery may be a page rather than the range.
    #[test]
    fn credited_coverage_answers_a_later_read_up_to_the_decline_ceiling() {
        let read = whole_file(900);
        let limits = Limits::default();
        let Reconcile::Credit(range) =
            reconcile_delivery(&read, run(1, 1), StatedDelivery::Complete(run(1, 901)))
        else {
            panic!("the statement credits the file");
        };
        let journal = Journal::from_lines(vec![Line::Read {
            turn: 1,
            path: read.path.clone(),
            hash: read.hash,
            range,
            total_lines: 900,
            at_ms: 10,
        }]);
        let inside = resolved("src/lib.rs", 3, 100, 200, 900);
        let declining = |slice_bytes| {
            decide(
                &inside,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                slice_bytes,
                limits,
            )
        };
        assert!(
            matches!(declining(4_000), Decision::Deny(_)),
            "a range inside credited coverage is a range the window holds"
        );
        assert_eq!(
            declining(limits.max_slice_bytes + 1),
            Decision::Allow,
            "and the ceiling on what one decline may cover is unmoved"
        );
    }

    #[test]
    fn a_request_inside_a_single_edit_site_is_allowed() {
        let journal = Journal::from_lines(vec![
            read_line(1, "src/lib.rs", 0x3f, 1, 900),
            Line::Edit {
                turn: 2,
                path: "src/lib.rs".to_owned(),
                hash_old: Some(hash(0x3f)),
                hash_new: hash(0x40),
                sites: vec![run(37, 44)],
                shift: 8,
                full: true,
                at_ms: 2,
            },
        ]);
        let limits = Limits::default();

        let inside = resolved("src/lib.rs", 0x40, 38, 42, 908);
        assert_eq!(
            decide(
                &inside,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                64,
                limits
            ),
            Decision::Allow,
            "reading exactly what changed is the right thing to do"
        );

        let whole = resolved("src/lib.rs", 0x40, 1, 908, 908);
        assert_eq!(
            decide(
                &whole,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                64,
                limits
            ),
            Decision::Deny(DenyShape::Delta {
                sites: vec![run(37, 44)],
                shift: 8,
                window: run(34, 47),
            }),
            "and re-reading the file to check it is answered with where it changed"
        );
    }

    #[test]
    fn a_file_written_in_full_is_declined_without_naming_every_line_as_changed() {
        let journal = Journal::from_lines(vec![Line::Edit {
            turn: 1,
            path: "src/new.rs".to_owned(),
            hash_old: None,
            hash_new: hash(0x11),
            sites: vec![run(1, 40)],
            shift: 40,
            full: true,
            at_ms: 1,
        }]);
        let read = resolved("src/new.rs", 0x11, 1, 40, 40);
        assert_eq!(
            decide(
                &read,
                &journal,
                Mode::Enforce,
                Some(CONFIRMED),
                EditEcho::Pinned,
                64,
                Limits::default()
            ),
            Decision::Deny(DenyShape::Plain)
        );
    }

    /// The edit-echo gate is a fact about a *family*, not a stand-down for
    /// everyone: on one journal and one read, both shapes an edit line can
    /// produce are still produced where the echo is pinned, and both are
    /// allowed where it is not.
    #[test]
    fn the_same_edit_is_decided_for_a_pinned_family_and_allowed_for_an_unpinned_one() {
        let edited = Journal::from_lines(vec![
            read_line(1, "src/lib.rs", 0x3f, 1, 900),
            Line::Edit {
                turn: 2,
                path: "src/lib.rs".to_owned(),
                hash_old: Some(hash(0x3f)),
                hash_new: hash(0x40),
                sites: vec![run(37, 44)],
                shift: 8,
                full: true,
                at_ms: 2,
            },
        ]);
        let written = Journal::from_lines(vec![Line::Edit {
            turn: 1,
            path: "src/new.rs".to_owned(),
            hash_old: None,
            hash_new: hash(0x11),
            sites: vec![run(1, 40)],
            shift: 40,
            full: true,
            at_ms: 1,
        }]);
        let verification = resolved("src/lib.rs", 0x40, 1, 908, 908);
        let read_back = resolved("src/new.rs", 0x11, 1, 40, 40);
        let at = |read: &ResolvedRead, journal: &Journal, echo| {
            decide(
                read,
                journal,
                Mode::Enforce,
                Some(CONFIRMED),
                echo,
                64,
                Limits::default(),
            )
        };

        assert_eq!(
            at(&verification, &edited, EditEcho::Pinned),
            Decision::Deny(DenyShape::Delta {
                sites: vec![run(37, 44)],
                shift: 8,
                window: run(34, 47),
            }),
        );
        assert_eq!(
            at(&verification, &edited, EditEcho::Unpinned),
            Decision::Allow,
            "the same verification read, on a family whose edit result says nothing"
        );

        assert_eq!(
            at(&read_back, &written, EditEcho::Pinned),
            Decision::Deny(DenyShape::Plain),
        );
        assert_eq!(
            at(&read_back, &written, EditEcho::Unpinned),
            Decision::Allow,
            "a whole-file write is still an edit line, and the gate covers it too"
        );
    }

    proptest::proptest! {
        /// ★ Coverage is exact at its edge: a request for one line more than
        /// was handed over is a request for a line the window does not hold,
        /// and it runs.
        #[test]
        fn a_request_one_line_past_coverage_is_allowed(
            lo in 1u32..=500,
            span in 0u32..=200,
            total_extra in 1u32..=100,
        ) {
            let hi = lo + span;
            let total_lines = hi + total_extra;
            let journal = Journal::from_lines(vec![Line::Read {
                turn: 1,
                path: "src/lib.rs".to_owned(),
                hash: hash(0x3f),
                range: run(lo, hi),
                total_lines,
                at_ms: 1,
            }]);
            // The floor is a different property, tested on its own below;
            // held out of the way here so a short generated span exercises
            // coverage rather than the trade.
            let limits = Limits { min_slice_lines: 1, ..Limits::default() };

            let exact = resolved("src/lib.rs", 0x3f, lo, hi, total_lines);
            proptest::prop_assert_eq!(
                decide(&exact, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, 64, limits),
                Decision::Deny(DenyShape::Plain),
                "exactly what was handed over is held"
            );

            let one_past = resolved("src/lib.rs", 0x3f, lo, hi + 1, total_lines);
            proptest::prop_assert_eq!(
                decide(&one_past, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, 64, limits),
                Decision::Allow,
                "one line further is not"
            );

            if lo > 1 {
                let one_before = resolved("src/lib.rs", 0x3f, lo - 1, hi, total_lines);
                proptest::prop_assert_eq!(
                    decide(&one_before, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, 64, limits),
                    Decision::Allow,
                    "nor is one line earlier"
                );
            }
        }

        /// ★ A slice larger than a client keeps inline is never declined: what
        /// the window holds of the earlier delivery may be a pointer to a file
        /// rather than the bytes themselves.
        #[test]
        fn a_slice_over_the_cap_never_denies(
            over in proptest::prop_oneof![
                // The boundary itself, drawn on purpose: a range sampled
                // uniformly from a million values reaches its own first value
                // about never, and the byte that matters is the first one.
                proptest::strategy::Just(1usize),
                proptest::strategy::Just(2usize),
                3usize..=1_000_000,
            ],
            cap in 1usize..=64_000,
        ) {
            let journal = Journal::from_lines(vec![read_line(1, "src/lib.rs", 0x3f, 1, 900)]);
            let limits = Limits { max_slice_bytes: cap, min_slice_lines: 1, ..Limits::default() };
            let read = resolved("src/lib.rs", 0x3f, 1, 900, 900);
            let at = |slice: usize| decide(
                &read, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, slice, limits
            );

            proptest::prop_assert_eq!(
                at(cap + over),
                Decision::Allow,
                "over the cap, nothing is decided"
            );
            proptest::prop_assert_eq!(
                at(cap + 1),
                Decision::Allow,
                "one byte over the cap is over the cap"
            );
            proptest::prop_assert_eq!(
                at(cap),
                Decision::Deny(DenyShape::Plain),
                "at the cap the decision is the ordinary one"
            );
        }

        /// ★ A slice shorter than the floor is observed and never declined —
        /// and, unlike the byte ceiling, it is still *decided*.
        ///
        /// The two conservative bounds differ on purpose. Past the byte
        /// ceiling nothing is known: what the window kept of the earlier
        /// delivery may be a page rather than the range, so there is no
        /// decision to make and the read is simply allowed. Under the line
        /// floor everything is known — the window holds the range, the repeat
        /// is covered — and only the trade is bad, so the decision is made,
        /// recorded as an observation, and the call runs. That is what lets
        /// the observe window price the pool the floor forgoes instead of
        /// arguing about where to put it.
        #[test]
        fn a_slice_under_the_line_floor_is_observed_never_denied(
            floor in 2u32..=400,
            under in 1u32..=200,
        ) {
            let span = |lines: u32| {
                let journal = Journal::from_lines(vec![read_line(1, "src/lib.rs", 0x3f, 1, 900)]);
                let read = resolved("src/lib.rs", 0x3f, 1, lines, 900);
                let limits = Limits {
                    min_slice_lines: floor,
                    ..Limits::default()
                };
                decide(
                    &read, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, 64, limits
                )
            };

            proptest::prop_assert_eq!(
                span(floor.saturating_sub(under).max(1)),
                Decision::Observe(DenyShape::Plain),
                "under the floor the decision is reached and recorded, never acted on"
            );
            proptest::prop_assert_eq!(
                span(floor - 1),
                Decision::Observe(DenyShape::Plain),
                "one line under the floor is under the floor"
            );
            proptest::prop_assert_eq!(
                span(floor),
                Decision::Deny(DenyShape::Plain),
                "at the floor the decision is the ordinary one"
            );
            proptest::prop_assert_eq!(
                span(floor + 1),
                Decision::Deny(DenyShape::Plain),
                "and above it"
            );
        }

        /// ★ Observing is not enforcing. Every read that enforcing would
        /// decline is recorded and allowed to run, and the row it leaves says
        /// so — the one switch that keeps a measured lever from acting on
        /// anyone's session before the numbers are in.
        #[test]
        fn observe_records_and_never_denies(
            lo in 1u32..=800,
            span in 0u32..=99,
            edited in proptest::bool::ANY,
            slice in 0usize..=4_000,
        ) {
            let mut lines = vec![read_line(1, "src/lib.rs", 0x3f, 1, 900)];
            if edited {
                lines.push(Line::Edit {
                    turn: 2,
                    path: "src/lib.rs".to_owned(),
                    hash_old: Some(hash(0x3f)),
                    hash_new: hash(0x40),
                    sites: vec![run(37, 44)],
                    shift: 8,
                    full: true,
                    at_ms: 2,
                });
            }
            let journal = Journal::from_lines(lines);
            let limits = Limits::default();
            let seed = if edited { 0x40 } else { 0x3f };
            let read = resolved("src/lib.rs", seed, lo, lo + span, 900);

            let enforcing = decide(&read, &journal, Mode::Enforce, Some(CONFIRMED), EditEcho::Pinned, slice, limits);
            let observing = decide(&read, &journal, Mode::Observe, Some(CONFIRMED), EditEcho::Pinned, slice, limits);

            proptest::prop_assert!(
                !matches!(observing, Decision::Deny(_)),
                "observing never declines: {:?}",
                observing
            );
            match enforcing {
                Decision::Deny(shape) => proptest::prop_assert_eq!(
                    observing.clone(),
                    Decision::Observe(shape),
                    "what enforcing would decline, observing records in the same shape"
                ),
                other => proptest::prop_assert_eq!(
                    observing.clone(),
                    other,
                    "and everything else is decided identically"
                ),
            }

            // The row it leaves says it was observed, not declined.
            if let Decision::Observe(_) = observing {
                let line = record(&read, &observing, 0, Limits::default(), 9, 99);
                proptest::prop_assert_eq!(line.kind(), "observe");
            }
        }

        /// ★ With the lever off, nothing is ever declined, whatever the
        /// journal says and whatever the family's channel offers.
        #[test]
        fn off_never_denies(
            lo in 1u32..=800,
            span in 0u32..=99,
            slice in 0usize..=100_000,
            edited in proptest::bool::ANY,
        ) {
            let mut lines = vec![read_line(1, "src/lib.rs", 0x3f, 1, 900)];
            if edited {
                lines.push(Line::Edit {
                    turn: 2,
                    path: "src/lib.rs".to_owned(),
                    hash_old: Some(hash(0x3f)),
                    hash_new: hash(0x40),
                    sites: vec![run(37, 44)],
                    shift: 0,
                    full: true,
                    at_ms: 2,
                });
            }
            let journal = Journal::from_lines(lines);
            let seed = if edited { 0x40 } else { 0x3f };
            let read = resolved("src/lib.rs", seed, lo, lo + span, 900);

            proptest::prop_assert_eq!(
                decide(&read, &journal, Mode::Off, Some(CONFIRMED), EditEcho::Pinned, slice, Limits::default()),
                Decision::Allow
            );
        }

        /// ★ A family whose explanation has not been seen reaching a model is
        /// never declined on: an unexplained tool failure is a thing a model is
        /// right to retry, and a retry is the cost the whole design avoids.
        #[test]
        fn an_unpinned_channel_never_denies(
            lo in 1u32..=800,
            span in 0u32..=99,
            channel_pick in 0usize..4,
        ) {
            let channels = [
                None,
                Some(UNCONFIRMED),
                Some(Channel::Neutral),
                Some(Channel::AdditionalContext { limit_field: None }),
            ];
            let journal = Journal::from_lines(vec![read_line(1, "src/lib.rs", 0x3f, 1, 900)]);
            let read = resolved("src/lib.rs", 0x3f, lo, lo + span, 900);

            let decision = decide(
                &read,
                &journal,
                Mode::Enforce,
                channels[channel_pick % channels.len()],
                EditEcho::Pinned,
                64,
                Limits::default(),
            );
            proptest::prop_assert_eq!(
                decision,
                Decision::Observe(DenyShape::Plain),
                "recorded, never acted on"
            );
        }

        /// ★ A family whose edit result is not pinned to tell the model it
        /// holds what it just wrote is never declined on the strength of that
        /// edit — not with the changed sites, not plainly, and not even as a
        /// recorded `observe`, whose shape is what a later flip would act on.
        /// The read runs and is recorded as a read, which is the only sight of
        /// its own edit such an agent has.
        #[test]
        fn a_family_whose_edit_echo_is_not_pinned_is_never_declined_on_its_own_edit(
            lo in 1u32..=800,
            span in 0u32..=99,
            site_lo in 1u32..=800,
            site_span in 0u32..=99,
            shift in -50i64..=50,
            whole_file in proptest::bool::ANY,
            enforcing in proptest::bool::ANY,
            slice in 0usize..=4_000,
        ) {
            const TOTAL: u32 = 900;
            // The window read the old revision in full and then wrote it, so
            // everything it holds of the new revision it holds by having
            // written it — the one situation the delta arm exists for.
            let sites = if whole_file {
                vec![run(1, TOTAL)]
            } else {
                vec![run(site_lo, (site_lo + site_span).min(TOTAL))]
            };
            let journal = Journal::from_lines(vec![
                read_line(1, "src/lib.rs", 0x3f, 1, TOTAL),
                Line::Edit {
                    turn: 2,
                    path: "src/lib.rs".to_owned(),
                    hash_old: Some(hash(0x3f)),
                    hash_new: hash(0x40),
                    sites,
                    shift,
                    full: true,
                    at_ms: 2,
                },
            ]);
            let read = resolved("src/lib.rs", 0x40, lo, lo + span, TOTAL);
            let mode = if enforcing { Mode::Enforce } else { Mode::Observe };

            let decision = decide(
                &read,
                &journal,
                mode,
                Some(CONFIRMED),
                EditEcho::Unpinned,
                slice,
                Limits::default(),
            );
            proptest::prop_assert_eq!(
                decision.clone(),
                Decision::Allow,
                "an unpinned edit echo decides nothing, in any mode"
            );
            proptest::prop_assert_eq!(
                    record(&read, &decision, 0, Limits::default(), 3, 3).kind(),
                    "read"
                );
        }

        /// ★ However reads, declines and losses of context interleave, one
        /// request is declined at most once between two losses — the property
        /// that bounds a model answering a decline by asking again to a single
        /// extra exchange.
        #[test]
        fn at_most_one_deny_per_key_under_any_interleaving(
            steps in proptest::collection::vec(
                (0usize..=3, 1u32..=6, 0u32..=3, 0usize..=1),
                1..40,
            ),
        ) {
            const PATHS: [&str; 2] = ["src/lib.rs", "src/other.rs"];
            const TOTAL: u32 = 60;
            let limits = Limits::default();
            let mut journal = Journal::default();
            let mut epoch = 0usize;
            // A key is a path, a revision and a range: two revisions of one
            // file are two questions, and each gets its own single answer.
            let mut seen: Vec<(usize, String, u8, u32, u32)> = Vec::new();

            for (turn, (action, lo, span, which)) in steps.into_iter().enumerate() {
                let path = PATHS[which];
                if action == 3 {
                    journal.push(Line::Evict {
                        turn: turn as u32,
                        reason: EvictReason::Clear,
                        at_ms: turn as u64,
                    });
                    epoch += 1;
                    continue;
                }
                // Two revisions in play, so a decision sometimes rests on
                // coverage that a later step's hash change wipes out.
                let seed = if action == 2 { 0x40 } else { 0x3f };
                let read = resolved(path, seed, lo * 8, lo * 8 + span, TOTAL);
                let decision = decide(
                    &read,
                    &journal,
                    Mode::Enforce,
                    Some(CONFIRMED),
                    EditEcho::Pinned,
                    64,
                    limits,
                );
                if let Decision::Deny(_) = decision {
                    let key = (epoch, path.to_owned(), seed, read.range.lo, read.range.hi);
                    proptest::prop_assert!(
                        !seen.contains(&key),
                        "a second decline for {:?} in one context",
                        key
                    );
                    seen.push(key);
                }
                journal.push(record(
                    &read,
                    &decision,
                    0,
                    Limits::default(),
                    turn as u32,
                    turn as u64,
                ));
            }
        }
    }
}
