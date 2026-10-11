//! A read another window of the family already holds: the row that records
//! the one decline it may take, and the union of what the other windows hold.
//!
//! A module of its own because it is the one decision whose ground is not this
//! window's own journal. Every other line here is a fact about the window it is
//! written in; a family line records that the window was answered from what
//! *other* live windows of the repository hold at the current revision — with
//! the cheaper route that answer carried, so the window can still reach the
//! bytes. Which windows are live, and where their journals are, is the
//! binary's question: it needs files. This half sees journals already loaded.

use rift_core::Sha256Id;
use serde_json::{json, Map, Value};

use super::decision::act_on;
use super::line::{hash_at, range_at, text_at, u32_at};
use super::{Decision, DenyShape, Interval, IntervalSet, Journal, Line, Mode, ResolvedRead};
use crate::respond::Channel;

/// The most runs a family line names as the cheaper route: the reads it
/// admits as follow-ups.
///
/// The frame that offers them names at most this many, and a row is one
/// atomic append, so a longer list is refused rather than cut.
pub const MAX_FAMILY_RUNS: usize = 16;

/// What a family decline handed the window instead of the lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FamilyRoute {
    /// The file's bound outline for these exact bytes, with the definitions'
    /// lines as the reads to make instead.
    Outline,
    /// No outline is bound yet: the runs the other windows hold, each inside
    /// the request, named as the reads to make instead.
    Held,
}

impl FamilyRoute {
    /// Every route, in the order a surface lists them.
    pub const ALL: [Self; 2] = [Self::Outline, Self::Held];

    /// The token this route is written as.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Outline => "outline",
            Self::Held => "held",
        }
    }

    /// The route a token names, or `None` for one this version does not know.
    #[must_use]
    pub fn from_str(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|route| route.as_str() == token)
    }

    /// The neutral tag an accounting row about a decline on this route
    /// carries, and the kind its frozen row is written as.
    #[must_use]
    pub const fn audit_tag(self) -> &'static str {
        match self {
            Self::Outline => "family_held:outline",
            Self::Held => "family_held:held",
        }
    }
}

impl Journal {
    /// The family decline this window took on `(path, hash)` since it last
    /// lost what it held, if it took one.
    ///
    /// The key is the revision, not the request: one decline per file and
    /// bytes, and every later read of them in the window is admitted — the
    /// follow-up the decline offered, or any other read, charged in full.
    #[must_use]
    pub fn family_decline(&self, path: &str, hash: Sha256Id) -> Option<&Line> {
        self.live().iter().find(
            |line| matches!(line, Line::Family { path: p, hash: h, .. } if p == path && *h == hash),
        )
    }
}

/// Whether a read is one a family decline may answer, before anything about
/// another window is read: the cheap gates first, so an ordinary read pays
/// nothing for the question.
///
/// Every gate is a reason **not** to decline:
///
/// * the lever is off;
/// * the read does not start at line one — a mid-file range names the lines
///   it wants, and no outline or list of runs is cheaper than those lines;
/// * it is a first page shorter than `min_page_lines`, the frame's own
///   break-even;
/// * the window holds some of this revision, so the read is decided on its
///   own lines and never on another window's;
/// * the window already took its one family decline on this revision — every
///   later read of it is admitted.
#[must_use]
pub fn is_family_candidate(
    read: &ResolvedRead,
    journal: &Journal,
    mode: Mode,
    min_page_lines: u32,
) -> bool {
    if mode == Mode::Off || read.range.lo != 1 {
        return false;
    }
    let whole = read.range.hi >= read.total_lines;
    if !whole && read.range.lines() < min_page_lines {
        return false;
    }
    journal.coverage(&read.path, read.hash).is_empty()
        && journal.family_decline(&read.path, read.hash).is_none()
}

