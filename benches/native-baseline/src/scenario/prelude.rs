//! What runs before the timer starts: the untimed steps that put a repository
//! and a session in the state the timed verb meets in life.
//!
//! Its own module because a prelude is where a benchmark flatters itself — a
//! verb timed against a session that never opened a turn does no work — so the
//! steps each row needs are stated in one place, in execution order.

use std::path::{Path, PathBuf};

use crate::{leg::LegId, measure::Invocation};

use super::{
    catalogue::{spec, ScenarioId, ScenarioSpec},
    event::{
        edit_body, serve_policy_path, HookEvent, CAPTURE_SYNC_ENV, EDIT_TARGET, ENFORCE_POLICY,
    },
    plan::{enable_agent, git_at, member_at, member_branch, switch_branch, PlanCtx},
};

/// One untimed preparation step.
#[derive(Debug, Clone)]
pub enum Step {
    /// Run a process (a hook, a `git commit`, a corpus plant).
    Invoke(Invocation),
    /// Write a file (a transcript, a fixture file the turn edits).
    Write(PathBuf, String),
}

/// Untimed preparation for one iteration: **ordered** steps, run in the
/// order declared. Order is part of the workload — a transcript must exist
/// before the session start that names it and be written last for the verb
/// that reads it, a file must be written inside the turn that edits it and
/// committed after the turn closes — so the prelude is a list of steps, not
/// two lists with a fixed relative order.
#[derive(Debug, Clone, Default)]
pub struct Prelude {
    /// The steps, in execution order.
    pub steps: Vec<Step>,
}

impl Prelude {
    /// The invocations, in order.
    #[must_use]
    pub fn invocations(&self) -> Vec<&Invocation> {
        self.steps
            .iter()
            .filter_map(|s| match s {
                Step::Invoke(inv) => Some(inv),
                Step::Write(..) => None,
            })
            .collect()
    }

    /// The files written, in order.
    #[must_use]
    pub fn files(&self) -> Vec<(&Path, &str)> {
        self.steps
            .iter()
            .filter_map(|s| match s {
                Step::Write(path, body) => Some((path.as_path(), body.as_str())),
                Step::Invoke(_) => None,
            })
            .collect()
    }

    /// How many steps deliver an agent hook (`hooks <agent> <verb>`): the
    /// invocations that each owe one durable event. A `git commit` or a
    /// corpus plant is an invocation and not a hook, and is not counted.
    #[must_use]
    pub fn hook_invocations(&self) -> u64 {
        self.invocations()
            .iter()
            .filter(|inv| inv.args.first().is_some_and(|a| a == "hooks"))
            .count() as u64
    }
}

/// A synthetic Claude Code transcript tail: the offset window reader reads the
/// last assistant records for provider-reported usage, so the enrichment the
/// `Stop` and `PreToolUse` paths perform is actually exercised.
#[must_use]
pub fn transcript_body(turns: usize) -> String {
    let mut out = String::new();
    for turn in 0..turns.max(1) {
        out.push_str(
            &serde_json::json!({
                "type": "user",
                "message": { "role": "user", "content": "add a test for the parser" },
            })
            .to_string(),
        );
        out.push('\n');
        out.push_str(
            &serde_json::json!({
                "type": "assistant",
                "message": {
                    "role": "assistant",
                    "model": "claude-opus-4",
                    "usage": {
                        "input_tokens": 1_200 + turn,
                        "output_tokens": 340,
                        "cache_read_input_tokens": 18_000,
                        "cache_creation_input_tokens": 900,
                    },
                },
            })
            .to_string(),
        );
        out.push('\n');
    }
    out
}

/// The untimed per-iteration preparation for `id`.
#[must_use]
pub fn prelude(id: ScenarioId, ctx: &PlanCtx<'_>) -> Prelude {
    let spec = spec(id);
    if ctx.leg == LegId::Rift {
        if let Some(prepared) = row_prelude(id, ctx, &spec) {
            return prepared;
        }
    }
    let Some(event) = spec.hook_event else {
        return Prelude::default();
    };
    hook_prelude(id, ctx, &spec, event)
}

