//! The builders every hooks suite drives the real `rift` binary with: the
//! spawn wrappers over `common::rift_cmd`, the fake agent's hook delivery, the
//! seeded live-session repository, the git-hook pair as git fires it, and the
//! readers that look at what a run left behind (shards, session phases, the
//! window journal). One module rather than a copy per suite, so a fixture
//! change reaches every reader at once. Every hooks suite includes this file
//! by `#[path]` as its own module, and each test binary uses a subset of the
//! builders, which is why each suite's declaration carries
//! `#[allow(dead_code)]`: the allow names a fact about the include, not a
//! suppression.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::common::rift_cmd;

pub(crate) const RETIRED_BRAND: &str = concat!("ent", "ire");

/// The native session id every seeded repository starts with.
pub(crate) const LIVE_SESSION: &str = "e2e-live-1";

/// A two-line agent transcript in the dialect the compactor models: one
/// prompt, one answer. The prompt is what `why`, `explain` and a resume serve
/// back, so the fixtures that assert on it all seed this one.
pub(crate) const TWO_LINE_TRANSCRIPT: &str = concat!(
    r#"{"type":"user","message":{"role":"user","content":"add a feature"}}"#,
    "\n",
    r#"{"type":"assistant","message":{"id":"m1","role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
    "\n"
);

/// Which channel of the child a spawn wrapper hands back. The other is
/// discarded rather than piped, so a detached worker the hook leaves behind
/// cannot hold a pipe open past the hook's own exit.
enum Keep {
    Stdout,
    Stderr,
}

/// The hermetic environment every fixture hands the binary: the cache root
/// inside the tempdir, no keychain, and the archive drained inline so a test
/// can read the shard the moment the hook returns.
pub(crate) fn sync_env(cache: &Path) -> [(&'static str, &str); 3] {
    [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ]
}

/// [`sync_env`] with a private substrate root too, so nothing on the
/// developer's machine can make an archive pass by accident.
pub(crate) fn sync_env_with_data<'a>(
    cache: &'a Path,
    data: &'a Path,
) -> [(&'static str, &'a str); 5] {
    [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("XDG_DATA_HOME", data.to_str().unwrap()),
        ("LOCALAPPDATA", data.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
        ("RIFT_CAPTURE_SYNC", "1"),
    ]
}

/// The environment for the query verbs, which archive nothing.
pub(crate) fn query_env(cache: &Path) -> [(&'static str, &str); 2] {
    [
        ("RIFT_CACHE_DIR", cache.to_str().unwrap()),
        ("RIFT_NO_KEYCHAIN", "1"),
    ]
}

/// Run `rift <args>` from `cwd` with optional stdin; return the exit code.
/// The cwd is MANDATORY and hermetic (see `common::rift_cmd`): the agent-hook
/// surface resolves its repository from the process working directory, so an
/// unpinned spawn is a write into whatever repository the test runner sits in.
pub(crate) fn run_rift(
    cwd: &Path,
    args: &[&str],
    stdin: Option<&str>,
    extra_env: &[(&str, &str)],
) -> i32 {
    spawn_rift(cwd, args, stdin.unwrap_or(""), extra_env, Keep::Stderr).0
}

/// Run `rift <args>` from `cwd` with optional stdin; return the exit code and
/// stderr text (the hook surface's only user-visible channel — loudness
/// assertions read it).
pub(crate) fn run_rift_capture(
    cwd: &Path,
    args: &[&str],
    stdin: Option<&str>,
    extra_env: &[(&str, &str)],
) -> (i32, String) {
    spawn_rift(cwd, args, stdin.unwrap_or(""), extra_env, Keep::Stderr)
}

/// Run `rift <args>` **from `cwd`**; return the exit code and stdout. The soak
/// assertions read `doctor --json`, so they need the data channel, not just the
/// code.
pub(crate) fn run_rift_capture_in(
    cwd: &Path,
    args: &[&str],
    stdin: &str,
    extra_env: &[(&str, &str)],
) -> (i32, String) {
    spawn_rift(cwd, args, stdin, extra_env, Keep::Stdout)
}

/// Run `rift <args>` **from `cwd`** with stdin; return the exit code. The
/// agent-hook surface reads its repository from the process working directory,
/// which is exactly what this regression is about.
pub(crate) fn run_rift_in(
    cwd: &Path,
    args: &[&str],
    stdin: &str,
    extra_env: &[(&str, &str)],
) -> i32 {
    spawn_rift(cwd, args, stdin, extra_env, Keep::Stdout).0
}

/// Run `rift <args>` from `cwd` and return `(exit code, stdout)`.
pub(crate) fn run_rift_stdout(
    cwd: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> (i32, String) {
    spawn_rift(cwd, args, "", extra_env, Keep::Stdout)
}

/// A `--json` verb that is expected to answer: exit 0, parsed. A refusal's
/// document is read through [`run_rift_stdout`], where the code is the claim.
pub(crate) fn rift_json(
    cwd: &Path,
    args: &[&str],
    extra_env: &[(&str, &str)],
) -> serde_json::Value {
    let (code, out) = run_rift_stdout(cwd, args, extra_env);
    assert_eq!(code, 0, "{args:?}: {out}");
    serde_json::from_str(&out).unwrap_or_else(|e| panic!("{args:?} answered no json: {e}: {out}"))
}

/// `rift doctor --json`, report-only: it heals nothing and reads everything.
pub(crate) fn doctor_json(repo: &Path, extra_env: &[(&str, &str)]) -> serde_json::Value {
    rift_json(repo, &["doctor", "--json"], extra_env)
}

/// `rift doctor --fix --json`: the sweep is doctor's repair arm, so a fixture
/// that asserts a sweep effect asks for the repair.
pub(crate) fn doctor_fix_json(repo: &Path, extra_env: &[(&str, &str)]) -> serde_json::Value {
    rift_json(repo, &["doctor", "--fix", "--json"], extra_env)
}

/// The sessions leg of a doctor document, wherever the envelope put it.
pub(crate) fn sessions_leg(doc: &serde_json::Value) -> &serde_json::Value {
    doc.pointer("/sessions")
        .or_else(|| doc.pointer("/data/sessions"))
        .expect("doctor json carries a sessions leg")
}

/// The capture-queue leg of a doctor document, wherever the envelope put it.
pub(crate) fn capture_queue_leg(doc: &serde_json::Value) -> &serde_json::Value {
    doc.pointer("/capture_queue")
        .or_else(|| doc.pointer("/data/capture_queue"))
        .expect("doctor json carries the capture-queue leg")
}

/// A path as a JSON string literal (quotes included): Windows paths carry
/// backslashes, which a raw `Display` splice turns into invalid escapes.
pub(crate) fn json_str(path: &Path) -> String {
    serde_json::Value::String(path.display().to_string()).to_string()
}

/// Milliseconds since the epoch. Read only where the subject under test is a
/// wall-clock threshold inside the binary — the idle sweep's floor, the 24 h
/// alarm — so "now" and "the epoch" are unambiguous whenever the test runs.
pub(crate) fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap()
}

/// A Claude Code `Read` payload over an absolute path, as the client sends it.
pub(crate) fn read_payload(file: &Path, offset: u32, limit: u32) -> String {
    format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{},"offset":{offset},"limit":{limit}}}}}"#,
        json_str(file)
    )
}

