//! Git latency: what a `git commit` costs beyond a bare one.
//!
//! The hook budget is **100 ms hard, defined as git latency** — not as the time
//! a hook body spends, but as the time a commit spends *beyond a commit with no
//! hooks at all*, summed over `prepare-commit-msg`, `commit-msg` and
//! `post-commit`. That definition is only measurable by running both commits, so
//! this module runs arms: a bare control and one arm per capture configuration,
//! all in the same run, on the same machine, minutes apart.
//!
//! # Why this exists as tracked Rust
//!
//! `benchmarks/hook-latency.md` is the tracked artifact that publishes those
//! numbers, and its protocol section said *"Reproducible by a second person;
//! every number below comes from these commands"* while naming `decompose.py`
//! and `certify-tip.sh` — **two scripts that were never committed anywhere and
//! are unrecoverable**. The sentence was false the moment the shell that held
//! them closed. The instrument had to be rebuilt as tracked, buildable Rust,
//! so that the claim of reproducibility is backed by something a
//! second person can actually run: `vs-reference hook-latency`.
//!
//! # The two rules the vanished instrument paid to learn
//!
//! **Quote every arm against a control measured in the same run.** Absolutes on
//! this machine move about 2× with load, so an arm compared with
//! yesterday's bare commit is comparing machine weather. `A_bare` is therefore
//! not optional and not filterable: it runs whenever any arm runs.
//!
//! **State what the arm exercised, beside the timing.** Two "50 MB projection"
//! arms in the artifact's own history were not large-store arms at all — the
//! harness wrote the big projection to a path rift never opened, rift made a
//! fresh 405 KB one, and every claim about a large store was measured on a small
//! one for three consecutive runs. It was caught by an oracle disagreeing with
//! the harness, not by review. So each arm here publishes [`Oracles`]: every
//! projection file under its own cache with its size (**enumerated, never
//! assumed to be one**), whether the checkpoint trailer landed, what `verify`
//! re-derived, and how many events reached durable storage.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::{
    freshness::SourceClock,
    hermetic::Hermetic,
    leg::LegResolver,
    leg::{LegId, LegStatus},
    measure::{measure, run_once, Invocation, Protocol, Stats},
    report::{now_unix, Host},
    scenario::{self, HookEvent, PlanCtx},
    BenchError, Result,
};

/// Schema of the git-latency artifact.
///
/// `2` adds provenance to the subject — `git_describe` and `dirty` — so an
/// artifact says which *commit* it is about rather than only which minute. The
/// committed `1` artifacts stay exactly as they are: they are the evidence that
/// produced the shipped caps and the two-facts verdict, they load unchanged,
/// and they may not derive a floor, which is the whole point of the field.
pub const HOOK_LATENCY_SCHEMA: u32 = 2;

/// The commit-message trailer a live capture arm must produce.
const TRAILER: &str = "Rift-Checkpoint";

/// The owner's hook budget: 100 ms of git latency, hard.
pub const BUDGET_MS: u64 = 100;

/// The sentence every absolute in this project's benchmarks travels with.
///
/// It is one sentence and it retires two mistakes: an absolute here moves about
/// **2x with machine load**, which is why every arm is quoted against a control
/// measured in the same run. Fresh Unix installs dispatch each unchained hook
/// through a direct executable link; a preserved user-hook chain and non-Unix
/// installs retain the shell shim. Process-launch cost is therefore an observed
/// implementation term, not an immutable hardware floor.
pub const MACHINE_CLASS_NOTE: &str = "**Machine class.** Every absolute here is one machine's \
     measurement and moves about 2x with load, which is why each arm is quoted against a \
     bare-commit control measured in the SAME run and the ratio is the portable number. Fresh \
     Unix installs invoke unchained hooks through direct executable links; preserved user-hook \
     chains and non-Unix installs retain shell shims. Process launch is part of the measured \
     implementation, and nothing here may be generalised into a product claim about another \
     machine.";

/// Whether a repository has rift's hooks installed, and whether a session is
/// live when the commit lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Capture {
    /// No `rift init`: the control. A commit here is a bare commit.
    Off,
    /// Hooks installed, but no session has ever opened — the "installed and
    /// idle" cost every repository pays whether or not an agent is running.
    Idle,
    /// Hooks installed and a session live at commit time: the path that
    /// condenses, archives and writes the trailer.
    Live,
}

/// An opted-in corpus: what `.worktreeinclude` points capture at.
///
/// **Sizes are stated in the unit the ceiling is charged in.**
/// `rift_shadow::inclusion::MAX_TOTAL_BYTES` is 8 KiB and is charged on bytes
/// **read**, decided from the `stat` before the open. But `Walk::file` writes
/// `f <path>` and then prefixes every content line with one space, so a file of
/// N bytes contributes up to **2N + 2** to the document the redactor is handed
/// — worst case a file that is all newlines, where the prefix is the whole
/// line. A full budget therefore hands on a document *just over 16 KiB*, not 8
/// (`the_document_handed_on_is_up_to_twice_the_bytes_read`).
///
/// So every arm publishes both numbers and the factor between them, and no arm
/// is described by one number alone without saying which of the two it means. The
/// factor for a corpus of long lines is near 1.0; the 2× is the pathological
/// end, not the typical one, and conflating them in either direction misstates
/// the ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Corpus {
    /// Files written under the opted-in directory.
    pub files: usize,
    /// Content lines per file.
    pub lines_per_file: usize,
    /// Characters per content line, excluding the newline.
    pub line_width: usize,
}

impl Corpus {
    /// Bytes on disk — what [`MAX_TOTAL_BYTES`-equivalent] budget is charged.
    ///
    /// [`MAX_TOTAL_BYTES`-equivalent]: https://docs.rs/
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.files * self.lines_per_file * (self.line_width + 1)
    }

    /// Bytes of the document handed to the redactor.
    ///
    /// Per file: `f <rel>\n` plus, for each part of `split('\n')`, a space, the
    /// part, and a newline. A file ending in a newline yields one extra empty
    /// part, which is why the count is `lines_per_file + 1`.
    #[must_use]
    pub fn document_bytes(&self, dir: &str) -> usize {
        let per_file = |name_len: usize| {
            2 + dir.len() + 1 + name_len + 1 + (self.lines_per_file + 1) * (self.line_width + 2)
        };
        // Names are `f0000.txt`-shaped, so every file's name is the same length.
        self.files * per_file(9)
    }

    /// Bytes in one file — what `MAX_FILE_BYTES` is charged against, and the
    /// limit that declines a file **before it is opened**.
    #[must_use]
    pub fn bytes_per_file(&self) -> usize {
        self.lines_per_file * (self.line_width + 1)
    }

    /// Whether the product's own caps would let this corpus be read whole.
    #[must_use]
    pub fn is_capturable(&self) -> bool {
        self.bytes_per_file() <= MAX_FILE_BYTES
            && self.bytes() <= MAX_TOTAL_BYTES
            && self.files <= MAX_ENTRIES
    }

    /// The multiple between what is read and what is handed on.
    #[must_use]
    pub fn document_factor(&self, dir: &str) -> f64 {
        let read = self.bytes();
        if read == 0 {
            return 0.0;
        }
        self.document_bytes(dir) as f64 / read as f64
    }
}

/// The directory `.worktreeinclude` opts in, and `.gitignore` keeps out of git.
pub const CORPUS_DIR: &str = "benchdata";

/// `rift_shadow::inclusion::MAX_TOTAL_BYTES`, restated because the harness
/// links no product crate. Pinned against the declaration by
/// `the_ceiling_and_the_control_file_still_match_the_product`.
///
/// **8 KiB since the owner cut it — down from 1 MiB, a 128× cut this
/// harness's own measurements produced.** The guard that pins it reddened on the day the
/// constant landed, which is the guard working; §9–§11 of
/// `benchmarks/hook-latency.md` were taken under the **1 MiB regime** and are
/// labelled there as pre-cap, because no arm in them is capturable now.
pub const MAX_TOTAL_BYTES: usize = 8 * 1024;

/// `rift_shadow::inclusion::MAX_FILE_BYTES` — half the total budget.
///
/// It is restated because it **silently decides whether an arm measures
/// anything at all**: a file over this size is declined before it is opened, so
/// a corpus declared in the old regime would have every file refused and the
/// arm would time a walk that captured nothing while its label still said
/// 1,048,576 B. `every_declared_corpus_is_capturable_under_the_products_caps`
/// is the guard that makes that impossible to declare.
pub const MAX_FILE_BYTES: usize = MAX_TOTAL_BYTES / 2;

/// `rift_shadow::inclusion::MAX_ENTRIES` — records the walk may write.
pub const MAX_ENTRIES: usize = 256;

/// One arm of the sweep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Arm {
    /// Stable identifier, used in the report and by `--arms`.
    pub id: String,
    /// One line describing what this arm exercises.
    pub title: String,
    /// Capture configuration.
    pub capture: Capture,
    /// The `.worktreeinclude` corpus this arm opts in, when it opts anything in.
    #[serde(default)]
    pub include: Option<Corpus>,
}

impl Arm {
    /// Whether this arm is the control every other arm is quoted against.
    #[must_use]
    pub fn is_control(&self) -> bool {
        self.capture == Capture::Off
    }
}

/// The default sweep: the control, the idle arm, the live arm.
#[must_use]
pub fn arms() -> Vec<Arm> {
    vec![
        Arm {
            id: "A_bare".to_owned(),
            title: "bare commit, no hooks installed (control)".to_owned(),
            capture: Capture::Off,
            include: None,
        },
        Arm {
            id: "B0_idle".to_owned(),
            title: "hooks installed, nothing opted in, no live session".to_owned(),
            capture: Capture::Idle,
            include: None,
        },
        Arm {
            id: "B1_live".to_owned(),
            title: "hooks installed, a session live at commit time".to_owned(),
            capture: Capture::Live,
            include: None,
        },
        // **Re-sized for the 8 KiB cap.** The previous arm set
        // — 51 KB to 1 MiB, cheap and worst twins, plus two ladders — was
        // declared against a 1 MiB budget with a **flat 256 KiB** per-file
        // limit (`MAX_TOTAL_BYTES / 2` is the rule that arrived *with* the cut).
        // Under the new caps only `F1_newline_8k` — 8 files x 1,024 B = 8,192 B
        // — remains capturable. The rest fail for **two different reasons** and
        // the distinction matters: `C1/C2/C3`, `D1/D2/D3`, `E1-E5` and `F5`
        // exceed the 4 KiB **per-file** limit and are declined file-by-file,
        // while `F2/F3/F4` are 2,048/3,072/4,096 B per file — all within it —
        // and are stopped by the **total** budget partway through the walk. An
        // arm in either state times a walk that captured little or nothing
        // while its label still says 1,048,576 B.
        // That is the false-label class this harness exists to remove, so they are
        // gone rather than kept as decoration. Their measurements are not: §9
        // through §11 of `benchmarks/hook-latency.md` and the committed run
        // artifacts hold them, labelled as the pre-cap evidence that
        // **produced** the shipped caps.
        //
        // Pairs again, same bytes read on both sides, so the only independent
        // variable is what the redactor is handed. `G3`/`H3` sit on the new
        // ceiling exactly.
        include_arm("G1_line_2k", 4, 8, LINE_WIDTH),
        include_arm("G2_line_4k", 4, 16, LINE_WIDTH),
        include_arm("G3_line_8k", 8, 16, LINE_WIDTH),
        include_arm("H1_newline_2k", 4, 512, 0),
        include_arm("H2_newline_4k", 4, 1_024, 0),
        include_arm("H3_newline_8k", 8, 1_024, 0),
    ]
}

/// A live arm that opts a corpus in through `.worktreeinclude`.
fn include_arm(id: &str, files: usize, lines_per_file: usize, line_width: usize) -> Arm {
    let corpus = Corpus {
        files,
        lines_per_file,
        line_width,
    };
    Arm {
        id: id.to_owned(),
        title: format!(
            "live session + `.worktreeinclude` over {files} files ({} B read, {} B handed on, {:.2}x)",
            corpus.bytes(),
            corpus.document_bytes(CORPUS_DIR),
            corpus.document_factor(CORPUS_DIR),
        ),
        capture: Capture::Live,
        include: Some(corpus),
    }
}

/// Content-line width. 63 characters plus the newline is 64 bytes a line, so a
/// corpus's on-disk size is an exact multiple and an arm can sit *on* the read
/// ceiling rather than near it.
const LINE_WIDTH: usize = 63;