/// The preparation a named row brings of its own: every row whose timed verb
/// is not a hook, and which therefore has a corpus, a whole session or a
/// member to put in place first. `None` for a row that needs none.
///
/// Rift-leg only. The backend leg has no counterpart for any of these verbs,
/// so preparing for them would be preparing for something it never runs.
fn row_prelude(id: ScenarioId, ctx: &PlanCtx<'_>, spec: &ScenarioSpec) -> Option<Prelude> {
    Some(match id {
        ScenarioId::ImportThroughput => import_prelude(ctx),
        ScenarioId::ResumeRestore => resume_prelude(ctx, spec),
        ScenarioId::BlameWhy => why_prelude(ctx, spec),
        ScenarioId::WorktreeAddAdopt => worktree_add_prelude(ctx),
        ScenarioId::MemberSwitch => member_switch_prelude(ctx),
        ScenarioId::RecallKnownState => known_state_prelude(ctx, spec),
        ScenarioId::RecallCompounding => compounding_prelude(ctx, spec),
        ScenarioId::RecallInterceptSavings => intercept_prelude(ctx, spec),
        _ => return None,
    })
}

/// A hook row's preparation: what the gate rows need on disk, the events the
/// timed verb must find behind it, and the transcript, written last.
fn hook_prelude(
    id: ScenarioId,
    ctx: &PlanCtx<'_>,
    spec: &ScenarioSpec,
    event: HookEvent,
) -> Prelude {
    let session = ctx.session_for(spec);
    let nth = spec.nth_event(ctx.iteration);
    let mut steps: Vec<Step> = Vec::new();
    // What the two gate rows need on disk before their verb runs: the policy
    // that asks for declines, and the file an edit is about to be recorded
    // against. Written rather than invoked, because neither is work the timed
    // verb should be charged for.
    if id == ScenarioId::HookReadGate {
        steps.push(Step::Write(
            serve_policy_path(ctx.repo),
            ENFORCE_POLICY.to_owned(),
        ));
    }
    if id == ScenarioId::HookEditRecord {
        steps.push(Step::Write(ctx.repo.join(EDIT_TARGET), edit_body(nth)));
    }
    steps.extend(
        preceding_events(event)
            .iter()
            .map(|e| Step::Invoke(e.invocation(ctx, &session, nth))),
    );
    // Written last so its mtime is the timed verb's own present: a Stop
    // must not be measured reading a transcript that has been quiet for
    // seconds, and a prompt that settles the previous turn reads bytes
    // that were written moments ago.
    if event.carries_transcript() {
        steps.push(Step::Write(
            ctx.transcript_path(&session),
            transcript_body(8),
        ));
    }
    Prelude { steps }
}

/// What has to have happened before `event` for the step it times to do its
/// whole work.
fn preceding_events(event: HookEvent) -> &'static [HookEvent] {
    use HookEvent::{PromptSubmit, SessionStart, Stop, ToolCallBash};
    match event {
        // A session's first hook follows nothing by definition; a prompt or a
        // tool call happens many times inside an established session, so
        // iteration i is simply the next one.
        HookEvent::SessionStart
        | HookEvent::PromptSubmit
        | HookEvent::ToolCallBash
        | HookEvent::ToolCallRead => &[],
        // A result needs the call it is a result of.
        HookEvent::ToolResult => &[ToolCallBash],
        HookEvent::ToolReadResult => &[HookEvent::ToolCallRead],
        // A turn end needs a turn: prompt, tool call, result, then Stop.
        HookEvent::Stop => &[PromptSubmit, ToolCallBash, HookEvent::ToolResult],
        // A session end needs a whole session, and a new one each iteration.
        HookEvent::SessionEnd => &[SessionStart, PromptSubmit, ToolCallBash, Stop],
        // A first read of the very lines the timed repeat asks for again:
        // without it the timed call is a first read and pays none of the
        // decision this row exists to time.
        HookEvent::ToolCallReadRepeat => &[HookEvent::ToolCallReadAbs],
        // An absolute first read, a result of one, and an edit each stand
        // alone: nothing has to have happened before them for the step they
        // time to do its whole work.
        HookEvent::ToolCallReadAbs | HookEvent::ToolReadAbsResult | HookEvent::ToolEditRecord => {
            &[]
        }
    }
}

