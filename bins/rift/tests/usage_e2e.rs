//! Process-level proof that settled usage is frozen into the checkpoint
//! shard: a native turn settles its spend through the real binary, the next
//! commit's shard carries `usage.jsonl` built from the projection rows, the
//! trailer `prepare-commit-msg` writes names that very shard with and without
//! usage, and `rift verify` reads the leaf back.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::{Path, PathBuf};

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;

use hooks_fixture::{
    checkpoint_shards, family_projection_db, git_in, hook_pair_commit, only_checkpoint_id,
    run_rift_in, run_rift_stdout, seed_live_session_repo, shard_blob, shard_tree_names, sync_env,
    LIVE_SESSION,
};

/// One hook of the live session, as Claude Code delivers it.
fn hook(repo: &Path, verb: &str, body: serde_json::Value, env: &[(&str, &str)]) {
    let mut body = body;
    body["session_id"] = serde_json::Value::String(LIVE_SESSION.to_owned());
    body["hook_event_name"] = serde_json::Value::String(verb.to_owned());
    let code = run_rift_in(
        repo,
        &["hooks", "claude-code", verb],
        &body.to_string(),
        env,
    );
    assert_eq!(code, 0, "a capture hook never fails the agent");
}

/// One turn of the live session whose spend the next hook settles: the turn
/// runs, its transcript lands after the Stop, and the next prompt settles it.
/// Returns the transcript's path, which the session cache now names.
fn settle_one_turn(repo: &Path, tmp: &Path, env: &[(&str, &str)]) -> PathBuf {
    let transcript = tmp.join(format!("{LIVE_SESSION}.jsonl"));
    let path = transcript.display().to_string();
    hook(
        repo,
        "SessionStart",
        serde_json::json!({"source": "startup", "transcript_path": path}),
        env,
    );
    hook(
        repo,
        "UserPromptSubmit",
        serde_json::json!({"prompt": "add the parser"}),
        env,
    );
    hook(
        repo,
        "Stop",
        serde_json::json!({"stop_hook_active": false}),
        env,
    );
    let lines = [
        serde_json::json!({"type":"user","message":{"content":"add the parser"}}),
        serde_json::json!({"type":"assistant","message":{"model":"claude-fable-5","usage":{"input_tokens":20,"output_tokens":2,"cache_read_input_tokens":30}}}),
    ];
    let body = lines.map(|l| l.to_string()).join("\n") + "\n";
    std::fs::write(&transcript, body).unwrap();
    hook(
        repo,
        "UserPromptSubmit",
        serde_json::json!({"prompt": "and the tests"}),
        env,
    );
    transcript
}

/// Remove the settled turn's transcript before the commit. The trailer's
/// prediction never reads a transcript and the archive binds one into the id
/// (see the transcript-carrying test below), so a test of the usage leaf's
/// prediction commits a session whose transcript is gone.
fn drop_transcript(transcript: &Path) {
    std::fs::remove_file(transcript).unwrap();
}

/// The `(call_id, model)` of every settled call in the family projection.
fn settled_calls(repo: &Path, cache: &Path) -> Vec<(String, String)> {
    use rift_store::CanonicalStore as _;

    let store = rift_store::open_capture_projection(family_projection_db(repo, cache)).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let rows = rt
        .block_on(store.query(
            "SELECT mc.call_id, mc.model FROM model_call mc \
             JOIN token_usage tu ON tu.call_id = mc.call_id ORDER BY mc.call_id",
            &[],
        ))
        .unwrap();
    rows.iter()
        .map(|r| (r.get_text(0).unwrap(), r.get_text(1).unwrap()))
        .collect()
}

/// The shard directory a checkpoint id names.
fn shard_dir(checkpoint: &str) -> String {
    format!("{}/{}", &checkpoint[..2], &checkpoint[2..])
}

