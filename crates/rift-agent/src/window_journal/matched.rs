//! Lines a window was handed that no grammar named: the row that records
//! them, and the one place a decision may lean on them.
//!
//! A module of its own because this is the one source of coverage learned
//! from what a call *delivered* rather than from what it asked for. A `read`
//! line is a range a recognised command line bounds; a `matched` line is the
//! sparse set of a file's lines whose bytes were found, in order and in
//! place, in a terminal result. Below `enforce` the rows are inert to every
//! decision, so the pool they size can be measured for as long as it takes
//! without any session losing a byte to it.

use rift_core::Sha256Id;
use serde_json::{json, Map, Value};

use super::line::{hash_at, text_at, u32_at};
use super::{Interval, IntervalSet, Journal, Limits, Line, Mode};

/// The most runs one matched row names.
///
/// A row is one atomic append, so a delivery matched in more runs than this
/// is written as several rows rather than cut: what is dropped by a cap is a
/// saving, and what is cut silently is a claim nobody can audit.
pub const MAX_MATCHED_RUNS: usize = 64;

/// The basis a matched row names: the lines were matched in the output.
///
/// Written into every row, and required on read, so a later basis for the
/// same kind is a row this version drops rather than one it misreads.
pub const OUTPUT_MATCHED: &str = "output_matched";

/// How far a decision may lean on matched lines when no policy says.
pub const MATCHED_MODE: Mode = Mode::Observe;

impl Journal {
    /// The lines of `(path, hash)` this window was shown in terminal output
    /// and still holds, from matched rows alone.
    #[must_use]
    pub fn matched_coverage(&self, path: &str, hash: Sha256Id) -> IntervalSet {
        let mut set = IntervalSet::new();
        for line in self.live() {
            let Line::Matched {
                path: p,
                hash: h,
                runs,
                ..
            } = line
            else {
                continue;
            };
            if p == path && *h == hash {
                for run in runs {
                    set.insert(*run);
                }
            }
        }
        set
    }

    /// This journal with every matched run standing as the read it proves,
    /// or `None` when there is none — so a window with nothing matched is
    /// decided over its own lines without a copy.
    fn admitting_matched(&self) -> Option<Self> {
        if !self
            .lines
            .iter()
            .any(|line| matches!(line, Line::Matched { .. }))
        {
            return None;
        }
        let mut lines = Vec::with_capacity(self.lines.len());
        for line in &self.lines {
            match line {
                Line::Matched {
                    turn,
                    path,
                    hash,
                    runs,
                    total_lines,
                    at_ms,
                    ..
                } => lines.extend(runs.iter().map(|run| Line::Read {
                    turn: *turn,
                    path: path.clone(),
                    hash: *hash,
                    range: *run,
                    total_lines: *total_lines,
                    at_ms: *at_ms,
                })),
                other => lines.push(other.clone()),
            }
        }
        Some(Self::from_lines(lines))
    }
}

/// The journal a decision is taken over once matched lines are weighed: the
/// view with every matched run standing as a read while the lever is at
/// `enforce`, and `None` — no copy — below it.
pub(super) fn matched_view(journal: &Journal, limits: Limits) -> Option<Journal> {
    if limits.matched != Mode::Enforce {
        return None;
    }
    journal.admitting_matched()
}

/// The matched rows that record `runs` of one file, at most
/// [`MAX_MATCHED_RUNS`] runs to a row, in order; none for no runs.
#[must_use]
pub fn matched_lines(
    path: &str,
    hash: Sha256Id,
    runs: &[Interval],
    total_lines: u32,
    member: u32,
    turn: u32,
    at_ms: u64,
) -> Vec<Line> {
    runs.chunks(MAX_MATCHED_RUNS)
        .map(|chunk| Line::Matched {
            turn,
            path: path.to_owned(),
            hash,
            runs: chunk.to_vec(),
            total_lines,
            member,
            at_ms,
        })
        .collect()
}

/// The members past the shared header of a matched line.
pub(super) fn insert_matched_fields(line: &Line, object: &mut Map<String, Value>) {
    let Line::Matched {
        path,
        hash,
        runs,
        total_lines,
        member,
        at_ms,
        ..
    } = line
    else {
        return;
    };
    let rows: Vec<Value> = runs.iter().map(|run| json!([run.lo, run.hi])).collect();
    object.insert("p".to_owned(), json!(path));
    object.insert("h".to_owned(), json!(hash.to_hex()));
    object.insert("runs".to_owned(), Value::Array(rows));
    object.insert("n".to_owned(), json!(total_lines));
    object.insert("member".to_owned(), json!(member));
    object.insert("basis".to_owned(), json!(OUTPUT_MATCHED));
    object.insert("at".to_owned(), json!(at_ms));
}

/// The matched line a row states, or `None` for one this version did not
/// write: another basis, no runs, too many, a run that is not one, or runs
/// out of order.
pub(super) fn matched_line_from(
    object: &Map<String, Value>,
    turn: u32,
    at_ms: u64,
) -> Option<Line> {
    if text_at(object, "basis")? != OUTPUT_MATCHED {
        return None;
    }
    Some(Line::Matched {
        turn,
        path: text_at(object, "p")?.to_owned(),
        hash: hash_at(object, "h")?,
        runs: sorted_runs_at(object)?,
        total_lines: u32_at(object, "n")?,
        member: u32_at(object, "member")?,
        at_ms,
    })
}