/// What an arm exercised, published beside what it cost.
///
/// A hook that got fast by ceasing to work looks identical in every timing, so
/// none of these is decorative. They are reported for **every** arm including
/// the control — a check that only ever confirms is not a check, and the idle
/// arm's *absent* trailer is what makes the live arm's present one mean
/// something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Oracles {
    /// Every `*.db` under this arm's own cache root, with its size.
    ///
    /// A map rather than a number because the defect this replaces was exactly
    /// a second file: the harness wrote a 52 MB projection to a path rift never
    /// opened, rift created a fresh 405 KB one beside it, and "the 50 MB arm"
    /// was measured on the small one. Two entries here is a finding, not a
    /// rounding difference.
    pub projections: BTreeMap<String, u64>,
    /// Whether the last commit's message carries the checkpoint trailer.
    pub trailer_present: bool,
    /// Checkpoints `rift verify` re-derived in this repository.
    pub verify_checkpoints: Option<u64>,
    /// Shards `rift verify` redaction/Merkle-checked.
    pub verify_shards: Option<u64>,
    /// Events the projection holds, from `rift status`.
    pub captured_events: Option<u64>,
    /// What `rift checkpoint explain` reports for the arm's last checkpoint,
    /// under the label the verb itself uses: **"files read"**.
    ///
    /// Named that way on purpose. It is the closest observable the shipped CLI
    /// offers to "how many spans did the opted-in walk record", and it is not
    /// proven to be the same number — so it is published as the verb's own
    /// figure rather than relabelled as a span count the harness has not
    /// established. The inclusion walk's true record count is not on any CLI
    /// surface today; a request for one is open with the owner.
    #[serde(default)]
    pub explain_files_read: Option<u64>,
}

/// One arm's result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArmResult {
    /// The arm.
    pub arm: Arm,
    /// Timing of the measured commits, or `None` when the arm could not run.
    pub stats: Option<Stats>,
    /// Why it could not run.
    pub error: Option<String>,
    /// What it exercised.
    pub oracles: Oracles,
    /// What the machine was doing when this arm started.
    ///
    /// Per arm rather than per run, because a sweep's arms are minutes apart
    /// and this machine's load moves inside that. A void reading does not stop
    /// the arm being published; it stops it deriving a floor.
    #[serde(default)]
    pub attestation: Option<crate::attest::Attestation>,
    /// Phase breakdown from one extra, **untimed** commit with
    /// `RIFT_HOOK_TIMING=1`: hook name -> phase -> microseconds.
    ///
    /// Untimed and separate because the timer takes clock readings the shipped
    /// default never takes; folding it into the published samples would measure
    /// the instrument.
    pub phases: BTreeMap<String, BTreeMap<String, u64>>,
}

impl ArmResult {
    /// Git latency: this arm's median minus the control's, in nanoseconds.
    /// `None` when either side is missing — never zero, which would read as
    /// "free".
    #[must_use]
    pub fn latency_ns(&self, control: &Stats) -> Option<u64> {
        Some(self.stats?.p50_ns.saturating_sub(control.p50_ns))
    }

    /// This arm's median as a multiple of the control's, the portable number.
    #[must_use]
    pub fn ratio(&self, control: &Stats) -> Option<f64> {
        let stats = self.stats?;
        (control.p50_ns > 0).then(|| stats.p50_ns as f64 / control.p50_ns as f64)
    }
}

/// The three facts an owner needs **together** to rule on a breach.
///
/// "It breaches" tells an owner nothing actionable. "It breaches, here is the
/// curve across three sizes, here is which side of the read/handed-on 2× each
/// number sits on, and here is the phase that owns the cost" tells them
/// everything — and it is the difference between a decision and another round.
///
/// **One caveat travels with every row and is not a disclaimer.** The magnitude
/// (`git_latency_us`) comes from the *median of the timed commits*; the
/// attribution comes from the arm's **one extra untimed commit** with the phase
/// timer on. They are two populations, so the residue below is an anatomy of a
/// representative commit set beside the magnitude of the median — not an exact
/// decomposition of the same commit. Subtracting them as if they were one
/// number is the mistake this doc comment exists to stop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BreachAnatomy {
    /// Which arm.
    pub arm: String,
    /// Bytes the opted-in corpus presents to be **read** — the unit
    /// `MAX_TOTAL_BYTES` is charged in. `None` for an arm that opts nothing in.
    pub bytes_read: Option<usize>,
    /// Bytes of the document handed to the redactor.
    pub document_bytes: Option<usize>,
    /// `document / read`. The ceiling's worst case is 2N + 2; a corpus of long
    /// lines sits near 1.0. Which end an arm is on changes what its number
    /// means.
    pub document_factor: Option<f64>,
    /// Median git latency: this arm's p50 minus the control's, microseconds.
    pub git_latency_us: u64,
    /// Sum of the three commit hooks' own `total_us`, from the phase commit.
    pub hook_bodies_us: u64,
    /// Git latency not inside any hook body: process creation, hook dispatch,
    /// and git's own cost of running three hooks.
    pub outside_hook_bodies_us: u64,
    /// `redact`, exclusive, summed across hooks.
    pub redact_us: u64,
    /// `predict_id` **with `redact` subtracted** — the id derivation and
    /// hashing alone.
    pub predict_id_excl_us: u64,
    /// The hook bodies measured larger than the median git latency. Possible
    /// across two populations (a slow phase commit against a fast median), and
    /// reported rather than clamped silently.
    pub bodies_exceed_latency: bool,
    /// Whether this arm is over the budget.
    pub breaches: bool,
}

/// A whole sweep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookLatencyReport {
    /// Artifact schema ([`HOOK_LATENCY_SCHEMA`]).
    pub schema_version: u32,
    /// Harness package version.
    pub harness_version: String,
    /// Unix seconds; provenance only.
    pub generated_at_unix: u64,
    /// Where it ran.
    pub host: Host,
    /// The rift binary measured, with its freshness provenance.
    pub subject: LegStatus,
    /// The newest source in the checkout the subject was resolved from.
    pub source_clock: Option<SourceClock>,
    /// What `git --version` reported: the control's identity, since a bare
    /// commit's cost is git's, not rift's.
    pub git_version: String,
    /// Iteration counts every arm used.
    pub protocol: Protocol,
    /// The budget, in milliseconds of git latency (owner-set).
    pub budget_ms: u64,
    /// Results in sweep order, control first.
    pub arms: Vec<ArmResult>,
}

impl HookLatencyReport {
    /// The control arm's timing, if it produced one.
    #[must_use]
    pub fn control(&self) -> Option<&Stats> {
        self.arms
            .iter()
            .find(|a| a.arm.is_control())
            .and_then(|a| a.stats.as_ref())
    }

    /// The per-arm anatomy the owner's decision needs.
    ///
    /// Empty without a control: every figure here is control-relative, and a
    /// sweep that lost its control has no latency to decompose.
    #[must_use]
    pub fn anatomy(&self) -> Vec<BreachAnatomy> {
        let Some(control) = self.control().copied() else {
            return Vec::new();
        };
        self.arms
            .iter()
            .filter(|a| !a.arm.is_control())
            .filter_map(|a| {
                let git_latency_us = a.latency_ns(&control)? / 1_000;
                let hook_bodies_us: u64 = a
                    .phases
                    .values()
                    .map(|p| p.get(TOTAL_PHASE).copied().unwrap_or_default())
                    .sum();
                let sum_exclusive = |name: &str| -> u64 {
                    a.phases
                        .values()
                        .map(|p| exclusive_phases(p).get(name).copied().unwrap_or_default())
                        .sum()
                };
                Some(BreachAnatomy {
                    arm: a.arm.id.clone(),
                    bytes_read: a.arm.include.map(|c| c.bytes()),
                    document_bytes: a.arm.include.map(|c| c.document_bytes(CORPUS_DIR)),
                    document_factor: a.arm.include.map(|c| c.document_factor(CORPUS_DIR)),
                    git_latency_us,
                    hook_bodies_us,
                    outside_hook_bodies_us: git_latency_us.saturating_sub(hook_bodies_us),
                    redact_us: sum_exclusive("redact"),
                    predict_id_excl_us: sum_exclusive("predict_id"),
                    bodies_exceed_latency: hook_bodies_us > git_latency_us,
                    breaches: git_latency_us > self.budget_ms.saturating_mul(1_000),
                })
            })
            .collect()
    }

    /// The file stem a sweep is written under.
    #[must_use]
    pub fn stem(&self) -> String {
        format!("hook-latency-{}", self.generated_at_unix)
    }

