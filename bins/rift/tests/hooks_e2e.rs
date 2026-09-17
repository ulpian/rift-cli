//! End-to-end: drive the real `rift` binary through the git-hook surface —
//! install writes absolute-path hook entries, `pre-push` filters `rift/*` egress, the
//! commit-path hooks never block even on a panic, and a GUI-style sparse `PATH`
//! still runs the installed entry via its absolute path.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Stdio};

mod common;
use common::rift_cmd;

const RETIRED_BRAND: &str = concat!("ent", "ire");
const RETIRED_SHADOW_PREFIX: &str = concat!("refs/", "ent", "ire", "/shadow/");
const RETIRED_CHECKPOINT_PREFIX: &str = concat!("refs/", "ent", "ire", "/checkpoints/");

fn git_dir(tmp: &Path) -> std::path::PathBuf {
    // A gitdir hardened discovery accepts (`HEAD` + `objects/`), not the bare
    // `<tmp>/.git` this helper used to leave behind: that shape is a phantom
    // and every store the product opens is now resolved through
    // the repository family a real gitdir names.
    let gd = rift_git::test_support::fake_repo(tmp).unwrap();
    std::fs::create_dir_all(gd.join("hooks")).unwrap();
    gd
}

/// Run `rift <args>` from `cwd` with optional stdin; return the exit code.
/// The cwd is MANDATORY and hermetic (see `common::rift_cmd`): the agent-hook
/// surface resolves its repository from the process working directory, so an
/// unpinned spawn is a write into whatever repository the test runner sits in.
fn run_rift(cwd: &Path, args: &[&str], stdin: Option<&str>, extra_env: &[(&str, &str)]) -> i32 {
    run_rift_capture(cwd, args, stdin, extra_env).0
}

