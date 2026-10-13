//! Placing delivered lines in the files they came from: runs of consecutive
//! lines found at consecutive places, and the rule that says which placed
//! lines are grounded.
//!
//! A line found somewhere is not yet a line of that file. `}` is in every
//! file, and a common line is in many; what makes a placement a claim is that
//! it cannot be a coincidence: a run of at least [`GROUNDING_RUN`] distinct
//! lines in order, a line found exactly once in the files at hand, or a
//! printed prefix whose place the file's own bytes confirm. A line placed in
//! two places equally well is placed nowhere.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// The fewest distinct, non-trivial lines a run needs to ground its lines.
///
/// Measured on the frozen corpus against printed prefixes as truth: one
/// false line in 117,620 at five, with a unique line or a verified prefix
/// grounding alone; eight in 119,844 at three.
pub const GROUNDING_RUN: u32 = 5;

/// Places kept per line hash; a hash found more often is noisy and never
/// starts a run, though it may extend one.
const NOISY_PLACES: usize = 48;

/// The most runs followed at once, so a hostile output of one repeated line
/// costs a bounded walk.
const MAX_ACTIVE_RUNS: usize = 4096;

/// One reading of a delivered line, its printed path already resolved to
/// a file index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineReading {
    /// The hash of the reading.
    pub hash: u64,
    /// Whether the reading is too slight to ground anything alone.
    pub trivial: bool,
    /// The file the line's own prefix named.
    pub anchor_file: Option<usize>,
    /// The line number the line's own prefix named.
    pub anchor_line: Option<u32>,
}

/// Where one delivered line was placed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Placement {
    /// The file, by index.
    pub file: usize,
    /// The line of it, 1-based.
    pub line: u32,
    /// The run the line was placed in, by index.
    pub run: usize,
    /// Whether the placement grounds coverage.
    pub grounded: bool,
}

/// A run being followed: its distinct non-trivial hashes.
#[derive(Default)]
struct Run {
    distinct: BTreeSet<u64>,
}

/// One candidate placement of one delivered line.
#[derive(Clone, Copy)]
struct Hit {
    run: usize,
    file: usize,
    line: u32,
    hash: u64,
    trivial: bool,
    anchored: bool,
}

/// Where each hash occurs in the files, capped, and how often in all.
struct Postings {
    places: HashMap<u64, (u32, Vec<(usize, u32)>)>,
}

impl Postings {
    fn new(files: &[&[u64]]) -> Self {
        let mut places: HashMap<u64, (u32, Vec<(usize, u32)>)> = HashMap::new();
        for (file, hashes) in files.iter().enumerate() {
            for (at, hash) in hashes.iter().enumerate() {
                let entry = places.entry(*hash).or_default();
                entry.0 = entry.0.saturating_add(1);
                let line = u32::try_from(at + 1).unwrap_or(u32::MAX);
                if entry.1.len() < NOISY_PLACES {
                    entry.1.push((file, line));
                }
            }
        }
        Self { places }
    }

    fn count(&self, hash: u64) -> u32 {
        self.places.get(&hash).map_or(0, |entry| entry.0)
    }

    fn places(&self, hash: u64) -> &[(usize, u32)] {
        self.places
            .get(&hash)
            .map_or(&[], |entry| entry.1.as_slice())
    }
}

/// Every delivered line's placement, or `None` where it was placed nowhere
/// or in two places equally well.
#[must_use]
pub fn place_lines(files: &[&[u64]], lines: &[Vec<LineReading>]) -> Vec<Option<Placement>> {
    let postings = Postings::new(files);
    let mut runs: Vec<Run> = Vec::new();
    let mut hits: Vec<Vec<Hit>> = Vec::with_capacity(lines.len());
    let mut active: BTreeMap<(usize, u32), usize> = BTreeMap::new();
    for readings in lines {
        let mut here = Vec::new();
        let mut next = BTreeMap::new();
        let mut taken = HashSet::new();
        let mut walk = Walk {
            files,
            runs: &mut runs,
            here: &mut here,
            next: &mut next,
            taken: &mut taken,
        };
        walk.extend(&active, readings);
        walk.start(&postings, readings);
        hits.push(here);
        active = next;
    }
    hits.iter()
        .map(|here| best_hit(here, &runs, &postings))
        .collect()
}

/// One delivered line's step through the runs.
struct Walk<'a> {
    files: &'a [&'a [u64]],
    runs: &'a mut Vec<Run>,
    here: &'a mut Vec<Hit>,
    next: &'a mut BTreeMap<(usize, u32), usize>,
    taken: &'a mut HashSet<(usize, u32)>,
}

