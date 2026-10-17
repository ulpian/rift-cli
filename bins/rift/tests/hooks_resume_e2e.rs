//! End-to-end: `rift session resume` — the round trip with the fake agent,
//! the typed refusals, the fetch ladder over a path remote and a
//! filter-capable one, and the landed checkpoint a trailer only predicted.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    archive_head, checkpoint_shards, clone_of, commit_carrying_trailer, current_branch, git_in,
    has_local_checkpoints_ref, json_str, now_ms, only_checkpoint_id, query_env, rev_parse,
    rift_json, run_rift, run_rift_stdout, seed_live_session_repo, seed_session_with_transcript,
    shard_blob_bytes, sync_env, sync_env_with_data, window_key, write_agent_transcript,
    LIVE_SESSION, TWO_LINE_TRANSCRIPT,
};

/// A repository archived once with the fake agent's transcript-carrying
/// session, then given the commit that carries the trailer: the state a
/// resume is asked to restore from.
struct Resumable {
    repo: PathBuf,
    cache: PathBuf,
    data: PathBuf,
    transcript: PathBuf,
    checkpoint_id: String,
    branch: String,
    /// The shard's stored redacted transcript — the byte truth resume serves.
    stored: Vec<u8>,
}

impl Resumable {
    /// Capture → commit → archive → the trailer commit, through the real hooks.
    fn archive(tmp: &Path) -> Self {
        let repo = seed_live_session_repo(tmp);
        let transcript = write_agent_transcript(tmp, LIVE_SESSION, TWO_LINE_TRANSCRIPT);
        seed_session_with_transcript(&repo, LIVE_SESSION, &transcript);
        let cache = tmp.join("cache");
        let data = tmp.join("data");
        archive_head(&repo, &sync_env_with_data(&cache, &data));
        let shard = checkpoint_shards(&repo);
        let checkpoint_id = shard.first().expect("one archived shard").replace('/', "");
        let stored = shard_blob_bytes(&repo, &format!("{}/transcript.jsonl", shard[0]));
        assert!(!stored.is_empty(), "the shard archived the transcript");
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        commit_carrying_trailer(&repo, &checkpoint_id);
        let branch = current_branch(&repo);
        Self {
            repo,
            cache,
            data,
            transcript,
            checkpoint_id,
            branch,
            stored,
        }
    }

    fn env(&self) -> [(&'static str, &str); 5] {
        sync_env_with_data(&self.cache, &self.data)
    }

    /// `rift session resume <args> --json`, non-interactive: the exit code and
    /// the document, which a refusal answers too.
    fn resume(&self, extra: &[&str]) -> (i32, serde_json::Value) {
        let mut args = vec!["session", "resume"];
        args.extend_from_slice(extra);
        args.push("--json");
        let (code, out) = run_rift_stdout(&self.repo, &args, &self.env());
        let doc = serde_json::from_str(&out)
            .unwrap_or_else(|e| panic!("resume answered no json: {e}: {out}"));
        (code, doc)
    }
}

/// The resume exit shape, end to end with the fake agent's session fixture:
/// capture → commit → **wipe the native log** → resume →
/// the transcript is back at the agent's own address with the shard's stored
/// redacted bytes, the printed command names the session, and the recall
/// known-state cursor is seeded so the first injected manifest reflects the
/// resumed session.
#[test]
fn resume_round_trips_with_the_fake_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Resumable::archive(tmp.path());

    // WIPE the native log — the resume must restore it, not find it.
    std::fs::remove_file(&fx.transcript).unwrap();

