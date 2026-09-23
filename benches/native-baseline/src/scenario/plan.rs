//! What a leg runs when a row is timed: the per-iteration context, and the one
//! invocation each scenario's timed verb is.
//!
//! Kept apart from the untimed preparation next door because the boundary
//! between them is the measurement: everything here is inside the timer, and a
//! step that drifts across that line changes what a published number means.

use std::path::{Path, PathBuf};

use crate::{leg::LegId, measure::Invocation};

use super::{
    catalogue::{spec, ScenarioId, ScenarioSpec, Session},
    event::{iteration_session, read_target, BENCH_SESSION, CAPTURE_SYNC_ENV, FIXTURE_BRANCH},
};

/// What one iteration of one leg is measured against.
#[derive(Debug, Clone, Copy)]
pub struct PlanCtx<'a> {
    /// Which leg.
    pub leg: LegId,
    /// Its resolved binary.
    pub binary: &'a Path,
    /// The repository copy this iteration runs in.
    pub repo: &'a Path,
    /// The leg's sandbox: everything that is not repository content.
    pub sandbox: &'a Path,
    /// Iteration index, warmup included.
    pub iteration: usize,
    /// The scripted-session driver (`vogon`), when it resolved.
    ///
    /// A driver, not a leg: it is never measured and never compared. `None`
    /// means the rows that need it record why, rather than silently measuring
    /// something else.
    pub tool: Option<&'a Path>,
}

impl PlanCtx<'_> {
    /// Where this session's transcript lives. Outside the repository, because a
    /// real transcript is not repository content — putting one in the worktree
    /// would change what every worktree hash and `status` sees.
    #[must_use]
    pub fn transcript_path(&self, session: &str) -> PathBuf {
        self.sandbox
            .join("transcripts")
            .join(format!("{session}.jsonl"))
    }

    /// The session the timed verb of `spec` is delivered to.
    #[must_use]
    pub fn session_for(&self, spec: &ScenarioSpec) -> String {
        match spec.session {
            Session::Established => BENCH_SESSION.to_owned(),
            Session::PerIteration => iteration_session(self.iteration),
        }
    }
}

