//! The scenario driver: fixtures → sandboxes → samples → report.
//!
//! One pass per scenario per leg. Everything that is not the thing being measured
//! happens outside the timed region: materializing the fixture, copying it into
//! the leg's sandbox, and enabling capture in that copy.
//!
//! A leg never poisons the other. An absent binary, a verb the leg does not have,
//! a scenario that is not wired, a failed setup and a failed iteration all become
//! *recorded outcomes* on that leg alone; the other leg's numbers still land in
//! the report.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use crate::{
    fixture::{FixtureManifest, FixtureSet, FixtureSpec},
    hermetic::Hermetic,
    leg::{LegId, LegResolver, LegStatus},
    measure::{measure, run_once, Invocation, Protocol, Sample, Stats},
    report::{
        now_unix, Comparison, Host, IngestCheck, LegOutcome, LegSummary, Report, ScenarioReport,
        SpawnCount, REPORT_SCHEMA,
    },
    rss::RssProbe,
    scenario::{self, Availability, Isolation, Plan, PlanCtx, ScenarioId, ScenarioSpec},
    spawns::{self, SpawnProbe},
    BenchError, Result,
};

/// What a run covers.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Scenarios to drive, in report order.
    pub scenarios: Vec<ScenarioId>,
    /// Legs to measure.
    pub legs: Vec<LegId>,
    /// Override every scenario's iteration counts (CLI `--iterations`).
    pub protocol: Option<Protocol>,
    /// Take the extra untimed peak-RSS pass.
    pub probe_rss: bool,
    /// Take the extra untimed child-process pass, for the rows that ask.
    pub probe_spawns: bool,
}

/// Which untimed extra passes a leg's measurement takes. Kept as one value so
/// adding a probe does not widen every signature it travels through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probes {
    /// Peak resident set.
    pub rss: bool,
    /// Child processes launched.
    pub spawns: bool,
}

impl Default for RunOptions {
    fn default() -> Self {
        Self {
            scenarios: ScenarioId::all().to_vec(),
            legs: LegId::all().to_vec(),
            protocol: None,
            probe_rss: true,
            probe_spawns: true,
        }
    }
}

/// Drives the catalogue.
#[derive(Debug)]
pub struct Driver {
    hermetic: Hermetic,
    fixtures: FixtureSet,
    resolver: LegResolver,
    run_root: PathBuf,
    rss: RssProbe,
    spawns: Option<SpawnProbe>,
    /// The scripted-session driver, resolved once. Not a leg: never measured,
    /// never compared, and held here so the rows that need it can say why it is
    /// missing instead of quietly measuring something else.
    vogon: LegStatus,
}

/// Everything one leg of one scenario runs against, resolved once before
/// its first iteration.
struct LegCtx<'a> {
    spec: &'a ScenarioSpec,
    leg: LegId,
    binary: &'a Path,
    sandbox: &'a Path,
    fixture_repo: &'a Path,
    protocol: Protocol,
    env: &'a [(String, String)],
}

/// What the measured window leaves behind: its samples, the command line
/// the artifact records, the last invocation the untimed passes re-read,
/// and the event count taken once the warmup was behind it.
struct Window {
    samples: Vec<Sample>,
    command: String,
    last: Option<Invocation>,
    events_before: Option<u64>,
}

impl Driver {
    /// Prepare a driver whose sandboxes live under `run_root`.
    ///
    /// # Errors
    /// [`BenchError::Io`] if the run root cannot be prepared.
    pub fn new(
        run_root: impl Into<PathBuf>,
        fixtures: FixtureSet,
        resolver: LegResolver,
    ) -> Result<Self> {
        let run_root = run_root.into();
        fs::create_dir_all(&run_root).map_err(|e| BenchError::io("create run root", e))?;
        let hermetic = Hermetic::prepare(run_root.join("hermetic"))?;
        let rss = RssProbe::detect(&hermetic, &run_root);
        let spawns = SpawnProbe::prepare(&run_root, spawns::OBSERVED, None);
        let vogon = resolver.resolve_vogon(&hermetic);
        Ok(Self {
            hermetic,
            fixtures,
            resolver,
            run_root,
            rss,
            spawns,
            vogon,
        })
    }

    /// How peak RSS will be obtained on this host.
    #[must_use]
    pub fn rss_probe(&self) -> &RssProbe {
        &self.rss
    }

    /// How the scripted-session driver resolved (or why it did not).
    #[must_use]
    pub fn vogon(&self) -> &LegStatus {
        &self.vogon
    }

    /// How child processes will be counted on this host, if they can be.
    #[must_use]
    pub fn spawn_probe(&self) -> Option<&SpawnProbe> {
        self.spawns.as_ref()
    }

    /// Resolve both legs, with binary sizes.
    #[must_use]
    pub fn legs(&self) -> Vec<LegSummary> {
        LegId::all()
            .into_iter()
            .map(|id| {
                let status = self.resolver.resolve(&self.hermetic, id);
                let binary_bytes = status
                    .binary()
                    .and_then(|p| fs::metadata(p).ok())
                    .map(|m| m.len());
                LegSummary {
                    id,
                    status,
                    binary_bytes,
                }
            })
            .collect()
    }

