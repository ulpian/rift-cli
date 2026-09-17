//! Phase timing for the git-hook path — env-gated, off by default.
//!
//! The hook budget is defined as **git latency**: the synchronous part of every
//! commit-path hook must return within 100 ms warm. A budget stated in one
//! number is only actionable if the number can be split, so this module exists
//! to answer *where the milliseconds went* from the shipped binary rather than
//! from a re-implementation of the hook in a benchmark.
//!
//! `RIFT_HOOK_TIMING=1` (any value) makes each instrumented hook print exactly
//! one line to stderr when its body finishes:
//!
//! ```text
//! rift-timing hook=post-commit total_us=305118 discover_us=1204 gix_open_us=…
//! ```
//!
//! Unset — the shipped default, and what every real commit runs — the timer
//! takes no clock reading at all past construction and prints nothing, so the
//! instrumented path is the same path an uninstrumented user runs.
//!
//! Why stderr and not the projection: a hook that cannot open its store still
//! has phases worth reporting, and stderr is the one sink a `git commit`
//! wrapper can capture without cooperating with the store.
//!
//! The agent-hook path (`rift hooks <agent> <verb>`, keyed `hook=agent-hook`)
//! carries the same instrument against its own budget — the agent-hook
//! p50 < 100 ms *including ingest* — with phases `build_raw`, `enrich`,
//! `discover`, `gate_policy`, `doc_gate`, `doc_convert`, `read_gate`,
//! `edit_record`, `search_gate`, `search_answer` (`search_head`, `search_probe`,
//! `search_load`), `open_store`, `repo_id`, `ingest`,
//! `bind_identity`, `snapshot_boundaries` and `post_ingest`. The two budgets are distinct and
//! must not be quoted for each other.
//!
//! A hook fired outside any repository stops after `discover` and records
//! `outside_repository` instead: it captures nothing, and a
//! phase that says so is how the instrument distinguishes that from a hook
//! that never ran.

use std::cell::RefCell;
use std::time::Instant;

/// The environment variable that turns phase reporting on.
pub(crate) const TIMING_ENV: &str = "RIFT_HOOK_TIMING";

/// A hook invocation's phase ledger. Records `(name, micros)` in call order and
/// prints one summary line on drop — so an early return (no live session, an
/// unborn HEAD) still reports the phases it did pay for.
pub(crate) struct HookTimer {
    /// The hook name the line is keyed by (`post-commit`, …).
    hook: &'static str,
    /// `false` short-circuits every phase to a plain call with no clock read.
    enabled: bool,
    /// When the hook body started (only meaningful while `enabled`).
    start: Instant,
    /// Phases in the order they completed.
    records: RefCell<Vec<(&'static str, u128)>>,
}

impl HookTimer {
    /// A timer for `hook`, enabled iff [`TIMING_ENV`] is set in the environment.
    pub(crate) fn from_env(hook: &'static str) -> Self {
        Self::new(hook, std::env::var_os(TIMING_ENV).is_some())
    }

    /// A timer with an explicit gate — the seam the tests drive, so no test has
    /// to mutate process-global environment to exercise either state.
    pub(crate) fn new(hook: &'static str, enabled: bool) -> Self {
        Self {
            hook,
            enabled,
            start: Instant::now(),
            records: RefCell::new(Vec::new()),
        }
    }

    /// Run `f`, recording its wall time under `name` when enabled.
    ///
    /// The value (including a `Result`, which callers `?` straight through) is
    /// returned untouched: wrapping a call in a phase must never change what
    /// the call does or how its failure propagates.
    pub(crate) fn phase<T>(&self, name: &'static str, f: impl FnOnce() -> T) -> T {
        if !self.enabled {
            return f();
        }
        let t0 = Instant::now();
        let out = f();
        self.records
            .borrow_mut()
            .push((name, t0.elapsed().as_micros()));
        out
    }

    /// The phases recorded so far, in completion order.
    #[cfg(test)]
    pub(crate) fn records(&self) -> Vec<(&'static str, u128)> {
        self.records.borrow().clone()
    }

    /// The one-line report: the hook, the total, then every phase in order.
    ///
    /// A repeated phase name appears repeatedly — a harness that sums by key
    /// gets the true total, and one that reads left to right sees the order.
    /// Collapsing duplicates here would hide a call site that runs twice, which
    /// is exactly the kind of finding this instrument exists to surface.
    pub(crate) fn report_line(&self) -> String {
        let mut line = format!(
            "rift-timing hook={} total_us={}",
            self.hook,
            self.start.elapsed().as_micros()
        );
        for (name, us) in self.records.borrow().iter() {
            line.push_str(&format!(" {name}_us={us}"));
        }
        line
    }
}

impl Drop for HookTimer {
    fn drop(&mut self) {
        if self.enabled {
            eprintln!("{}", self.report_line());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disabled_timer_still_runs_the_body_and_records_nothing() {
        // The shipped default. The phase wrapper must be a pure pass-through:
        // an instrument that changes behaviour when off is not an instrument.
        let t = HookTimer::new("post-commit", false);
        let mut ran = 0;
        let out = t.phase("work", || {
            ran += 1;
            41 + 1
        });
        assert_eq!(out, 42, "the body's value passes through");
        assert_eq!(ran, 1, "the body ran exactly once");
        assert!(t.records().is_empty(), "nothing recorded while disabled");
    }

    #[test]
    fn an_enabled_timer_records_phases_in_completion_order() {
        let t = HookTimer::new("prepare-commit-msg", true);
        t.phase("first", || ());
        t.phase("second", || ());
        let names: Vec<&str> = t.records().into_iter().map(|(n, _)| n).collect();
        assert_eq!(names, vec!["first", "second"]);
    }

    #[test]
    fn a_result_phase_propagates_its_error_unchanged() {
        // Every hook phase wraps a `?` call; a wrapper that swallowed or
        // re-typed the error would silently change the never-block contract.
        let t = HookTimer::new("post-commit", true);
        let out: Result<(), &str> = t.phase("failing", || Err("boom"));
        assert_eq!(out, Err("boom"));
        assert_eq!(t.records().len(), 1, "a failed phase is still recorded");
    }

    #[test]
    fn the_report_line_names_the_hook_the_total_and_every_phase() {
        let t = HookTimer::new("post-commit", true);
        t.phase("open", || ());
        t.phase("archive", || ());
        let line = t.report_line();
        assert!(
            line.starts_with("rift-timing hook=post-commit total_us="),
            "{line}"
        );
        assert!(line.contains(" open_us="), "{line}");
        assert!(line.contains(" archive_us="), "{line}");
    }

    #[test]
    fn a_repeated_phase_name_is_reported_twice_rather_than_collapsed() {
        // Two occurrences of one key is how "this call site runs on both
        // hooks" shows up in the data; summing them is the harness's job.
        let t = HookTimer::new("post-commit", true);
        t.phase("redact", || ());
        t.phase("redact", || ());
        let line = t.report_line();
        assert_eq!(
            line.matches(" redact_us=").count(),
            2,
            "both occurrences survive into the line: {line}"
        );
    }
}