/// The runs a held-lines frame offers: what the other windows hold of the
/// request, cut to it — or `None` when they hold none of it, hold all of it
/// (the cheaper read would be the read itself), or hold it in more runs than
/// a frame names.
#[must_use]
pub fn held_runs(read: &ResolvedRead, holders: &IntervalSet) -> Option<Vec<Interval>> {
    let runs: Vec<Interval> = holders
        .runs()
        .iter()
        .filter_map(|run| run.intersection(read.range))
        .collect();
    if runs.is_empty() || runs.len() > MAX_FAMILY_RUNS || holders.contains(read.range) {
        return None;
    }
    Some(runs)
}

/// What to do about a family candidate once its route is known: decline it
/// once, in this shape, where the mode and the family's channel allow, and
/// record it otherwise.
///
/// The route is never empty-handed: the caller reaches here only with an
/// outline bound for these bytes or with runs to offer, so no decline this
/// returns is a bare refusal. `frame_bytes` is the frame's size, sent or not.
#[must_use]
pub fn decide_family_held(
    route: FamilyRoute,
    offered: Vec<Interval>,
    frame_bytes: u32,
    mode: Mode,
    channel: Option<Channel>,
) -> Decision {
    if mode == Mode::Off || offered.is_empty() {
        return Decision::Allow;
    }
    let shape = DenyShape::Family {
        route,
        offered,
        frame_bytes,
    };
    act_on(shape, mode, true, channel)
}

/// The family line an observed family decline leaves beside the read it
/// records, or `None` for every other decision. An answered one leaves its
/// family line as its own record.
#[must_use]
pub fn record_family(
    read: &ResolvedRead,
    decision: &Decision,
    turn: u32,
    at_ms: u64,
) -> Option<Line> {
    let Decision::Observe(DenyShape::Family {
        route,
        offered,
        frame_bytes,
    }) = decision
    else {
        return None;
    };
    Some(family_line(
        read,
        *route,
        offered,
        *frame_bytes,
        false,
        turn,
        at_ms,
    ))
}

/// The line a family decline is recorded as.
pub(super) fn family_line(
    read: &ResolvedRead,
    route: FamilyRoute,
    offered: &[Interval],
    frame_bytes: u32,
    acted: bool,
    turn: u32,
    at_ms: u64,
) -> Line {
    Line::Family {
        turn,
        path: read.path.clone(),
        hash: read.hash,
        range: read.range,
        route,
        offered: offered.to_vec(),
        acted,
        frame_bytes,
        at_ms,
    }
}

/// The lines of a read that ran which a family decline of this window had
/// already been credited with, or `None` when there was no such decline.
///
/// Every read of the declined revision after the decline is admitted, and
/// each is charged against it — inside the offered runs, the follow-up it
/// promised; outside them, a read the decline did not spare after all. Only
/// a decline that was made is charged: an observed one spared nothing.
#[must_use]
pub fn family_follow_up(read: &ResolvedRead, journal: &Journal) -> Option<Interval> {
    let Some(Line::Family {
        range, acted: true, ..
    }) = journal.family_decline(&read.path, read.hash)
    else {
        return None;
    };
    read.range.intersection(*range)
}

/// The lines of `(path, hash)` the given windows hold, together.
///
/// Each window's own coverage — what it was handed or wrote since it last lost
/// what it held — and nothing it inherited or was told: a decline resting on
/// another window must rest on bytes that window really has.
#[must_use]
pub fn family_coverage<'a>(
    siblings: impl IntoIterator<Item = &'a Journal>,
    path: &str,
    hash: Sha256Id,
) -> IntervalSet {
    let mut set = IntervalSet::new();
    for journal in siblings {
        for run in journal.coverage(path, hash).runs() {
            set.insert(*run);
        }
    }
    set
}

/// The members past the shared header of a family line.
pub(super) fn insert_family_fields(line: &Line, object: &mut Map<String, Value>) {
    let Line::Family {
        path,
        hash,
        range,
        route,
        offered,
        acted,
        frame_bytes,
        at_ms,
        ..
    } = line
    else {
        return;
    };
    let runs: Vec<Value> = offered.iter().map(|run| json!([run.lo, run.hi])).collect();
    object.insert("p".to_owned(), json!(path));
    object.insert("h".to_owned(), json!(hash.to_hex()));
    object.insert("lo".to_owned(), json!(range.lo));
    object.insert("hi".to_owned(), json!(range.hi));
    object.insert("route".to_owned(), json!(route.as_str()));
    object.insert("runs".to_owned(), Value::Array(runs));
    object.insert("acted".to_owned(), json!(acted));
    object.insert("frame_bytes".to_owned(), json!(frame_bytes));
    object.insert("at".to_owned(), json!(at_ms));
}

