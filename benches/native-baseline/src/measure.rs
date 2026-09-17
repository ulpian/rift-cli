//! The measurement protocol: how one number is produced.
//!
//! * The timed region covers **spawn through exit**. Process startup is not
//!   overhead to be excluded — for the hook comparison it *is* the difference
//!   (Reference spawns a Go binary per event), so excluding it would flatter Rift.
//! * Warmup iterations run and are discarded; measured iterations are then timed
//!   individually and kept as a sample vector, never as a running average.
//! * Percentiles are **nearest-rank** over the sorted samples: no interpolation,
//!   no smoothing, and `p95` of a small sample is honestly just "the worst few".
//!   Sample counts are published with every statistic.
//! * A non-zero exit is counted, not hidden. A measure with any failed iteration
//!   is reported as failed rather than as a fast number.
//! * Peak RSS is taken in a **separate, untimed pass** (see [`crate::rss`]),
//!   because the platform `time` wrapper would otherwise sit inside the timing.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::Stdio,
    time::Instant,
};

use serde::{Deserialize, Serialize};

use crate::{
    hermetic::{Hermetic, Hooks},
    BenchError, Result,
};

/// How many times a measure is executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Protocol {
    /// Discarded iterations run before measurement (page cache, JIT-free but the
    /// OS still warms).
    pub warmup: usize,
    /// Measured iterations.
    pub iterations: usize,
}

impl Protocol {
    /// An explicit protocol.
    #[must_use]
    pub const fn new(warmup: usize, iterations: usize) -> Self {
        Self { warmup, iterations }
    }

    /// Sub-second verbs (hook round-trip, `status`, `checkpoint list`): enough
    /// samples for a meaningful p95.
    #[must_use]
    pub const fn latency() -> Self {
        Self::new(3, 30)
    }

    /// Multi-second work (condense, import, a full scripted session).
    #[must_use]
    pub const fn heavy() -> Self {
        Self::new(1, 3)
    }

    /// A single measured run — for smoke checks and CI dry runs.
    #[must_use]
    pub const fn smoke() -> Self {
        Self::new(0, 1)
    }
}

/// A bar a measure is held to, stated by the plan rather than by the run.
///
/// A budget is published with the number it judges, so a reader sees the bar and
/// the result together instead of taking the harness's word for a verdict.
///
/// **Two shapes, because latency and throughput fail in opposite directions.** A
/// latency measure is judged by a ceiling it must stay under; a throughput
/// measure by a floor it must stay above, and a floor cannot be expressed as a
/// wall time at all — 3 s is excellent for ten thousand records and disgraceful
/// for ten. Until this existed the harness could not state a throughput bar, so
/// the plan's two throughput rows could not be judged even once they were driven.
///
/// **`Throughput { min_units_per_s: None }` is a declared-but-unratified floor,
/// and it is deliberately representable.** The owner set no condense or
/// import throughput target anywhere, so those rows are
/// not merely unmeasured, they are *unmeetable* — and a harness that renders
/// "met" for a bar nobody set would be manufacturing a pass. The verdict is
/// `OPEN — awaiting owner ratification` until the owner sets a number; it is
/// never silently green.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Budget {
    /// A wall-time ceiling the measure must stay under.
    Latency {
        /// Median ceiling, milliseconds.
        p50_ms: u64,
        /// 95th-percentile ceiling, milliseconds.
        p95_ms: u64,
    },
    /// A work-rate floor the measure must stay above, in the scenario's own work
    /// unit. `None` means the floor has not been ratified.
    Throughput {
        /// Units per second the measure must reach, once an owner sets it.
        min_units_per_s: Option<u64>,
    },
}

impl Budget {
    /// The agent-hook hot path, **including** ingest.
    #[must_use]
    pub const fn hook() -> Self {
        Self::Latency {
            p50_ms: 100,
            p95_ms: 500,
        }
    }

    /// The hard ceiling a Rift hook may add to a **git** operation a person
    /// typed.
    ///
    /// One number, so both percentiles carry it. The owner set a hard bar and
    /// named no tail allowance, and inventing a p95 this harness was never
    /// given would be the harness marking its own paper.
    #[must_use]
    pub const fn git_latency() -> Self {
        Self::Latency {
            p50_ms: 100,
            p95_ms: 100,
        }
    }

    /// A throughput row whose floor the owner has not set.
    #[must_use]
    pub const fn unratified_throughput() -> Self {
        Self::Throughput {
            min_units_per_s: None,
        }
    }

