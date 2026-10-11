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

use super::line::{hash_at, range_at, text_at, u32_at};
use super::{Interval, IntervalSet, Journal, Line};

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
    use super::super::fixtures::{hash, read_line, run};
    use super::super::EvictReason;
    use super::*;

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
