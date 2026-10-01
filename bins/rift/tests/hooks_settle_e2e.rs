//! End-to-end: the detached worker's half of a turn — the Stop returns before
//! the transcript settles and the next hook records the spend, a session end
//! condenses after the hook is gone, doctor drains a killed worker's job, and a
//! read's token count lands at the read's own row.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_hook, agent_read_hook, capture_queue_leg, checkpoint_shards, doctor_fix_json,
    doctor_json, git_in, rev_parse, run_rift_capture, run_rift_in, run_rift_stdout,
    seed_live_session_repo, session_phase, sync_env, DetachedHooks,
};

// --- the deferred settle -----------------------------------------------------

/// A native Claude Code turn as the hooks deliver it, against the real binary:
/// SessionStart (naming the transcript), UserPromptSubmit, PreToolUse, Stop.
/// Returns the transcript path. The transcript does NOT exist yet when the
/// Stop runs — the caller writes it afterwards, which is the contract under
/// test: a Stop cannot have read what was not there.
///
/// Every hook runs through `hooks`, so a caller on the detached path can wait
/// for the workers the turn's own hooks started as well as its last one's.
fn drive_native_turn(
    hooks: &DetachedHooks,
    repo: &Path,
    session: &str,
    env: &[(&str, &str)],
) -> std::path::PathBuf {
    let transcript = repo.join("..").join(format!("{session}.jsonl"));
    let hook = |verb: &str, body: serde_json::Value| {
        let mut body = body;
        body["session_id"] = serde_json::Value::String(session.to_owned());
        body["hook_event_name"] = serde_json::Value::String(verb.to_owned());
        let args = ["hooks", "claude-code", verb];
        let (code, _) = hooks.run(repo, &args, &body.to_string(), env);
        assert_eq!(code, 0, "a capture hook never fails the agent");
    };
    hook(
        "SessionStart",
        serde_json::json!({"source": "startup", "transcript_path": transcript.display().to_string()}),
    );
    hook(
        "UserPromptSubmit",
        serde_json::json!({"prompt": "add the parser"}),
    );
    hook(
        "PreToolUse",
        serde_json::json!({"tool_name": "Bash", "tool_input": {"command": "ls"}}),
    );
    assert!(
        !transcript.exists(),
        "the transcript is written only after the Stop has returned"
    );
    hook("Stop", serde_json::json!({"stop_hook_active": false}));
    transcript
}

/// The one-turn transcript the deferred settle reads: a prompt, a sidechain
/// call (real spend, folded in) and a main-chain call. 900 + 20 uncached in.
fn write_native_transcript(path: &Path) {
    let lines = [
        serde_json::json!({"type":"user","message":{"content":"add the parser"}}),
        serde_json::json!({"type":"assistant","isSidechain":true,"message":{"model":"claude-fable-5","usage":{"input_tokens":900,"output_tokens":90}}}),
        serde_json::json!({"type":"assistant","isSidechain":false,"message":{"model":"claude-fable-5","usage":{"input_tokens":20,"output_tokens":2,"cache_read_input_tokens":30}}}),
    ]
    .into_iter()
    .map(|v| v.to_string())
    .collect::<Vec<_>>()
    .join("\n")
        + "\n";
    std::fs::write(path, lines).unwrap();
}