    /// Drive `opts` and produce the report.
    ///
    /// # Errors
    /// Only for failures that invalidate the whole run (a fixture that cannot be
    /// built). Per-leg problems are recorded in the report, not returned.
    pub fn run(&self, opts: &RunOptions) -> Result<Report> {
        let legs = self.legs();
        let mut fixtures: Vec<FixtureManifest> = Vec::new();
        let mut scenarios = Vec::new();
        for id in &opts.scenarios {
            scenarios.push(self.scenario_report(*id, &legs, opts, &mut fixtures)?);
        }

        Ok(Report {
            schema_version: REPORT_SCHEMA,
            harness_version: env!("CARGO_PKG_VERSION").to_owned(),
            generated_at_unix: now_unix(),
            host: Host::detect(self.rss.describe()),
            source_clock: self.resolver.source_clock.clone(),
            attestation: Some(crate::attest::Attestation::take()),
            legs: reported_legs(legs),
            driver: self.reported_driver(opts),
            fixtures,
            scenarios,
        })
    }

    /// One scenario's row: every leg's outcome and, when both legs measured,
    /// their comparison. A fixture it builds joins `fixtures` once.
    fn scenario_report(
        &self,
        id: ScenarioId,
        legs: &[LegSummary],
        opts: &RunOptions,
        fixtures: &mut Vec<FixtureManifest>,
    ) -> Result<ScenarioReport> {
        let spec = scenario::spec(id);
        // A scenario that is not wired builds nothing: no fixture, no sandbox,
        // no binary needed. Declaring future work must stay free.
        if let Availability::Planned(note) = spec.availability {
            return Ok(planned_report(&spec, &opts.legs, note));
        }
        // Decide what *cannot* run before building anything: with no leg left
        // to measure there is no reason to spend minutes generating a fixture.
        let preflight: Vec<(LegId, Option<LegOutcome>)> = opts
            .legs
            .iter()
            .map(|leg| {
                let status = status_of(legs, *leg);
                (*leg, preflight(&spec, *leg, status, self.vogon.binary()))
            })
            .collect();

        let protocol = opts.protocol.unwrap_or(spec.protocol);
        let outcomes = if preflight.iter().all(|(_, skip)| skip.is_some()) {
            skipped_outcomes(preflight)
        } else {
            self.measured_outcomes(&spec, legs, preflight, protocol, opts, fixtures)?
        };
        let comparison = match (
            outcomes.get(LegId::Rift.as_str()),
            outcomes.get(LegId::Reference.as_str()),
        ) {
            (Some(rift), Some(reference)) => Comparison::of(rift, reference),
            _ => None,
        };

        Ok(ScenarioReport {
            id,
            title: spec.title.to_owned(),
            fixture: spec.fixture.to_owned(),
            headline: spec.headline,
            wired: spec.is_wired(),
            protocol,
            budget: spec.budget,
            work_unit: spec.work_unit.map(str::to_owned),
            caveat: spec.caveat.map(str::to_owned),
            legs: outcomes,
            comparison,
        })
    }

    /// Build the scenario's fixture once, then measure every leg preflight
    /// left to run; a leg it skipped keeps its recorded reason.
    fn measured_outcomes(
        &self,
        spec: &ScenarioSpec,
        legs: &[LegSummary],
        preflight: Vec<(LegId, Option<LegOutcome>)>,
        protocol: Protocol,
        opts: &RunOptions,
        fixtures: &mut Vec<FixtureManifest>,
    ) -> Result<BTreeMap<String, LegOutcome>> {
        let fixture_spec = FixtureSpec::preset(spec.fixture).ok_or_else(|| {
            BenchError::Spec(format!(
                "scenario {} names no known fixture",
                spec.id.as_str()
            ))
        })?;
        let fixture = self.fixtures.materialize(&fixture_spec)?;
        if !fixtures
            .iter()
            .any(|m| m.spec_digest == fixture.manifest.spec_digest)
        {
            fixtures.push(fixture.manifest.clone());
        }
        let probes = Probes {
            rss: opts.probe_rss,
            spawns: opts.probe_spawns,
        };
        let mut outcomes = BTreeMap::new();
        for (leg, skip) in preflight {
            let outcome = skip.unwrap_or_else(|| {
                let status = status_of(legs, leg);
                self.measure_leg(spec, leg, status, &fixture.repo, protocol, probes)
            });
            outcomes.insert(leg.as_str().to_owned(), outcome);
        }
        Ok(outcomes)
    }

    /// The scripted-session driver as the report records it.
    ///
    /// Recorded whenever a driven row was asked for, present or not: an
    /// absent driver is why a row was skipped, which is exactly the sort of
    /// thing a report must not leave to inference.
    fn reported_driver(&self, opts: &RunOptions) -> Option<LegStatus> {
        opts.scenarios
            .iter()
            .any(|id| scenario::spec(*id).needs_driver())
            .then(|| crate::leg::reported(self.vogon.clone(), &crate::leg::default_repo_root()))
    }