    /// Write `<dir>/<stem>.json`.
    ///
    /// # Errors
    /// [`BenchError::Io`] / [`BenchError::Json`] on failure.
    pub fn write_json(&self, dir: &Path) -> Result<PathBuf> {
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
    pub fn write_markdown(&self, dir: &Path) -> Result<PathBuf> {
        fs::create_dir_all(dir).map_err(|e| BenchError::io("create report dir", e))?;
        let path = dir.join(format!("{}.md", self.stem()));
        fs::write(&path, self.to_markdown()).map_err(|e| BenchError::io("write report", e))?;
        Ok(path)
    }

    /// Render the sweep.
    #[must_use]
    pub fn to_markdown(&self) -> String {
        let mut out = format!(
            "# Hook latency — git-latency sweep\n\n\
             - harness `{}`, artifact schema {}, generated {}\n\
             - host: {} / {} ({} logical CPUs)\n- control git: {}\n\
             - protocol: {} warmup + {} measured commits per arm\n\
             - budget: **{} ms** of git latency (owner-set, hard)\n\n",
            self.harness_version,
            self.schema_version,
            self.generated_at_unix,
            self.host.os,
            self.host.arch,
            self.host
                .logical_cpus
                .map_or_else(|| "?".to_owned(), |n| n.to_string()),
            self.git_version,
            self.protocol.warmup,
            self.protocol.iterations,
            self.budget_ms,
        );
        self.subject_section(&mut out);
        self.arms_section(&mut out);
        self.corpus_section(&mut out);
        self.anatomy_section(&mut out);
        self.oracles_section(&mut out);
        self.phases_section(&mut out);
        out.push_str(&format!("\n{MACHINE_CLASS_NOTE}\n"));
        out
    }

    fn subject_section(&self, out: &mut String) {
        out.push_str("## Subject\n\n");
        match &self.subject {
            LegStatus::Present {
                binary,
                version,
                source,
                binary_mtime_unix,
                git_describe,
                dirty,
            } => out.push_str(&format!(
                "`{}` — {version}, resolved via {source}, built {}\n\ncommit `{}`{}\n",
                binary.display(),
                binary_mtime_unix.map_or_else(|| "?".to_owned(), |t| t.to_string()),
                git_describe.as_deref().unwrap_or("UNKNOWN"),
                match dirty {
                    // Printed as a sentence rather than a flag: a dirty subject
                    // names a tree that exists on no other machine, and a
                    // reader has to meet that fact beside the number, not two
                    // sections away in a floors file.
                    Some(true) =>
                        " — **DIRTY working tree**: this run is a record and may not \
                                   derive a floor",
                    Some(false) => "",
                    None => " — provenance unknown, so this run may not derive a floor",
                },
            )),
            LegStatus::Absent { reason } => {
                out.push_str(&format!("**no subject binary**: {reason}\n"));
            }
        }
        if let Some(clock) = &self.source_clock {
            out.push_str(&format!(
                "\nNewest source in the checkout: `{}` at {} ({} files).\n",
                clock.newest_path.display(),
                clock.newest_unix,
                clock.files
            ));
        }
    }

    fn arms_section(&self, out: &mut String) {
        out.push_str(
            "\n## Arms\n\n`hook latency` is this arm's p50 minus the control's, in the same \
             run; `× bare` is the ratio, and the ratio is the portable number.\n\n\
 | arm | p50 | min | hook latency | × bare | verdict |\n|---|---|---|---|---|---|\n",
        );
        let control = self.control().copied();
        for a in &self.arms {
            let Some(stats) = a.stats else {
                out.push_str(&format!(
                    "| `{}` | — | — | — | — | FAILED — {} |\n",
                    a.arm.id,
                    a.error.as_deref().unwrap_or("no samples")
                ));
                continue;
            };
            let load = a
                .attestation
                .map_or_else(|| "load1 unknown".to_owned(), |x| x.describe());
            let (latency, ratio, verdict) = match control {
                Some(_) if a.arm.is_control() => {
                    ("—".to_owned(), "1.00×".to_owned(), "control".to_owned())
                }
                Some(c) => {
                    let ns = a.latency_ns(&c).unwrap_or_default();
                    let over = ns > self.budget_ms.saturating_mul(1_000_000);
                    (
                        crate::report::ms(ns),
                        a.ratio(&c)
                            .map_or_else(|| "—".to_owned(), |r| format!("{r:.2}×")),
                        if over {
                            "**BREACH**".to_owned()
                        } else {
                            "meets".to_owned()
                        },
                    )
                }
                None => (
                    "—".to_owned(),
                    "—".to_owned(),
                    "**no control** — nothing to quote against".to_owned(),
                ),
            };
            out.push_str(&format!(
                "| `{}` | {} | {} | {latency} | {ratio} | {verdict} · {load} |\n",
                a.arm.id,
                crate::report::ms(stats.p50_ns),
                crate::report::ms(stats.min_ns),
            ));
        }
        for a in &self.arms {
            out.push_str(&format!("\n- `{}` — {}\n", a.arm.id, a.arm.title));
        }
    }

    /// What each opted-in arm actually handed capture, in both units.
    fn corpus_section(&self, out: &mut String) {
        let rows: Vec<_> = self
            .arms
            .iter()
            .filter_map(|a| a.arm.include.map(|c| (a, c)))
            .collect();
        if rows.is_empty() {
            return;
        }
        out.push_str(&format!(
            "\n## Opted-in corpus\n\n`MAX_TOTAL_BYTES` is {cap} and is charged on bytes \
                 **read**; `Walk::file` prefixes every content line with one space, so N bytes \
                 contribute up to **2N + 2** to the document the redactor is handed. A full \
                 budget therefore hands on just over {doubled}, not {cap}. Both numbers are \
                 published, because an arm described by one figure alone misstates the ceiling \
                 in one direction or the other.\n\n\
 | arm | files | bytes read | budget | document | factor | files read (explain) \
 |\n|---|---|---|---|---|---|---|\n",
            cap = human_bytes(MAX_TOTAL_BYTES),
            doubled = human_bytes(MAX_TOTAL_BYTES * 2),
        ));
        for (result, corpus) in rows {
            let read = corpus.bytes();
            out.push_str(&format!(
                "| `{}` | {} | {read} | {} | {} | {:.2}× | {} |\n",
                result.arm.id,
                corpus.files,
                if read >= MAX_TOTAL_BYTES {
                    format!(
                        "**at/over the {} read ceiling**",
                        human_bytes(MAX_TOTAL_BYTES)
                    )
                } else {
                    "under".to_owned()
                },
                corpus.document_bytes(CORPUS_DIR),
                corpus.document_factor(CORPUS_DIR),
                result
                    .oracles
                    .explain_files_read
                    .map_or_else(|| "—".to_owned(), |n| n.to_string()),
            ));
        }
        // **The corroboration rule, turned on itself.** `explain_files_read` was `null` in
        // 96 of 96 opted-in arm-runs across 18 artifacts and the column rendered
        // `—` every time while saying nothing about it. So every "bytes read"
        // figure in this report — including the ceiling claim that anchored the
        // budget argument — was harness-side arithmetic confirmed by nothing the
        // product said, and the one probe built to catch exactly that never
        // answered. It is also the check that would have caught the
        // `MAX_TOTAL_BYTES` cut on its own. Silence is now stated where the
        // numbers are.
        let unconfirmed: Vec<String> = self
            .arms
            .iter()
            .filter(|a| a.arm.include.is_some() && a.oracles.explain_files_read.is_none())
            .map(|a| format!("`{}`", a.arm.id))
            .collect();
        if !unconfirmed.is_empty() {
            out.push_str(&format!(
                "\n**UNCONFIRMED BY THE PRODUCT: {}.** `files read (explain)` is `—` for these \
                 arms, so every byte figure in their rows is the harness's own arithmetic over \
                 the corpus it wrote — nothing the product reported back agrees with it. That \
                 is the corroboration probe failing *silently*, which is worse than no probe \
                 because it occupies the slot where a check would go. Read those byte columns \
                 as declarations, not measurements, until it answers.\n",
                unconfirmed.join(", ")
            ));
        }
        out.push_str(
            "\nThe factor here is near 1.0 because these lines are long. The 2× is the              pathological end — a file that is all newlines, where the one-space prefix is the              whole line — so a full budget hands on nearly twice the read ceiling. Neither end is the other, and this table says which one each arm is.\n\n`files read (explain)` is `rift checkpoint explain`'s **own** label, not a span              count: the inclusion walk's record count is on no CLI surface today, so the              harness publishes the figure the product publishes rather than relabelling it as              something it has not established.\n",
        );
    }

    /// Curve, units, and attribution in one table — what a decision needs.
    fn anatomy_section(&self, out: &mut String) {
        let rows = self.anatomy();
        if rows.is_empty() {
            return;
        }
        let breached: Vec<&str> = rows
            .iter()
            .filter(|r| r.breaches)
            .map(|r| r.arm.as_str())
            .collect();
        out.push_str(&format!(
            "\n## Anatomy — the curve, the units, and the phase that owns it\n\n{}\n\n**Magnitude and attribution come from different commits.** `git latency` is the \
             median of the timed commits minus the control's; every phase column comes from \
             the arm's one **extra untimed** commit with the timer on. Read the residue as an \
             anatomy of a representative commit beside the magnitude of the median, never as \
             an exact decomposition of one commit.\n\n`predict_id` here is **exclusive** — `redact` subtracted — so the columns are \
             disjoint and may be added.\n\n| arm | bytes read | handed on | ×read | git latency | hook bodies | outside \
             bodies | `redact` | `predict_id` less `redact` | verdict |\n|---|---|---|---|---|---|---|---|---|---|\n",
            if breached.is_empty() {
                "No arm is over the budget in this run.".to_owned()
            } else {
                format!(
                    "**BREACH: {}.** The budget is {} ms of git latency and is HARD (owner-set). \
                     The corpus was not adjusted in response — the arms are the \
                     sizes they were declared at.",
                    breached
                        .iter()
                        .map(|a| format!("`{a}`"))
                        .collect::<Vec<_>>()
                        .join(", "),
                    self.budget_ms
                )
            }
        ));
        for r in &rows {
            let us = |v: u64| format!("{:.2} ms", v as f64 / 1_000.0);
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                r.arm,
                r.bytes_read
                    .map_or_else(|| "—".to_owned(), |b| b.to_string()),
                r.document_bytes
                    .map_or_else(|| "—".to_owned(), |b| b.to_string()),
                r.document_factor
                    .map_or_else(|| "—".to_owned(), |f| format!("{f:.2}×")),
                us(r.git_latency_us),
                us(r.hook_bodies_us),
                us(r.outside_hook_bodies_us),
                us(r.redact_us),
                us(r.predict_id_excl_us),
                if r.breaches { "**BREACH**" } else { "meets" },
            ));
        }
        if rows.iter().any(|r| r.bodies_exceed_latency) {
            out.push_str(
                "\n**One arm's hook bodies measured larger than its median git latency.** \
                 That is possible across two populations — a slow phase commit against a fast \
                 median — and it is reported rather than clamped, because the alternative is a \
                 zero in the `outside bodies` column that would read as \"process creation is \
                 free\".\n",
            );
        }
        out.push_str(
            "\n`outside bodies` is measured rather than assumed: process creation, hook \
             dispatch, and git's own cost of running the three hooks. Fresh Unix installs use \
             direct executable links for unchained hooks; preserved user-hook chains and \
             non-Unix installs retain shell shims.\n",
        );
    }

    fn oracles_section(&self, out: &mut String) {
        out.push_str(
            "\n## Oracles — what each arm exercised\n\n\
             Reported for every arm including the control, because a check that only ever \
             confirms is not a check: the idle arm's **absent** trailer is what makes the live \
             arm's present one mean something.\n\n\
 | arm | projections | trailer | verify | events |\n|---|---|---|---|---|\n",
        );
        for a in &self.arms {
            let projections = if a.oracles.projections.is_empty() {
                "none".to_owned()
            } else {
                a.oracles
                    .projections
                    .iter()
                    .map(|(name, bytes)| format!("{name} {bytes} B"))
                    .collect::<Vec<_>>()
                    .join("; ")
            };
            let verify = match (a.oracles.verify_checkpoints, a.oracles.verify_shards) {
                (Some(c), Some(s)) => format!("{c} checkpoint(s), {s} shard(s)"),
                _ => "—".to_owned(),
            };
            out.push_str(&format!(
                "| `{}` | {projections} | {} | {verify} | {} |\n",
                a.arm.id,
                if a.oracles.trailer_present {
                    "present"
                } else {
                    "absent"
                },
                a.oracles
                    .captured_events
                    .map_or_else(|| "—".to_owned(), |n| n.to_string()),
            ));
        }
        if self.arms.iter().any(|a| a.oracles.projections.len() > 1) {
            out.push_str(
                "\n**More than one projection under a single arm's cache.** That is the shape \
                 of the defect this table exists for: the number may be about a store rift \
                 never opened.\n",
            );
        }
    }

    fn phases_section(&self, out: &mut String) {
        let any = self.arms.iter().any(|a| !a.phases.is_empty());
        if !any {
            return;
        }
        out.push_str(
            "\n## Phases\n\nFrom one extra **untimed** commit per arm with \
             `RIFT_HOOK_TIMING=1`, straight out of the shipped binary. Untimed because the \
             timer takes clock readings the shipped default never takes.\n\n\
             **Read the `exclusive` column, and never sum `recorded`.** The timer records on \
             completion, so a nested phase prints before its parent and the parent's figure \
             already contains it: `predict_id` INCLUDES `redact`. `recorded` is what the \
             binary printed; `exclusive` has every declared child subtracted, so those values \
             are disjoint and may be added. Summing `recorded` would double-count the \
             redaction — the one quantity the opted-in arms exist to separate.\n\n\
 | arm | hook | phase | recorded µs | exclusive µs |\n|---|---|---|---|---|\n",
        );
        for a in &self.arms {
            for (hook, phases) in &a.phases {
                let exclusive = exclusive_phases(phases);
                for (phase, us) in phases {
                    let excl = exclusive.get(phase).copied().unwrap_or(*us);
                    let note = if excl == *us {
                        String::new()
                    } else {
                        " (parent, `redact` subtracted)".to_owned()
                    };
                    out.push_str(&format!(
                        "| `{}` | {hook} | {phase} | {us} | {excl}{note} |\n",
                        a.arm.id
                    ));
                }
            }
        }
        self.phase_audit_section(out);
    }

    /// The free oracle on the phase table: `total_us` is measured independently
    /// of the phases, so disjoint phases can never exceed it.
    fn phase_audit_section(&self, out: &mut String) {
        out.push_str(
            "\n### Phase audit\n\n`total` is measured independently of the phases, so \
             disjoint phases can never exceed it. `over-attributed` is therefore a proof that \
             something is nested which the harness does not declare — not a rounding \
             remark.\n\n| arm | hook | total µs | attributed µs | unattributed µs | verdict \
 |\n|---|---|---|---|---|---|\n",
        );
        for a in &self.arms {
            for (hook, phases) in &a.phases {
                let audit = PhaseAudit::of(phases);
                out.push_str(&format!(
                    "| `{}` | {hook} | {} | {} | {} | {} |\n",
                    a.arm.id,
                    audit.total_us,
                    audit.attributed_us,
                    audit.unattributed_us,
                    if audit.over_attributed {
                        "**OVER-ATTRIBUTED — an undeclared nested phase**"
                    } else {
                        "adds up"
                    },
                ));
            }
        }
    }
}

/// Phase pairs where the first phase's recorded time **contains** the second's.
///
/// **This is the trap, and it aims at exactly the number this module was built
/// to isolate.** `HookTimer::phase` records on *completion*, so an inner phase
/// prints before its parent and the parent's figure already includes it.
/// `report_line`'s own doc invites summing by key — which is right for a
/// *repeated* key (a call site that ran twice, the finding that emitter exists
/// to surface) and wrong across a containment pair, where it double-counts.
///
/// `redact` inside `predict_id` is the instrument's first nested phase, so
/// nobody had hit this before. A sum would double-count the redaction: the
/// single quantity the opted-in arms exist to separate, in the one hook that
/// cannot detach. A benchmark whose total exceeds its parts by precisely the
/// quantity under investigation is the worst available failure mode here, so
/// containment is declared as data, subtracted mechanically, and **audited** —
/// an undeclared nesting shows up as over-attribution rather than as a quiet
/// overstatement (see [`PhaseAudit`]).
pub const NESTED_PHASES: &[(&str, &str)] = &[("predict_id", "redact")];

/// The key the emitter uses for the whole hook body.
const TOTAL_PHASE: &str = "total";

/// Phase times with every declared child subtracted from its parent, so the
/// values are **disjoint** and may be summed.
#[must_use]
pub fn exclusive_phases(phases: &BTreeMap<String, u64>) -> BTreeMap<String, u64> {
    let mut out = phases.clone();
    for (parent, child) in NESTED_PHASES {
        let Some(inner) = phases.get(*child).copied() else {
            continue;
        };
        if let Some(outer) = out.get_mut(*parent) {
            *outer = outer.saturating_sub(inner);
        }
    }
    out
}

/// Whether the phases of one hook add up, and what is left over.
///
/// The emitter's `total_us` is measured independently of the phases, which
/// makes it a free oracle on them: disjoint phases can never exceed it, so
/// `over_attributed` is a *proof* that something is nested which
/// [`NESTED_PHASES`] does not declare — the failure this whole mechanism
/// exists to make loud instead of quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PhaseAudit {
    /// The hook body's own measured total, microseconds.
    pub total_us: u64,
    /// Sum of the phases after subtracting declared nesting.
    pub attributed_us: u64,
    /// `total - attributed`: time inside the hook body that no phase names.
    pub unattributed_us: u64,
    /// `attributed > total` — impossible for disjoint phases, so an undeclared
    /// containment pair. Never rendered as a rounding remark.
    pub over_attributed: bool,
}

impl PhaseAudit {
    /// Audit one hook's phases.
    #[must_use]
    pub fn of(phases: &BTreeMap<String, u64>) -> Self {
        let total_us = phases.get(TOTAL_PHASE).copied().unwrap_or_default();
        let attributed_us: u64 = exclusive_phases(phases)
            .iter()
            .filter(|(name, _)| name.as_str() != TOTAL_PHASE)
            .map(|(_, us)| *us)
            .sum();
        Self {
            total_us,
            attributed_us,
            unattributed_us: total_us.saturating_sub(attributed_us),
            over_attributed: attributed_us > total_us,
        }
    }
}