/// Run `rift <args>` from `cwd` with optional stdin; return the exit code and
/// stderr text (the hook surface's only user-visible channel — loudness
/// assertions read it).
fn run_rift_capture(
    cwd: &Path,
    args: &[&str],
    stdin: Option<&str>,
    extra_env: &[(&str, &str)],
) -> (i32, String) {
    let mut cmd = rift_cmd(cwd);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    if let Some(s) = stdin {
        child.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    }
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A path as a JSON string literal (quotes included): Windows paths carry
/// backslashes, which a raw `Display` splice turns into invalid escapes.
fn json_str(path: &Path) -> String {
    serde_json::Value::String(path.display().to_string()).to_string()
}

/// Drive one real agent read hook — `PostToolUse` carrying a `Read` payload with
/// an absolute path, as Claude Code sends it — from the repo's cwd, so the
/// session captures a read fact. The hook surface never fails the agent, so the
/// only assertion here is that it exited cleanly.
fn agent_read_hook(repo: &Path, session: &str, turn: u32, file: &Path, env: &[(&str, &str)]) {
    let body = format!(
        r#"{{"schema":1,"session":"{session}","turn":{turn},"payload":{{"tool_name":"Read","tool_input":{{"file_path":{}}}}}}}"#,
        json_str(file)
    );
    let mut child = rift_cmd(repo)
        .args(["hooks", "claude-code", "PostToolUse"])
        .envs(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "agent hook never fails");
}

/// Drive one native agent hook (`rift hooks claude-code <verb>`) with the
/// stdin capture contract. The fake agent of the capture fixtures, generalized
/// past `PostToolUse Read`.
fn agent_hook(
    repo: &Path,
    verb: &str,
    session: &str,
    turn: u32,
    payload: &str,
    env: &[(&str, &str)],
) {
    let body = format!(r#"{{"schema":1,"session":"{session}","turn":{turn},"payload":{payload}}}"#);
    let mut child = rift_cmd(repo)
        .args(["hooks", "claude-code", verb])
        .envs(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success(), "agent hook never fails");
}

/// The same hook, with its stdout kept: the one channel a decision about a
/// tool call travels on. Everything else about it is [`agent_hook`].
fn agent_hook_out(
    repo: &Path,
    agent: &str,
    verb: &str,
    session: &str,
    turn: u32,
    payload: &str,
    env: &[(&str, &str)],
) -> String {
    let body = format!(r#"{{"schema":1,"session":"{session}","turn":{turn},"payload":{payload}}}"#);
    let mut child = rift_cmd(repo)
        .args(["hooks", agent, verb])
        .envs(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "agent hook never fails");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Drive one native agent hook with the bytes its **own client** would put on
/// stdin — no envelope around them — and hand back what it wrote to stdout.
///
/// The families whose payload fields decide the answer are read straight off
/// that object, so a walk that wrapped it would be testing the wrapper.
fn agent_hook_raw(
    repo: &Path,
    agent: &str,
    verb: &str,
    body: &str,
    env: &[(&str, &str)],
) -> String {
    let mut child = rift_cmd(repo)
        .args(["hooks", agent, verb])
        .envs(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "agent hook never fails");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Seed a hermetic git repo with a **live** native-hook session cache file and
/// one staged file, but **no commit**: HEAD is unborn, so an archive has nothing
/// to anchor a checkpoint to. This is how a deferral is reproduced now that
/// there is no second archive route whose binary could be withheld.
fn seed_live_session_repo_unborn(tmp: &Path) -> std::path::PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "a.txt"]);
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join("e2e-live-1.json"),
        r#"{"schema":1,"session_id":"e2e-live-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{}}"#,
    )
    .unwrap();
    repo
}

/// Seed a hermetic git repo with one commit and a **live** native-hook session
/// cache file, so the post-commit leg sees an archivable session. Returns the
/// work-tree path. Fixed timestamps only — the test never reads the clock.
fn seed_live_session_repo(tmp: &Path) -> std::path::PathBuf {
    let repo = seed_live_session_repo_unborn(tmp);
    git_in(&repo, &["commit", "-q", "-m", "seed"]);
    repo
}

/// The condense regression at the product surface: a commit made while a
/// native-hook session is live archives a real checkpoint shard onto
/// `refs/heads/rift/checkpoints/v1`, with its four structural artifacts, on a
/// repository that has **never been configured** — no `.rift/rewrite.toml`,
/// which is every repository, because nothing in the product ever wrote one.
///
/// Before 2026-08-10 this exact scenario produced no
/// checkpoint at all: the route table's absent-file arm answered `Wrapper`, the
/// wrapper binary was not installed, and the hook skipped with a "run
/// `rift setup`" line. That was the shipped default, and it is the defect
/// "zero checkpoints → recall empty" standing in the product for a year.
/// The route table and the wrapper were deleted on 2026-08-11; this is the
/// behaviour that outlived them, and its twin
/// `a_stale_rewrite_toml_is_named_once_and_changes_nothing` proves the file the
/// old default was read from now decides nothing.
#[test]
fn an_unconfigured_repo_archives_a_checkpoint_without_any_rewrite_toml() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    assert!(
        !repo.join(".rift").join("rewrite.toml").exists(),
        "the point of this test is that nothing configures the archive"
    );
    let cache = tmp.path().join("cache");
    // A private substrate root too, so nothing on the developer's machine can
    // make this pass by accident.
    let data = tmp.path().join("data");

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
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("XDG_DATA_HOME", data.to_str().unwrap()),
            ("LOCALAPPDATA", data.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    assert_eq!(code, 0, "hook never fails the git op; stderr: {stderr}");
    assert!(
        !stderr.contains("FAILED") && !stderr.contains("skipped"),
        "a successful archive is quiet: {stderr}"
    );

    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let listing = String::from_utf8_lossy(&refs.stdout).into_owned();
    assert!(
        listing.contains("refs/heads/rift/checkpoints/v1"),
        "an unconfigured repo archived nothing: {listing:?}"
    );
    // The shard carries the four structural artifacts of a native archive.
    let tree = Command::new("git")
        .args([
            "ls-tree",
            "-r",
            "--name-only",
            "refs/heads/rift/checkpoints/v1",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    let names = String::from_utf8_lossy(&tree.stdout).into_owned();
    for artifact in [
        "content.redacted",
        "content_hash.txt",
        "metadata.json",
        "redaction_report.json",
    ] {
        assert!(names.contains(artifact), "{artifact} in shard: {names}");
    }
}

/// The transcript pair at the product surface: a session whose
/// cache remembers the agent transcript's ADDRESS archives the transcript
/// PAIR into its checkpoint shard — compact + full, with the compact name and
/// boundary in `metadata.json` — through the real post-commit hook binary.
#[test]
#[allow(clippy::too_many_lines)] // one linear e2e scenario: cache -> commit -> shard -> verify
fn a_real_archive_carries_the_compact_transcript() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());

    // The agent's transcript, where the agent keeps it (outside the repo —
    // the archive must read the ADDRESS, not assume a location).
    let transcript = tmp
        .path()
        .join("agent-transcripts")
        .join("e2e-live-1.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"add a feature"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    // The cache carries the path the way a hook payload would have left it.
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::write(
        sessions.join("e2e-live-1.json"),
        format!(
            r#"{{"schema":1,"session_id":"e2e-live-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{{}},"transcript_path":{}}}"#,
            json_str(&transcript)
        ),
    )
    .unwrap();

    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
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
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("XDG_DATA_HOME", data.to_str().unwrap()),
            ("LOCALAPPDATA", data.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    assert_eq!(code, 0, "hook never fails the git op; stderr: {stderr}");

    let tree = Command::new("git")
        .args([
            "ls-tree",
            "-r",
            "--name-only",
            "refs/heads/rift/checkpoints/v1",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    let names = String::from_utf8_lossy(&tree.stdout).into_owned();
    for artifact in ["transcript.compact.jsonl", "transcript.jsonl"] {
        assert!(names.contains(artifact), "{artifact} in shard: {names}");
    }
    let shard_path = names
        .lines()
        .find(|l| l.ends_with("metadata.json"))
        .unwrap()
        .to_owned();
    let meta = Command::new("git")
        .args([
            "show",
            &format!("refs/heads/rift/checkpoints/v1:{shard_path}"),
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    let meta = String::from_utf8_lossy(&meta.stdout).into_owned();
    assert!(
        meta.contains(r#""compact_transcript":"transcript.compact.jsonl""#),
        "metadata records the compact artifact name: {meta}"
    );
    assert!(
        meta.contains(r#""compact_transcript_start":0"#),
        "…and the first checkpoint's boundary starts at 0: {meta}"
    );
    // The worker recorded its progress boundary for the NEXT checkpoint:
    // two non-empty raw lines consumed.
    let progress = std::fs::read_to_string(sessions.join("e2e-live-1.transcript")).unwrap();
    assert!(
        progress.contains(r#""raw_lines": 2"#),
        "the sidecar carries the compactor's own raw-line count: {progress}"
    );
    // And the verify surface accepts the transcript-carrying shard.
    let (code, out) = run_rift_capture_in(
        &repo,
        &["verify"],
        "",
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("XDG_DATA_HOME", data.to_str().unwrap()),
            ("LOCALAPPDATA", data.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
        ],
    );
    assert_eq!(
        code, 0,
        "verify green over a transcript-carrying shard: {out}"
    );

    // Rw2, demonstrated by P83Bv and pinned here: the prompt arm of `why` and
    // `explain` must be ALIVE on a shard the CURRENT native writer produced —
    // canon-3 archives the transcript as `transcript.jsonl`, and both verbs
    // serve the STORED redacted prompt out of it (never a re-read of the
    // agent's live file, which this fixture keeps outside the repo).
    let shard = checkpoint_shards(&repo);
    let checkpoint_id = shard.first().expect("one archived shard").replace('/', "");
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(repo.join("b.txt"), "two\n").unwrap();
    git_in(&repo, &["add", "b.txt"]);
    git_in(
        &repo,
        &[
            "commit",
            "-q",
            "-m",
            &format!("carry\n\nRift-Checkpoint: {checkpoint_id}"),
        ],
    );
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("XDG_DATA_HOME", data.to_str().unwrap()),
        ("LOCALAPPDATA", data.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];
    let (code, out) = run_rift_stdout(&repo, &["why", "b.txt:1", "--json"], &env);
    assert_eq!(code, 0, "why over the native shard: {out}");
    let w: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(
        w["prompt"]["text"], "add a feature",
        "why serves the stored redacted prompt from the canon-3 shard: {w}"
    );
    assert_eq!(w["prompt"]["extracted"], true, "{w}");
    assert_eq!(
        w["prompt"]["line"], 0,
        "the intent locator's address, not a guess: {w}"
    );
    let (code, out) = run_rift_stdout(
        &repo,
        &["checkpoint", "explain", &checkpoint_id, "--json"],
        &env,
    );
    assert_eq!(code, 0, "explain over the native shard: {out}");
    let e: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(
        e["prompt"]["text"], "add a feature",
        "explain's prompt arm reads the same canon-3 artifact: {e}"
    );
}

/// The resume exit shape, end to end with the fake agent's session fixture:
/// capture → commit → **wipe the native log** → resume →
/// the transcript is back at the agent's own address with the shard's stored
/// redacted bytes, the printed command names the session, and the recall
/// known-state cursor is seeded so the first injected manifest reflects the
/// resumed session. Then the safety rail (item 6): a newer un-checkpointed
/// commit turns resume into a refusal that `--force` overrides — and nothing
/// is ever reset.
#[test]
#[allow(clippy::too_many_lines)] // one linear round-trip scenario
fn resume_round_trips_with_the_fake_agent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let transcript = tmp
        .path()
        .join("agent-transcripts")
        .join("e2e-live-1.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"add a feature"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::write(
        sessions.join("e2e-live-1.json"),
        format!(
            r#"{{"schema":1,"session_id":"e2e-live-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{{}},"transcript_path":{}}}"#,
            json_str(&transcript)
        ),
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("XDG_DATA_HOME", data.to_str().unwrap()),
        ("LOCALAPPDATA", data.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let gd = repo.join(".git");
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );
    let shard = checkpoint_shards(&repo);
    let checkpoint_id = shard.first().expect("one archived shard").replace('/', "");
    // The shard's stored redacted transcript — the byte truth resume serves.
    let stored = Command::new("git")
        .args([
            "show",
            &format!(
                "refs/heads/rift/checkpoints/v1:{}/transcript.jsonl",
                shard.first().unwrap()
            ),
        ])
        .current_dir(&repo)
        .output()
        .unwrap()
        .stdout;
    assert!(!stored.is_empty(), "the shard archived the transcript");

    // A commit that CARRIES the trailer, so the branch walk has a claim.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(repo.join("b.txt"), "two\n").unwrap();
    git_in(&repo, &["add", "b.txt"]);
    git_in(
        &repo,
        &[
            "commit",
            "-q",
            "-m",
            &format!("carry\n\nRift-Checkpoint: {checkpoint_id}"),
        ],
    );
    let branch = git_in(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .trim()
        .to_owned();

    // WIPE the native log — the resume must restore it, not find it.
    std::fs::remove_file(&transcript).unwrap();

    let (code, out) = run_rift_stdout(&repo, &["session", "resume", &branch, "--json"], &env);
    assert_eq!(code, 0, "resume: {out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v["checkpoint_id"], checkpoint_id, "{v}");
    assert_eq!(v["newer_commits"], 0, "the trailer commit is the tip: {v}");
    assert_eq!(v["sessions"][0]["session_id"], "e2e-live-1", "{v}");
    assert_eq!(v["sessions"][0]["agent"], "claude-code", "{v}");
    assert_eq!(v["sessions"][0]["restored"], "restored", "{v}");
    assert_eq!(
        v["sessions"][0]["transcript_path"],
        transcript.display().to_string(),
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
        std::fs::read(&transcript).unwrap(),
        stored,
        "the restored transcript is the shard's stored bytes"
    );
    // The recall known-state cursor: a baseline now exists for this session's
    // MAIN-THREAD window, so the next injected manifest is a delta, not a
    // bootstrap. A resume restores the conversation, and a subagent's window
    // does not outlive the turn that spawned it, so the key's agent half is
    // the empty one.
    let key = window_key("e2e-live-1", "");
    let baseline = repo
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

    // Second resume without --force: the existing local log is KEPT.
    let (code, out) = run_rift_stdout(&repo, &["session", "resume", &branch, "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(
        v["sessions"][0]["restored"], "kept-existing",
        "an existing local session log is never overwritten by default: {v}"
    );
    // …and with --force it is overwritten from the checkpoint again.
    let (code, out) = run_rift_stdout(
        &repo,
        &["session", "resume", &branch, "--force", "--json"],
        &env,
    );
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v["sessions"][0]["restored"], "restored", "{v}");

    // The no-arg surface lists the session with its branch (non-TTY: a list,
    // never a hanging prompt) — the picker's data, Canopy-row shaped.
    let (code, out) = run_rift_stdout(&repo, &["session", "resume", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    let rows = v["resumable"].as_array().unwrap();
    assert!(
        rows.iter().any(|r| r["session_id"] == "e2e-live-1"
            && r["branch"] == branch.as_str()
            && r["agent"] == "claude-code"),
        "the stopped session is listed with its branch: {v}"
    );

    // Safety rail (item 6): newer un-checkpointed work above the checkpoint
    // turns a non-interactive resume into a typed refusal…
    std::fs::write(repo.join("c.txt"), "three\n").unwrap();
    git_in(&repo, &["add", "c.txt"]);
    git_in(&repo, &["commit", "-q", "-m", "newer work, no checkpoint"]);
    let tip_before = git_in(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let (code, out) = run_rift_stdout(&repo, &["session", "resume", &branch, "--json"], &env);
    assert_eq!(code, 4, "a refusal, not a guess: {out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v["error"], "newer_commits", "{v}");
    assert_eq!(v["newer_commits"], 1, "{v}");
    // …that --force overrides, and NOTHING was reset either way.
    let (code, out) = run_rift_stdout(
        &repo,
        &["session", "resume", &branch, "--force", "--json"],
        &env,
    );
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v["checkpoint_id"], checkpoint_id, "{v}");
    assert_eq!(v["newer_commits"], 1, "{v}");
    assert_eq!(
        git_in(&repo, &["rev-parse", "HEAD"]).trim(),
        tip_before,
        "resume never moves HEAD"
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
#[allow(clippy::too_many_lines)] // one linear archive -> clone -> ladder scenario
fn resume_fetch_ladder_pulls_the_shard_from_a_path_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let transcript = tmp
        .path()
        .join("agent-transcripts")
        .join("e2e-live-1.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        concat!(r#"{"type":"user","message":"hello"}"#, "\n"),
    )
    .unwrap();
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::write(
        sessions.join("e2e-live-1.json"),
        format!(
            r#"{{"schema":1,"session_id":"e2e-live-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{{}},"transcript_path":{}}}"#,
            json_str(&transcript)
        ),
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    common::provision_here(&repo, &cache);
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let gd = repo.join(".git");
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );
    let shard = checkpoint_shards(&repo);
    let checkpoint_id = shard.first().expect("one archived shard").replace('/', "");
    std::fs::write(repo.join("b.txt"), "two\n").unwrap();
    git_in(&repo, &["add", "b.txt"]);
    git_in(
        &repo,
        &[
            "commit",
            "-q",
            "-m",
            &format!("carry\n\nRift-Checkpoint: {checkpoint_id}"),
        ],
    );
    let branch = git_in(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .trim()
        .to_owned();

    // The clone: branches arrive as remote-tracking refs only, so the local
    // checkpoints ref — the one the walker reads — is absent by construction.
    let clone = tmp.path().join("clone");
    let st = Command::new("git")
        .args([
            "clone",
            "-q",
            repo.to_str().unwrap(),
            clone.to_str().unwrap(),
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(st.status.success());
    let absent = Command::new("git")
        .args(["show-ref", "--verify", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&clone)
        .output()
        .unwrap();
    assert!(
        !absent.status.success(),
        "precondition: the clone has no local checkpoints ref"
    );
    std::fs::create_dir_all(clone.join(".rift")).unwrap();

    let clone_cache = tmp.path().join("clone-cache");
    common::provision_here(&clone, &clone_cache);
    let clone_env = [
        ("RIFT_CACHE_DIR", clone_cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];
    let (code, out) = run_rift_stdout(
        &clone,
        &["session", "resume", &branch, "--json"],
        &clone_env,
    );
    assert_eq!(code, 0, "the ladder resolved the shard: {out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v["checkpoint_id"], checkpoint_id, "{v}");
    // The rung's effect is durable: the local ref now exists.
    let present = Command::new("git")
        .args(["show-ref", "--verify", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&clone)
        .output()
        .unwrap();
    assert!(
        present.status.success(),
        "the origin rung fetched the checkpoints ref into the clone"
    );
    // No journal travelled with the clone, so the agent is honestly unknown
    // and the restore says where the transcript still lives.
    assert_eq!(v["sessions"][0]["session_id"], "e2e-live-1", "{v}");
    assert_eq!(v["sessions"][0]["agent"], "unknown", "{v}");
    assert_eq!(v["sessions"][0]["restored"], "no-native-path", "{v}");
}

/// `attach` adopts an out-of-band session (no journal, no
/// hooks ever fired) by finding its native transcript at the agent's default
/// location, archiving it against the last commit through the one archive
/// machinery, and — only on an explicit yes — amending the commit to carry
/// the trailer. Idempotent: an unchanged re-attach derives the same
/// checkpoint id and moves nothing.
#[test]
#[allow(clippy::too_many_lines)] // one linear adopt -> idempotency -> amend scenario
fn attach_adopts_a_session_idempotently_and_amend_needs_an_explicit_yes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "a.txt"]);
    git_in(&repo, &["commit", "-q", "-m", "seed"]);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();

    // The out-of-band transcript, at claude-code's own default location under
    // a redirected HOME (abs worktree path, '/' and '.' folded to '-'). The
    // binary discovers the worktree CANONICALIZED (macOS /var -> /private/var),
    // so the munge is computed over the canonical path.
    let home = tmp.path().join("home");
    // On Windows the canonical spelling is verbatim and carries `\` and the
    // drive's `:`; Claude Code folds those too (its own project directory
    // for `c:\Users\…\x` is `c--Users-…-x`), so the fixture spells the plain
    // `/`-form first and folds `:` with the rest. The result is relative on
    // every platform, so the transcript stays inside this tempdir.
    let munged: String = common::shell_spelling(&repo.canonicalize().unwrap())
        .chars()
        .map(|c| {
            if c == '/' || c == '.' || c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect();
    let native = home
        .join(".claude")
        .join("projects")
        .join(munged)
        .join("oob-1.jsonl");
    std::fs::create_dir_all(native.parent().unwrap()).unwrap();
    std::fs::write(
        &native,
        concat!(r#"{"type":"user","message":"out of band work"}"#, "\n"),
    )
    .unwrap();

    let cache = tmp.path().join("cache");
    common::provision_here(&repo, &cache);
    // The amend below is a real `git commit`, so the identity it commits
    // under is stated here the way `git_in` states it for the fixture's own
    // commits. Left to git's auto-detection it depends on the machine: a
    // Linux runner's hostname yields one, a Windows box's `(none)` domain is
    // refused outright.
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("HOME", home.to_str().unwrap()),
        ("GIT_AUTHOR_NAME", "T"),
        ("GIT_AUTHOR_EMAIL", "t@t"),
        ("GIT_COMMITTER_NAME", "T"),
        ("GIT_COMMITTER_EMAIL", "t@t"),
    ];
    let head_before = git_in(&repo, &["rev-parse", "HEAD"]).trim().to_owned();

    // 1. Non-TTY without --force: archived, adopted, NOT amended.
    let (code, out) = run_rift_stdout(
        &repo,
        &[
            "session",
            "attach",
            "oob-1",
            "--agent",
            "claude-code",
            "--json",
        ],
        &env,
    );
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    let checkpoint_id = v["checkpoint_id"].as_str().unwrap().to_owned();
    assert_eq!(
        v["amended"], false,
        "a pipe never amends without --force: {v}"
    );
    assert_eq!(v["already_claimed"], false, "{v}");
    // Compared canonically: the binary derives its answer from its cwd, which
    // is never the verbatim spelling `canonicalize` gives the expectation.
    assert_eq!(
        v["transcript_path"]
            .as_str()
            .map(|p| std::fs::canonicalize(p).expect("the transcript it names exists")),
        Some(native.canonicalize().unwrap()),
        "found at the agent's default location: {v}"
    );
    assert_eq!(
        git_in(&repo, &["rev-parse", "HEAD"]).trim(),
        head_before,
        "no amend happened"
    );
    // Adopted: the journal now records the session as ended work.
    let journal =
        std::fs::read_to_string(repo.join(".git").join("rift-sessions").join("oob-1.json"))
            .expect("attach adopted the session into the journal");
    assert!(journal.contains(r#""state": "ended""#), "{journal}");
    // The shard is real and readable.
    assert!(
        checkpoint_shards(&repo)
            .first()
            .is_some_and(|s| s.replace('/', "") == checkpoint_id),
        "the shard landed at the reported id"
    );

    // 2. Idempotency: the same attach derives the same id and moves nothing.
    let ref_before = git_in(&repo, &["rev-parse", "refs/heads/rift/checkpoints/v1"])
        .trim()
        .to_owned();
    let (code, out) = run_rift_stdout(
        &repo,
        &[
            "session",
            "attach",
            "oob-1",
            "--agent",
            "claude-code",
            "--json",
        ],
        &env,
    );
    assert_eq!(code, 0, "{out}");
    let v2: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v2["checkpoint_id"], checkpoint_id.as_str(), "same id: {v2}");
    assert_eq!(
        git_in(&repo, &["rev-parse", "refs/heads/rift/checkpoints/v1"]).trim(),
        ref_before,
        "an unchanged re-attach is a no-op on the checkpoint ref"
    );

    // 3. --force: the amend fires, HEAD moves, the message carries the trailer.
    let (code, out) = run_rift_stdout(
        &repo,
        &[
            "session",
            "attach",
            "oob-1",
            "--agent",
            "claude-code",
            "--force",
            "--json",
        ],
        &env,
    );
    assert_eq!(code, 0, "{out}");
    let v3: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v3["amended"], true, "{v3}");
    let head_after = git_in(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    assert_ne!(head_after, head_before, "the amend rewrote the commit");
    assert!(
        git_in(&repo, &["log", "-1", "--format=%B"])
            .contains(&format!("Rift-Checkpoint: {checkpoint_id}")),
        "the trailer rode the amend"
    );

    // 4. After the amend the claim already stands: nothing amends again.
    let (code, out) = run_rift_stdout(
        &repo,
        &[
            "session",
            "attach",
            "oob-1",
            "--agent",
            "claude-code",
            "--force",
            "--json",
        ],
        &env,
    );
    assert_eq!(code, 0, "{out}");
    let v4: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v4["already_claimed"], true, "{v4}");
    assert_eq!(v4["amended"], false, "{v4}");
    assert_eq!(
        git_in(&repo, &["rev-parse", "HEAD"]).trim(),
        head_after,
        "an already-claimed attach never amends again"
    );

    // 5. The provenance surface resolves the attached checkpoint.
    let (code, out) = run_rift_stdout(
        &repo,
        &["checkpoint", "explain", &checkpoint_id, "--json"],
        &env,
    );
    assert_eq!(code, 0, "explain over the attached shard: {out}");
    let e: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(e["checkpoint_id"], checkpoint_id.as_str(), "{e}");
    assert_eq!(
        e["prompt"]["text"], "out of band work",
        "the adopted transcript serves its prompt: {e}"
    );
}

/// Every capture finding-class doctor reports has a broken
/// fixture here and a safe `--fix` that heals exactly it. All plants live in
/// THIS test's throwaway tempdir repo — every destructive step is rooted at
/// the fixture path, never the process cwd.
#[test]
#[allow(clippy::too_many_lines)] // one linear plant -> report -> fix -> verify scenario
fn doctor_findings_have_broken_fixtures_and_safe_fixes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let gd = repo.join(".git");

    // A real archive first: it gives the repo a checkpoint branch, a chain
    // entry, and a projected row — the healthy baseline every plant breaks.
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );
    assert_eq!(checkpoint_shards(&repo).len(), 1);

    // PLANT stuck session: the seeded journal is already `active` with an
    // ancient stamp — that IS the stuck shape. (The archive above did not
    // close it; manual-commit archives never do.)
    // PLANT watermark drift: drop the projection db (a rebuildable cache in
    // this fixture's own tempdir) so the shard on the branch has no row.
    let db = cache.join("rift");
    assert!(db.is_dir(), "the projection cache exists before the plant");
    std::fs::remove_dir_all(&db).unwrap();
    // PLANT chain tail gap: rewind the chain ref by one entry (throwaway
    // repo; this is the broken fixture, not an operation on real history).
    let chain_tip = git_in(&repo, &["rev-parse", "refs/rift/chain/v1"])
        .trim()
        .to_owned();
    let parents = git_in(&repo, &["rev-list", "--parents", "-1", &chain_tip]);
    match parents.split_whitespace().count() {
        1 => git_in(&repo, &["update-ref", "-d", "refs/rift/chain/v1"]),
        _ => git_in(
            &repo,
            &["update-ref", "refs/rift/chain/v1", &format!("{chain_tip}^")],
        ),
    };
    // PLANT a queue entry: a commit awaiting condense that no worker drained.
    let head = git_in(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    let qdir = gd.join("rift-capture-queue");
    std::fs::create_dir_all(&qdir).unwrap();
    std::fs::write(
        qdir.join(format!("{head}.json")),
        format!(
            r#"{{"schema_version":1,"commit":"{head}","work_tree":{},"session_id":null,"agent":null,"session_state":null,"queued_at_ms":1783000000000,"attempts":0}}"#,
            json_str(&repo)
        ),
    )
    .unwrap();

    // REPORT (no --fix): every finding is named and NOTHING is repaired.
    let (code, out) = run_rift_stdout(&repo, &["doctor", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let d: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(
        d["sessions"]["stuck"][0], "e2e-live-1",
        "the stuck session is a finding: {d}"
    );
    assert_eq!(
        d["capture_chain"]["gap"], 1,
        "the tail gap is a finding: {d}"
    );
    assert!(
        d["watermark"]["drift"].as_i64().unwrap() >= 1,
        "the un-projected shard is a finding: {d}"
    );
    assert!(
        d["capture_queue"]["pending"].as_i64().unwrap_or(0) >= 1,
        "the queue entry is a finding: {d}"
    );
    let journal =
        std::fs::read_to_string(gd.join("rift-sessions").join("e2e-live-1.json")).unwrap();
    assert!(
        journal.contains(r#""state":"active""#) || journal.contains(r#""state": "active""#),
        "report-only doctor did NOT finalize the session: {journal}"
    );

    // FIX: each finding's safe repair runs, and a second doctor is clean.
    let (code, out) = run_rift_stdout(&repo, &["doctor", "--fix", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let f: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(f["fix"], true, "{f}");
    let (code, out) = run_rift_stdout(&repo, &["doctor", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let after: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(
        after["sessions"]["stuck"].as_array().map(Vec::len),
        Some(0),
        "the stuck session was finalized: {after}"
    );
    assert_eq!(
        after["capture_chain"]["gap"], 0,
        "the tail gap healed: {after}"
    );
    assert_eq!(
        after["watermark"]["drift"], 0,
        "the projection leveled: {after}"
    );
    assert_eq!(
        after["capture_queue"]["pending"].as_i64().unwrap_or(0),
        0,
        "the queue drained: {after}"
    );
}

/// The `clean` half: transients go (ephemeral shadow refs
/// past TTL, stale queue entries, run-state caches) and the canonical
/// surfaces stay byte-identical — pinned, not assumed.
#[test]
#[allow(clippy::too_many_lines)] // one linear plant -> dry-run -> clean -> pin scenario
fn clean_prunes_transients_and_never_touches_canonical_refs() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let gd = repo.join(".git");
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );
    let head = git_in(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    // Plants: an ephemeral shadow ref, the LEGACY metadata ref that must
    // survive (read-compat import source), an ancient queue entry, and a
    // manifest baseline cache.
    git_in(
        &repo,
        &[
            "update-ref",
            concat!("refs/", "ent", "ire", "/shadow/deadbeef"),
            &head,
        ],
    );
    git_in(
        &repo,
        &[
            "update-ref",
            concat!("refs/", "ent", "ire", "/checkpoints/v1"),
            &head,
        ],
    );
    let qdir = gd.join("rift-capture-queue");
    std::fs::create_dir_all(&qdir).unwrap();
    std::fs::write(
        qdir.join(format!("{head}.json")),
        format!(
            r#"{{"schema_version":1,"commit":"{head}","work_tree":{},"session_id":null,"agent":null,"session_state":null,"queued_at_ms":1,"attempts":0}}"#,
            json_str(&repo)
        ),
    )
    .unwrap();
    let state = repo.join(".rift").join("state").join("manifest").join("k");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("1.json"), "{}").unwrap();
    // A spent recovery marker, in the shape and the place the boundary hook
    // writes one. Nothing removed these before, so one file per compaction
    // generation per window accumulated for the life of the repository.
    let recovery = repo.join(".rift").join("state").join("recovery");
    std::fs::create_dir_all(&recovery).unwrap();
    let marker = recovery.join(format!("{}.json", "c".repeat(64)));
    std::fs::write(&marker, "").unwrap();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&marker)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
        .unwrap();

    // **DOCTOR'S HUMAN MODE, WITH THE REF ACTUALLY PLANTED.** The sibling
    // guard on this line swept `rift doctor` for the retired brand in a
    // sandbox that never created a retired shadow ref, so
    // `doctor_shadow_refs` returned its empty arm and the sweep read an empty
    // string: vacuous by construction, and it stayed green while the branded
    // branch printed the brand. Here the ref exists, so the branch is
    // REACHED — which is asserted first, because a purge assertion over output
    // that never contains the leg is the same vacuity wearing the other sign.
    //
    // Plain `rift doctor` (no `--fix`) is report-only for the session sweep
    // (`main.rs`'s `doctor_session_sweep` early-returns), so it perturbs nothing
    // the rest of this test measures, and `RIFT_CACHE_DIR` is this sandbox's
    // (a leg that runs doctor must not write into a real projection).
    let (code, doc) = run_rift_stdout(&repo, &["doctor"], &env);
    assert_eq!(code, 0, "{doc}");
    assert!(
        doc.contains("shadow refs:") && doc.contains("orphaned"),
        "the shadow-refs leg must be REACHED, or the purge below asserts nothing: {doc}"
    );
    assert!(
        !doc.to_ascii_lowercase().contains(RETIRED_BRAND),
        "the previous tool's brand reached a human surface: {doc}"
    );

    let checkpoints_before = git_in(&repo, &["rev-parse", "refs/heads/rift/checkpoints/v1"]);
    let chain_before = git_in(&repo, &["rev-parse", "refs/rift/chain/v1"]);

    // Dry run reports and removes NOTHING.
    let (code, out) = run_rift_stdout(
        &repo,
        &["clean", "--dry-run", "--ttl-days", "0", "--json"],
        &env,
    );
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v["mode"], "dry-run", "{v}");
    assert_eq!(
        v["pruned_shadow_refs"][0],
        concat!("refs/", "ent", "ire", "/shadow/deadbeef"),
        "{v}"
    );
    assert!(
        git_in(&repo, &["for-each-ref", RETIRED_SHADOW_PREFIX]).contains("deadbeef"),
        "dry run deleted nothing"
    );
    assert!(
        qdir.join(format!("{head}.json")).exists(),
        "dry run kept the queue entry"
    );

    // TTL is honored: at 14 days the fresh shadow ref is KEPT — while the
    // ANCIENT queue entry (queued in 1970) and the run-state caches are
    // legitimately past any TTL and go now.
    let (code, out) = run_rift_stdout(&repo, &["clean", "--ttl-days", "14", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let kept: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(kept["kept_shadow_refs"], 1, "{kept}");
    assert_eq!(kept["pruned_queue_entries"], 1, "{kept}");
    assert!(kept["dropped_state_dirs"].as_i64().unwrap() >= 1, "{kept}");
    assert_eq!(
        kept["expired_recovery_markers"], 1,
        "the spent recovery marker is swept with the rest of the run state: {kept}"
    );
    assert!(!marker.exists(), "and it is really gone");
    assert!(
        git_in(&repo, &["for-each-ref", RETIRED_SHADOW_PREFIX]).contains("deadbeef"),
        "a ref inside TTL survives"
    );

    // The real clean at TTL 0: the shadow ref goes too; canonical stays.
    let (code, out) = run_rift_stdout(&repo, &["clean", "--ttl-days", "0", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let c: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(
        c["pruned_shadow_refs"][0],
        concat!("refs/", "ent", "ire", "/shadow/deadbeef"),
        "{c}"
    );
    assert!(
        !git_in(&repo, &["for-each-ref", RETIRED_SHADOW_PREFIX]).contains("deadbeef"),
        "the ephemeral shadow ref is gone"
    );
    assert!(
        git_in(&repo, &["for-each-ref", RETIRED_CHECKPOINT_PREFIX]).contains("checkpoints/v1"),
        "the legacy metadata ref (read-compat import source) SURVIVES"
    );
    assert_eq!(
        git_in(&repo, &["rev-parse", "refs/heads/rift/checkpoints/v1"]),
        checkpoints_before,
        "the canonical checkpoint branch is byte-identical"
    );
    assert_eq!(
        git_in(&repo, &["rev-parse", "refs/rift/chain/v1"]),
        chain_before,
        "the chain ref is byte-identical"
    );
    assert!(
        !qdir.join(format!("{head}.json")).exists(),
        "the stale entry is gone"
    );
    assert!(!state.exists(), "the run-state cache dir is gone");
}

/// The session-state surface. `list` carries ages, agents,
/// models and token totals; `info` adds the session's checkpoints; `stop`
/// finalizes through the state machine's IdleTimeout → condense →
/// CondenseDone path (targeted: nothing else is even looked at) and is
/// idempotent (`already_ended`). The `sessions` alias answers too.
#[test]
#[allow(clippy::too_many_lines)] // one linear list -> info -> stop -> re-stop scenario
fn session_state_surface_lists_infos_and_stops_through_the_machine() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    // One turn with provider-reported usage on the turn end.
    agent_hook(
        &repo,
        "UserPromptSubmit",
        "e2e-live-1",
        1,
        r#"{"text":"work"}"#,
        &env,
    );
    agent_hook(
        &repo,
        "Stop",
        "e2e-live-1",
        1,
        r#"{"usage":{"input_tokens":5,"output_tokens":7}}"#,
        &env,
    );

    // list: the live session rides with agent, state and its token totals —
    // through the `sessions` alias, which the PRD spells.
    let (code, out) = run_rift_stdout(&repo, &["sessions", "list", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    let row = v["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["session_id"] == "e2e-live-1")
        .expect("the live session is listed")
        .clone();
    assert_eq!(row["agent"], "claude-code", "{row}");
    assert_eq!(row["state"], "active", "{row}");
    assert!(
        row["age_ms"].as_i64().is_some(),
        "an age rides the row: {row}"
    );
    assert_eq!(
        row["tokens"]["output"], 7,
        "token totals ride the row: {row}"
    );
    assert_eq!(row["tokens"]["uncached_input"], 5, "{row}");

    // info: the same identity plus the session's checkpoints (none yet).
    let (code, out) = run_rift_stdout(&repo, &["session", "info", "e2e-live-1", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let i: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(i["state"], "active", "{i}");
    assert_eq!(i["checkpoints"].as_array().unwrap().len(), 0, "{i}");
    // …and an unknown id is a typed refusal, not an empty document.
    let (code, out) = run_rift_stdout(&repo, &["session", "info", "nope", "--json"], &env);
    assert_eq!(code, 4, "{out}");
    let e: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(e["error"], "unknown_session", "{e}");

    // stop: the machine finalizes it — IdleTimeout -> condense (a real
    // checkpoint lands, SessionEnd reason) -> CondenseDone -> `ended`.
    let (code, out) = run_rift_stdout(&repo, &["session", "stop", "e2e-live-1", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let s: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(s["finalized"][0], "e2e-live-1", "{s}");
    assert!(
        !checkpoint_shards(&repo).is_empty(),
        "the stop condensed a checkpoint"
    );
    let journal = std::fs::read_to_string(
        repo.join(".git")
            .join("rift-sessions")
            .join("e2e-live-1.json"),
    )
    .unwrap();
    assert!(journal.contains(r#""state": "ended""#), "{journal}");
    // The stop's condense recorded its OWN chain position (P84Bv claim-gap
    // pin): report-only doctor — which heals NOTHING — sees no tail gap.
    // Before archive_now recorded in-op, this exact read showed gap 1.
    let (code, out) = run_rift_stdout(&repo, &["doctor", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let report: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(
        report["capture_chain"]["gap"], 0,
        "the stop-condensed checkpoint is IN the chain without any heal: {report}"
    );
    assert!(
        report["capture_chain"]["recorded"].as_i64().unwrap_or(0) >= 1,
        "…and the chain records it: {report}"
    );

    // A second stop is honest about it.
    let (code, out) = run_rift_stdout(&repo, &["session", "stop", "e2e-live-1", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let s2: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(s2["already_ended"][0], "e2e-live-1", "{s2}");

    // The stopped session now lists as ended — and still resumable from the
    // picker's list (a stopped session is exactly what resume shows).
    let (code, out) = run_rift_stdout(&repo, &["session", "list", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let after: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    let row = after["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["session_id"] == "e2e-live-1")
        .expect("still listed")
        .clone();
    assert_eq!(row["state"], "ended", "{row}");
    let (code, out) = run_rift_stdout(&repo, &["session", "resume", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let picker: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert!(
        picker["resumable"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["session_id"] == "e2e-live-1" && r["state"] == "ended"),
        "an ended session is exactly what the picker offers: {picker}"
    );
}

/// `import <agent>` bulk-reads the agent's per-repo
/// transcript store into READ-ONLY checkpoints — dry-run writes nothing, a
/// real run chains each shard once (the appended positions are the run's
/// import epoch), a re-run is a full no-op (same content-derived ids; ref
/// and chain unmoved), the imports are explainable by id, and nothing about
/// them is resumable (no journal entry, no commit claim).
#[test]
#[allow(clippy::too_many_lines)] // one linear dry-run -> import -> re-import -> surfaces scenario
fn import_bulk_reads_the_agent_store_idempotently_as_read_only_checkpoints() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let git = |args: &[&str]| {
        let st = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "git {args:?}");
    };
    git(&["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&["add", "a.txt"]);
    git_in(&repo, &["commit", "-q", "-m", "seed"]);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();

    let home = tmp.path().join("home");
    // On Windows the canonical spelling is verbatim and carries `\` and the
    // drive's `:`; Claude Code folds those too (its own project directory
    // for `c:\Users\…\x` is `c--Users-…-x`), so the fixture spells the plain
    // `/`-form first and folds `:` with the rest. The result is relative on
    // every platform, so the transcript stays inside this tempdir.
    let munged: String = common::shell_spelling(&repo.canonicalize().unwrap())
        .chars()
        .map(|c| {
            if c == '/' || c == '.' || c == ':' {
                '-'
            } else {
                c
            }
        })
        .collect();
    let store_dir = home.join(".claude").join("projects").join(munged);
    std::fs::create_dir_all(&store_dir).unwrap();
    std::fs::write(
        store_dir.join("s-one.jsonl"),
        concat!(
            r#"{"type":"user","message":"first imported session"}"#,
            "\n"
        ),
    )
    .unwrap();
    std::fs::write(
        store_dir.join("s-two.jsonl"),
        concat!(
            r#"{"type":"user","message":"second imported session"}"#,
            "\n"
        ),
    )
    .unwrap();

    let cache = tmp.path().join("cache");
    common::provision_here(&repo, &cache);
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("HOME", home.to_str().unwrap()),
    ];

    // Dry run: the report, and NOTHING on disk.
    let (code, out) = run_rift_stdout(
        &repo,
        &["import", "claude-code", "--dry-run", "--json"],
        &env,
    );
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v["dry_run"], true, "{v}");
    assert_eq!(v["sessions"].as_array().unwrap().len(), 2, "{v}");
    assert_eq!(v["imported"], 0, "{v}");
    assert!(checkpoint_shards(&repo).is_empty(), "dry-run wrote nothing");

    // The real run: two read-only checkpoints, chained as one import epoch.
    let (code, out) = run_rift_stdout(&repo, &["import", "claude-code", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v["imported"], 2, "{v}");
    assert_eq!(v["already_imported"], 0, "{v}");
    assert_eq!(v["import_epoch"]["start_position"], 0, "{v}");
    assert_eq!(v["import_epoch"]["end_position"], 1, "{v}");
    let first = v["sessions"][0].clone();
    assert_eq!(first["session_id"], "s-one", "name-sorted: {v}");
    assert_eq!(first["status"], "imported", "{v}");
    let id1 = first["checkpoint_id"].as_str().unwrap().to_owned();
    assert_eq!(checkpoint_shards(&repo).len(), 2, "two shards landed");
    // The import marker: the projection row says `import`,
    // not `manual-commit` — an imported transcript is not a live archive.
    let (code, out) = run_rift_stdout(&repo, &["checkpoint", "show", &id1, "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let shown: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(shown["strategy"], "import", "{shown}");
    let ref_tip = git_in(&repo, &["rev-parse", "refs/heads/rift/checkpoints/v1"])
        .trim()
        .to_owned();

    // Idempotency: the re-run derives the same ids and moves NOTHING.
    let (code, out) = run_rift_stdout(&repo, &["import", "claude-code", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let v2: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v2["imported"], 0, "{v2}");
    assert_eq!(v2["already_imported"], 2, "{v2}");
    assert_eq!(
        v2["import_epoch"],
        serde_json::Value::Null,
        "a no-op run appends no epoch: {v2}"
    );
    assert_eq!(
        git_in(&repo, &["rev-parse", "refs/heads/rift/checkpoints/v1"]).trim(),
        ref_tip,
        "the checkpoint ref is unmoved by a re-import"
    );
    // …and `rift rebuild` re-derives the marker from the shard's own
    // metadata.json, so the projection cannot forget it.
    let (code, out) = run_rift_stdout(&repo, &["rebuild", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let (code, out) = run_rift_stdout(&repo, &["checkpoint", "show", &id1, "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let shown: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(
        shown["strategy"], "import",
        "rebuild lost the import marker: {shown}"
    );

    // Read-only means NOT resumable: no journal entry rides an import, and
    // no commit claims one, so both resume surfaces answer emptiness.
    let (code, out) = run_rift_stdout(&repo, &["session", "resume", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let picker: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(
        picker["resumable"].as_array().unwrap().len(),
        0,
        "imported sessions are not resumable: {picker}"
    );
    let branch = git_in(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .trim()
        .to_owned();
    let (code, out) = run_rift_stdout(&repo, &["session", "resume", &branch, "--json"], &env);
    assert_eq!(code, 4, "{out}");
    let w: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(
        w["error"], "no_checkpoint",
        "no commit claims an import: {w}"
    );

    // …but they ARE searchable/explainable: explain by id serves the prompt.
    let (code, out) = run_rift_stdout(&repo, &["checkpoint", "explain", &id1, "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let e: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(e["prompt"]["text"], "first imported session", "{e}");

    // --session imports exactly the named one.
    std::fs::write(
        store_dir.join("s-three.jsonl"),
        concat!(
            r#"{"type":"user","message":"third imported session"}"#,
            "\n"
        ),
    )
    .unwrap();
    let (code, out) = run_rift_stdout(
        &repo,
        &["import", "claude-code", "--session", "s-three", "--json"],
        &env,
    );
    assert_eq!(code, 0, "{out}");
    let v3: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v3["imported"], 1, "{v3}");
    assert_eq!(v3["sessions"].as_array().unwrap().len(), 1, "{v3}");
    assert_eq!(v3["sessions"][0]["session_id"], "s-three", "{v3}");
}

/// The hook-refusal rail (import half): the same typed,
/// byte-locked refusal attach gives.
#[test]
fn import_refuses_to_run_from_a_hook_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let mut cmd = rift_cmd(&repo);
    cmd.args(["import", "claude-code", "--json"])
        .env("RIFT_CACHE_DIR", cache.to_str().unwrap())
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("GIT_DIR", repo.join(".git"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(4), "a typed refusal");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "{\n  \"env\": \"GIT_DIR\",\n  \"error\": \"refused_in_hook\",\n  \"verb\": \"import\"\n}\n",
        "the refusal document is locked"
    );
    assert!(checkpoint_shards(&repo).is_empty(), "nothing was imported");
}

/// The hook-refusal rail (attach half): a hook environment
/// gets a typed, byte-locked refusal — never an archive, never a hang.
#[test]
fn attach_refuses_to_run_from_a_hook_environment() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let mut cmd = rift_cmd(&repo);
    cmd.args([
        "session",
        "attach",
        "e2e-live-1",
        "--agent",
        "claude-code",
        "--json",
    ])
    .env("RIFT_CACHE_DIR", cache.to_str().unwrap())
    .env("RIFT_NO_KEYCHAIN", "1")
    // The exact variable git exports to every hook it runs. `rift_cmd`
    // scrubs it from the inherited environment; setting it here recreates
    // the hook shape deliberately.
    .env("GIT_DIR", repo.join(".git"))
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(4), "a typed refusal");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "{\n  \"env\": \"GIT_DIR\",\n  \"error\": \"refused_in_hook\",\n  \"verb\": \"attach\"\n}\n",
        "the refusal document is locked"
    );
    assert!(
        checkpoint_shards(&repo).is_empty(),
        "nothing was archived from a hook"
    );
    // Rift's own worker marker refuses too — the rail is not git-shaped only.
    let mut cmd = rift_cmd(&repo);
    cmd.args([
        "session",
        "attach",
        "e2e-live-1",
        "--agent",
        "claude-code",
        "--json",
    ])
    .env("RIFT_CACHE_DIR", cache.to_str().unwrap())
    .env("RIFT_NO_KEYCHAIN", "1")
    .env("RIFT_HOOK", "1")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(4), "{out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"env\": \"RIFT_HOOK\""),
        "the rift worker marker is refused by name"
    );
}

/// Land one checkpoint on a server fixture: end every live journal session,
/// start session `res-<n>` with its own transcript, commit, run the real
/// post-commit archive, and put the landed id in a trailer-carrying commit.
/// Returns the checkpoint id. (The resume-regression fixture's generator.)
fn land_server_checkpoint(repo: &Path, tmp: &Path, n: u32, env: &[(&str, &str)]) -> String {
    let sessions = repo.join(".git").join("rift-sessions");
    for entry in std::fs::read_dir(&sessions).into_iter().flatten().flatten() {
        let p = entry.path();
        if p.extension().is_some_and(|e| e == "json") {
            if let Ok(s) = std::fs::read_to_string(&p) {
                let _ = std::fs::write(&p, s.replace("\"active\"", "\"ended\""));
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
            r#"{{"schema":1,"session_id":"{sid}","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{{}},"transcript_path":{}}}"#,
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
#[allow(clippy::too_many_lines)] // one linear resume -> new checkpoint -> resume scenario
fn resume_twice_from_a_filter_capable_remote_stays_whole() {
    let tmp = tempfile::tempdir().unwrap();
    let server = seed_live_session_repo(tmp.path());
    // The condition that arms the poison: a server that ALLOWS filters.
    git_in(&server, &["config", "uploadpack.allowFilter", "true"]);
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let cp1 = land_server_checkpoint(&server, tmp.path(), 1, &env);

    let clone = tmp.path().join("clone");
    let st = Command::new("git")
        .args([
            "clone",
            "-q",
            server.to_str().unwrap(),
            clone.to_str().unwrap(),
        ])
        .current_dir(tmp.path())
        .output()
        .unwrap();
    assert!(st.status.success());
    std::fs::create_dir_all(clone.join(".rift")).unwrap();
    let clone_cache = tmp.path().join("clone-cache");
    common::provision_here(&clone, &clone_cache);
    let clone_env = [
        ("RIFT_CACHE_DIR", clone_cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];
    let branch = git_in(&clone, &["rev-parse", "--abbrev-ref", "HEAD"])
        .trim()
        .to_owned();

    // Resume #1: the ladder's full rung lands the shard.
    let (code, out) = run_rift_stdout(
        &clone,
        &["session", "resume", &branch, "--json"],
        &clone_env,
    );
    assert_eq!(code, 0, "resume #1: {out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v["checkpoint_id"], cp1, "{v}");
    // The direct poison pin: NO partial-clone config was written.
    let promisor = git_in(
        &clone,
        &["config", "--default", "", "--get", "remote.origin.promisor"],
    );
    let filter = git_in(
        &clone,
        &[
            "config",
            "--default",
            "",
            "--get",
            "remote.origin.partialclonefilter",
        ],
    );
    assert_eq!(promisor.trim(), "", "no promisor config after resume #1");
    assert_eq!(filter.trim(), "", "no partial-clone filter after resume #1");

    // A NEW checkpoint lands on the server; the clone pulls the branch.
    let cp2 = land_server_checkpoint(&server, tmp.path(), 2, &env);
    git_in(&clone, &["fetch", "-q", "origin"]);
    git_in(
        &clone,
        &["merge", "-q", "--ff-only", &format!("origin/{branch}")],
    );

    // Resume #2 — the exact sequence the poison killed — must stay whole.
    let (code, out) = run_rift_stdout(
        &clone,
        &["session", "resume", &branch, "--json"],
        &clone_env,
    );
    assert_eq!(code, 0, "resume #2 over the same remote: {out}");
    let v2: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v2["checkpoint_id"], cp2, "{v2}");
    // …and the shard's metadata blob is PHYSICALLY present (no lazy fetch).
    let shard_path = format!("{}/{}", &cp2[..2], &cp2[2..]);
    let oid = git_in(
        &clone,
        &[
            "rev-parse",
            &format!("refs/heads/rift/checkpoints/v1:{shard_path}/metadata.json"),
        ],
    )
    .trim()
    .to_owned();
    let present = Command::new("git")
        .args(["cat-file", "-e", &oid])
        .env("GIT_NO_LAZY_FETCH", "1")
        .current_dir(&clone)
        .output()
        .unwrap();
    assert!(
        present.status.success(),
        "the shard's blobs arrived with the ref (no filtered fetch)"
    );
}

/// The attribution exit criterion, closed on the resume fixture's repo: the
/// STORED attribution rows agree with a **recomputation from
/// primary sources** — the same commit diffed again, the same session's
/// turn-boundary rows replayed again, by this test rather than by the archive
/// that persisted them — and `blame` surfaces those same stored rows.
///
/// The fixture is the fake agent doing real work: a prompt opens turn 1, the
/// "agent" edits the committed file between the turn's boundary hooks, the
/// turn closes, and the real hook pair commits + archives. Deliberately
/// transcript-less, so the predicted trailer is exact and `blame` can walk
/// commit → trailer → checkpoint.
#[test]
#[allow(clippy::too_many_lines)] // one linear capture -> archive -> recompute scenario
fn stored_attribution_agrees_with_blame_and_a_recompute_from_primary_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // Turn 1: prompt → the agent edits a committed file → turn ends. The two
    // boundary snapshots bracket exactly the agent's two appended lines.
    agent_hook(
        &repo,
        "UserPromptSubmit",
        "e2e-live-1",
        1,
        r#"{"text":"add a feature"}"#,
        &env,
    );
    std::fs::write(
        repo.join("a.txt"),
        "one\nagent line two\nagent line three\n",
    )
    .unwrap();
    agent_hook(&repo, "Stop", "e2e-live-1", 1, "{}", &env);

    // Commit through the real hook pair; a transcript-less session's predicted
    // trailer is the shard that lands.
    git_in(&repo, &["add", "a.txt"]);
    let msg = tmp.path().join("COMMIT_EDITMSG");
    std::fs::write(&msg, "agent work\n").unwrap();
    let gd = repo.join(".git");
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "prepare-commit-msg",
                "--git-dir",
                gd.to_str().unwrap(),
                "--",
                msg.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );
    let predicted = std::fs::read_to_string(&msg)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("Rift-Checkpoint: ").map(str::to_owned))
        .expect("trailer injected");
    git_in(&repo, &["commit", "-q", "-F", msg.to_str().unwrap()]);
    let anchor = git_in(&repo, &["rev-parse", "HEAD"]).trim().to_owned();
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );
    assert_eq!(
        checkpoint_shards(&repo),
        vec![format!("{}/{}", &predicted[..2], &predicted[2..])],
        "the archive landed where the trailer promised"
    );

    // STORED: the attribution rows exactly as the archive persisted them.
    let repo_id = rift_git::repo_id_for(&repo).unwrap();
    let db = rift_store::projection_db_under(&cache, &repo_id);
    let store = rift_store::open_capture_projection(&db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    use rift_store::CanonicalStore as _;
    let stored = rt
        .block_on(store.query(
            "SELECT fr.path, a.agent_added, a.human_added, a.human_modified_agent, \
             a.confidence_milli \
             FROM attribution a \
             JOIN file_revision fr ON fr.file_revision_id = a.file_revision_id \
             WHERE a.checkpoint_id = ? ORDER BY fr.path",
            &[rift_store::Value::text(predicted.clone())],
        ))
        .unwrap();
    assert_eq!(stored.len(), 1, "one attributed file");
    let row = &stored[0];
    let (s_path, s_agent, s_human, s_hma, s_conf) = (
        row.get_text(0).unwrap(),
        row.get_i64(1).unwrap(),
        row.get_i64(2).unwrap(),
        row.get_i64(3).unwrap(),
        row.get_i64(4).unwrap(),
    );
    assert_eq!(s_path, "a.txt");
    assert!(
        s_agent >= 2,
        "the plant bit: the agent's two lines are attributed (got {s_agent})"
    );

    // RECOMPUTED: the same math, re-run here over the primary sources.
    let reader = rift_git::GixBlobReader::open(&repo).unwrap();
    let mut capture = rift_shadow::attribution::capture_commit(&reader, &anchor).unwrap();
    rt.block_on(rift_shadow::enrich_attribution(
        &store,
        &reader,
        "claude-code",
        "e2e-live-1",
        &mut capture,
    ))
    .unwrap();
    let f = capture
        .files
        .iter()
        .find(|f| f.path == "a.txt")
        .expect("the edited file is in the recomputed capture");
    assert_eq!(
        i64::from(f.attribution.agent_lines),
        s_agent,
        "stored vs recomputed agent_added"
    );
    assert_eq!(
        i64::from(f.attribution.human_lines),
        s_human,
        "stored vs recomputed human_added"
    );
    assert_eq!(
        i64::from(f.attribution.human_modified_agent_lines),
        s_hma,
        "stored vs recomputed human_modified_agent"
    );
    // Confidence within bounds: the same factors re-derived, allowing one
    // milli of rounding.
    #[allow(clippy::cast_possible_truncation)]
    let recomputed_milli = (f.confidence * 1000.0).round() as i64;
    assert!(
        (recomputed_milli - s_conf).abs() <= 1,
        "stored confidence {s_conf} vs recomputed {recomputed_milli}"
    );

    // …and BLAME surfaces the same stored rows over the same repo. The query
    // surface sits behind the `.rift/` init guard, so the fixture opts in.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let (code, out) = run_rift_stdout(&repo, &["blame", "a.txt", "--line", "2", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let b: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    let span = &b["spans"][0];
    assert_eq!(span["attribution"]["agent_added"], s_agent, "{b}");
    assert_eq!(span["attribution"]["human_added"], s_human, "{b}");
    assert_eq!(span["attribution"]["confidence_milli"], s_conf, "{b}");
    assert_eq!(span["tag"], "ai", "two agent lines, no human edit: {b}");
}

/// **The populated pipeline, end to end, through the shipped binary** — the
/// stand-in for real usage, since no gate may wait on it. Two sequential agent
/// sessions with turn-boundary snapshots, a transcript-carrying prompt, agent
/// AND human edits, three real hook-pair commits.
///
/// The 2026-08-24 assessment measured `SUM(agent_added) = 0` over the owner's
/// real projection: turn-boundary snapshots existed for 2 of 61 historical
/// sessions and the enricher rightly refuses to invent anchors, so on real
/// data the verbs returned nothing and no test noticed. This fixture holds
/// the anchors PRESENT — the shape capture produces since the boundary hooks
/// landed — and asserts the pipeline POPULATES:
///
/// * `blame` answers agent AND human spans over one file whose history mixes
///   two sessions and a between-sessions human edit (the next session's
///   first boundary snapshot is what observes it);
/// * `why` resolves an agent line to its checkpoint, its session, and the
///   STORED redacted prompt (non-null, extracted, byte-served);
/// * `explain` affirms the same prompt non-null over the same shard;
/// * the stored attribution splits are non-zero on BOTH axes;
/// * every checkpoint row carries a real `created_at` (never the `-1`
///   sentinel) — the writer fix observed at the projection, through the
///   binary;
/// * and a COVERAGE RATIO holds at exact counts: of the fixture's 5 blame
///   spans, the 4 written under capture resolve to a known actor — the
///   pre-capture seed line stays honestly unknown — so the floor is stated
///   as 3/4, not as "> 0". (The assessment: a coverage-ratio test would have
///   caught 2/105.)
///
/// Building this fixture MEASURED a second root cause of "the verbs return
/// nothing on real data", distinct from the missing snapshots: the predicted
/// trailer is derived pre-commit from an archive carrying NO transcript,
/// while the landed shard of a transcript-carrying session includes its
/// transcript and digests in the content-addressed id — so on exactly those
/// sessions (every real Claude Code session reports `transcript_path`) the
/// trailer names an id that never lands, and a trailer-only claims walk
/// answered `unknown` for every line. `explain`'s commit-ish resolver had
/// always unioned the commit's claims with the catalogue's ANCHOR link;
/// blame/why now union the same way through `checkpoint_commit`, and this
/// test pins both halves: the claim still records what the commit says, the
/// selection resolves to the shard that exists.
#[test]
#[allow(clippy::too_many_lines)] // one linear production-like scenario
fn the_populated_pipeline_answers_blame_why_explain_with_a_coverage_floor() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let gd = repo.join(".git");

    // A real transcript for session 1, in the dialect shape the compactor
    // models; its prompt line is what `why`/`explain` must serve back.
    let transcript = tmp.path().join("s1.jsonl");
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","timestamp":"2026-08-01T00:00:00Z","message":{"role":"user","content":[{"type":"text","text":"add the parser feature"}]}}"#,
            "\n",
            r#"{"type":"assistant","timestamp":"2026-08-01T00:00:01Z","message":{"id":"msg_01MULTIA","role":"assistant","usage":{"input_tokens":64,"output_tokens":8},"content":[{"type":"text","text":"Writing the parser."}]}}"#,
            "\n",
        ),
    )
    .unwrap();

    // One real hook-pair commit: prepare-commit-msg (trailer prediction) →
    // `git commit` → post-commit (archive + projection ingest). Returns the
    // predicted checkpoint id the trailer carries.
    let hook_commit = |subject: &str| -> String {
        let msg = tmp.path().join(format!("MSG-{subject}"));
        std::fs::write(&msg, format!("{subject}\n")).unwrap();
        assert_eq!(
            run_rift(
                &repo,
                &[
                    "hooks",
                    "git",
                    "prepare-commit-msg",
                    "--git-dir",
                    gd.to_str().unwrap(),
                    "--",
                    msg.to_str().unwrap(),
                ],
                Some(""),
                &env,
            ),
            0
        );
        let predicted = std::fs::read_to_string(&msg)
            .unwrap()
            .lines()
            .find_map(|l| l.strip_prefix("Rift-Checkpoint: ").map(str::to_owned))
            .unwrap_or_else(|| panic!("no trailer injected for {subject}"));
        git_in(&repo, &["commit", "-q", "-F", msg.to_str().unwrap()]);
        assert_eq!(
            run_rift(
                &repo,
                &[
                    "hooks",
                    "git",
                    "post-commit",
                    "--git-dir",
                    gd.to_str().unwrap(),
                ],
                Some(""),
                &env,
            ),
            0
        );
        predicted
    };

    // ---- Session 1, turn 1: the agent writes parser.txt between boundaries.
    agent_hook(
        &repo,
        "UserPromptSubmit",
        "e2e-live-1",
        1,
        &format!(
            r#"{{"text":"add the parser feature","transcript_path":{}}}"#,
            json_str(&transcript)
        ),
        &env,
    );
    std::fs::write(
        repo.join("parser.txt"),
        "agent parser line one\nagent parser line two\nagent parser line three\n",
    )
    .unwrap();
    agent_hook(&repo, "Stop", "e2e-live-1", 1, "{}", &env);
    git_in(&repo, &["add", "parser.txt"]);
    let ckpt1 = hook_commit("agent parser work");

    // ---- Between turns, inside session 1, the human appends lines 4-5 to
    // parser.txt — the file the session has ALREADY committed once. Turn 2's
    // Start boundary observes the delta as the human window H_2.
    // (Until the replay fix landed, this fixture deliberately kept parser.txt
    // out of the second commit: a file committed mid-session replayed its
    // consumed hunks again and blamed `unknown`. Now it is the un-shaped
    // case this test exists to hold.)
    let mid_session = std::fs::read_to_string(repo.join("parser.txt")).unwrap()
        + "human mid-session line four\nhuman mid-session line five\n";
    std::fs::write(repo.join("parser.txt"), mid_session).unwrap();

    // ---- Session 1, turn 2: the agent writes util.txt; the commit carries
    // BOTH the agent's util.txt and the human's parser.txt delta.
    agent_hook(
        &repo,
        "UserPromptSubmit",
        "e2e-live-1",
        2,
        r#"{"text":"tidy the util"}"#,
        &env,
    );
    std::fs::write(
        repo.join("util.txt"),
        "agent util line one\nagent util line two\n",
    )
    .unwrap();
    agent_hook(&repo, "Stop", "e2e-live-1", 2, "{}", &env);
    git_in(&repo, &["add", "parser.txt", "util.txt"]);
    let ckpt2 = hook_commit("agent util work plus human parser lines");
    // MEASURED, then pinned: the predicted trailer is capture-addressed
    // (opted-in worktree bytes + read facts), and neither changed between
    // these commits — turn events are not capture bytes — so the two commits
    // PREDICT the same id. The landed shards still differ (the transcript
    // slice moved); the union below is what keeps that honest.
    assert_eq!(
        ckpt1, ckpt2,
        "unchanged capture predicts the same trailer id"
    );
    agent_hook(
        &repo,
        "SessionEnd",
        "e2e-live-1",
        2,
        r#"{"reason":"exit"}"#,
        &env,
    );

    // ---- The human edits BETWEEN SESSIONS — the commonest human shape: the
    // agent session ended, the developer keeps typing. Nothing observes the
    // keystrokes; the NEXT session's first prompt snapshot diffs the worktree
    // against HEAD and records the delta as the closing human window.
    let with_human = std::fs::read_to_string(repo.join("parser.txt")).unwrap()
        + "human refactor line six\nhuman refactor line seven\n";
    std::fs::write(repo.join("parser.txt"), with_human).unwrap();

    // ---- Session 2: its first boundary observes the human's parser delta;
    // its agent extends util.txt; one commit then carries BOTH — the mixed
    // commit real capture produces.
    agent_hook(
        &repo,
        "SessionStart",
        "e2e-live-2",
        0,
        r#"{"model":"claude-sonnet-4-5"}"#,
        &env,
    );
    agent_hook(
        &repo,
        "UserPromptSubmit",
        "e2e-live-2",
        1,
        r#"{"text":"extend the util"}"#,
        &env,
    );
    let extended =
        std::fs::read_to_string(repo.join("util.txt")).unwrap() + "agent util line three\n";
    std::fs::write(repo.join("util.txt"), extended).unwrap();
    agent_hook(&repo, "Stop", "e2e-live-2", 1, "{}", &env);
    git_in(&repo, &["add", "parser.txt", "util.txt"]);
    let ckpt3 = hook_commit("human parser refactor plus agent util extension");
    assert_ne!(ckpt2, ckpt3, "a different session is a different capture");
    agent_hook(
        &repo,
        "SessionEnd",
        "e2e-live-2",
        1,
        r#"{"reason":"exit"}"#,
        &env,
    );

    // ---- The query surface.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let blame = |file: &str| -> serde_json::Value {
        let (code, out) = run_rift_stdout(&repo, &["blame", file, "--json"], &env);
        assert_eq!(code, 0, "blame {file}: {out}");
        serde_json::from_str(&out).unwrap()
    };

    // BLAME: the mixed file answers agent AND human spans, in order, each
    // bound to the checkpoint whose commit wrote it.
    let parser_doc = blame("parser.txt");
    let spans = parser_doc["spans"].as_array().unwrap();
    let shape: Vec<(i64, i64, &str)> = spans
        .iter()
        .map(|s| {
            (
                s["start"].as_i64().unwrap(),
                s["end"].as_i64().unwrap(),
                s["tag"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        vec![(1, 3, "ai"), (4, 5, "human"), (6, 7, "human")],
        "{parser_doc}"
    );
    // THE DIVERGENCE, pinned where it lives: session 1 carried a transcript,
    // so its predicted trailer (derived pre-commit from an archive with no
    // transcript) names an id that never landed — the claim in the document
    // still records it, and the SELECTION resolves through the projection's
    // anchor link to the shard that really exists. Session 2 archived no
    // transcript, so its trailer and its shard agree and the union is a
    // no-op.
    let resolved1 = spans[0]["selection"]["checkpoint_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(
        resolved1, ckpt1,
        "a transcript-carrying session's trailer is not the landed shard: {parser_doc}"
    );
    assert_eq!(
        parser_doc["commits"][spans[0]["commit"].as_str().unwrap()]["claims"][0]["checkpoint_id"],
        ckpt1.as_str(),
        "…while the document still records what the commit claims: {parser_doc}"
    );
    assert_eq!(
        spans[2]["selection"]["checkpoint_id"],
        ckpt3.as_str(),
        "a transcript-less session's trailer IS the landed shard: {parser_doc}"
    );

    // The sessions resolve on their own spans — the agent's lines to session
    // 1's first shard, the mid-session human lines to session 1's SECOND
    // shard (a file committed mid-session replays only the
    // hunks after its parent, so those two lines are credited to the human
    // instead of degrading the file to unknown), the between-sessions human
    // window to the session that OBSERVED it.
    let sid1 = rift_core::capture_session_row_id("claude-code", "e2e-live-1");
    let sid2 = rift_core::capture_session_row_id("claude-code", "e2e-live-2");
    assert_eq!(spans[0]["sessions"][0]["session_id"], sid1, "{parser_doc}");
    assert_eq!(spans[1]["sessions"][0]["session_id"], sid1, "{parser_doc}");
    assert_ne!(
        spans[1]["selection"]["checkpoint_id"], resolved1,
        "the mid-session lines resolve to session 1's second shard: {parser_doc}"
    );
    assert_eq!(spans[1]["attribution"]["human_added"], 2, "{parser_doc}");
    assert!(
        spans[1]["attribution"]["confidence_milli"]
            .as_i64()
            .is_some_and(|c| c > 0),
        "a replayed human window carries confidence: {parser_doc}"
    );
    assert_eq!(spans[2]["sessions"][0]["session_id"], sid2, "{parser_doc}");

    let util_doc = blame("util.txt");
    let uspans = util_doc["spans"].as_array().unwrap();
    let ushape: Vec<(i64, i64, &str)> = uspans
        .iter()
        .map(|s| {
            (
                s["start"].as_i64().unwrap(),
                s["end"].as_i64().unwrap(),
                s["tag"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(ushape, vec![(1, 2, "ai"), (3, 3, "ai")], "{util_doc}");
    assert_eq!(uspans[0]["sessions"][0]["session_id"], sid1, "{util_doc}");
    assert_eq!(uspans[1]["sessions"][0]["session_id"], sid2, "{util_doc}");
    let seed_doc = blame("a.txt");
    assert_eq!(seed_doc["spans"].as_array().unwrap().len(), 1, "{seed_doc}");
    assert_eq!(
        seed_doc["spans"][0]["tag"], "unknown",
        "the pre-capture seed line stays honestly unknown: {seed_doc}"
    );

    // COVERAGE RATIO, at exact counts: 6 spans across the fixture's three
    // files, 5 of them — every line written under capture — resolve to a
    // known actor. Floor stated as a fraction, not as "> 0": had the
    // pipeline lost its anchors (the 2-of-61 shape), this reads 1/6 and reds.
    let all_spans: Vec<&serde_json::Value> = [&parser_doc, &util_doc, &seed_doc]
        .iter()
        .flat_map(|d| d["spans"].as_array().unwrap())
        .collect();
    let known = all_spans.iter().filter(|s| s["tag"] != "unknown").count();
    assert_eq!((all_spans.len(), known), (6, 5));
    assert!(
        known * 4 >= all_spans.len() * 3,
        "coverage floor 3/4 broken: {known} of {} spans resolve",
        all_spans.len()
    );

    // WHY: an agent line resolves to its checkpoint, its session, and the
    // STORED redacted prompt — non-null, extracted, and byte-served from the
    // shard the archive wrote.
    let (code, out) = run_rift_stdout(&repo, &["why", "parser.txt:2", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let why_ai: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(why_ai["tag"], "ai", "{why_ai}");
    assert_eq!(
        why_ai["selection"]["checkpoint_id"],
        resolved1.as_str(),
        "{why_ai}"
    );
    assert_eq!(why_ai["sessions"][0]["session_id"], sid1, "{why_ai}");
    assert_eq!(
        why_ai["prompt"]["text"], "add the parser feature",
        "{why_ai}"
    );
    assert_eq!(why_ai["prompt"]["extracted"], true, "{why_ai}");

    // …and a HUMAN line tells its own story: the session that observed the
    // edit, the human tag, and — because that session archived no transcript
    // — an honest null prompt rather than a guess.
    let (code, out) = run_rift_stdout(&repo, &["why", "parser.txt:6", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let w4: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(w4["tag"], "human", "{w4}");
    assert_eq!(w4["selection"]["checkpoint_id"], ckpt3.as_str(), "{w4}");
    assert_eq!(w4["sessions"][0]["session_id"], sid2, "{w4}");
    assert_eq!(w4["prompt"], serde_json::Value::Null, "{w4}");

    // EXPLAIN affirms with the same non-null redacted prompt over the shard
    // blame resolved — the id that exists, not the phantom the trailer names.
    let (code, out) = run_rift_stdout(
        &repo,
        &["checkpoint", "explain", &resolved1, "--json"],
        &env,
    );
    assert_eq!(code, 0, "{out}");
    let explain_doc: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(
        explain_doc["checkpoint_id"],
        resolved1.as_str(),
        "{explain_doc}"
    );
    assert_eq!(
        explain_doc["prompt"]["text"], "add the parser feature",
        "{explain_doc}"
    );
    assert!(
        explain_doc["created_at_ms"]
            .as_i64()
            .is_some_and(|ms| ms > 0),
        "explain must surface the real capture time, not the sentinel: {explain_doc}"
    );

    // The stored splits are non-zero on BOTH axes — the assessment's finding
    // was SUM(agent_added) = 0 on real data; this fixture is the counter-
    // proof through the same tables.
    let repo_id = rift_git::repo_id_for(&repo).unwrap();
    let db = rift_store::projection_db_under(&cache, &repo_id);
    let store = rift_store::open_capture_projection(&db).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    use rift_store::CanonicalStore as _;
    let sums = rt
        .block_on(store.query(
            "SELECT COALESCE(SUM(agent_added),0), COALESCE(SUM(human_added),0) FROM attribution",
            &[],
        ))
        .unwrap();
    let (agent_sum, human_sum) = (sums[0].get_i64(0).unwrap(), sums[0].get_i64(1).unwrap());
    assert!(
        agent_sum >= 6,
        "agent lines across both sessions: {agent_sum}"
    );
    assert!(
        human_sum >= 4,
        "the mid-session and between-sessions human lines: {human_sum}"
    );

    // Every checkpoint row carries the capture event's own time — the writer
    // fix observed at the projection layer, through the binary.
    let times = rt
        .block_on(store.query(
            "SELECT COUNT(*), COALESCE(SUM(created_at > 0),0) FROM checkpoint",
            &[],
        ))
        .unwrap();
    let (rows, dated) = (times[0].get_i64(0).unwrap(), times[0].get_i64(1).unwrap());
    assert!(rows >= 3, "three hook-pair commits archived: {rows}");
    assert_eq!(
        dated, rows,
        "a native checkpoint row still carries the -1 sentinel"
    );

    // DOCTOR's per-session turn-boundary coverage (2026-08-29): session 1
    // fired two prompts and two Stops, and every one landed as its own
    // boundary snapshot — the count that, at zero, is the blame-unknown
    // signature the register documents.
    let (code, out) = run_rift_stdout(&repo, &["doctor", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let d: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    let s1 = d["turn_boundaries"]["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["session_id"] == sid1)
        .unwrap_or_else(|| panic!("session 1 missing from doctor: {d}"));
    assert_eq!(
        (s1["starts"].as_i64(), s1["ends"].as_i64()),
        (Some(2), Some(2)),
        "{d}"
    );
}

/// The read-facts chain: a read-shaped PostToolUse hook captures the
/// path + content hash durably, and the next post-commit archive freezes them
/// into the shard's `tool_calls.jsonl` — the artifact the recall plane ingests
/// as `n_file_read`. Without this, memory is empty by construction.
#[test]
#[allow(clippy::too_many_lines)] // one linear e2e scenario: hook -> commit -> shard -> ingest
fn captured_read_facts_reach_the_checkpoint_shard() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env: Vec<(&str, &str)> = vec![
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // The agent reads a repo file.
    agent_read_hook(&repo, "e2e-live-1", 1, &repo.join("a.txt"), &env);

    // Commit → the archive freezes the session's read facts into the shard.
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
    assert_eq!(code, 0, "post-commit ok; stderr: {stderr}");

    let tree = Command::new("git")
        .args([
            "ls-tree",
            "-r",
            "--name-only",
            "refs/heads/rift/checkpoints/v1",
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    let names = String::from_utf8_lossy(&tree.stdout).into_owned();
    assert!(
        names.contains("tool_calls.jsonl"),
        "the shard carries the read facts: {names}"
    );
    let shard_path = names
        .lines()
        .find(|l| l.ends_with("tool_calls.jsonl"))
        .unwrap();
    let body = Command::new("git")
        .args([
            "cat-file",
            "-p",
            &format!("refs/heads/rift/checkpoints/v1:{shard_path}"),
        ])
        .current_dir(&repo)
        .output()
        .unwrap();
    let jsonl = String::from_utf8_lossy(&body.stdout).into_owned();
    assert!(
        jsonl.contains(r#""path":"a.txt""#) && jsonl.contains(r#""tool":"Read""#),
        "the read fact names the repo-relative path: {jsonl}"
    );
    // The recorded hash is the read-time content hash (sha256 of "one\n").
    assert!(
        jsonl.contains("2c8b08da5ce60398e1f19af0e5dccc744df274b826abe585eaba68c525434806"),
        "content_sha256 matches the bytes: {jsonl}"
    );

    // The recall plane ingests the shard: cost-hotspots shows the REAL read
    // (memory is no longer empty by construction), and a second run is a
    // watermark no-op (same answer, no error).
    for pass in 0..2 {
        let out = rift_cmd(&repo)
            .args(["memory", "cost-hotspots"])
            .envs(
                env.iter()
                    .copied()
                    .map(|(k, v)| (k.to_owned(), v.to_owned())),
            )
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success(), "cost-hotspots pass {pass}: {out:?}");
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            text.contains("a.txt"),
            "pass {pass}: the captured read is a hotspot: {text}"
        );
        assert!(
            !text.contains("no captured reads"),
            "pass {pass}: the empty-state is gone: {text}"
        );
    }
}

/// The detach itself, on the DEFAULT path — no `RIFT_CAPTURE_SYNC`.
///
/// Every other end-to-end test here drains inline for determinism, which would
/// leave the shipped behaviour — post-commit queues, spawns a child and returns
/// before the archive exists — with no test at all. This one takes the real
/// path: the hook returns while the queue still holds its entry, and the shard
/// appears afterwards, written by a process this one only started.
///
/// It polls rather than asserting a duration. How long the child takes is a
/// property of the machine; that it finishes, drains its entry and leaves a
/// shard is a property of the code.
#[test]
fn the_detached_worker_archives_after_the_hook_has_already_returned() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let queue = repo.join(".git").join("rift-capture-queue");

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
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
        ],
    );
    assert_eq!(code, 0, "the hook exits 0; stderr: {stderr}");
    assert!(queue.is_dir(), "the hook left a durable record of intent");

    let shard_exists = || {
        std::process::Command::new("git")
            .args(["rev-parse", "--verify", "refs/heads/rift/checkpoints/v1"])
            .current_dir(&repo)
            .output()
            .is_ok_and(|o| o.status.success())
    };
    // Poll for BOTH facts, not just the shard.
    //
    // The shard ref appearing does not mean the worker is finished: it still
    // records the checkpoint's chain position and runs the soak's verify before
    // it drains. Measured on this machine, the shard ref lands 169 ms before
    // the drain does (0 ms before the chain step existed), so asserting the
    // drain the instant the ref appears was asserting a duration after all —
    // the one thing this test's own docstring says it must not do.
    let entries_left = || {
        std::fs::read_dir(&queue)
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name())
                    .filter(|n| n.to_string_lossy().ends_with(".json"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    while std::time::Instant::now() < deadline && !(shard_exists() && entries_left().is_empty()) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(
        shard_exists(),
        "the detached worker archived the checkpoint"
    );
    assert!(
        entries_left().is_empty(),
        "and drained its entry: {:?}",
        entries_left()
    );
}

/// The installed Unix entry is a direct symlink to Rift. Its detached child
/// must resolve that link before spawning `hooks worker`: otherwise argv0 is
/// still `post-commit`, the child re-enters the hook dispatcher, and the queue
/// can never drain.
#[cfg(unix)]
#[test]
fn a_direct_hook_link_spawns_the_worker_binary_not_another_hook() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let git_dir = repo.join(".git");
    let cache = tmp.path().join("cache");
    assert_eq!(
        run_rift(
            &repo,
            &["hook", "install", "--git-dir", git_dir.to_str().unwrap()],
            None,
            &[
                ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
                ("RIFT_NO_KEYCHAIN", "1"),
            ],
        ),
        0
    );
    let hook = git_dir.join("hooks/post-commit");
    assert!(std::fs::read_link(&hook).is_ok(), "the entry is a symlink");

    let status = hook_command(&hook)
        .current_dir(&repo)
        .env("GIT_DIR", &git_dir)
        .env("RIFT_CACHE_DIR", &cache)
        .env("RIFT_NO_KEYCHAIN", "1")
        .env("RIFT_NO_PROVISION", "1")
        .env_remove("RIFT_CAPTURE_SYNC")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0), "the direct hook never blocks git");

    let queue = git_dir.join("rift-capture-queue");
    let entries_left = || {
        std::fs::read_dir(&queue)
            .map(|d| {
                d.flatten()
                    .filter(|entry| entry.path().extension().is_some_and(|x| x == "json"))
                    .count()
            })
            .unwrap_or_default()
    };
    let shard_exists = || {
        Command::new("git")
            .args(["rev-parse", "--verify", "refs/heads/rift/checkpoints/v1"])
            .current_dir(&repo)
            .output()
            .is_ok_and(|output| output.status.success())
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    while std::time::Instant::now() < deadline && !(shard_exists() && entries_left() == 0) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(shard_exists(), "the direct-link worker archived the commit");
    assert_eq!(
        entries_left(),
        0,
        "the direct-link worker drained its queue"
    );
}

#[test]
fn hook_install_uses_absolute_binary_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let gd = git_dir(tmp.path());
    let cache = tmp.path().join("cache");
    let code = run_rift(
        tmp.path(),
        &["hook", "install", "--git-dir", gd.to_str().unwrap()],
        None,
        // Hermetic: install now warms the projection and mints the salt, so an
        // empty env would write into the developer's real cache root and their
        // real keychain. A test must not touch either.
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
        ],
    );
    assert_eq!(code, 0, "install exits 0");

    for name in [
        "post-commit",
        "pre-push",
        "prepare-commit-msg",
        "commit-msg",
    ] {
        let entry = gd.join("hooks").join(name);
        assert!(entry.exists(), "{name} hook entry installed");
        #[cfg(unix)]
        {
            assert_eq!(
                std::fs::read_link(&entry).unwrap(),
                Path::new(common::rift_bin()),
                "{name} links directly to the absolute Rift binary"
            );
            let marker = gd.join("hooks").join(format!("{name}.rift-managed-link"));
            let record = std::fs::read_to_string(marker).unwrap();
            assert!(record.contains("# Rift-managed hook"));
            assert!(!record.to_ascii_lowercase().contains(RETIRED_BRAND));
        }
        #[cfg(not(unix))]
        {
            let text = std::fs::read_to_string(&entry).unwrap();
            assert!(
                text.contains(common::rift_bin()),
                "{name} shim calls rift by absolute path"
            );
            assert!(text.contains(&format!("hooks git {name} ")));
            assert!(!text.to_ascii_lowercase().contains(RETIRED_BRAND));
        }
    }
}

#[test]
fn pre_push_rejects_rift_refs_only() {
    let tmp = tempfile::tempdir().unwrap();
    let normal = "refs/heads/main abc refs/heads/main def\n";
    assert_eq!(
        run_rift(tmp.path(), &["hooks", "git", "pre-push"], Some(normal), &[]),
        0
    );

    let rift_ref = "refs/heads/rift/x abc refs/heads/rift/x def\n";
    assert_eq!(
        run_rift(
            tmp.path(),
            &["hooks", "git", "pre-push"],
            Some(rift_ref),
            &[]
        ),
        1,
        "a rift/* ref is rejected"
    );

    // a mixed push fails closed on the rift/* presence
    let mixed = format!("{normal}{rift_ref}");
    assert_eq!(
        run_rift(tmp.path(), &["hooks", "git", "pre-push"], Some(&mixed), &[]),
        1
    );
}

#[test]
fn commit_path_hook_never_blocks_even_on_panic() {
    let tmp = tempfile::tempdir().unwrap();
    // The injected capture panic must be swallowed; git sees success.
    let code = run_rift(
        tmp.path(),
        &["hooks", "git", "post-commit"],
        Some(""),
        &[("RIFT_HOOK_TEST_PANIC", "1")],
    );
    assert_eq!(code, 0, "post-commit exits 0 even when capture panics");
}

#[test]
fn agent_hook_never_fails_agent_on_bad_input() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    // Malformed stdin → swallowed, exit 0 (deferred). Cache redirected to a tempdir.
    let code = run_rift(
        tmp.path(),
        &["hooks", "claude-code", "SessionStart"],
        Some("not json at all"),
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    assert_eq!(code, 0, "a bad payload never fails the agent");

    // A valid SessionStart is recorded (exit 0).
    let good = r#"{"schema":1,"session":"01JE2E","turn":0,"payload":{}}"#;
    let code2 = run_rift(
        tmp.path(),
        &["hooks", "claude-code", "SessionStart"],
        Some(good),
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    assert_eq!(code2, 0);
}

/// **The leak mechanism, pinned in a THROWAWAY repo.** An
/// agent hook resolves its repository from the process cwd, and the session
/// journal lives under the DISCOVERED git dir — so a `RIFT_CACHE_DIR`
/// redirect alone never isolated it. This is the exact spawn shape that wrote
/// fixture session `01JE2E` into the real repository's journal (whence the
/// real post-commit hook archived it as shard `51/4865…`), aimed here at a
/// tempdir repo: the journal entry must land under the REPO's git dir, and
/// nothing session-named may land under the cache root.
#[test]
fn the_session_journal_follows_the_discovered_repo_not_the_cache_redirect() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q"]);
    let cache = tmp.path().join("cache");

    let body = r#"{"schema":1,"session":"01JRNLPIN","turn":0,"payload":{}}"#;
    let code = run_rift_in(
        &repo,
        &["hooks", "claude-code", "SessionStart"],
        body,
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    assert_eq!(code, 0);

    assert!(
        repo.join(".git")
            .join("rift-sessions")
            .join("01JRNLPIN.json")
            .exists(),
        "the journal entry follows the discovered repo's git dir"
    );
    fn names_under(dir: &Path, needle: &str, hits: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                names_under(&p, needle, hits);
            } else if p
                .file_name()
                .is_some_and(|n| n.to_string_lossy().contains(needle))
            {
                hits.push(p);
            }
        }
    }
    let mut cache_hits = Vec::new();
    names_under(&cache, "01JRNLPIN", &mut cache_hits);
    assert!(
        cache_hits.is_empty(),
        "the cache redirect does not receive the journal — cwd isolation is \
         what isolates it, which is why every spawn pins a hermetic cwd: \
         {cache_hits:?}"
    );
}

#[cfg(unix)]
/// `hook install` creates what capture would otherwise create lazily on the
/// developer's first commit: the projection and the install salt.
///
/// The point is the *first commit*, not the install — so the test asserts the
/// artefacts exist under the isolated cache root after install and before any
/// hook has run, which is the only moment at which "warm" and "lazy" differ.
#[test]
fn install_warms_the_projection_and_the_salt_so_the_first_commit_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let gd = git_dir(tmp.path());
    let cache = tmp.path().join("cache");
    let code = run_rift(
        tmp.path(),
        &["hook", "install", "--git-dir", gd.to_str().unwrap()],
        None,
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
        ],
    );
    assert_eq!(code, 0, "install exits 0");

    assert!(
        cache.join("install-salt").exists(),
        "the salt was minted at install, not at the first commit"
    );
    // The machine id's one derivation (an `ioreg` child on macOS) is paid
    // here too, so a session's first hook reads the salted digest instead.
    if cfg!(target_os = "macos") {
        assert!(
            cache.join("machine-id.json").exists(),
            "the machine id was derived and cached at install"
        );
    }
    // `<cache>/rift/<repo-id>/projection.db` — the repo id is a hash of the
    // canonical worktree path, so the test finds it by shape, not by spelling.
    let projections: Vec<_> = std::fs::read_dir(cache.join("rift"))
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("projection.db").exists())
        .collect();
    assert_eq!(
        projections.len(),
        1,
        "exactly one projection was created at install"
    );
}

#[test]
fn gui_sparse_path_still_runs_installed_hook_via_absolute_path() {
    let tmp = tempfile::tempdir().unwrap();
    let gd = git_dir(tmp.path());
    let cache = tmp.path().join("cache");
    assert_eq!(
        run_rift(
            tmp.path(),
            &["hook", "install", "--git-dir", gd.to_str().unwrap()],
            None,
            // Hermetic, for the same reason as above.
            &[
                ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
                ("RIFT_NO_KEYCHAIN", "1"),
            ]
        ),
        0
    );
    // Execute the installed entry directly with an EMPTY PATH (a GUI client's
    // sparse environment). It must still reach Rift via its absolute path.
    let hook = gd.join("hooks").join("post-commit");
    let status = hook_command(&hook)
        // `rift_cmd` cannot spawn a hook path — pin its cwd to the same hermetic
        // root by hand (the same leak class: an unpinned hook child discovers
        // whatever repository the runner sits in).
        .current_dir(tmp.path())
        .env("PATH", "")
        .env("GIT_DIR", &gd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert_eq!(
        status.code(),
        Some(0),
        "hook runs rift by absolute path under sparse PATH"
    );
}

/// The command that runs an installed hook shim the way git runs it.
///
/// On Unix the shim is executable and runs itself. On Windows there is no
/// execute bit and the installer writes the same `#!/bin/sh` shim (see
/// `rift-shadow-hooks::install`), which git runs through its bundled `sh`;
/// spawning the file directly is error 193. The shell is resolved here, in
/// the test process, before the child's `PATH` is scrubbed — so the property
/// under test (the shim reaches rift by absolute path with an empty `PATH`)
/// is the same on both platforms.
fn hook_command(hook: &Path) -> Command {
    #[cfg(windows)]
    {
        let mut cmd = Command::new(bundled_sh());
        cmd.arg(hook.display().to_string().replace('\\', "/"));
        cmd
    }
    #[cfg(not(windows))]
    {
        Command::new(hook)
    }
}

/// Git for Windows' own `sh.exe`, found from `git --exec-path`
/// (`<root>/mingw64/libexec/git-core`): `<root>/usr/bin/sh.exe`, or the
/// `<root>/bin/sh.exe` launcher. The same lookup lives beside the document
/// helper's stand-in in `rift-recall-doc`; the two crates share no test edge.
#[cfg(windows)]
fn bundled_sh() -> std::path::PathBuf {
    let out = Command::new("git")
        .arg("--exec-path")
        .output()
        .expect("git is on PATH");
    let exec_path = std::path::PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    let root = exec_path
        .ancestors()
        .nth(3)
        .expect("exec-path is three levels under the install root")
        .to_path_buf();
    [root.join("usr").join("bin"), root.join("bin")]
        .into_iter()
        .map(|dir| dir.join("sh.exe"))
        .find(|sh| sh.is_file())
        .expect("git for Windows ships sh.exe")
}
// --- linked worktrees --------------------------------------------------------

/// A hermetic `git` invocation inside `dir`, returning its stdout — user/system
/// config off, a fixed identity, and every inherited `GIT_*` re-targeting
/// variable scrubbed, so the developer's own environment cannot steer a fixture.
/// A failing command panics with git's own stderr, so a broken fixture names
/// itself instead of surfacing as an assertion three steps later.
fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_OBJECT_DIRECTORY")
        .env_remove("GIT_ALTERNATE_OBJECT_DIRECTORIES")
        .output()
        .expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Run `rift <args>` **from `cwd`**; return the exit code and stdout. The soak
/// assertions read `doctor --json`, so they need the data channel, not just the
/// code.
fn run_rift_capture_in(
    cwd: &Path,
    args: &[&str],
    stdin: &str,
    extra_env: &[(&str, &str)],
) -> (i32, String) {
    let mut cmd = rift_cmd(cwd);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Run `rift <args>` **from `cwd`** with stdin; return the exit code. The
/// agent-hook surface reads its repository from the process working directory,
/// which is exactly what this regression is about.
fn run_rift_in(cwd: &Path, args: &[&str], stdin: &str, extra_env: &[(&str, &str)]) -> i32 {
    let mut cmd = rift_cmd(cwd);
    cmd.args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child
        .wait_with_output()
        .unwrap()
        .status
        .code()
        .unwrap_or(-1)
}

/// The captured hook receipts in the **family's** projection, resolved from
/// `member` exactly as the product resolves it.
///
/// [`rift_git::MintPolicy::Never`]: the test reads the identity the product
/// minted, and would rather see an empty list than mint one of its own and
/// assert against a store nothing wrote to.
async fn family_hook_events(cache: &Path, member: &Path) -> Vec<String> {
    let (_, state) = rift_git::family_of(member, rift_git::MintPolicy::Never).unwrap();
    let rift_git::FamilyState::Minted(family) = state else {
        return Vec::new();
    };
    let db = rift_store::projection_db_for_family_under(cache, &family);
    if !db.exists() {
        return Vec::new();
    }
    let store = rift_store::open_capture_projection(db).unwrap();
    let rows = rift_store::CanonicalStore::query(
        &store,
        "SELECT session_id FROM agent_hook_event ORDER BY session_id",
        &[],
    )
    .await
    .unwrap();
    rows.iter().map(|r| r.get_text(0).unwrap()).collect()
}

/// How many archive decisions the **family's** projection holds, read from
/// `member`'s vantage. Zero when nothing has minted the family or the store yet.
fn archived_in_family(cache: &Path, member: &Path) -> i64 {
    let Ok((_, rift_git::FamilyState::Minted(family))) =
        rift_git::family_of(member, rift_git::MintPolicy::Never)
    else {
        return 0;
    };
    let db = rift_store::projection_db_for_family_under(cache, &family);
    if !db.exists() {
        return 0;
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        let store = rift_store::open_capture_projection(db).unwrap();
        rift_store::CanonicalStore::query_opt_i64(
            &store,
            "SELECT COUNT(*) FROM archive_decision WHERE archived = 1",
            &[],
        )
        .await
        .unwrap()
        .unwrap_or(0)
    })
}

/// The projection a checkout would have owned under the retired path rule —
/// the per-path derivation the family model replaced. Used only to assert that nothing
/// creates one any more.
fn per_path_projection(cache: &Path, work_tree: &Path) -> std::path::PathBuf {
    rift_store::projection_db_under(cache, &rift_git::repo_id_for(work_tree).unwrap())
}

/// The soak union counts the family's projection **once**, whatever the
/// worktree count.
///
/// `doctor`'s soak reads "the repository's evidence, not this checkout's", which
/// before the family mint meant unioning one projection per working tree. Members now
/// resolve one file, so a union that still deduped by the member's own path
/// would open that file once per worktree and multiply every count in the soak
/// by the number of checkouts that happen to exist. Stated through the real
/// binary, from the member — the vantage whose lexical parent used to resolve to
/// `…/.git/worktrees`.
#[test]
fn the_soak_union_counts_one_family_projection_once() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    let nohooks = tmp.path().join("nohooks");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(&nohooks).unwrap();
    git_in(&main, &["init", "-q", "."]);
    git_in(&main, &["config", "user.email", "t@t"]);
    git_in(&main, &["config", "user.name", "T"]);
    git_in(
        &main,
        &["config", "core.hooksPath", nohooks.to_str().unwrap()],
    );
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    git_in(&main, &["add", "a.txt"]);
    git_in(&main, &["commit", "-q", "-m", "seed"]);
    for name in ["wt1", "wt2"] {
        let member = tmp.path().join(name);
        git_in(
            &main,
            &[
                "worktree",
                "add",
                "-q",
                member.to_str().unwrap(),
                "-b",
                name,
            ],
        );
    }

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    // A hook creates the family's projection; the members then have one to find.
    assert_eq!(
        run_rift_in(
            &main,
            &["hooks", "claude-code", "SessionStart"],
            r#"{"session_id":"01SOAKSESSION","hook_event_name":"SessionStart"}"#,
            &env,
        ),
        0
    );

    for vantage in [&main, &tmp.path().join("wt1")] {
        let (code, out) = run_rift_stdout(vantage, &["--json", "doctor"], &env);
        assert!(code == 0 || code == 1, "doctor answers: {out}");
        let doc: serde_json::Value = serde_json::from_str(&out).expect("doctor --json");
        assert_eq!(
            doc["soak"]["projections"], 1,
            "three worktrees, one family projection, unioned once: {out}"
        );
    }
}

/// **`session.repo_id` is the family id**, for every new row,
/// whichever member the session was worked in.
///
/// The value and the file are two separate decisions, and getting only one of
/// them right is worse than getting neither: rows written under a member's own
/// path hash into the family's projection are invisible to every reader that
/// scopes by `repo_id`, which is all of them. So the assertion is stated
/// against the id the member's path *would* have produced, not merely against
/// "some id".
///
/// Historical rows keep whatever they were written with. Both values name the
/// same database after adoption, so no reader changes meaning — which is why
/// this needs no migration.
#[tokio::test]
async fn session_rows_carry_the_family_id() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    let nohooks = tmp.path().join("nohooks");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(&nohooks).unwrap();
    git_in(&main, &["init", "-q", "."]);
    git_in(&main, &["config", "user.email", "t@t"]);
    git_in(&main, &["config", "user.name", "T"]);
    git_in(
        &main,
        &["config", "core.hooksPath", nohooks.to_str().unwrap()],
    );
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    git_in(&main, &["add", "a.txt"]);
    git_in(&main, &["commit", "-q", "-m", "seed"]);
    let linked = tmp.path().join("linked");
    git_in(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "wt",
        ],
    );

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let body = r#"{"session_id":"01MEMBERSESSION","hook_event_name":"SessionStart"}"#;
    assert_eq!(
        run_rift_in(
            &linked,
            &["hooks", "claude-code", "SessionStart"],
            body,
            &env
        ),
        0
    );

    let (_, state) = rift_git::family_of(&linked, rift_git::MintPolicy::Never).unwrap();
    let rift_git::FamilyState::Minted(family) = state else {
        panic!("the hook minted the family it captured into");
    };
    let store = rift_store::open_capture_projection(rift_store::projection_db_for_family_under(
        &cache, &family,
    ))
    .unwrap();
    let rows =
        rift_store::CanonicalStore::query(&store, "SELECT DISTINCT repo_id FROM session", &[])
            .await
            .unwrap();
    let ids: Vec<String> = rows.iter().map(|r| r.get_text(0).unwrap()).collect();
    assert_eq!(
        ids,
        vec![family.family_id.as_str().to_owned()],
        "every session row carries the family id"
    );
    assert_ne!(
        family.family_id.as_str(),
        rift_git::repo_id_for(&linked).unwrap().as_str(),
        "the member's own path hashes to something else — which is what makes \
         the assertion above worth making"
    );
}

/// **The serve-audit sink writes the family's `recall.db`** — from a
/// subdirectory, and from a linked member — never a store keyed by whatever
/// directory the hook happened to be standing in.
///
/// `rift hooks memd <verb>` used to resolve its store from the raw process
/// cwd. A serve audited from `<repo>/crates/x` therefore landed in a
/// `recall.db` keyed by `<repo>/crates/x`, while `rift recall <verb>` served
/// out of the one keyed by `<repo>` — writer and reader on different files,
/// which is what stranded the savings panel on "no serves yet" while real
/// serves were being made. The cwd is now resolved to its work tree and that
/// work tree to its family.
#[tokio::test]
async fn the_memd_audit_sink_writes_the_family_recall_db_from_a_subdirectory() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    let main_git = rift_git::test_support::fake_repo(&main).unwrap();
    let deep = main.join("crates").join("x");
    std::fs::create_dir_all(&deep).unwrap();
    let member = tmp.path().join("member");
    rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();
    let member_sub = member.join("bins");
    std::fs::create_dir_all(&member_sub).unwrap();

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];
    let envelope = |key: &str| {
        format!(
            r#"{{"schema":1,"idempotency_key":"{key}","verb":"memd.known_state","payload":{{"handle":"h","estimated_tokens_returned":7,"estimated_tokens_avoided":11}}}}"#
        )
    };
    for (cwd, key) in [(&deep, "from-subdir"), (&member_sub, "from-member")] {
        assert_eq!(
            run_rift_in(cwd, &["hooks", "memd", "known-state"], &envelope(key), &env),
            0,
            "an audit-write never disturbs the serve path"
        );
    }

    // One store, and it is the family's — which for a main work tree is the
    // file the recall id has always named, adopted in place.
    let family_db =
        rift_store::recall_db_under(&cache, &rift_store::recall_repo_id(&main).unwrap());
    let store = rift_store::open_migrated(&family_db, &rift_store::recall_migrations()).unwrap();
    let rows = rift_store::CanonicalStore::query(
        &store,
        "SELECT audit_id FROM memd_audit_event ORDER BY audit_id",
        &[],
    )
    .await
    .unwrap();
    let got: Vec<String> = rows.iter().map(|r| r.get_text(0).unwrap()).collect();
    assert_eq!(
        got,
        vec!["from-member".to_string(), "from-subdir".to_string()],
        "both serves are audited into the one family store"
    );

    // And no store was keyed by a directory that is not a repository root.
    for stray in [&deep, &member_sub, &member] {
        let per_path =
            rift_store::recall_db_under(&cache, &rift_store::recall_repo_id(stray).unwrap());
        assert!(
            !per_path.exists(),
            "no recall.db may be minted for {}",
            stray.display()
        );
    }
}

/// **The whole point of the family model, through the real binary.** A hook fired in a
/// linked worktree lands in the family's projection — the one the main work
/// tree already owns — and the member gets no second, empty store of its own.
///
/// Two defects meet here. The first silenced capture in every linked worktree
/// of this repository: a linked worktree's `.git` is a FILE
/// (`gitdir: <main>/.git/worktrees/<name>`), so deriving `<cwd>/.git` as a
/// directory made every session read fail `ENOTDIR` — `NotADirectory`, not
/// `NotFound` — and ingest aborted *before* writing its receipt, exiting 0.
/// The second is the one the family identity closes: even once the hook captured, it
/// captured into a projection keyed by the member's own path, so from
/// `~/rift-worktrees/canopy` the binary reported 0 checkpoints while the
/// family's refs held 134.
///
/// The assertion that makes this load-bearing is the *negative* one: no
/// per-path projection exists for the member afterwards. Counting the member's
/// own rows would pass under either rule.
#[tokio::test]
async fn an_agent_hook_fired_in_a_linked_worktree_lands_in_the_family_projection() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("main");
    let nohooks = tmp.path().join("nohooks");
    std::fs::create_dir_all(&main).unwrap();
    std::fs::create_dir_all(&nohooks).unwrap();
    git_in(&main, &["init", "-q", "."]);
    git_in(&main, &["config", "user.email", "t@t"]);
    git_in(&main, &["config", "user.name", "T"]);
    git_in(
        &main,
        &["config", "core.hooksPath", nohooks.to_str().unwrap()],
    );
    std::fs::write(main.join("a.txt"), "one\n").unwrap();
    git_in(&main, &["add", "a.txt"]);
    git_in(&main, &["commit", "-q", "-m", "seed"]);
    let linked = tmp.path().join("linked");
    git_in(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "wt",
        ],
    );
    assert!(linked.join(".git").is_file(), "a linked .git is a file");

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let hook = |cwd: &Path, session: &str| {
        let body = format!(r#"{{"session_id":"{session}","hook_event_name":"SessionStart"}}"#);
        assert_eq!(
            run_rift_in(cwd, &["hooks", "claude-code", "SessionStart"], &body, &env),
            0,
            "a capture hook never fails the agent"
        );
    };
    hook(&main, "01MAINSESSION");
    hook(&linked, "01LINKEDSESSION");

    // One store, both members: the session worked in the linked worktree and
    // the session worked in main are in the SAME projection, and the family is
    // reachable from either door.
    let from_linked = family_hook_events(&cache, &linked).await;
    let from_main = family_hook_events(&cache, &main).await;
    assert_eq!(
        from_linked,
        vec!["01LINKEDSESSION".to_string(), "01MAINSESSION".to_string()],
        "a hook fired in a linked worktree lands in the family projection, \
         beside the main work tree's"
    );
    assert_eq!(
        from_linked, from_main,
        "either member's vantage resolves the one store"
    );

    // The negative half, and the one that can only pass under the new rule:
    // the member has no projection of its own. Nothing was minted for
    // `~/…/linked`, so nothing can be stranded in it.
    assert!(
        !per_path_projection(&cache, &linked).exists(),
        "a member must not mint a second projection keyed by its own path"
    );
    // And adoption is in place: the family's file is exactly the one the main
    // work tree already owned — no row moved, no rebuild.
    let (_, state) = rift_git::family_of(&linked, rift_git::MintPolicy::Never).unwrap();
    let rift_git::FamilyState::Minted(family) = state else {
        panic!("the hook minted the family it captured into");
    };
    assert_eq!(
        rift_store::projection_db_for_family_under(&cache, &family),
        per_path_projection(&cache, &main),
        "the family adopts the main work tree's historical id, in place"
    );

    // Per-worktree state is the opposite case and stays split: a
    // session lives in one checkout at a time.
    // …its session state lands under its own admin dir.
    let linked_admin = main.join(".git").join("worktrees").join("linked");
    assert!(linked_admin
        .join("rift-sessions")
        .join("01LINKEDSESSION.json")
        .is_file());
    assert!(!main
        .join(".git")
        .join("rift-sessions")
        .join("01LINKEDSESSION.json")
        .exists());
}

/// Run `rift <args>` from `cwd` and return `(exit code, stdout)`.
fn run_rift_stdout(cwd: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> (i32, String) {
    let mut cmd = rift_cmd(cwd);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

/// Write a live session cache under `<repo>/.git/rift-sessions`.
fn seed_session(repo: &Path, id: &str, updated_at_ms: i64) {
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join(format!("{id}.json")),
        format!(
            r#"{{"schema":1,"session_id":"{id}","state":"active","agent":"claude-code",
                "current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,
                "updated_at_ms":{updated_at_ms},"native_id_map":{{}}}}"#
        ),
    )
    .unwrap();
}

/// **Both halves of the credit rule in one fixture: a linked worktree adopts
/// the session that NAMES it, and never the one that names its neighbour.**
///
/// Measured 2026-08-18, and this is a product defect for any user with git
/// worktrees rather than an artifact of how this repository is developed: an
/// agent registers its session under the git dir its own process resolved,
/// while a commit made in a different checkout of the same repository fires its
/// hooks against that checkout's admin dir — which holds no sessions. Neither
/// side has both halves, so post-commit takes its no-session branch and
/// archives nothing. On this repository's five linked worktrees that was 0
/// sessions, 0 checkpoints and 0 verification runs against 63 file revisions.
///
/// The over-correction is the other half and is why the discriminator exists:
/// reading the shared registry without one would credit a session driven in
/// worktree A with a commit made in worktree B. So the fixture builds TWO
/// linked worktrees off one repository, puts one session in the shared registry
/// claiming worktree A, and commits in BOTH.
///
/// **THE POSITIVE HALF SEEDS A SHAPE NO PRODUCTION WRITER EMITS, and that is
/// stated rather than quietly relied on.** `seed_shared_session` hand-writes a
/// shared-registry entry claiming a LINKED worktree. Nothing in production
/// writes that: `write_cache` stamps the claim from the git dir it is addressed
/// to, so a file in the COMMON registry is always stamped with the MAIN
/// worktree, while `live_sessions_for`'s shared branch fires only for a linked
/// one. Planted both ways on the shipped binary — a real session started in the
/// main checkout gives `archived=0` when committing in a linked worktree and
/// `archived=1` in the main checkout, the control. So this test pins the RULE
/// and not a reachable path; the gap it leaves is enumerated and OPEN.
///
/// It is not weakened or deleted on that account: calibration plant 3 showed it
/// bites — flipping `is_some_and` to `is_none_or` reds its negative half — and a
/// rule that is unreachable today is exactly the rule that must already be right
/// on the day something makes it reachable.
fn seed_shared_session(common_git: &Path, id: &str, work_tree: &Path, updated_at_ms: i64) {
    let sessions = common_git.join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    let wt = std::fs::canonicalize(work_tree).unwrap();
    std::fs::write(
        sessions.join(format!("{id}.json")),
        format!(
            r#"{{"schema":1,"session_id":"{id}","state":"active","agent":"claude-code",
                "current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,
                "updated_at_ms":{updated_at_ms},"native_id_map":{{}},
                "work_tree":{}}}"#,
            json_str(&wt)
        ),
    )
    .unwrap();
}

#[test]
fn a_worktree_adopts_only_the_shared_session_that_names_it() {
    let tmp = tempfile::tempdir().unwrap();
    let main = seed_live_session_repo(tmp.path());
    // The seeder leaves a session of its own in the shared registry; this test
    // is about which registry a LINKED worktree reads, so start it clean.
    let shared = main.join(".git").join("rift-sessions");
    std::fs::remove_dir_all(&shared).ok();

    let wt_a = tmp.path().join("wt-a");
    let wt_b = tmp.path().join("wt-b");
    for (path, branch) in [(&wt_a, "side-a"), (&wt_b, "side-b")] {
        let out = Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                branch,
                path.to_str().unwrap(),
                "HEAD",
            ])
            .current_dir(&main)
            .output()
            .unwrap();
        assert!(out.status.success(), "git worktree add: {out:?}");
    }

    // ONE session, in the SHARED registry, claiming worktree A only.
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    seed_shared_session(&main.join(".git"), "shared-a", &wt_a, now);
    // …and an OLDER session in the same registry, carrying no discriminator
    // at all — every cache file written before the claim existed looks like this. It
    // claims nobody, so nobody may adopt it. Without this entry the
    // "absence is not a claim" rule is unfalsifiable: measured, flipping
    // `is_some_and` to `is_none_or` left the suite green until it was added.
    let sessions = main.join(".git").join("rift-sessions");
    std::fs::write(
        sessions.join("legacy-nobody.json"),
        format!(
            r#"{{"schema":1,"session_id":"legacy-nobody","state":"active","agent":"claude-code",
                "current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,
                "updated_at_ms":{now},"native_id_map":{{}}}}"#
        ),
    )
    .unwrap();

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // Commit in BOTH worktrees, each firing its own post-commit hook.
    let mut condensed = Vec::new();
    for wt in [&wt_a, &wt_b] {
        // The projection is the family's now: both members write
        // to one store, so "did THIS worktree condense" is the DELTA its own
        // hook made, not the store's total. The checkpoint ref would prove even
        // less — `refs/heads/rift/checkpoints/v1` has always been shared, so
        // reading it from B would show A's condensation and this test would
        // pass having demonstrated nothing.
        let before = archived_in_family(&cache, wt);
        std::fs::write(wt.join("w.txt"), "work").unwrap();
        git_in(wt, &["add", "w.txt"]);
        git_in(wt, &["commit", "-q", "-m", "work"]);
        let git_dir = String::from_utf8_lossy(
            &Command::new("git")
                .args(["rev-parse", "--absolute-git-dir"])
                .current_dir(wt)
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_owned();
        let (code, stderr) = run_rift_capture(
            wt,
            &["hooks", "git", "post-commit", "--git-dir", &git_dir],
            Some(""),
            &env,
        );
        assert_eq!(code, 0, "the hook never fails the git op: {stderr}");
        condensed.push(archived_in_family(&cache, wt) > before);
    }

    // A: the session names this worktree, so the commit condenses under it —
    // the capability this rule restores.
    assert!(
        condensed[0],
        "worktree A adopts the shared session that names it and condenses"
    );
    // B: the same session is visible in the same shared registry and names
    // SOMEONE ELSE, so B archives nothing. This is the half that must never
    // regress: it is the difference between capturing work and misattributing
    // it.
    assert!(
        !condensed[1],
        "worktree B must NOT be credited with a session that names worktree A, \
         nor with an older session that names nobody"
    );
}

/// "Crashed-session auto-finalize (doctor hook)" **and**
/// "concurrent sessions same base commit", which are one fixture: two sessions
/// live in one worktree against one base commit, one abandoned by a dead agent
/// and one current. A doctor pass must finalize exactly the dead one and leave
/// the other alone. Nothing else in the product can reach a crashed session —
/// it fires no more hooks — so without this pass it stayed `active` forever and
/// kept being offered to every later archive as the session to attribute to.
#[test]
fn doctor_finalizes_the_crashed_session_and_leaves_its_live_neighbour() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    // The seeded repo already carries one live session; remove it so the two
    // below are the whole population.
    std::fs::remove_file(
        repo.join(".git")
            .join("rift-sessions")
            .join("e2e-live-1.json"),
    )
    .unwrap();

    // The idle window is 24 h, so "now" and "the epoch" are unambiguous
    // regardless of when this test runs; the wall clock is read only because
    // the subject under test is a wall-clock threshold inside the binary.
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    seed_session(&repo, "crashed-1", 0);
    seed_session(&repo, "current-1", now);

    let cache = tmp.path().join("cache");
    let (code, stdout) = run_rift_stdout(
        &repo,
        &["--json", "doctor", "--fix"],
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    assert_eq!(code, 0, "doctor reports, it never fails: {stdout}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let sessions = doc
        .pointer("/sessions")
        .or_else(|| doc.pointer("/data/sessions"))
        .expect("doctor json carries a sessions leg");
    assert_eq!(
        sessions["finalized"],
        serde_json::json!(["crashed-1"]),
        "exactly the abandoned session is finalized: {sessions}"
    );
    assert_eq!(
        sessions["live"],
        serde_json::json!(["current-1"]),
        "a session inside its idle window is untouched: {sessions}"
    );

    // The finalized session is closed in the cache, and the live one is not.
    let phase = |id: &str| -> String {
        let raw = std::fs::read_to_string(
            repo.join(".git")
                .join("rift-sessions")
                .join(format!("{id}.json")),
        )
        .unwrap();
        serde_json::from_str::<serde_json::Value>(&raw).unwrap()["state"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(phase("crashed-1"), "ended");
    assert_eq!(phase("current-1"), "active");

    // Finalizing condensed real work: a checkpoint exists on the chain ref.
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&refs.stdout).trim().is_empty(),
        "the finalized session condensed to a checkpoint"
    );

    // The sweep is idempotent: a second pass finds one live session and
    // finalizes nothing.
    let (_, stdout2) = run_rift_stdout(
        &repo,
        &["--json", "doctor", "--fix"],
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    let doc2: serde_json::Value = serde_json::from_str(&stdout2).unwrap();
    let s2 = doc2
        .pointer("/sessions")
        .or_else(|| doc2.pointer("/data/sessions"))
        .unwrap();
    assert_eq!(s2["finalized"], serde_json::json!([]));
    assert_eq!(s2["live"], serde_json::json!(["current-1"]));
}

/// The checkpoint shards currently on the native chain ref.
fn checkpoint_shards(repo: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args([
            "ls-tree",
            "-r",
            "--name-only",
            "refs/heads/rift/checkpoints/v1",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    let mut shards: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|p| p.rsplit_once('/').map(|(dir, _)| dir.to_owned()))
        .collect();
    shards.sort_unstable();
    shards.dedup();
    shards
}

/// "HEAD move without commit" and "mid-session stash → pull → apply".
///
/// In the legacy backend these are one case for a structural reason: its ephemeral
/// checkpoints live on a shadow branch **named after the base commit**
/// (`legacy/<c7>-<wt6>`), so moving HEAD without committing orphans that branch
/// and it has to be renamed — the tag's own architecture note calls this out as
/// "Shadow Branch Migration". Rift's native archive writes every checkpoint to
/// one orphan ref, `NATIVE_CHECKPOINT_REF` = `refs/heads/rift/checkpoints/v1`,
/// and the modelled `shadow_branch` relation has no writer anywhere in the tree
/// (verified by search, not assumed). So there is no base-commit-named ref to
/// migrate: the migration class is structurally **absent** here, not merely
/// unimplemented, and it can only return if a future refs-based checkpoint
/// backend reintroduces per-base-commit ephemeral refs.
///
/// What remains is the guarantee underneath it, which is what this pins: the
/// session survives the move, work archived *before* HEAD moved is still on the
/// ref *after* it, and an archive after the move lands on the same ref.
#[test]
fn a_session_and_its_checkpoints_survive_a_head_move_without_a_commit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let git = |args: &[&str]| git_in(&repo, args);
    // An "upstream" commit the working branch has not seen yet.
    let base = git(&["rev-parse", "HEAD"]).trim().to_owned();
    git(&["checkout", "-q", "-b", "upstream"]);
    std::fs::write(repo.join("b.txt"), "upstream\n").unwrap();
    git(&["add", "b.txt"]);
    git(&["commit", "-q", "-m", "upstream work"]);
    git(&["checkout", "-q", "-"]);
    assert_eq!(git(&["rev-parse", "HEAD"]).trim(), base);

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let post_commit = || {
        assert_eq!(
            run_rift(
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
            ),
            0
        );
    };
    post_commit();
    let before = checkpoint_shards(&repo);
    assert!(!before.is_empty(), "a checkpoint exists before the move");

    // stash → pull → apply: HEAD moves with no commit of ours in between.
    std::fs::write(repo.join("a.txt"), "work in progress\n").unwrap();
    git(&["stash", "--quiet"]);
    git(&["merge", "--ff-only", "-q", "upstream"]);
    git(&["stash", "pop", "--quiet"]);
    let moved = git(&["rev-parse", "HEAD"]).trim().to_owned();
    assert_ne!(moved, base, "HEAD really moved");
    assert_eq!(
        std::fs::read_to_string(repo.join("a.txt")).unwrap(),
        "work in progress\n",
        "the stashed work came back"
    );

    // The session is untouched by the move — it is keyed by session id, never
    // by the commit it happened to start against.
    let raw = std::fs::read_to_string(
        repo.join(".git")
            .join("rift-sessions")
            .join("e2e-live-1.json"),
    )
    .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&raw).unwrap()["state"],
        serde_json::json!("active")
    );

    // Nothing was orphaned, and the ref keeps taking archives at the new HEAD.
    //
    // Survival on its own would also be satisfied by a ref nothing can write to
    // any more — which is what the migration class looks like when it bites — so
    // the post-move archive has to be seen LANDING. That needs the session to
    // have captured something new, and the reason is a real property of the
    // native archive rather than test scaffolding: the checkpoint id folds in
    // the captured content and deliberately EXCLUDES the anchoring commit (the
    // trailer is predicted pre-commit), so a session that captured nothing new
    // re-archives the SAME shard path as an idempotent no-op. Measured, not
    // assumed: with no read in between, the second post-commit leaves the shard
    // list byte-identical.
    agent_read_hook(&repo, "e2e-live-1", 2, &repo.join("b.txt"), &env);
    post_commit();
    let after = checkpoint_shards(&repo);
    for shard in &before {
        assert!(
            after.contains(shard),
            "checkpoint {shard} archived before the HEAD move is still on the ref: {after:?}"
        );
    }
    assert!(
        after.len() > before.len(),
        "the post-move archive landed on the ref: {before:?} → {after:?}"
    );
    // And it landed at the MOVED head, not the base the session started against.
    let fresh: Vec<&String> = after.iter().filter(|s| !before.contains(s)).collect();
    assert_eq!(fresh.len(), 1, "one new checkpoint: {before:?} → {after:?}");
    let meta = git(&[
        "show",
        &format!("refs/heads/rift/checkpoints/v1:{}/metadata.json", fresh[0]),
    ]);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&meta).unwrap()["commit"],
        serde_json::json!(moved),
        "the post-move checkpoint is anchored at the new HEAD"
    );
}

/// The state of one session cache on disk.
fn session_phase(repo: &Path, id: &str) -> String {
    let raw = std::fs::read_to_string(
        repo.join(".git")
            .join("rift-sessions")
            .join(format!("{id}.json")),
    )
    .unwrap();
    serde_json::from_str::<serde_json::Value>(&raw).unwrap()["state"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// End to end and durably: a git-ignored file opted in through
/// `.worktreeinclude` is captured into the checkpoint, and the secret inside it
/// is masked in the bytes git actually stores.
///
/// The two halves have to be asserted together. "No secret in the shard" passes
/// trivially if nothing was captured, and "the file was captured" says nothing
/// about the gate — so this reads the real `content.redacted` blob back out of
/// the checkpoint ref and requires the record naming `.env`, the placeholder,
/// and the absence of the key, in one artifact.
#[test]
fn an_opted_in_ignored_file_reaches_the_shard_redacted() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    // The shape this control exists for: a credential file git is told to
    // ignore, so nothing else in Rift can ever see the work done in it.
    std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    std::fs::write(
        repo.join(".env"),
        "AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE\nFEATURE_FLAG=on\n",
    )
    .unwrap();
    std::fs::write(repo.join(".worktreeinclude"), "# local config\n.env\n").unwrap();

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    assert_eq!(
        run_rift(
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
        ),
        0
    );

    let shards = checkpoint_shards(&repo);
    assert_eq!(shards.len(), 1, "one checkpoint: {shards:?}");
    let blob = git_in(
        &repo,
        &[
            "show",
            &format!(
                "refs/heads/rift/checkpoints/v1:{}/content.redacted",
                shards[0]
            ),
        ],
    );
    assert!(
        blob.contains("rift-capture/1") && blob.contains("\nf .env\n"),
        "the opted-in file is in the checkpoint, named: {blob:?}"
    );
    assert!(
        blob.contains("FEATURE_FLAG=on"),
        "its non-secret content is captured, not thrown away with the secret: \
         {blob:?}"
    );
    assert!(
        !blob.contains("AKIAIOSFODNN7EXAMPLE"),
        "and the secret never reached the bytes git stores: {blob:?}"
    );
    assert!(blob.contains("[REDACTED:vendor]"), "masked, not dropped");

    // The report beside it counts the masking, so the shard is self-describing.
    let report = git_in(
        &repo,
        &[
            "show",
            &format!(
                "refs/heads/rift/checkpoints/v1:{}/redaction_report.json",
                shards[0]
            ),
        ],
    );
    let report: serde_json::Value = serde_json::from_str(&report).unwrap();
    assert!(
        report["total_spans"].as_u64().unwrap() >= 1,
        "the redaction report accounts for it: {report}"
    );
}

/// The commit trailer must still name the checkpoint that actually lands, now
/// that the capture it is derived from has content in it.
///
/// `prepare-commit-msg` predicts the condense id BEFORE the commit exists, and
/// `post-commit` archives at the id it derives afterwards. Both read the same
/// `session_raw_capture`, which is why they agree — but the inclusion document
/// is the first thing ever to make that capture non-empty, so a divergence
/// (different work tree, different read moment) would now produce a trailer
/// pointing at a shard that does not exist: a provenance lie, silently.
#[test]
fn the_predicted_trailer_id_is_the_shard_that_lands_with_an_include_file() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::write(repo.join(".env"), "FEATURE_FLAG=on\n").unwrap();
    std::fs::write(repo.join(".worktreeinclude"), ".env\n").unwrap();

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let msg = tmp.path().join("COMMIT_EDITMSG");
    std::fs::write(&msg, "do the thing\n").unwrap();
    let gd = repo.join(".git");
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "prepare-commit-msg",
                "--git-dir",
                gd.to_str().unwrap(),
                "--",
                msg.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );
    let injected = std::fs::read_to_string(&msg).unwrap();
    let predicted = injected
        .lines()
        .find_map(|l| l.strip_prefix("Rift-Checkpoint: "))
        .expect("the trailer was injected")
        .to_owned();

    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );
    assert_eq!(
        checkpoint_shards(&repo),
        vec![format!("{}/{}", &predicted[..2], &predicted[2..])],
        "the archive landed exactly where the trailer promised"
    );

    provenance_verbs_answer_over_the_real_shard(&repo, &env, &predicted);
}

/// The provenance composition (called from the predicted-trailer test, which owns the
/// only fixture in this file that archives a REAL shard): `checkpoint
/// explain <id>` resolves natively, and `why` walks line → commit → trailer →
/// checkpoint → session end-to-end.
fn provenance_verbs_answer_over_the_real_shard(repo: &Path, env: &[(&str, &str)], predicted: &str) {
    // The query surface (unlike the hook surface) sits behind the `.rift/`
    // init guard, so the fixture opts in the way `rift init` would.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let (code, out) = run_rift_stdout(repo, &["checkpoint", "explain", predicted, "--json"], env);
    assert_eq!(code, 0, "explain by id: {out}");
    let v: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(v["checkpoint_id"], predicted);
    assert_eq!(
        v["sessions"][0]["session_id"],
        rift_core::capture_session_row_id("claude-code", "e2e-live-1"),
        "the bridge's linkage answers with the DERIVED session row id even \
         though this fixture seeded the cache directly (no SessionStart ever \
         ingested a projection session row): {v}"
    );
    assert_eq!(
        v["sessions"][0]["agent"], "unknown",
        "an un-ingested session keeps its linkage and says its metadata is \
         unknown rather than vanishing: {v}"
    );
    assert_eq!(
        v["metadata"]["sessions"][0], "e2e-live-1",
        "the shard itself records the NATIVE session id: {v}"
    );
    assert!(
        v["metadata"]["canon_version"].as_u64().is_some(),
        "shard metadata was read from the ref: {v}"
    );

    // A commit that CARRIES the trailer gives `why` a real chain to walk.
    std::fs::write(repo.join("b.txt"), "two\n").unwrap();
    git_in(repo, &["add", "b.txt"]);
    git_in(
        repo,
        &[
            "commit",
            "-q",
            "-m",
            &format!("carry\n\nRift-Checkpoint: {predicted}"),
        ],
    );
    let (code, out) = run_rift_stdout(repo, &["why", "b.txt:1", "--json"], env);
    assert_eq!(code, 0, "why through a real trailer: {out}");
    let w: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert_eq!(w["selection"]["checkpoint_id"], predicted);
    assert_eq!(
        w["sessions"][0]["session_id"],
        rift_core::capture_session_row_id("claude-code", "e2e-live-1")
    );
    assert_eq!(
        w["prompt"],
        serde_json::Value::Null,
        "this session archived no transcript, and why says absence rather than guessing"
    );
    assert!(
        w["handles"]["file"]
            .as_str()
            .is_some_and(|h| h.starts_with("file:b.txt@sha256:")),
        "{w}"
    );
}

/// A redactor pack whose regex does not compile must reach the developer
/// **within one commit cycle**.
///
/// It did not. `prepare-commit-msg` loaded the packs and dropped the loader's
/// notes on the floor (`let (packs, _notes) = …`), and the only other loader on
/// the commit path runs in the detached worker, whose stderr is `/dev/null`. So
/// the repository's coverage silently fell back to the built-ins and nothing
/// anywhere said so — the silent-degradation failure mode, in the one plane where it is a
/// security property rather than an inconvenience.
///
/// The cycle is driven as git drives it: `prepare-commit-msg`, then
/// `post-commit`. The assertion is on the second one's stderr, because that is
/// the sweep that says markers out loud.
#[test]
fn a_pack_that_will_not_compile_is_said_within_one_commit_cycle() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let packs = repo.join(".rift").join("policies").join("redactors");
    std::fs::create_dir_all(&packs).unwrap();
    std::fs::write(
        packs.join("broken.toml"),
        "schema_version = 1\nname = \"broken\"\nversion = \"1\"\n\
         [[rules]]\nid = \"unclosed\"\nregex = \"ACME-[0-9\"\n",
    )
    .unwrap();

    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let gd = repo.join(".git");
    let msg = tmp.path().join("COMMIT_EDITMSG");
    std::fs::write(&msg, "do the thing\n").unwrap();

    // Cycle step 1. This hook notices; it is not the one that speaks.
    let (code, _) = run_rift_capture(
        &repo,
        &[
            "hooks",
            "git",
            "prepare-commit-msg",
            "--git-dir",
            gd.to_str().unwrap(),
            "--",
            msg.to_str().unwrap(),
        ],
        Some(""),
        &env,
    );
    assert_eq!(code, 0, "a broken pack never blocks a commit");

    // Cycle step 2. This one does.
    let (code, stderr) = run_rift_capture(
        &repo,
        &[
            "hooks",
            "git",
            "post-commit",
            "--git-dir",
            gd.to_str().unwrap(),
        ],
        Some(""),
        &env,
    );
    assert_eq!(code, 0, "nor does saying so");
    assert!(
        stderr.contains("unclosed"),
        "the rule that was dropped is named: {stderr:?}"
    );
    assert!(
        stderr.contains("did not compile"),
        "and why it was dropped: {stderr:?}"
    );
    // Counted by LINE, not by token: the compiler's own reason happens to
    // contain the rule's name too, so `matches("unclosed")` would read 2 for a
    // single note and quietly turn this into an assertion about wording.
    assert_eq!(
        stderr
            .lines()
            .filter(|l| l.contains("redactor rule"))
            .count(),
        1,
        "said once for the condition, not once per path that noticed it: {stderr:?}"
    );

    // …and the commit still produced its checkpoint: a broken pack costs its
    // own rule, never the capture.
    assert_eq!(
        checkpoint_shards(&repo).len(),
        1,
        "the archive still landed"
    );

    // The WORKER's own notice, isolated: this second `post-commit` has no
    // `prepare-commit-msg` before it, so anything it says came from the marker
    // the previous cycle's worker left. That path is not redundant — a merge
    // commit's `prepare-commit-msg` returns before the loader runs, so the
    // worker is the only thing that notices at all.
    let (code, stderr) = run_rift_capture(
        tmp.path(),
        &[
            "hooks",
            "git",
            "post-commit",
            "--git-dir",
            gd.to_str().unwrap(),
        ],
        Some(""),
        &env,
    );
    assert_eq!(code, 0);
    assert!(
        stderr.contains("did not compile"),
        "the detached worker's notice reaches the next sweep too: {stderr:?}"
    );
}

/// A repository that never opted anything in must condense exactly as it did
/// before the control existed — same empty capture, therefore the same
/// checkpoint id. Without this, adding `.worktreeinclude` support would silently
/// re-key every checkpoint in every repo that does not use it.
#[test]
fn a_repo_without_the_include_file_condenses_byte_identically() {
    let shard_of = |include: Option<&str>| -> Vec<String> {
        let tmp = tempfile::tempdir().unwrap();
        let repo = seed_live_session_repo(tmp.path());
        if let Some(text) = include {
            std::fs::write(repo.join(".worktreeinclude"), text).unwrap();
        }
        let cache = tmp.path().join("cache");
        assert_eq!(
            run_rift(
                &repo,
                &[
                    "hooks",
                    "git",
                    "post-commit",
                    "--git-dir",
                    repo.join(".git").to_str().unwrap(),
                ],
                Some(""),
                &[
                    ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
                    ("RIFT_NO_KEYCHAIN", "1"),
                    ("RIFT_CAPTURE_SYNC", "1"),
                ],
            ),
            0
        );
        checkpoint_shards(&repo)
    };
    let without = shard_of(None);
    assert_eq!(without.len(), 1);
    assert_eq!(
        shard_of(Some("# nothing opted in\n")),
        without,
        "an include file that names nothing is the same as none at all"
    );
    assert_ne!(
        shard_of(Some("a.txt\n")),
        without,
        "…and one that names something is not — the capture is in the id"
    );
}

/// Doctor's deferral line must name the reason the session actually deferred.
///
/// It used to branch: on a repository whose archive was routed through the
/// capture backend it said "none is provisioned — run `rift setup`", and on a
/// native one it said the truth. That was a real discrimination while two routes
/// existed, and it sent operators to install something they did not need
/// whenever it got the repository wrong. With the wrapper deleted (2026-08-11)
/// there is one cause left and the line states it. The negative assertion below
/// is kept deliberately as a resurrection guard: an absent binary is not a
/// deferral cause any more, so nothing may ever tell a developer it is.
#[test]
fn the_deferral_line_names_this_repos_true_cause() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    // A repository with NO commit: the archive has nothing to anchor to, which
    // is the only deferral cause there is.
    git_in(&repo, &["init", "-q", "."]);
    seed_session(&repo, "crashed-1", 0);

    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    // `--fix` because the sweep is doctor's repair arm: the deferral line is the sweep's own
    // report, and the sweep is doctor's repair arm.
    let (code, stdout) = run_rift_stdout(
        &repo,
        &["doctor", "--fix"],
        &[
            ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
            ("XDG_DATA_HOME", data.to_str().unwrap()),
            ("LOCALAPPDATA", data.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
            ("RIFT_CAPTURE_SYNC", "1"),
        ],
    );
    assert_eq!(code, 0, "doctor reports, it never fails: {stdout}");
    assert!(
        stdout.contains("condense still owed"),
        "the session did defer, so there IS a cause line: {stdout}"
    );
    assert!(
        stdout.contains("no commit to anchor to"),
        "the cause is this repo's, and it is the true one: {stdout}"
    );
    assert!(
        !stdout.contains("rift setup") && !stdout.to_lowercase().contains("not provisioned"),
        "nothing may send a developer to install a capture backend: there is \
         none, and there is nothing to install: {stdout}"
    );
}

/// Rewind (or advance) a session cache's last-touch stamp on disk. The sweep's
/// input is this recorded number, never the clock, so a fixture can age a
/// session by a day without sleeping for one or reaching for a fake clock.
fn set_session_touched(repo: &Path, id: &str, updated_at_ms: i64) {
    let path = repo
        .join(".git")
        .join("rift-sessions")
        .join(format!("{id}.json"));
    let mut cache: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    cache["updated_at_ms"] = serde_json::json!(updated_at_ms);
    std::fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
}

/// The unborn-HEAD sibling below never reaches the archive guard
/// (`drive_post_ingest` returns before `RequestCondense` when there is no
/// commit), so replacing the guard with a tautology left 197 tests green.
/// This test reaches it: a repo WITH a commit whose archive FAILS (the git
/// object store is made unwritable), driven through a real `SessionEnd`. The
/// property is the docstring's own claim — a `SessionEnd` that archived
/// nothing must not reach `ended`.
#[test]
#[cfg(unix)]
fn a_failing_archive_with_a_commit_present_still_defers_the_close() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("XDG_DATA_HOME", data.to_str().unwrap()),
        ("LOCALAPPDATA", data.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // The archive path must WANT to run (a commit exists) and then FAIL
    // (nothing can write an object). chmod the object store read-only.
    let objects = repo.join(".git").join("objects");
    let restore: Vec<(std::path::PathBuf, u32)> = walkdir_dirs(&objects)
        .into_iter()
        .map(|d| {
            let mode = std::fs::metadata(&d).unwrap().permissions().mode();
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();
            (d, mode)
        })
        .collect();

    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    let code = run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env);

    // Restore FIRST so a failing assertion cannot strand a read-only tempdir.
    for (d, mode) in restore {
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    assert_eq!(code, 0, "a capture hook never fails the agent, even here");
    assert_ne!(
        session_phase(&repo, "e2e-live-1"),
        "ended",
        "a SessionEnd whose archive FAILED must not close the session — \
         this is the guard the unborn-HEAD tests cannot reach"
    );
}

/// Every directory under `root`, including itself — the minimal walker the
/// permission plant needs, no dependency added.
#[cfg(unix)]
fn walkdir_dirs(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = vec![root.to_path_buf()];
    let mut i = 0;
    while i < out.len() {
        if let Ok(rd) = std::fs::read_dir(&out[i]) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    out.push(p);
                }
            }
        }
        i += 1;
    }
    out
}

/// The lifecycle rule settled 2026-08-07, end to end through the real binary: a
/// `SessionEnd` whose archive could not run must land in `ending`, not `ended`.
///
/// The hole this closes was the worst kind — ordinary inputs, no error, total
/// loss. When the archive could not run, the hook path used to close the session
/// anyway. `Ended` has no outgoing edges, so that session's work could never be
/// condensed by anything afterwards: not the sweep, not doctor, not a later
/// hook. Everything the agent did in it was captured and then orphaned.
///
/// The rescue is the other half of the rule and is what makes deferral honest
/// rather than merely cautious: the session is still `ending`, so the moment the
/// repository has an archive it can reach, the sweep finishes the condense the
/// hook could not.
///
/// **How the deferral is reproduced changed with the wrapper.** It used to be a
/// repository pinned to the wrapper route on a machine with no capture backend
/// installed; that route and that backend are gone (2026-08-11). The condition
/// left is the one the archive genuinely cannot work around: an **unborn HEAD**
/// — no commit to anchor a checkpoint to. Same property, reproduced by the same
/// mechanism the product will actually meet.
#[test]
fn a_session_end_that_could_not_archive_defers_instead_of_closing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo_unborn(tmp.path());
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("XDG_DATA_HOME", data.to_str().unwrap()),
        ("LOCALAPPDATA", data.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    assert_eq!(
        run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env),
        0,
        "a capture hook never fails the agent, deferral or not"
    );

    // Nothing was archived…
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&refs.stdout).trim().is_empty(),
        "nothing to anchor to: no checkpoint exists"
    );
    // …so the session is NOT closed. `ended` would be a false record of the one
    // fact the phase exists to carry, and an unrecoverable one.
    assert_eq!(
        session_phase(&repo, "e2e-live-1"),
        "ending",
        "a SessionEnd that archived nothing leaves the session sweep-retryable"
    );

    // A day passes with the agent gone, and the repository gains the commit the
    // archive needs. The sweep must finish what the hook could not.
    git_in(&repo, &["commit", "-q", "-m", "seed"]);
    set_session_touched(&repo, "e2e-live-1", 0);
    // `--fix` because the sweep is doctor's repair arm: the rescue is a repair, and bare doctor
    // only reports it as stuck.
    let (code, stdout) = run_rift_stdout(&repo, &["--json", "doctor", "--fix"], &env);
    assert_eq!(code, 0, "doctor reports, it never fails: {stdout}");
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let sessions = doc
        .pointer("/sessions")
        .or_else(|| doc.pointer("/data/sessions"))
        .expect("doctor json carries a sessions leg");
    assert_eq!(
        sessions["finalized"],
        serde_json::json!(["e2e-live-1"]),
        "the sweep rescued the condense the hook deferred: {sessions}"
    );
    assert_eq!(session_phase(&repo, "e2e-live-1"), "ended");
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&refs.stdout).trim().is_empty(),
        "the rescue condensed real work"
    );
}

/// The other half of the same rule, and the regression guard for it: a
/// `SessionEnd` whose archive **does** run still closes the session on the spot,
/// exactly as before. Deferral is the exception the absent checkpoint earns, not
/// the new normal — without this pinned, "leave it `ending`" could quietly become
/// the only path and every session would need a sweep to close.
#[test]
fn a_session_end_that_archived_still_closes_immediately() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    let body = r#"{"schema":1,"session":"e2e-live-1","turn":1,"payload":{"reason":"exit"}}"#;
    assert_eq!(
        run_rift_in(&repo, &["hooks", "claude-code", "SessionEnd"], body, &env),
        0
    );
    assert_eq!(
        session_phase(&repo, "e2e-live-1"),
        "ended",
        "a condense that landed closes the session at the hook, no sweep needed"
    );
    assert!(
        !checkpoint_shards(&repo).is_empty(),
        "and it closed on a checkpoint that exists"
    );
}

/// The sweep when the condense **cannot** land — the case the crashed-session
/// pass actually meets most often, and the one that carried a measured
/// regression.
///
/// Two things are pinned here, and they are the same defect seen from two ends.
/// (1) A deferred session must never become the repository's archive target:
/// `live_sessions` counts `ending` as live and returns most-recently-updated
/// first, so a sweep that stamped the cache would hand the *dead* session to the
/// next post-commit, and that attribution is durable. (2) A deferred session
/// must still be reachable: `Ending` has no `IdleTimeout` edge and `post-commit`
/// only ever runs the strategy decision — never a pending `RequestCondense` — so
/// the sweep is the only retry there is. Without it, `ending` is a terminal
/// state in practice: counted live forever, condensed by nothing.
///
/// The condense is made deterministically unable to land — an **unborn HEAD**,
/// so there is no commit to anchor a checkpoint to — rather than assumed unable.
/// It used to be an absent capture backend on a wrapper-routed repository; that
/// route and that backend were deleted on 2026-08-11, and this is the condition
/// that remains.
#[test]
fn a_deferred_sweep_keeps_the_live_session_first_and_retries_later() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo_unborn(tmp.path());
    std::fs::remove_file(
        repo.join(".git")
            .join("rift-sessions")
            .join("e2e-live-1.json"),
    )
    .unwrap();
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    seed_session(&repo, "crashed-1", 0);
    seed_session(&repo, "current-1", now);

    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("XDG_DATA_HOME", data.to_str().unwrap()),
        ("LOCALAPPDATA", data.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let doctor = || {
        // `--fix` because the sweep is doctor's repair arm: this closure asserts sweep effects.
        let (code, stdout) = run_rift_stdout(&repo, &["--json", "doctor", "--fix"], &env);
        assert_eq!(code, 0, "doctor reports, it never fails: {stdout}");
        let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
        doc.pointer("/sessions")
            .or_else(|| doc.pointer("/data/sessions"))
            .expect("doctor json carries a sessions leg")
            .clone()
    };

    let first = doctor();
    assert_eq!(
        first["condense_deferred"],
        serde_json::json!(["crashed-1"]),
        "nothing to anchor to: the session defers, it is not closed on a \
         condense that never happened: {first}"
    );
    assert_eq!(first["finalized"], serde_json::json!([]));
    assert_eq!(first["live"], serde_json::json!(["current-1"]));
    assert_eq!(session_phase(&repo, "crashed-1"), "ending");

    // THE REGRESSION: the deferred session is still live (it is `ending`), but
    // it must not be the one the next archive attributes against.
    let live = rift_shadow::hooks::session_cache::live_sessions(&repo.join(".git")).unwrap();
    let order: Vec<&str> = live.iter().map(|c| c.session_id.as_str()).collect();
    assert_eq!(
        order,
        vec!["current-1", "crashed-1"],
        "the developer's live session stays the repo's archive target"
    );

    // THE HOLE UNDERNEATH IT: give the repository the commit the archive needs,
    // and the next sweep must finish the condense it owes — nothing else will.
    git_in(&repo, &["commit", "-q", "-m", "seed"]);
    let second = doctor();
    assert_eq!(
        second["finalized"],
        serde_json::json!(["crashed-1"]),
        "the sweep retried the condense it owed: {second}"
    );
    assert_eq!(second["live"], serde_json::json!(["current-1"]));
    assert_eq!(session_phase(&repo, "crashed-1"), "ended");
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        !String::from_utf8_lossy(&refs.stdout).trim().is_empty(),
        "the retry condensed real work"
    );
}

/// The soak half: a real commit that condenses accrues one
/// `verification_run` row, and `rift doctor` says the count out loud.
///
/// The verify runs in the DETACHED worker, after git has returned, so this
/// costs the hook budget nothing — asserted here only in that the hook still
/// exits 0. What is asserted directly is the thing the kill commit is gated on:
/// the counter moves by itself, from an ordinary commit, with nobody
/// remembering to run anything.
///
/// **HALF OF A PAIR — DO NOT DELETE EITHER HALF.** This test and
/// [`five_bare_verifies_are_not_soak_evidence`] are one measurement, and
/// neither is a measurement alone. This one says the counter MOVES when work
/// is condensed; that one says it does NOT move when the operator merely runs
/// `rift verify`. Un-scope `SoakEvidence::gather` from witnessed runs and this
/// test stays green while that one reddens — which is the only reason either
/// number means anything. A reader who finds one of them "redundant" and
/// removes it does not weaken the suite by one test; they silently restore
/// the whole defect the witness row closes, because a counter that moves is
/// indistinguishable from a counter that moves for the wrong reason.
#[test]
fn a_condensed_commit_accrues_one_clean_verify_and_doctor_reports_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    // Re-stamp the session as FRESH. The seeder's fixed timestamp is idle past
    // the sweep threshold, so the `doctor` call below would finalize it, condense
    // it, and accrue a row of its own — and this test would pass with the drain
    // accrual deleted. It did: that is how this line came to exist.
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    seed_session(&repo, "e2e-live-1", now);
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

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
    assert_eq!(code, 0, "the hook never fails the git op: {stderr}");

    let (code, after) = run_rift_capture_in(&repo, &["doctor", "--json"], "", &env);
    assert_eq!(code, 0, "doctor --json: {after}");
    let v: serde_json::Value = serde_json::from_str(&after).expect("doctor emits json");
    let soak = &v["soak"];
    assert_eq!(soak["readable"], serde_json::json!(true), "{after}");
    assert_eq!(soak["target"], serde_json::json!(20), "the soak threshold");
    assert!(
        soak["total_runs"].as_i64().unwrap_or(0) >= 1,
        "a condensed commit recorded a WITNESSED verification run: {after}"
    );
    assert!(
        soak["consecutive_clean"].as_i64().unwrap_or(0) >= 1,
        "and it was clean: {after}"
    );
    // The run is evidence because it names the condensation it
    // verified. Without the witness row the counter reads zero here — which is
    // exactly what a bare `rift verify` produces, and the test below plants one.
    assert!(
        soak["distinct_checkpoints"].as_i64().unwrap_or(0) >= 1,
        "the run witnessed a real checkpoint: {after}"
    );
    // Not soaked on one run: absence of 20 is not evidence of safety.
    assert_eq!(soak["soaked"], serde_json::json!(false), "{after}");
}

/// **The calibration the witness row exists for: a bare `rift verify` is not
/// soak evidence, however many times it is run.**
///
/// `verification_run` has three writers and `rift verify` is one of them, so
/// before the witness row twenty invocations of the operator's own diagnostic
/// filled the soak — on a repository that had condensed nothing. This plants
/// exactly that: five verifies against a repository with a live session and no
/// condense, and the counter must not move.
///
/// **HALF OF A PAIR — DO NOT DELETE EITHER HALF.** The other half is
/// [`a_condensed_commit_accrues_one_clean_verify_and_doctor_reports_it`]: it
/// condenses and the counter moves, this one verifies five times and it does
/// not. Restoring the un-scoped reader reddens THIS test and leaves that one
/// green — measured, exit 101, 2026-08-18 — which is what makes the two of them
/// a measurement rather than an assertion. Deleting either one leaves a suite
/// that still passes and no longer distinguishes a soak from a diagnostic.
#[test]
fn five_bare_verifies_are_not_soak_evidence() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    for _ in 0..5 {
        // WHICH verdict is not the point — measured, this fixture verifies
        // CLEAN (exit 0) — the ROW it writes is, and it writes one either way.
        //
        // But that it reached the verify path at all IS the point, and this
        // assertion used to be `!out.is_empty() || out.is_empty()` — a tautology
        // wearing a message that claimed a check. It mattered here more than
        // most places: the assertion below is `total_runs == 0`, which passes
        // just as happily if `rift verify` never ran. So this pins a real VERIFY
        // verdict — 0 clean, 1 tamper, 3 redaction, 5 structure — which a usage
        // error (2) or an internal failure (70) could not produce.
        let (code, out) = run_rift_capture_in(&repo, &["verify"], "", &env);
        assert!(
            matches!(code, 0 | 1 | 3 | 5),
            "verify reached a verdict rather than failing before it: exit {code}, {out}"
        );
    }

    // Bare `rift doctor` — no `--fix`, so its own sweep condenses nothing and
    // cannot contribute a witness of its own.
    let (code, after) = run_rift_capture_in(&repo, &["doctor", "--json"], "", &env);
    assert_eq!(code, 0, "doctor --json: {after}");
    let v: serde_json::Value = serde_json::from_str(&after).expect("doctor emits json");
    let soak = &v["soak"];
    assert_eq!(
        soak["total_runs"],
        serde_json::json!(0),
        "five bare verifies witnessed nothing, so the soak counted nothing: {after}"
    );
    assert_eq!(soak["consecutive_clean"], serde_json::json!(0), "{after}");
    assert_eq!(
        soak["distinct_checkpoints"],
        serde_json::json!(0),
        "{after}"
    );
    assert_eq!(soak["soaked"], serde_json::json!(false), "{after}");
}

/// The second condense site, found by measurement rather than by reading:
/// `rift doctor`'s idle-session sweep **condenses**, and before accrual was
/// wired here it accrued nothing.
///
/// That mattered twice over. The sweep's checkpoint went uncounted, and because
/// the sweep also *closes* the session, the next commit had nothing left to
/// archive and accrued nothing either — so running the command that DISPLAYS
/// the soak counter was enough to stop the counter moving.
#[test]
fn doctors_own_sweep_accrues_a_verify_instead_of_swallowing_one() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // `--fix` because the sweep is doctor's repair arm: the sweep is doctor's REPAIR, and bare
    // doctor reports without repairing.
    let (code, out) = run_rift_capture_in(&repo, &["doctor", "--fix", "--json"], "", &env);
    assert_eq!(code, 0, "doctor: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("doctor emits json");

    // The sweep really did condense — otherwise this test proves nothing.
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&refs.stdout).contains("rift/checkpoints/v1"),
        "doctor's sweep archived a checkpoint"
    );

    // …and the same invocation reports the row it just earned.
    assert!(
        v["soak"]["total_runs"].as_i64().unwrap_or(0) >= 1,
        "doctor's own condense accrued a verify: {out}"
    );
    // And the verify NAMES what it condensed. The sweep runs
    // one verify for every session it finalizes, so this is the witness half of
    // the same row — without it the accrual is indistinguishable from doctor
    // having simply run `verify` on itself.
    assert!(
        v["soak"]["distinct_checkpoints"].as_i64().unwrap_or(0) >= 1,
        "the sweep's verify witnessed the checkpoint it minted: {out}"
    );
}

/// **The one property the levelling must not break, and until now nothing
/// could redden when it did: verify must not launder a projection into
/// agreeing with the source it is about to be compared against.**
///
/// `run_at_with` levels before comparing, which is only safe
/// because `record_checkpoint_if_new` is `insert_or_ignore`
/// (`rift-store/src/integrity.rs`) — append-only means levelling can ADD a
/// missing row and can never overwrite a present one. Change that single call
/// to `INSERT OR REPLACE`, or add any "heal the projection" step, and the whole
/// `ProjectionVsSource` comparison silently becomes a tautology: verify would
/// fill the projection from git and then report that it matches git.
///
/// Nothing in the suite drove that path. `verify.rs`'s unit tests call
/// `verify_under` directly, `e2e.rs` uses the library verify over an in-memory
/// store, and the divergence test beside this one plants a *divergence* row,
/// not a *checkpoint* row — so the sentence in `run_at_with` claiming the
/// laundering is forbidden was an assertion with nothing behind it. This is the
/// test that makes it a measurement: a GENUINE checkpoint id carrying a WRONG
/// merkle root, planted into an empty migrated projection, and verify must come
/// back TAMPER rather than quietly overwriting it.
#[test]
fn levelling_cannot_launder_a_planted_checkpoint_into_agreement() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache_a = tmp.path().join("cache-a");
    let env_a = [
        ("RIFT_CACHE_DIR", cache_a.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // A real condensation, so git holds a real shard with a real checkpoint id.
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
        &env_a,
    );
    assert_eq!(code, 0, "the hook never fails the git op: {stderr}");

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let repo_id = rift_git::repo_id_for(&repo).unwrap();

    // The GENUINE id, read from the projection the real condensation built.
    let genuine: String = rt.block_on(async {
        let store = rift_store::open_capture_projection(rift_store::projection_db_under(
            &cache_a, &repo_id,
        ))
        .unwrap();
        let rows = rift_store::CanonicalStore::query(
            &store,
            "SELECT checkpoint_id FROM checkpoint LIMIT 1",
            &[],
        )
        .await
        .unwrap();
        assert_eq!(rows.len(), 1, "the condensation recorded a checkpoint");
        rows[0].get_text(0).unwrap()
    });

    // A SECOND, empty cache: migrated, then given that genuine id with a merkle
    // root that is not the one git will re-derive.
    let cache_b = tmp.path().join("cache-b");
    rt.block_on(async {
        let store = rift_store::open_capture_projection(rift_store::projection_db_under(
            &cache_b, &repo_id,
        ))
        .unwrap();
        rift_store::record_checkpoint_if_new(
            &store,
            &rift_store::CheckpointRecord {
                checkpoint_id: rift_core::Sha256Id::parse_hex(&genuine).unwrap(),
                strategy: "manual-commit".to_owned(),
                created_at: 1,
                merkle_root: rift_core::Sha256Id::parse_hex(&"f".repeat(64)).unwrap(),
                prev: None,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    });

    let env_b = [
        ("RIFT_CACHE_DIR", cache_b.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let (code, out) = run_rift_capture_in(&repo, &["--json", "verify"], "", &env_b);
    assert_eq!(
        code, 1,
        "levelling must not overwrite the planted row into agreement: {out}"
    );
    // `verify` prints its repo/projection lines before the JSON document, so the
    // payload starts at the first `{` rather than at byte zero.
    let json_start = out
        .find('{')
        .expect("verify emits a json document after its repo/projection lines");
    let v: serde_json::Value = serde_json::from_str(&out[json_start..]).expect("verify emits json");
    assert_eq!(
        v["verify"],
        serde_json::json!("tamper"),
        "and the verdict is tamper, not a laundered ok: {out}"
    );
    assert!(
        v["detail"]
            .as_str()
            .is_some_and(|d| d.contains(&genuine[..8])),
        "naming the checkpoint it disagreed about: {out}"
    );
}

/// **The divergence veto has to reach the USER, not just the source: one
/// recorded divergence vetoes that projection's soak permanently.**
///
/// `verification_divergence` is append-only and nothing supersedes a row, so
/// this is not a state that waiting fixes — the projection has to be rebuilt
/// from git. A counter that simply refuses to clear, with the reason available
/// only by reading `soak.rs`, is the same silence the pending-marker line was
/// added to end.
///
/// The divergence is PLANTED directly into the sandbox projection rather than
/// provoked, because provoking a real one means corrupting a checkpoint, and
/// what is under test here is the REPORTING, not the detection.
#[test]
fn a_recorded_divergence_says_it_vetoed_the_soak_and_says_it_is_permanent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // A real condensation first, so the streak is genuinely non-zero and the
    // veto is what stops the soak rather than an empty log.
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
    assert_eq!(code, 0, "the hook never fails the git op: {stderr}");

    // PLANT the divergence, on a run id that witnessed nothing — the case
    // that was decided, and the one an earlier revision excused.
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        let repo_id = rift_git::repo_id_for(&repo).unwrap();
        let db = rift_store::projection_db_under(&cache, &repo_id);
        let store = rift_store::open_capture_projection(db).unwrap();
        rift_store::record_verification_run(
            &store,
            &rift_store::VerificationRunRow {
                run_id: "planted-bare-verify".to_owned(),
                branch: "rift/checkpoints/v1".to_owned(),
                exit_code: 1,
                checked_at: 1,
                outcome_sha256: "ab".repeat(32),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        rift_store::record_verification_divergence(
            &store,
            &rift_store::VerificationDivergenceRow {
                run_id: "planted-bare-verify".to_owned(),
                ordinal: 0,
                checkpoint_id: "cd".repeat(32),
                layer: "chain".to_owned(),
                site: "ProjectionVsSource".to_owned(),
                detail: "planted".to_owned(),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    });

    let (code, out) = run_rift_capture_in(&repo, &["doctor", "--json"], "", &env);
    assert_eq!(code, 0, "doctor --json: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).expect("doctor emits json");
    assert_eq!(
        v["soak"]["divergence_veto"],
        serde_json::json!(true),
        "the veto is machine-readable: {out}"
    );
    assert_eq!(v["soak"]["soaked"], serde_json::json!(false), "{out}");

    // …and a human is TOLD, including that it is permanent.
    let (code, human) = run_rift_capture_in(&repo, &["doctor"], "", &env);
    assert_eq!(code, 0, "doctor: {human}");
    assert!(
        human.contains("VETOED"),
        "the human output names the veto: {human}"
    );
    assert!(
        human.contains("permanent") && human.contains("rift rebuild"),
        "and says it is permanent and how to clear it: {human}"
    );
}

/// A developer diagnosing a reset soak streak must be shown the REASON, not
/// only the counter. The marker is written by the detached worker and was read
/// only by the next hook, so between commits the explanation existed on disk
/// and nothing would print it.
#[test]
fn doctor_says_and_consumes_a_pending_capture_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // A marker left behind exactly as the worker leaves one.
    let queue = repo.join(".git").join("rift-capture-queue");
    std::fs::create_dir_all(&queue).unwrap();
    std::fs::write(
        queue.join("stale-entry.error"),
        "soak verify is not clean: tamper",
    )
    .unwrap();

    let (code, first) = run_rift_capture_in(&repo, &["doctor", "--json"], "", &env);
    assert_eq!(code, 0, "doctor: {first}");
    let v: serde_json::Value = serde_json::from_str(&first).expect("doctor emits json");
    let pending = v["soak"]["pending_markers"]
        .as_array()
        .expect("pending_markers is an array");
    assert!(
        pending.iter().any(|m| m
            .as_str()
            .is_some_and(|s| s.contains("soak verify is not clean"))),
        "doctor must say WHY the streak reset: {first}"
    );

    // …and consumes it, so a warning that always fires never happens.
    let (code, second) = run_rift_capture_in(&repo, &["doctor", "--json"], "", &env);
    assert_eq!(code, 0, "doctor: {second}");
    let v2: serde_json::Value = serde_json::from_str(&second).expect("doctor emits json");
    assert!(
        v2["soak"]["pending_markers"]
            .as_array()
            .is_some_and(std::vec::Vec::is_empty),
        "the marker is reported exactly once: {second}"
    );
    assert!(!queue.join("stale-entry.error").exists());
}

/// The fail-loud rule, honoured by loudness instead of by a dial.
///
/// A `.rift/rewrite.toml` used to route every write-side capability, and
/// `kill_switch = true` used to force them all back to the wrapper. Deleting the
/// wrapper (2026-08-11) turned the whole file inert — and an inert safety
/// control that a user still believes in is exactly the silence §3 forbids.
///
/// So the file is now read for one thing: whether it is there. Both directions
/// are asserted here because only the pair is a calibration — a note that always
/// fires and a note that never fires look identical from one side.
///
/// The **content is deliberately the benign case**: `archive = "native"` agreed
/// with reality even before the deletion. If the note only fired on
/// `kill_switch` or a wrapper route, this repository would keep a file it
/// believes configures something, and this test would pass while the user stayed
/// wrong.
#[test]
fn a_stale_rewrite_toml_is_named_once_and_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];

    // SILENCE: no file, nothing said, on the hook channel or in doctor.
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
    assert_eq!(code, 0, "the hook never fails the git op: {stderr}");
    let (code, quiet) = run_rift_capture_in(&repo, &["doctor", "--json"], "", &env);
    assert_eq!(code, 0, "doctor: {quiet}");
    let v: serde_json::Value = serde_json::from_str(&quiet).expect("doctor emits json");
    assert_eq!(
        v["stale_rewrite_toml"]["present"],
        serde_json::json!(false),
        "no file, no note: {quiet}"
    );

    // NOISE: the file appears, and the next commit's worker leaves the note
    // where the hook after it will say it out loud.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("rewrite.toml"),
        "[routes]\narchive = \"native\"\n",
    )
    .unwrap();
    git_in(&repo, &["commit", "-q", "--allow-empty", "-m", "second"]);
    let hook = || {
        run_rift_capture(
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
        )
    };
    let (code, _) = hook();
    assert_eq!(code, 0, "the note never fails the git op");
    let (code, said) = hook();
    assert_eq!(code, 0, "still never fails the git op");
    assert!(
        said.contains("this file does nothing"),
        "the stale file is named at the next hook: {said:?}"
    );
    assert_eq!(
        said.matches("this file does nothing").count(),
        1,
        "said once, not once per line ever written: {said:?}"
    );

    // …and doctor says it every time, because the FILE is still there even
    // after the marker has been consumed. A marker is said once; a state is
    // reported.
    for pass in 0..2 {
        let (code, out) = run_rift_capture_in(&repo, &["doctor", "--json"], "", &env);
        assert_eq!(code, 0, "doctor pass {pass}: {out}");
        let v: serde_json::Value = serde_json::from_str(&out).expect("doctor emits json");
        assert_eq!(
            v["stale_rewrite_toml"]["present"],
            serde_json::json!(true),
            "pass {pass}: doctor reports the file it can still see: {out}"
        );
    }

    // AND IT CHANGED NOTHING: the archive ran exactly as it does without the
    // file. A note that came with a behaviour change would be a route by
    // another name.
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&refs.stdout).contains("rift/checkpoints/v1"),
        "the checkpoint landed regardless of the stale file"
    );
}

/// The shard reaches git; its POSITION has to reach git too.
///
/// Two commits, not one. One entry proves only that something was written —
/// it is the SECOND that shows where a new entry attaches, and "attaches to the
/// head" versus "starts again at genesis" is the whole defect this ref exists
/// to end (a second genesis in the projection, from a chain derived by sorting
/// content hashes).
#[test]
fn two_native_archives_record_two_chain_positions_and_one_genesis() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let git_out = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let post_commit = || {
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
        assert_eq!(code, 0, "the hook never fails git: {stderr}");
        stderr
    };
    // A commit with its own live session, so each one condenses.
    let commit_live = |n: u32| {
        std::fs::write(
            repo.join(".git")
                .join("rift-sessions")
                .join(format!("e2e-chain-{n}.json")),
            format!(
                "{{\"schema\":1,\"session_id\":\"e2e-chain-{n}\",\"state\":\"active\",\
                 \"agent\":\"claude-code\",\"current_turn_ordinal\":1,\"turn_open\":true,\
                 \"compaction_boundary\":null,\"updated_at_ms\":1783000000000,\
                 \"native_id_map\":{{}}}}"
            ),
        )
        .unwrap();
        std::fs::write(repo.join("a.txt"), format!("line {n}\n")).unwrap();
        for args in [
            vec!["add", "a.txt"],
            vec![
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=T",
                "commit",
                "-q",
                "-m",
                "next",
            ],
        ] {
            assert!(Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success());
        }
    };

    // First archive: the repository's genesis position.
    let stderr = post_commit();
    assert!(!stderr.contains("chain"), "quiet on success: {stderr}");
    let after_one = git_out(&["rev-list", "--count", "refs/rift/chain/v1"]);
    assert_eq!(after_one.trim(), "1", "one archive, one position");

    // Second archive: it must attach to the head.
    commit_live(2);
    let stderr = post_commit();
    assert!(!stderr.contains("chain"), "quiet on success: {stderr}");
    assert_eq!(
        git_out(&["rev-list", "--count", "refs/rift/chain/v1"]).trim(),
        "2",
        "the second archive appended rather than replacing"
    );
    assert_eq!(
        git_out(&["rev-list", "--max-parents=0", "refs/rift/chain/v1"])
            .lines()
            .count(),
        1,
        "EXACTLY ONE genesis: the second entry is parented on the first"
    );

    // The positions are the branch's checkpoints, in the branch's own order.
    let branch_ids = branch_checkpoint_ids(&repo);
    assert_eq!(branch_ids.len(), 2, "two checkpoints on the branch");
    assert_eq!(
        chain_entry_ids(&repo),
        branch_ids,
        "the chain records the branch's checkpoints, in the branch's order"
    );
}

/// Planted twice by an adversarial review before this held: the chain step's marker rode the
/// queue entry's own `.error` file, and `accrue_soak` overwrote that same file
/// moments later — a chain refusal makes the soak verify unclean BY
/// CONSTRUCTION, so the operator only ever read "soak verify is not clean",
/// never WHY. The chain marker now rides the repository channel (`mark_note`,
/// key "chain"); this asserts both lines coexist after the clobber shape.
#[test]
fn a_chain_refusal_and_the_soak_verdict_both_survive_to_be_read() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let post_commit = || {
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
        assert_eq!(code, 0, "the hook never fails git: {stderr}");
        stderr
    };
    let commit_live = |n: u32| {
        std::fs::write(
            repo.join(".git")
                .join("rift-sessions")
                .join(format!("e2e-clob-{n}.json")),
            format!(
                "{{\"schema\":1,\"session_id\":\"e2e-clob-{n}\",\"state\":\"active\",\
                 \"agent\":\"claude-code\",\"current_turn_ordinal\":1,\"turn_open\":true,\
                 \"compaction_boundary\":null,\"updated_at_ms\":1783000000000,\
                 \"native_id_map\":{{}}}}"
            ),
        )
        .unwrap();
        std::fs::write(repo.join("a.txt"), format!("clob {n}\n")).unwrap();
        for args in [
            vec!["add", "a.txt"],
            vec![
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=T",
                "commit",
                "-q",
                "-m",
                "next",
            ],
        ] {
            assert!(Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .status()
                .unwrap()
                .success());
        }
    };

    // Two archived checkpoints, chain healthy.
    post_commit();
    commit_live(2);
    post_commit();
    // Erase the order: the next archive sees history it must not backfill.
    assert!(Command::new("git")
        .args(["update-ref", "-d", "refs/rift/chain/v1"])
        .current_dir(&repo)
        .status()
        .unwrap()
        .success());
    commit_live(3);
    post_commit();

    let qdir = repo.join(".git").join("rift-capture-queue");
    let chain_note = std::fs::read_to_string(qdir.join("chain.error"))
        .or_else(|_| std::fs::read_to_string(qdir.join("chain")))
        .expect("the chain refusal has its own file on the repository channel");
    assert!(
        chain_note.contains("rift chain init"),
        "the chain marker says WHAT to run: {chain_note}"
    );
    // And the soak verdict is still where it always was — on the entry.
    let soak_lines: Vec<String> = std::fs::read_dir(&qdir)
        .unwrap()
        .filter_map(|e| {
            let p = e.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            (std::path::Path::new(&name)
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("error"))
                && !name.starts_with("chain"))
            .then(|| std::fs::read_to_string(&p).unwrap_or_default())
        })
        .collect();
    assert!(
        soak_lines.iter().any(|l| l.contains("soak verify")),
        "the soak verdict coexists instead of clobbering: {soak_lines:?}"
    );
}

/// The checkpoint ids the branch's own first-parent history carries, oldest
/// first — read with git alone, so the assertion does not go through the code
/// under test.
fn branch_checkpoint_ids(repo: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args([
            "log",
            "--first-parent",
            "--reverse",
            "--root",
            "--name-only",
            "--format=",
            "refs/heads/rift/checkpoints/v1",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|p| p.strip_suffix("/metadata.json"))
        .map(|d| d.split('/').collect())
        .collect()
}

/// The checkpoint ids the chain ref records, in its own first-parent order.
fn chain_entry_ids(repo: &Path) -> Vec<String> {
    let out = Command::new("git")
        .args([
            "log",
            "--first-parent",
            "--reverse",
            "--format=%B",
            "refs/rift/chain/v1",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().strip_prefix("Rift-Chain-Checkpoint:"))
        .map(|id| id.trim().to_owned())
        .collect()
}

/// **Determinism as a property of INVOCATIONS, not of one process — for ALL
/// FOUR workflow verbs.**
///
/// The exit criterion is "same window ⇒ byte-identical `--json`", and
/// the only test that spoke to it called the verb twice inside one process
/// (`workflows/data_tests.rs`). That is `f(x) == f(x)` over warm statics: it
/// cannot see a `HashMap` iteration order that is seeded per process, a
/// `SystemTime` read on a cold path, an address-dependent sort, or a value
/// cached on first use. Two processes can.
///
/// **It covered two verbs of four**, and `workflows/data_tests.rs` said its own
/// gap was closed by "`review_and_investigate_…`" — a sentence naming a test
/// that covers two DIFFERENT verbs from the two it was standing in for. That
/// sentence is corrected alongside this widening; a claim nothing would redden
/// is a claim nobody has tested.
/// `dispatch` and `recap` are driven here now, so all four are.
///
/// **The binary is the freshly built one, structurally.** `common::rift_cmd`
/// spawns the path Cargo exports as `CARGO_BIN_EXE_<name>`, which it defines
/// only after building this package's `[[bin]]` targets for this test run — so
/// the stale-binary hazard (a suite driving a binary it never relinked) cannot
/// arise through this door, and `only_the_chokepoint_names_the_binary` keeps it
/// the only door. (That guard is LEXICAL, and it caught this very paragraph
/// when it first spelled the variable out in full.)
///
/// The non-vacuity controls matter as much as the equalities: a test that only
/// asserts "twice the same" passes just as well for a verb that always answers
/// the same thing, which is precisely what `rift review` did when its data layer
/// was reading a sentinel and answering zero.
#[test]
fn every_workflow_verb_is_byte_identical_across_two_processes() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    // A real agent tool call BEFORE the commit, so the window holds something
    // `recap` reads and `dispatch` does not: `tool_mix` comes from `tool_call`,
    // which no other fact in this fixture would fill.
    agent_read_hook(&repo, "e2e-live-1", 1, &repo.join("a.txt"), &env);
    let gd = repo.join(".git");
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );

    // The fixture must actually hold a checkpoint, or every equality below is
    // about the empty report.
    let (code, dispatched) = run_rift_stdout(&repo, &["dispatch", "--json"], &env);
    assert_eq!(code, 0, "{dispatched}");
    let d: serde_json::Value = serde_json::from_str(&dispatched).unwrap();
    assert!(
        d["checkpoints"]["count"].as_u64().unwrap_or(0) >= 1,
        "the fixture must hold a checkpoint: {d}"
    );

    let run = |args: &[&str]| -> String {
        let (code, out) = run_rift_stdout(&repo, args, &env);
        assert_eq!(code, 0, "{args:?}: {out}");
        assert!(!out.is_empty(), "{args:?} produced nothing");
        out
    };

    // `rift review --json`, twice, in two processes.
    let review_args = ["review", "--profile", "full", "--json"];
    assert_eq!(
        run(&review_args),
        run(&review_args),
        "two `rift review` processes disagreed"
    );
    // …and a different profile is a different document, so the equality above
    // is not the equality of a constant.
    assert_ne!(
        run(&review_args),
        run(&["review", "--profile", "risk", "--json"]),
        "two profiles produced one document"
    );

    // `rift investigate`, twice, in two processes. The seed is an error
    // signature: an identifier by shape alone, so this asserts the verb rather
    // than the fixture's paths.
    let seed = format!("sha256:{}", "a".repeat(64));
    let other = format!("sha256:{}", "b".repeat(64));
    let inv = ["investigate", seed.as_str(), "--json"];
    assert_eq!(
        run(&inv),
        run(&inv),
        "two `rift investigate` processes disagreed"
    );
    // The scaffold is written by the FIRST run and never overwritten, so the
    // second run reporting the same path is part of the property, not a
    // coincidence of ordering.
    assert!(repo.join("findings.md").exists());

    // A different seed is a different investigation — which was only true of
    // the RENDER until the pack key started carrying the seed's hash.
    let a: serde_json::Value = serde_json::from_str(&run(&inv)).unwrap();
    let b: serde_json::Value =
        serde_json::from_str(&run(&["investigate", other.as_str(), "--json"])).unwrap();
    assert_ne!(
        a["pack"]["pack_key"], b["pack"]["pack_key"],
        "two seeds shared one pack key"
    );

    // `rift dispatch --json`, twice, in two processes — the verb whose document
    // the whole workflow family is built on, and the one this test was named
    // after not covering.
    let dispatch_args = ["dispatch", "--json"];
    assert_eq!(
        run(&dispatch_args),
        run(&dispatch_args),
        "two `rift dispatch` processes disagreed"
    );
    // …and a BOUNDED window is a different document, so the equality above is
    // not the equality of a constant. `--until 0` is before every fact this
    // fixture holds, which also makes the sentinel arm observable: see
    // `a_bounded_recap_excludes_what_the_unbounded_one_reports`.
    assert_ne!(
        run(&dispatch_args),
        run(&["dispatch", "--until", "0", "--json"]),
        "a bounded window produced the unbounded document"
    );

    // `rift recap --json`, twice, on both axes: the agent split and the model
    // split take different branches of the screen, so one axis cannot certify
    // the other.
    for axis in [vec!["recap", "--json"], vec!["recap", "--model", "--json"]] {
        assert_eq!(
            run(&axis),
            run(&axis),
            "two `rift recap` processes disagreed on {axis:?}"
        );
    }
    assert_ne!(
        run(&["recap", "--json"]),
        run(&["recap", "--model", "--json"]),
        "two axes produced one document"
    );
}

/// **`rift recap` over a repository that really holds rows** — the verb had
/// zero integration tests, and the historical trap is exactly the one a
/// vacuous fixture would walk into.
///
/// Native checkpoints now record `created_at` from the capture event's own
/// time; every shard archived BEFORE that fix carries the
/// `CREATED_AT_UNKNOWN` sentinel (`-1`) forever, and for those a BOUNDED
/// window still filters the sentinel: the checkpoint is placed by its
/// sessions or reported as unresolved — the derivation that is the only
/// reason `rift dispatch` stopped answering `checkpoints: 0` on a repository
/// holding 103. So this drives the shipped binary over a repository whose rows
/// were written by real hooks, and asserts the counts are non-zero rather than
/// asserting the shape of an empty document.
#[test]
fn recap_reports_the_rows_real_hooks_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    // A report verb refuses an uninitialized repository outright
    // (`error: not_initialized`), so the marker directory is part of the
    // fixture rather than an incantation.
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    agent_read_hook(&repo, "e2e-live-1", 1, &repo.join("a.txt"), &env);
    let gd = repo.join(".git");
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );

    let (code, out) = run_rift_stdout(&repo, &["recap", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let d: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(d["schema_version"], 2, "the screened contract: {d}");
    assert!(
        d["sessions"].as_u64().unwrap_or(0) >= 1,
        "recap reported no session on a repo whose hooks wrote one: {d}"
    );
    assert!(
        d["checkpoints"].as_u64().unwrap_or(0) >= 1,
        "recap reported no checkpoint on a repo holding one: {d}"
    );
    assert!(
        d["tool_mix"].as_array().is_some_and(|a| !a.is_empty()),
        "the `Read` hook wrote a tool_call and recap did not see it: {d}"
    );
    assert!(
        d["split"]["counts"]
            .as_array()
            .is_some_and(|a| !a.is_empty()),
        "recap split nothing: {d}"
    );
    // The agent axis rides verbatim — `claude-code` is a registered adapter
    // token — which is the half of `crate::screen` a hostile fixture cannot
    // show.
    assert_eq!(d["split"]["by"], "agent");
    assert_eq!(d["split"]["counts"][0]["key"], "claude-code");

    // The human render is not the JSON, and it is not empty either.
    let (hcode, human) = run_rift_stdout(&repo, &["recap"], &env);
    assert_eq!(hcode, 0, "{human}");
    assert!(
        human.contains("recap  sessions") && human.contains("claude-code"),
        "the human recap must resolve what the document counts: {human}"
    );
}

/// **The window bound, documented from the surface that exposes it.**
///
/// A checkpoint this fixture archives carries its capture event's own
/// `created_at` (the `-1` production write is closed), and `--until 1`
/// ends the window at epoch+1ms — before every fact this fixture holds — so
/// the bounded recap must report zero where the unbounded one reports rows,
/// and the two must not be the same document. (Shards archived before the
/// fix carry the sentinel forever; for those rows the window still places
/// through sessions or reports unresolved, and the sentinel arm keeps its
/// own pins in `workflows::data_tests`.)
#[test]
fn a_bounded_recap_excludes_what_the_unbounded_one_reports() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    agent_read_hook(&repo, "e2e-live-1", 1, &repo.join("a.txt"), &env);
    let gd = repo.join(".git");
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap(),
            ],
            Some(""),
            &env,
        ),
        0
    );

    let read = |args: &[&str]| -> serde_json::Value {
        let (code, out) = run_rift_stdout(&repo, args, &env);
        assert_eq!(code, 0, "{args:?}: {out}");
        serde_json::from_str(&out).unwrap()
    };
    let unbounded = read(&["recap", "--json"]);
    let bounded = read(&["recap", "--since", "0", "--until", "1", "--json"]);

    assert!(
        unbounded["sessions"].as_u64().unwrap_or(0) >= 1,
        "the control must be non-empty: {unbounded}"
    );
    assert_eq!(
        bounded["sessions"], 0,
        "a window ending at epoch+1ms holds no session: {bounded}"
    );
    assert_eq!(
        bounded["checkpoints"], 0,
        "…and no checkpoint: its recorded capture time sits outside a window \
         that ends at epoch+1ms: {bounded}"
    );
    assert!(
        bounded["tool_mix"].as_array().is_some_and(Vec::is_empty),
        "the tool histogram is anchored to its own start times: {bounded}"
    );
}

// --- the deferred settle -----------------------------------------------------

/// A native Claude Code turn as the hooks deliver it, against the real binary:
/// SessionStart (naming the transcript), UserPromptSubmit, PreToolUse, Stop.
/// Returns the transcript path. The transcript does NOT exist yet when the
/// Stop runs — the caller writes it afterwards, which is the contract under
/// test: a Stop cannot have read what was not there.
fn drive_native_turn(repo: &Path, session: &str, env: &[(&str, &str)]) -> std::path::PathBuf {
    let transcript = repo.join("..").join(format!("{session}.jsonl"));
    let hook = |verb: &str, body: serde_json::Value| {
        let mut body = body;
        body["session_id"] = serde_json::Value::String(session.to_owned());
        body["hook_event_name"] = serde_json::Value::String(verb.to_owned());
        assert_eq!(
            run_rift_in(
                repo,
                &["hooks", "claude-code", verb],
                &body.to_string(),
                env
            ),
            0,
            "a capture hook never fails the agent"
        );
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
    let transcript = drive_native_turn(&repo, session, &env);
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
    let transcript = drive_native_turn(&repo, session, &env);
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
    let transcript = tmp
        .path()
        .join("agent-transcripts")
        .join("e2e-live-1.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(
        &transcript,
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"add a feature"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
            "\n"
        ),
    )
    .unwrap();
    std::fs::write(
        repo.join(".git").join("rift-sessions").join("e2e-live-1.json"),
        format!(
            r#"{{"schema":1,"session_id":"e2e-live-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{{}},"transcript_path":{}}}"#,
            json_str(&transcript)
        ),
    )
    .unwrap();
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    // A commit whose trailer claims an id that will never land.
    let predicted = "0".repeat(64);
    std::fs::write(repo.join("b.txt"), "two\n").unwrap();
    git_in(&repo, &["add", "b.txt"]);
    git_in(
        &repo,
        &[
            "commit",
            "-q",
            "-m",
            &format!("carry\n\nRift-Checkpoint: {predicted}"),
        ],
    );
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("XDG_DATA_HOME", data.to_str().unwrap()),
        ("LOCALAPPDATA", data.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    // The archive anchors the LANDED checkpoint to that commit.
    let gd = repo.join(".git");
    assert_eq!(
        run_rift(
            &repo,
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                gd.to_str().unwrap()
            ],
            Some(""),
            &env,
        ),
        0
    );
    // Shard directories are `<2 hex>/<62 hex>`; the id is the 64 hex digits.
    let landed: Vec<String> = checkpoint_shards(&repo)
        .into_iter()
        .map(|dir| dir.replace('/', ""))
        .collect();
    assert_eq!(landed.len(), 1, "one shard landed: {landed:?}");
    assert_ne!(
        landed[0], predicted,
        "the trailer's prediction never landed"
    );

    let branch = git_in(&repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .trim()
        .to_owned();
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
        v["checkpoint_id"], landed[0],
        "the landed id, not the predicted one: {v}"
    );
    assert_eq!(v["sessions"][0]["restored"], "restored", "{v}");
    assert!(transcript.is_file(), "the transcript is back in place");
}

/// **A planted capture note is reported by doctor as a finding, and said
/// once** (cause E of `turn_boundary`'s register, 2026-08-29). The
/// `capture_queue` leg OBSERVES the note channel (`peek_errors`) and reports
/// every marker under `capture_queue.notes` — the finding class — before any
/// leg that might clear it. What clears it is the soak leg's existing,
/// deliberate contract ("said here and CONSUMED here": a marker that survived
/// being reported would print forever), so the SECOND run reports nothing and
/// the marker is gone. Only the read-only viewer doctor (Canopy `/doctor`)
/// peeks throughout; it has no CLI entry, so the CLI pin is: reported as a
/// finding on the run it is found, exactly once.
#[test]
fn a_planted_capture_note_is_reported_by_doctor_as_a_finding_and_said_once() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];
    // The note channel's shape, planted by hand: `<key>.error` under the
    // capture queue, one line, the body a boundary-snapshot failure would leave.
    let qdir = repo.join(".git").join("rift-capture-queue");
    std::fs::create_dir_all(&qdir).unwrap();
    let marker = qdir.join("boundary-0123456789abcdef.error");
    let body =
        "turn-boundary snapshot failed; the session's H_i/A_i windows are incomplete (planted)";
    std::fs::write(&marker, body).unwrap();

    let (code, out) = run_rift_stdout(&repo, &["doctor", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let d: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    let notes = d["capture_queue"]["notes"].as_array().unwrap();
    assert!(
        notes.iter().any(|n| n == body),
        "the planted note is not a finding: {d}"
    );
    assert!(
        !marker.exists(),
        "the soak leg's said-once contract must have consumed the marker"
    );

    let (code, out) = run_rift_stdout(&repo, &["doctor", "--json"], &env);
    assert_eq!(code, 0, "{out}");
    let d: serde_json::Value = serde_json::from_slice(out.as_bytes()).unwrap();
    assert!(
        d["capture_queue"]["notes"].as_array().unwrap().is_empty(),
        "said once, not twice: {d}"
    );
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
/// the done-marker, all after the hook is gone. The test polls the marker
/// rather than asserting a duration, for the reason the post-commit twin
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
    let transcript = drive_native_turn(&repo, session, &env);
    write_native_transcript(&transcript);
    let (usage, stop) = settled_usage(&cache, &repo, session).await;
    assert!(
        usage.is_empty(),
        "nothing settled before the end: {usage:?}"
    );
    let stop = stop.expect("turn 1 closed");

    let body = serde_json::json!({"session_id": session, "hook_event_name": "SessionEnd", "reason": "exit"});
    assert_eq!(
        run_rift_in(
            &repo,
            &["hooks", "claude-code", "SessionEnd"],
            &body.to_string(),
            &env
        ),
        0,
        "a capture hook never fails the agent"
    );
    assert!(
        repo.join(".git").join("rift-agent-queue").is_dir(),
        "the hook left a durable record of the condense it owes"
    );

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(45);
    while std::time::Instant::now() < deadline && agent_queue_state(&repo, session) != (true, 0) {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
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
/// job found again after the work landed — condenses nothing twice: the
/// machine has no edge out of `ended`, and the journal's done-marker refuses
/// even a job planted by hand.
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
/// recall ledger, cost) sees it as if it had been measured inline.
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
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
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
    let blob_oid = git_in(&repo, &["rev-parse", "HEAD:a.txt"])
        .trim()
        .to_owned();
    // The agent read "one\n"; the file now says something else.
    std::fs::write(repo.join("a.txt"), "two\n").unwrap();
    let plant = |call: &str, sha: &str, oid: &str| {
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
    };
    let from_blob = "a".repeat(64);
    plant(
        &from_blob,
        "2c8b08da5ce60398e1f19af0e5dccc744df274b826abe585eaba68c525434806",
        &blob_oid,
    );
    let vanished = "b".repeat(64);
    plant(&vanished, &"f".repeat(64), &"0".repeat(40));

    let (code, stdout) = run_rift_stdout(&repo, &["--json", "doctor", "--fix"], &env);
    assert_eq!(code, 0, "{stdout}");
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
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let queue = doc
        .pointer("/capture_queue")
        .or_else(|| doc.pointer("/data/capture_queue"))
        .unwrap();
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
    let (_, stdout) = run_rift_stdout(&repo, &["--json", "doctor"], &env);
    let doc: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let queue = doc
        .pointer("/capture_queue")
        .or_else(|| doc.pointer("/data/capture_queue"))
        .unwrap();
    assert_eq!(queue["agent_jobs"]["pending"], 1, "still owed: {queue}");
    assert_eq!(
        queue["notes"],
        serde_json::json!([]),
        "said once, not twice"
    );
}

/// The cached machine fingerprint, through the real binary with a counting shim
/// for the fingerprint read: the FIRST session's first hook spawns `ioreg`
/// once and caches the salted digest under the cache root; the next
/// session's first hook spawns nothing and binds to the same machine id.
/// macOS only — that is the platform whose fingerprint is a child process.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn the_machine_fingerprint_is_read_once_per_install_not_once_per_session() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let shims = tmp.path().join("shims");
    std::fs::create_dir_all(&shims).unwrap();
    let log = tmp.path().join("spawns.log");
    let shim = shims.join("ioreg");
    std::fs::write(
        &shim,
        "#!/bin/sh\necho ioreg >> \"$RIFT_E2E_SPAWN_LOG\"\nexec /usr/sbin/ioreg \"$@\"\n",
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        shims.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
        ("PATH", path.as_str()),
        ("RIFT_E2E_SPAWN_LOG", log.to_str().unwrap()),
    ];
    let spawns = || {
        std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .count()
    };

    let start = |session: &str| {
        let body = serde_json::json!({"session_id": session, "hook_event_name": "SessionStart", "source": "startup"});
        assert_eq!(
            run_rift_in(
                &repo,
                &["hooks", "claude-code", "SessionStart"],
                &body.to_string(),
                &env
            ),
            0
        );
    };
    start("01JMACHINEIDCACHE0000000001");
    assert_eq!(spawns(), 1, "the first session's first hook derived the id");
    let cache_file = cache.join("machine-id.json");
    assert!(cache_file.exists(), "and cached the salted digest");
    let body = std::fs::read_to_string(&cache_file).unwrap();
    let raw = Command::new("/usr/sbin/ioreg")
        .args(["-rd1", "-c", "IOPlatformExpertDevice"])
        .output()
        .unwrap();
    let uuid = String::from_utf8_lossy(&raw.stdout)
        .lines()
        .find(|l| l.contains("IOPlatformUUID"))
        .and_then(|l| l.split('"').nth(3).map(str::to_owned))
        .expect("this machine has a platform uuid");
    assert!(
        !body.contains(&uuid),
        "the raw hardware id is never on disk"
    );

    start("01JMACHINEIDCACHE0000000002");
    assert_eq!(
        spawns(),
        1,
        "the second session's first hook spawned nothing"
    );

    let repo_id = rift_git::repo_id_for(&repo).unwrap();
    let store =
        rift_store::open_capture_projection(rift_store::projection_db_under(&cache, &repo_id))
            .unwrap();
    let rows = rift_store::CanonicalStore::query(
        &store,
        "SELECT machine_id FROM session_binding ORDER BY session_id",
        &[],
    )
    .await
    .unwrap();
    let ids: Vec<String> = rows.iter().map(|r| r.get_text(0).unwrap()).collect();
    assert_eq!(ids.len(), 2, "both sessions bound");
    assert_eq!(ids[0], ids[1], "to the same machine id, derived and cached");
}

/// Through the shipped binary: an agent hook fired with a cwd
/// **outside any repository** exits 0 and writes nothing at all.
///
/// The retired fallback answered `<cwd>/.git` when discovery found no
/// repository, and the capture path then created it — so a hook whose cwd
/// followed the agent's shell out of a checkout left a directory named `.git`
/// wherever it was standing, which the next discovery walking up through it
/// read as a repository of its own. Five of those existed in this tree at once.
/// Both shapes are driven here: a plain directory, and one already carrying the
/// empty `.git` an older build left behind.
#[test]
fn a_hook_outside_any_repository_writes_nothing() {
    /// Every file under `dir`, relative and sorted — the whole of what a run
    /// wrote, rather than a guess at which path to look for.
    fn files_under(dir: &Path) -> Vec<String> {
        let mut out = Vec::new();
        let mut queue = vec![dir.to_path_buf()];
        while let Some(d) = queue.pop() {
            let Ok(entries) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    queue.push(p);
                } else {
                    out.push(p.strip_prefix(dir).unwrap_or(&p).display().to_string());
                }
            }
        }
        out.sort();
        out
    }

    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let data = tmp.path().join("data");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("XDG_DATA_HOME", data.to_str().unwrap()),
        ("LOCALAPPDATA", data.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ];

    // A tempdir has no `.git` in any ancestor, which is precisely the
    // condition — a hook fired from a directory that belongs to no checkout.
    let plain = tmp.path().join("outside");
    std::fs::create_dir_all(&plain).unwrap();
    let haunted = tmp.path().join("haunted");
    rift_git::test_support::phantom_gitdir(&haunted).unwrap();

    for (n, dir) in [&plain, &haunted].into_iter().enumerate() {
        let session = format!("01JOUTSIDE00000000000000{n}");
        // SessionStart is what opens a session cache; PostToolUse is what
        // writes captured facts. Neither has anywhere to write.
        agent_hook(dir, "SessionStart", &session, 1, "{}", &env);
        agent_hook(
            dir,
            "PostToolUse",
            &session,
            1,
            r#"{"tool_name":"Read","tool_input":{"file_path":"/etc/hosts"}}"#,
            &env,
        );
    }

    assert!(
        !plain.join(".git").exists(),
        "the `<cwd>/.git` fallback is retired: a hook mints no repository"
    );
    assert_eq!(
        files_under(&plain),
        Vec::<String>::new(),
        "a hook outside a repository leaves the directory it ran in untouched"
    );
    assert_eq!(
        files_under(&haunted),
        Vec::<String>::new(),
        "and does not adopt a phantom `.git` someone else left behind"
    );
    assert_eq!(
        files_under(&cache),
        Vec::<String>::new(),
        "no projection is minted for a path that is not a repository"
    );
}

// --- the read ledger ---------------------------------------------------------

/// The files in this checkout's read-ledger directory, by name.
fn read_ledger_files(repo: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo.join(".rift").join("state").join("reads"))
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Through the real binary: the agent edits a file it read in full, and the
/// window's ledger says where the file changed, by how much, and that the
/// window still holds every line of the revision it left behind.
///
/// The bytes the edit put there are bytes the agent authored, so the follow-up
/// read that verifies them is the largest thing this whole mechanism exists to
/// answer. Answering it needs three facts to be right at once, from the shipped
/// binary and not from a unit's idea of one: the place, in the numbering of the
/// new bytes; the shift; and the coverage, which is only whole because the
/// window had read the whole file first.
#[test]
fn an_edit_hook_leaves_the_window_holding_the_bytes_it_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let file = repo.join("b.txt");
    std::fs::write(&file, "a\nold\nc\n").unwrap();
    let read = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}}}}"#,
        json_str(&file)
    );
    agent_hook(&repo, "PreToolUse", "e2e-live-1", 1, &read, &env);

    // The edit the agent then makes, and the bytes it leaves behind.
    let edit = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":{},"old_string":"old\n","new_string":"new\nnewer\n","replace_all":false}}}}"#,
        json_str(&file)
    );
    std::fs::write(&file, "a\nnew\nnewer\nc\n").unwrap();
    agent_hook(&repo, "PostToolUse", "e2e-live-1", 2, &edit, &env);

    let names = read_ledger_files(&repo);
    let name = names.first().expect("one context window, one ledger");
    let path = repo.join(".rift").join("state").join("reads").join(name);
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    let kinds: Vec<&str> = rows.iter().map(|r| r["k"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["read", "edit"], "{rows:?}");

    let edited = &rows[1];
    assert_eq!(edited["p"], "b.txt", "the checkout's own spelling");
    assert_eq!(
        edited["h_old"], rows[0]["h"],
        "the revision the window had read is the one the edit started from"
    );
    assert_eq!(
        edited["sites"],
        serde_json::json!([[2, 3]]),
        "lines 2-3 of the file as it is now: {edited}"
    );
    assert_eq!(edited["shift"], 1, "one line longer than it was");
    assert_eq!(
        edited["full"], true,
        "it had read every line, and it wrote the difference"
    );
    assert_eq!(
        edited["h_new"].as_str().map(str::len),
        Some(64),
        "a revision, in the only shape a reader accepts: {edited}"
    );
    assert!(
        !edited.as_object().unwrap().contains_key("content")
            && !serde_json::to_string(edited)
                .unwrap()
                .contains("new\\nnewer"),
        "identifiers and numbers, never a byte of the file: {edited}"
    );
}

/// Through the real binary: a read arrives, and the window's own ledger says
/// which lines of which revision it was handed — one file under this
/// checkout's run state, named for the context window and nothing else.
///
/// The second read of the same lines is the whole point. Nothing is declined
/// while the lever is in observe, but the ledger says out loud that a decline
/// is what those bytes were worth, which is the number the flip is decided on.
#[test]
fn a_read_hook_leaves_the_window_a_ledger_of_the_lines_it_was_handed() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let file = repo.join("a.txt");
    let read = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":1}}}}"#,
        json_str(&file)
    );

    agent_hook(&repo, "PreToolUse", "e2e-live-1", 1, &read, &env);

    let names = read_ledger_files(&repo);
    assert_eq!(names.len(), 1, "one context window, one ledger: {names:?}");
    let name = names.first().unwrap();
    let stem = name.strip_suffix(".jsonl").expect("a ledger file: {name}");
    assert_eq!(
        stem.len(),
        64,
        "named for the window, not the session: {name}"
    );
    assert!(
        stem.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "the name is a hash and nothing else: {name}"
    );

    let path = repo.join(".rift").join("state").join("reads").join(name);
    let first: serde_json::Value = serde_json::from_str(
        std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["k"], "read");
    assert_eq!(
        first["p"], "a.txt",
        "the checkout's own spelling of the path"
    );
    assert_eq!(first["lo"], 1);
    assert_eq!(first["hi"], 1);
    assert_eq!(
        first["n"], 1,
        "the file's own length, counted while hashing"
    );
    assert_eq!(
        first["h"].as_str().map(str::len),
        Some(64),
        "a revision, in the only shape a reader accepts: {first}"
    );

    // The same lines again, at the same revision.
    agent_hook(&repo, "PreToolUse", "e2e-live-1", 2, &read, &env);
    let rows: Vec<serde_json::Value> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
        .collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert_eq!(
        rows[1]["k"], "observe",
        "the repeat is what a decline would be for, and nothing was declined"
    );

    // And the compaction signal empties the window, so the next read of those
    // lines is a first read again rather than a repeat.
    agent_hook(&repo, "PreCompact", "e2e-live-1", 2, "{}", &env);
    agent_hook(&repo, "PreToolUse", "e2e-live-1", 3, &read, &env);
    let kinds: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|row| {
            serde_json::from_str::<serde_json::Value>(row).unwrap()["k"]
                .as_str()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    assert_eq!(kinds, vec!["read", "observe", "evict", "read"], "{kinds:?}");
}

/// The kinds of line the one context window's ledger holds, in order — the
/// record a decision leaves behind, read back from the file the product wrote.
fn ledger_kinds(repo: &Path) -> Vec<String> {
    let names = read_ledger_files(repo);
    let name = names.first().expect("one context window, one ledger");
    ledger_kinds_of(repo, name)
}

/// The same, for a walk that puts two context windows in one repository: one
/// ledger file per window, named for the window rather than for the order the
/// directory happens to list them in.
fn ledger_kinds_of(repo: &Path, name: &str) -> Vec<String> {
    std::fs::read_to_string(repo.join(".rift").join("state").join("reads").join(name))
        .unwrap()
        .lines()
        .map(|row| {
            serde_json::from_str::<serde_json::Value>(row).unwrap()["k"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect()
}

/// The repository asks for declines, for this family, in its own policy file.
///
/// Written as the operator would write it — the tables, not a constant — so
/// what this walk exercises is the whole route from a file on disk to an
/// object on stdout.
///
/// The line floor is held at one throughout: these fixtures are a handful of
/// lines each, the floor is a trade with its own tests, and a walk about the
/// route from a file on disk to an object on stdout should not turn on it.
fn enforce_for(repo: &Path, family: &str) {
    let policies = repo.join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(
        policies.join("serve.toml"),
        format!(
            "[interception]\nmode = \"enforce\"\nmin_slice_lines = 1\n\n\
             [interception.agents]\n{family} = \"enforce\"\n"
        ),
    )
    .unwrap();
}

/// The decision one hook wrote to stdout: the reason a read was declined with,
/// or `None` where the call was left alone.
fn declined_with(stdout: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(stdout.trim()).ok()?;
    // Both pinned envelopes, because a reader that knew only one of them would
    // quietly pass on the other family however it answered.
    let (decision, reason) = value.get("hookSpecificOutput").map_or_else(
        || (value.get("decision"), value.get("reason")),
        |nested| {
            (
                nested.get("permissionDecision"),
                nested.get("permissionDecisionReason"),
            )
        },
    );
    if decision?.as_str()? != "deny" {
        return None;
    }
    Some(reason?.as_str()?.to_owned())
}

/// The whole walk, through the shipped binary and the repository's own policy
/// file: read a file in full, edit it, read it back to check the edit.
///
/// The verification read is the largest measured pool this mechanism exists
/// for, and every clause of the answer it gets is asserted here rather than in
/// a unit's idea of one: declined exactly once, with the places the file
/// changed in the numbering the reader is about to see, the shift, and a
/// window worth reading instead. Then the two escapes — the narrow read of the
/// changed lines, which is the behaviour the frame asks for, and the identical
/// repeat, which is what bounds a model that answers a decline by asking again
/// — both run untouched.
#[test]
fn a_verification_read_is_declined_once_and_the_ways_out_of_it_are_open() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    enforce_for(&repo, "claude-code");
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let file = repo.join("b.txt");
    std::fs::write(&file, "a\nold\nc\n").unwrap();
    let whole = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}}}}"#,
        json_str(&file)
    );

    let first = agent_hook_out(
        &repo,
        "claude-code",
        "PreToolUse",
        "e2e-gate",
        1,
        &whole,
        &env,
    );
    assert_eq!(declined_with(&first), None, "the first read is the saving");

    let edit = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":{},"old_string":"old\n","new_string":"new\nnewer\n","replace_all":false}}}}"#,
        json_str(&file)
    );
    std::fs::write(&file, "a\nnew\nnewer\nc\n").unwrap();
    agent_hook(&repo, "PostToolUse", "e2e-gate", 2, &edit, &env);

    let verify = agent_hook_out(
        &repo,
        "claude-code",
        "PreToolUse",
        "e2e-gate",
        3,
        &whole,
        &env,
    );
    let reason = declined_with(&verify).expect("the verification read is declined: {verify}");
    assert!(
        reason.contains("changed only at lines 2-3"),
        "the places, in the numbering of the bytes it is about to see: {reason}"
    );
    assert!(reason.contains("(+1 lines)"), "and the shift: {reason}");
    assert!(
        reason.contains("Read lines 1-4"),
        "and a window inside the file: {reason}"
    );
    assert!(
        !reason.contains("newer"),
        "identifiers and numbers, never a byte of the file: {reason}"
    );

    let narrow = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":2,"limit":2}}}}"#,
        json_str(&file)
    );
    let inside = agent_hook_out(
        &repo,
        "claude-code",
        "PreToolUse",
        "e2e-gate",
        4,
        &narrow,
        &env,
    );
    assert_eq!(
        declined_with(&inside),
        None,
        "reading the changed lines is the behaviour the reason asks for"
    );

    let again = agent_hook_out(
        &repo,
        "claude-code",
        "PreToolUse",
        "e2e-gate",
        5,
        &whole,
        &env,
    );
    assert_eq!(
        declined_with(&again),
        None,
        "one decline per call, so the repeat is honoured"
    );

    assert_eq!(
        ledger_kinds(&repo),
        vec!["read", "edit", "deny", "read", "read"],
        "declined once, then handed the bytes twice and recorded as holding them"
    );
}

/// The verification walk on Antigravity, through the shipped binary: read the
/// whole file, replace one line with two, read it back to check. Returns what
/// the hook answered that third call with.
fn antigravity_verification_walk(repo: &Path, file: &Path, env: &[(&str, &str)]) -> String {
    let view = format!(
        r#"{{"toolCall":{{"name":"view_file","args":{{"AbsolutePath":{},"StartLine":0,"EndLine":500}}}}}}"#,
        json_str(file)
    );
    let replace = format!(
        r#"{{"toolCall":{{"name":"replace_file_content","args":{{"TargetFile":{},"TargetContent":"old\n","ReplacementContent":"new\nnewer\n","AllowMultiple":false}}}}}}"#,
        json_str(file)
    );
    std::fs::write(file, "a\nold\nc\n").unwrap();
    assert_eq!(
        declined_with(&agent_hook_out(
            repo,
            "antigravity",
            "PreToolUse",
            "e2e-agy-echo",
            1,
            &view,
            env,
        )),
        None,
        "the first read is the saving"
    );
    std::fs::write(file, "a\nnew\nnewer\nc\n").unwrap();
    agent_hook_out(
        repo,
        "antigravity",
        "PostToolUse",
        "e2e-agy-echo",
        2,
        &replace,
        env,
    );
    agent_hook_out(
        repo,
        "antigravity",
        "PreToolUse",
        "e2e-agy-echo",
        3,
        &view,
        env,
    )
}

/// The same walk on Claude Code, whose edit result is pinned to tell the model
/// the file it just wrote is current in its context.
fn claude_code_verification_walk(repo: &Path, file: &Path, env: &[(&str, &str)]) -> String {
    let whole = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}}}}"#,
        json_str(file)
    );
    let edit = format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":{},"old_string":"old\n","new_string":"new\nnewer\n","replace_all":false}}}}"#,
        json_str(file)
    );
    std::fs::write(file, "a\nold\nc\n").unwrap();
    agent_hook_out(
        repo,
        "claude-code",
        "PreToolUse",
        "e2e-cc-echo",
        1,
        &whole,
        env,
    );
    std::fs::write(file, "a\nnew\nnewer\nc\n").unwrap();
    agent_hook(repo, "PostToolUse", "e2e-cc-echo", 2, &edit, env);
    agent_hook_out(
        repo,
        "claude-code",
        "PreToolUse",
        "e2e-cc-echo",
        3,
        &whole,
        env,
    )
}