    let (code, v) = fx.resume(&[&fx.branch]);
    assert_eq!(code, 0, "resume: {v}");
    assert_eq!(v["checkpoint_id"], fx.checkpoint_id, "{v}");
    assert_eq!(v["newer_commits"], 0, "the trailer commit is the tip: {v}");
    assert_eq!(v["sessions"][0]["session_id"], "e2e-live-1", "{v}");
    assert_eq!(v["sessions"][0]["agent"], "claude-code", "{v}");
    assert_eq!(v["sessions"][0]["restored"], "restored", "{v}");
    assert_eq!(
        v["sessions"][0]["transcript_path"],
        fx.transcript.display().to_string(),
        "restored to the journal-recorded native address: {v}"
    );
    assert_eq!(
        v["sessions"][0]["resume_command"], "claude -r e2e-live-1",
        "the printed command names the session: {v}"
    );
    assert_eq!(v["recall_cursor"]["seeded"], true, "{v}");
    // The agent's context is BACK: the native file again exists and is byte-
    // identical to the shard's stored redacted transcript.
    assert_eq!(
        std::fs::read(&fx.transcript).unwrap(),
        fx.stored,
        "the restored transcript is the shard's stored bytes"
    );
    // The recall known-state cursor: a baseline now exists for this session's
    // MAIN-THREAD window, so the next injected manifest is a delta, not a
    // bootstrap. A resume restores the conversation, and a subagent's window
    // does not outlive the turn that spawned it, so the key's agent half is
    // the empty one.
    let key = window_key("e2e-live-1", "");
    let baseline = fx
        .repo
        .join(".rift")
        .join("state")
        .join("manifest")
        .join(&key)
        .join("1.json");
    let baseline = std::fs::read_to_string(baseline).expect("resume seeded a baseline");
    assert!(
        baseline.contains("known_hashes"),
        "the cursor is a baseline document: {baseline}"
    );
}

/// A second resume finds the log the first one restored and keeps it; only
/// `--force` overwrites it from the checkpoint again. The no-arg surface lists
/// the session with its branch (non-TTY: a list, never a hanging prompt) — the
/// picker's data, Canopy-row shaped.
#[test]
fn a_second_resume_keeps_the_existing_log_unless_forced() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Resumable::archive(tmp.path());
    std::fs::remove_file(&fx.transcript).unwrap();
    let (code, v) = fx.resume(&[&fx.branch]);
    assert_eq!(code, 0, "the first resume restores: {v}");

    // Second resume without --force: the existing local log is KEPT.
    let (code, v) = fx.resume(&[&fx.branch]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        v["sessions"][0]["restored"], "kept-existing",
        "an existing local session log is never overwritten by default: {v}"
    );
    // …and with --force it is overwritten from the checkpoint again.
    let (code, v) = fx.resume(&[&fx.branch, "--force"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["sessions"][0]["restored"], "restored", "{v}");

    let (code, v) = fx.resume(&[]);
    assert_eq!(code, 0, "{v}");
    let rows = v["resumable"].as_array().unwrap();
    assert!(
        rows.iter().any(|r| r["session_id"] == "e2e-live-1"
            && r["branch"] == fx.branch.as_str()
            && r["agent"] == "claude-code"),
        "the stopped session is listed with its branch: {v}"
    );
}

/// The safety rail: newer un-checkpointed work above the checkpoint turns a
/// non-interactive resume into a typed refusal that `--force` overrides — and
/// nothing is ever reset either way.
#[test]
fn newer_uncheckpointed_work_turns_resume_into_a_refusal_that_force_overrides() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Resumable::archive(tmp.path());
    std::fs::write(fx.repo.join("c.txt"), "three\n").unwrap();
    git_in(&fx.repo, &["add", "c.txt"]);
    git_in(
        &fx.repo,
        &["commit", "-q", "-m", "newer work, no checkpoint"],
    );
    let tip_before = rev_parse(&fx.repo, "HEAD");

    let (code, v) = fx.resume(&[&fx.branch]);
    assert_eq!(code, 4, "a refusal, not a guess: {v}");
    assert_eq!(v["error"], "newer_commits", "{v}");
    assert_eq!(v["newer_commits"], 1, "{v}");

    let (code, v) = fx.resume(&[&fx.branch, "--force"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["checkpoint_id"], fx.checkpoint_id, "{v}");
    assert_eq!(v["newer_commits"], 1, "{v}");
    assert_eq!(
        rev_parse(&fx.repo, "HEAD"),
        tip_before,
        "resume never moves HEAD"
    );
}