    /// Drive one leg of one scenario. Never returns an error: every failure mode
    /// is a recorded outcome for this leg.
    fn measure_leg(
        &self,
        spec: &ScenarioSpec,
        leg: LegId,
        leg_status: Option<&LegStatus>,
        fixture_repo: &Path,
        protocol: Protocol,
        probes: Probes,
    ) -> LegOutcome {
        if let Some(skip) = preflight(spec, leg, leg_status, self.vogon.binary()) {
            return skip;
        }
        let Some(LegStatus::Present { binary, .. }) = leg_status else {
            return skipped(format!("leg `{}` was not resolved", leg.as_str()));
        };

        let sandbox = self.run_root.join(spec.id.as_str()).join(leg.as_str());
        if let Err(e) = reset_dir(&sandbox) {
            return failed(e.to_string(), None);
        }
        let env = scenario::leg_env(leg, &sandbox);
        let ctx = LegCtx {
            spec,
            leg,
            binary,
            sandbox: &sandbox,
            fixture_repo,
            protocol,
            env: &env,
        };
        let owes_proof = self.owes_ingest_proof(spec, leg, binary);

        let window = match self.timed_window(&ctx, owes_proof) {
            Ok(window) => window,
            Err(e) => return failed(e.to_string(), None),
        };
        let Some(stats) = Stats::of(&window.samples) else {
            return failed("no samples were collected".to_owned(), None);
        };
        if !stats.clean() {
            return failed(
                self.failure_reason(window.last.as_ref(), &stats),
                Some(stats),
            );
        }
        // What the timing was a timing *of*, and whether the verb answered.
        // Untimed, and after the samples, so it observes the same repository
        // state they ran against.
        let stats = match self.after_window(&ctx, stats, window.last.as_ref()) {
            Ok(stats) => stats,
            Err(reason) => return failed(reason, Some(stats)),
        };
        let ingest = match self.ingest_of(&ctx, owes_proof, &window, &stats) {
            Ok(ingest) => ingest,
            Err(reason) => return failed(reason, Some(stats)),
        };

        let (peak_rss_bytes, child_spawns) =
            self.untimed_probes(&ctx, probes, window.last.as_ref());
        LegOutcome::Measured {
            command: window.command,
            stats,
            peak_rss_bytes,
            ingest,
            child_spawns,
        }
    }

    /// The measured window. Everything per iteration except the verb itself —
    /// the repository copy, the previous iteration's detached work, the
    /// event count and the prelude — happens inside `prepare`, which
    /// `measure` runs outside the timer.
    fn timed_window(&self, ctx: &LegCtx<'_>, owes_proof: bool) -> Result<Window> {
        let shared = ctx.spec.isolation == Isolation::Shared;
        let mut command = String::new();
        let mut last: Option<Invocation> = None;
        // Read once the warmup is behind us, so the proof covers exactly the
        // published samples.
        let mut events_before: Option<u64> = None;

        let samples = measure(&self.hermetic, ctx.protocol, |i| {
            let repo = repo_path(ctx.sandbox, shared, i);
            self.ensure_repo(ctx.leg, ctx.binary, ctx.fixture_repo, &repo, ctx.env)?;
            let plan = PlanCtx {
                leg: ctx.leg,
                binary: ctx.binary,
                repo: &repo,
                sandbox: ctx.sandbox,
                iteration: i,
                tool: self.vogon.binary(),
            };
            // The previous iteration's detached work settles HERE, untimed,
            // before this iteration's prelude and verb.
            settle_detached(ctx.spec.id, &plan)?;
            if i == ctx.protocol.warmup && owes_proof {
                events_before = self.count_events(&plan, ctx.env);
            }
            self.run_prelude(ctx.spec.id, &plan, ctx.env)?;
            let Plan::Run(inv) = scenario::plan(ctx.spec.id, &plan) else {
                return Err(BenchError::Spec("no analogue".to_owned()));
            };
            let inv = with_env(*inv, ctx.env);
            command = sanitize_command(&inv.command_line(), ctx.sandbox);
            last = Some(inv.clone());
            Ok(inv)
        })?;
        Ok(Window {
            samples,
            command,
            last,
            events_before,
        })
    }

    /// The ingest proof this leg owes, over the window's last invocation;
    /// `Err` is the reason the measure is failed.
    fn ingest_of(
        &self,
        ctx: &LegCtx<'_>,
        owes_proof: bool,
        window: &Window,
        stats: &Stats,
    ) -> std::result::Result<Option<IngestCheck>, String> {
        if !owes_proof {
            // Nothing to prove, or no leg-side counter to prove it with — said
            // out loud in the report rather than assumed.
            return Ok(None);
        }
        let plan = window.last.as_ref().map(|inv| PlanCtx {
            leg: ctx.leg,
            binary: ctx.binary,
            repo: &inv.cwd,
            sandbox: ctx.sandbox,
            iteration: ctx.protocol.warmup + ctx.protocol.iterations,
            tool: self.vogon.binary(),
        });
        let per_iteration = scenario::durable_events_per_iteration(ctx.spec.id);
        let check = self.prove_ingest(
            plan.as_ref(),
            window.events_before,
            stats,
            per_iteration,
            ctx.env,
        )?;
        Ok(Some(check))
    }

