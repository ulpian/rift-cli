//! What the harness reads back: the untimed probes that prove a measured
//! iteration really ingested, really answered and really did the work its rate
//! is divided by, plus the parsers that read each answer.
//!
//! Separate from the plan because a probe never runs inside a timed region: a
//! wall time is a claim about a workload, and these are what makes the claim
//! falsifiable.

use std::path::{Path, PathBuf};

use crate::{leg::LegId, measure::Invocation};

use super::{
    catalogue::{spec, ScenarioId},
    plan::{member_at, plan, reread_report_at, serve_at, Plan, PlanCtx},
    prelude::prelude,
};

/// Where a row's timed verb leaves work for the detached agent-hook worker
/// — `<repo>/.git/rift-agent-queue`, the journal a `SessionEnd` condense and
/// a read's BPE count are handed to (an owner decision of 2026-08-30) — or `None`
/// for a row that detaches nothing.
///
/// The harness WAITS for that journal to drain, and only ever outside the
/// timed region: before the next iteration's untimed prelude, and once more
/// after the window before the probes. Two reasons, both about honesty. A
/// worker still condensing the previous session while the next iteration's
/// verb is timed would be measured as contention the budget never meant;
/// and a row whose deferred work never lands has published the latency of
/// writing a note, so the wait is bounded and a journal that will not drain
/// is a failed measure, not a fast one.
#[must_use]
pub fn detached_journal(id: ScenarioId, ctx: &PlanCtx<'_>) -> Option<PathBuf> {
    match (ctx.leg, spec(id).hook_event) {
        (LegId::Rift, Some(_)) => Some(ctx.repo.join(".git").join("rift-agent-queue")),
        _ => None,
    }
}

/// The untimed check that a row's verb really ANSWERED — ran once more
/// outside the timed region, over the same repository state — for verbs
/// whose wall time is meaningless without their answer: a `session resume`
/// that restored nothing and a `why` that resolved no checkpoint both exit
/// 0 quickly, and publishing either as a latency would be timing a refusal.
#[must_use]
pub fn answer_probe(id: ScenarioId, ctx: &PlanCtx<'_>) -> Option<Invocation> {
    match (id, ctx.leg) {
        (
            ScenarioId::ResumeRestore
            | ScenarioId::BlameWhy
            | ScenarioId::RecallInterceptSavings
            | ScenarioId::HookReadGate,
            LegId::Rift,
        ) => match plan(id, ctx) {
            Plan::Run(inv) => Some(*inv),
            Plan::NoAnalogue(_) => None,
        },
        _ => None,
    }
}

/// Whether this row's answer probe has to be asked a FRESH question — its own
/// untimed prelude replayed, one iteration past the window — rather than the
/// last measured iteration's question asked again.
///
/// The read gate answers a key once: the window it just declined would allow
/// the same call on a re-run, and a probe reading that Allow would report the
/// opposite of what the samples did. Replaying the prelude gives the probe a
/// window of its own, a first read of its own, and therefore the same decision
/// the timed samples were paid for.
#[must_use]
pub const fn probe_replays_the_prelude(id: ScenarioId) -> bool {
    matches!(id, ScenarioId::HookReadGate)
}

/// Read an [`answer_probe`]'s stdout: `Ok` when the verb answered, `Err`
/// with the reason when it did not. `None` for a row with no probe.
#[must_use]
pub fn parse_answer(id: ScenarioId, stdout: &str) -> Option<Result<(), String>> {
    let answered = match id {
        ScenarioId::ResumeRestore => resume_restored,
        ScenarioId::BlameWhy => why_resolved,
        ScenarioId::RecallInterceptSavings => report_saw_the_session,
        ScenarioId::HookReadGate => gate_declined,
        _ => return None,
    };
    let json = serde_json::from_str::<serde_json::Value>(stdout)
        .map_err(|e| format!("the answer is not JSON: {e}"));
    Some(json.and_then(|value| answered(&value)))
}

