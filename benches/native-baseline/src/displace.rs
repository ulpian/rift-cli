//! Where the initialization cost sits: at `enable`, or at the first commit.
//!
//! # The claim, and why one order cannot test it
//!
//! The owner moved projection init to **enable time**. That is a claim
//! about *displacement*, not about speed, and displacement is invisible from
//! one direction: measure only enable-then-commit and a cheap first commit is
//! equally consistent with "the cost moved to enable" and with "the cost never
//! existed". So this measures both orders in one run, on one machine, minutes
//! apart:
//!
//! * `E0_enable` — what `rift init` itself costs. A wall time, **not** a git
//!   latency, and never quoted against the bare-commit control: initializing a
//!   repository and committing to one are different acts and a ratio between
//!   them would be a number wearing a unit it does not have.
//! * `E1_commit_after_enable` — the **first** commit in a repository that was
//!   enabled beforehand. This is the arm the owner's hard 100 ms git-latency
//!   budget binds: it is what a user's next commit costs.
//! * `E2_commit_without_prior_enable` — the first commit in a repository whose
//!   hooks are installed and whose projection state is **not** there yet.
//!
//! `E2 - E1` is the displaced cost, measured rather than argued. If the two
//! arms land together the cost was never at commit time at all — which is a
//! finding this instrument must be able to publish, not a result it may assume.
//!
//! # E2 is a faithful simulation and says so
//!
//! There is no supported way to install rift's hooks *without* initializing:
//! `rift init` does both, which is the whole point of the claim. So E2 runs
//! the real `rift init` through the real binary and then removes the projection
//! state the hook path would find, leaving the installed hooks in place. The
//! evidence rules permit exactly this — "deterministic unit tests, frozen
//! fixtures, or faithful simulation through the REAL binary" — and require it
//! to be recorded as such, which is why the arm carries the sentence into the
//! artifact rather than into a comment.
//!
//! Every arm gets a **fresh repository per iteration**, because "first commit"
//! is a property that exists exactly once in a repository's life.

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use crate::{
    freshness::SourceClock,
    hermetic::Hermetic,
    hooklat::BUDGET_MS,
    leg::{LegId, LegResolver, LegStatus},
    measure::{run_once, Invocation, Protocol, Sample, Stats},
    report::{now_unix, Host},
    scenario, BenchError, Result,
};

/// Schema of the displacement artifact.
pub const DISPLACEMENT_SCHEMA: u32 = 1;

/// The scenario id this artifact measures, as `benchmarks/floors.toml` spells
/// it. A paired LATENCY displacement measure, **never** a throughput floor —
/// an `enable` that did more work per second would not thereby be better, and
/// a slower commit is not thereby doing more.
pub const SCENARIO: &str = "enable-displacement";

/// Maximum time one iteration may spend waiting for the capture work that the
/// timed hook deliberately detached.
///
/// This wait is outside the sample. Its purpose is measurement isolation: the
/// next fresh repository must not compete with the previous repository's
/// condense/verify worker, because the git-latency budget explicitly excludes
/// that worker. A worker that cannot drain in this generous bound
/// invalidates the arm instead of contaminating every later sample.
const WORKER_QUIESCE_TIMEOUT: Duration = Duration::from_secs(30);
const WORKER_QUIESCE_POLL: Duration = Duration::from_millis(10);

/// The sentence `E2` carries into the artifact.
pub const SIMULATION_NOTE: &str = "FAITHFUL SIMULATION, recorded as such: there is no supported \
     way to install rift's hooks without initializing, because `rift init` does both — which is \
     the claim under test. This arm runs the real `rift init` through the real binary and then \
     removes the projection state the hook path would find, leaving the installed hooks in place. \
     It is not a user flow; it is the counterfactual the displacement claim is measured against.";

/// What one arm times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Act {
    /// A `git commit`, with nothing installed: the control.
    BareCommit,
    /// `rift init` itself — a wall time, not a git latency.
    Enable,
    /// The first commit in an enabled repository.
    FirstCommitAfterEnable,
    /// The first commit where enable has not paid the initialization.
    FirstCommitWithoutPriorEnable,
}