/// ★ One policy file, both families named `enforce`, one walk each: the family
/// whose edit result is pinned to say the model holds what it wrote is
/// declined once, and the family whose edit result nothing pins is not
/// declined at all.
///
/// This is the exact configuration an operator may legitimately write, so the
/// guard cannot live in a mode. Antigravity's own artifact says nothing about
/// what `replace_file_content` returns, which makes the verification read the
/// only sight of the edit that agent has; taking it away would buy a saving
/// with the one thing the read exists to provide. The edit is
/// still recorded — the coverage of the old bytes still has to stop being
/// coverage — so what is missing from that ledger is the decision, not the
/// line.
#[test]
fn a_family_whose_edit_echo_is_not_pinned_is_never_declined_on_its_own_edit() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let policies = repo.join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(
        policies.join("serve.toml"),
        "[interception]\nmode = \"enforce\"\nmin_slice_lines = 1\n\n\
         [interception.agents]\nantigravity = \"enforce\"\nclaude-code = \"enforce\"\n",
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let file = repo.join("b.txt");

    let verify = antigravity_verification_walk(&repo, &file, &env);
    assert_eq!(
        declined_with(&verify),
        None,
        "a decline here would remove this agent's only sight of its own edit: {verify}"
    );
    let unpinned = read_ledger_files(&repo);
    let [agy] = unpinned.as_slice() else {
        panic!("one window so far, got {unpinned:?}");
    };
    assert_eq!(
        ledger_kinds_of(&repo, agy),
        vec!["read", "edit", "read"],
        "the edit is recorded and the read runs; only the decision is absent"
    );

    // The same policy file, the same shape of walk, on the family whose edit
    // result is pinned: still declined, exactly once. The guard is the
    // family's, not a stand-down for everyone.
    let reason = declined_with(&claude_code_verification_walk(&repo, &file, &env))
        .expect("the pinned family is still declined");
    assert!(
        reason.contains("changed only at lines 2-3"),
        "with the places, in the numbering it is about to see: {reason}"
    );
    let both = read_ledger_files(&repo);
    let cc = both
        .iter()
        .find(|name| *name != agy)
        .expect("the second window has its own ledger");
    assert_eq!(ledger_kinds_of(&repo, cc), vec!["read", "edit", "deny"]);
}