    /// The two optional untimed passes: peak RSS of the last measured
    /// invocation, and — for the rows that ask — the child processes one
    /// invocation launches. Each is `None` when not asked for or not
    /// measurable, never zero.
    fn untimed_probes(
        &self,
        ctx: &LegCtx<'_>,
        probes: Probes,
        last: Option<&Invocation>,
    ) -> (Option<u64>, Option<SpawnCount>) {
        let peak_rss_bytes = if probes.rss {
            last.and_then(|inv| self.rss.peak_rss_bytes(&self.hermetic, inv))
        } else {
            None
        };
        let child_spawns = (probes.spawns && ctx.spec.spawn_probe)
            .then(|| self.count_spawns(ctx))
            .flatten();
        (peak_rss_bytes, child_spawns)
    }

    /// Count the child processes one invocation of this row's verb launches.
    ///
    /// The counted invocation is **one iteration past the measured window**,
    /// with its own repository state and its own untimed prelude. Re-running
    /// the last measured invocation instead — the way the RSS pass does — would
    /// re-deliver an event this session has already seen, and for the row this
    /// probe exists for that is fatal: `identity-attribution` binds a *new*
    /// session's identity, and a session that is already bound spawns nothing
    /// at all. The count would be a beautiful, meaningless zero.
    fn count_spawns(&self, ctx: &LegCtx<'_>) -> Option<SpawnCount> {
        let probe = self.spawns.as_ref()?;
        let iteration = ctx.protocol.warmup + ctx.protocol.iterations;
        let shared = ctx.spec.isolation == Isolation::Shared;
        let repo = repo_path(ctx.sandbox, shared, iteration);
        self.ensure_repo(ctx.leg, ctx.binary, ctx.fixture_repo, &repo, ctx.env)
            .ok()?;
        let plan = PlanCtx {
            leg: ctx.leg,
            binary: ctx.binary,
            repo: &repo,
            sandbox: ctx.sandbox,
            iteration,
            tool: self.vogon.binary(),
        };
        self.run_prelude(ctx.spec.id, &plan, ctx.env).ok()?;
        let Plan::Run(inv) = scenario::plan(ctx.spec.id, &plan) else {
            return None;
        };
        probe.count(&self.hermetic, &with_env(*inv, ctx.env))
    }

    /// Why a dirty measure failed, with the leg's own last words: the invocation
    /// is re-run once *after* measurement purely to capture its stderr, so the
    /// reader learns what went wrong instead of only that something did.
    fn failure_reason(&self, last: Option<&Invocation>, stats: &Stats) -> String {
        let detail = last
            .and_then(|inv| run_once(&self.hermetic, inv).ok())
            .map(|exec| trim(&exec.stderr))
            .unwrap_or_default();
        format!(
            "{} of {} iterations exited non-zero: {detail}",
            stats.failures, stats.samples
        )
    }

    /// Materialize the repository copy this iteration runs in, once, with
    /// capture enabled. All of it is outside the timed region.
    fn ensure_repo(
        &self,
        leg: LegId,
        binary: &Path,
        fixture_repo: &Path,
        repo: &Path,
        env: &[(String, String)],
    ) -> Result<()> {
        if repo.exists() {
            return Ok(());
        }
        copy_dir(fixture_repo, repo)?;
        let setup = with_env(scenario::setup_plan(leg, binary, repo), env);
        let exec = run_once(&self.hermetic, &setup)?;
        if exec.sample.ok() {
            return Ok(());
        }
        Err(BenchError::Subprocess {
            program: binary.display().to_string(),
            args: setup.args.join(" "),
            status: format!("{:?}", exec.sample.exit_code),
            stderr: trim(&exec.stderr),
        })
    }