/// Resuming a checkpointed branch from another local branch switches the work
/// tree onto it as a branch: HEAD names the branch afterwards, not a detached
/// commit, and the restore completes. A checkout argv that put the branch after
/// `--` asked git for a file of that name and failed the whole verb.
#[test]
fn resume_from_another_local_branch_checks_the_branch_out_and_stays_on_it() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Resumable::archive(tmp.path());
    git_in(&fx.repo, &["checkout", "-q", "-b", "elsewhere"]);

    let (code, v) = fx.resume(&[&fx.branch]);

    assert_eq!(code, 0, "resume switches branches: {v}");
    assert_eq!(v["checkpoint_id"], fx.checkpoint_id, "{v}");
    assert_eq!(
        git_in(&fx.repo, &["symbolic-ref", "HEAD"]).trim(),
        format!("refs/heads/{}", fx.branch),
        "HEAD is on the resumed branch, not detached"
    );
}

/// The resume refusal contracts, `--json` byte-locked (they are the NEW
/// goldens this verb adds): an unknown branch, a branch with no checkpoint,
/// and a dirty worktree each answer a typed, deterministic document.
#[test]
fn resume_refusals_are_typed_and_their_json_is_locked() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    git_in(&repo, &["checkout", "-q", "-b", "rift-e2e-branch"]);
    let cache = tmp.path().join("cache");
    common::provision_here(&repo, &cache);
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];

    let (code, out) = run_rift_stdout(
        &repo,
        &["session", "resume", "no-such-branch", "--json"],
        &env,
    );
    assert_eq!(code, 4, "{out}");
    assert_eq!(
        out, "{\n  \"branch\": \"no-such-branch\",\n  \"error\": \"unknown_branch\"\n}\n",
        "the unknown-branch document is locked"
    );

    let (code, out) = run_rift_stdout(
        &repo,
        &["session", "resume", "rift-e2e-branch", "--json"],
        &env,
    );
    assert_eq!(code, 4, "{out}");
    assert_eq!(
        out, "{\n  \"branch\": \"rift-e2e-branch\",\n  \"error\": \"no_checkpoint\"\n}\n",
        "the checkpointless-branch document is locked"
    );

    // A dirty worktree refuses a branch SWITCH (resume on the current branch
    // needs no checkout, so the refusal is reached via another branch).
    git_in(&repo, &["branch", "-q", "other-branch"]);
    std::fs::write(repo.join("a.txt"), "dirty\n").unwrap();
    let (code, out) = run_rift_stdout(
        &repo,
        &["session", "resume", "other-branch", "--json"],
        &env,
    );
    assert_eq!(code, 4, "{out}");
    assert_eq!(
        out, "{\n  \"error\": \"dirty_worktree\"\n}\n",
        "the dirty-worktree document is locked"
    );
}

/// The fetch ladder, exercised offline over a **path remote**: a clone that
/// lacks the local checkpoints ref (git clones branches as remote-tracking
/// refs) resolves the claimed shard by fetching
/// `refs/heads/rift/checkpoints/v1` from origin — the ladder's origin rung —
/// and the resume completes.
#[test]
fn resume_fetch_ladder_pulls_the_shard_from_a_path_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let transcript = write_agent_transcript(
        tmp.path(),
        LIVE_SESSION,
        concat!(r#"{"type":"user","message":"hello"}"#, "\n"),
    );
    seed_session_with_transcript(&repo, LIVE_SESSION, &transcript);
    let cache = tmp.path().join("cache");
    common::provision_here(&repo, &cache);
    let env = sync_env(&cache);
    archive_head(&repo, &env);
    let checkpoint_id = only_checkpoint_id(&repo);
    commit_carrying_trailer(&repo, &checkpoint_id);
    let branch = current_branch(&repo);

    let clone = clone_of(tmp.path(), &repo, "clone");
    assert!(
        !has_local_checkpoints_ref(&clone),
        "precondition: the clone has no local checkpoints ref"
    );
    std::fs::create_dir_all(clone.join(".rift")).unwrap();
    let clone_cache = tmp.path().join("clone-cache");
    common::provision_here(&clone, &clone_cache);
    let clone_env = query_env(&clone_cache);
    let v = rift_json(
        &clone,
        &["session", "resume", &branch, "--json"],
        &clone_env,
    );
    assert_eq!(v["checkpoint_id"], checkpoint_id, "{v}");
    // The rung's effect is durable: the local ref now exists.
    assert!(
        has_local_checkpoints_ref(&clone),
        "the origin rung fetched the checkpoints ref into the clone"
    );
    // No journal travelled with the clone, so the agent is honestly unknown
    // and the restore says where the transcript still lives.
    assert_eq!(v["sessions"][0]["session_id"], "e2e-live-1", "{v}");
    assert_eq!(v["sessions"][0]["agent"], "unknown", "{v}");
    assert_eq!(v["sessions"][0]["restored"], "no-native-path", "{v}");
}