/// `(call_id, uncached_input, cached_input)` rows of `token_usage`, plus the
/// captured `turn_end` event ordinal of turn 1 — what the spend must anchor on.
async fn settled_usage(
    cache: &Path,
    repo: &Path,
    session: &str,
) -> (Vec<(String, i64, i64)>, Option<u32>) {
    let repo_id = rift_git::repo_id_for(repo).unwrap();
    let store =
        rift_store::open_capture_projection(rift_store::projection_db_under(cache, &repo_id))
            .unwrap();
    let rows = rift_store::CanonicalStore::query(
        &store,
        "SELECT call_id, uncached_input, cached_input FROM token_usage ORDER BY call_id",
        &[],
    )
    .await
    .unwrap();
    let usage = rows
        .iter()
        .map(|r| {
            (
                r.get_text(0).unwrap(),
                r.get_i64(1).unwrap(),
                r.get_i64(2).unwrap(),
            )
        })
        .collect();
    let stop = rift_store::CanonicalStore::query(
        &store,
        "SELECT event_ordinal FROM captured_event WHERE session_id = ? AND turn_ordinal = 1 \
         AND kind = 'turn_end'",
        &[rift_store::Value::text(session.to_owned())],
    )
    .await
    .unwrap();
    let stop = stop
        .first()
        .map(|r| u32::try_from(r.get_i64(0).unwrap()).unwrap());
    (usage, stop)
}

/// The `.usage` sidecar the hook path owns: `(byte_offset, settled_turn)`.
fn usage_sidecar(repo: &Path, session: &str) -> (u64, u32) {
    let raw = std::fs::read_to_string(
        repo.join(".git")
            .join("rift-sessions")
            .join(format!("{session}.usage")),
    )
    .expect("the hook path wrote its usage sidecar");
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    (
        v["byte_offset"].as_u64().unwrap(),
        u32::try_from(v["settled_turn"].as_u64().unwrap()).unwrap(),
    )
}

/// The Stop path plus the hook-stop budget, end to end through the real
/// binary: the Stop records no spend (the transcript it would have read does
/// not even exist when it runs), the NEXT hook for the session settles the
/// turn from the transcript offset, and the spend lands at the Stop event's
/// own ids — `model_call_id(session_row, 1, ordinal-of-turn_end)` — so every
/// `turn → model_call → token_usage` reader sees what the inline settle used
/// to write, one hook later. Subagent spend is folded in (900 + 20).
#[tokio::test]
async fn a_stop_returns_before_the_transcript_settles_and_the_next_hook_records_the_turn() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let session = "01JDEFERREDSETTLE0000000001";
    let transcript = drive_native_turn(&DetachedHooks::default(), &repo, session, &env);
    write_native_transcript(&transcript);

    let (usage, stop) = settled_usage(&cache, &repo, session).await;
    assert!(
        usage.is_empty(),
        "the Stop itself recorded no spend: {usage:?}"
    );
    let stop = stop.expect("the Stop was captured as turn 1's turn_end");
    let (offset, settled) = usage_sidecar(&repo, session);
    assert_eq!(
        (offset, settled),
        (0, 0),
        "seeded at the (absent) transcript's length, nothing settled"
    );

    // The next hook — turn 2's prompt — settles turn 1.
    let body = serde_json::json!({
        "session_id": session, "hook_event_name": "UserPromptSubmit", "prompt": "and the tests"
    });
    assert_eq!(
        run_rift_in(
            &repo,
            &["hooks", "claude-code", "UserPromptSubmit"],
            &body.to_string(),
            &env
        ),
        0
    );
    let (usage, _) = settled_usage(&cache, &repo, session).await;
    let session_row = rift_shadow::ids::session_row_id("claude-code", session);
    assert_eq!(
        usage,
        vec![(
            rift_shadow::ids::model_call_id(&session_row, 1, stop),
            920,
            30
        )],
        "turn 1's spend at the Stop event's own ids, subagent spend folded in"
    );
    let len = std::fs::metadata(&transcript).unwrap().len();
    assert_eq!(usage_sidecar(&repo, session), (len, 1));
}

