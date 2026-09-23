//! Driving the sweep: one sandbox per arm, the session a live commit meets,
//! and the commits that are actually timed.
//!
//! An arm-local failure is recorded on that arm rather than returned, so one
//! sandbox that would not start never costs the run its other arms — including
//! the control every one of them is quoted against.

use std::{collections::BTreeMap, fs, path::Path};

use super::{
    arm::{arms, Arm, Capture, Corpus, CORPUS_DIR},
    oracle::{has_trailer, parse_explain_files, parse_verify, projection_sizes, Oracles},
    phase::parse_timings,
    sweep::{ArmResult, HookLatencyReport, BUDGET_MS, HOOK_LATENCY_SCHEMA},
};
use crate::{
    hermetic::Hermetic,
    leg::{LegId, LegResolver},
    measure::{measure, run_once, Invocation, Protocol, Stats},
    report::{now_unix, Host},
    scenario::{self, HookEvent, PlanCtx},
    BenchError, Result,
};

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
/// Spelled once: [`SEEDED_FILE`] is written by `drive`, read by `read_pair`,
/// and the pair is what makes `checkpoint explain` answerable.
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
}