/// The import row's untimed preparation is the corpus itself: one
/// `fast-import` child plants the reconstructed shards on the legacy
/// checkpoint branch of this iteration's own fresh copy.
///
/// Outside the timed region, like every other prelude — the rate is about
/// reading them, not about writing them. A missing frozen fixture yields no
/// invocation here and the row then measures a repository with nothing in it,
/// which the work probe reports as zero rather than as a fast import.
fn import_prelude(ctx: &PlanCtx<'_>) -> Prelude {
    let plant = crate::legacy::plant(
        &crate::leg::default_repo_root(),
        ctx.repo,
        crate::legacy::SHARD_COUNT,
    );
    Prelude {
        steps: plant.into_iter().map(Step::Invoke).collect(),
    }
}

/// An enabled agent is what makes the timed `git worktree add` the FULL
/// adoption shape: without one the member never gets an ownership manifest,
/// and the row would publish a third, cheaper shape under this row's name.
fn worktree_add_prelude(ctx: &PlanCtx<'_>) -> Prelude {
    Prelude {
        steps: vec![Step::Invoke(enable_agent(ctx))],
    }
}

/// The member is created and adopted HERE, untimed: the timed verb is the
/// switch that follows an adoption, which is every switch after the first and
/// so the case a person actually pays.
fn member_switch_prelude(ctx: &PlanCtx<'_>) -> Prelude {
    let nth = ctx.iteration;
    let member = member_at(ctx);
    Prelude {
        steps: vec![
            Step::Invoke(enable_agent(ctx)),
            Step::Invoke(git_at(ctx.repo).args([
                "worktree".to_owned(),
                "add".to_owned(),
                "--quiet".to_owned(),
                member.display().to_string(),
                "-b".to_owned(),
                member_branch(nth),
            ])),
            Step::Invoke(git_at(ctx.repo).args(["branch".to_owned(), switch_branch(nth)])),
        ],
    }
}

/// One whole session on this iteration's own session id — transcript first
/// (the session start names it), then start / prompt / tool call / Stop.
fn session_steps(ctx: &PlanCtx<'_>, session: &str, nth: usize) -> Vec<Step> {
    use HookEvent::{PromptSubmit, SessionStart, Stop, ToolCallBash};
    let mut steps = vec![Step::Write(
        ctx.transcript_path(session),
        transcript_body(2),
    )];
    steps.extend(
        [SessionStart, PromptSubmit, ToolCallBash, Stop]
            .iter()
            .map(|e| Step::Invoke(e.invocation(ctx, session, nth))),
    );
    steps
}

/// Write `rel_path` in the repository and commit it **through the
/// repository's own hooks** with an inline archive, so the commit carries its
/// `Rift-Checkpoint` trailer and the checkpoint shard has landed when the
/// step returns. The prelude runs with hooks enabled (see
/// [`crate::hermetic::Hooks`]); `git` is the one on `PATH`, as the fixture
/// generator's is.
fn commit_steps(ctx: &PlanCtx<'_>, rel_path: &str, body: &str, message: &str) -> Vec<Step> {
    let git = |args: &[&str]| {
        Step::Invoke(
            Invocation::new("git", ctx.repo)
                .args(args.to_vec())
                .env(CAPTURE_SYNC_ENV, "1"),
        )
    };
    vec![
        Step::Write(ctx.repo.join(rel_path), body.to_owned()),
        git(&["add", "--", rel_path]),
        git(&["commit", "--quiet", "--message", message]),
    ]
}

/// The resume row's prelude: a session, a commit that condenses it (so the
/// branch's latest checkpoint is this iteration's), nothing else. The timed
/// verb then restores exactly that session.
fn resume_prelude(ctx: &PlanCtx<'_>, spec: &ScenarioSpec) -> Prelude {
    let session = ctx.session_for(spec);
    let nth = spec.nth_event(ctx.iteration);
    let mut steps = session_steps(ctx, &session, nth);
    steps.extend(commit_steps(
        ctx,
        &format!("bench/resume_{nth}.txt"),
        &format!("resume fixture {nth}\n"),
        &format!("bench: resume fixture {nth}"),
    ));
    Prelude { steps }
}