/// A resume that restores nothing is a refusal, not a latency.
fn resume_restored(answer: &serde_json::Value) -> Result<(), String> {
    let restored = answer
        .get("sessions")
        .and_then(|sessions| sessions.get(0))
        .and_then(|session| session.get("restored"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("absent");
    if restored == "restored" {
        return Ok(());
    }
    Err(format!(
        "session resume restored no transcript (sessions[0].restored = {restored:?}): \
         a resume that restores nothing is a refusal, not a latency"
    ))
}

/// A `why` that resolved no checkpoint answered nothing.
fn why_resolved(answer: &serde_json::Value) -> Result<(), String> {
    let resolved = answer
        .get("selection")
        .and_then(|sel| sel.get("checkpoint_id"))
        .and_then(serde_json::Value::as_str);
    if resolved.is_some() {
        return Ok(());
    }
    Err(
        "why resolved no checkpoint for the line (selection.checkpoint_id is not a \
         string): an unresolved why is a refusal, not a latency"
            .to_owned(),
    )
}

/// A report that classified no reads at all saw no session, and its residue of
/// zero would read as a perfect saving rather than as an empty fixture — which
/// is the one way this row could flatter itself.
fn report_saw_the_session(answer: &serde_json::Value) -> Result<(), String> {
    let reads = answer
        .get("repeat_classes")
        .and_then(|c| c.get("first_read"))
        .and_then(|c| c.get("reads"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or_default();
    if reads > 0 {
        return Ok(());
    }
    Err(format!(
        "the scripted session left {reads} first reads in the projection: a residue \
         measured over an empty pool is an empty fixture, not a saving"
    ))
}

/// A budget row is a claim about the arm it names. This one names the decline
/// — the arm that hashes, reads the journal, decides, composes a reason and
/// journals the accounting — and the allow path is cheaper than all of that.
///
/// A row that timed the cheap arm would make its own bar easier to clear, so
/// the probe reads the decision out of the same envelope an agent reads it
/// from and fails the measure on anything but a decline.
fn gate_declined(answer: &serde_json::Value) -> Result<(), String> {
    let decision = answer
        .get("hookSpecificOutput")
        .and_then(|out| out.get("permissionDecision"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("absent");
    if decision == "deny" {
        return Ok(());
    }
    Err(format!(
        "the gate answered this call {decision:?}, not a decline: the row is \
         named for the arm that decides, and timing the arm that waves the \
         call through would publish the cheap half of the verb"
    ))
}

/// How many durable events one measured iteration of `id` owes: the timed
/// verb's own, plus one for every ingesting invocation in its untimed prelude.
///
/// The looser "at least one per iteration" floor a hook row started with is
/// satisfied by a row whose prelude ingests and whose *timed verb* does not —
/// the exact failure the proof exists to catch. Every prelude invocation is a
/// distinct hook event (they carry the same `nth` the timed verb does, and a
/// per-iteration row gives each iteration its own session), so each is worth
/// exactly one durable event and the floor is tight.
#[must_use]
pub fn durable_events_per_iteration(id: ScenarioId) -> u64 {
    let spec = spec(id);
    if !spec.requires_ingest() {
        return 0;
    }
    // Neither the paths nor the iteration change how many invocations a
    // prelude has — only the verb does.
    let ctx = PlanCtx {
        leg: LegId::Rift,
        binary: Path::new(""),
        repo: Path::new(""),
        sandbox: Path::new(""),
        iteration: 0,
        tool: None,
    };
    1 + prelude(id, &ctx).hook_invocations()
}

/// The untimed probe that counts how much work one measured iteration did.
///
/// A wall time is a claim about a workload, and the harness has to be able to
/// say what the workload was — otherwise "3 seconds" is unfalsifiable. The
/// probe runs **outside** every timed region, exactly as the ingest proof does,
/// and its count is published beside the timing rather than folded into it.
///
/// `None` where the leg or the row has nothing to count. That is reported as
/// absent, never as zero (see [`crate::measure::Stats::units_per_iteration`]).
#[must_use]
pub fn work_probe(id: ScenarioId, ctx: &PlanCtx<'_>) -> Option<Invocation> {
    match (id, ctx.leg) {
        // The verb under test is its own counter: it reports how many
        // checkpoints it re-derived. Run once more, untimed, so the count
        // covers the same repository state the published samples ran against.
        (ScenarioId::VerifyThroughput, LegId::Rift) => {
            Some(Invocation::new(ctx.binary, ctx.repo).args(["verify"]))
        }
        // The denominator is **measured, not scripted**. `vogon script` would
        // report the hooks the session *would* fire; the projection reports the
        // canonical events the session actually produced, so a drive that
        // silently stopped ingesting collapses the rate instead of holding it
        // up with a constant. Every condense iteration gets its own repository
        // copy, so this counts exactly one session's work.
        (ScenarioId::Condense, LegId::Rift) => {
            Some(Invocation::new(ctx.binary, ctx.repo).args(["status"]))
        }
        // The verb under test is its own counter again, and it can be: reading
        // a legacy history writes nothing to the source repository, so running
        // it once more untimed observes exactly the corpus the samples ran
        // against and disturbs nothing.
        (ScenarioId::ImportThroughput, LegId::Rift) => {
            Some(Invocation::new(ctx.binary, ctx.repo).args(["import", "--from-legacy"]))
        }
        // The serve is its own counter: it reports what it avoided, in the
        // accounting seam's own figure and the tokenizer that produced it. Run
        // once more untimed from the same member, over the same store — a
        // serve writes an audit row and no fact the next serve would read
        // differently, so the count covers exactly the state the samples ran
        // against.
        // The report is its own counter: it states how many tokens of this
        // session's reads it still classes as repeats of unchanged bytes. Run
        // once more untimed over the same repository state — the report
        // appends a metric snapshot and changes no fact a later run would read
        // differently.
        (ScenarioId::RecallInterceptSavings, LegId::Rift) => Some(reread_report_at(ctx)),
        (ScenarioId::RecallCompounding, LegId::Rift) => {
            // The caller's ctx sits one past the window, which is right for
            // repository-state probes but wrong here: the past-the-window
            // iteration's prelude never ran, so ITS member does not exist.
            // The store state every sample ran against is the family store,
            // and the last measured member serves over exactly that.
            let last = PlanCtx {
                iteration: ctx.iteration.saturating_sub(1),
                ..*ctx
            };
            Some(serve_at(&last, member_at(&last)))
        }
        _ => None,
    }
}

/// Read the work count out of a [`work_probe`]'s stdout.
#[must_use]
pub fn parse_work_units(id: ScenarioId, stdout: &str) -> Option<u64> {
    match id {
        // `verify: OK — re-derived 12 checkpoint(s) from …`
        ScenarioId::VerifyThroughput => stdout
            .split_once("re-derived ")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .and_then(|n| n.parse().ok()),
        ScenarioId::Condense => captured_events(stdout),
        // `importable N · refused M · events K` — the product's own count of
        // what it could read, never the count the harness planted.
        ScenarioId::ImportThroughput => crate::legacy::parse_importable(stdout),
        // `{"verb":…,"estimated_tokens_avoided":N,…}` — the product's own
        // figure, never one the harness re-derives.
        // `{"repeat_classes":{"reread_unchanged":{"tokens":N,…},…},…}` — the
        // product's own classification, never one the harness re-derives.
        ScenarioId::RecallInterceptSavings => serde_json::from_str::<serde_json::Value>(stdout)
            .ok()
            .and_then(|doc| {
                doc.get("repeat_classes")?
                    .get("reread_unchanged")?
                    .get("tokens")?
                    .as_u64()
            }),
        ScenarioId::RecallCompounding => serde_json::from_str::<serde_json::Value>(stdout)
            .ok()
            .and_then(|doc| {
                doc.get("estimated_tokens_avoided")
                    .and_then(serde_json::Value::as_u64)
            }),
        _ => None,
    }
}

/// The untimed check that a hook scenario really wrote something durable.
///
/// Only the rift leg can answer it: `rift status` publishes the projection's own
/// counters. The backend leg has no equivalent counter verb, so its hook numbers
/// are published without this proof and the report says which is which.
#[must_use]
pub fn ingest_probe(ctx: &PlanCtx<'_>) -> Option<Invocation> {
    match ctx.leg {
        LegId::Rift => Some(Invocation::new(ctx.binary, ctx.repo).args(["status"])),
        LegId::Reference => None,
    }
}

/// Read the durable event count out of `rift status`.
#[must_use]
pub fn captured_events(stdout: &str) -> Option<u64> {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("captured events:"))
        .and_then(|rest| rest.trim().parse().ok())
}

/// The untimed preparation a leg needs before its verbs mean anything: capture
/// has to be enabled in the copy under test. It runs outside every timed region.
#[must_use]
pub fn setup_plan(leg: LegId, binary: &Path, repo: &Path) -> Invocation {
    match leg {
        // `--no-recall` only. **`--no-provision` was removed by `5028107`
        // ("collapse the archive to the one route that exists") and this line
        // went on passing it**, so every wired row's setup exited 2 and the
        // harness has measured nothing since — discovered by running it, in a
        // sitting the machine was held quiet for. Provisioning is gone fully:
        // capture is native and there is nothing to download, so the flag is
        // not merely renamed, its whole reason is deleted.
        LegId::Rift => Invocation::new(binary, repo).args(["init", "--no-recall"]),
        // Flags read off the backend's own `enable --help`. `--agent` is not
        // cosmetic: without it `enable` runs an interactive configuration flow, and
        // a benchmark that waits on a prompt never finishes. The rest keep the run
        // local — no telemetry, no session pushes, no repository creation.
        LegId::Reference => Invocation::new(binary, repo).args([
            "enable",
            "--agent",
            "claude-code",
            "--local",
            "--telemetry=false",
            "--skip-push-sessions",
            "--no-init-repo",
        ]),
    }
}

/// Environment that keeps a leg's state inside `sandbox` instead of the
/// developer's home directory — an unisolated run measures whatever happened to
/// be in `~` and cannot be reproduced.
#[must_use]
pub fn leg_env(leg: LegId, sandbox: &Path) -> Vec<(String, String)> {
    let at = |sub: &str| sandbox.join(sub).display().to_string();
    match leg {
        LegId::Rift => vec![
            ("RIFT_CONFIG_DIR".into(), at("rift-config")),
            ("RIFT_CACHE_DIR".into(), at("rift-cache")),
            // Never prompt for a keychain unlock, never fetch a backend: a
            // benchmark that blocks on a dialog is not a benchmark.
            ("RIFT_NO_KEYCHAIN".into(), "1".into()),
            ("RIFT_NO_PROVISION".into(), "1".into()),
            ("RIFT_HEADLESS".into(), "1".into()),
            ("HOME".into(), at("home")),
        ],
        LegId::Reference => vec![
            ("HOME".into(), at("home")),
            ("XDG_CONFIG_HOME".into(), at("xdg-config")),
            ("XDG_DATA_HOME".into(), at("xdg-data")),
            // Telemetry and login state are network-shaped; the pinned binary runs
            // as a local benchmark fixture only.
            ("REFERENCE_TELEMETRY".into(), "0".into()),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::leg::LegId;
    use crate::scenario::tests::*;

    /// Every hook row on the rift leg names the worker's journal to wait on;
    /// no other row, and no reference-leg row, waits on anything.
    #[test]
    fn hook_rows_name_the_detached_journal_and_nothing_else_does() {
        let c = probe(LegId::Rift);
        for id in ScenarioId::all() {
            let named = detached_journal(id, &c);
            if spec(id).hook_event.is_some() {
                assert_eq!(
                    named.as_deref(),
                    Some(c.repo.join(".git").join("rift-agent-queue").as_path()),
                    "{}",
                    id.as_str()
                );
            } else {
                assert_eq!(named, None, "{}", id.as_str());
            }
            assert_eq!(
                detached_journal(id, &probe(LegId::Reference)),
                None,
                "{}: the reference leg detaches nothing the harness knows of",
                id.as_str()
            );
        }
    }

    #[test]
    fn the_ingest_floor_counts_the_prelude_as_well_as_the_timed_verb() {
        // One per row, spelled out: these are the numbers the recorded baseline
        // observed (+30 / +60 / +120 / +150 over 30 iterations), so the floor is
        // an equality in practice and a row that quietly stopped ingesting
        // cannot hide behind its prelude's growth.
        for (id, expected) in [
            (ScenarioId::HookRoundTrip, 1),
            (ScenarioId::HookSessionStart, 1),
            (ScenarioId::HookPromptSubmit, 1),
            (ScenarioId::HookReadTool, 1),
            (ScenarioId::HookToolResult, 2),
            (ScenarioId::HookStop, 4),
            (ScenarioId::HookSessionEnd, 5),
            // A replay writes nothing by design and is asked for no proof.
            (ScenarioId::HookReplay, 0),
            // Not a hook row at all.
            (ScenarioId::ColdStatus, 0),
        ] {
            assert_eq!(
                durable_events_per_iteration(id),
                expected,
                "{}",
                id.as_str()
            );
        }
        // The floor is exactly "the timed verb plus its prelude", for every row.
        for id in ScenarioId::hooks() {
            let s = spec(id);
            let pre = prelude(id, &probe(LegId::Rift)).hook_invocations();
            assert_eq!(
                durable_events_per_iteration(id),
                if s.requires_ingest() { 1 + pre } else { 0 },
                "{}",
                id.as_str()
            );
        }
    }

    #[test]
    fn ingest_is_proved_from_the_projections_own_counter() {
        let real = "rift status\n  checkpoints:   0\n  sessions:      1\n  \
                    captured events: 42\n  hook receipts: 42\n  transitions:   3\n";
        assert_eq!(captured_events(real), Some(42));
        assert_eq!(captured_events("rift status\n  sessions: 1\n"), None);
        assert_eq!(captured_events(""), None);
        assert_eq!(captured_events("captured events: not-a-number"), None);

        let inv = ingest_probe(&probe(LegId::Rift)).expect("rift publishes counters");
        assert_eq!(inv.args, ["status"]);
        assert!(
            ingest_probe(&probe(LegId::Reference)).is_none(),
            "the backend has no counter verb; its hook numbers carry no proof"
        );
    }

    #[test]
    fn setup_never_provisions_or_phones_home() {
        let rift = setup_plan(LegId::Rift, Path::new("rift"), Path::new("/tmp/repo"));
        assert_eq!(rift.args, vec!["init".to_owned(), "--no-recall".to_owned()]);
        let reference = setup_plan(
            LegId::Reference,
            Path::new("reference"),
            Path::new("/tmp/repo"),
        );
        assert!(reference.args.contains(&"--telemetry=false".to_owned()));
        assert!(reference.args.contains(&"--skip-push-sessions".to_owned()));
        // Non-interactive or the benchmark hangs on a configuration prompt.
        assert!(reference.args.contains(&"--agent".to_owned()));
    }

    #[test]
    fn leg_env_keeps_every_leg_inside_the_sandbox() {
        let sandbox = Path::new("/tmp/sandbox-42");
        for leg in LegId::all() {
            let env = leg_env(leg, sandbox);
            let home = env
                .iter()
                .find(|(k, _)| k == "HOME")
                .map(|(_, v)| v.clone())
                .unwrap();
            assert!(home.starts_with("/tmp/sandbox-42"), "{home}");
            assert!(env.iter().all(|(_, v)| !v.is_empty()));
        }
        let rift = leg_env(LegId::Rift, sandbox);
        assert!(rift
            .iter()
            .any(|(k, v)| k == "RIFT_NO_KEYCHAIN" && v == "1"));
        assert!(rift
            .iter()
            .any(|(k, v)| k == "RIFT_NO_PROVISION" && v == "1"));
    }

    /// The answer probe asks the iteration it is handed about ITS file, and
    /// refuses a `why` that resolved no checkpoint.
    #[test]
    fn a_why_that_resolved_no_checkpoint_is_a_refusal() {
        let c = probe(LegId::Rift);
        let Plan::Run(inv) = plan(ScenarioId::BlameWhy, &c) else {
            panic!("wired")
        };
        let probe_inv = answer_probe(ScenarioId::BlameWhy, &c).expect("the row answers");
        assert_eq!(probe_inv.args, inv.args);
        // The driver hands the probe the last measured iteration, never the
        // one past the measured region (whose file no prelude wrote — the
        // first proof run failed its own probe on exactly that).
        let later = PlanCtx {
            iteration: 7,
            ..probe(LegId::Rift)
        };
        assert_eq!(
            answer_probe(ScenarioId::BlameWhy, &later).unwrap().args[2],
            "bench/why_7.txt:1"
        );
        assert_eq!(
            parse_answer(
                ScenarioId::BlameWhy,
                r#"{"selection":{"checkpoint_id":"abc123","resolved":true}}"#
            ),
            Some(Ok(()))
        );
        let refused = parse_answer(ScenarioId::BlameWhy, r#"{"selection":null}"#)
            .unwrap()
            .unwrap_err();
        assert!(refused.contains("resolved no checkpoint"), "{refused}");
        assert!(parse_answer(ScenarioId::BlameWhy, "nope").unwrap().is_err());
    }

    /// The answer probe re-asks the timed verb's own question and refuses a
    /// resume that restored nothing.
    #[test]
    fn a_resume_that_restored_nothing_is_a_refusal() {
        let c = probe(LegId::Rift);
        let Plan::Run(inv) = plan(ScenarioId::ResumeRestore, &c) else {
            panic!("wired")
        };
        let probe_inv = answer_probe(ScenarioId::ResumeRestore, &c).expect("the row answers");
        assert_eq!(probe_inv.args, inv.args);
        assert!(answer_probe(ScenarioId::ResumeRestore, &probe(LegId::Reference)).is_none());
        assert_eq!(
            parse_answer(
                ScenarioId::ResumeRestore,
                r#"{"sessions":[{"restored":"restored"}]}"#
            ),
            Some(Ok(()))
        );
        let refused = parse_answer(
            ScenarioId::ResumeRestore,
            r#"{"sessions":[{"restored":"no-archived-transcript"}]}"#,
        )
        .unwrap()
        .unwrap_err();
        assert!(refused.contains("no-archived-transcript"), "{refused}");
        assert!(parse_answer(ScenarioId::ResumeRestore, "not json")
            .unwrap()
            .is_err());
        assert!(
            parse_answer(ScenarioId::ResumeRestore, r#"{"sessions":[]}"#)
                .unwrap()
                .is_err()
        );
        assert!(parse_answer(ScenarioId::ColdStatus, "{}").is_none());
    }
}