/// Repository-wide enforcement reaches Antigravity without an agent-specific
/// override. Retry telemetry does not demote support or policy; the identical
/// third call is allowed as the universal one-deny safety escape.
#[test]
fn repository_wide_enforcement_reaches_antigravity_with_one_retry_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let policies = repo.join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(
        policies.join("serve.toml"),
        "[interception]\nmode = \"enforce\"\nmin_slice_lines = 1\n",
    )
    .unwrap();
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    let file = repo.join("b.txt");
    std::fs::write(&file, "a\nb\nc\nd\n").unwrap();
    let view = format!(
        r#"{{"toolCall":{{"name":"view_file","args":{{"AbsolutePath":{},"StartLine":0,"EndLine":4}}}}}}"#,
        json_str(&file)
    );

    let first = agent_hook_out(
        &repo,
        "antigravity",
        "PreToolUse",
        "e2e-agy",
        1,
        &view,
        &env,
    );
    assert_eq!(declined_with(&first), None, "the first read runs");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(first.trim()).unwrap(),
        serde_json::json!({"decision": "allow"})
    );
    let second = agent_hook_out(
        &repo,
        "antigravity",
        "PreToolUse",
        "e2e-agy",
        2,
        &view,
        &env,
    );
    assert!(
        declined_with(&second).is_some(),
        "the unchanged repeat is declined"
    );
    let third = agent_hook_out(
        &repo,
        "antigravity",
        "PreToolUse",
        "e2e-agy",
        3,
        &view,
        &env,
    );
    assert_eq!(declined_with(&third), None, "the identical retry runs");

    assert_eq!(
        ledger_kinds(&repo),
        vec!["read", "deny", "read"],
        "the ledger records the active decision and bounded retry"
    );
}