/// The `runs` member: one to [`MAX_MATCHED_RUNS`] pairs, each a run, sorted
/// and disjoint. One entry that is not drops the line, because a run read
/// wrong names lines the window was never shown.
fn sorted_runs_at(object: &Map<String, Value>) -> Option<Vec<Interval>> {
    let entries = object.get("runs")?.as_array()?;
    if entries.is_empty() || entries.len() > MAX_MATCHED_RUNS {
        return None;
    }
    let mut runs: Vec<Interval> = Vec::with_capacity(entries.len());
    for entry in entries {
        let pair = entry.as_array()?;
        if pair.len() != 2 {
            return None;
        }
        let lo = u32::try_from(pair.first()?.as_u64()?).ok()?;
        let hi = u32::try_from(pair.get(1)?.as_u64()?).ok()?;
        let run = Interval::new(lo, hi)?;
        if runs.last().is_some_and(|last| last.hi >= run.lo) {
            return None;
        }
        runs.push(run);
    }
    Some(runs)
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::{hash, read_line, resolved, run};
    use super::super::{decide, Decision, EditEcho};
    use super::*;

    fn matched(runs: &[(u32, u32)]) -> Line {
        Line::Matched {
            turn: 2,
            path: "src/lib.rs".to_owned(),
            hash: hash(3),
            runs: runs.iter().map(|(lo, hi)| run(*lo, *hi)).collect(),
            total_lines: 900,
            member: 1,
            at_ms: 5,
        }
    }

    /// ★ A matched row round-trips with its basis, and a row naming another
    /// basis, no runs, or runs out of order is dropped.
    #[test]
    fn a_matched_row_round_trips_and_a_malformed_one_is_dropped() {
        let line = matched(&[(1, 40), (90, 95)]);
        let row = line.to_json_line();
        assert!(row.contains(r#""basis":"output_matched""#), "{row}");
        assert_eq!(Line::from_json_line(&row), Some(line));
        let good: Value = serde_json::from_str(&row).unwrap();
        for (member, value) in [
            ("basis", json!("output_guessed")),
            ("runs", json!([])),
            ("runs", json!([[90, 95], [1, 40]])),
            ("runs", json!([[1, 40], [40, 50]])),
            ("runs", json!([[0, 4]])),
            ("member", json!(-1)),
        ] {
            let mut bad = good.clone();
            bad[member] = value.clone();
            assert_eq!(
                Line::from_json_line(&bad.to_string()),
                None,
                "accepted {member} = {value}"
            );
        }
    }

    /// A delivery matched in more runs than a row holds is written as
    /// several rows, none cut.
    #[test]
    fn many_runs_are_written_as_several_whole_rows() {
        let runs: Vec<Interval> = (0..130).map(|i| run(i * 3 + 1, i * 3 + 1)).collect();
        let lines = matched_lines("src/lib.rs", hash(3), &runs, 900, 0, 1, 2);
        assert_eq!(lines.len(), 3);
        let carried: usize = lines
            .iter()
            .map(|line| match line {
                Line::Matched { runs, .. } => runs.len(),
                _ => 0,
            })
            .sum();
        assert_eq!(carried, 130);
        assert!(matched_lines("src/lib.rs", hash(3), &[], 900, 0, 1, 2).is_empty());
    }

    /// ★ Matched lines decline nothing below `enforce`, and at `enforce` a
    /// re-read of lines the window was shown is a re-read like any other.
    #[test]
    fn matched_lines_ground_a_decline_only_at_enforce() {
        let journal = Journal::from_lines(vec![matched(&[(1, 40)])]);
        let read = resolved("src/lib.rs", 3, 1, 40, 900);
        let ask = |mode: Mode| {
            let limits = Limits {
                matched: mode,
                min_slice_lines: 1,
                ..Limits::default()
            };
            decide(
                &read,
                &journal,
                Mode::Observe,
                None,
                EditEcho::Pinned,
                400,
                limits,
            )
        };
        assert_eq!(ask(Mode::Observe), Decision::Allow);
        assert_eq!(ask(Mode::Off), Decision::Allow);
        assert!(
            matches!(ask(Mode::Enforce), Decision::Observe(_)),
            "{:?}",
            ask(Mode::Enforce)
        );
        assert_eq!(
            journal.matched_coverage("src/lib.rs", hash(3)).runs(),
            [run(1, 40)]
        );
        assert!(journal.coverage("src/lib.rs", hash(3)).is_empty());
    }

    /// An eviction empties matched coverage as it empties reads.
    #[test]
    fn an_eviction_empties_matched_coverage() {
        let journal = Journal::from_lines(vec![
            read_line(1, "src/other.rs", 3, 1, 4),
            matched(&[(1, 40)]),
            Line::Evict {
                turn: 3,
                reason: super::super::EvictReason::Clear,
                at_ms: 6,
            },
        ]);
        assert!(journal.matched_coverage("src/lib.rs", hash(3)).is_empty());
    }
}