    /// How `stats` stands against the bar — **as separate facts, never one**.
    ///
    /// A measure with any failed iteration clears nothing: it did not do the
    /// work.
    #[must_use]
    pub fn verdict(self, stats: &Stats) -> BudgetVerdict {
        if stats.moved_no_work() && matches!(self, Self::Throughput { .. }) {
            return BudgetVerdict::NoWork;
        }
        match self {
            Self::Latency { p50_ms, p95_ms } => BudgetVerdict::Latency {
                p50_met: stats.clean() && stats.p50_ns <= p50_ms.saturating_mul(1_000_000),
                p95_met: stats.clean() && stats.p95_ns <= p95_ms.saturating_mul(1_000_000),
            },
            Self::Throughput { min_units_per_s } => {
                let (Some(floor), Some(rate)) = (min_units_per_s, stats.rate_per_s()) else {
                    return BudgetVerdict::Unratified;
                };
                BudgetVerdict::Throughput(stats.clean() && rate >= floor as f64)
            }
        }
    }

    /// The bar as the report prints it, beside the number it judges.
    #[must_use]
    pub fn describe(self, unit: Option<&str>) -> String {
        let unit = unit.unwrap_or("units");
        match self {
            Self::Latency { p50_ms, p95_ms } => {
                format!("p50 ≤ {p50_ms} ms, p95 ≤ {p95_ms} ms")
            }
            Self::Throughput {
                min_units_per_s: Some(floor),
            } => format!("≥ {floor} {unit}/s"),
            Self::Throughput {
                min_units_per_s: None,
            } => format!("{unit}/s — no floor ratified"),
        }
    }
}

/// Where a measure stands against its bar.
///
/// **By owner decision a latency verdict is TWO facts, and this type refuses
/// to carry one.** A measured p50 can meet the hook budget while the p95 does not;
/// collapsing those into a single word would either credit the budget with a
/// p95 it does not have, or fail a p50 that genuinely meets it. There is
/// deliberately **no** `fn met(&self) -> bool` here: an accessor returning one
/// boolean is how the two facts get collapsed again by the next caller, and
/// this project has spent two days removing guards that print one word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetVerdict {
    /// A latency bar, judged twice.
    Latency {
        /// The median is inside the ceiling.
        p50_met: bool,
        /// The 95th percentile is inside the ceiling.
        p95_met: bool,
    },
    /// A throughput floor, cleared or not.
    Throughput(bool),
    /// A floor the owner has not set. Never a pass.
    Unratified,
    /// The row ran and moved no work, so the rate is unmeasurable.
    NoWork,
}

/// One invocation of a leg binary.
#[derive(Debug, Clone)]
pub struct Invocation {
    /// The binary to run.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<String>,
    /// Working directory (usually a fixture repository).
    pub cwd: PathBuf,
    /// Bytes fed to stdin (hook payloads).
    pub stdin: Option<String>,
    /// Extra environment on top of the hermetic base.
    pub env: Vec<(String, String)>,
    /// Whether repository git hooks may run.
    pub hooks: Hooks,
}

impl Invocation {
    /// A bare invocation of `program` in `cwd` with hooks enabled.
    pub fn new(program: impl Into<PathBuf>, cwd: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: cwd.into(),
            stdin: None,
            env: Vec::new(),
            hooks: Hooks::Enabled,
        }
    }

    /// Append arguments.
    #[must_use]
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Feed `body` on stdin.
    #[must_use]
    pub fn stdin(mut self, body: impl Into<String>) -> Self {
        self.stdin = Some(body.into());
        self
    }

    /// Add one environment variable.
    #[must_use]
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    /// Run in `cwd` instead (per-iteration fresh copies).
    #[must_use]
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = cwd.into();
        self
    }

    /// The command line as displayed in reports.
    #[must_use]
    pub fn command_line(&self) -> String {
        let program = Path::new(&self.program).file_name().map_or_else(
            || self.program.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        if self.args.is_empty() {
            program
        } else {
            format!("{program} {}", self.args.join(" "))
        }
    }
}

/// One timed execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    /// Wall time from spawn to exit.
    pub wall_ns: u64,
    /// Exit code, or `None` when the process was signalled.
    pub exit_code: Option<i32>,
}

impl Sample {
    /// Whether this iteration succeeded.
    #[must_use]
    pub fn ok(&self) -> bool {
        self.exit_code == Some(0)
    }
}