/// Parse one `rift-timing hook=<name> total_us=… <phase>_us=…` line.
///
/// A repeated phase name is **summed**, not overwritten: the emitter documents
/// that a phase appearing twice means a call site that ran twice, and dropping
/// the second occurrence would hide exactly the finding the instrument exists
/// to surface.
#[must_use]
pub fn parse_timing_line(line: &str) -> Option<(String, BTreeMap<String, u64>)> {
    let rest = line.trim().strip_prefix("rift-timing ")?;
    let mut hook = None;
    let mut phases: BTreeMap<String, u64> = BTreeMap::new();
    for field in rest.split_whitespace() {
        let (key, value) = field.split_once('=')?;
        if key == "hook" {
            hook = Some(value.to_owned());
            continue;
        }
        let Some(phase) = key.strip_suffix("_us") else {
            continue;
        };
        let Ok(us) = value.parse::<u64>() else {
            continue;
        };
        *phases.entry(phase.to_owned()).or_default() += us;
    }
    Some((hook?, phases))
}

/// Collect every timing line in `stderr`, keyed by hook.
#[must_use]
pub fn parse_timings(stderr: &str) -> BTreeMap<String, BTreeMap<String, u64>> {
    let mut out: BTreeMap<String, BTreeMap<String, u64>> = BTreeMap::new();
    for line in stderr.lines() {
        if let Some((hook, phases)) = parse_timing_line(line) {
            let entry = out.entry(hook).or_default();
            for (phase, us) in phases {
                *entry.entry(phase).or_default() += us;
            }
        }
    }
    out
}

/// A byte count as prose (`8 KiB`, `1 MiB`, `2,048 B`).
///
/// Exists because three generated sentences hard-coded `1 MiB` and went on
/// saying it after the constant became `8 * 1024` — one of them contradicting
/// itself 48 lines later in the same generated section. A cap that appears in
/// prose must be **formatted from the constant**, or the prose is a second
/// declaration of it that nothing keeps in step.
#[must_use]
pub fn human_bytes(n: usize) -> String {
    if n >= 1024 * 1024 && n.is_multiple_of(1024 * 1024) {
        format!("{} MiB", n / (1024 * 1024))
    } else if n >= 1024 && n.is_multiple_of(1024) {
        format!("{} KiB", n / 1024)
    } else {
        format!("{} B", thousands(n))
    }
}

/// `8192` -> `"8,192"`. Correct for every magnitude, which
/// `format!("{},{:03}", n / 1000, n % 1000)` was not: it rendered 512 as
/// `"0,512"`, and a guard comparing against that silently never matched.
#[must_use]
pub fn thousands(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Every `*.db` under `cache_root`, by file name, with its size.
#[must_use]
pub fn projection_sizes(cache_root: &Path) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    let mut stack = vec![cache_root.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(entries) = fs::read_dir(&path) {
                stack.extend(entries.flatten().map(|e| e.path()));
            }
            continue;
        }
        if path.extension().and_then(|e| e.to_str()) == Some("db") {
            let name = stable_cache_key(
                &path
                    .strip_prefix(cache_root)
                    .unwrap_or(&path)
                    .display()
                    .to_string(),
            );
            out.insert(name, meta.len());
        }
    }
    out
}