/// Drive one native agent hook and hand back what it wrote to stdout — the
/// bytes the client parses, which is the only place an injection can be
/// observed as the agent sees it.
fn agent_hook_stdout(
    repo: &Path,
    verb: &str,
    session: &str,
    turn: u32,
    payload: &str,
    env: &[(&str, &str)],
) -> String {
    let body = format!(r#"{{"schema":1,"session":"{session}","turn":{turn},"payload":{payload}}}"#);
    let mut child = rift_cmd(repo)
        .args(["hooks", "claude-code", verb])
        .envs(env.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success(), "agent hook never fails");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Through the real binary: a session reads a file, its context is compacted,
/// and the hook that starts the session again hands it back the map of what it
/// stopped holding — once, on the family's own pinned channel, with the verdict
/// that lets the model skip the re-read altogether.
///
/// This is the whole boundary in one walk, and each step is a fact the unit
/// tests cannot state: that the second boundary verb is wired at all (the
/// family's pinned surface carries two, and only one of them was written on
/// before), that the map travels in the field the client reads, that the ledger
/// lines the compaction did not delete are what it is built from, and that the
/// same hook arriving twice does not hand the map over twice.
#[test]
fn a_compacted_context_is_handed_what_it_stopped_holding() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    let file = repo.join("a.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let read = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}}}}"#,
        json_str(&file)
    );
    agent_hook(&repo, "PreToolUse", "e2e-live-1", 1, &read, &env);
    agent_hook(&repo, "PreCompact", "e2e-live-1", 2, "{}", &env);

    let out = agent_hook_stdout(
        &repo,
        "SessionStart",
        "e2e-live-1",
        3,
        r#"{"source":"compact"}"#,
        &env,
    );
    let response: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("the hook wrote an object the client can read: {e} in {out:?}"));
    assert_eq!(
        response["hookSpecificOutput"]["hookEventName"], "SessionStart",
        "{response}"
    );
    let map = response["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("the map travels in the family's pinned context field");
    assert!(map.contains("a.txt@"), "{map}");
    assert!(map.contains("L1-3"), "the lines it held: {map}");
    assert!(
        map.contains("unchanged since your read"),
        "nothing moved, so the re-read can be skipped: {map}"
    );
    assert!(
        !map.contains("one\ntwo"),
        "identifiers and numbers, never a byte of the file: {map}"
    );

    // The same boundary again hands over no second map.
    let again = agent_hook_stdout(
        &repo,
        "SessionStart",
        "e2e-live-1",
        4,
        r#"{"source":"compact"}"#,
        &env,
    );
    assert!(
        !again.contains("stopped holding") && !again.contains("Context was compacted"),
        "one eviction, one map: {again}"
    );

    // And the read after the compaction is a first read again: the ledger kept
    // the lines it was built from, and they satisfy nothing.
    agent_hook(&repo, "PreToolUse", "e2e-live-1", 5, &read, &env);
    assert_eq!(
        ledger_kinds(&repo),
        vec!["read", "evict", "evict", "hold", "evict", "read"],
        "the pre-eviction lines stay, the map records what it announced, and \
         the re-read is not a repeat"
    );
}

/// The key one context window's local state is named by: the session it
/// belongs to and the agent inside it, hashed together with a separator.
///
/// Spelled out here rather than imported because the production function is
/// private to the binary. That is the point of stating it: if the two ever
/// disagree, this walk finds no marker where it expects one.
fn window_key(session: &str, agent_id: &str) -> String {
    let mut bytes = session.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(agent_id.as_bytes());
    rift_hashchain::leaf_hash(&bytes).to_hex()
}

/// Through the real binary: the window a subagent runs in is introduced at the
/// one boundary it has, exactly once, and its parent's own introduction is
/// neither spent by it nor owed to it.
///
/// Every step is a fact the unit tests cannot state. That the family answers on
/// this verb at all — the pinned row and the recorded run are both new, and
/// before them a subagent received nothing, because neither hook a session is
/// introduced at fires inside one. That the claim is keyed by the **window**:
/// the payload names the *parent's* session id, so a marker keyed by the
/// session alone turns this walk into one pointer and three silences, in
/// whichever order the hooks happened to arrive. That two subagents of one turn
/// are two windows. And that a file only a subagent read is not announced back
/// to the parent as something it has read itself.
#[test]
fn a_subagents_window_is_introduced_once_at_the_verb_that_opens_it() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    let session = "e2e-sub-1";
    let context_of = |out: &str, verb: &str| -> String {
        let response: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| {
            panic!("the hook wrote an object the client can read: {e} in {out:?}")
        });
        assert_eq!(
            response["hookSpecificOutput"]["hookEventName"], verb,
            "{response}"
        );
        response["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_else(|| panic!("the text travels in this family's field: {response}"))
            .to_owned()
    };

    // The main thread is introduced at a boundary it has.
    let parent = context_of(
        &agent_hook_stdout(&repo, "UserPromptSubmit", session, 1, "{}", &env),
        "UserPromptSubmit",
    );
    assert!(
        parent.contains("rift recall") && parent.contains("rift agent-help"),
        "the pointer names the surface it points at: {parent}"
    );

    // …and a subagent at the only one it has, under the same session id.
    let opened = |agent_id: &str, turn: u32| -> String {
        agent_hook_stdout(
            &repo,
            "SubagentStart",
            session,
            turn,
            &format!(r#"{{"agent_id":"{agent_id}","agent_type":"general-purpose"}}"#),
            &env,
        )
    };
    let child = context_of(&opened("a0e79504", 2), "SubagentStart");
    assert_eq!(
        child, parent,
        "the same sealed pointer, on the channel this window's own row pins"
    );
    let repeat = opened("a0e79504", 2);
    assert!(
        repeat.trim().is_empty(),
        "one window, one pointer — and no manifest behind it: {repeat:?}"
    );
    let sibling = context_of(&opened("a2f52a50", 2), "SubagentStart");
    assert_eq!(
        sibling, parent,
        "a second subagent of the same turn is a second window"
    );

    // Three windows, three markers, each named by the key the read ledger uses
    // — which is what makes the parent's claim and the children's independent.
    let markers: std::collections::BTreeSet<String> =
        std::fs::read_dir(repo.join(".rift").join("state").join("injection"))
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
    assert_eq!(markers.len(), 3, "one flat marker per window: {markers:?}");
    for agent_id in ["", "a0e79504", "a2f52a50"] {
        assert!(
            markers.contains(&format!("{}.json", window_key(session, agent_id))),
            "no marker for the window {session}/{agent_id:?}: {markers:?}"
        );
    }

    // A file only the subagent reads belongs to the subagent's window, so the
    // parent's next boundary is not told it has read it.
    let file = repo.join("a.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let read = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}},"agent_id":"a0e79504","agent_type":"general-purpose"}}"#,
        json_str(&file)
    );
    agent_hook(&repo, "PreToolUse", session, 3, &read, &env);
    std::fs::write(&file, "one\nTWO\nthree\n").unwrap();
    let next = agent_hook_stdout(&repo, "UserPromptSubmit", session, 4, "{}", &env);
    assert!(
        !next.contains("a.txt"),
        "a subagent's read is not the parent's to be told about: {next:?}"
    );
    assert!(
        !next.contains("you have read changed"),
        "the parent has read nothing, so nothing it read changed: {next:?}"
    );
}

/// Through the real binary, under a repository that asks for declines: the
/// first re-read after a compaction is allowed and the second is declined
/// again.
///
/// This is the whole of what an eviction does to the decision, stated where it
/// can actually fail. Coverage recorded before the boundary is kept — the
/// recovery map is built from it — and satisfies nothing, so the context that
/// no longer holds the bytes is never starved of them; the read that follows
/// re-records what it was handed, and the repeat after *that* is a repeat
/// again. Read as one walk: declined, compacted, allowed, declined.
#[test]
fn the_second_post_compaction_read_of_an_unchanged_file_is_denied() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    enforce_for(&repo, "claude-code");
    let file = repo.join("b.txt");
    std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
    let read = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":1,"limit":500}}}}"#,
        json_str(&file)
    );
    let read_at = |turn: u32| -> String {
        agent_hook_stdout(&repo, "PreToolUse", "e2e-live-1", turn, &read, &env)
    };

    assert!(
        declined_with(&read_at(1)).is_none(),
        "the first read of a file is never declined"
    );
    let repeat = declined_with(&read_at(2)).expect("the repeat is declined while it is held");
    assert!(repeat.contains("b.txt"), "{repeat}");

    // The context is compacted, and is handed what it stopped holding.
    agent_hook(&repo, "PreCompact", "e2e-live-1", 3, "{}", &env);
    let start = agent_hook_stdout(
        &repo,
        "SessionStart",
        "e2e-live-1",
        4,
        r#"{"source":"compact"}"#,
        &env,
    );
    assert!(
        start.contains("unchanged since your read"),
        "the map says the file did not move: {start}"
    );

    assert!(
        declined_with(&read_at(5)).is_none(),
        "the first re-read after a compaction is a first read: the window no \
         longer holds the bytes the map only *named*"
    );
    let second = declined_with(&read_at(6))
        .expect("and the read after that one is a repeat of what it now holds");
    assert!(second.contains("b.txt"), "{second}");

    assert_eq!(
        ledger_kinds(&repo),
        vec!["read", "deny", "evict", "evict", "hold", "read", "deny"],
        "declined, compacted, told what it had held, allowed, declined — and \
         the read that follows the announcement answers it, so nothing books \
         a read that did happen"
    );
}