/// A timed execution plus what the process said.
#[derive(Debug, Clone)]
pub struct Execution {
    /// The timing.
    pub sample: Sample,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

/// Run `inv` once, timing spawn through exit.
///
/// # Errors
/// [`BenchError::Spawn`] if the binary cannot be launched, [`BenchError::Io`] if
/// stdin cannot be written or output collected.
pub fn run_once(hermetic: &Hermetic, inv: &Invocation) -> Result<Execution> {
    let mut cmd = hermetic.command(&inv.program, &inv.cwd, inv.hooks);
    cmd.args(&inv.args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(if inv.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    for (k, v) in &inv.env {
        cmd.env(k, v);
    }

    let started = Instant::now();
    let mut child = cmd.spawn().map_err(|e| BenchError::Spawn {
        program: inv.program.display().to_string(),
        source: e,
    })?;
    // Feed stdin from its own thread. Writing it inline and only then draining
    // the output pipes deadlocks as soon as the payload outgrows the pipe buffer
    // (64 KiB on macOS/Linux): the child blocks writing stdout that nobody is
    // reading, so it never drains stdin, so the harness blocks writing stdin.
    // Today's hook payloads are far below that, but a scripted-session replay is
    // exactly the workload that crosses it — and a benchmark that hangs on its
    // own biggest input is the least useful failure mode there is.
    let writer = inv.stdin.clone().and_then(|body| {
        let mut pipe = child.stdin.take()?;
        std::thread::Builder::new()
            .name("bench-stdin".into())
            .spawn(move || pipe.write_all(body.as_bytes()))
            .ok()
    });
    let out = child
        .wait_with_output()
        .map_err(|e| BenchError::io("wait for child", e))?;
    let wall_ns = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
    if let Some(writer) = writer {
        match writer.join() {
            // A child that exits without reading its whole payload breaks the
            // pipe. That is the child's decision, not a harness failure, and the
            // exit code already carries the verdict.
            Ok(Err(e)) if e.kind() != std::io::ErrorKind::BrokenPipe => {
                return Err(BenchError::io("write child stdin", e));
            }
            Ok(_) => {}
            Err(_) => return Err(BenchError::Spec("stdin writer thread panicked".into())),
        }
    }

    Ok(Execution {
        sample: Sample {
            wall_ns,
            exit_code: out.status.code(),
        },
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Execute `protocol` against invocations produced by `prepare`.
///
/// `prepare(i)` runs **outside** the timed region, so per-iteration setup (a
/// fresh fixture copy, a cleared cache) never lands in the number. Warmup
/// iterations are numbered from zero and discarded.
///
/// # Errors
/// Propagates any error from `prepare` or from launching the child.
pub fn measure<F>(hermetic: &Hermetic, protocol: Protocol, mut prepare: F) -> Result<Vec<Sample>>
where
    F: FnMut(usize) -> Result<Invocation>,
{
    for i in 0..protocol.warmup {
        let inv = prepare(i)?;
        run_once(hermetic, &inv)?;
    }
    let mut samples = Vec::with_capacity(protocol.iterations);
    for i in 0..protocol.iterations {
        let inv = prepare(protocol.warmup + i)?;
        samples.push(run_once(hermetic, &inv)?.sample);
    }
    Ok(samples)
}

/// The published shape of a sample vector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stats {
    /// Measured iterations.
    pub samples: usize,
    /// Iterations that exited non-zero (a measure with any is reported failed).
    pub failures: usize,
    /// Fastest iteration.
    pub min_ns: u64,
    /// Nearest-rank median.
    pub p50_ns: u64,
    /// Nearest-rank 95th percentile.
    pub p95_ns: u64,
    /// Slowest iteration.
    pub max_ns: u64,
    /// Arithmetic mean (published for completeness; the gates use p50/p95).
    pub mean_ns: u64,
    /// How much work one measured iteration performed, in the scenario's own
    /// unit — **counted by a probe, never assumed**.
    ///
    /// This is the denominator a rate needs and the harness did not have: a
    /// `Stats` with only wall times can say a run took 3 s and cannot say
    /// whether that was fast, because nothing records what was moved. It is
    /// also the corroboration probe on the measure itself. `rift verify` over a
    /// repository holding no checkpoints exits quickly and correctly, and its
    /// wall time is a beautiful number about nothing; with this field the
    /// report prints `0 checkpoints` beside it and the reader can see that.
    ///
    /// `None` where nothing counts the work, which is honest and is not zero:
    /// zero units is a measure that did nothing, absent is a measure nobody
    /// asked.
    #[serde(default)]
    pub units_per_iteration: Option<u64>,
}

impl Stats {
    /// Summarize `samples`; `None` for an empty vector (a measure that never ran
    /// has no statistics, and inventing zeros would read as "infinitely fast").
    #[must_use]
    pub fn of(samples: &[Sample]) -> Option<Self> {
        if samples.is_empty() {
            return None;
        }
        let mut sorted: Vec<u64> = samples.iter().map(|s| s.wall_ns).collect();
        sorted.sort_unstable();
        let total: u128 = sorted.iter().map(|n| u128::from(*n)).sum();
        let mean = u64::try_from(total / sorted.len() as u128).unwrap_or(u64::MAX);
        Some(Self {
            samples: sorted.len(),
            failures: samples.iter().filter(|s| !s.ok()).count(),
            min_ns: nearest_rank(&sorted, 0),
            p50_ns: nearest_rank(&sorted, 50),
            p95_ns: nearest_rank(&sorted, 95),
            max_ns: nearest_rank(&sorted, 100),
            mean_ns: mean,
            units_per_iteration: None,
        })
    }

    /// Whether every measured iteration succeeded.
    #[must_use]
    pub fn clean(&self) -> bool {
        self.failures == 0
    }

    /// Attach the probed work count for one measured iteration.
    #[must_use]
    pub fn with_units(mut self, per_iteration: u64) -> Self {
        self.units_per_iteration = Some(per_iteration);
        self
    }

    /// Work units per second at the **median** iteration.
    ///
    /// Quoted off `p50` rather than off the total elapsed time, so the rate is
    /// the same statistic as every other figure in the row and one slow
    /// iteration cannot drag it.
    ///
    /// `None` in three cases, and they are all "there is no rate here" rather
    /// than "the rate is bad":
    ///
    /// * nothing counted the work — no denominator at all;
    /// * the median iteration was too fast to time — a division by zero
    ///   rendered as an infinite rate is the most flattering thing a benchmark
    ///   can print;
    /// * **the row moved zero work.** `0/s` is not the honest rendering of an
    ///   empty corpus: it reads as *"it did work, slowly"*, and blames the code
    ///   for a fixture that handed it nothing. A rate needs a positive
    ///   denominator. The zero itself is still published, by
    ///   [`Self::units_per_iteration`] — the reader must be able to see that
    ///   the measure ran and moved nothing, which is exactly the finding.
    #[must_use]
    pub fn rate_per_s(&self) -> Option<f64> {
        let units = self.units_per_iteration?;
        if units == 0 || self.p50_ns == 0 {
            return None;
        }
        Some(units as f64 * 1_000_000_000.0 / self.p50_ns as f64)
    }

    /// Whether this measure ran and moved no work at all.
    ///
    /// Distinct from "nobody counted": a row that is unmeasurable *as a rate*
    /// because its corpus is empty must say so rather than publish a zero.
    #[must_use]
    pub fn moved_no_work(&self) -> bool {
        self.units_per_iteration == Some(0)
    }
}

/// Nearest-rank percentile over an ascending vector (`pct` in `0..=100`).
///
/// Rank is `ceil(pct/100 * n)`, clamped to `1..=n`; the value returned is always
/// an observed sample, never an interpolation between two.
#[must_use]
pub fn nearest_rank(sorted: &[u64], pct: u32) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let rank = (u128::from(pct) * n as u128).div_ceil(100).max(1) as usize;
    let idx = rank.min(n) - 1;
    sorted.get(idx).copied().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(values: &[u64]) -> Vec<Sample> {
        values
            .iter()
            .map(|v| Sample {
                wall_ns: *v,
                exit_code: Some(0),
            })
            .collect()
    }

    #[test]
    fn nearest_rank_returns_observed_values_only() {
        let sorted = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100];
        assert_eq!(nearest_rank(&sorted, 0), 10);
        assert_eq!(nearest_rank(&sorted, 50), 50);
        assert_eq!(nearest_rank(&sorted, 95), 100);
        assert_eq!(nearest_rank(&sorted, 100), 100);
        assert_eq!(nearest_rank(&[], 50), 0);
        assert_eq!(nearest_rank(&[7], 95), 7);
        // p90 of ten samples is the 9th, not an average of the 9th and 10th.
        assert_eq!(nearest_rank(&sorted, 90), 90);
    }

    #[test]
    fn stats_are_ordered_and_count_failures() {
        let mut s = samples(&[5, 1, 3, 2, 4]);
        s.push(Sample {
            wall_ns: 9,
            exit_code: Some(1),
        });
        let stats = Stats::of(&s).unwrap();
        assert_eq!(stats.samples, 6);
        assert_eq!(stats.failures, 1);
        assert!(!stats.clean());
        assert!(stats.min_ns <= stats.p50_ns);
        assert!(stats.p50_ns <= stats.p95_ns);
        assert!(stats.p95_ns <= stats.max_ns);
        assert_eq!(stats.min_ns, 1);
        assert_eq!(stats.max_ns, 9);
        assert_eq!(stats.mean_ns, 4);
    }

    #[test]
    fn a_budget_judges_p50_and_p95_and_never_forgives_a_failure() {
        let budget = Budget::hook();
        assert_eq!(
            budget,
            Budget::Latency {
                p50_ms: 100,
                p95_ms: 500
            }
        );
        let inside = Stats::of(&samples(&[50_000_000, 60_000_000, 400_000_000])).unwrap();
        assert_eq!(
            budget.verdict(&inside),
            BudgetVerdict::Latency {
                p50_met: true,
                p95_met: true
            }
        );
        // The two-facts shape, and the one this project actually measured: the
        // median meets and the tail does not. A single boolean here would have
        // to lie about one of them.
        let tail = Stats::of(&samples(&[50_000_000, 60_000_000, 900_000_000])).unwrap();
        assert_eq!(
            budget.verdict(&tail),
            BudgetVerdict::Latency {
                p50_met: true,
                p95_met: false
            }
        );
        // Everything inside, but an iteration did not do the work.
        let mut dirty = samples(&[50_000_000, 60_000_000]);
        dirty.push(Sample {
            wall_ns: 1,
            exit_code: Some(1),
        });
        assert_eq!(
            budget.verdict(&Stats::of(&dirty).unwrap()),
            BudgetVerdict::Latency {
                p50_met: false,
                p95_met: false
            }
        );
    }

    /// A rate needs a denominator, and the harness had none: the same three
    /// wall times mean opposite things depending on how much work they moved.
    #[test]
    fn a_rate_is_work_over_the_median_and_absent_without_a_count() {
        let stats = Stats::of(&samples(&[1_000_000_000, 2_000_000_000, 4_000_000_000])).unwrap();
        assert_eq!(stats.p50_ns, 2_000_000_000);
        assert_eq!(stats.units_per_iteration, None);
        assert_eq!(stats.rate_per_s(), None, "no denominator, no rate");

        let counted = stats.with_units(500);
        assert_eq!(counted.units_per_iteration, Some(500));
        // 500 units in the 2 s median.
        assert!((counted.rate_per_s().unwrap() - 250.0).abs() < 1e-9);

        // A measure that moved nothing has NO rate — `0/s` would read as "it
        // did work, slowly" and blame the code for an empty corpus. The zero
        // itself still travels, as the work count.
        let empty = stats.with_units(0);
        assert_eq!(empty.rate_per_s(), None);
        assert!(empty.moved_no_work());
        assert_eq!(empty.units_per_iteration, Some(0));
        assert!(!stats.moved_no_work(), "uncounted is not zero");
    }

    /// An iteration too fast to time must not render as an infinite rate — the
    /// single most flattering thing a benchmark can print.
    #[test]
    fn an_unmeasurably_fast_median_has_no_rate_rather_than_an_infinite_one() {
        let stats = Stats::of(&samples(&[0, 0, 0])).unwrap().with_units(1_000);
        assert_eq!(stats.p50_ns, 0);
        assert_eq!(stats.rate_per_s(), None);
    }

    /// No condense or import throughput target was ever set, so those rows
    /// are unmeetable, not merely unmeasured. A floor nobody set
    /// must be unjudgeable — never a pass.
    #[test]
    fn an_unratified_throughput_floor_is_unjudgeable_and_a_ratified_one_is_not() {
        let fast = Stats::of(&samples(&[1_000_000_000]))
            .unwrap()
            .with_units(900);
        let open = Budget::unratified_throughput();
        assert_eq!(
            open.verdict(&fast),
            BudgetVerdict::Unratified,
            "a bar nobody set cannot be cleared"
        );
        assert!(open.describe(Some("shards")).contains("no floor ratified"));

        let ratified = Budget::Throughput {
            min_units_per_s: Some(500),
        };
        assert_eq!(ratified.verdict(&fast), BudgetVerdict::Throughput(true));
        assert_eq!(ratified.describe(Some("shards")), "≥ 500 shards/s");

        let slow = Stats::of(&samples(&[1_000_000_000]))
            .unwrap()
            .with_units(100);
        assert_eq!(ratified.verdict(&slow), BudgetVerdict::Throughput(false));

        // Ratified, but nothing counted the work: still unjudgeable, and NOT a
        // pass — a rate that cannot be computed is not a rate that is fine.
        let uncounted = Stats::of(&samples(&[1_000_000_000])).unwrap();
        assert_eq!(ratified.verdict(&uncounted), BudgetVerdict::Unratified);
    }

    #[test]
    fn an_empty_sample_vector_has_no_statistics() {
        assert!(Stats::of(&[]).is_none());
    }

    #[test]
    fn a_signalled_process_is_not_a_success() {
        assert!(!Sample {
            wall_ns: 1,
            exit_code: None
        }
        .ok());
    }

    #[test]
    fn run_once_times_a_real_process_and_captures_output() {
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Hermetic::prepare(tmp.path()).unwrap();
        let inv = Invocation::new("git", tmp.path()).args(["--version"]);
        let exec = run_once(&hermetic, &inv).unwrap();
        assert!(exec.sample.ok());
        assert!(exec.sample.wall_ns > 0);
        assert!(exec.stdout.starts_with("git version"));
        assert_eq!(inv.command_line(), "git --version");
    }

    #[test]
    fn stdin_reaches_the_child() {
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Hermetic::prepare(tmp.path()).unwrap();
        // `git hash-object --stdin` echoes a digest of exactly what it read.
        let inv = Invocation::new("git", tmp.path())
            .args(["hash-object", "--stdin"])
            .stdin("rift\n");
        let exec = run_once(&hermetic, &inv).unwrap();
        assert!(exec.sample.ok(), "stderr: {}", exec.stderr);
        assert_eq!(exec.stdout.trim().len(), 40);
    }

    #[test]
    fn a_payload_larger_than_the_pipe_buffer_does_not_deadlock() {
        // The child has to **echo** its input, or the test does not discriminate:
        // a child that reads 512 KiB and answers with 41 bytes never fills the
        // output pipe, so the inline-write bug this guards against would pass.
        // `git stripspace` copies stdin to stdout (it only trims trailing
        // whitespace and blank lines, and this body has neither), so 512 KiB —
        // eight times the usual 64 KiB pipe buffer — is in flight in both
        // directions at once.
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Hermetic::prepare(tmp.path()).unwrap();
        let body = "0123456789abcdef".repeat(32 * 1024);
        let inv = Invocation::new("git", tmp.path())
            .args(["stripspace"])
            .stdin(body.clone());
        let exec = run_once(&hermetic, &inv).unwrap();
        assert!(exec.sample.ok(), "stderr: {}", exec.stderr);
        assert_eq!(
            exec.stdout.trim_end(),
            body,
            "the child echoed {} bytes, not {}",
            exec.stdout.trim_end().len(),
            body.len()
        );

        // And a child that never reads the payload still yields its own verdict
        // rather than a harness error.
        let ignores_stdin = Invocation::new("git", tmp.path())
            .args(["--version"])
            .stdin(body);
        assert!(run_once(&hermetic, &ignores_stdin).unwrap().sample.ok());
    }

    #[test]
    fn measure_discards_warmup_and_keeps_every_measured_iteration() {
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Hermetic::prepare(tmp.path()).unwrap();
        let mut prepared = Vec::new();
        let protocol = Protocol::new(2, 3);
        let samples = measure(&hermetic, protocol, |i| {
            prepared.push(i);
            Ok(Invocation::new("git", tmp.path()).args(["--version"]))
        })
        .unwrap();
        assert_eq!(samples.len(), 3);
        assert_eq!(
            prepared,
            vec![0, 1, 2, 3, 4],
            "prepare sees every iteration"
        );
        assert!(samples.iter().all(Sample::ok));
    }

    #[test]
    fn a_failing_child_is_recorded_not_swallowed() {
        let tmp = tempfile::tempdir().unwrap();
        let hermetic = Hermetic::prepare(tmp.path()).unwrap();
        let inv = Invocation::new("git", tmp.path()).args(["definitely-not-a-verb"]);
        let exec = run_once(&hermetic, &inv).unwrap();
        assert!(!exec.sample.ok());
        assert!(!Stats::of(&[exec.sample]).unwrap().clean());
    }
}