impl Act {
    /// Whether this act's number is a **git latency** — comparable with the
    /// bare-commit control and judged against the 100 ms budget.
    ///
    /// `Enable` is not: it is an absolute wall time for a different act, and
    /// subtracting a commit from it would produce a number with no meaning.
    #[must_use]
    pub const fn is_git_latency(self) -> bool {
        matches!(
            self,
            Self::BareCommit | Self::FirstCommitAfterEnable | Self::FirstCommitWithoutPriorEnable
        )
    }

    /// Whether this is the control every git-latency arm is quoted against.
    #[must_use]
    pub const fn is_control(self) -> bool {
        matches!(self, Self::BareCommit)
    }
}

/// One arm of the displacement sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arm {
    /// Stable identifier.
    pub id: String,
    /// What it exercises, in one line.
    pub title: String,
    /// What it times.
    pub act: Act,
    /// Anything that must be read with this arm's number.
    #[serde(default)]
    pub note: Option<String>,
}

/// The four arms, control first.
#[must_use]
pub fn arms() -> Vec<Arm> {
    vec![
        Arm {
            id: "A_bare".to_owned(),
            title: "first commit, nothing installed (control)".to_owned(),
            act: Act::BareCommit,
            note: None,
        },
        Arm {
            id: "E0_enable".to_owned(),
            title: "`rift init` itself — a wall time, not a git latency".to_owned(),
            act: Act::Enable,
            note: Some(
                "Never quoted against the bare-commit control: initializing a repository and \
                 committing to one are different acts, and a ratio between them would be a \
                 number wearing a unit it does not have."
                    .to_owned(),
            ),
        },
        Arm {
            id: "E1_commit_after_enable".to_owned(),
            title: "the first commit in a repository enabled beforehand".to_owned(),
            act: Act::FirstCommitAfterEnable,
            note: Some(
                "The arm the hard 100 ms git-latency budget binds: it is what a user's next \
                 commit costs."
                    .to_owned(),
            ),
        },
        Arm {
            id: "E2_commit_without_prior_enable".to_owned(),
            title: "the first commit with the projection state not yet there".to_owned(),
            act: Act::FirstCommitWithoutPriorEnable,
            note: Some(SIMULATION_NOTE.to_owned()),
        },
    ]
}

/// One arm's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmResult {
    /// The arm.
    pub arm: Arm,
    /// Timing, or `None` when the arm could not run.
    pub stats: Option<Stats>,
    /// Why it could not.
    pub error: Option<String>,
    /// What the machine was doing when this arm started.
    #[serde(default)]
    pub attestation: Option<crate::attest::Attestation>,
    /// Whether the commits this arm measured carried the checkpoint trailer —
    /// the oracle that says the hook path ran at all. A "fast" commit whose
    /// hooks never fired is not a fast commit.
    #[serde(default)]
    pub trailer_present: Option<bool>,
}

/// A whole displacement sweep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DisplacementReport {
    /// Artifact schema.
    pub schema_version: u32,
    /// Harness package version.
    pub harness_version: String,
    /// Unix seconds; provenance only.
    pub generated_at_unix: u64,
    /// Which scenario this artifact measures.
    pub scenario: String,
    /// Where it ran.
    pub host: Host,
    /// The rift binary measured, with its commit provenance.
    pub subject: LegStatus,
    /// The newest source in the checkout the subject came from.
    pub source_clock: Option<SourceClock>,
    /// Iteration counts.
    pub protocol: Protocol,
    /// The git-latency budget, milliseconds (owner-set, hard).
    pub budget_ms: u64,
    /// Results, control first.
    pub arms: Vec<ArmResult>,
}

impl DisplacementReport {
    /// The control's timing.
    #[must_use]
    pub fn control(&self) -> Option<&Stats> {
        self.arms
            .iter()
            .find(|a| a.arm.act.is_control())
            .and_then(|a| a.stats.as_ref())
    }

    /// One arm's git latency: its median minus the control's, nanoseconds.
    ///
    /// `None` for `E0_enable` — it is not a git latency — and `None` without a
    /// control, never zero, which would read as free.
    #[must_use]
    pub fn latency_ns(&self, arm: &ArmResult) -> Option<u64> {
        if !arm.arm.act.is_git_latency() || arm.arm.act.is_control() {
            return None;
        }
        let control = self.control()?;
        Some(arm.stats?.p50_ns.saturating_sub(control.p50_ns))
    }

