//! `vs-reference` — the benchmark harness command line.
//!
//! Four verbs: inspect the catalogue, build fixtures, check which legs resolve on
//! this machine, and run. Nothing here provisions a binary or reaches the network.

// Mirrors `lib.rs`: assertions in this bin's own tests may panic. Test-only, so
// nothing in the shipped path is relaxed.
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use clap::{Args, Parser, Subcommand};
use rift_bench_native_baseline::{
    driver::{Driver, RunOptions},
    fixture::{FixtureSet, FixtureSpec},
    leg::{LegId, LegResolver, LegStatus},
    measure::Protocol,
    report::{ms, now_unix, LegOutcome},
    scenario::{self, Availability, ScenarioId},
    spawns::SpawnProbe,
};

/// Compare rift against the pinned capture backend over generated fixtures.
#[derive(Parser, Debug)]
#[command(name = "vs-reference", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// List the scenario catalogue and what each one waits on.
    Scenarios,
    /// Inspect or build the generated fixture repositories.
    #[command(subcommand)]
    Fixtures(FixturesCmd),
    /// Report which legs resolve here, and why the others do not.
    Doctor,
    /// Drive scenarios and write a report.
    Run(RunArgs),
    /// Compare two hook-latency artifacts as ratios.
    ///
    /// **Report-only by default (the owner's call): a regression exits 0.**
    /// Pass `--enforce` to exit non-zero.
    //
    // The history stays in source and OFF the CLI surface, deliberately. This
    // doc comment previously promised the opposite — that the verb exits
    // non-zero on a regression so it can be a gate leg — in the very commit
    // that made it report-only, so `--help` printed a false claim three options
    // above the flag contradicting it. Quoting that phrasing back inside a
    // `///` comment put it on the same surface again, where the guard passed
    // only on a capital letter. A `//` comment is the right home for it:
    // clap never prints it, so the assertion can be case-insensitive and mean
    // what it says. Pinned by
    // `the_help_text_does_not_promise_an_exit_code_the_binary_does_not_give`.
    Ratchet(RatchetArgs),
    /// Measure git latency: what a commit costs beyond a bare one.
    ///
    /// This is the instrument `benchmarks/hook-latency.md` claims
    /// reproducibility from. The scripts it used to name were never committed
    /// and are gone; this verb is the tracked replacement.
    HookLatency(HookLatencyArgs),
    /// Regenerate a report's ```bench-facts fence from its own artifacts.
    ///
    /// The fence is the only gate-bearing number set in any `benchmarks/*.md`.
    /// It is **generated**, never typed, and the
    /// artifact-consistency guard recomputes it: a fence that disagrees with
    /// the JSON it came from is a red, not a rounding difference.
    Facts(FactsArgs),
    /// Check that the published numbers, floors and provenance agree.
    /// Exits non-zero on any disagreement.
    ///
    /// This is a CONSISTENCY verdict, never a performance one: nothing it
    /// prints says the machine is fast.
    Audit,
    /// Measure where the initialization cost sits: at `enable`, or at the first
    /// commit — the owner moved projection init to enable time, and this says
    /// whether the cost moved or vanished.
    ///
    /// Both orders in one run — enable-then-commit and
    /// commit-without-prior-enable — because one order cannot tell a cost that
    /// moved from a cost that never existed.
    EnableDisplacement(DisplacementArgs),
}

#[derive(Args, Debug)]
struct DisplacementArgs {
    /// Measured iterations per arm, each in its own fresh repository.
    #[arg(long, default_value_t = 11)]
    iterations: usize,
    /// Discarded iterations per arm.
    #[arg(long, default_value_t = 3)]
    warmup: usize,
    /// Report output directory (default: `benchmarks/enable-displacement/`).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Sandbox directory for this sweep.
    #[arg(long)]
    run_dir: Option<PathBuf>,
}

/// Which fence `facts` regenerates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
enum FactsKind {
    /// The git-latency family's ```bench-facts fence (`hook-latency.md`).
    HookLatency,
    /// The all-families ```bench-report fence (`benchmarks/README.md`),
    /// read from every artifact under `benchmarks/`.
    Report,
}