/// Run these steps' hooks **inline**.
///
/// A non-hook row names no detached journal (`detached_journal`), so the
/// harness does not wait for one — which is correct for what these rows time
/// and wrong for what their preludes produce. Capturing inline is what keeps
/// the two consistent: the reads a serve is about to be measured over have
/// landed before the timer starts, rather than racing it from a worker.
fn inline(steps: Vec<Step>) -> Vec<Step> {
    steps
        .into_iter()
        .map(|step| match step {
            Step::Invoke(inv) => Step::Invoke(inv.env(CAPTURE_SYNC_ENV, "1")),
            write @ Step::Write(..) => write,
        })
        .collect()
}

/// The serve both recall rows time is served out of a session somebody played:
/// recall on, one whole session whose turn READS a file, and a commit that
/// archives the checkpoint the reads hang from — all in the **main work tree**.
fn recall_session_steps(ctx: &PlanCtx<'_>, spec: &ScenarioSpec) -> Vec<Step> {
    use HookEvent::{PromptSubmit, SessionStart, Stop, ToolCallRead, ToolReadResult};
    let session = ctx.session_for(spec);
    let nth = spec.nth_event(ctx.iteration);
    // Recall is off by default and `setup_plan` keeps it off (`--no-recall`),
    // so a row that serves has to turn it on. Idempotent.
    let mut steps = vec![Step::Invoke(
        Invocation::new(ctx.binary, ctx.repo).args(["recall", "init"]),
    )];
    // Stage the policy file `recall init` wrote, so the fixture commit below
    // carries it and a member cut from the tip inherits recall ON by checking
    // it out — the tracked-policy convention the product reads per worktree
    // (`EffectiveConfig::load(repo/.rift)`) and the compounding fixture
    // states outright ("the policy every member inherits by checking it
    // out"). Without this the member serve exits `recall_disabled` and the
    // compounding row measures a refusal. Idempotent under Shared isolation.
    steps.push(Step::Invoke(
        Invocation::new("git", ctx.repo)
            .args(["add", "--", ".rift/config.toml"])
            .env(CAPTURE_SYNC_ENV, "1"),
    ));
    steps.push(Step::Write(
        ctx.transcript_path(&session),
        transcript_body(2),
    ));
    steps.extend(inline(
        [
            SessionStart,
            PromptSubmit,
            ToolCallRead,
            ToolReadResult,
            Stop,
        ]
        .iter()
        .map(|e| Step::Invoke(e.invocation(ctx, &session, nth)))
        .collect(),
    ));
    steps.extend(commit_steps(
        ctx,
        &format!("bench/recall_{nth}.txt"),
        &format!("recall fixture {nth}\n"),
        &format!("bench: recall fixture {nth}"),
    ));
    steps
}

/// The ancestry row serves from the main work tree, so the session it serves
/// over is the whole of its preparation.
fn known_state_prelude(ctx: &PlanCtx<'_>, spec: &ScenarioSpec) -> Prelude {
    Prelude {
        steps: recall_session_steps(ctx, spec),
    }
}

/// The compounding row cuts one linked worktree from the tip afterwards, so
/// its `HEAD` is the checkpoint's own anchor commit: the member is on the
/// family's line and the serve it is about to be given compounds a sibling's
/// reads as branch containment requires.
///
/// Cut last, deliberately — a member created before the commit would sit
/// behind the anchor and containment would rightly hold the whole thing back,
/// which is a different row's claim.
fn compounding_prelude(ctx: &PlanCtx<'_>, spec: &ScenarioSpec) -> Prelude {
    let nth = spec.nth_event(ctx.iteration);
    let mut steps = recall_session_steps(ctx, spec);
    steps.push(Step::Invoke(git_at(ctx.repo).args([
        "worktree".to_owned(),
        "add".to_owned(),
        "--quiet".to_owned(),
        member_at(ctx).display().to_string(),
        "-b".to_owned(),
        member_branch(nth),
    ])));
    Prelude { steps }
}