    /// The measured displacement: `E2 - E1`, nanoseconds.
    ///
    /// **Signed on purpose, and `None` rather than zero when either arm is
    /// missing.** If the two arms land together the answer is "no cost was at
    /// commit time", which is a finding; a saturating subtraction would render
    /// that as a confident zero.
    #[must_use]
    pub fn displaced_ns(&self) -> Option<i128> {
        let find = |act: Act| {
            self.arms
                .iter()
                .find(|a| a.arm.act == act)
                .and_then(|a| a.stats)
        };
        let after = find(Act::FirstCommitAfterEnable)?;
        let without = find(Act::FirstCommitWithoutPriorEnable)?;
        Some(i128::from(without.p50_ns) - i128::from(after.p50_ns))
    }

    /// The file stem a sweep is written under.
    #[must_use]
    pub fn stem(&self) -> String {
        format!("enable-displacement-{}", self.generated_at_unix)
    }

    /// Write `<dir>/<stem>.json`.
    ///
    /// # Errors
    /// [`BenchError::Io`] / [`BenchError::Json`] on failure.
    pub fn write_json(&self, dir: &Path) -> Result<std::path::PathBuf> {
        fs::create_dir_all(dir).map_err(|e| BenchError::io("create report dir", e))?;
        let path = dir.join(format!("{}.json", self.stem()));
        let body = serde_json::to_vec_pretty(self).map_err(|e| BenchError::json("report", e))?;
        fs::write(&path, body).map_err(|e| BenchError::io("write report", e))?;
        Ok(path)
    }

    /// Write `<dir>/<stem>.md`.
    ///
    /// # Errors
    /// [`BenchError::Io`] on failure.
    pub fn write_markdown(&self, dir: &Path) -> Result<std::path::PathBuf> {
        fs::create_dir_all(dir).map_err(|e| BenchError::io("create report dir", e))?;
        let path = dir.join(format!("{}.md", self.stem()));
        fs::write(&path, self.to_markdown()).map_err(|e| BenchError::io("write report", e))?;
        Ok(path)
    }

    /// Render the sweep.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let ms = crate::report::ms;
        let mut out = format!(
            "# Enable-time displacement\n\n\
             - harness `{}`, artifact schema {}, generated {}\n\
             - host: {} / {}\n- protocol: {} warmup + {} measured, **a fresh repository per \
             iteration** (a first commit exists once in a repository's life)\n\
             - budget: **{} ms** of git latency on the first-commit arm (owner-set, hard)\n\n\
             The claim under test is **displacement, not speed**: the owner moved projection \
             init to enable time, and one order cannot tell a cost that moved from a cost that \
             never existed. Both orders are measured here, in this run.\n\n\
 | arm | p50 | git latency | verdict |\n|---|---|---|---|\n",
            self.harness_version,
            self.schema_version,
            self.generated_at_unix,
            self.host.os,
            self.host.arch,
            self.protocol.warmup,
            self.protocol.iterations,
            self.budget_ms,
        );
        for arm in &self.arms {
            let Some(stats) = arm.stats else {
                out.push_str(&format!(
                    "| `{}` | — | — | FAILED — {} |\n",
                    arm.arm.id,
                    arm.error.as_deref().unwrap_or("no samples")
                ));
                continue;
            };
            let (latency, verdict) = match self.latency_ns(arm) {
                Some(ns) => (
                    ms(ns),
                    if ns > self.budget_ms.saturating_mul(1_000_000) {
                        "**OVER the 100 ms budget**".to_owned()
                    } else {
                        "within budget".to_owned()
                    },
                ),
                None if arm.arm.act.is_control() => ("—".to_owned(), "control".to_owned()),
                None => (
                    "n/a".to_owned(),
                    "not a git latency — an absolute wall time".to_owned(),
                ),
            };
            let load = arm
                .attestation
                .map_or_else(|| "load1 unknown".to_owned(), |a| a.describe());
            out.push_str(&format!(
                "| `{}` | {} | {latency} | {verdict} · {load} |\n",
                arm.arm.id,
                ms(stats.p50_ns)
            ));
        }
        match self.displaced_ns() {
            Some(ns) if ns > 0 => out.push_str(&format!(
                "\n**Displaced: {} of first-commit cost is not paid when enable paid it.** \
                 That is `E2 - E1`, both measured in this run.\n",
                ms(u64::try_from(ns).unwrap_or(0))
            )),
            Some(ns) => out.push_str(&format!(
                "\n**No displacement measured: `E2 - E1` is {ns} ns, at or below zero.** The two \
                 orders cost the same, so nothing was moved off the first commit — reported \
                 because an instrument that can only confirm its claim is not an instrument.\n"
            )),
            None => out.push_str(
                "\n**Displacement not measurable in this run**: an arm is missing, and an \
                 absent difference is not a zero one.\n",
            ),
        }
        for arm in &self.arms {
            if let Some(note) = &arm.arm.note {
                out.push_str(&format!("\n- **`{}`** — {note}\n", arm.arm.id));
            }
        }
        out.push_str(&format!("\n{}\n", crate::hooklat::MACHINE_CLASS_NOTE));
        out
    }
}