/// The reads this family makes: twelve bounded `sed` reads through `shell`, spelled the way
/// the client spells them — a relative path resolved against the working
/// directory the payload states. The strict recogniser is what puts those
/// lines in the ledger a map is built from, so without it this family's map
/// would be empty; and nothing declines them, because this family is observed.
/// Each call is also the standing assertion that injection stays off the gate
/// path: this verb answers zero bytes.
fn codex_shell_reads(repo: &Path, session: &str, env: &[(&str, &str)]) -> Vec<String> {
    let files: Vec<String> = (0..12)
        .map(|i| format!("src/module-{i:02}/file.rs"))
        .collect();
    for name in &files {
        let path = repo.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut body = String::new();
        for line in 1..=40 {
            std::fmt::Write::write_fmt(&mut body, format_args!("line {line}\n")).unwrap();
        }
        std::fs::write(&path, body).unwrap();
        let read = format!(
            r#"{{{},"tool_name":"Bash","tool_input":{{"command":"sed -n '1,40p' {name}"}}}}"#,
            codex_session_fields(repo, session)
        );
        let out = agent_hook_raw(repo, "codex", "PreToolUse", &read, env);
        // Byte-empty, which is two facts in one: this family is observed and
        // never declined, and the gate path is not an injection channel — a
        // pointer or a map that leaked onto it would show up here as bytes.
        assert!(
            out.is_empty(),
            "the gate path answers nothing at all for this family — no \
             decision, and no additionalContext: {out:?}"
        );
    }
    files
}

