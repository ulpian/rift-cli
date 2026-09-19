//! Runs of lines and sets of them: the arithmetic every coverage question
//! is asked in.
//!
//! A separate module because nothing here knows what a row of the journal
//! is. An interval is a pair of line numbers and a set is their union, and
//! the row grammar, the queries and the decision all reason in them.

/// A run of lines, 1-based and inclusive at both ends — the same numbering a
/// reader sees in an editor and a tool reports in its output.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Interval {
    /// The first line of the run.
    pub lo: u32,
    /// The last line of the run.
    pub hi: u32,
}

impl Interval {
    /// A run from `lo` to `hi`, or `None` when the pair is not a run: a line
    /// zero (the numbering starts at one) or an end before its start.
    ///
    /// The constructor is the only way in, so nothing downstream has to ask
    /// whether an interval it was handed makes sense.
    #[must_use]
    pub const fn new(lo: u32, hi: u32) -> Option<Self> {
        if lo == 0 || hi < lo {
            None
        } else {
            Some(Self { lo, hi })
        }
    }

    /// Whether every line of `other` is a line of this run.
    #[must_use]
    pub const fn contains(&self, other: Self) -> bool {
        self.lo <= other.lo && other.hi <= self.hi
    }

    /// Whether this run and `other` have a line in common.
    ///
    /// Touching counts: `L1-10` and `L10-20` share line ten, and a claim
    /// about line ten is the same claim whichever run it is made from.
    #[must_use]
    pub const fn intersects(&self, other: Self) -> bool {
        self.lo <= other.hi && other.lo <= self.hi
    }

    /// How many lines the run spans.
    #[must_use]
    pub const fn lines(&self) -> u32 {
        self.hi - self.lo + 1
    }

    /// Whether this run is the stand-in for *every* line of a file, however
    /// long it is — the run [`IntervalSet::whole_file`] is made of.
    ///
    /// A caller pricing the run has to tell the two apart: `L1-4294967295` is
    /// not a claim about four billion lines, it is the claim that no request
    /// can fall outside what the window holds.
    #[must_use]
    pub const fn is_whole_file(&self) -> bool {
        self.lo == 1 && self.hi == u32::MAX
    }
}

/// A set of line runs, kept sorted, disjoint and non-adjacent, so that
/// "does this hold every line asked for?" is a walk rather than a search.
///
/// Two runs that meet end to end are one run: lines 1-5 and 6-10 leave no gap
/// between them, and a request for 1-10 is satisfied by having been given
/// both halves.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IntervalSet {
    /// Sorted by start; no two entries overlap or touch.
    runs: Vec<Interval>,
}

impl IntervalSet {
    /// An empty set — the coverage of a revision nothing has been read of.
    #[must_use]
    pub const fn new() -> Self {
        Self { runs: Vec::new() }
    }

    /// Every line of a file, however long it is.
    ///
    /// This is what an agent holds of a file it wrote in full and nothing has
    /// rewritten since: the count of lines does not have to be known, because
    /// no request can fall outside it.
    #[must_use]
    pub fn whole_file() -> Self {
        Self {
            runs: vec![Interval {
                lo: 1,
                hi: u32::MAX,
            }],
        }
    }

    /// Add a run, merging it with everything it overlaps or touches.
    pub fn insert(&mut self, run: Interval) {
        let mut merged = run;
        let mut out: Vec<Interval> = Vec::with_capacity(self.runs.len() + 1);
        for existing in self.runs.drain(..) {
            if existing.hi.saturating_add(1) < merged.lo {
                out.push(existing);
            } else if merged.hi.saturating_add(1) < existing.lo {
                out.push(merged);
                merged = existing;
            } else {
                merged = Interval {
                    lo: merged.lo.min(existing.lo),
                    hi: merged.hi.max(existing.hi),
                };
            }
        }
        out.push(merged);
        self.runs = out;
    }

    /// Whether every line of `run` is in the set.
    #[must_use]
    pub fn contains(&self, run: Interval) -> bool {
        self.runs.iter().any(|held| held.contains(run))
    }

    /// Whether nothing is held at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.runs.is_empty()
    }

    /// The runs, in order.
    #[must_use]
    pub fn runs(&self) -> &[Interval] {
        &self.runs
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::run;
    use super::*;

    #[test]
    fn a_run_starts_at_one_and_ends_at_or_after_its_start() {
        assert_eq!(Interval::new(0, 4), None, "line zero is not a line");
        assert_eq!(Interval::new(9, 8), None, "an end before its start");
        assert_eq!(run(3, 3).lines(), 1);
        assert_eq!(run(1, 10).lines(), 10);
        assert!(run(1, 10).contains(run(3, 4)));
        assert!(!run(1, 10).contains(run(3, 11)));
    }

    #[test]
    fn runs_that_meet_end_to_end_are_one_run() {
        let mut set = IntervalSet::new();
        set.insert(run(6, 10));
        set.insert(run(1, 5));
        assert_eq!(set.runs(), [run(1, 10)], "1-5 and 6-10 leave no gap");
        assert!(set.contains(run(1, 10)));

        let mut gapped = IntervalSet::new();
        gapped.insert(run(1, 5));
        gapped.insert(run(7, 10));
        assert_eq!(gapped.runs(), [run(1, 5), run(7, 10)], "line 6 is missing");
        assert!(!gapped.contains(run(1, 10)));
        gapped.insert(run(6, 6));
        assert_eq!(gapped.runs(), [run(1, 10)], "line 6 closes the gap");
    }
}