#[derive(Args, Debug)]
struct FactsArgs {
    /// Which fence to regenerate.
    #[arg(long, value_enum, default_value_t = FactsKind::HookLatency)]
    kind: FactsKind,
    /// The report to splice the fence into (default: `benchmarks/hook-latency.md`
    /// for `hook-latency`, `benchmarks/README.md` for `report`).
    #[arg(long)]
    report: Option<PathBuf>,
    /// The artifact directory to read (`hook-latency` only; `report` reads
    /// all of `benchmarks/`).
    #[arg(long, default_value = "benchmarks/hook-latency")]
    from: PathBuf,
    /// Print the fence instead of writing it.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Args, Debug)]
struct RatchetArgs {
    /// The recorded baseline artifact (`hook-latency-*.json`).
    #[arg(long)]
    baseline: PathBuf,
    /// The run being judged.
    #[arg(long)]
    candidate: PathBuf,
    /// Percentage the control-relative ratio may grow before it is a
    /// regression.
    #[arg(long, default_value_t = rift_bench_native_baseline::hooklat::ratchet::DEFAULT_TOLERANCE_PCT)]
    tolerance: f64,
    /// Exit non-zero on a regression.
    ///
    /// Off by default, on the owner's call: the ratchet is report-only
    /// until the variance is characterised, because at 10% it fails 5 arms of
    /// 5 between two identical runs of the same binary. The flag exists so the
    /// enforcing behaviour stays exercised and can be switched on by a
    /// threshold rather than by a rewrite.
    #[arg(long)]
    enforce: bool,
}

#[derive(Args, Debug)]
struct HookLatencyArgs {
    /// Arms to measure (repeatable, comma-separated). The control always runs.
    #[arg(long = "arms", value_delimiter = ',')]
    arms: Vec<String>,
    /// Measured commits per arm.
    #[arg(long, default_value_t = 11)]
    iterations: usize,
    /// Discarded commits per arm before measurement.
    #[arg(long, default_value_t = 3)]
    warmup: usize,
    /// Report output directory (default: the tracked `benchmarks/hook-latency/`).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Sandbox directory for this sweep.
    #[arg(long)]
    run_dir: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum FixturesCmd {
    /// Show the fixture specs.
    List,
    /// Build one fixture (or `all`), printing its head oid.
    Build {
        /// `small` | `medium` | `large` | `all`.
        name: String,
    },
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Scenario to run (repeatable). Default: the whole catalogue.
    #[arg(long = "scenario")]
    scenarios: Vec<String>,
    /// Legs to measure. Default `rift`: the pinned capture backend and its pin
    /// crate were deleted together, so a default naming it asked
    /// every run to resolve a binary that no longer exists and recorded a
    /// skipped comparison for every scenario. It stays *selectable* — the
    /// binary survives as a benchmark fixture outside the repository and
    /// `--legs rift,reference` still measures it — but a leg for a deleted
    /// binary is not a default.
    #[arg(long, value_delimiter = ',', default_value = "rift")]
    legs: Vec<String>,
    /// Override every scenario's measured iteration count.
    #[arg(long)]
    iterations: Option<usize>,
    /// Override every scenario's warmup count.
    #[arg(long)]
    warmup: Option<usize>,
    /// Skip the extra untimed peak-RSS pass.
    #[arg(long)]
    no_rss: bool,
    /// Skip the extra untimed child-process pass.
    #[arg(long)]
    no_spawns: bool,
    /// Report output directory (default: the tracked `benchmarks/native-baseline/`).
    #[arg(long)]
    out: Option<PathBuf>,
    /// Fixture cache directory.
    #[arg(long)]
    fixture_dir: Option<PathBuf>,
    /// Sandbox directory for this run.
    #[arg(long)]
    run_dir: Option<PathBuf>,
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Scenarios => scenarios(),
        Command::Fixtures(cmd) => fixtures(&cmd),
        Command::Doctor => doctor(),
        Command::Run(args) => run(&args),
        Command::HookLatency(args) => hook_latency(&args),
        Command::Ratchet(args) => ratchet(&args),
        Command::Facts(args) => facts(&args),
        Command::Audit => audit(),
        Command::EnableDisplacement(args) => enable_displacement(&args),
    }
}

fn enable_displacement(args: &DisplacementArgs) -> Result<()> {
    use rift_bench_native_baseline::displace;
    use rift_bench_native_baseline::hermetic::Hermetic;

    let run_root = args
        .run_dir
        .clone()
        .unwrap_or_else(|| default_run_dir().join("enable-displacement"));
    let hermetic = Hermetic::prepare(run_root.join("hermetic"))?;
    let resolver = LegResolver::from_env();
    let protocol = Protocol::new(args.warmup, args.iterations);
    let report = displace::run_sweep(&hermetic, &resolver, &run_root, protocol)?;

    for arm in &report.arms {
        let line = match arm.stats {
            Some(stats) => match report.latency_ns(arm) {
                Some(ns) => format!("p50 {} — git latency {}", ms(stats.p50_ns), ms(ns)),
                None => format!("p50 {} (not a git latency)", ms(stats.p50_ns)),
            },
            None => format!("FAILED — {}", arm.error.as_deref().unwrap_or("no samples")),
        };
        println!("{:<32} {line}", arm.arm.id);
    }
    match report.displaced_ns() {
        Some(ns) => println!("\ndisplaced (E2 - E1): {ns} ns"),
        None => println!("\ndisplacement not measurable in this run"),
    }

    let out = args.out.clone().unwrap_or_else(|| {
        rift_bench_native_baseline::leg::default_repo_root()
            .join("benchmarks")
            .join("enable-displacement")
    });
    let json = report.write_json(&out)?;
    let md = report.write_markdown(&out)?;
    println!("\nreport: {}\n        {}", json.display(), md.display());
    Ok(())
}

fn audit() -> Result<()> {
    use rift_bench_native_baseline as bench;

    let root = bench::leg::default_repo_root();
    let verdict = bench::audit::audit(&root);
    print!("{}", verdict.render());
    if verdict.clean() {
        Ok(())
    } else {
        Err(anyhow!(
            "artifact-consistency: {} finding(s)",
            verdict.findings.len()
        ))
    }
}

/// The heading the fence lands under when a report does not have one yet.
///
/// Written once and then preserved — `splice` replaces the fence, never the
/// prose around it — so everything a reader needs in order not to misread the
/// fence has to be said here, including the two ways these numbers differ from
/// the hand-written tables they sit beside.
const FACTS_HEADING: &str = "\n## Gate-bearing facts\n\n\
     Generated by `vs-reference facts` from the JSON artifacts named in the fence below, and \
     **the only gate-bearing numbers in this document**. Everything else in this \
     file is commentary — it says what a number means, what produced it and what it retired, \
     and it is not what the guard reads. `vs-reference facts --dry-run` prints the same numbers \
     as a table.\n\n\
     **The reduction, stated because it is not the one the tables below use.** Each arm's \
     figure is the nearest-rank median, across every committed artifact that measured that \
     arm, of (arm p50 - that run's own control p50). Two consequences, and neither is a \
     rounding difference:\n\n\
     1. **It pools sittings the sections keep apart.** Section 9 and section 10 both ran \
     `C1_include_50k`, in different sessions at different machine loads, and report 109.1 ms \
     and 84.5 ms. The fence pools all eight runs into one number, which is neither of those \
     and is not a correction of either.\n\
     2. **Nearest-rank, not midpoint.** For an even number of runs the sections average the \
     middle two; the harness returns an observed run. `D3_newline_1mib` is 245.8 ms in section \
     10.2 and 243.94 ms here, from the identical four runs, and both are right about their own \
     definition. Nearest-rank is what every other percentile in this harness means, so it is \
     what the fence means.\n\n\
     Read a section's own table for that session's finding; read the fence for what the \
     committed artifacts, taken together, say.\n\n";

/// The heading the aggregate fence lands under when `benchmarks/README.md`
/// does not have one yet; preserved thereafter (`splice_tagged` replaces the
/// fence, never the prose).
const REPORT_HEADING: &str = "\n## Gate-bearing facts (every family)\n\n\
     Generated by `vs-reference facts --kind report` from every artifact under \
     `benchmarks/`, and **the only gate-bearing numbers in this document**. \
     The artifact-consistency guard recomputes the fence from the artifacts it names and \
     reds on disagreement. Rates are milli-units per second and times are nanoseconds; \
     every median is nearest-rank across runs of each run's own median. A row that no clean \
     run has measured says so rather than carrying a number. The git-latency arms \
     keep their one home in `hook-latency.md`'s fence; only their counts appear here.\n\n";

fn facts(args: &FactsArgs) -> Result<()> {
    use rift_bench_native_baseline::{
        facts as fence, hooklat::HookLatencyReport, leg::default_repo_root,
    };

    let root = default_repo_root();
    let resolve = |p: &PathBuf| {
        if p.is_absolute() {
            p.clone()
        } else {
            root.join(p)
        }
    };
    if args.kind == FactsKind::Report {
        return report_facts(args, &root);
    }
    let report_path = resolve(
        &args
            .report
            .clone()
            .unwrap_or_else(|| PathBuf::from("benchmarks/hook-latency.md")),
    );
    let from = resolve(&args.from);

    let mut entries: Vec<PathBuf> = std::fs::read_dir(&from)
        .with_context(|| format!("reading {}", from.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    entries.sort();
    let mut loaded: Vec<(String, HookLatencyReport)> = Vec::new();
    for path in &entries {
        let body =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let stem = path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        loaded.push((
            stem,
            serde_json::from_str(&body).with_context(|| format!("parsing {}", path.display()))?,
        ));
    }
    if loaded.is_empty() {
        return Err(anyhow!("no artifacts under {}", from.display()));
    }

    let built = fence::BenchFacts::of(&loaded, SUPERSEDED_REASON.to_owned());
    if args.dry_run {
        print!("{}", built.render_fence());
        print!("{}", built.render_table());
        return Ok(());
    }
    let markdown = std::fs::read_to_string(&report_path)
        .with_context(|| format!("reading {}", report_path.display()))?;
    let spliced = fence::splice(&markdown, &built, FACTS_HEADING);
    std::fs::write(&report_path, &spliced)
        .with_context(|| format!("writing {}", report_path.display()))?;
    println!(
        "{}: {} artifact(s), {} arm(s), {} superseded",
        report_path.display(),
        built.artifacts.len(),
        built.arms.len(),
        built.superseded_pending_rerun.len()
    );
    Ok(())
}

/// `facts --kind report`: the aggregate fence over every family.
fn report_facts(args: &FactsArgs, root: &std::path::Path) -> Result<()> {
    use rift_bench_native_baseline::{audit::Inputs, facts as fence, report_facts::ReportFacts};

    let report_path = args.report.clone().map_or_else(
        || root.join("benchmarks").join("README.md"),
        |p| {
            if p.is_absolute() {
                p
            } else {
                root.join(p)
            }
        },
    );
    let inputs = Inputs::load(root);
    if let Some(err) = &inputs.floors_error {
        return Err(anyhow!("benchmarks/floors.toml did not load: {err}"));
    }
    let built = ReportFacts::of(&inputs, inputs.floors.as_ref());
    if args.dry_run {
        print!("{}", built.render_fence());
        print!("{}", built.render_table());
        return Ok(());
    }
    let markdown = std::fs::read_to_string(&report_path)
        .with_context(|| format!("reading {}", report_path.display()))?;
    let spliced = fence::splice_tagged(
        &markdown,
        rift_bench_native_baseline::report_facts::REPORT_TAG,
        &built.render_fence(),
        REPORT_HEADING,
    );
    std::fs::write(&report_path, &spliced)
        .with_context(|| format!("writing {}", report_path.display()))?;
    println!(
        "{}: {} family(ies), {} throughput row(s), {} latency row(s)",
        report_path.display(),
        built.families.len(),
        built.throughput.len(),
        built.latency.len()
    );
    Ok(())
}

/// Why an artifact behind the fence is a record rather than a current number.
///
/// Stated once, here, because it is the same sentence for all of them: they
/// were taken under the pre-cap regime and carry no commit provenance at all,
/// so nothing ties them to a tree — and the caps landed about five hours after
/// the last of them.
const SUPERSEDED_REASON: &str =
    "Taken before the shipped caps (the owner cut MAX_TOTAL_BYTES from 1 MiB to 8 KiB \
     roughly five hours after the last of these runs) and recorded under artifact schema 1, \
     which carries no commit provenance. They are the evidence that PRODUCED that decision and \
     are kept exactly as measured; they may not derive a floor and may not be quoted as current. \
     Only a fresh quiet-machine sweep replaces them.";

fn ratchet(args: &RatchetArgs) -> Result<()> {
    use rift_bench_native_baseline::hooklat::{ratchet as gate, HookLatencyReport};

    let read = |path: &PathBuf| -> Result<HookLatencyReport> {
        let body =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&body).with_context(|| format!("parsing {}", path.display()))
    };
    let deltas = gate::compare(
        &read(&args.baseline)?,
        &read(&args.candidate)?,
        args.tolerance,
    );
    print!("{}", gate::to_markdown(&deltas, args.tolerance));

    match gate::outcome(&deltas, args.enforce) {
        gate::Outcome::Held => Ok(()),
        gate::Outcome::Reported { failed, total } => {
            // Re-emitted on the way out, because a reader who scrolled past the
            // header to the exit code must not be able to mistake this for a
            // pass. This branch previously did not exist: the flag was declared
            // and never read, so the process printed "does not fail a build"
            // and then failed the build.
            println!(
                "\nREPORT-ONLY (the owner's call): {failed} of {total} arms did not hold at \
                 {}%. Exiting 0 — this is NOT a passing gate, it is a gate that cannot fail. \
                 Pass --enforce to make it exit non-zero.",
                args.tolerance
            );
            Ok(())
        }
        gate::Outcome::Enforced { failed, total } => Err(anyhow!(
            "hook-latency ratchet FAILED: {failed} of {total} arms did not hold at {}%",
            args.tolerance
        )),
    }
}

fn hook_latency(args: &HookLatencyArgs) -> Result<()> {
    use rift_bench_native_baseline::hermetic::Hermetic;
    use rift_bench_native_baseline::hooklat;

    let run_root = args
        .run_dir
        .clone()
        .unwrap_or_else(|| default_run_dir().join("hook-latency"));
    let hermetic = Hermetic::prepare(run_root.join("hermetic"))?;
    let resolver = LegResolver::from_env();
    let protocol = Protocol::new(args.warmup, args.iterations);
    let report = hooklat::run_sweep(&hermetic, &resolver, &run_root, protocol, &args.arms)?;

    let control = report.control().copied();
    for arm in &report.arms {
        let line = match (arm.stats, control) {
            (Some(stats), Some(c)) if !arm.arm.is_control() => format!(
                "p50 {} — hook latency {} ({:.2}x bare)",
                ms(stats.p50_ns),
                ms(arm.latency_ns(&c).unwrap_or_default()),
                arm.ratio(&c).unwrap_or_default(),
            ),
            (Some(stats), _) => format!("p50 {} (control)", ms(stats.p50_ns)),
            (None, _) => format!("FAILED — {}", arm.error.as_deref().unwrap_or("no samples")),
        };
        println!("{:<12} {line}", arm.arm.id);
    }

    let out = args.out.clone().unwrap_or_else(default_hook_latency_dir);
    let json = report.write_json(&out)?;
    let md = report.write_markdown(&out)?;
    println!("\nreport: {}\n        {}", json.display(), md.display());
    Ok(())
}

/// Sweeps land beside the curated `benchmarks/hook-latency.md` they back.
fn default_hook_latency_dir() -> PathBuf {
    rift_bench_native_baseline::leg::default_repo_root()
        .join("benchmarks")
        .join("hook-latency")
}

fn scenarios() -> Result<()> {
    println!(
        "{:<20} {:<9} {:<8} {:<10} state",
        "scenario", "fixture", "warm/it", "headline"
    );
    for spec in scenario::catalogue() {
        let state = match spec.availability {
            Availability::Wired => "wired".to_owned(),
            Availability::Planned(note) => format!("planned — {note}"),
        };
        println!(
            "{:<20} {:<9} {:<8} {:<10} {state}",
            spec.id.as_str(),
            spec.fixture,
            format!("{}/{}", spec.protocol.warmup, spec.protocol.iterations),
            if spec.headline { "headline" } else { "" },
        );
    }
    Ok(())
}

fn fixtures(cmd: &FixturesCmd) -> Result<()> {
    let set = FixtureSet::new(FixtureSet::default_root());
    match cmd {
        FixturesCmd::List => {
            println!("cache root: {}", set.root().display());
            for spec in FixtureSpec::presets() {
                println!(
                    "{:<8} {:>6} files  {:>4} commits  {:>6} turns  seed {:#x}",
                    spec.name,
                    spec.total_files(),
                    spec.commits,
                    spec.session_turns,
                    spec.seed
                );
            }
        }
        FixturesCmd::Build { name } => {
            let specs: Vec<FixtureSpec> = if name == "all" {
                FixtureSpec::presets().to_vec()
            } else {
                vec![FixtureSpec::preset(name)
                    .ok_or_else(|| anyhow!("unknown fixture `{name}` (small|medium|large|all)"))?]
            };
            for spec in specs {
                let built = set.materialize(&spec).context("building fixture")?;
                println!(
                    "{:<8} {} head {} — {} files, {} bytes at {}",
                    spec.name,
                    if built.reused { "reused" } else { "built " },
                    built.manifest.head_oid,
                    built.manifest.worktree_files,
                    built.manifest.worktree_bytes,
                    built.repo.display()
                );
            }
        }
    }
    Ok(())
}

fn doctor() -> Result<()> {
    let run_root = default_run_dir();
    let driver = Driver::new(
        &run_root,
        FixtureSet::new(FixtureSet::default_root()),
        LegResolver::from_env(),
    )?;
    println!("run root:    {}", run_root.display());
    println!("fixtures:    {}", FixtureSet::default_root().display());
    match rift_bench_native_baseline::freshness::SourceClock::scan(
        &rift_bench_native_baseline::leg::default_repo_root(),
    ) {
        Some(clock) => println!(
            "sources:     newest {} at {} ({} files)",
            clock.newest_path.display(),
            clock.newest_unix,
            clock.files
        ),
        None => println!("sources:     NO CHECKOUT FOUND — freshness cannot be checked"),
    }
    println!("peak RSS:    {}", driver.rss_probe().describe());
    println!(
        "spawns:      {}",
        driver
            .spawn_probe()
            .map_or_else(|| "unavailable".to_owned(), SpawnProbe::describe)
    );
    match driver.vogon() {
        LegStatus::Present {
            binary,
            version,
            source,
            ..
        } => println!(
            "vogon        PRESENT  {} ({source}) — {version}",
            binary.display()
        ),
        LegStatus::Absent { reason } => println!("vogon        ABSENT   {reason}"),
    }
    for leg in driver.legs() {
        match &leg.status {
            LegStatus::Present {
                binary,
                version,
                source,
                binary_mtime_unix,
                ..
            } => println!(
                "leg {:<7} PRESENT  {} ({source}) — {version}, {} KiB, built {}",
                leg.id.as_str(),
                binary.display(),
                leg.binary_bytes.unwrap_or_default() / 1024,
                binary_mtime_unix.map_or_else(|| "?".to_owned(), |t| t.to_string()),
            ),
            LegStatus::Absent { reason } => {
                println!("leg {:<7} ABSENT   {reason}", leg.id.as_str());
            }
        }
    }
    Ok(())
}

fn run(args: &RunArgs) -> Result<()> {
    let scenarios = if args.scenarios.is_empty() {
        ScenarioId::all().to_vec()
    } else {
        args.scenarios
            .iter()
            .map(|name| ScenarioId::parse(name).ok_or_else(|| anyhow!("unknown scenario `{name}`")))
            .collect::<Result<Vec<_>>>()?
    };
    let legs = args
        .legs
        .iter()
        .map(|name| {
            LegId::all()
                .into_iter()
                .find(|l| l.as_str() == name)
                .ok_or_else(|| anyhow!("unknown leg `{name}` (rift|reference)"))
        })
        .collect::<Result<Vec<_>>>()?;
    let protocol = match (args.warmup, args.iterations) {
        (None, None) => None,
        (w, i) => Some(Protocol::new(w.unwrap_or(0), i.unwrap_or(1))),
    };

    let fixture_dir = args
        .fixture_dir
        .clone()
        .unwrap_or_else(FixtureSet::default_root);
    let run_dir = args.run_dir.clone().unwrap_or_else(default_run_dir);
    let driver = Driver::new(
        &run_dir,
        FixtureSet::new(fixture_dir),
        LegResolver::from_env(),
    )?;
    let report = driver.run(&RunOptions {
        scenarios,
        legs,
        protocol,
        probe_rss: !args.no_rss,
        probe_spawns: !args.no_spawns,
    })?;

    for scenario in &report.scenarios {
        for (leg, outcome) in &scenario.legs {
            let line = match outcome {
                LegOutcome::Measured { stats, .. } => format!(
                    "p50 {} p95 {} (n={})",
                    ms(stats.p50_ns),
                    ms(stats.p95_ns),
                    stats.samples
                ),
                LegOutcome::Skipped { reason } => format!("skipped — {reason}"),
                LegOutcome::Failed { reason, .. } => format!("FAILED — {reason}"),
            };
            println!("{:<20} {:<7} {line}", scenario.id.as_str(), leg);
        }
    }

    let out = args.out.clone().unwrap_or_else(default_out_dir);
    let json = report.write_json(&out)?;
    let md = report.write_markdown(&out)?;
    println!("\nreport: {}\n        {}", json.display(), md.display());
    Ok(())
}

/// Where a run's report lands by default.
///
/// **`benchmarks/`, which is tracked — not `docs/benchmarks/`, which is not.**
/// `docs/` is git-ignored in this repository and absent from a fresh clone, so
/// every report the harness had written since it was built was invisible to
/// anyone who did not run it, while the owner requires the results to be a
/// tracked artifact. Reports go in their own subdirectory
/// so a directory of curated, hand-written artifacts (`hook-latency.md`,
/// `redaction-fp-baseline.md`) does not silently fill with machine output.
fn default_out_dir() -> PathBuf {
    rift_bench_native_baseline::leg::default_repo_root()
        .join("benchmarks")
        .join("native-baseline")
}

fn default_run_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(".runs")
        .join(format!("run-{}", now_unix()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the default: it must land inside the tracked
    /// `benchmarks/` tree, and must never again point at git-ignored `docs/`.
    #[test]
    fn the_default_report_directory_is_tracked() {
        let out = default_out_dir();
        let repo = rift_bench_native_baseline::leg::default_repo_root();
        assert_eq!(out, repo.join("benchmarks").join("native-baseline"));
        assert!(
            !out.starts_with(repo.join("docs")),
            "docs/ is git-ignored here; a report written there is not published"
        );
    }

    /// The default leg set names only binaries that still exist.
    #[test]
    fn the_default_leg_set_is_rift_alone_and_the_backend_stays_selectable() {
        let cli = Cli::parse_from(["vs-reference", "run"]);
        let Command::Run(args) = cli.command else {
            panic!("expected the run subcommand")
        };
        assert_eq!(args.legs, vec!["rift".to_owned()]);

        let cli = Cli::parse_from(["vs-reference", "run", "--legs", "rift,reference"]);
        let Command::Run(args) = cli.command else {
            panic!("expected the run subcommand")
        };
        assert_eq!(args.legs, vec!["rift".to_owned(), "reference".to_owned()]);
    }
}