/// Replace a volatile sandbox repo-id path segment with a stable placeholder.
///
/// **The measurement here is the byte count; the id was an accident of the
/// key.** `resolve_cache_root` keys per repository, so a projection's path
/// carries the 32-hex id of the throwaway sandbox the run created — an
/// identifier for a directory that stopped existing when the run ended, and a
/// different one on every run.
///
/// Recording it cost three things. It put 224 high-entropy tokens into the
/// tracked corpus that `rift-shadow-redact`'s false-positive gate walks, for no
/// provenance value. It made **two runs of the same arm textually
/// undiffable**, since every run differed from every other purely by sandbox
/// id — which degrades the ratchet's own comparison. And it invited the fix of
/// raising a security crate's ceiling to accommodate noise.
///
/// Collapsing is lossless: within one arm's map there is exactly one
/// `rift/<id>/projection.db` beside `rift/canopy/ui-state.db`, so the
/// placeholder collides with nothing and drops no row.
#[must_use]
pub fn stable_cache_key(relative: &str) -> String {
    relative
        .split('/')
        .map(|seg| {
            if seg.len() >= 32 && seg.chars().all(|c| c.is_ascii_hexdigit()) {
                "<repo-id>"
            } else {
                seg
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Read `re-derived N checkpoint(s) … M shard(s)` out of `rift verify`.
#[must_use]
pub fn parse_verify(stdout: &str) -> (Option<u64>, Option<u64>) {
    let checkpoints = stdout
        .split_once("re-derived ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok());
    let shards = stdout
        .split_once("; ")
        .and_then(|(_, rest)| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok());
    (checkpoints, shards)
}

/// Whether a commit message carries the checkpoint trailer.
#[must_use]
pub fn has_trailer(message: &str) -> bool {
    message.lines().any(|l| l.trim_start().starts_with(TRAILER))
}

/// The hook events that put a live session in front of the timed commit.
///
/// A session that opened but never took a turn condenses nothing, so a "live"
/// arm built from `SessionStart` alone would time the idle path while calling
/// it live — the flattering-number failure this whole module is built around.
const LIVE_PRELUDE: &[HookEvent] = &[
    HookEvent::SessionStart,
    HookEvent::PromptSubmit,
    HookEvent::ToolCallBash,
    HookEvent::ToolResult,
];

/// The one file every arm's repository is seeded with, and therefore the only
/// path a read-shaped call can name here and resolve.
///
/// Spelled once: [`SEEDED_FILE`] is written by `drive`, read by
/// [`read_call`], and the pair is what makes `checkpoint explain` answerable.
pub const SEEDED_FILE: &str = "README.md";

/// A **completed** read of [`SEEDED_FILE`]: the `PreToolUse` and the
/// `PostToolUse` that finishes it.
///
/// **This closes a probe that reported nothing for eighteen runs, and neither
/// half of the cause was a wiring bug.** `explain_files_read` was `null` in 135
/// of 135 recorded values, and running the sweep and then `checkpoint explain`
/// by hand in the arm's own sandbox found two things:
///
/// 1. the live prelude's only tool call was a **Bash** command, and a shell
///    command reads no file, so the checkpoint carried no `digest.shape.json`
///    — which is the only thing `rift checkpoint explain` prints its
///    `files read:` line from;
/// 2. a `PreToolUse` **alone** is not enough either. Measured, both ways, in
///    two sandboxes minutes apart: pre-only yields `digests: (none archived)`,
///    pre-plus-post yields `digests: digest.shape.json` and `files read: 1`.
///    The shape is recorded when the call *completes*, which is the right
///    behaviour and is why the pair travels together.
///
/// Both calls are part of the **untimed prelude**, so no published timing
/// moves; what changes is that the session the timed commit meets has now
/// finished reading a file, which is also the more representative session.
fn read_pair(binary: &Path, repo: &Path, session: &str, nth: usize) -> [Invocation; 2] {
    let input = serde_json::json!({ "file_path": SEEDED_FILE, "offset": nth });
    let call = |verb: &str, extra: Option<serde_json::Value>| {
        let mut body = serde_json::json!({
            "session_id": session,
            "hook_event_name": verb,
            "tool_name": "Read",
            "tool_input": input,
        });
        if let (Some(map), Some(response)) = (body.as_object_mut(), extra) {
            map.insert("tool_response".to_owned(), response);
        }
        Invocation::new(binary, repo)
            .args(["hooks", "claude-code", verb])
            .stdin(body.to_string())
    };
    [
        call("PreToolUse", None),
        call(
            "PostToolUse",
            Some(serde_json::json!({ "stdout": "arm\n" })),
        ),
    ]
}

/// Drive one arm and return its result. Never returns `Err` for an arm-local
/// failure: that is recorded on the arm so the other arms still publish.
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
            oracles: Oracles::default(),
            attestation: Some(crate::attest::Attestation::take()),
            phases: BTreeMap::new(),
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
    // Read before any of this arm's work, so the number describes the machine
    // the samples were taken on rather than the one they left behind.
    let attestation = Some(crate::attest::Attestation::take());
    let sandbox = root.join(&arm.id);
    let repo = sandbox.join("repo");
    fs::create_dir_all(&repo).map_err(|e| BenchError::io("create arm repo", e))?;
    let env = scenario::leg_env(LegId::Rift, &sandbox);

    git(hermetic, &repo, &["init", "--quiet"], &env)?;
    fs::write(repo.join(SEEDED_FILE), "arm\n").map_err(|e| BenchError::io("seed file", e))?;
    git(hermetic, &repo, &["add", "--", SEEDED_FILE], &env)?;
    if let Some(corpus) = arm.include {
        write_corpus(&repo, corpus)?;
        // By name, one at a time. `git add -A` would sweep the corpus in too,
        // and the corpus being *outside* git is the point of the arm.
        for name in [".gitignore", scenario::INCLUDE_FILE] {
            git(hermetic, &repo, &["add", "--", name], &env)?;
        }
    }
    git(hermetic, &repo, &["commit", "--quiet", "-m", "seed"], &env)?;

    if arm.capture != Capture::Off {
        let init = with_env(scenario::setup_plan(LegId::Rift, binary, &repo), &env);
        let exec = run_once(hermetic, &init)?;
        if !exec.sample.ok() {
            return Err(BenchError::Subprocess {
                program: binary.display().to_string(),
                args: init.args.join(" "),
                status: format!("{:?}", exec.sample.exit_code),
                stderr: exec.stderr.chars().take(400).collect(),
            });
        }
    }

    let mut n = 0usize;
    let samples = measure(hermetic, protocol, |i| {
        prepare_commit(hermetic, binary, arm, &repo, &sandbox, &env, i)?;
        n = i;
        Ok(commit_invocation(&repo, i, &env, false))
    })?;

    // One more commit, untimed, with the phase timer on.
    let phases = {
        prepare_commit(hermetic, binary, arm, &repo, &sandbox, &env, n + 1)?;
        let inv = commit_invocation(&repo, n + 1, &env, true);
        run_once(hermetic, &inv)
            .map(|exec| parse_timings(&exec.stderr))
            .unwrap_or_default()
    };

    let oracles = oracles(hermetic, binary, arm, &repo, &sandbox, &env);
    Ok(ArmResult {
        arm: arm.clone(),
        stats: Stats::of(&samples),
        error: None,
        oracles,
        attestation,
        phases,
    })
}

/// Everything that is not the commit: the file, the staging, and the session
/// state a live arm's commit has to meet.
fn prepare_commit(
    hermetic: &Hermetic,
    binary: &Path,
    arm: &Arm,
    repo: &Path,
    sandbox: &Path,
    env: &[(String, String)],
    i: usize,
) -> Result<()> {
    if arm.capture == Capture::Live {
        let session = scenario::iteration_session(i);
        let ctx = PlanCtx {
            leg: LegId::Rift,
            binary,
            repo,
            sandbox,
            iteration: i,
            // The git-latency sweep drives hooks directly; it never scripts a
            // session through the fake agent.
            tool: None,
        };
        let transcript = ctx.transcript_path(&session);
        if let Some(parent) = transcript.parent() {
            fs::create_dir_all(parent).map_err(|e| BenchError::io("transcript dir", e))?;
        }
        fs::write(&transcript, scenario::transcript_body(8))
            .map_err(|e| BenchError::io("write transcript", e))?;
        for event in LIVE_PRELUDE {
            let inv = with_env(event.invocation(&ctx, &session, i), env);
            run_once(hermetic, &inv)?;
        }
        // One COMPLETED read, so the checkpoint carries a shape digest and the
        // `explain` oracle has something to answer with. See `read_pair`.
        for inv in read_pair(binary, repo, &session, i) {
            run_once(hermetic, &with_env(inv, env))?;
        }
    }
    let name = format!("step_{i}.txt");
    fs::write(repo.join(&name), format!("commit {i}\n"))
        .map_err(|e| BenchError::io("write commit file", e))?;
    // By name, never `git add -A`: on this machine a broad add resurrects paths
    // another agent deleted.
    git(hermetic, repo, &["add", "--", &name], env)?;
    Ok(())
}

fn commit_invocation(repo: &Path, i: usize, env: &[(String, String)], timing: bool) -> Invocation {
    let mut inv = Invocation::new("git", repo)
        .args(["commit", "--quiet", "-m", &format!("step {i}")])
        .env("GIT_TERMINAL_PROMPT", "0");
    if timing {
        inv = inv.env("RIFT_HOOK_TIMING", "1");
    }
    with_env(inv, env)
}

fn oracles(
    hermetic: &Hermetic,
    binary: &Path,
    arm: &Arm,
    repo: &Path,
    sandbox: &Path,
    env: &[(String, String)],
) -> Oracles {
    let message = git(hermetic, repo, &["log", "-1", "--format=%B"], env).unwrap_or_default();
    let mut oracles = Oracles {
        projections: projection_sizes(&sandbox.join("rift-cache")),
        trailer_present: has_trailer(&message),
        ..Oracles::default()
    };
    if arm.capture == Capture::Off {
        return oracles;
    }
    if let Ok(exec) = run_once(
        hermetic,
        &with_env(Invocation::new(binary, repo).args(["verify"]), env),
    ) {
        let (checkpoints, shards) = parse_verify(&exec.stdout);
        oracles.verify_checkpoints = checkpoints;
        oracles.verify_shards = shards;
    }
    if let Ok(exec) = run_once(
        hermetic,
        &with_env(Invocation::new(binary, repo).args(["status"]), env),
    ) {
        oracles.captured_events = scenario::captured_events(&exec.stdout);
    }
    if let Ok(exec) = run_once(
        hermetic,
        &with_env(
            Invocation::new(binary, repo).args(["checkpoint", "explain"]),
            env,
        ),
    ) {
        oracles.explain_files_read = parse_explain_files(&exec.stdout);
    }
    oracles
}

/// Read `files read: N` out of `rift checkpoint explain`.
#[must_use]
pub fn parse_explain_files(stdout: &str) -> Option<u64> {
    stdout
        .lines()
        .find_map(|l| l.trim().strip_prefix("files read:"))
        .and_then(|rest| rest.trim().parse().ok())
}

/// Write the opted-in corpus, the `.gitignore` that keeps it out of git, and
/// the `.worktreeinclude` that opts it back in.
///
/// Git-ignored on purpose: the whole point of `.worktreeinclude` is to fold a
/// path git does *not* track back into capture, so a corpus git tracked would
/// exercise a different path fully and would also be committed by every
/// timed commit.
fn write_corpus(repo: &Path, corpus: Corpus) -> Result<()> {
    let dir = repo.join(CORPUS_DIR);
    fs::create_dir_all(&dir).map_err(|e| BenchError::io("corpus dir", e))?;
    // `line_width: 0` yields a file of bare newlines — the 2N + 2 worst case
    // the ceiling's own doc names, not a degenerate input the harness invented.
    let line = format!("{}\n", "x".repeat(corpus.line_width));
    let body = line.repeat(corpus.lines_per_file);
    for i in 0..corpus.files {
        fs::write(dir.join(format!("f{i:04}.txt")), &body)
            .map_err(|e| BenchError::io("corpus file", e))?;
    }
    fs::write(repo.join(".gitignore"), format!("/{CORPUS_DIR}/\n"))
        .map_err(|e| BenchError::io("corpus gitignore", e))?;
    fs::write(
        repo.join(scenario::INCLUDE_FILE),
        format!("{CORPUS_DIR}/\n"),
    )
    .map_err(|e| BenchError::io("worktreeinclude", e))
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

/// What `git --version` says, so the control's identity travels with it.
#[must_use]
pub fn git_version(hermetic: &Hermetic, cwd: &Path) -> String {
    run_once(hermetic, &Invocation::new("git", cwd).args(["--version"]))
        .ok()
        .and_then(|exec| exec.stdout.lines().next().map(str::trim).map(str::to_owned))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_owned())
}

/// Drive the sweep and produce the artifact.
///
/// The control runs whether or not `only` names it: an arm quoted against a
/// control from another run is the error the same-run control rule exists to prevent, and the
/// filter exists to re-measure one arm cheaply, not to drop its yardstick.
///
/// # Errors
/// Only for failures that invalidate the whole sweep (no run root). A single
/// arm's failure is recorded on that arm.
pub fn run_sweep(
    hermetic: &Hermetic,
    resolver: &LegResolver,
    run_root: &Path,
    protocol: Protocol,
    only: &[String],
) -> Result<HookLatencyReport> {
    fs::create_dir_all(run_root).map_err(|e| BenchError::io("create sweep root", e))?;
    let subject = resolver.resolve(hermetic, LegId::Rift);
    let selected: Vec<Arm> = arms()
        .into_iter()
        .filter(|a| only.is_empty() || a.is_control() || only.contains(&a.id))
        .collect();
    let results = match subject.binary() {
        Some(binary) => selected
            .iter()
            .map(|arm| run_arm(hermetic, binary, arm, run_root, protocol))
            .collect(),
        None => selected
            .iter()
            .map(|arm| ArmResult {
                arm: arm.clone(),
                stats: None,
                error: Some("no subject binary; see the Subject section".to_owned()),
                oracles: Oracles::default(),
                attestation: None,
                phases: BTreeMap::new(),
            })
            .collect(),
    };
    Ok(HookLatencyReport {
        schema_version: HOOK_LATENCY_SCHEMA,
        harness_version: env!("CARGO_PKG_VERSION").to_owned(),
        generated_at_unix: now_unix(),
        host: Host::detect("n/a (git-latency sweep)".to_owned()),
        subject,
        source_clock: resolver.source_clock.clone(),
        git_version: git_version(hermetic, run_root),
        protocol,
        budget_ms: BUDGET_MS,
        arms: results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_control_is_the_only_arm_with_capture_off_and_it_is_first() {
        let arms = arms();
        assert!(arms[0].is_control(), "the control leads the sweep");
        assert_eq!(
            arms.iter().filter(|a| a.is_control()).count(),
            1,
            "exactly one control: an arm quoted against two is quoted against neither"
        );
    }

    #[test]
    fn a_timing_line_is_parsed_and_a_repeated_phase_is_summed_not_dropped() {
        let (hook, phases) = parse_timing_line(
            "rift-timing hook=prepare-commit-msg total_us=11760 discover_us=120 \
             open_projection_us=2030 predict_id_us=5710 rule_packs_us=10 rule_packs_us=15",
        )
        .unwrap();
        assert_eq!(hook, "prepare-commit-msg");
        assert_eq!(phases.get("predict_id"), Some(&5710));
        assert_eq!(phases.get("open_projection"), Some(&2030));
        // The emitter's own contract: a phase twice means a call site twice.
        assert_eq!(phases.get("rule_packs"), Some(&25));
        assert_eq!(phases.get("total"), Some(&11760));
        assert!(parse_timing_line("some other stderr noise").is_none());
    }

    #[test]
    fn all_three_commit_hooks_are_collected_from_one_commits_stderr() {
        let stderr = "hint: something\n\
             rift-timing hook=prepare-commit-msg total_us=8710 predict_id_us=5690\n\
             rift-timing hook=commit-msg total_us=140\n\
             rift-timing hook=post-commit total_us=2650 spawn_worker_us=1160\n";
        let timings = parse_timings(stderr);
        assert_eq!(timings.len(), 3);
        assert_eq!(timings["post-commit"]["spawn_worker"], 1160);
        assert_eq!(timings["prepare-commit-msg"]["predict_id"], 5690);
    }

    /// The reviewer's caveat, made mechanical. `predict_id` contains
    /// `redact`, so the naive sum over-reports by exactly the redaction — the
    /// one quantity the opted-in arms exist to isolate.
    #[test]
    fn summing_the_recorded_phases_double_counts_the_redaction_and_exclusive_does_not() {
        let (_, phases) = parse_timing_line(
            "rift-timing hook=prepare-commit-msg total_us=11760 discover_us=120 \
             open_projection_us=2030 redact_us=5000 predict_id_us=5710",
        )
        .unwrap();

        // What a reader summing `report_line` by key would get.
        let naive: u64 = phases
            .iter()
            .filter(|(k, _)| k.as_str() != "total")
            .map(|(_, v)| *v)
            .sum();
        assert_eq!(naive, 12_860);
        assert!(
            naive > phases["total"],
            "the naive sum exceeds the independently measured total by the redaction"
        );
        assert_eq!(naive - 5_000, 7_860, "the excess is exactly `redact`");

        let exclusive = exclusive_phases(&phases);
        assert_eq!(exclusive["predict_id"], 710, "the parent loses its child");
        assert_eq!(exclusive["redact"], 5_000, "the child is untouched");
        assert_eq!(exclusive["open_projection"], 2_030, "siblings untouched");

        let audit = PhaseAudit::of(&phases);
        assert_eq!(audit.total_us, 11_760);
        assert_eq!(audit.attributed_us, 7_860);
        assert_eq!(audit.unattributed_us, 3_900);
        assert!(
            !audit.over_attributed,
            "disjoint phases fit inside the total"
        );
    }

    /// The audit is an oracle, not a formatting nicety: `total_us` is measured
    /// independently, so disjoint phases exceeding it *proves* an undeclared
    /// containment pair rather than suggesting one.
    #[test]
    fn an_undeclared_nested_phase_shows_up_as_over_attribution() {
        let (_, phases) =
            parse_timing_line("rift-timing hook=post-commit total_us=100 a_us=80 b_us=80").unwrap();
        let audit = PhaseAudit::of(&phases);
        assert!(
            audit.over_attributed,
            "160 µs of phases cannot fit in a 100 µs body: {audit:?}"
        );
        assert_eq!(audit.unattributed_us, 0, "saturating, never negative");
    }

    /// A build that has not landed the nested phase yet must be unaffected:
    /// subtracting a child that was never recorded is a no-op, not a hole.
    #[test]
    fn a_hook_without_the_nested_phase_is_left_exactly_as_it_was() {
        let (_, phases) = parse_timing_line(
            "rift-timing hook=prepare-commit-msg total_us=8710 predict_id_us=5690",
        )
        .unwrap();
        assert_eq!(exclusive_phases(&phases), phases);
        assert!(!PhaseAudit::of(&phases).over_attributed);
    }

    /// The probe that reported nothing for eighteen runs, and why.
    ///
    /// `explain_files_read` was `null` in 135 of 135 recorded values. The cause
    /// was not a parser and not a wiring bug: the live prelude's only tool call
    /// was a **Bash** command, a shell command reads no file, so the checkpoint
    /// carried no `digest.shape.json` — and `rift checkpoint explain` prints
    /// its `files read:` line only when that digest exists. The arm never gave
    /// the product a question to answer. This pins the read-shaped call that
    /// closes it, and pins that it names the file the arm is actually seeded
    /// with, because a read of a path that is not there resolves to no fact and
    /// puts the column straight back to `null`.
    #[test]
    fn the_live_prelude_reads_a_file_the_arm_really_has() {
        let pair = read_pair(
            Path::new("/bin/rift"),
            Path::new("/tmp/repo"),
            "01JBENCH000000000000000000",
            7,
        );
        let verbs: Vec<&str> = pair.iter().map(|i| i.args[2].as_str()).collect();
        // The PAIR, not the call: measured here, a PreToolUse alone archives no
        // shape digest at all and the column goes straight back to null.
        assert_eq!(verbs, ["PreToolUse", "PostToolUse"]);
        for inv in &pair {
            let payload: serde_json::Value =
                serde_json::from_str(inv.stdin.as_deref().unwrap()).unwrap();
            assert_eq!(payload["tool_name"], "Read");
            assert_eq!(payload["tool_input"]["file_path"], SEEDED_FILE);
            assert_eq!(payload["tool_input"]["offset"], 7);
        }
        let done: serde_json::Value =
            serde_json::from_str(pair[1].stdin.as_deref().unwrap()).unwrap();
        assert!(
            done["tool_response"].is_object(),
            "a completed call carries its result"
        );

        // The pair that has to stay together: the file the arm seeds and the
        // file the read names are one constant, not two literals.
        assert_eq!(SEEDED_FILE, "README.md");

        // The prelude before it is unchanged, and none of its calls is a read:
        // that is the defect, kept visible rather than edited away.
        assert_eq!(LIVE_PRELUDE.len(), 4);
        assert!(
            !LIVE_PRELUDE.contains(&HookEvent::ToolCallRead),
            "the Bash-only prelude is why the probe was silent"
        );
    }

    /// The parser was never the problem, and this is the shape it reads —
    /// taken verbatim from a real `rift checkpoint explain` run in an arm
    /// sandbox on this machine.
    #[test]
    fn the_explain_parser_reads_the_real_verbs_real_output() {
        let real = "checkpoint 1a15bfb0fc3d45cf9aaaae4726740e8f85d3d74a66e47afef713d3f8ff11451f\n                      anchored: 6a87047dc463  strategy: manual-commit  canon: 3  created_at_ms: -1\n                      digests: digest.shape.json\n  files read: 1\n    README.md\n";
        assert_eq!(parse_explain_files(real), Some(1));
        // And the output the eighteen recorded runs actually got: a checkpoint
        // with no shape digest, so no line to read.
        let without = "checkpoint 7964925569b1dee3\n  digests: digest.intent.json · digest.log.json\n                         prompt (redacted, transcript line 0):\n";
        assert_eq!(parse_explain_files(without), None);
    }

    #[test]
    fn verify_output_yields_both_counts_and_neither_when_it_did_not_run() {
        let (checkpoints, shards) = parse_verify(
            "verify: OK — re-derived 26 checkpoint(s) from refs/rift/chain/v1; \
             26 shard(s) redaction/Merkle-checked\n",
        );
        assert_eq!((checkpoints, shards), (Some(26), Some(26)));
        assert_eq!(parse_verify(""), (None, None));
    }

    #[test]
    fn the_trailer_oracle_reads_the_trailer_and_not_a_mention_of_it() {
        assert!(has_trailer("step 3\n\nRift-Checkpoint: 01JABC\n"));
        assert!(!has_trailer("step 3\n"));
        assert!(
            !has_trailer("step 3\n\nthis commit talks about Rift-Checkpoint trailers\n"),
            "a body that mentions the trailer does not carry one"
        );
    }

    /// The §8 defect, in test form: a second projection under one arm's cache
    /// is the shape of "the number is about a store rift never opened", so the
    /// oracle enumerates rather than counting.
    #[test]
    fn every_projection_under_an_arms_cache_is_enumerated_with_its_size() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = tmp.path().join("rift-cache");
        fs::create_dir_all(cache.join("rift/abc")).unwrap();
        fs::write(cache.join("rift/abc/projection.db"), vec![0u8; 4096]).unwrap();
        fs::write(cache.join("stray.db"), vec![0u8; 128]).unwrap();
        fs::write(cache.join("notes.txt"), b"ignored").unwrap();

        let found = projection_sizes(&cache);
        assert_eq!(found.len(), 2, "both, so a stray one is visible: {found:?}");
        assert_eq!(found.get("stray.db"), Some(&128));
        assert_eq!(found.get("rift/abc/projection.db"), Some(&4096));
        assert!(projection_sizes(&tmp.path().join("nope")).is_empty());
    }

    /// **The guard that would have caught the whole B1 ripple by itself.**
    /// The old arm set survived a 128× cap cut as *declarations* — the numbers
    /// still parsed, the tests still named them, and only three assertions
    /// about one constant reddened. What nothing checked was whether the
    /// product would actually read the corpora those arms declare. Under the
    /// new caps all but `F1_newline_8k` would have been declined — most by the
    /// per-file limit, `F2`/`F3`/`F4` by the total budget instead — while their
    /// labels still said 1,048,576 B.
    #[test]
    fn every_declared_corpus_is_capturable_under_the_products_caps() {
        for arm in arms() {
            let Some(c) = arm.include else { continue };
            assert!(
                c.bytes_per_file() <= MAX_FILE_BYTES,
                "{}: {} B per file is over MAX_FILE_BYTES ({MAX_FILE_BYTES}); every file \
                 would be DECLINED BEFORE IT IS OPENED and the arm would time a walk that \
                 captured nothing while its label claimed {} B",
                arm.id,
                c.bytes_per_file(),
                c.bytes(),
            );
            assert!(
                c.bytes() <= MAX_TOTAL_BYTES,
                "{}: {} B is over MAX_TOTAL_BYTES ({MAX_TOTAL_BYTES}); the walk would stop \
                 partway and the arm's declared size would be fiction",
                arm.id,
                c.bytes(),
            );
            assert!(c.files <= MAX_ENTRIES, "{}: over MAX_ENTRIES", arm.id);
            assert!(c.is_capturable(), "{}", arm.id);
        }
    }

    /// Pairs under the new cap: same bytes read, 1.0× against 2.0× handed on,
    /// so the only independent variable is what the redactor is given.
    ///
    /// **Corrected wording, per the verifier:** the twins also differ in line
    /// *count* by 64×, which is inseparable from the mechanism — the 2× **is**
    /// the per-line prefix. "Exactly one variable" was wrong; "one
    /// *independent* variable" is right.
    #[test]
    fn the_cap_sized_pairs_read_the_same_bytes_and_hand_on_twice_as_much() {
        let all = arms();
        let rung = |id: &str| all.iter().find(|a| a.id == id).unwrap().include.unwrap();
        for (cheap, worst, bytes) in [
            ("G1_line_2k", "H1_newline_2k", 2_048),
            ("G2_line_4k", "H2_newline_4k", 4_096),
            ("G3_line_8k", "H3_newline_8k", MAX_TOTAL_BYTES),
        ] {
            let (c, w) = (rung(cheap), rung(worst));
            assert_eq!(c.bytes(), bytes, "{cheap}");
            assert_eq!(w.bytes(), bytes, "{worst} must read its twin's bytes");
            assert!(
                c.document_factor(CORPUS_DIR) < 1.35,
                "{cheap} is the cheap side"
            );
            assert!(
                w.document_factor(CORPUS_DIR) > 1.9,
                "{worst} is the 2N + 2 end"
            );
            assert!(c.is_capturable() && w.is_capturable());
        }
        // The top pair sits on the new ceiling exactly, not near it.
        assert_eq!(rung("G3_line_8k").bytes(), MAX_TOTAL_BYTES);
        assert_eq!(rung("H3_newline_8k").bytes(), MAX_TOTAL_BYTES);
    }

    /// **§1 of the tracked artifact describes this instrument, so it is pinned
    /// to it — line by line, arm to size.**
    ///
    /// The first version of this pin searched the **whole 750-line document**
    /// and asserted the arm id and the byte count *independently*, so nothing
    /// bound an arm to its size. A verifier rewrote §1's ceiling row to a
    /// fabricated `9,999` and **the pin passed**, because `8,192` still
    /// occurred elsewhere in the document. It also fell through a broken
    /// thousands formatter that rendered 512 as `"0,512"`.
    ///
    /// So: §1 is sliced out first, the arm's **own line** must carry its size,
    /// and there is no whole-document escape hatch. Calibrated by planting a
    /// fabricated size in §1 rather than by reading this code — which is the
    /// discipline the sentence describing a guard needs and did not get twice.
    ///
    /// # Declared gaps — what this guard does NOT catch
    ///
    /// It is **line-scoped string presence**, nothing stronger, and two plants
    /// pass it. Both were demonstrated, not reasoned to:
    ///
    /// 1. **Collapsing the three table rows into one line** carrying all six
    ///    arm ids and all three sizes. Every arm then finds "its" line and that
    ///    line contains "its" size, so the binding this guard was rebuilt to
    ///    enforce is satisfied by a line that binds nothing.
    /// 2. **An additive contradiction** — `| 8,192 superseded, now 9,999 (the
    ///    ceiling exactly) |`. The required substring is still present, so the
    ///    guard stays green while a reader takes away 9,999.
    ///
    /// Closing either needs a parser over the table rather than a substring
    /// search, and that is not written. The docstring above is literally true
    /// of what the code does; this section exists because true-but-narrow is
    /// still a gap, and a gap is declared by enumeration or it is a hole
    /// nobody knows about.
    #[test]
    fn section_one_states_the_arms_the_instrument_actually_runs() {
        let doc = crate::leg::default_repo_root().join("benchmarks/hook-latency.md");
        let text = match fs::read_to_string(&doc) {
            Ok(text) => text,
            Err(e) => {
                assert!(
                    !crate::leg::default_repo_root().join("benchmarks").is_dir(),
                    "{} is unreadable ({e}) but benchmarks/ exists — the guard would skip \
                     itself",
                    doc.display()
                );
                return;
            }
        };
        // §1 only. A claim about §1 checked against the whole document is the
        // vacuity this test was rebuilt to close.
        let from = text
            .find("## 1. Protocol")
            .expect("the artifact has a §1 Protocol");
        let to = text[from..]
            .find("\n## 2.")
            .map_or(text.len(), |o| from + o);
        let section = &text[from..to];
        assert!(
            section.len() < text.len(),
            "the slice must not be the document"
        );

        for arm in arms() {
            let Some(c) = arm.include else { continue };
            let needle = format!("`{}`", arm.id);
            let line = section
                .lines()
                .find(|l| l.contains(&needle))
                .unwrap_or_else(|| panic!("§1 does not name `{}`", arm.id));
            let size = thousands(c.bytes());
            assert!(
                line.contains(&size),
                "§1's line for `{}` states no size of {size} B — an arm and a number that are \
                 merely both present somewhere bind nothing:\n{line}",
                arm.id
            );
        }
        for stale in ["51,200", "256,000", "1,048,576"] {
            assert!(
                !section.contains(stale),
                "§1 still advertises a deleted arm's size ({stale})"
            );
        }
    }

    #[test]
    fn the_ceiling_and_the_control_file_still_match_the_product() {
        let root = crate::leg::default_repo_root();
        let inclusion = root.join("crates/rift-shadow/src/inclusion.rs");
        // A checkout without the product tree cannot check this — but the
        // skip is bounded, not free: if `crates/` is there the file must be
        // readable, so the guard cannot be silently disarmed by a move.
        let text = match fs::read_to_string(&inclusion) {
            Ok(text) => text,
            Err(e) => {
                assert!(
                    !root.join("crates").is_dir(),
                    "the product tree is here but {} is not readable ({e}) — the guard \
                     would have skipped itself",
                    inclusion.display()
                );
                return;
            }
        };
        assert!(
            text.contains("pub const MAX_TOTAL_BYTES: usize = 8 * 1024;"),
            "the read ceiling moved in the product; this harness still says {MAX_TOTAL_BYTES}"
        );
        assert!(
            text.contains("pub const MAX_FILE_BYTES: u64 = MAX_TOTAL_BYTES as u64 / 2;"),
            "the per-file limit moved; arms sized against {MAX_FILE_BYTES} would be declined"
        );
        // Restated in this harness and, until now, pinned by nothing — the same
        // unpinned-restatement class the per-file limit was just fixed for.
        assert!(
            text.contains(&format!("pub const MAX_ENTRIES: usize = {MAX_ENTRIES};")),
            "the record cap moved in the product; this harness still says {MAX_ENTRIES}"
        );
        assert!(
            text.contains(&format!(
                "pub const INCLUDE_FILE: &str = \"{}\";",
                crate::scenario::INCLUDE_FILE
            )),
            "the opt-in control file was renamed; the arms would opt nothing in"
        );
    }

    /// The probe was `null` in 96 of 96 opted-in arm-runs and the report said
    /// nothing. A probe that fails silently is worse than no probe: it occupies
    /// the slot where a check would go.
    #[test]
    fn an_unanswered_product_probe_is_stated_loudly_rather_than_rendered_as_a_dash() {
        let mut report = tests_support::report_with(&[("A_bare", 37), ("H3_newline_8k", 200)]);
        let md = report.to_markdown();
        assert!(md.contains("UNCONFIRMED BY THE PRODUCT"), "{md}");
        assert!(md.contains("`H3_newline_8k`"), "{md}");
        assert!(
            md.contains("declarations, not measurements"),
            "it must say what the byte columns are worth: {md}"
        );

        // When the product does answer, the warning goes away — a notice that
        // is always printed is wallpaper.
        report
            .arms
            .iter_mut()
            .find(|a| a.arm.id == "H3_newline_8k")
            .unwrap()
            .oracles
            .explain_files_read = Some(8);
        let md = report.to_markdown();
        assert!(!md.contains("UNCONFIRMED BY THE PRODUCT"), "{md}");
    }

    /// The generated ceiling sentence must be **formatted from the constant**,
    /// not restated beside it.
    ///
    /// It was restated, and it went on saying `1 MiB` after the constant became
    /// `8 * 1024` — contradicting itself 48 lines later in the same generated
    /// section. A cap that appears in prose is a second declaration of it, and
    /// nothing was keeping the two in step.
    #[test]
    fn the_generated_ceiling_prose_is_derived_from_the_constant() {
        let report = tests_support::report_with(&[("A_bare", 37), ("H3_newline_8k", 200)]);
        let md = report.to_markdown();
        let cap = human_bytes(MAX_TOTAL_BYTES);
        assert!(
            md.contains(&format!("`MAX_TOTAL_BYTES` is {cap}")),
            "the corpus header must state the live cap ({cap}):\n{md}"
        );
        assert!(
            md.contains(&format!("just over {}", human_bytes(MAX_TOTAL_BYTES * 2))),
            "and the doubled figure with it:\n{md}"
        );
        // No stale magnitude may survive anywhere in a generated report.
        for stale in ["1 MiB", "2 MiB", "512 KiB"] {
            assert!(
                !md.contains(stale),
                "a generated report still says {stale} while the cap is {cap}:\n{md}"
            );
        }
    }

    /// **No line of THIS renderer's markdown may be indented.**
    ///
    /// `report::Report` is a different renderer with its own tables and has its
    /// own copy of this guard — found by planting an indented header in *that*
    /// file and watching this test pass. CommonMark renders 4+
    /// leading spaces as a code block, so a table emitted that way is not a
    /// table — 140 lines across the 18 committed artifacts were rendering as
    /// code. The generator was fixed once for the ratchet banner and the tables
    /// were missed, which is why this asserts over *every* section rather than
    /// over the one that was noticed.
    #[test]
    fn no_generated_line_is_indented_into_a_code_block() {
        let mut report = tests_support::report_with(&[
            ("A_bare", 37),
            ("B0_idle", 90),
            ("G3_line_8k", 150),
            ("H3_newline_8k", 200),
        ]);
        report
            .arms
            .iter_mut()
            .find(|a| a.arm.id == "H3_newline_8k")
            .unwrap()
            .phases = parse_timings(
            "rift-timing hook=prepare-commit-msg total_us=90000 predict_id_us=70000 \
             redact_us=66000\n",
        );
        for (n, line) in report.to_markdown().lines().enumerate() {
            assert!(
                !line.starts_with("    ") || line.trim().is_empty(),
                "line {n} is indented and will render as a code block: {line:?}"
            );
        }
    }

    /// **B2: the committed artifacts stay indentation-clean.**
    ///
    /// I reflowed 145 leading-space lines down to 5 and then *wrote* that the
    /// property was "asserted, not claimed" — while no assertion existed. The
    /// substance was true and the sentence was not, which is the pattern this
    /// suite exists to break. Here is the assertion.
    ///
    /// It also closes the other direction the verifier named: without it the
    /// 140 lines could silently return, since fixing the generator only governs
    /// files that do not exist yet.
    ///
    /// **There is no exception, because none was needed.** An earlier version
    /// carved out `hook-latency-1787120100.md` for "5 survivors", and the
    /// carve-out was both wrong and dead weight: it skipped *every* line in
    /// that file not starting with `"| "` — a whole-file pattern scoped by
    /// filename, while its own text claimed the opposite — and deleting it
    /// leaves the guard green, because **zero 4-space-indented lines exist in
    /// any of the 18 artifacts**. The 5 survivors are ` |` single-space
    /// continuations and unindented `Usage:` text, which the 4-space rule never
    /// covered in the first place. An exception protecting nothing is a hole
    /// with a reason attached.
    #[test]
    fn the_committed_artifacts_are_indentation_clean() {
        let dir = crate::leg::default_repo_root().join("benchmarks/hook-latency");
        let Ok(entries) = fs::read_dir(&dir) else {
            assert!(
                !crate::leg::default_repo_root().join("benchmarks").is_dir(),
                "benchmarks/ exists but {} does not — the guard would skip itself",
                dir.display()
            );
            return;
        };
        let mut checked = 0usize;
        for path in entries.flatten().map(|e| e.path()) {
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            checked += 1;
            for (n, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                assert!(
                    !line.starts_with("    "),
                    "{}:{} is indented and renders as a code block, not a table: {line:?}",
                    path.display(),
                    n + 1
                );
            }
        }
        assert!(checked >= 18, "only {checked} artifacts were checked");
    }

    /// **No committed artifact carries a volatile sandbox identifier.**
    ///
    /// The artifacts recorded `rift/<32-hex>/projection.db` — 224 tokens, 112
    /// distinct ids, one per throwaway sandbox — because `oracles.projections`
    /// keys disk usage **by path** and the path carries the repo-id. The
    /// measurement was always the byte count; the id was an accident of the
    /// key, identifying a directory that stopped existing when the run ended.
    ///
    /// It reddened `rift-shadow-redact`'s false-positive gate (whose corpus is
    /// `git ls-files` from the **workspace** root, so every tracked file added
    /// here is inside it) and it made two runs of the same arm textually
    /// undiffable, degrading the ratchet's own comparison.
    ///
    /// **This guard reads the artifacts, not the generator.** A generator-side
    /// check would pass forever while stale files kept the tokens.
    ///
    /// The guard matches the offending path shape, rather than treating every
    /// long hexadecimal value as volatile. Full Git object ids, fixture heads,
    /// and binary/script digests are durable provenance in the newer artifact
    /// schemas; an allowlist of today's values would reject each legitimate
    /// future run while still failing to describe the defect this guards.
    #[test]
    fn no_committed_artifact_carries_a_volatile_sandbox_id() {
        let planted = format!("rift/{}/projection.db", "a".repeat(32));
        assert_eq!(
            volatile_cache_repo_ids(&planted),
            vec!["a".repeat(32)],
            "the guard must red on the path shape it exists to prevent"
        );
        assert!(
            volatile_cache_repo_ids(&format!("git_describe={}", "b".repeat(40))).is_empty(),
            "a full Git object id is durable provenance, not a sandbox path"
        );

        let root = crate::leg::default_repo_root().join("benchmarks");
        let mut stack = vec![root.clone()];
        let mut checked = 0usize;
        while let Some(path) = stack.pop() {
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                if let Ok(entries) = fs::read_dir(&path) {
                    stack.extend(entries.flatten().map(|e| e.path()));
                }
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            checked += 1;
            let volatile = volatile_cache_repo_ids(&text);
            assert!(
                volatile.is_empty(),
                "{} carries volatile sandbox path segment(s) {volatile:?} — the repository id \
                 has no provenance value, makes equivalent runs textually differ, and lands in \
                 a security crate's false-positive corpus",
                path.display(),
            );
        }
        assert!(checked >= 37, "only {checked} artifacts were checked");
    }

    fn volatile_cache_repo_ids(text: &str) -> Vec<String> {
        text.match_indices("rift/")
            .filter_map(|(at, marker)| {
                let tail = &text[at + marker.len()..];
                let repo_id = tail.split('/').next().unwrap_or_default();
                (repo_id.len() >= 32 && repo_id.chars().all(|c| c.is_ascii_hexdigit()))
                    .then(|| repo_id.to_owned())
            })
            .collect()
    }

    #[test]
    fn byte_prose_is_correct_at_every_magnitude() {
        assert_eq!(human_bytes(8 * 1024), "8 KiB");
        assert_eq!(human_bytes(16 * 1024), "16 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1 MiB");
        // The formatter that rendered 512 as "0,512" and so never matched.
        assert_eq!(thousands(512), "512");
        assert_eq!(thousands(2_048), "2,048");
        assert_eq!(thousands(8_192), "8,192");
        assert_eq!(thousands(1_048_576), "1,048,576");
        assert_eq!(human_bytes(2_048), "2 KiB");
        assert_eq!(human_bytes(999), "999 B");
    }

    #[test]
    fn explain_output_yields_the_files_read_count_it_labels() {
        assert_eq!(parse_explain_files("  files read: 16\n"), Some(16));
        assert_eq!(parse_explain_files("nothing of the sort\n"), None);
    }

    /// The decision table: an owner must be able to read the curve, the units
    /// and the owning phase in one pass. Each of the three is asserted, because
    /// a table missing one of them sends the decision back for another round.
    #[test]
    fn the_anatomy_answers_the_curve_the_units_and_the_owning_phase_at_once() {
        let mut report = tests_support::report_with(&[
            ("A_bare", 37),
            ("G1_line_2k", 60),
            ("H3_newline_8k", 260),
        ]);
        // The phase commit for the ceiling arm: 210 ms of hook bodies, of which
        // 150 ms is the redaction and 20 ms the id derivation around it.
        let ceiling = report
            .arms
            .iter_mut()
            .find(|a| a.arm.id == "H3_newline_8k")
            .unwrap();
        ceiling.phases = parse_timings(
            "rift-timing hook=prepare-commit-msg total_us=207000 predict_id_us=170000 \
             redact_us=150000 open_projection_us=2030\n\
             rift-timing hook=commit-msg total_us=140\n\
             rift-timing hook=post-commit total_us=2860\n",
        );

        let rows = report.anatomy();
        assert_eq!(
            rows.len(),
            2,
            "the control is decomposed against, not itself"
        );
        let top = rows.iter().find(|r| r.arm == "H3_newline_8k").unwrap();

        // (1) The curve: the arms are ordered by the bytes they read.
        let small = rows.iter().find(|r| r.arm == "G1_line_2k").unwrap();
        assert_eq!(small.bytes_read, Some(2_048));
        assert_eq!(top.bytes_read, Some(MAX_TOTAL_BYTES));
        assert!(top.git_latency_us > small.git_latency_us);

        // (2) The units: both, and the factor, so which side of the 2× is not
        // left to the reader.
        assert!(top.document_bytes.unwrap() > top.bytes_read.unwrap());
        let factor = top.document_factor.unwrap();
        assert!(
            factor > 1.9,
            "the worst-case twin is the 2N + 2 end: {factor}"
        );

        // (3) The owning phase, exclusive, so redact is not counted twice.
        assert_eq!(top.git_latency_us, 223_000, "260 ms − the 37 ms control");
        assert_eq!(top.hook_bodies_us, 210_000, "the three hooks' own totals");
        assert_eq!(
            top.outside_hook_bodies_us, 13_000,
            "the measured outside-body term"
        );
        assert_eq!(top.redact_us, 150_000);
        assert_eq!(
            top.predict_id_excl_us, 20_000,
            "predict_id less redact, so the two columns may be added"
        );
        assert!(top.breaches, "223 ms against a 100 ms hard budget");
        assert!(!small.breaches);
        assert!(!top.bodies_exceed_latency);

        let md = report.to_markdown();
        assert!(md.contains("**BREACH: `H3_newline_8k`.**"), "{md}");
        assert!(
            md.contains("The corpus was not adjusted in response"),
            "{md}"
        );
        // The three answers are in one table, in one row.
        assert!(
            md.contains(
                "| `H3_newline_8k` | 8192 | 16576 | 2.02× | 223.00 ms | 210.00 ms | 13.00 ms \
 | 150.00 ms | 20.00 ms | **BREACH** |"
            ),
            "{md}"
        );
    }

    /// Every figure in the anatomy is control-relative, so a sweep that lost
    /// its control has nothing to decompose — and says so by being empty rather
    /// than by quoting absolutes as if they were latencies.
    #[test]
    fn an_anatomy_without_a_control_is_empty_rather_than_absolute() {
        let report = tests_support::report_with(&[("H3_newline_8k", 260)]);
        assert!(report.anatomy().is_empty());
        assert!(!report.to_markdown().contains("## Anatomy"));
    }

    /// Two populations can disagree; the report says so rather than clamping a
    /// zero into the column that would then read "process creation is free".
    #[test]
    fn hook_bodies_larger_than_the_median_latency_are_reported_not_clamped() {
        let mut report = tests_support::report_with(&[("A_bare", 37), ("B1_live", 50)]);
        let arm = report
            .arms
            .iter_mut()
            .find(|a| a.arm.id == "B1_live")
            .unwrap();
        arm.phases = parse_timings("rift-timing hook=prepare-commit-msg total_us=90000\n");
        let row = &report.anatomy()[0];
        assert_eq!(row.git_latency_us, 13_000);
        assert!(row.bodies_exceed_latency, "90 ms of body inside 13 ms");
        assert_eq!(row.outside_hook_bodies_us, 0, "saturating, never negative");
        assert!(report
            .to_markdown()
            .contains("reported rather than clamped"));
    }

    #[test]
    fn latency_is_measured_against_the_control_and_absent_without_one() {
        let control = Stats::of(&[crate::measure::Sample {
            wall_ns: 37_000_000,
            exit_code: Some(0),
        }])
        .unwrap();
        let arm = ArmResult {
            attestation: Some(crate::attest::Attestation {
                load1: Some(1.65),
                void_above: crate::attest::VOID_ABOVE_LOAD1,
            }),
            arm: arms()[2].clone(),
            stats: Stats::of(&[crate::measure::Sample {
                wall_ns: 94_000_000,
                exit_code: Some(0),
            }]),
            error: None,
            oracles: Oracles::default(),
            phases: BTreeMap::new(),
        };
        assert_eq!(arm.latency_ns(&control), Some(57_000_000));
        assert!((arm.ratio(&control).unwrap() - 2.540_54).abs() < 1e-4);

        let unmeasured = ArmResult { stats: None, ..arm };
        assert_eq!(unmeasured.latency_ns(&control), None, "absent, never zero");
    }
}

/// The perf-delta ratchet: **ratios, never absolutes**.
///
/// The plan asks for a gate that fails on a >10% regression. Gating on one
/// machine's absolute p50 would false-red constantly: absolutes here move about
/// 2× with machine load, and this project has already watched a busy box
/// measure 108.65 s for a test that runs in 8.27 s isolated and nearly file a
/// 13× regression against its own change.
///
/// So the compared quantity is each arm's ratio to **the bare-commit control
/// measured in the same run**. Machine weather multiplies both terms and
/// cancels; a real regression does not. The absolutes travel with the verdict
/// anyway, because a reader who wants them should not have to reopen the JSON.
pub mod ratchet {
    use serde::{Deserialize, Serialize};

    use super::HookLatencyReport;

    /// The default tolerance the plan asks for.
    pub const DEFAULT_TOLERANCE_PCT: f64 = 10.0;

    /// **By owner decision this ratchet is REPORT-ONLY.**
    ///
    /// It computes and publishes a verdict; it does not fail a build. Measured
    /// on the development machine, the 10% gate fails **5 arms of 5 between two
    /// identical runs of the same binary** (+19.0%, +98.2%, +37.8%, +56.6%,
    /// +39.7%). The ratio half is demonstrably working — the bare control moved
    /// 40.06 → 61.63 ms across four runs, a 54% spread, and the ratio absorbs
    /// it — but it cannot absorb per-arm run-to-run variance reaching +98%.
    ///
    /// A gate that reds on noise teaches everyone to ignore it, which is worse
    /// than having no gate. So it reports until enough runs exist to set a
    /// threshold that reflects real variance.
    ///
    /// **A gate that cannot fail must say so in the same breath it prints a
    /// verdict**, or it becomes the decorative guard this project has spent
    /// two days removing — which is why the banner is emitted by
    /// [`to_markdown`] itself and not by its caller.
    pub const REPORT_ONLY: bool = true;

    /// What characterising the variance would take, so the next person setting
    /// a threshold does not start from zero.
    ///
    /// Four runs are **not** enough: `C1_include_50k` and `C2_include_250k`
    /// came out with the *same* median ratio (3.25×) over four runs, so a
    /// fivefold difference in corpus size was unresolvable. Named here rather
    /// than in a ledger nobody re-reads.
    pub const VARIANCE_NOTE: &str = "four runs cannot resolve the middle of the curve — the \
         50 KB and 250 KB arms shared a median ratio of 3.25×. A usable threshold needs \
         several runs a side reduced by median-of-medians, more iterations per arm, or arms \
         interleaved rather than run in blocks (block order confounds a slow arm with a \
         drifting machine, which is what produced run 1's inversion that run 2 reversed).";

    /// One arm's verdict.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "verdict", rename_all = "kebab-case")]
    pub enum ArmVerdict {
        /// Both runs measured the arm and the ratio did not grow past tolerance.
        Held {
            /// Ratio in the baseline run.
            baseline: f64,
            /// Ratio in the candidate run.
            candidate: f64,
            /// Percentage change of the ratio; negative is an improvement.
            growth_pct: f64,
        },
        /// The ratio grew past tolerance.
        Regressed {
            /// Ratio in the baseline run.
            baseline: f64,
            /// Ratio in the candidate run.
            candidate: f64,
            /// Percentage change of the ratio.
            growth_pct: f64,
        },
        /// One side has no ratio for this arm.
        ///
        /// **Not a pass.** An arm that stopped being measured is the cheapest
        /// possible way to make a ratchet green, so it is its own verdict and
        /// it fails the gate.
        Uncomparable {
            /// Why.
            reason: String,
        },
    }

    impl ArmVerdict {
        /// Whether this verdict lets a gate stay green.
        #[must_use]
        pub fn ok(&self) -> bool {
            matches!(self, Self::Held { .. })
        }
    }

    /// One arm, judged.
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    pub struct ArmDelta {
        /// The arm identifier.
        pub arm: String,
        /// Its verdict.
        pub verdict: ArmVerdict,
    }

    /// Compare `candidate` against `baseline`, arm by arm.
    ///
    /// Arms are matched by id and the **union** is judged: an arm the candidate
    /// dropped and an arm the baseline never had are both `Uncomparable`, and
    /// both fail. Silence is the failure mode a ratchet is most vulnerable to.
    #[must_use]
    pub fn compare(
        baseline: &HookLatencyReport,
        candidate: &HookLatencyReport,
        tolerance_pct: f64,
    ) -> Vec<ArmDelta> {
        let mut ids: Vec<String> = baseline
            .arms
            .iter()
            .chain(&candidate.arms)
            .filter(|a| !a.arm.is_control())
            .map(|a| a.arm.id.clone())
            .collect();
        ids.sort_unstable();
        ids.dedup();

        ids.into_iter()
            .map(|arm| ArmDelta {
                verdict: judge(baseline, candidate, &arm, tolerance_pct),
                arm,
            })
            .collect()
    }

    fn judge(
        baseline: &HookLatencyReport,
        candidate: &HookLatencyReport,
        arm: &str,
        tolerance_pct: f64,
    ) -> ArmVerdict {
        let (Some(base), Some(cand)) = (ratio(baseline, arm), ratio(candidate, arm)) else {
            return ArmVerdict::Uncomparable {
                reason: format!(
                    "`{arm}` has no control-relative ratio in both runs (an arm that stopped \
                     being measured is the cheapest way to make a ratchet green, so this is a \
                     failure, not a skip)"
                ),
            };
        };
        if base <= 0.0 {
            return ArmVerdict::Uncomparable {
                reason: format!("`{arm}`'s baseline ratio is {base}, which nothing can grow from"),
            };
        }
        let growth_pct = (cand - base) / base * 100.0;
        if growth_pct > tolerance_pct {
            ArmVerdict::Regressed {
                baseline: base,
                candidate: cand,
                growth_pct,
            }
        } else {
            ArmVerdict::Held {
                baseline: base,
                candidate: cand,
                growth_pct,
            }
        }
    }

    fn ratio(report: &HookLatencyReport, arm: &str) -> Option<f64> {
        let control = *report.control()?;
        report
            .arms
            .iter()
            .find(|a| a.arm.id == arm)?
            .ratio(&control)
    }

    /// Whether every arm held.
    #[must_use]
    pub fn passed(deltas: &[ArmDelta]) -> bool {
        !deltas.is_empty() && deltas.iter().all(|d| d.verdict.ok())
    }

    /// What the caller should do with a comparison.
    ///
    /// Exists so the report-only decision is a **value the tests can assert**
    /// rather than a branch buried in `main`. It was buried in `main`, and it
    /// was not there at all: `--enforce` was declared and never read, so the
    /// process printed "this does not fail a build" and then failed the build,
    /// identically with and without the flag. A flag nobody reads is worse than
    /// a missing flag, because the banner beside it becomes a lie.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Outcome {
        /// Every arm held; nothing to report.
        Held,
        /// Arms regressed, and the run is report-only ([`REPORT_ONLY`]).
        Reported {
            /// Arms that did not hold.
            failed: usize,
            /// Arms compared.
            total: usize,
        },
        /// Arms regressed and the caller asked to enforce.
        Enforced {
            /// Arms that did not hold.
            failed: usize,
            /// Arms compared.
            total: usize,
        },
    }

    /// Decide what a comparison means, given whether the caller enforces.
    #[must_use]
    pub fn outcome(deltas: &[ArmDelta], enforce: bool) -> Outcome {
        if passed(deltas) {
            return Outcome::Held;
        }
        let failed = deltas.iter().filter(|d| !d.verdict.ok()).count();
        let total = deltas.len();
        if enforce {
            Outcome::Enforced { failed, total }
        } else {
            Outcome::Reported { failed, total }
        }
    }

    /// Render the comparison.
    ///
    /// The report-only banner is emitted **here**, beside the verdict, rather
    /// than left to a caller: a gate that cannot fail and does not say so in
    /// the same breath as its verdict is the decorative guard this project
    /// exists to remove.
    #[must_use]
    pub fn to_markdown(deltas: &[ArmDelta], tolerance_pct: f64) -> String {
        let mut out = format!(
            "# Hook-latency ratchet\n\n**REPORT-ONLY (the owner's call). This does not fail a build, and it must not be \
             read as a passing gate.** Measured: at {tolerance_pct:.0}% it fails 5 arms of 5 \
             between two identical runs of the same binary (+19.0%, +98.2%, +37.8%, +56.6%, \
             +39.7%), so as specified it would red on noise and teach everyone to ignore it. \
             It publishes a verdict until enough runs exist to set a threshold reflecting real \
             variance: {VARIANCE_NOTE}\n\nRatios to each run's **own** bare-commit control, tolerance {tolerance_pct:.0}%. \
             Absolutes are not compared: they move ~2× with machine load — this harness's own \
             control moved 54% across four runs — so a gate on them false-reds.\n\n\
 | arm | baseline × bare | candidate × bare | change | verdict |\n\
|---|---|---|---|---|\n"
        );
        for delta in deltas {
            match &delta.verdict {
                ArmVerdict::Held {
                    baseline,
                    candidate,
                    growth_pct,
                } => out.push_str(&format!(
                    "| `{}` | {baseline:.2}× | {candidate:.2}× | {growth_pct:+.1}% | held |\n",
                    delta.arm
                )),
                ArmVerdict::Regressed {
                    baseline,
                    candidate,
                    growth_pct,
                } => out.push_str(&format!(
                    "| `{}` | {baseline:.2}× | {candidate:.2}× | {growth_pct:+.1}% | \
                     **REGRESSED** |\n",
                    delta.arm
                )),
                ArmVerdict::Uncomparable { reason } => out.push_str(&format!(
                    "| `{}` | — | — | — | **UNCOMPARABLE** — {reason} |\n",
                    delta.arm
                )),
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crate::hooklat::tests_support::report_with;

        #[test]
        fn machine_load_that_multiplies_every_arm_is_not_a_regression() {
            // Every absolute doubles — the exact shape a busy machine produces —
            // while the control doubles with them. An absolute gate reds here;
            // a ratio gate must not.
            let base = report_with(&[("A_bare", 37), ("B1_live", 94)]);
            let slow = report_with(&[("A_bare", 74), ("B1_live", 188)]);
            let deltas = compare(&base, &slow, DEFAULT_TOLERANCE_PCT);
            assert_eq!(deltas.len(), 1);
            assert!(passed(&deltas), "{deltas:?}");
            let ArmVerdict::Held { growth_pct, .. } = deltas[0].verdict else {
                panic!("expected held: {:?}", deltas[0].verdict)
            };
            assert!(growth_pct.abs() < 1e-9, "{growth_pct}");
        }

        #[test]
        fn an_arm_that_really_got_slower_relative_to_its_control_regresses() {
            let base = report_with(&[("A_bare", 37), ("B1_live", 94)]);
            // The control is unchanged and the arm grew 20%.
            let worse = report_with(&[("A_bare", 37), ("B1_live", 113)]);
            let deltas = compare(&base, &worse, DEFAULT_TOLERANCE_PCT);
            assert!(!passed(&deltas));
            let ArmVerdict::Regressed { growth_pct, .. } = deltas[0].verdict else {
                panic!("expected regressed: {:?}", deltas[0].verdict)
            };
            assert!(growth_pct > 10.0, "{growth_pct}");
            assert!(to_markdown(&deltas, 10.0).contains("**REGRESSED**"));
        }

        #[test]
        fn a_dropped_arm_fails_rather_than_disappearing() {
            let base = report_with(&[("A_bare", 37), ("B1_live", 94)]);
            let thin = report_with(&[("A_bare", 37)]);
            let deltas = compare(&base, &thin, DEFAULT_TOLERANCE_PCT);
            assert_eq!(deltas.len(), 1, "the union is judged, not the intersection");
            assert!(!passed(&deltas), "not measuring an arm is not passing it");
            assert!(matches!(deltas[0].verdict, ArmVerdict::Uncomparable { .. }));
        }

        #[test]
        fn a_run_with_no_control_cannot_ratchet_anything() {
            let base = report_with(&[("A_bare", 37), ("B1_live", 94)]);
            let no_control = report_with(&[("B1_live", 94)]);
            let deltas = compare(&base, &no_control, DEFAULT_TOLERANCE_PCT);
            assert!(!passed(&deltas));
            assert!(!passed(&compare(&base, &report_with(&[]), 10.0)));
        }

        /// Report-only as a value rather than a promise. The flag was
        /// declared and never read: the verifier ran the built binary with and
        /// without `--enforce` against two committed artifacts and got exit 1
        /// both times, byte-identical output, while the banner said the
        /// opposite.
        #[test]
        fn report_only_and_enforce_reach_different_outcomes_on_the_same_regression() {
            let base = report_with(&[("A_bare", 37), ("B1_live", 94)]);
            let worse = report_with(&[("A_bare", 37), ("B1_live", 113)]);
            let deltas = compare(&base, &worse, DEFAULT_TOLERANCE_PCT);
            assert!(!passed(&deltas));

            assert_eq!(
                outcome(&deltas, false),
                Outcome::Reported {
                    failed: 1,
                    total: 1
                },
                "the default must not fail a build — the banner says it does not"
            );
            assert_eq!(
                outcome(&deltas, true),
                Outcome::Enforced {
                    failed: 1,
                    total: 1
                },
                "--enforce must reach a different outcome, or it is decoration"
            );
            assert_ne!(outcome(&deltas, false), outcome(&deltas, true));

            // And a clean comparison is `Held` under both, so the flag changes
            // only what a regression means.
            let same = compare(&base, &base, DEFAULT_TOLERANCE_PCT);
            assert_eq!(outcome(&same, false), Outcome::Held);
            assert_eq!(outcome(&same, true), Outcome::Held);
            // The banner describes the default, so the default is what the
            // CLI must pass: `--enforce` off means report-only.
            assert_eq!(outcome(&deltas, !REPORT_ONLY), outcome(&deltas, false));
        }

        #[test]
        fn an_improvement_holds_however_large() {
            let base = report_with(&[("A_bare", 37), ("B1_live", 200)]);
            let fast = report_with(&[("A_bare", 37), ("B1_live", 50)]);
            assert!(passed(&compare(&base, &fast, DEFAULT_TOLERANCE_PCT)));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests_support {
    use std::collections::BTreeMap;

    use super::{arms, Arm, ArmResult, Capture, HookLatencyReport, Oracles, HOOK_LATENCY_SCHEMA};
    use crate::{
        leg::LegStatus,
        measure::{Protocol, Sample, Stats},
        report::Host,
    };

    /// A report holding only the arms named, at the milliseconds named.
    pub(crate) fn report_with(arms_ms: &[(&str, u64)]) -> HookLatencyReport {
        let catalogue = arms();
        let results = arms_ms
            .iter()
            .map(|(id, ms)| {
                let arm = catalogue
                    .iter()
                    .find(|a| a.id == *id)
                    .cloned()
                    .unwrap_or(Arm {
                        id: (*id).to_owned(),
                        title: String::new(),
                        capture: Capture::Live,
                        include: None,
                    });
                ArmResult {
                    attestation: Some(crate::attest::Attestation {
                        load1: Some(1.65),
                        void_above: crate::attest::VOID_ABOVE_LOAD1,
                    }),
                    arm,
                    stats: Stats::of(&[Sample {
                        wall_ns: ms * 1_000_000,
                        exit_code: Some(0),
                    }]),
                    error: None,
                    oracles: Oracles::default(),
                    phases: BTreeMap::new(),
                }
            })
            .collect();
        HookLatencyReport {
            schema_version: HOOK_LATENCY_SCHEMA,
            harness_version: "0.0.1".to_owned(),
            generated_at_unix: 1_800_000_000,
            host: Host::detect("n/a".to_owned()),
            subject: LegStatus::Absent {
                reason: "fixture".to_owned(),
            },
            source_clock: None,
            git_version: "git version 2.0".to_owned(),
            protocol: Protocol::new(1, 1),
            budget_ms: super::BUDGET_MS,
            arms: results,
        }
    }
}