/// Drive one arm.
#[must_use]
pub fn run_arm(
    hermetic: &Hermetic,
    binary: &Path,
    arm: &Arm,
    root: &Path,
    protocol: Protocol,
) -> ArmResult {
    match drive(hermetic, binary, arm, root, protocol) {
        Ok(result) => result,
        Err(e) => ArmResult {
            arm: arm.clone(),
            stats: None,
            error: Some(e.to_string()),
            attestation: Some(crate::attest::Attestation::take()),
            trailer_present: None,
        },
    }
}

fn drive(
    hermetic: &Hermetic,
    binary: &Path,
    arm: &Arm,
    root: &Path,
    protocol: Protocol,
) -> Result<ArmResult> {
    let attestation = Some(crate::attest::Attestation::take());
    let mut last_repo = None;
    let mut env_of_last = Vec::new();
    let mut iteration = |i| -> Result<Sample> {
        // A fresh repository per iteration, sandbox included: "first commit"
        // and "not yet initialized" are both once-in-a-lifetime states, and a
        // shared cache root would leak one iteration's projection into the
        // next one's answer.
        let sandbox = root.join(&arm.id).join(format!("iter-{i}"));
        let repo = sandbox.join("repo");
        fs::create_dir_all(&repo).map_err(|e| BenchError::io("create arm repo", e))?;
        let env = scenario::leg_env(LegId::Rift, &sandbox);
        seed_repo(hermetic, &repo, &env)?;

        let inv = match arm.act {
            Act::BareCommit => commit_invocation(&repo, &env),
            Act::Enable => with_env(scenario::setup_plan(LegId::Rift, binary, &repo), &env),
            Act::FirstCommitAfterEnable => {
                enable(hermetic, binary, &repo, &env)?;
                commit_invocation(&repo, &env)
            }
            Act::FirstCommitWithoutPriorEnable => {
                enable(hermetic, binary, &repo, &env)?;
                // The counterfactual: the hooks stay installed, the projection
                // state the hook path would find does not. See `SIMULATION_NOTE`.
                let cache = sandbox.join("rift-cache");
                if cache.exists() {
                    fs::remove_dir_all(&cache)
                        .map_err(|e| BenchError::io("clear projection state", e))?;
                }
                commit_invocation(&repo, &env)
            }
        };
        let sample = run_once(hermetic, &inv)?.sample;

        // `post-commit` writes the queue entry before it spawns its detached
        // worker, so observing the queue become empty is a race-free completion
        // oracle. Waiting here is deliberately outside `sample`: the product's
        // contract moved condense/verify off git's critical path, and allowing
        // fourteen workers to overlap would make the harness charge that
        // excluded work to the next repository's commit.
        if matches!(
            arm.act,
            Act::FirstCommitAfterEnable | Act::FirstCommitWithoutPriorEnable
        ) {
            wait_for_detached_capture(&repo, WORKER_QUIESCE_TIMEOUT)?;
        }

        last_repo = Some(repo);
        env_of_last.clone_from(&env);
        Ok(sample)
    };
    for i in 0..protocol.warmup {
        let _ = iteration(i)?;
    }
    let mut samples = Vec::with_capacity(protocol.iterations);
    for i in 0..protocol.iterations {
        samples.push(iteration(protocol.warmup + i)?);
    }

    // The oracle, from the last iteration's own repository: did the hook path
    // run at all? A commit that got fast by not firing its hooks would be
    // indistinguishable in the timing alone.
    let trailer_present = match (&last_repo, arm.act.is_control()) {
        (Some(repo), false) => git(hermetic, repo, &["log", "-1", "--format=%B"], &env_of_last)
            .ok()
            .map(|m| crate::hooklat::has_trailer(&m)),
        _ => None,
    };
    Ok(ArmResult {
        arm: arm.clone(),
        stats: Stats::of(&samples),
        error: None,
        attestation,
        trailer_present,
    })
}