/// Land one checkpoint on a server fixture: end every live journal session,
/// start session `res-<n>` with its own transcript, commit, run the real
/// post-commit archive, and put the landed id in a trailer-carrying commit.
/// Returns the checkpoint id. (The resume-regression fixture's generator.)
fn land_server_checkpoint(repo: &Path, tmp: &Path, n: u32, env: &[(&str, &str)]) -> String {
    let sessions = common::sessions_dir(&repo.join(".git"), &tmp.join("cache"));
    std::fs::create_dir_all(&sessions).unwrap();
    // The seeded session sits where a fixture seeds one until the binary's
    // first run moves it, so both places are ended.
    let seeded = common::seed_sessions_dir(&repo.join(".git"));
    for dir in [&seeded, &sessions] {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "json") {
                if let Ok(s) = std::fs::read_to_string(&p) {
                    let _ = std::fs::write(&p, s.replace("\"active\"", "\"ended\""));
                }
            }
        }
    }
    let sid = format!("res-{n}");
    let transcript = tmp.join("agent-transcripts").join(format!("{sid}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        format!("{{\"type\":\"user\",\"message\":\"prompt {n}\"}}\n"),
    )
    .unwrap();
    std::fs::write(
        sessions.join(format!("{sid}.json")),
        format!(
            r#"{{"schema":1,"session_id":"{sid}","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,"updated_at_ms":{},"native_id_map":{{}},"transcript_path":{}}}"#,
            now_ms(),
            json_str(&transcript)
        ),
    )
    .unwrap();
    let before = checkpoint_shards(repo);
    std::fs::write(repo.join(format!("w{n}.txt")), format!("w{n}\n")).unwrap();
    git_in(repo, &["add", "."]);
    git_in(repo, &["commit", "-q", "-m", &format!("w{n}")]);
    let gd = repo.join(".git");
    assert_eq!(
        run_rift(
            repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
            Some(""),
            env,
        ),
        0
    );
    let new: Vec<String> = checkpoint_shards(repo)
        .into_iter()
        .filter(|s| !before.contains(s))
        .collect();
    let id = new.first().expect("a new shard landed").replace('/', "");
    git_in(
        repo,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("carry{n}\n\nRift-Checkpoint: {id}"),
        ],
    );
    id
}