/// The backstop for a session's LAST turn, which no later hook will settle:
/// the doctor sweep (`rift doctor --fix`) settles it under `SettleMode::Final`
/// before condensing — same ids, same rows — so a crashed or simply abandoned
/// session does not lose its final turn's spend.
#[tokio::test]
async fn the_doctor_sweep_settles_the_last_turn_of_a_session_no_hook_will_end() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    // The seeded live session is not this test's; the sweep must see one.
    std::fs::remove_file(
        repo.join(".git")
            .join("rift-sessions")
            .join("e2e-live-1.json"),
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let session = "01JABANDONEDLASTTURN0000001";
    let transcript = drive_native_turn(&DetachedHooks::default(), &repo, session, &env);
    write_native_transcript(&transcript);
    let (usage, stop) = settled_usage(&cache, &repo, session).await;
    assert!(usage.is_empty());
    let stop = stop.expect("turn 1 closed");

    // The agent is gone: back-date the session's last touch past the idle
    // window so the sweep finalizes it. Only the stamp changes.
    let cache_file = repo
        .join(".git")
        .join("rift-sessions")
        .join(format!("{session}.json"));
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cache_file).unwrap()).unwrap();
    v["updated_at_ms"] = serde_json::json!(0);
    std::fs::write(&cache_file, v.to_string()).unwrap();

    let (code, stdout) = run_rift_stdout(&repo, &["--json", "doctor", "--fix"], &env);
    assert_eq!(code, 0, "doctor reports, it never fails: {stdout}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let sessions = doc
        .pointer("/sessions")
        .or_else(|| doc.pointer("/data/sessions"))
        .expect("doctor json carries a sessions leg");
    assert_eq!(
        sessions["finalized"],
        serde_json::json!([session]),
        "{sessions}"
    );

    let (usage, _) = settled_usage(&cache, &repo, session).await;
    let session_row = rift_shadow::ids::session_row_id("claude-code", session);
    assert_eq!(
        usage,
        vec![(
            rift_shadow::ids::model_call_id(&session_row, 1, stop),
            920,
            30
        )],
        "the sweep settled the last turn at its Stop event's ids"
    );
    let len = std::fs::metadata(&transcript).unwrap().len();
    assert_eq!(usage_sidecar(&repo, session), (len, 1));
}

/// The agent-hook journal's marker files, keyed by session: the worker's
/// `condensed-<sha256(session)>` done-marker, and whether any job is still
/// queued.
fn agent_queue_state(repo: &Path, session: &str) -> (bool, usize) {
    let dir = repo.join(".git").join("rift-agent-queue");
    let marker = dir.join(format!(
        "condensed-{}",
        rift_shadow::sha256_hex(&[session.as_bytes()])
    ));
    let queued = std::fs::read_dir(&dir).map_or(0, |d| {
        d.flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .count()
    });
    (marker.exists(), queued)
}

/// A hand-written session-end condense job, exactly as the hook journals it —
/// what a worker killed before it ran leaves behind.
fn plant_condense_job(repo: &Path, session: &str) {
    let dir = repo.join(".git").join("rift-agent-queue");
    std::fs::create_dir_all(&dir).unwrap();
    let repo_id = rift_git::repo_id_for(repo).unwrap().to_string();
    let job = serde_json::json!({
        "schema_version": 1,
        "work_tree": repo.display().to_string(),
        "queued_at_ms": 1_783_000_000_000_i64,
        "attempts": 0,
        "kind": {"kind": "condense", "session_id": session, "agent": "claude-code", "repo_id": repo_id},
    });
    std::fs::write(
        dir.join(format!(
            "condense-{}.json",
            rift_shadow::sha256_hex(&[session.as_bytes()])
        )),
        job.to_string(),
    )
    .unwrap();
}