    /// Untimed per-iteration preparation, run step by step in the order the
    /// scenario declared: hooks that put the session where the timed verb
    /// finds it in life, files written where the workload writes them (a
    /// transcript last, so it is as fresh as a live agent's), commits that
    /// land a checkpoint the verb resolves.
    fn run_prelude(
        &self,
        id: ScenarioId,
        ctx: &PlanCtx<'_>,
        env: &[(String, String)],
    ) -> Result<()> {
        for step in scenario::prelude(id, ctx).steps {
            match step {
                scenario::Step::Invoke(inv) => {
                    run_once(&self.hermetic, &with_env(inv, env))?;
                }
                scenario::Step::Write(path, body) => {
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)
                            .map_err(|e| BenchError::io("create prelude dir", e))?;
                    }
                    fs::write(&path, body).map_err(|e| BenchError::io("write prelude file", e))?;
                }
            }
        }
        Ok(())
    }

    /// Did the work the number claims to time actually become durable? A hook
    /// that ingests nothing exits just as fast and would publish a beautiful
    /// figure for a no-op.
    ///
    /// The floor is `per_iteration × samples`, counting the untimed prelude's
    /// own events as well as the timed verb's: a row whose prelude ingests
    /// three events per iteration clears a floor of one *without the timed verb
    /// writing anything at all*, which is precisely the case this check exists
    /// to catch.
    ///
    /// `Err` carries the reason the measure is disqualified. An unreadable
    /// counter is a *failed* proof, not an absent one: silence there is exactly
    /// how a measure of nothing would slip through wearing a number.
    fn prove_ingest(
        &self,
        ctx: Option<&PlanCtx<'_>>,
        events_before: Option<u64>,
        stats: &Stats,
        per_iteration: u64,
        env: &[(String, String)],
    ) -> std::result::Result<IngestCheck, String> {
        let unreadable = || {
            "the leg's event counter could not be read, so nothing proves the timed verb \
             wrote anything"
                .to_owned()
        };
        let (Some(ctx), Some(before)) = (ctx, events_before) else {
            return Err(unreadable());
        };
        let after = self.count_events(ctx, env).ok_or_else(unreadable)?;
        let check = IngestCheck {
            events_before: before,
            events_after: after,
            required_growth: per_iteration.saturating_mul(stats.samples as u64),
        };
        if check.verified() {
            Ok(check)
        } else {
            Err(format!(
                "{} measured iterations grew the projection by {} events, short of the {} \
                 they owe ({per_iteration} each): the timed verb is not reaching durable \
                 storage",
                stats.samples,
                after.saturating_sub(before),
                check.required_growth,
            ))
        }
    }

    /// Whether this leg owes proof of ingest at all: a measure that writes
    /// nothing has none to give, and a leg with no counter verb cannot be
    /// asked. Both are recorded rather than assumed.
    fn owes_ingest_proof(&self, spec: &ScenarioSpec, leg: LegId, binary: &Path) -> bool {
        spec.requires_ingest()
            && scenario::ingest_probe(&PlanCtx {
                leg,
                binary,
                tool: self.vogon.binary(),
                repo: Path::new(""),
                sandbox: Path::new(""),
                iteration: 0,
            })
            .is_some()
    }

    /// The untimed steps after the measured window, over the same repository
    /// state the samples ran against: attach the probed work count, then ask
    /// whether the verb ANSWERED. For the rows whose wall time means nothing
    /// without their answer (a `session resume` that restored nothing, a
    /// `why` that resolved no checkpoint) one re-run decides; `Err` is the
    /// reason the measure is failed, with its timings inside the failure. An
    /// unrunnable or unparsable probe is a failed answer, not an absent one —
    /// silence is how a refusal would publish as a latency.
    fn after_window(
        &self,
        leg_ctx: &LegCtx<'_>,
        stats: Stats,
        last: Option<&Invocation>,
    ) -> std::result::Result<Stats, String> {
        let Some(last) = last else {
            return Ok(stats);
        };
        let (id, protocol, env) = (leg_ctx.spec.id, leg_ctx.protocol, leg_ctx.env);
        let ctx = PlanCtx {
            leg: leg_ctx.leg,
            binary: leg_ctx.binary,
            repo: &last.cwd,
            sandbox: leg_ctx.sandbox,
            iteration: protocol.warmup + protocol.iterations,
            tool: self.vogon.binary(),
        };
        // The last iteration's detached work settles before anything reads
        // the repository state it leaves behind.
        settle_detached(id, &ctx).map_err(|e| e.to_string())?;
        let stats = self
            .count_work_units(id, &ctx, env)
            .map_or(stats, |n| stats.with_units(n));
        // The answer is asked of the LAST MEASURED iteration — the question
        // its verb asked, over the state its prelude built — not of the
        // iteration past the window, whose prelude never ran and whose file
        // was never written. (The work probe above is different: it counts
        // repository state, which the last iteration left behind.)
        let asked = PlanCtx {
            iteration: (protocol.warmup + protocol.iterations).saturating_sub(1),
            ..ctx
        };
        // Except where the verb answers a question only once. Asking the last
        // measured iteration's question again would then read the SECOND
        // answer — the opposite of what the samples were paid for — so the
        // row's own untimed prelude is replayed one iteration past the window
        // and the probe asks a question nothing has answered yet.
        let asked = if scenario::probe_replays_the_prelude(id) {
            let fresh = PlanCtx {
                iteration: protocol.warmup + protocol.iterations,
                ..ctx
            };
            self.run_prelude(id, &fresh, env)
                .map_err(|e| format!("the answer probe's prelude could not run: {e}"))?;
            fresh
        } else {
            asked
        };
        let Some(probe) = scenario::answer_probe(id, &asked) else {
            return Ok(stats);
        };
        let exec = run_once(&self.hermetic, &with_env(probe, env))
            .map_err(|e| format!("the answer probe could not run: {e}"))?;
        if !exec.sample.ok() {
            return Err(format!(
                "the answer probe exited {:?}: {}",
                exec.sample.exit_code,
                trim(&exec.stderr)
            ));
        }
        match scenario::parse_answer(id, &exec.stdout) {
            Some(Ok(())) | None => Ok(stats),
            Some(Err(reason)) => Err(reason),
        }
    }

    /// How much work one measured iteration performed, from the row's own
    /// untimed probe. `None` when the row has no probe or the probe failed —
    /// which is published as absent, never as zero.
    fn count_work_units(
        &self,
        id: ScenarioId,
        ctx: &PlanCtx<'_>,
        env: &[(String, String)],
    ) -> Option<u64> {
        let inv = with_env(scenario::work_probe(id, ctx)?, env);
        let exec = run_once(&self.hermetic, &inv).ok()?;
        scenario::parse_work_units(id, &exec.stdout)
    }

    /// The leg's own count of durably captured events, or `None` when the leg
    /// publishes no such counter (or the probe itself failed).
    fn count_events(&self, ctx: &PlanCtx<'_>, env: &[(String, String)]) -> Option<u64> {
        let inv = with_env(scenario::ingest_probe(ctx)?, env);
        let exec = run_once(&self.hermetic, &inv).ok()?;
        exec.sample
            .ok()
            .then(|| scenario::captured_events(&exec.stdout))
            .flatten()
    }
}