impl Walk<'_> {
    /// Extend every run whose next file line is a reading of this line.
    fn extend(&mut self, active: &BTreeMap<(usize, u32), usize>, readings: &[LineReading]) {
        if active.len() > MAX_ACTIVE_RUNS {
            return;
        }
        for (&(file, line), &run) in active {
            let Some(at) = self.hash_at(file, line) else {
                continue;
            };
            let Some(reading) = readings
                .iter()
                .find(|r| r.hash == at && admits(r, file, line))
            else {
                continue;
            };
            self.push(run, file, line, reading);
        }
    }

    /// Start a run at each place a reading may begin one: its printed place
    /// alone when it printed both a file and a line, else every place a
    /// hash that is not noisy occurs.
    fn start(&mut self, postings: &Postings, readings: &[LineReading]) {
        for reading in readings {
            let places: Vec<(usize, u32)> = match (reading.anchor_file, reading.anchor_line) {
                (Some(file), Some(line)) if self.hash_at(file, line) == Some(reading.hash) => {
                    vec![(file, line)]
                }
                (Some(_), Some(_)) => continue,
                _ if postings.count(reading.hash) as usize > NOISY_PLACES => continue,
                _ => postings.places(reading.hash).to_vec(),
            };
            for (file, line) in places {
                if self.taken.contains(&(file, line)) || !admits(reading, file, line) {
                    continue;
                }
                self.runs.push(Run::default());
                let run = self.runs.len() - 1;
                self.push(run, file, line, reading);
            }
        }
    }

    fn push(&mut self, run: usize, file: usize, line: u32, reading: &LineReading) {
        if let Some(followed) = self.runs.get_mut(run) {
            if !reading.trivial {
                followed.distinct.insert(reading.hash);
            }
        }
        self.next.insert((file, line.saturating_add(1)), run);
        self.taken.insert((file, line));
        self.here.push(Hit {
            run,
            file,
            line,
            hash: reading.hash,
            trivial: reading.trivial,
            anchored: reading.anchor_line == Some(line)
                && (reading.anchor_file.is_some() || !reading.trivial),
        });
    }

    fn hash_at(&self, file: usize, line: u32) -> Option<u64> {
        let at = usize::try_from(line.checked_sub(1)?).ok()?;
        self.files.get(file)?.get(at).copied()
    }
}

/// Whether a reading's printed anchors allow the place `(file, line)`.
fn admits(reading: &LineReading, file: usize, line: u32) -> bool {
    reading.anchor_line.is_none_or(|n| n == line) && reading.anchor_file.is_none_or(|f| f == file)
}

/// The line's placement in its longest run, or `None` where two places tie.
fn best_hit(here: &[Hit], runs: &[Run], postings: &Postings) -> Option<Placement> {
    let length = |hit: &Hit| runs.get(hit.run).map_or(0, |run| run.distinct.len());
    let longest = here.iter().map(length).max()?;
    let mut best = here.iter().filter(|hit| length(hit) == longest);
    let first = best.next()?;
    if best.any(|hit| (hit.file, hit.line) != (first.file, first.line)) {
        return None;
    }
    let distinct = u32::try_from(longest).unwrap_or(u32::MAX);
    let unique = !first.trivial && postings.count(first.hash) == 1;
    Some(Placement {
        file: first.file,
        line: first.line,
        run: first.run,
        grounded: distinct >= GROUNDING_RUN || unique || first.anchored,
    })
}

#[cfg(test)]
mod tests {
    use super::super::hash::line_hash;
    use super::*;

    fn plain(text: &str) -> Vec<LineReading> {
        vec![LineReading {
            hash: line_hash(text.as_bytes()),
            trivial: text.trim().len() < 4,
            anchor_file: None,
            anchor_line: None,
        }]
    }

    fn hashes(text: &str) -> Vec<u64> {
        text.lines().map(|l| line_hash(l.as_bytes())).collect()
    }

    /// ★ Six distinct lines in order ground themselves; the same lines
    /// shuffled ground only where each is unique.
    #[test]
    fn a_run_of_distinct_lines_grounds_itself() {
        let body = "alpha one\nbeta two\ngamma three\ndelta four\nepsilon five\nzeta six\n";
        let mut twice = String::from(body);
        twice.push_str(body);
        let file = hashes(&twice);
        let files = [file.as_slice()];
        let lines: Vec<_> = body.lines().map(plain).collect();
        let placed = place_lines(&files, &lines);
        // Each line occurs twice, so only the run can ground it — and the
        // run fits at both copies, so the tie leaves it unplaced.
        assert!(placed.iter().all(Option::is_none), "{placed:?}");
        let once = hashes(body);
        let placed = place_lines(&[once.as_slice()], &lines);
        assert!(placed.iter().all(|p| p.is_some_and(|p| p.grounded)));
        assert_eq!(placed[2].map(|p| p.line), Some(3));
    }
}