/// The Claude Code `Edit` payload the journal walks use: one line replaced by
/// two, so the edit has a place and a shift.
pub(crate) fn edit_old_to_new_payload(file: &Path) -> String {
    format!(
        r#"{{"tool_name":"Edit","tool_input":{{"file_path":{},"old_string":"old\n","new_string":"new\nnewer\n","replace_all":false}}}}"#,
        json_str(file)
    )
}

/// Drive one real agent read hook — `PostToolUse` carrying a `Read` payload with
/// an absolute path, as Claude Code sends it — from the repo's cwd, so the
/// session captures a read fact. The hook surface never fails the agent, so the
/// only assertion here is that it exited cleanly.
pub(crate) fn agent_read_hook(
    repo: &Path,
    session: &str,
    turn: u32,
    file: &Path,
    env: &[(&str, &str)],
) {
    let payload = format!(
        r#"{{"tool_name":"Read","tool_input":{{"file_path":{}}}}}"#,
        json_str(file)
    );
    agent_hook(repo, "PostToolUse", session, turn, &payload, env);
}

/// Drive one native agent hook (`rift hooks claude-code <verb>`) with the
/// stdin capture contract. The fake agent of the capture fixtures, generalized
/// past `PostToolUse Read`.
pub(crate) fn agent_hook(
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
pub(crate) fn agent_hook_out(
    repo: &Path,
    agent: &str,
    verb: &str,
    session: &str,
    turn: u32,
    payload: &str,
    env: &[(&str, &str)],
) -> String {
    let body = format!(r#"{{"schema":1,"session":"{session}","turn":{turn},"payload":{payload}}}"#);
    agent_hook_raw(repo, agent, verb, &body, env)
}

/// Drive one native agent hook with the bytes its **own client** would put on
/// stdin — no envelope around them — and hand back what it wrote to stdout.
///
/// The families whose payload fields decide the answer are read straight off
/// that object, so a walk that wrapped it would be testing the wrapper.
pub(crate) fn agent_hook_raw(
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
pub(crate) fn seed_live_session_repo_unborn(tmp: &Path) -> PathBuf {
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
    // Stamped NOW: a live session is one touched inside the idle sweep's
    // floor, and every worker pass sweeps past it, so a seed stamped in the
    // past would be condensed and ended by the first drain it met.
    std::fs::write(
        sessions.join("e2e-live-1.json"),
        format!(
            r#"{{"schema":1,"session_id":"e2e-live-1","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":{},"native_id_map":{{}}}}"#,
            now_ms()
        ),
    )
    .unwrap();
    repo
}

/// Seed a hermetic git repo with one commit and a **live** native-hook session
/// cache file, so the post-commit leg sees an archivable session. Returns the
/// work-tree path. The session is stamped now, which is what live means to the
/// idle sweep; a test that wants it stale ages it explicitly.
pub(crate) fn seed_live_session_repo(tmp: &Path) -> PathBuf {
    let repo = seed_live_session_repo_unborn(tmp);
    git_in(&repo, &["commit", "-q", "-m", "seed"]);
    repo
}

/// A bare git repository with one committed file and nothing of Rift's in it
/// — the shape `attach` and `import` meet, where no hook has ever fired.
pub(crate) fn seed_plain_repo(tmp: &Path) -> PathBuf {
    let repo = tmp.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-q"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git_in(&repo, &["add", "a.txt"]);
    git_in(&repo, &["commit", "-q", "-m", "seed"]);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    repo
}

/// Write a live session cache under `<repo>/.git/rift-sessions`.
pub(crate) fn seed_session(repo: &Path, id: &str, updated_at_ms: i64) {
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

/// A live session cache whose turn is closed and which remembers the agent
/// transcript's ADDRESS — the way a hook payload would have left it.
pub(crate) fn seed_session_with_transcript(repo: &Path, id: &str, transcript: &Path) {
    let sessions = repo.join(".git").join("rift-sessions");
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(
        sessions.join(format!("{id}.json")),
        format!(
            r#"{{"schema":1,"session_id":"{id}","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,"updated_at_ms":{},"native_id_map":{{}},"transcript_path":{}}}"#,
            now_ms(),
            json_str(transcript)
        ),
    )
    .unwrap();
}

/// The agent's transcript, where the agent keeps it — outside the repo, so an
/// archive must read the ADDRESS rather than assume a location.
pub(crate) fn write_agent_transcript(tmp: &Path, session: &str, body: &str) -> PathBuf {
    let transcript = tmp
        .join("agent-transcripts")
        .join(format!("{session}.jsonl"));
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    std::fs::write(&transcript, body).unwrap();
    transcript
}

/// A hermetic `git` invocation inside `dir`, returning its stdout — user/system
/// config off, a fixed identity, and every inherited `GIT_*` re-targeting
/// variable scrubbed, so the developer's own environment cannot steer a fixture.
/// A failing command panics with git's own stderr, so a broken fixture names
/// itself instead of surfacing as an assertion three steps later.
pub(crate) fn git_in(dir: &Path, args: &[&str]) -> String {
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

/// `git rev-parse <rev>`, trimmed.
pub(crate) fn rev_parse(repo: &Path, rev: &str) -> String {
    git_in(repo, &["rev-parse", rev]).trim().to_owned()
}

/// The real `post-commit` hook against `repo`'s own git dir, as git would fire
/// it: `(exit code, stderr)`. Every archive in these suites goes through here.
pub(crate) fn post_commit_hook(repo: &Path, env: &[(&str, &str)]) -> (i32, String) {
    let gd = repo.join(".git");
    run_rift_capture(
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
    )
}

/// [`post_commit_hook`], asserting the one thing every caller asserts first:
/// the hook never fails the git operation.
pub(crate) fn archive_head(repo: &Path, env: &[(&str, &str)]) -> String {
    let (code, stderr) = post_commit_hook(repo, env);
    assert_eq!(code, 0, "the hook never fails the git op; stderr: {stderr}");
    stderr
}

/// The real `prepare-commit-msg` hook over the message file `msg`, as git
/// would fire it: `(exit code, stderr)`.
pub(crate) fn prepare_commit_msg_hook(
    repo: &Path,
    msg: &Path,
    env: &[(&str, &str)],
) -> (i32, String) {
    let gd = repo.join(".git");
    run_rift_capture(
        repo,
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
        env,
    )
}

/// [`prepare_commit_msg_hook`] over a message file already holding its
/// subject: the checkpoint id the injected trailer predicts.
pub(crate) fn predicted_trailer(repo: &Path, msg: &Path, env: &[(&str, &str)]) -> String {
    let (code, stderr) = prepare_commit_msg_hook(repo, msg, env);
    assert_eq!(
        code, 0,
        "prepare-commit-msg never fails the commit: {stderr}"
    );
    std::fs::read_to_string(msg)
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("Rift-Checkpoint: ").map(str::to_owned))
        .unwrap_or_else(|| panic!("no trailer injected into {}", msg.display()))
}

/// One real hook-pair commit of whatever is staged: `prepare-commit-msg`
/// (trailer prediction) → `git commit` → `post-commit` (archive + projection
/// ingest). Returns the predicted checkpoint id the trailer carries.
pub(crate) fn hook_pair_commit(
    repo: &Path,
    tmp: &Path,
    subject: &str,
    env: &[(&str, &str)],
) -> String {
    let msg = tmp.join(format!("MSG-{subject}"));
    std::fs::write(&msg, format!("{subject}\n")).unwrap();
    let predicted = predicted_trailer(repo, &msg, env);
    git_in(repo, &["commit", "-q", "-F", msg.to_str().unwrap()]);
    archive_head(repo, env);
    predicted
}

/// A commit that CARRIES the trailer naming `checkpoint_id`, so a branch walk
/// has a claim: `b.txt` is written and committed with it.
pub(crate) fn commit_carrying_trailer(repo: &Path, checkpoint_id: &str) {
    std::fs::write(repo.join("b.txt"), "two\n").unwrap();
    git_in(repo, &["add", "b.txt"]);
    git_in(
        repo,
        &[
            "commit",
            "-q",
            "-m",
            &format!("carry\n\nRift-Checkpoint: {checkpoint_id}"),
        ],
    );
}

/// The checkpoint shards currently on the native chain ref.
pub(crate) fn checkpoint_shards(repo: &Path) -> Vec<String> {
    let mut shards: Vec<String> = shard_tree_names(repo)
        .lines()
        .filter_map(|p| p.rsplit_once('/').map(|(dir, _)| dir.to_owned()))
        .collect();
    shards.sort_unstable();
    shards.dedup();
    shards
}

/// The one shard the repository holds, as a checkpoint id (the directory's
/// `<2 hex>/<62 hex>` folded back into 64 hex digits).
pub(crate) fn only_checkpoint_id(repo: &Path) -> String {
    let shards = checkpoint_shards(repo);
    assert_eq!(shards.len(), 1, "one archived shard: {shards:?}");
    shards[0].replace('/', "")
}

/// Every path under the checkpoint branch, one per line — read with git alone
/// so an assertion about the shard never goes through the code under test.
pub(crate) fn shard_tree_names(repo: &Path) -> String {
    let tree = Command::new("git")
        .args([
            "ls-tree",
            "-r",
            "--name-only",
            "refs/heads/rift/checkpoints/v1",
        ])
        .current_dir(repo)
        .output()
        .unwrap();
    String::from_utf8_lossy(&tree.stdout).into_owned()
}

/// The text of one artifact on the checkpoint branch, by its tree path.
pub(crate) fn shard_blob(repo: &Path, path: &str) -> String {
    String::from_utf8_lossy(&shard_blob_bytes(repo, path)).into_owned()
}

/// The exact bytes of one artifact on the checkpoint branch — what a resume
/// must put back byte for byte.
pub(crate) fn shard_blob_bytes(repo: &Path, path: &str) -> Vec<u8> {
    Command::new("git")
        .args(["show", &format!("refs/heads/rift/checkpoints/v1:{path}")])
        .current_dir(repo)
        .output()
        .unwrap()
        .stdout
}

/// The branch HEAD is on.
pub(crate) fn current_branch(repo: &Path) -> String {
    git_in(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .trim()
        .to_owned()
}

/// `git clone -q <repo>` into `<tmp>/<name>`: branches arrive as
/// remote-tracking refs only, so the local checkpoints ref — the one the
/// walker reads — is absent by construction.
pub(crate) fn clone_of(tmp: &Path, repo: &Path, name: &str) -> PathBuf {
    let clone = tmp.join(name);
    let st = Command::new("git")
        .args([
            "clone",
            "-q",
            repo.to_str().unwrap(),
            clone.to_str().unwrap(),
        ])
        .current_dir(tmp)
        .output()
        .unwrap();
    assert!(st.status.success(), "git clone: {st:?}");
    clone
}

/// Whether the LOCAL checkpoints ref exists in `dir` — a clone starts without
/// one, and the resume ladder's origin rung creates it.
pub(crate) fn has_local_checkpoints_ref(dir: &Path) -> bool {
    Command::new("git")
        .args(["show-ref", "--verify", "refs/heads/rift/checkpoints/v1"])
        .current_dir(dir)
        .output()
        .unwrap()
        .status
        .success()
}

/// The first tree path on the checkpoint branch ending in `artifact`.
pub(crate) fn shard_path_of(repo: &Path, artifact: &str) -> String {
    shard_tree_names(repo)
        .lines()
        .find(|l| l.ends_with(artifact))
        .unwrap_or_else(|| panic!("the shard carries {artifact}"))
        .to_owned()
}

/// Whether the checkpoint branch exists at all — the coarsest fact an archive
/// leaves behind, read with git alone.
pub(crate) fn checkpoint_ref_exists(repo: &Path) -> bool {
    let refs = Command::new("git")
        .args(["for-each-ref", "refs/heads/rift/checkpoints/v1"])
        .current_dir(repo)
        .output()
        .unwrap();
    !String::from_utf8_lossy(&refs.stdout).trim().is_empty()
}

/// The state of one session cache on disk.
pub(crate) fn session_phase(repo: &Path, id: &str) -> String {
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

/// The files in this checkout's window-journal directory, by name.
pub(crate) fn window_journal_files(repo: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(repo.join(".rift").join("state").join("reads"))
        .into_iter()
        .flatten()
        .filter_map(std::result::Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The kinds of line the one context window's journal holds, in order — the
/// record a decision leaves behind, read back from the file the product wrote.
pub(crate) fn journal_kinds(repo: &Path) -> Vec<String> {
    let names = window_journal_files(repo);
    let name = names.first().expect("one context window, one journal");
    journal_kinds_of(repo, name)
}

/// The same, for a walk that puts two context windows in one repository: one
/// journal file per window, named for the window rather than for the order the
/// directory happens to list them in.
pub(crate) fn journal_kinds_of(repo: &Path, name: &str) -> Vec<String> {
    journal_rows(repo, name)
        .iter()
        .map(|row| row["k"].as_str().unwrap().to_owned())
        .collect()
}

/// Every line of one window's journal, parsed.
pub(crate) fn journal_rows(repo: &Path, name: &str) -> Vec<serde_json::Value> {
    std::fs::read_to_string(repo.join(".rift").join("state").join("reads").join(name))
        .unwrap()
        .lines()
        .map(|row| serde_json::from_str(row).unwrap())
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
pub(crate) fn enforce_for(repo: &Path, family: &str) {
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

/// Recall on, in the shape `rift init` leaves the project file.
pub(crate) fn enable_recall(repo: &Path) {
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift").join("config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
}

/// The decision one hook wrote to stdout: the reason a read was declined with,
/// or `None` where the call was left alone.
pub(crate) fn declined_with(stdout: &str) -> Option<String> {
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

/// The key one context window's local state is named by: the session it
/// belongs to and the agent inside it, hashed together with a separator.
///
/// Spelled out here rather than imported because the production function is
/// private to the binary. That is the point of stating it: if the two ever
/// disagree, this walk finds no marker where it expects one.
pub(crate) fn window_key(session: &str, agent_id: &str) -> String {
    let mut bytes = session.as_bytes().to_vec();
    bytes.push(0);
    bytes.extend_from_slice(agent_id.as_bytes());
    rift_hashchain::leaf_hash(&bytes).to_hex()
}

/// The one spawn every wrapper above goes through: stdin written, the kept
/// channel piped, the other discarded, `-1` for a signal death.
fn spawn_rift(
    cwd: &Path,
    args: &[&str],
    stdin: &str,
    extra_env: &[(&str, &str)],
    keep: Keep,
) -> (i32, String) {
    let mut cmd = rift_cmd(cwd);
    cmd.args(args).stdin(Stdio::piped());
    match keep {
        Keep::Stdout => cmd.stdout(Stdio::piped()).stderr(Stdio::null()),
        Keep::Stderr => cmd.stdout(Stdio::null()).stderr(Stdio::piped()),
    };
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
    let kept = match keep {
        Keep::Stdout => out.stdout,
        Keep::Stderr => out.stderr,
    };
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&kept).into_owned(),
    )
}