/// The legs as the ARTIFACT records them: binaries relative to the checkout
/// root.
///
/// Relativized HERE and not in [`Driver::legs`]: the resolver's absolute
/// paths are what `run` executes, and one measurement sitting proved it the
/// hard way — a sanitized `legs()` launched nothing (`cannot launch
/// target/release/rift`) while three runs recorded refusals. Only the RECORD
/// leaving the process is census-shed.
fn reported_legs(legs: Vec<LegSummary>) -> Vec<LegSummary> {
    let root = crate::leg::default_repo_root();
    legs.into_iter()
        .map(|l| LegSummary {
            id: l.id,
            status: crate::leg::reported(l.status, &root),
            binary_bytes: l.binary_bytes,
        })
        .collect()
}

/// The command line as the ARTIFACT records it: the sandbox and the checkout
/// root replaced by placeholders.
///
/// The executed invocation is untouched — this rewrites the display string
/// only. A recorded absolute sandbox path is one census token per artifact
/// (`/` is in the entropy layer's token class), and whether it crosses the
/// 4.0-bit floor depends on the machine's directory names; `<sandbox>` and
/// `<repo>` split the token at characters outside the class and say more.
fn sanitize_command(command: &str, sandbox: &Path) -> String {
    command
        .replace(&sandbox.display().to_string(), "<sandbox>")
        .replace(
            &crate::leg::default_repo_root().display().to_string(),
            "<repo>",
        )
}

fn skipped(reason: String) -> LegOutcome {
    LegOutcome::Skipped { reason }
}

/// The reason this leg cannot run, decided before any fixture is built:
/// the scenario is not wired, the binary is absent, or the leg has no analogue
/// for the verb. `None` means "this one has to actually run".
fn preflight(
    spec: &ScenarioSpec,
    leg: LegId,
    leg_status: Option<&LegStatus>,
    tool: Option<&Path>,
) -> Option<LegOutcome> {
    if let Availability::Planned(note) = spec.availability {
        return Some(skipped(format!("scenario not wired yet: {note}")));
    }
    let binary = match leg_status {
        Some(LegStatus::Present { binary, .. }) => binary,
        Some(LegStatus::Absent { reason }) => return Some(skipped(reason.clone())),
        None => {
            return Some(skipped(format!("leg `{}` was not resolved", leg.as_str())));
        }
    };
    // Neither the repository nor the iteration affects whether an analogue
    // exists — only the leg and the verb do.
    let ctx = PlanCtx {
        leg,
        binary,
        repo: Path::new(""),
        sandbox: Path::new(""),
        iteration: 0,
        tool,
    };
    match scenario::plan(spec.id, &ctx) {
        Plan::NoAnalogue(note) => Some(skipped(note.to_owned())),
        Plan::Run(_) => None,
    }
}

/// The resolved status of `leg`, if the resolver listed it.
fn status_of(legs: &[LegSummary], leg: LegId) -> Option<&LegStatus> {
    legs.iter().find(|l| l.id == leg).map(|l| &l.status)
}

/// The outcomes of a scenario whose every leg preflight skipped.
fn skipped_outcomes(preflight: Vec<(LegId, Option<LegOutcome>)>) -> BTreeMap<String, LegOutcome> {
    let mut outcomes = BTreeMap::new();
    for (leg, skip) in preflight {
        if let Some(outcome) = skip {
            outcomes.insert(leg.as_str().to_owned(), outcome);
        }
    }
    outcomes
}

/// The report row for a declared-but-not-wired scenario.
fn planned_report(spec: &ScenarioSpec, legs: &[LegId], note: &str) -> ScenarioReport {
    let outcomes = legs
        .iter()
        .map(|leg| {
            (
                leg.as_str().to_owned(),
                skipped(format!("scenario not wired yet: {note}")),
            )
        })
        .collect();
    ScenarioReport {
        id: spec.id,
        title: spec.title.to_owned(),
        fixture: spec.fixture.to_owned(),
        headline: spec.headline,
        wired: false,
        protocol: spec.protocol,
        budget: spec.budget,
        work_unit: spec.work_unit.map(str::to_owned),
        caveat: spec.caveat.map(str::to_owned),
        legs: outcomes,
        comparison: None,
    }
}

fn failed(reason: String, stats: Option<Stats>) -> LegOutcome {
    LegOutcome::Failed { reason, stats }
}

/// Where one iteration's repository copy lives: shared rows reuse a single
/// prepared repository, cold rows get their own.
/// How long a row's detached work may take to drain before the measure is
/// reported failed. Generous against a worker's real cost (a condense is
/// hundreds of milliseconds); tight against a worker that never ran.
const DETACHED_SETTLE: Duration = Duration::from_secs(30);