/// The two fields every payload of this family carries that the boundary and
/// the gate read: which conversation this is, and where the agent was standing.
fn codex_session_fields(repo: &Path, session: &str) -> String {
    format!(
        r#""session_id":"{session}","cwd":{}"#,
        json_str(&repo.canonicalize().unwrap())
    )
}

/// Recall on, and a map budget an operator would have to write: the family's
/// declared bound sits above every budget Rift ships, so a list that overruns
/// its ceiling is reached by lowering the operator's number rather than by
/// seeding hundreds of reads into one window.
fn seed_recall_with_a_small_map_budget(repo: &Path) {
    std::fs::create_dir_all(repo.join(".rift").join("policies")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    std::fs::write(
        repo.join(".rift").join("policies").join("serve.toml"),
        "[interception]\nrecovery_budget = 200\n",
    )
    .unwrap();
}

/// A map that overran its budget lost whole entries and counted them: every
/// line that survived names a file, the revision the window held and the lines
/// it can honestly claim, and what went is a number rather than a cut.
///
/// A `cat` names no end — the client's own result cap decides where the output
/// stops — so the lines an entry claims start and end at the line it began on.
fn assert_the_map_dropped_whole_entries(map: &str, files: &[String]) {
    assert!(
        rift_tokens::count(map) <= 200,
        "inside the operator's budget: {map}"
    );
    let shown: Vec<&str> = map.lines().filter(|line| line.starts_with("- ")).collect();
    assert!(
        !shown.is_empty() && shown.len() < files.len(),
        "entries were dropped, not the frame: {map}"
    );
    assert!(map.contains("not shown"), "and counted: {map}");
    for line in &shown {
        assert!(
            files.iter().any(|name| line.contains(name.as_str()))
                && line.contains('@')
                && line.contains("L1-"),
            "a dropped entry is dropped whole: {line}"
        );
    }
}

/// Through the shipped binary, on the Codex stdin contract: this family's two
/// boundary hooks carry the same facts Claude Code's do — the session pointer
/// once, silence on a quiet turn, and the recovery map after a compaction —
/// on the context field its own generated schemas pin, inside the bound Rift
/// declared for its own hook handlers.
///
/// Each step is a fact the unit tests cannot state: that the family answers on
/// the channel at all (it was pinned and unwritten until the bound had a single
/// home), that both of its boundary verbs are wired, that the quiet turn stays
/// quiet, that the reads this family makes through `shell` are what the map is
/// built from, that the map rides the first boundary hook after this family's
/// own compaction signal — it names no source, so the ledger's last signal is
/// what decides — and that a map larger than the budget is paid for in whole
/// entries and a count, never in a cut the client makes.
///
/// Boundary injection is independent of whether a separate tool call is
/// allowed or declined; this walk isolates the context channel.
#[test]
fn codex_receives_the_pointer_and_manifest_through_its_pinned_channel() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    seed_recall_with_a_small_map_budget(&repo);
    let declared = rift_agent::respond::DECLARED_CONTEXT_LIMIT_TOKENS;
    let session = |id: &str| codex_session_fields(&repo, id);
    let hook =
        |verb: &str, body: &str| -> String { agent_hook_raw(&repo, "codex", verb, body, &env) };
    // The context field, and the assertion that what travels in it is inside
    // the bound written into this family's own hook configuration — which is
    // what keeps the client from spilling the middle of a frame to a file.
    let context_of = |out: &str, verb: &str| -> String {
        let response: serde_json::Value = serde_json::from_str(out.trim()).unwrap_or_else(|e| {
            panic!("the hook wrote an object the client can read: {e} in {out:?}")
        });
        assert_eq!(
            response["hookSpecificOutput"]["hookEventName"], verb,
            "{response}"
        );
        let text = response["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("the text travels in the field this family reads: {response}")
            })
            .to_owned();
        assert!(
            rift_tokens::count(&text) <= declared,
            "{} tokens over the declared bound of {declared}",
            rift_tokens::count(&text)
        );
        text
    };

    // The gate path is not a context channel, and this is where that is
    // enforced end to end: a tool call on a session that has never been
    // introduced is the one payload an injection would have something to say
    // on, and the answer is still zero bytes. A pointer or a map that leaked
    // onto this verb reddens here rather than arriving beside a decision.
    let gate = hook(
        "PreToolUse",
        &format!(
            r#"{{{},"tool_name":"Bash","tool_input":{{"command":"cat README.md"}}}}"#,
            session("codex-e2e-gate")
        ),
    );
    assert!(
        gate.is_empty(),
        "the gate path carries no injection: {gate:?}"
    );

    // The first prompt of a session is introduced, once.
    let pointer = context_of(
        &hook(
            "UserPromptSubmit",
            &format!("{{{}}}", session("codex-e2e-1")),
        ),
        "UserPromptSubmit",
    );
    assert!(
        pointer.contains("rift recall") && pointer.contains("rift agent-help"),
        "the pointer names the surface it points at: {pointer}"
    );

    // A quiet turn is quiet: nothing has changed and nothing is owed, so this
    // family's contract for "no answer" is no bytes at all.
    let quiet = hook(
        "UserPromptSubmit",
        &format!("{{{}}}", session("codex-e2e-1")),
    );
    assert!(
        quiet.trim().is_empty(),
        "an unchanged known state injects nothing: {quiet:?}"
    );

    // The other boundary verb is wired too, and a second session is a session
    // that holds nothing and is introduced again.
    let start = context_of(
        &hook("SessionStart", &format!("{{{}}}", session("codex-e2e-2"))),
        "SessionStart",
    );
    assert_eq!(start, pointer, "the same introduction on either boundary");

    let files = codex_shell_reads(&repo, "codex-e2e-1", &env);
    // Its compaction signal, then the next hook that can carry text: this
    // family names no source on a boundary, so the ledger's last signal is what
    // makes this the hook that meets a context which lost its state.
    hook("PostCompact", &format!("{{{}}}", session("codex-e2e-1")));
    let map = context_of(
        &hook(
            "UserPromptSubmit",
            &format!("{{{}}}", session("codex-e2e-1")),
        ),
        "UserPromptSubmit",
    );
    assert!(
        map.contains("Context was compacted"),
        "the map says why it is here: {map}"
    );
    assert!(
        map.contains("unchanged since your read"),
        "nothing moved, so the re-read can be skipped: {map}"
    );
    assert!(
        !map.contains("one\ntwo"),
        "identifiers and numbers, never a byte of a file: {map}"
    );
    // Over the budget, so it is paid for in whole entries and a count — the
    // client never sees a head, a tail and a path to the rest.
    assert_the_map_dropped_whole_entries(&map, &files);

    // And the boundary after that hands over no second map.
    let again = hook(
        "UserPromptSubmit",
        &format!("{{{}}}", session("codex-e2e-1")),
    );
    assert!(
        !again.contains("Context was compacted"),
        "one eviction, one map: {again}"
    );
}