/// How many files the intercept row's scripted session reads and then re-reads.
///
/// Four rather than one, so the residue is a sum over a session rather than a
/// statement about a single file, and few enough that a heavy-protocol row
/// stays minutes.
pub const INTERCEPT_FILES: usize = 4;

/// The intercept row's preparation: recall on, this repository asking for
/// declines, and one whole session that reads four files, is handed each of
/// them, then asks for every one of them again.
///
/// The repeats are delivered as the calls they are (`PreToolUse`) and their
/// results are deliberately NOT delivered, because a declined call has no
/// result — that is the shape of the saving, and a script that reported one
/// anyway would put the pool back into the projection by hand.
fn intercept_prelude(ctx: &PlanCtx<'_>, spec: &ScenarioSpec) -> Prelude {
    use HookEvent::{
        PromptSubmit, SessionStart, Stop, ToolCallReadAbs, ToolCallReadRepeat, ToolReadAbsResult,
    };
    let session = ctx.session_for(spec);
    let nth = spec.nth_event(ctx.iteration);
    let mut steps = vec![
        Step::Invoke(Invocation::new(ctx.binary, ctx.repo).args(["recall", "init"])),
        Step::Write(serve_policy_path(ctx.repo), ENFORCE_POLICY.to_owned()),
        Step::Write(ctx.transcript_path(&session), transcript_body(2)),
    ];
    let mut beats: Vec<Invocation> = vec![
        SessionStart.invocation(ctx, &session, nth),
        PromptSubmit.invocation(ctx, &session, nth),
    ];
    // The reads, each with the result that makes it a fact the projection
    // classes; then the repeats, which is the pool.
    for k in 0..INTERCEPT_FILES {
        let file = nth * INTERCEPT_FILES + k;
        beats.push(ToolCallReadAbs.invocation(ctx, &session, file));
        beats.push(ToolReadAbsResult.invocation(ctx, &session, file));
    }
    for k in 0..INTERCEPT_FILES {
        beats.push(ToolCallReadRepeat.invocation(ctx, &session, nth * INTERCEPT_FILES + k));
    }
    beats.push(Stop.invocation(ctx, &session, nth));
    steps.extend(inline(beats.into_iter().map(Step::Invoke).collect()));
    steps.extend(commit_steps(
        ctx,
        &format!("bench/intercept_{nth}.txt"),
        &format!("intercept fixture {nth}\n"),
        &format!("bench: intercept fixture {nth}"),
    ));
    Prelude { steps }
}