/// Wait, untimed, for the row's detached agent-hook journal to drain
/// (`scenario::detached_journal`): no `*.json` job left, or no journal at
/// all. A journal that has not drained within [`DETACHED_SETTLE`] fails the
/// measure — the alternative is publishing the latency of a hook whose work
/// never happened.
fn settle_detached(id: ScenarioId, ctx: &PlanCtx<'_>) -> Result<()> {
    let Some(journal) = scenario::detached_journal(id, ctx) else {
        return Ok(());
    };
    let pending = || -> usize {
        fs::read_dir(&journal).map_or(0, |rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
                .count()
        })
    };
    let deadline = Instant::now() + DETACHED_SETTLE;
    while pending() > 0 {
        if Instant::now() >= deadline {
            return Err(BenchError::Spec(format!(
                "{} job(s) still in the detached worker's journal after {:?}: the row's \
                 deferred work did not land, so its latency would be a latency of nothing",
                pending(),
                DETACHED_SETTLE
            )));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

fn repo_path(sandbox: &Path, shared: bool, iteration: usize) -> PathBuf {
    if shared {
        sandbox.join("repo")
    } else {
        sandbox.join(format!("iter-{iteration}"))
    }
}

fn with_env(mut inv: Invocation, env: &[(String, String)]) -> Invocation {
    inv.env.extend(env.iter().cloned());
    inv
}

fn trim(stderr: &str) -> String {
    stderr.trim().chars().take(240).collect()
}

/// Remove `dir` if present and recreate it empty.
fn reset_dir(dir: &Path) -> Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|e| BenchError::io("clear sandbox", e))?;
    }
    fs::create_dir_all(dir).map_err(|e| BenchError::io("create sandbox", e))
}