/// The DEFAULT path — no `RIFT_CAPTURE_SYNC`: a
/// `SessionEnd` journals its condense and returns; the detached worker
/// settles the session's last turn, archives, closes the session and leaves
/// the done-marker, all after the hook is gone. The test waits for the
/// workers to exit rather than for a duration, for the reason the
/// post-commit twin
/// (`the_detached_worker_archives_after_the_hook_has_already_returned`) gives.
#[tokio::test]
async fn a_session_end_returns_and_the_detached_worker_condenses_and_settles() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::remove_file(
        repo.join(".git")
            .join("rift-sessions")
            .join("e2e-live-1.json"),
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];
    let session = "01JDETACHEDSESSIONEND000001";
    let hooks = DetachedHooks::default();
    let transcript = drive_native_turn(&hooks, &repo, session, &env);
    write_native_transcript(&transcript);
    let (usage, stop) = settled_usage(&cache, &repo, session).await;
    assert!(
        usage.is_empty(),
        "nothing settled before the end: {usage:?}"
    );
    let stop = stop.expect("turn 1 closed");

    let body = serde_json::json!({"session_id": session, "hook_event_name": "SessionEnd", "reason": "exit"});
    let args = ["hooks", "claude-code", "SessionEnd"];
    let (code, _) = hooks.run(&repo, &args, &body.to_string(), &env);
    assert_eq!(code, 0, "a capture hook never fails the agent");
    assert!(
        repo.join(".git").join("rift-agent-queue").is_dir(),
        "the hook left a durable record of the condense it owes"
    );

    hooks.await_workers(|| agent_queue_state(&repo, session) == (true, 0));
    assert_eq!(
        agent_queue_state(&repo, session),
        (true, 0),
        "the worker left its done-marker and drained the job"
    );
    assert_eq!(session_phase(&repo, session), "ended");
    assert!(
        !checkpoint_shards(&repo).is_empty(),
        "the session closed on a checkpoint that exists"
    );
    let (usage, _) = settled_usage(&cache, &repo, session).await;
    let session_row = rift_shadow::ids::session_row_id("claude-code", session);
    assert_eq!(
        usage,
        vec![(
            rift_shadow::ids::model_call_id(&session_row, 1, stop),
            920,
            30
        )],
        "the last turn's spend, settled at its Stop event's ids"
    );
}

/// A worker that died before it ran leaves its job in the journal, and the
/// session it was closing was touched seconds ago — inside the idle window
/// the crashed-session sweep keys on. `rift doctor --fix` drains the journal
/// first, so the rescue does not wait a day.
#[test]
fn doctor_drains_a_killed_workers_session_end_job() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    // The hook's own half ran: the machine is in `ending`, touched just now.
    let cache_file = repo
        .join(".git")
        .join("rift-sessions")
        .join("e2e-live-1.json");
    let mut v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&cache_file).unwrap()).unwrap();
    v["state"] = serde_json::json!("ending");
    v["updated_at_ms"] = serde_json::json!(i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
    )
    .unwrap());
    std::fs::write(&cache_file, v.to_string()).unwrap();
    plant_condense_job(&repo, "e2e-live-1");

    // Bare doctor reports the job and touches nothing.
    let (code, stdout) = run_rift_stdout(&repo, &["--json", "doctor"], &env);
    assert_eq!(code, 0, "{stdout}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let queue = doc
        .pointer("/capture_queue")
        .or_else(|| doc.pointer("/data/capture_queue"))
        .expect("doctor json carries the capture-queue leg");
    assert_eq!(queue["agent_jobs"]["pending"], 1, "{queue}");
    assert_eq!(agent_queue_state(&repo, "e2e-live-1"), (false, 1));
    assert!(checkpoint_shards(&repo).is_empty());

    let (code, stdout) = run_rift_stdout(&repo, &["--json", "doctor", "--fix"], &env);
    assert_eq!(code, 0, "{stdout}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let sessions = doc
        .pointer("/sessions")
        .or_else(|| doc.pointer("/data/sessions"))
        .expect("doctor json carries a sessions leg");
    assert_eq!(
        sessions["finalized"],
        serde_json::json!(["e2e-live-1"]),
        "the rescued condense is witnessed on the sweep: {sessions}"
    );
    assert_eq!(agent_queue_state(&repo, "e2e-live-1"), (true, 0));
    assert_eq!(session_phase(&repo, "e2e-live-1"), "ended");
    assert_eq!(checkpoint_shards(&repo).len(), 1);
}

/// A second `SessionEnd` for the same session — the agent redelivering, or a
/// job found again after the work landed — condenses nothing twice: an end
/// redelivered to an ended session asks the machine nothing, and the
/// journal's done-marker refuses even a job planted by hand.
#[test]
fn a_redelivered_session_end_never_condenses_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    for _ in 0..2 {
        assert_eq!(
            run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env),
            0
        );
    }
    assert_eq!(session_phase(&repo, "e2e-live-1"), "ended");
    assert_eq!(checkpoint_shards(&repo).len(), 1, "one condense, not two");
    assert_eq!(agent_queue_state(&repo, "e2e-live-1"), (true, 0));

    plant_condense_job(&repo, "e2e-live-1");
    let (code, stdout) = run_rift_stdout(&repo, &["--json", "doctor", "--fix"], &env);
    assert_eq!(code, 0, "{stdout}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let sessions = doc
        .pointer("/sessions")
        .or_else(|| doc.pointer("/data/sessions"))
        .unwrap();
    assert_eq!(sessions["finalized"], serde_json::json!([]), "{sessions}");
    assert_eq!(checkpoint_shards(&repo).len(), 1, "the marker refused it");
    assert_eq!(agent_queue_state(&repo, "e2e-live-1"), (true, 0));
}