/// P84Bv blocker 2, pinned: resuming TWICE from a **filter-capable** remote
/// (`uploadpack.allowFilter=true` — the GitHub-class server the path-remote
/// e2e missed) must stay whole. The deleted treeless probe used to poison
/// the clone on resume #1 (`remote.origin.promisor=true` +
/// `partialclonefilter=tree:0`, written durably by `git fetch --filter`),
/// after which the ladder's own full rung fetched tree-filtered and resume
/// #2 of a NEW checkpoint dead-ended on a shard whose blobs never arrived.
#[test]
fn resume_twice_from_a_filter_capable_remote_stays_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let server = seed_live_session_repo(tmp.path());
    // The condition that arms the poison: a server that ALLOWS filters.
    git_in(&server, &["config", "uploadpack.allowFilter", "true"]);
    let cache = tmp.path().join("cache");
    let env = sync_env(&cache);
    let cp1 = land_server_checkpoint(&server, tmp.path(), 1, &env);

    let clone = clone_of(tmp.path(), &server, "clone");
    std::fs::create_dir_all(clone.join(".rift")).unwrap();
    let clone_cache = tmp.path().join("clone-cache");
    common::provision_here(&clone, &clone_cache);
    let clone_env = query_env(&clone_cache);
    let branch = current_branch(&clone);

    // Resume #1: the ladder's full rung lands the shard.
    let v = rift_json(
        &clone,
        &["session", "resume", &branch, "--json"],
        &clone_env,
    );
    assert_eq!(v["checkpoint_id"], cp1, "{v}");
    // The direct poison pin: NO partial-clone config was written.
    assert_eq!(
        git_config_or_empty(&clone, "remote.origin.promisor"),
        "",
        "no promisor config after resume #1"
    );
    assert_eq!(
        git_config_or_empty(&clone, "remote.origin.partialclonefilter"),
        "",
        "no partial-clone filter after resume #1"
    );

    // A NEW checkpoint lands on the server; the clone pulls the branch.
    let cp2 = land_server_checkpoint(&server, tmp.path(), 2, &env);
    git_in(&clone, &["fetch", "-q", "origin"]);
    git_in(
        &clone,
        &["merge", "-q", "--ff-only", &format!("origin/{branch}")],
    );

    // Resume #2 — the exact sequence the poison killed — must stay whole.
    let v2 = rift_json(
        &clone,
        &["session", "resume", &branch, "--json"],
        &clone_env,
    );
    assert_eq!(v2["checkpoint_id"], cp2, "{v2}");
    assert!(
        metadata_blob_is_local(&clone, &cp2),
        "the shard's blobs arrived with the ref (no filtered fetch)"
    );
}

/// `git config --get <key>`, or the empty string where nothing set it.
fn git_config_or_empty(dir: &Path, key: &str) -> String {
    git_in(dir, &["config", "--default", "", "--get", key])
        .trim()
        .to_owned()
}

/// Whether the shard's `metadata.json` blob is PHYSICALLY present in `clone`
/// — a lazy fetch is refused, so a promisor object reads as absent.
fn metadata_blob_is_local(clone: &Path, checkpoint_id: &str) -> bool {
    let shard_path = format!("{}/{}", &checkpoint_id[..2], &checkpoint_id[2..]);
    let oid = rev_parse(
        clone,
        &format!("refs/heads/rift/checkpoints/v1:{shard_path}/metadata.json"),
    );
    Command::new("git")
        .args(["cat-file", "-e", &oid])
        .env("GIT_NO_LAZY_FETCH", "1")
        .current_dir(clone)
        .output()
        .unwrap()
        .status
        .success()
}

/// The trailer-is-a-claim rule, at the resume surface. `prepare-commit-msg`
/// predicts an id before the commit exists and
/// the shard the archive lands derives another, so a resume that reads only
/// the trailer refuses with `shard_unavailable` on every real session (the
/// benchmark harness's `resume-restore` row failed 3/3 on exactly this). The
/// commit here carries a trailer naming an id that never lands; the archive
/// anchors the landed one to the commit; the resume must restore from it.
#[test]
fn a_resume_restores_from_the_landed_checkpoint_when_the_trailer_predicted_another() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let transcript = write_agent_transcript(tmp.path(), LIVE_SESSION, TWO_LINE_TRANSCRIPT);
    seed_session_with_transcript(&repo, LIVE_SESSION, &transcript);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    // A commit whose trailer claims an id that will never land.
    let predicted = "0".repeat(64);
    commit_carrying_trailer(&repo, &predicted);
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = sync_env_with_data(&cache, &data);
    // The archive anchors the LANDED checkpoint to that commit.
    archive_head(&repo, &env);
    let landed = only_checkpoint_id(&repo);
    assert_ne!(landed, predicted, "the trailer's prediction never landed");

    let branch = current_branch(&repo);
    std::fs::remove_file(&transcript).unwrap();
    let (code, out) = run_rift_stdout(
        &repo,
        &["session", "resume", &branch, "--force", "--json"],
        &env,
    );
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap_or_default();
    assert_eq!(
        code, 0,
        "resume must not refuse on a trailer that never landed: {out}"
    );
    assert_eq!(
        v["checkpoint_id"], landed,
        "the landed id, not the predicted one: {v}"
    );
    assert_eq!(v["sessions"][0]["restored"], "restored", "{v}");
    assert!(transcript.is_file(), "the transcript is back in place");
}