/// What a leg does for a scenario.
#[derive(Debug, Clone)]
pub enum Plan {
    /// Run this invocation.
    Run(Box<Invocation>),
    /// This leg has no equivalent verb. Recorded, never scored.
    NoAnalogue(&'static str),
}

/// Turns the condense row's scripted session plays.
///
/// `vogon --turns N` emits **6N + 7** hooks: `SessionStart`, then per turn a
/// prompt, a `Read` pair, an `Edit` pair and a `Stop`, plus the subagent beat
/// (four) and the compaction beat (one) and `SessionEnd`. Three turns is 25
/// hooks — enough that a turn boundary, a subagent fan-out and a compaction all
/// land inside one measured iteration, and few enough that a heavy-protocol row
/// stays minutes rather than hours.
pub const CONDENSE_TURNS: u32 = 3;

/// Hooks one `--turns N` script fires.
#[must_use]
pub const fn condense_hooks(turns: u32) -> u32 {
    6 * turns + 7
}

/// The flags `vogon` gets, built **once** and shared by every mode.
///
/// **This is a trap with a name.** `--repo` changes the payload (it adds `cwd`
/// and makes tool paths absolute) and a canonical event's identity hashes the
/// payload — so a `script` invocation and a `drive` invocation given different
/// flags describe different workloads while looking identical, and they
/// disagree silently. One builder, one flag vector. `--commit` is the single
/// permitted difference: it is a drive-only *action* taken after the last turn
/// and it alters no payload.
#[must_use]
pub fn vogon_spec_flags(repo: &Path) -> Vec<String> {
    vec![
        "--agent".to_owned(),
        "claude-code".to_owned(),
        "--turns".to_owned(),
        CONDENSE_TURNS.to_string(),
        "--repo".to_owned(),
        repo.display().to_string(),
    ]
}

/// The four shadow-worktree rows' timed verbs, or `None` for every other row.
///
/// Lifted out of [`plan`] so each stays readable: two of these time `git` and
/// two time the product binary, and the two pairs have nothing else in common.
fn shadow_worktree_plan(id: ScenarioId, ctx: &PlanCtx<'_>) -> Option<Plan> {
    Some(match id {
        ScenarioId::WorktreeAddAdopt => match ctx.leg {
            LegId::Rift => Plan::Run(Box::new(git_at(ctx.repo).args([
                "worktree".to_owned(),
                "add".to_owned(),
                "--quiet".to_owned(),
                member_at(ctx).display().to_string(),
                "-b".to_owned(),
                member_branch(ctx.iteration),
            ]))),
            LegId::Reference => Plan::NoAnalogue(NO_MEMBER_ANALOGUE),
        },
        ScenarioId::MemberSwitch => match ctx.leg {
            LegId::Rift => Plan::Run(Box::new(git_at(member_at(ctx)).args([
                "switch".to_owned(),
                "--quiet".to_owned(),
                switch_branch(ctx.iteration),
            ]))),
            LegId::Reference => Plan::NoAnalogue(NO_MEMBER_ANALOGUE),
        },
        // Served from the main work tree: the walk is what is being timed, and
        // it costs the same wherever the caller stands.
        ScenarioId::RecallKnownState => match ctx.leg {
            LegId::Rift => Plan::Run(Box::new(serve_at(ctx, ctx.repo))),
            LegId::Reference => Plan::NoAnalogue(NO_RECALL_ANALOGUE),
        },
        // The product's own repeat-class report, over the session the prelude
        // just played: the residue is a number only the product can state,
        // because only it knows which reads were repeats of unchanged bytes.
        ScenarioId::RecallInterceptSavings => match ctx.leg {
            LegId::Rift => Plan::Run(Box::new(reread_report_at(ctx))),
            LegId::Reference => Plan::NoAnalogue(NO_RECALL_ANALOGUE),
        },
        // Served from the MEMBER, which is the whole claim: this checkout
        // captured none of the reads it is about to be handed.
        ScenarioId::RecallCompounding => match ctx.leg {
            LegId::Rift => Plan::Run(Box::new(serve_at(ctx, member_at(ctx)))),
            LegId::Reference => Plan::NoAnalogue(NO_RECALL_ANALOGUE),
        },
        _ => return None,
    })
}

/// Build the invocation `ctx.leg` uses for `id`.
///
/// Argv for the backend leg follows its documented surface at the pinned tag
/// (`status`, `checkpoint list`, `hooks <agent> <verb>`); it is re-verified when
/// the leg is actually activated, since an absent binary cannot be probed.
#[must_use]
pub fn plan(id: ScenarioId, ctx: &PlanCtx<'_>) -> Plan {
    let spec = spec(id);
    if let Some(event) = spec.hook_event {
        let nth = spec.nth_event(ctx.iteration);
        return Plan::Run(Box::new(event.invocation(
            ctx,
            &ctx.session_for(&spec),
            nth,
        )));
    }
    if let Some(plan) = shadow_worktree_plan(id, ctx) {
        return plan;
    }
    let inv = |args: &[&str]| {
        Plan::Run(Box::new(
            Invocation::new(ctx.binary, ctx.repo).args(args.to_vec()),
        ))
    };
    match id {
        ScenarioId::ColdStatus => inv(&["status"]),
        ScenarioId::CheckpointList => inv(&["checkpoint", "list"]),
        ScenarioId::Condense => match ctx.leg {
            LegId::Rift => condense_plan(ctx),
            LegId::Reference => Plan::NoAnalogue(
                "the scripted driver speaks rift's `hooks <agent> <verb>` surface; it cannot \
                 drive the retired reference implementation, so this row is rift-only \
                 rather than a win over a measured competitor",
            ),
        },
        ScenarioId::ImportThroughput => match ctx.leg {
            // `--from-legacy` names the FORMAT, never the tool that wrote it
            // by owner decision, and the verb is read-only: it walks, parses and
            // re-derives roots, and writes nothing to the source repository.
            // That is exactly the work the rate is about.
            LegId::Rift => inv(&["import", "--from-legacy"]),
            LegId::Reference => Plan::NoAnalogue(
                "importing a pre-Rift history is a migration INTO rift; the retired \
                 reference implementation had no verb that read its own format back as an \
                 import, so this row is rift-only rather than a win over a measured \
                 competitor",
            ),
        },
        ScenarioId::ResumeRestore => match ctx.leg {
            // `--force` says yes where resume would ask (overwrite the local
            // transcript; a checkpoint behind the tip), which in a pipe is
            // the only answer that does not hang.
            LegId::Rift => inv(&["--json", "session", "resume", FIXTURE_BRANCH, "--force"]),
            LegId::Reference => Plan::NoAnalogue(
                "the retired reference implementation resumed by re-launching the agent \
                 against its shadow branch; it had no verb that restored a transcript from \
                 a checkpoint into place, so this row is rift-only rather than a win over a \
                 measured competitor",
            ),
        },
        ScenarioId::BlameWhy => match ctx.leg {
            LegId::Rift => inv(&["--json", "why", &why_target(spec.nth_event(ctx.iteration))]),
            LegId::Reference => Plan::NoAnalogue(
                "the retired reference implementation answered no line → checkpoint → \
                 prompt question; its checkpoints were shadow-branch snapshots with no \
                 per-line resolver, so this row is rift-only rather than a win over a \
                 measured competitor",
            ),
        },
        ScenarioId::VerifyThroughput => match ctx.leg {
            LegId::Rift => inv(&["verify"]),
            LegId::Reference => Plan::NoAnalogue(
                "the retired reference implementation shipped no integrity check (no hash \
                 chain, no Merkle roots) — integrity-at-speed is a rift-only measure, not a \
                 win over a measured competitor",
            ),
        },
        _ => Plan::NoAnalogue("scenario not wired yet"),
    }
}

/// The condense row's timed verb: the scripted-session driver, told to play a
/// session and then commit.
///
/// Without the driver the row is **refused with a reason** rather than quietly
/// replaced by something measurable — substituting a workload one can run for
/// the one that was asked for is what the wired/planned distinction exists to
/// prevent.
fn condense_plan(ctx: &PlanCtx<'_>) -> Plan {
    let Some(vogon) = ctx.tool else {
        return Plan::NoAnalogue(
            "the scripted-session driver did not resolve: build it with \
             `cargo build --release -p rift-shadow-hooks --bin vogon` or set \
             $RIFT_BENCH_VOGON_BIN",
        );
    };
    let mut args = vec![
        "drive".to_owned(),
        "--rift".to_owned(),
        ctx.binary.display().to_string(),
    ];
    args.extend(vogon_spec_flags(ctx.repo));
    // The one flag `script` does not get: an action, not a payload.
    args.push("--commit".to_owned());
    Plan::Run(Box::new(Invocation::new(vogon, ctx.repo).args(args)))
}

/// Why the backend leg has no worktree row: it keys its state by checkout, so
/// "the same repository from a second path" is not a thing it can be asked.
const NO_MEMBER_ANALOGUE: &str =
    "the pinned backend has no repository-family notion: a second checkout is a second \
     repository to it, so a member's adoption and its fast path have no counterpart to compare \
     against. Rift-only, and reported as such rather than as a win.";

/// Why the backend leg has no recall row.
const NO_RECALL_ANALOGUE: &str =
    "the pinned backend serves no memory-delta plane, so there is no verb to time against this \
     one. Rift-only, and reported as such rather than as a win.";

/// A hermetic `git` in `cwd` — the one on `PATH`, as every other git step in
/// this crate uses.
pub(super) fn git_at(cwd: impl Into<PathBuf>) -> Invocation {
    Invocation::new("git", cwd).env(CAPTURE_SYNC_ENV, "1")
}

/// The linked worktree this iteration's member rows create.
///
/// Under the **sandbox**, never inside the fixture: a work tree nested in
/// another work tree is a directory the outer repository's own `status` walks,
/// and every later iteration would be timing a bigger tree than the last.
pub(super) fn member_at(ctx: &PlanCtx<'_>) -> PathBuf {
    ctx.sandbox
        .join("members")
        .join(format!("m{}", ctx.iteration))
}

/// The branch a member is created on.
pub(super) fn member_branch(nth: usize) -> String {
    format!("bench-member-{nth}")
}

/// The branch [`ScenarioId::MemberSwitch`]'s timed `git switch` moves to.
pub(super) fn switch_branch(nth: usize) -> String {
    format!("bench-switch-{nth}")
}

/// The repeat-class report the intercept row times and probes — the same
/// invocation for both, so the probe cannot be reading a cheaper verb than the
/// one the timing is about.
pub(super) fn reread_report_at(ctx: &PlanCtx<'_>) -> Invocation {
    Invocation::new(ctx.binary, ctx.repo).args([
        "--json".to_owned(),
        "memory".to_owned(),
        "reread-report".to_owned(),
    ])
}

/// The serve both recall rows time, from `cwd`.
///
/// `--json` on the timed verb as well as on the work probe, so the two are the
/// same invocation and the probe cannot be measuring a cheaper one.
pub(super) fn serve_at(ctx: &PlanCtx<'_>, cwd: impl Into<PathBuf>) -> Invocation {
    let nth = ctx.iteration;
    Invocation::new(ctx.binary, cwd).args([
        "recall".to_owned(),
        "known-state".to_owned(),
        "--json".to_owned(),
        "--task".to_owned(),
        format!("finish the parser change {nth}"),
        "--file".to_owned(),
        read_target(nth),
    ])
}

/// Wire this agent on, so the family has an integration to fan out to.
/// Idempotent: the verb rewrites what it owns and leaves everything else.
pub(super) fn enable_agent(ctx: &PlanCtx<'_>) -> Invocation {
    Invocation::new(ctx.binary, ctx.repo).args(["enable", "--agent", "claude-code"])
}

/// The `why` row's file for the `nth` iteration, with the line the verb asks
/// about: `bench/why_<n>.txt:1`.
fn why_target(nth: usize) -> String {
    format!("bench/why_{nth}.txt:1")
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::leg::LegId;
    use crate::scenario::tests::*;
    use crate::scenario::*;

    /// `vogon --turns N` fires 6N + 7 hooks. Pinned here because the number
    /// appears in the row's published title, and a title that states a workload
    /// is a claim.
    #[test]
    fn the_scripted_session_is_six_per_turn_plus_seven() {
        assert_eq!(condense_hooks(1), 13);
        assert_eq!(condense_hooks(2), 19);
        assert_eq!(condense_hooks(CONDENSE_TURNS), 25);
        assert!(
            spec(ScenarioId::Condense).title.contains("25 hooks"),
            "the row's title states the workload it plays"
        );
    }

    /// Without the driver the row is **refused with a reason**, never quietly
    /// replaced by something measurable. A benchmark that substitutes a
    /// workload it can run for the one it was asked for is the failure this
    /// whole catalogue's wired/planned distinction exists to prevent.
    #[test]
    fn condense_without_the_driver_is_refused_and_says_how_to_get_it() {
        let with_tool = ctx(
            LegId::Rift,
            Path::new("/bin/rift"),
            Path::new("/tmp/repo"),
            Path::new("/tmp/sandbox"),
        );
        let Plan::Run(inv) = plan(ScenarioId::Condense, &with_tool) else {
            panic!("with a driver the row must run")
        };
        let line = inv.args.join(" ");
        assert!(line.starts_with("drive --rift /bin/rift"), "{line}");
        assert!(line.contains("--repo /tmp/repo"), "{line}");
        assert!(line.contains("--turns 3"), "{line}");
        assert!(line.ends_with("--commit"), "{line}");

        let without = PlanCtx {
            tool: None,
            ..with_tool
        };
        let Plan::NoAnalogue(reason) = plan(ScenarioId::Condense, &without) else {
            panic!("without a driver the row must not run")
        };
        assert!(reason.contains("RIFT_BENCH_VOGON_BIN"), "{reason}");
    }

    /// The trap named in `vogon_spec_flags`: `--repo` is part of the payload
    /// and the payload is part of a canonical event's identity, so every mode
    /// must be handed the same vector. `--commit` is the one permitted
    /// difference and it is an action, not a payload.
    #[test]
    fn every_vogon_mode_is_handed_the_same_payload_flags() {
        let flags = vogon_spec_flags(Path::new("/tmp/repo"));
        assert!(flags.contains(&"--repo".to_owned()));
        assert!(!flags.iter().any(|f| f == "--commit"));
        assert_eq!(flags, vogon_spec_flags(Path::new("/tmp/repo")));
        assert_ne!(flags, vogon_spec_flags(Path::new("/tmp/other")));
    }

    /// What each of the four rows actually times, read off the plan.
    ///
    /// The two member rows time **git**, not Rift: the claim is what the
    /// seventh hook adds to an operation a person typed, and a row that timed
    /// `rift` would be measuring the hook without the git it must not delay.
    /// The two recall rows time the product binary, and the compounding row
    /// times it **in the member** — a serve from the main work tree would be a
    /// checkout being handed its own reads back, which is not compounding.
    #[test]
    fn the_member_rows_time_git_and_the_compounding_row_serves_the_member() {
        let ctx = probe(LegId::Rift);
        let member = member_at(&ctx);
        let timed = |id| match plan(id, &ctx) {
            Plan::Run(inv) => *inv,
            Plan::NoAnalogue(why) => panic!("{}: {why}", ScenarioId::as_str(id)),
        };

        let add = timed(ScenarioId::WorktreeAddAdopt);
        assert_eq!(add.program, Path::new("git"));
        assert_eq!(add.cwd, Path::new("/tmp/repo"));
        assert!(add.args.contains(&"worktree".to_owned()) && add.args.contains(&"add".to_owned()));
        assert!(
            add.args.contains(&member.display().to_string()),
            "the timed add creates the member the row is named for: {:?}",
            add.args
        );

        let switch = timed(ScenarioId::MemberSwitch);
        assert_eq!(switch.program, Path::new("git"));
        assert_eq!(
            switch.cwd, member,
            "the switch is measured INSIDE the member, not in the main work tree"
        );
        assert!(switch.args.contains(&switch_branch(0)));

        let known = timed(ScenarioId::RecallKnownState);
        assert_eq!(known.program, Path::new("/bin/rift"));
        assert_eq!(known.cwd, Path::new("/tmp/repo"));
        let compounding = timed(ScenarioId::RecallCompounding);
        assert_eq!(compounding.program, Path::new("/bin/rift"));
        assert_eq!(compounding.cwd, member);
        for inv in [&known, &compounding] {
            assert_eq!(inv.args.first().map(String::as_str), Some("recall"));
            assert_eq!(inv.args.get(1).map(String::as_str), Some("known-state"));
            assert!(inv.args.contains(&"--json".to_owned()));
        }
        // The work probe re-runs the SAME invocation: a probe that measured a
        // cheaper serve would publish a figure the samples never produced.
        assert_eq!(
            work_probe(ScenarioId::RecallCompounding, &ctx).map(|i| i.args),
            Some(compounding.args.clone())
        );
        assert_eq!(
            parse_work_units(
                ScenarioId::RecallCompounding,
                r#"{"verb":"known-state","estimated_tokens_avoided":41}"#
            ),
            Some(41)
        );
        assert_eq!(
            parse_work_units(ScenarioId::RecallCompounding, "not json"),
            None,
            "an unreadable answer is absent, never zero"
        );
        // The backend leg has no counterpart for any of them, and says so.
        for id in WORKTREE_ROWS {
            assert!(
                matches!(plan(id, &probe(LegId::Reference)), Plan::NoAnalogue(_)),
                "{} claims a backend analogue it does not have",
                id.as_str()
            );
        }
    }

    #[test]
    fn wired_scenarios_use_the_same_argv_on_both_legs() {
        let (bin, repo, sb) = (
            Path::new("/bin/echo"),
            Path::new("/tmp/repo"),
            Path::new("/tmp/sandbox"),
        );
        let mut ids = vec![ScenarioId::ColdStatus, ScenarioId::CheckpointList];
        ids.extend(ScenarioId::hooks());
        for id in ids {
            let (Plan::Run(rift), Plan::Run(reference)) = (
                plan(id, &ctx(LegId::Rift, bin, repo, sb)),
                plan(id, &ctx(LegId::Reference, bin, repo, sb)),
            ) else {
                panic!("{} must be runnable on both legs", id.as_str())
            };
            assert_eq!(rift.args, reference.args, "identical scripted invocation");
            assert_eq!(rift.stdin, reference.stdin, "identical scripted payload");
        }
    }

    #[test]
    fn verify_is_rift_only_and_says_why() {
        let Plan::NoAnalogue(note) = plan(ScenarioId::VerifyThroughput, &probe(LegId::Reference))
        else {
            panic!("the retired reference implementation had no integrity check to measure")
        };
        assert!(note.contains("rift-only measure"));
        assert!(matches!(
            plan(ScenarioId::VerifyThroughput, &probe(LegId::Rift)),
            Plan::Run(_)
        ));
    }

    /// The why row: wired, rift-only, and each iteration asks about its own
    /// line.
    #[test]
    fn blame_why_is_wired_and_asks_about_this_iterations_line() {
        let s = spec(ScenarioId::BlameWhy);
        assert!(s.is_wired());
        assert_eq!(s.session, Session::PerIteration);
        assert_eq!(s.isolation, Isolation::Shared);
        assert_eq!(s.budget, None, "no owner has ratified a why budget");
        assert_eq!(s.fixture, HOOK_FIXTURE);

        let Plan::Run(inv) = plan(ScenarioId::BlameWhy, &probe(LegId::Rift)) else {
            panic!("wired")
        };
        assert_eq!(inv.args, ["--json", "why", "bench/why_0.txt:1"]);
        let later = PlanCtx {
            iteration: 7,
            ..probe(LegId::Rift)
        };
        let Plan::Run(inv7) = plan(ScenarioId::BlameWhy, &later) else {
            panic!("wired")
        };
        assert_eq!(
            inv7.args[2], "bench/why_7.txt:1",
            "each iteration asks about its own line"
        );
        assert!(matches!(
            plan(ScenarioId::BlameWhy, &probe(LegId::Reference)),
            Plan::NoAnalogue(note) if note.contains("rift-only")
        ));
    }

    /// The resume row: wired, rift-only, and the timed verb resumes the
    /// fixture branch with `--force` — which in a pipe is the only answer that
    /// does not hang.
    #[test]
    fn resume_restore_is_wired_and_resumes_the_fixture_branch() {
        let s = spec(ScenarioId::ResumeRestore);
        assert!(s.is_wired());
        assert_eq!(s.session, Session::PerIteration);
        assert_eq!(s.isolation, Isolation::Shared);
        assert_eq!(s.budget, None, "no owner has ratified a resume budget");
        assert_eq!(s.work_unit, None);
        assert_eq!(s.fixture, HOOK_FIXTURE);

        let Plan::Run(inv) = plan(ScenarioId::ResumeRestore, &probe(LegId::Rift)) else {
            panic!("wired")
        };
        assert_eq!(
            inv.args,
            ["--json", "session", "resume", FIXTURE_BRANCH, "--force"]
        );
        assert!(matches!(
            plan(ScenarioId::ResumeRestore, &probe(LegId::Reference)),
            Plan::NoAnalogue(note) if note.contains("rift-only")
        ));
    }
}