/// The `why` row's prelude: a session whose turn WRITES the file (the write
/// lands between the tool call and the Stop, inside the turn's boundaries),
/// then a commit through the hooks so the line has a commit, the commit a
/// checkpoint, and the checkpoint a prompt.
fn why_prelude(ctx: &PlanCtx<'_>, spec: &ScenarioSpec) -> Prelude {
    let session = ctx.session_for(spec);
    let nth = spec.nth_event(ctx.iteration);
    let mut steps = session_steps(ctx, &session, nth);
    let rel_path = format!("bench/why_{nth}.txt");
    let stop = steps.pop();
    steps.push(Step::Write(
        ctx.repo.join(&rel_path),
        format!("why fixture {nth}: written inside the turn\n"),
    ));
    steps.extend(stop);
    let git = |args: &[&str]| {
        Step::Invoke(
            Invocation::new("git", ctx.repo)
                .args(args.to_vec())
                .env(CAPTURE_SYNC_ENV, "1"),
        )
    };
    steps.push(git(&["add", "--", &rel_path]));
    steps.push(git(&[
        "commit",
        "--quiet",
        "--message",
        &format!("bench: why fixture {nth}"),
    ]));
    Prelude { steps }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::leg::LegId;
    use crate::scenario::tests::*;
    use crate::scenario::*;

    /// Each row's untimed preparation leaves exactly the state its timed verb
    /// needs — and, for the rows that capture, leaves it INLINE, because a
    /// non-hook row names no detached journal for the harness to wait on.
    #[test]
    fn the_shadow_worktree_preludes_leave_what_the_timed_verb_needs() {
        let ctx = probe(LegId::Rift);
        let member = member_at(&ctx).display().to_string();

        // The add row must not create the member itself — the timed verb does.
        let add = prelude(ScenarioId::WorktreeAddAdopt, &ctx);
        assert!(add
            .invocations()
            .iter()
            .all(|inv| !inv.args.contains(&member)));
        assert!(
            add.invocations()
                .iter()
                .any(|inv| inv.args.first().is_some_and(|a| a == "enable")),
            "without an enabled agent this row measures a cheaper shape"
        );

        // The switch row must: the timed switch happens in a member that is
        // already there and already adopted.
        let switch = prelude(ScenarioId::MemberSwitch, &ctx);
        assert!(switch
            .invocations()
            .iter()
            .any(|inv| inv.args.contains(&member)));
        assert!(switch.invocations().iter().any(|inv| inv
            .args
            .first()
            .is_some_and(|a| a == "branch")
            && inv.args.contains(&switch_branch(0))));

        for id in [ScenarioId::RecallKnownState, ScenarioId::RecallCompounding] {
            let p = prelude(id, &ctx);
            assert!(
                p.invocations()
                    .iter()
                    .any(|inv| inv.args == ["recall", "init"]),
                "{}: recall is off by default and setup keeps it off",
                id.as_str()
            );
            assert!(
                p.hook_invocations() > 0,
                "{}: a serve over a session nobody played serves nothing",
                id.as_str()
            );
            assert!(
                p.invocations()
                    .iter()
                    .filter(|inv| inv.program == Path::new("git")
                        || inv.args.first().is_some_and(|a| a == "hooks"))
                    .all(|inv| inv
                        .env
                        .iter()
                        .any(|(k, v)| k == CAPTURE_SYNC_ENV && v == "1")),
                "{}: every step that can capture does it inline, or the timer races the worker",
                id.as_str()
            );
            assert_eq!(
                detached_journal(id, &ctx),
                None,
                "{}: a row that waits on no journal must leave none owed",
                id.as_str()
            );
            let creates_member = p.invocations().iter().any(|inv| inv.args.contains(&member));
            assert_eq!(
                creates_member,
                id == ScenarioId::RecallCompounding,
                "{}: only the compounding row is served from a member",
                id.as_str()
            );
        }
    }

    #[test]
    fn a_stop_closes_a_turn_it_opened_and_leaves_the_transcript_for_the_next_hook() {
        let c = probe(LegId::Rift);
        let pre = prelude(ScenarioId::HookStop, &c);
        let verbs: Vec<_> = pre
            .invocations()
            .iter()
            .map(|i| i.args[2].clone())
            .collect();
        assert_eq!(verbs, ["UserPromptSubmit", "PreToolUse", "PostToolUse"]);
        assert!(
            matches!(pre.steps.last(), Some(Step::Write(..))),
            "a hook prelude ends with the transcript write"
        );
        // One transcript, written after the prelude ran, where the payload
        // says — and named on EVERY hook of the session, because the settle
        // that records this turn's spend runs on the next hook, not on the
        // Stop (the deferred-settle contract the row's title states).
        assert!(spec(ScenarioId::HookStop)
            .title
            .contains("settled by the next hook"));
        for verb in HookEvent::all_for_test() {
            assert!(verb.carries_transcript(), "{}", verb.verb());
            assert!(
                payload_of(&verb.invocation(&c, BENCH_SESSION, 0))["transcript_path"].is_string(),
                "{} names the transcript",
                verb.verb()
            );
        }
        let (path, body) = pre
            .files()
            .first()
            .map(|(p, b)| (p.to_path_buf(), (*b).to_owned()))
            .expect("a stop leaves a transcript for the next hook");
        let Plan::Run(inv) = plan(ScenarioId::HookStop, &c) else {
            panic!("wired")
        };
        assert_eq!(
            payload_of(&inv)["transcript_path"],
            path.display().to_string()
        );
        assert!(
            !path.starts_with(c.repo),
            "a transcript is not repo content"
        );
        // It parses as the JSONL the usage enrichment reads.
        let lines: Vec<_> = body.lines().collect();
        assert_eq!(lines.len(), 16);
        for line in &lines {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(v["type"].is_string());
        }
        assert!(body.ends_with('\n'), "every record is newline-terminated");
        let last: serde_json::Value = serde_json::from_str(lines[15]).unwrap();
        assert_eq!(last["type"], "assistant");
        assert!(last["message"]["usage"]["input_tokens"].is_u64());
    }

    #[test]
    fn a_session_end_replays_a_whole_session_first() {
        let verbs: Vec<_> = prelude(ScenarioId::HookSessionEnd, &probe(LegId::Rift))
            .invocations()
            .iter()
            .map(|i| i.args[2].clone())
            .collect();
        assert_eq!(
            verbs,
            ["SessionStart", "UserPromptSubmit", "PreToolUse", "Stop"]
        );
        // A session start follows nothing at all.
        assert!(prelude(ScenarioId::HookSessionStart, &probe(LegId::Rift))
            .invocations()
            .is_empty());
        // And a read-only verb has no prelude at all.
        assert!(prelude(ScenarioId::ColdStatus, &probe(LegId::Rift))
            .steps
            .is_empty());
    }

    /// The why row's prelude writes the file INSIDE the turn (between the tool
    /// call and the Stop) and commits it through the repository's own hooks,
    /// so the line has a commit, the commit a checkpoint and the checkpoint a
    /// prompt by the time the timed verb asks.
    #[test]
    fn the_why_prelude_writes_the_file_inside_the_turn() {
        let c = probe(LegId::Rift);
        let s = spec(ScenarioId::BlameWhy);
        let pre = prelude(ScenarioId::BlameWhy, &c);
        let session = c.session_for(&s);
        assert_eq!(
            step_shape(&pre),
            [
                format!("write {session}.jsonl"),
                "rift hooks claude-code SessionStart".to_owned(),
                "rift hooks claude-code UserPromptSubmit".to_owned(),
                "rift hooks claude-code PreToolUse".to_owned(),
                "write why_0.txt".to_owned(),
                "rift hooks claude-code Stop".to_owned(),
                "git add -- bench/why_0.txt".to_owned(),
                "git commit --quiet --message".to_owned(),
            ]
        );
        let commit = pre.invocations().last().copied().unwrap();
        assert!(commit
            .env
            .iter()
            .any(|(k, v)| k == CAPTURE_SYNC_ENV && v == "1"));
        assert_eq!(pre.hook_invocations(), 4);
        assert_eq!(durable_events_per_iteration(ScenarioId::BlameWhy), 0);
    }

    /// The resume row's prelude plays a session on a session id of its own and
    /// commits it THROUGH the hooks with the archive inline, so the branch's
    /// latest checkpoint is this iteration's by the time the timed verb runs.
    #[test]
    fn the_resume_prelude_lands_a_checkpoint_inline() {
        let c = probe(LegId::Rift);
        let s = spec(ScenarioId::ResumeRestore);
        let pre = prelude(ScenarioId::ResumeRestore, &c);
        let session = c.session_for(&s);
        assert_eq!(
            step_shape(&pre),
            [
                format!("write {session}.jsonl"),
                "rift hooks claude-code SessionStart".to_owned(),
                "rift hooks claude-code UserPromptSubmit".to_owned(),
                "rift hooks claude-code PreToolUse".to_owned(),
                "rift hooks claude-code Stop".to_owned(),
                "write resume_0.txt".to_owned(),
                "git add -- bench/resume_0.txt".to_owned(),
                "git commit --quiet --message".to_owned(),
            ]
        );
        // The commit condenses inline, through the repository's own hooks.
        let commit = pre.invocations().last().copied().unwrap();
        assert_eq!(commit.hooks, crate::hermetic::Hooks::Enabled);
        assert!(commit
            .env
            .iter()
            .any(|(k, v)| k == CAPTURE_SYNC_ENV && v == "1"));
        assert_eq!(pre.hook_invocations(), 4);
        assert_eq!(
            durable_events_per_iteration(ScenarioId::ResumeRestore),
            0,
            "not a hook row: it owes no ingest proof, it owes an ANSWER"
        );
    }
}