/// Through the shipped binary, on the Antigravity stdin contract: the hook that
/// runs before a model invocation hands the session its pointer **once per
/// execution**, at the first invocation and at no other.
///
/// This family's boundary hook is the one that fires before every invocation
/// rather than once a turn, so the walk is written the way the loop actually
/// runs: the first invocation, then the invocations inside the tool loop, then
/// the hook after the invocation, then the same first invocation delivered
/// twice, then a second execution. Each step is a fact the unit tests cannot
/// state — that the step travels in the field this family's client reads, that
/// the kind is the one a recorded run found still readable later in the loop,
/// that a repeat delivery costs nothing, and that a new execution is a new
/// session and is introduced again.
#[test]
fn antigravity_is_injected_once_per_execution_at_invocation_zero() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    let cache = tmp.path().join("cache");
    let env = [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ];
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    let boundary = |conversation: &str, invocation: u32| {
        format!(
            r#"{{"conversationId":"{conversation}","invocationNum":{invocation},"initialNumSteps":1,"modelName":"m","workspacePaths":[{}]}}"#,
            json_str(&repo)
        )
    };
    let answer = |body: &str, verb: &str| -> serde_json::Value {
        let out = agent_hook_raw(&repo, "antigravity", verb, body, &env);
        serde_json::from_str(out.trim()).unwrap_or_else(|e| {
            panic!("the hook wrote an object the client can read: {e} in {out:?}")
        })
    };

    // The first invocation of an execution: one step, of the kind the pinned
    // row names, carrying the pointer.
    let first = answer(&boundary("agy-e2e-1", 0), "PreInvocation");
    let steps = first["injectSteps"]
        .as_array()
        .unwrap_or_else(|| panic!("the pointer travels as a trajectory step: {first}"));
    assert_eq!(steps.len(), 1, "one boundary, one step: {first}");
    let pointer = steps[0]["userMessage"]
        .as_str()
        .unwrap_or_else(|| panic!("the kind that is still readable later: {first}"))
        .to_owned();
    assert!(
        pointer.contains("rift recall") && pointer.contains("rift agent-help"),
        "the pointer names the surface it points at: {pointer}"
    );

    // Every invocation inside the tool loop, and the hook after one, is the
    // acknowledgement the family requires and nothing else.
    for invocation in [1_u32, 2, 7] {
        assert_eq!(
            answer(&boundary("agy-e2e-1", invocation), "PreInvocation"),
            serde_json::json!({}),
            "invocation {invocation} is inside the loop, not at its boundary"
        );
    }
    assert_eq!(
        answer(&boundary("agy-e2e-1", 7), "PostInvocation"),
        serde_json::json!({}),
        "one loop injected into twice spends twice for one fact"
    );

    // The same first invocation delivered again: the claim is durable, so the
    // conversation is introduced once however often the hook arrives.
    let again = answer(&boundary("agy-e2e-1", 0), "PreInvocation");
    assert!(
        !again.to_string().contains("rift agent-help"),
        "one execution, one pointer: {again}"
    );

    // A second execution is a second conversation, and is introduced again.
    let second = answer(&boundary("agy-e2e-2", 0), "PreInvocation");
    assert_eq!(
        second["injectSteps"][0]["userMessage"],
        serde_json::Value::from(pointer.as_str()),
        "a new execution holds nothing, and is handed the same introduction"
    );
}