/// ★ **The riskiest line: one id, predicted and committed.** The trailer
/// names the checkpoint the commit archives for a commit whose session has
/// settled no call, and again once a turn has settled, whose call the shard
/// then freezes into `usage.jsonl` under the projection's own key; `rift
/// verify` reads the leaf back.
#[test]
fn the_trailer_names_the_checkpoint_the_commit_archives_with_and_without_usage() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    std::fs::write(repo.join("w1.txt"), "one\n").unwrap();
    git_in(&repo, &["add", "w1.txt"]);
    let bare = hook_pair_commit(&repo, tmp.path(), "bare", &env);
    assert!(
        checkpoint_shards(&repo).contains(&shard_dir(&bare)),
        "no shard at the trailer's id {bare}"
    );
    let names = shard_tree_names(&repo);
    assert!(!names.contains("usage.jsonl"), "nothing settled: {names}");

    let transcript = settle_one_turn(&repo, tmp.path(), &env);
    drop_transcript(&transcript);
    let calls = settled_calls(&repo, &cache);
    assert_eq!(calls.len(), 1, "one settled call: {calls:?}");

    std::fs::write(repo.join("w2.txt"), "two\n").unwrap();
    git_in(&repo, &["add", "w2.txt"]);
    let settled = hook_pair_commit(&repo, tmp.path(), "settled", &env);
    assert!(
        checkpoint_shards(&repo).contains(&shard_dir(&settled)),
        "no shard at the trailer's id {settled}"
    );
    let frozen = shard_blob(&repo, &format!("{}/usage.jsonl", shard_dir(&settled)));
    let lines: Vec<serde_json::Value> = frozen
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{frozen}");
    let line = &lines[0];
    assert_eq!(line["call_id"], calls[0].0.as_str(), "the row's own key");
    assert_eq!(line["model"], "claude-fable-5");
    assert_eq!(line["session"], LIVE_SESSION);
    assert_eq!(
        (
            &line["uncached_input"],
            &line["cached_input"],
            &line["output"]
        ),
        (&20.into(), &30.into(), &2.into())
    );

    let (code, out) = run_rift_stdout(&repo, &["verify"], &env);
    assert_eq!(code, 0, "verify reads the usage leaf back: {out}");
}

/// A second commit with nothing settled since folds the same usage and so
/// re-derives the checkpoint the first one minted: the artifact is
/// cumulative per session, not the delta since the last checkpoint.
#[test]
fn a_commit_with_nothing_settled_since_names_the_same_checkpoint() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    let transcript = settle_one_turn(&repo, tmp.path(), &env);
    drop_transcript(&transcript);
    std::fs::write(repo.join("w1.txt"), "one\n").unwrap();
    git_in(&repo, &["add", "w1.txt"]);
    let first = hook_pair_commit(&repo, tmp.path(), "first", &env);
    std::fs::write(repo.join("w2.txt"), "two\n").unwrap();
    git_in(&repo, &["add", "w2.txt"]);
    let second = hook_pair_commit(&repo, tmp.path(), "second", &env);
    assert_eq!(second, first, "unchanged capture, one checkpoint");
    assert_eq!(checkpoint_shards(&repo), vec![shard_dir(&first)]);
}

/// **What the trailer claims for a session that carries its transcript.** The
/// commit's archive binds the transcript pair into the checkpoint id and the
/// trailer's prediction never reads a transcript, so the two ids differ. The
/// same holds at the commit before `usage.jsonl` existed, with the same
/// session and no usage frozen: the mismatch belongs to the transcript, not to
/// the usage leaf, which this commit's shard carries all the same.
#[test]
fn a_transcript_carrying_commit_archives_under_an_id_its_trailer_did_not_predict() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);

    settle_one_turn(&repo, tmp.path(), &env);
    std::fs::write(repo.join("w1.txt"), "one\n").unwrap();
    git_in(&repo, &["add", "w1.txt"]);
    let predicted = hook_pair_commit(&repo, tmp.path(), "carried", &env);
    let committed = only_checkpoint_id(&repo);
    let names = shard_tree_names(&repo);
    let dir = shard_dir(&committed);
    assert!(
        names.contains(&format!("{dir}/transcript.jsonl")),
        "{names}"
    );
    assert!(names.contains(&format!("{dir}/usage.jsonl")), "{names}");
    // fixme: the trailer names a checkpoint no shard carries whenever the
    // session's transcript reaches the archive, because the prediction does
    // not read the transcript the archive binds into the id; the read side
    // resolves such a commit through the shard's own commit anchor instead.
    assert_ne!(
        predicted, committed,
        "today the trailer misses by the transcript"
    );
}