/// Recursively copy `src` to `dst` (regular files and directories only — the
/// generated fixtures contain nothing else).
///
/// # Errors
/// [`BenchError::Io`] on any filesystem failure.
pub fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(|e| BenchError::io("create copy target", e))?;
    let entries = fs::read_dir(src).map_err(|e| BenchError::io("read fixture", e))?;
    for entry in entries {
        let entry = entry.map_err(|e| BenchError::io("read fixture entry", e))?;
        let kind = entry
            .file_type()
            .map_err(|e| BenchError::io("stat fixture entry", e))?;
        let target = dst.join(entry.file_name());
        if kind.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else if kind.is_file() {
            fs::copy(entry.path(), &target).map_err(|e| BenchError::io("copy fixture file", e))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn driver(root: &Path) -> Driver {
        let resolver = LegResolver {
            rift_override: None,
            reference_override: None,
            vogon_override: None,
            repo_root: root.join("repo-root"),
            path_var: None,
            install_root: None,
            allow_debug: false,
            source_clock: None,
            allow_stale: false,
            git_provenance: None,
        };
        Driver::new(
            root.join("runs"),
            FixtureSet::new(root.join("fixtures")),
            resolver,
        )
        .unwrap()
    }

    #[test]
    fn copy_dir_reproduces_a_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        fs::create_dir_all(src.join("a/b")).unwrap();
        fs::write(src.join("a/b/f.txt"), b"hello").unwrap();
        let dst = tmp.path().join("dst");
        copy_dir(&src, &dst).unwrap();
        assert_eq!(fs::read(dst.join("a/b/f.txt")).unwrap(), b"hello");
    }

    #[test]
    fn reset_dir_starts_from_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("sandbox");
        fs::create_dir_all(dir.join("stale")).unwrap();
        reset_dir(&dir).unwrap();
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
    }

    #[test]
    fn with_no_binaries_every_scenario_is_skipped_with_a_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let driver = driver(tmp.path());
        let opts = RunOptions {
            scenarios: vec![ScenarioId::ColdStatus],
            protocol: Some(Protocol::smoke()),
            probe_rss: false,
            ..RunOptions::default()
        };
        let report = driver.run(&opts).unwrap();

        assert_eq!(report.schema_version, REPORT_SCHEMA);
        assert!(
            report.fixtures.is_empty(),
            "with nothing to measure, nothing is generated"
        );
        let scenario = &report.scenarios[0];
        assert!(scenario.comparison.is_none(), "no legs, no head-to-head");
        for leg in LegId::all() {
            let outcome = &scenario.legs[leg.as_str()];
            let LegOutcome::Skipped { reason } = outcome else {
                panic!("expected a skip for {}: {outcome:?}", leg.as_str())
            };
            assert!(!reason.is_empty());
        }
        // Both legs absent is a state the report can render.
        assert!(driver.run(&opts).unwrap().to_markdown().contains("absent"));
    }

    /// A stand-in leg binary: exits 0 for every verb and writes nothing durable.
    /// It is exactly the failure the ingest proof exists to catch — a hook that
    /// returns beautifully fast because it did nothing at all.
    #[cfg(unix)]
    fn silent_binary(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("silent-leg");
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn a_hook_measure_that_cannot_prove_ingest_is_failed_not_published() {
        let tmp = tempfile::tempdir().unwrap();
        let mut driver = driver(tmp.path());
        driver.resolver.rift_override = Some(silent_binary(tmp.path()));

        let opts = RunOptions {
            scenarios: vec![ScenarioId::HookRoundTrip],
            legs: vec![LegId::Rift],
            protocol: Some(Protocol::new(1, 2)),
            probe_rss: false,
            probe_spawns: false,
        };
        let report = driver.run(&opts).unwrap();
        let outcome = &report.scenarios[0].legs["rift"];
        let LegOutcome::Failed { reason, stats } = outcome else {
            panic!("a hook that ingests nothing is not a fast hook: {outcome:?}")
        };
        assert!(reason.contains("counter could not be read"), "{reason}");
        // The timings are still published *inside the failure*, so the reader can
        // see how flattering the number would have been.
        assert_eq!(stats.map(|s| s.samples), Some(2));
        assert!(report.scenarios[0].budget_verdict_of().is_none());
        let md = report.to_markdown();
        assert!(md.contains("FAILED"), "{md}");
        assert!(!md.contains("**met**"), "{md}");
    }

    /// A leg that ingests every hook **except** the one being timed. Its
    /// projection still grows every iteration — from the prelude — so it clears
    /// the old "at least one event per iteration" floor while the measured verb
    /// writes nothing at all.
    #[cfg(unix)]
    fn prelude_only_binary(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;
        let path = dir.join("prelude-only-leg");
        fs::write(
            &path,
            "#!/bin/sh\n\
             mkdir -p \"$HOME\"\n\
             c=\"$HOME/events\"\n\
             case \"$1\" in\n\
             status) n=0; [ -f \"$c\" ] && n=$(wc -l < \"$c\" | tr -d ' '); \
             printf 'captured events: %s\\n' \"$n\" ;;\n\
             hooks) [ \"$3\" = PostToolUse ] || echo x >> \"$c\" ;;\n\
             esac\n\
             exit 0\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn a_prelude_cannot_pay_the_timed_verbs_share_of_the_ingest_floor() {
        let tmp = tempfile::tempdir().unwrap();
        let mut driver = driver(tmp.path());
        driver.resolver.rift_override = Some(prelude_only_binary(tmp.path()));

        // `hook-tool-result` owes two events per iteration: its prelude's
        // `PreToolUse` and the timed `PostToolUse`. This leg delivers only the
        // first, so growth is 2 where 4 is owed.
        let opts = RunOptions {
            scenarios: vec![ScenarioId::HookToolResult],
            legs: vec![LegId::Rift],
            protocol: Some(Protocol::new(1, 2)),
            probe_rss: false,
            probe_spawns: false,
        };
        let report = driver.run(&opts).unwrap();
        let outcome = &report.scenarios[0].legs["rift"];
        let LegOutcome::Failed { reason, .. } = outcome else {
            panic!("a timed verb that writes nothing is not a fast verb: {outcome:?}")
        };
        assert!(reason.contains("short of the 4 they owe"), "{reason}");
        assert!(reason.contains("(2 each)"), "{reason}");
    }

    /// The declared-but-not-driven STATE still exists and still costs
    /// nothing, even though no catalogue row is in it any more (the last
    /// two, `resume-restore` and `blame-why`, were wired on 2026-08-29): a
    /// hand-built `Planned` spec is skipped by `preflight` before a binary
    /// is even asked for, and the planned report row carries the note.
    #[test]
    fn planned_scenarios_are_skipped_before_any_binary_is_needed() {
        let planned = ScenarioSpec {
            availability: Availability::Planned("waits on a verb that does not exist yet"),
            ..scenario::spec(ScenarioId::ColdStatus)
        };
        let outcome = preflight(&planned, LegId::Rift, None, None)
            .expect("a planned spec never reaches a run");
        let LegOutcome::Skipped { reason } = outcome else {
            panic!("planned scenarios do not run: {outcome:?}")
        };
        assert!(reason.starts_with("scenario not wired yet"), "{reason}");
        assert!(reason.contains("does not exist yet"), "{reason}");
        // A present binary changes nothing: the state is decided first.
        let present = LegStatus::Present {
            binary: PathBuf::from("/bin/rift"),
            version: "test".to_owned(),
            source: "test".to_owned(),
            binary_mtime_unix: None,
            git_describe: None,
            dirty: None,
        };
        assert!(matches!(
            preflight(&planned, LegId::Rift, Some(&present), None),
            Some(LegOutcome::Skipped { .. })
        ));
        let row = planned_report(
            &planned,
            &[LegId::Rift],
            "waits on a verb that does not exist yet",
        );
        assert!(!row.wired);
        assert!(
            matches!(&row.legs["rift"], LegOutcome::Skipped { reason } if reason.contains("not wired yet"))
        );
    }

    #[test]
    fn the_rift_only_measure_skips_the_backend_leg_with_its_reason() {
        let tmp = tempfile::tempdir().unwrap();
        let driver = driver(tmp.path());
        let opts = RunOptions {
            scenarios: vec![ScenarioId::VerifyThroughput],
            legs: vec![LegId::Reference],
            protocol: Some(Protocol::smoke()),
            probe_rss: false,
            probe_spawns: false,
        };
        let report = driver.run(&opts).unwrap();
        let LegOutcome::Skipped { reason } = &report.scenarios[0].legs["reference"] else {
            panic!("the backend has no integrity check")
        };
        // The absent-leg reason wins here (no binary at all), but either way the
        // scenario is never scored as a backend loss.
        assert!(!reason.is_empty());
        assert!(report.scenarios[0].comparison.is_none());
        // Nothing could run, so the (expensive) `medium` fixture was never built.
        assert!(report.fixtures.is_empty());
    }
}