/// The family line a row states, or `None` for one this version did not
/// write: an unknown route, a run that is not one, too many runs.
pub(super) fn family_line_from(object: &Map<String, Value>, turn: u32, at_ms: u64) -> Option<Line> {
    Some(Line::Family {
        turn,
        path: text_at(object, "p")?.to_owned(),
        hash: hash_at(object, "h")?,
        range: range_at(object)?,
        route: FamilyRoute::from_str(text_at(object, "route")?)?,
        offered: runs_at(object)?,
        acted: object.get("acted")?.as_bool()?,
        frame_bytes: u32_at(object, "frame_bytes")?,
        at_ms,
    })
}

/// The `runs` member: at most [`MAX_FAMILY_RUNS`] pairs, each a run. One
/// entry that is not drops the line, because the runs are what a later read
/// is admitted on.
fn runs_at(object: &Map<String, Value>) -> Option<Vec<Interval>> {
    let entries = object.get("runs")?.as_array()?;
    if entries.len() > MAX_FAMILY_RUNS {
        return None;
    }
    entries
        .iter()
        .map(|entry| {
            let pair = entry.as_array()?;
            if pair.len() != 2 {
                return None;
            }
            let lo = u32::try_from(pair.first()?.as_u64()?).ok()?;
            let hi = u32::try_from(pair.get(1)?.as_u64()?).ok()?;
            Interval::new(lo, hi)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{hash, read_line, resolved, run, whole_file};
    use super::super::{record, EvictReason, Limits};
    use super::*;

    const CONFIRMED: Option<Channel> = Some(Channel::DenyWithReason {
        reason_field: "permissionDecisionReason",
        confirmed: true,
    });

    fn holders(runs: &[(u32, u32)]) -> IntervalSet {
        let mut set = IntervalSet::new();
        for (lo, hi) in runs {
            set.insert(run(*lo, *hi));
        }
        set
    }

    /// Only a whole-file or a long-enough first-page read of a revision the
    /// window holds nothing of, and has not been family-declined on, is a
    /// candidate; a mid-file range runs.
    #[test]
    fn only_a_first_page_or_whole_read_of_an_unheld_revision_is_a_candidate() {
        let empty = Journal::default();
        let whole = whole_file(900);
        assert!(is_family_candidate(&whole, &empty, Mode::Observe, 95));
        assert!(is_family_candidate(
            &resolved("src/lib.rs", 3, 1, 95, 900),
            &empty,
            Mode::Enforce,
            95
        ));
        for (read, mode) in [
            (whole.clone(), Mode::Off),
            (resolved("src/lib.rs", 3, 2, 900, 900), Mode::Observe),
            (resolved("src/lib.rs", 3, 1, 94, 900), Mode::Observe),
        ] {
            assert!(!is_family_candidate(&read, &empty, mode, 95), "{read:?}");
        }
        let held = Journal::from_lines(vec![read_line(1, "src/lib.rs", 3, 400, 410)]);
        assert!(!is_family_candidate(&whole, &held, Mode::Observe, 95));
        let declined = Journal::from_lines(vec![record(
            &whole,
            &decide_family_held(
                FamilyRoute::Held,
                vec![run(1, 9)],
                90,
                Mode::Enforce,
                CONFIRMED,
            ),
            0,
            Limits::default(),
            1,
            2,
        )]);
        assert!(!is_family_candidate(&whole, &declined, Mode::Enforce, 95));
    }

    /// The held runs are what the other windows hold of the request, cut to
    /// it; holding none or all of it, or too many runs, offers nothing.
    #[test]
    fn held_runs_are_the_holders_runs_inside_the_request() {
        let page = resolved("src/lib.rs", 3, 1, 300, 900);
        assert_eq!(
            held_runs(&page, &holders(&[(10, 20), (250, 400), (700, 800)])),
            Some(vec![run(10, 20), run(250, 300)])
        );
        assert_eq!(held_runs(&page, &holders(&[(500, 600)])), None);
        assert_eq!(held_runs(&page, &holders(&[(1, 900)])), None);
        let many: Vec<(u32, u32)> = (0..17).map(|n| (n * 10 + 1, n * 10 + 2)).collect();
        assert_eq!(held_runs(&page, &holders(&many)), None);
    }

    /// ★ An answered family decline leaves its family line as the record and
    /// no coverage; an observed one leaves the read that ran and the family
    /// line beside it, marked not acted.
    #[test]
    fn an_answered_decline_records_the_family_line_and_an_observed_one_the_read_too() {
        let whole = whole_file(900);
        let answered = decide_family_held(
            FamilyRoute::Outline,
            vec![run(1, 40)],
            1_200,
            Mode::Enforce,
            CONFIRMED,
        );
        let line = record(&whole, &answered, 40_000, Limits::default(), 2, 9);
        assert!(matches!(line, Line::Family { acted: true, .. }), "{line:?}");
        assert_eq!(record_family(&whole, &answered, 2, 9), None);
        let journal = Journal::from_lines(vec![line]);
        assert!(journal.coverage("src/lib.rs", hash(3)).is_empty());

        let observed = decide_family_held(
            FamilyRoute::Outline,
            vec![run(1, 40)],
            1_200,
            Mode::Observe,
            CONFIRMED,
        );
        assert_eq!(
            record(&whole, &observed, 40_000, Limits::default(), 2, 9).kind(),
            "read"
        );
        let beside = record_family(&whole, &observed, 2, 9).unwrap();
        assert!(matches!(
            beside,
            Line::Family {
                acted: false,
                frame_bytes: 1_200,
                ..
            }
        ));
    }

    /// After an answered decline every later read of the revision is charged
    /// against it, inside or outside the offered runs; after an observed one
    /// nothing is.
    #[test]
    fn every_later_read_is_charged_against_an_answered_decline_only() {
        let whole = whole_file(900);
        let answered = Journal::from_lines(vec![record(
            &whole,
            &decide_family_held(
                FamilyRoute::Held,
                vec![run(10, 20)],
                90,
                Mode::Enforce,
                CONFIRMED,
            ),
            0,
            Limits::default(),
            1,
            2,
        )]);
        let inside = resolved("src/lib.rs", 3, 10, 20, 900);
        let outside = resolved("src/lib.rs", 3, 500, 520, 900);
        assert_eq!(family_follow_up(&inside, &answered), Some(run(10, 20)));
        assert_eq!(family_follow_up(&outside, &answered), Some(run(500, 520)));
        assert_eq!(family_follow_up(&whole, &answered), Some(run(1, 900)));
        let other = resolved("src/lib.rs", 4, 10, 20, 900);
        assert_eq!(family_follow_up(&other, &answered), None);
        let observed = Journal::from_lines(vec![record_family(
            &whole,
            &decide_family_held(
                FamilyRoute::Held,
                vec![run(10, 20)],
                90,
                Mode::Observe,
                CONFIRMED,
            ),
            1,
            2,
        )
        .unwrap()]);
        assert_eq!(family_follow_up(&inside, &observed), None);
    }

    proptest::proptest! {
        /// ★ While the lever is at most observe, no family decision is ever
        /// a decline, whatever the route, the runs or the channel.
        #[test]
        fn no_family_decline_is_ever_made_below_enforce(
            mode in proptest::sample::select(vec![Mode::Off, Mode::Observe]),
            outline in proptest::bool::ANY,
            runs in proptest::collection::vec((1u32..500, 0u32..50), 0..20),
            frame_bytes in 0u32..5_000,
            confirmed in proptest::bool::ANY,
        ) {
            let route = if outline { FamilyRoute::Outline } else { FamilyRoute::Held };
            let offered: Vec<Interval> =
                runs.iter().map(|(lo, len)| run(*lo, lo + len)).collect();
            let channel = Some(Channel::DenyWithReason {
                reason_field: "permissionDecisionReason",
                confirmed,
            });
            let decision = decide_family_held(route, offered, frame_bytes, mode, channel);
            proptest::prop_assert!(!matches!(decision, Decision::Deny(_)), "{decision:?}");
        }
    }

    fn family(route: FamilyRoute, offered: Vec<Interval>) -> Line {
        Line::Family {
            turn: 4,
            path: "src/lib.rs".to_owned(),
            hash: hash(3),
            range: run(1, 900),
            route,
            offered,
            acted: false,
            frame_bytes: 812,
            at_ms: 40,
        }
    }

    /// ★ A family line round-trips, and a binary that predates it drops it
    /// rather than reading it as some other kind.
    #[test]
    fn a_family_line_round_trips_and_is_its_own_kind() {
        for line in [
            family(FamilyRoute::Outline, vec![run(1, 40), run(120, 300)]),
            family(FamilyRoute::Held, vec![run(10, 20)]),
            family(FamilyRoute::Held, Vec::new()),
        ] {
            let row = line.to_json_line();
            assert_eq!(line.kind(), "family");
            assert_eq!(Line::from_json_line(&row), Some(line.clone()), "{row}");
        }
    }

    /// A row with an unknown route, a run that is not one, or more runs than
    /// a frame may offer is dropped whole.
    #[test]
    fn a_malformed_family_row_is_dropped() {
        let good = family(FamilyRoute::Held, vec![run(1, 2)]).to_json_line();
        let too_many: Vec<[u32; 2]> = (1..=17).map(|n| [n, n]).collect();
        for bad in [
            good.replace("\"held\"", "\"delta\""),
            good.replace("[[1,2]]", "[[2,1]]"),
            good.replace("[[1,2]]", "[[0,2]]"),
            good.replace("[[1,2]]", "[[1]]"),
            good.replace("[[1,2]]", &serde_json::to_string(&too_many).unwrap()),
            good.replace("\"acted\":false", "\"acted\":\"no\""),
        ] {
            assert_ne!(bad, good, "the planting changed the row");
            assert_eq!(Line::from_json_line(&bad), None, "{bad}");
        }
    }

    /// One decline per revision in a window, until the window loses what it
    /// held; another revision of the file is another key.
    #[test]
    fn a_family_decline_is_found_for_its_revision_until_an_emptying() {
        let mut journal = Journal::from_lines(vec![family(FamilyRoute::Outline, vec![])]);
        assert!(journal.family_decline("src/lib.rs", hash(3)).is_some());
        assert!(journal.family_decline("src/lib.rs", hash(4)).is_none());
        assert!(journal.family_decline("src/other.rs", hash(3)).is_none());
        journal.push(Line::Evict {
            turn: 5,
            reason: EvictReason::Compact,
            at_ms: 41,
        });
        assert!(journal.family_decline("src/lib.rs", hash(3)).is_none());
    }

    /// The family's coverage is the union of what each window holds now —
    /// not what one held before it lost it, and not another revision.
    #[test]
    fn family_coverage_is_the_union_of_what_each_window_holds_now() {
        let one = Journal::from_lines(vec![read_line(1, "src/lib.rs", 3, 1, 40)]);
        let two = Journal::from_lines(vec![
            read_line(1, "src/lib.rs", 3, 41, 90),
            read_line(1, "src/lib.rs", 4, 200, 300),
        ]);
        let emptied = Journal::from_lines(vec![
            read_line(1, "src/lib.rs", 3, 500, 600),
            Line::Evict {
                turn: 2,
                reason: EvictReason::Clear,
                at_ms: 3,
            },
        ]);
        let held = family_coverage([&one, &two, &emptied], "src/lib.rs", hash(3));
        assert_eq!(held.runs(), [run(1, 90)]);
        assert!(family_coverage([&one], "src/lib.rs", hash(9)).is_empty());
    }
}