/// `(input_tokens, tokenizer)` rows of `file_read_tokens` for one read call.
async fn read_token_rows(cache: &Path, repo: &Path, call: &str) -> Vec<(i64, String)> {
    let repo_id = rift_git::repo_id_for(repo).unwrap();
    let store =
        rift_store::open_capture_projection(rift_store::projection_db_under(cache, &repo_id))
            .unwrap();
    rift_store::CanonicalStore::query(
        &store,
        "SELECT input_tokens, tokenizer FROM file_read_tokens WHERE tool_call_id = ?",
        &[rift_store::Value::text(call.to_owned())],
    )
    .await
    .unwrap()
    .iter()
    .map(|r| (r.get_i64(0).unwrap(), r.get_text(1).unwrap()))
    .collect()
}

/// The read path, end to end: the hook records the read's
/// identity and journals its BPE count; the worker appends the count at the
/// read's own `tool_call` row (`file_read_tokens`, insert-or-ignore); a
/// redelivered read writes no second row; and the archive that follows
/// carries the figure in the shard, so every downstream reader (Canopy, the
/// recall journal, cost) sees it as if it had been measured inline.
#[tokio::test]
async fn a_reads_token_count_lands_from_the_worker_at_the_reads_own_row() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env: Vec<(&str, &str)> = vec![
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let session_row = rift_shadow::ids::session_row_id("claude-code", "e2e-live-1");
    let call = rift_shadow::ids::read_call_row_id(
        &session_row,
        1,
        "a.txt",
        "2c8b08da5ce60398e1f19af0e5dccc744df274b826abe585eaba68c525434806",
    );
    let expected = i64::from(rift_tokens::count("one\n"));
    assert_eq!(rift_tokens::provenance(), rift_tokens::TOKENIZER);

    for _ in 0..2 {
        agent_read_hook(&repo, "e2e-live-1", 1, &repo.join("a.txt"), &env);
        assert_eq!(
            read_token_rows(&cache, &repo, &call).await,
            vec![(expected, rift_tokens::TOKENIZER.to_owned())],
            "one row, from the worker, at the read's own id — redelivered or not"
        );
    }
    assert_eq!(agent_queue_state(&repo, "e2e-live-1").1, 0, "drained");

    let (code, stderr) = run_rift_capture(
        &repo,
        &[
            "hooks",
            "git",
            "post-commit",
            "--git-dir",
            repo.join(".git").to_str().unwrap(),
        ],
        Some(""),
        &env,
    );
    assert_eq!(code, 0, "{stderr}");
    let names = git_in(
        &repo,
        &[
            "ls-tree",
            "-r",
            "--name-only",
            "refs/heads/rift/checkpoints/v1",
        ],
    );
    let shard_path = names
        .lines()
        .find(|l| l.ends_with("tool_calls.jsonl"))
        .expect("the shard carries the read facts");
    let jsonl = git_in(
        &repo,
        &[
            "cat-file",
            "-p",
            &format!("refs/heads/rift/checkpoints/v1:{shard_path}"),
        ],
    );
    assert!(
        jsonl.contains(&format!(r#""input_tokens":{expected}"#)),
        "the shard carries the late count: {jsonl}"
    );
}

/// A count is a count of the bytes the agent READ. When the working file has
/// moved on by the time the worker runs, the bytes are taken from the git
/// blob the fact named — and when neither source still has them, nothing is
/// counted under a guessed identity: the job is kept and said once.
#[tokio::test]
async fn the_worker_counts_the_bytes_the_fact_named_or_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    // The projection must exist for the worker to write into; a PreToolUse
    // on the session opens it without recording a read.
    agent_hook(
        &repo,
        "PreToolUse",
        "e2e-live-1",
        1,
        r#"{"tool_name":"Bash","tool_input":{"command":"ls"}}"#,
        &env,
    );
    let blob_oid = rev_parse(&repo, "HEAD:a.txt");
    // The agent read "one\n"; the file now says something else.
    std::fs::write(repo.join("a.txt"), "two\n").unwrap();
    let from_blob = "a".repeat(64);
    plant_read_tokens_job(
        &repo,
        &from_blob,
        "2c8b08da5ce60398e1f19af0e5dccc744df274b826abe585eaba68c525434806",
        &blob_oid,
    );
    let vanished = "b".repeat(64);
    plant_read_tokens_job(&repo, &vanished, &"f".repeat(64), &"0".repeat(40));

    let doc = doctor_fix_json(&repo, &env);
    assert_eq!(
        read_token_rows(&cache, &repo, &from_blob).await,
        vec![(
            i64::from(rift_tokens::count("one\n")),
            rift_tokens::TOKENIZER.to_owned()
        )],
        "counted from the blob the fact named, not the file that moved on"
    );
    assert!(
        read_token_rows(&cache, &repo, &vanished).await.is_empty(),
        "bytes nobody can produce are not counted under a guessed identity"
    );
    // The `--fix` run's own report: the sweep drained first, so its queue leg
    // sees the kept job and the note (which doctor's soak leg then says and
    // consumes, exactly once).
    let queue = capture_queue_leg(&doc);
    assert_eq!(
        queue["agent_jobs"]["pending"], 1,
        "kept for a retry: {queue}"
    );
    assert!(
        queue["notes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n.as_str().unwrap().contains("no longer available")),
        "and said once: {queue}"
    );
    let doc = doctor_json(&repo, &env);
    let queue = capture_queue_leg(&doc);
    assert_eq!(queue["agent_jobs"]["pending"], 1, "still owed: {queue}");
    assert_eq!(
        queue["notes"],
        serde_json::json!([]),
        "said once, not twice"
    );
}

/// A read-tokens job in the agent queue, exactly as the hook journals one:
/// the read `call` over `a.txt` at `sha`, whose bytes the git blob `oid` holds.
fn plant_read_tokens_job(repo: &Path, call: &str, sha: &str, oid: &str) {
    let dir = repo.join(".git").join("rift-agent-queue");
    std::fs::create_dir_all(&dir).unwrap();
    let job = serde_json::json!({
        "schema_version": 1,
        "work_tree": repo.display().to_string(),
        "queued_at_ms": 1_783_000_000_000_i64,
        "attempts": 0,
        "kind": {"kind": "read-tokens", "tool_call_id": call, "rel_path": "a.txt",
                 "content_sha256": sha, "blob_oid": oid, "schema_version": 1},
    });
    std::fs::write(dir.join(format!("tokens-{call}.json")), job.to_string()).unwrap();
}