/// Wait until one fresh repository's detached capture worker has finished.
///
/// The queue entry is durable before the worker is spawned, and the worker
/// removes it only after a successful drain. The lock is included so the next
/// iteration cannot begin in the tiny interval after the final entry is
/// removed but before the worker drops its exclusion guard.
fn wait_for_detached_capture(repo: &Path, timeout: Duration) -> Result<()> {
    let queue = repo.join(".git").join("rift-capture-queue");
    let started = Instant::now();
    loop {
        if !capture_worker_busy(&queue)? {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            return Err(BenchError::Spec(format!(
                "detached capture worker did not quiesce within {} ms for {}",
                timeout.as_millis(),
                repo.display()
            )));
        }
        std::thread::sleep(WORKER_QUIESCE_POLL);
    }
}

fn capture_worker_busy(queue: &Path) -> Result<bool> {
    let entries = match fs::read_dir(queue) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(BenchError::io("read capture queue", e)),
    };
    for entry in entries {
        let path = entry
            .map_err(|e| BenchError::io("read capture queue entry", e))?
            .path();
        if path.file_name().is_some_and(|name| name == ".lock")
            || path.extension().is_some_and(|ext| ext == "json")
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// A repository with one staged file, ready for its first commit.
fn seed_repo(hermetic: &Hermetic, repo: &Path, env: &[(String, String)]) -> Result<()> {
    git(hermetic, repo, &["init", "--quiet"], env)?;
    fs::write(repo.join("README.md"), "displacement arm\n")
        .map_err(|e| BenchError::io("seed file", e))?;
    // By name, never `git add -A`: on this machine a broad add resurrects paths
    // another agent deleted.
    git(hermetic, repo, &["add", "--", "README.md"], env)?;
    Ok(())
}

fn enable(hermetic: &Hermetic, binary: &Path, repo: &Path, env: &[(String, String)]) -> Result<()> {
    let init = with_env(scenario::setup_plan(LegId::Rift, binary, repo), env);
    let exec = run_once(hermetic, &init)?;
    if exec.sample.ok() {
        return Ok(());
    }
    Err(BenchError::Subprocess {
        program: binary.display().to_string(),
        args: init.args.join(" "),
        status: format!("{:?}", exec.sample.exit_code),
        stderr: exec.stderr.chars().take(400).collect(),
    })
}

fn commit_invocation(repo: &Path, env: &[(String, String)]) -> Invocation {
    with_env(
        Invocation::new("git", repo)
            .args(["commit", "--quiet", "-m", "first"])
            .env("GIT_TERMINAL_PROMPT", "0"),
        env,
    )
}

fn git(
    hermetic: &Hermetic,
    repo: &Path,
    args: &[&str],
    env: &[(String, String)],
) -> Result<String> {
    let inv = with_env(
        Invocation::new("git", repo).args(args.iter().map(|a| (*a).to_owned())),
        env,
    );
    let exec = run_once(hermetic, &inv)?;
    if exec.sample.ok() {
        Ok(exec.stdout)
    } else {
        Err(BenchError::Subprocess {
            program: "git".to_owned(),
            args: args.join(" "),
            status: format!("{:?}", exec.sample.exit_code),
            stderr: exec.stderr.chars().take(400).collect(),
        })
    }
}

fn with_env(mut inv: Invocation, env: &[(String, String)]) -> Invocation {
    for (k, v) in env {
        inv = inv.env(k.clone(), v.clone());
    }
    inv
}

/// Drive the sweep and produce the artifact.
///
/// # Errors
/// Only for failures that invalidate the whole sweep. A single arm's failure is
/// recorded on that arm.
pub fn run_sweep(
    hermetic: &Hermetic,
    resolver: &LegResolver,
    run_root: &Path,
    protocol: Protocol,
) -> Result<DisplacementReport> {
    fs::create_dir_all(run_root).map_err(|e| BenchError::io("create sweep root", e))?;
    let subject = resolver.resolve(hermetic, LegId::Rift);
    let arms = arms();
    let results = match subject.binary() {
        Some(binary) => arms
            .iter()
            .map(|arm| run_arm(hermetic, binary, arm, run_root, protocol))
            .collect(),
        None => arms
            .iter()
            .map(|arm| ArmResult {
                arm: arm.clone(),
                stats: None,
                error: Some("no subject binary; see the Subject section".to_owned()),
                attestation: None,
                trailer_present: None,
            })
            .collect(),
    };
    Ok(DisplacementReport {
        schema_version: DISPLACEMENT_SCHEMA,
        harness_version: env!("CARGO_PKG_VERSION").to_owned(),
        generated_at_unix: now_unix(),
        scenario: SCENARIO.to_owned(),
        host: Host::detect("n/a (displacement sweep)".to_owned()),
        subject,
        source_clock: resolver.source_clock.clone(),
        protocol,
        budget_ms: BUDGET_MS,
        arms: results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(p50: u64) -> Option<Stats> {
        Stats::of(&[crate::measure::Sample {
            wall_ns: p50,
            exit_code: Some(0),
        }])
    }

    fn report(bare: u64, enable_ns: u64, after: u64, without: u64) -> DisplacementReport {
        let value = |act: Act| match act {
            Act::BareCommit => bare,
            Act::Enable => enable_ns,
            Act::FirstCommitAfterEnable => after,
            Act::FirstCommitWithoutPriorEnable => without,
        };
        DisplacementReport {
            schema_version: DISPLACEMENT_SCHEMA,
            harness_version: "0.0.1".to_owned(),
            generated_at_unix: 1_800_000_000,
            scenario: SCENARIO.to_owned(),
            host: Host::detect("n/a".to_owned()),
            subject: LegStatus::Absent {
                reason: "fixture".to_owned(),
            },
            source_clock: None,
            protocol: Protocol::new(3, 11),
            budget_ms: BUDGET_MS,
            arms: arms()
                .into_iter()
                .map(|arm| ArmResult {
                    stats: stats(value(arm.act)),
                    error: None,
                    attestation: Some(crate::attest::Attestation {
                        load1: Some(1.65),
                        void_above: crate::attest::VOID_ABOVE_LOAD1,
                    }),
                    trailer_present: (!arm.act.is_control()).then_some(true),
                    arm,
                })
                .collect(),
        }
    }

    #[test]
    fn the_four_arms_are_the_two_orders_plus_enable_and_a_control() {
        let arms = arms();
        assert_eq!(arms.len(), 4);
        assert!(arms[0].act.is_control());
        let acts: Vec<Act> = arms.iter().map(|a| a.act).collect();
        assert_eq!(
            acts,
            [
                Act::BareCommit,
                Act::Enable,
                Act::FirstCommitAfterEnable,
                Act::FirstCommitWithoutPriorEnable
            ]
        );
        // The counterfactual arm says in the ARTIFACT that it is a simulation.
        let e2 = arms.last().unwrap();
        let note = e2.note.as_deref().unwrap();
        assert!(note.contains("FAITHFUL SIMULATION, recorded as such"));
        assert!(note.contains("is not a user flow"));
    }

    /// `E0_enable` is a wall time for a different act, and the type refuses to
    /// let it become a git latency. A ratio between initializing a repository
    /// and committing to one is a number wearing a unit it does not have.
    #[test]
    fn enable_is_never_quoted_as_a_git_latency() {
        let r = report(40_000_000, 300_000_000, 90_000_000, 160_000_000);
        let enable = r.arms.iter().find(|a| a.arm.act == Act::Enable).unwrap();
        assert!(!Act::Enable.is_git_latency());
        assert_eq!(r.latency_ns(enable), None);
        let md = r.to_markdown();
        assert!(
            md.contains("not a git latency — an absolute wall time"),
            "{md}"
        );
    }

    #[test]
    fn the_first_commit_arm_is_the_one_the_hard_budget_binds() {
        // 90 ms commit against a 40 ms bare control = 50 ms of git latency.
        let r = report(40_000_000, 300_000_000, 90_000_000, 160_000_000);
        let e1 = r
            .arms
            .iter()
            .find(|a| a.arm.act == Act::FirstCommitAfterEnable)
            .unwrap();
        assert_eq!(r.latency_ns(e1), Some(50_000_000));
        let md = r.to_markdown();
        assert!(md.contains("within budget"), "{md}");

        // The same arm over budget says so, in the words the budget uses.
        let over = report(40_000_000, 300_000_000, 190_000_000, 260_000_000);
        assert!(
            over.to_markdown().contains("**OVER the 100 ms budget**"),
            "{}",
            over.to_markdown()
        );
    }

    /// The displacement itself, and the case an instrument that can only
    /// confirm its own claim would hide: the two orders landing together.
    #[test]
    fn displacement_is_measured_and_its_absence_is_publishable() {
        let moved = report(40_000_000, 300_000_000, 90_000_000, 160_000_000);
        assert_eq!(moved.displaced_ns(), Some(70_000_000));
        assert!(
            moved.to_markdown().contains("Displaced: 70.00 ms"),
            "{}",
            moved.to_markdown()
        );

        // No displacement at all: E2 == E1.
        let flat = report(40_000_000, 300_000_000, 90_000_000, 90_000_000);
        assert_eq!(flat.displaced_ns(), Some(0));
        let md = flat.to_markdown();
        assert!(md.contains("No displacement measured"), "{md}");
        assert!(
            md.contains("nothing was moved off the first commit"),
            "{md}"
        );

        // And negative, which a saturating subtraction would have shown as
        // zero — a confident claim about a measurement that says the opposite.
        let backwards = report(40_000_000, 300_000_000, 160_000_000, 90_000_000);
        assert_eq!(backwards.displaced_ns(), Some(-70_000_000));
        assert!(backwards.to_markdown().contains("at or below zero"));

        // An absent arm is not a zero difference.
        let mut missing = moved.clone();
        missing
            .arms
            .retain(|a| a.arm.act != Act::FirstCommitWithoutPriorEnable);
        assert_eq!(missing.displaced_ns(), None);
        assert!(missing
            .to_markdown()
            .contains("an absent difference is not a zero one"));
    }

    #[test]
    fn the_artifact_round_trips_and_names_its_scenario_and_machine_class() {
        let r = report(40_000_000, 300_000_000, 90_000_000, 160_000_000);
        let json = serde_json::to_string(&r).unwrap();
        let back: DisplacementReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.scenario, "enable-displacement");
        assert_eq!(back.stem(), "enable-displacement-1800000000");
        // Every absolute carries the machine-class sentence.
        assert!(
            r.to_markdown().contains("one machine"),
            "{}",
            r.to_markdown()
        );
    }

    #[test]
    fn an_arm_that_could_not_run_is_a_failure_not_a_fast_number() {
        let mut r = report(40_000_000, 300_000_000, 90_000_000, 160_000_000);
        r.arms[2].stats = None;
        r.arms[2].error = Some("rift init exited 2".to_owned());
        let md = r.to_markdown();
        assert!(md.contains("FAILED — rift init exited 2"), "{md}");
        assert_eq!(r.displaced_ns(), None);
    }

    #[test]
    fn an_absent_capture_queue_is_already_quiescent() {
        let repo = tempfile::tempdir().unwrap();
        wait_for_detached_capture(repo.path(), Duration::ZERO).unwrap();
    }

    #[test]
    fn the_next_sample_waits_for_both_the_entry_and_worker_lock() {
        let repo = tempfile::tempdir().unwrap();
        let queue = repo.path().join(".git/rift-capture-queue");
        fs::create_dir_all(&queue).unwrap();
        let entry = queue.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json");
        let lock = queue.join(".lock");
        fs::write(&entry, b"queued").unwrap();
        fs::write(&lock, b"held").unwrap();

        let cleaner = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            fs::remove_file(entry).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            fs::remove_file(lock).unwrap();
        });
        wait_for_detached_capture(repo.path(), Duration::from_secs(1)).unwrap();
        cleaner.join().unwrap();
    }

    #[test]
    fn a_worker_that_never_quiesces_invalidates_the_arm() {
        let repo = tempfile::tempdir().unwrap();
        let queue = repo.path().join(".git/rift-capture-queue");
        fs::create_dir_all(&queue).unwrap();
        fs::write(
            queue.join("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.json"),
            b"queued",
        )
        .unwrap();

        let error = wait_for_detached_capture(repo.path(), Duration::ZERO).unwrap_err();
        assert!(
            error.to_string().contains("did not quiesce within 0 ms"),
            "{error}"
        );
    }
}
