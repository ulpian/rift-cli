//! A cold rebuild restores the worktree observations a session froze into
//! its checkpoint.
//!
//! One Claude Code session is replayed through the shipped `rift` binary
//! with the capture worker drained inline, from the main checkout of a
//! repository with one linked member: a prompt, a shell read whose leading
//! `cd` moves into the member, and a session end that condenses the
//! session's observations into the checkpoint's `session_worktrees.jsonl`;
//! then a second turn in which a subagent reads, and a second end that
//! condenses a second checkpoint whose artifact is the first's plus the
//! subagent's state.
//! The rows of both observation relations are recorded off the live capture
//! projection; then the cache root moves to an empty directory, `rift
//! rebuild` reads the same checkpoint, and the rows are recorded again. Every
//! column the shard freezes must come back as it was. The head is not
//! frozen, so a restored row carries none and its id is the one a writer with
//! no head derives; the path, which no shard carries either, must resolve to
//! this machine's checkout of the member each row names.
//!
//! The fixture's directories are canonical before any hook names them: a
//! worktree token hashes the path as it was spelled, and a tempdir behind a
//! symlink would be spelled one way by a hook's payload and another by the
//! rebuild's discovery.
//!
//! Hermetic as `window_journal_rebuild_e2e` is: a fixture repository in a
//! tempdir and every root either binary could resolve redirected. The `rift`
//! binary is the sibling of `rift-memd` in the same target directory, which
//! `cargo test -p rift -p rift-memd` builds before any test starts.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rift_store::{CanonicalStore, SqliteStore, Value};

/// The native session id the replay delivers on every hook.
const SESSION: &str = "session-worktrees-1";

/// Environment that re-targets a child git or rift at another repository.
const RETARGETING_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_CEILING_DIRECTORIES",
];

/// Every column the shard freezes, from `{relation}`, in a fixed order: all
/// but the id, the head and the path.
const OBSERVATIONS: &str = "SELECT session_id, worktree_token, worktree_name, is_linked, \
     branch, subagent_id, basis, first_observed_at, schema_version \
     FROM {relation} ORDER BY first_observed_at";

/// A fixture repository with one linked member, and the redirected roots
/// both binaries run under.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    member: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        // Canonical, so macOS's /var alias is resolved, but never in Windows'
        // verbatim `\\?\` spelling: git cannot create a worktree under it.
        let canonical_root = std::fs::canonicalize(tmp.path()).unwrap();
        let spelled = canonical_root.display().to_string();
        let root = PathBuf::from(spelled.strip_prefix(r"\\?\").unwrap_or(&spelled));
        let (repo, member, home) = (root.join("repo"), root.join("lane-a"), root.join("home"));
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        seed_repository(&repo, &member);
        Self {
            _tmp: tmp,
            repo,
            member,
            home,
        }
    }

    /// Every root either binary resolves, under `home`; the cache root is
    /// the one this call names, so the two passes share everything but the
    /// projections.
    fn env(&self, cache: &str) -> Vec<(&'static str, PathBuf)> {
        let home = &self.home;
        vec![
            ("RIFT_CACHE_DIR", home.join(cache)),
            ("RIFT_CONFIG_DIR", home.join("config")),
            ("RIFT_FAMILY_DIR", home.join("families")),
            ("XDG_DATA_HOME", home.join("data")),
            ("LOCALAPPDATA", home.join("data")),
            ("HOME", home.clone()),
            ("RIFT_NO_KEYCHAIN", PathBuf::from("1")),
            ("RIFT_CAPTURE_SYNC", PathBuf::from("1")),
        ]
    }

    /// `rift <args>` from the main checkout under `cache`, `stdin` fed to
    /// it: the exit code and stdout.
    fn rift(&self, cache: &str, args: &[&str], stdin: &str) -> (i32, String) {
        let mut cmd = Command::new(rift_bin());
        cmd.current_dir(&self.repo)
            .args(args)
            .envs(self.env(cache))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for key in RETARGETING_ENV {
            cmd.env_remove(key);
        }
        let mut child = cmd.spawn().unwrap();
        let mut input = child.stdin.take().unwrap();
        input.write_all(stdin.as_bytes()).unwrap();
        drop(input);
        let out = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        (out.status.code().unwrap_or(-1), stdout)
    }

    /// One Claude Code hook of the session, under the `live` cache root.
    fn hook(&self, verb: &str, body: serde_json::Value) {
        let mut body = body;
        body["session_id"] = SESSION.into();
        body["hook_event_name"] = verb.into();
        body["cwd"] = self.repo.display().to_string().into();
        let args = ["hooks", "claude-code", verb];
        let (code, out) = self.rift("live", &args, &body.to_string());
        assert_eq!(
            code, 0,
            "a capture hook never fails the agent: {verb}: {out}"
        );
    }

    /// The capture projection `rift savings --json` names under `cache`.
    fn capture_projection(&self, cache: &str) -> SqliteStore {
        let (code, out) = self.rift(cache, &["savings", "--json"], "");
        assert_eq!(code, 0, "rift savings: {out}");
        let savings: serde_json::Value = serde_json::from_str(&out).unwrap();
        let path = savings["projection_db"]
            .as_str()
            .unwrap_or_else(|| panic!("savings names its projection_db: {savings}"));
        SqliteStore::open_read_only(path).unwrap()
    }
}

/// ★ The observations the live hooks recorded and the observations a cold
/// rebuild reads back from the checkpoint are one set in every frozen
/// column, in both relations; each restored row carries no head, and each
/// restored path is this machine's checkout of its member.
#[test]
fn a_cold_rebuild_restores_every_worktree_observation_the_live_hooks_recorded() {
    let fx = Fixture::new();
    replay_session(&fx);
    replay_subagent_turn(&fx);

    let live = fx.capture_projection("live");
    let (code, out) = fx.rift("rebuilt", &["rebuild"], "");
    assert_eq!(code, 0, "rift rebuild: {out}");
    let rebuilt = fx.capture_projection("rebuilt");

    let main_rows = observations(&live, "session_worktree");
    let cd_rows = observations(&live, "session_worktree_cd");
    assert_eq!(
        main_rows.len(),
        2,
        "the parent and the subagent: {main_rows:?}"
    );
    assert_eq!(cd_rows.len(), 1, "the member the cd named: {cd_rows:?}");
    assert_eq!(observations(&rebuilt, "session_worktree"), main_rows);
    assert_eq!(observations(&rebuilt, "session_worktree_cd"), cd_rows);

    let want = [
        ("lane-a".to_owned(), canonical(&fx.member)),
        ("main".to_owned(), canonical(&fx.repo)),
        ("main".to_owned(), canonical(&fx.repo)),
    ];
    assert_eq!(paths(&live), want, "the live hooks' paths");
    assert_eq!(paths(&rebuilt), want, "the restored paths");
    assert_eq!(heads(&live), [true; 3], "the live hooks read HEAD");
    assert_eq!(heads(&rebuilt), [false; 3], "no shard carries a head");
}

/// The session, replayed hook by hook: a prompt, a shell read that moves into
/// the linked member, and the end that condenses.
fn replay_session(fx: &Fixture) {
    fx.hook("SessionStart", serde_json::json!({"source": "startup"}));
    fx.hook("UserPromptSubmit", serde_json::json!({"prompt": "look"}));
    let command = format!("cd {} && head -n 1 a.txt", fx.member.display());
    let read = serde_json::json!({
        "tool_name": "Bash",
        "tool_input": {"command": command},
        "tool_use_id": "toolu_worktree_cd",
    });
    fx.hook("PreToolUse", read);
    fx.hook("SessionEnd", serde_json::json!({"reason": "exit"}));
}

/// The session again, after its first checkpoint: a turn in which a
/// subagent reads, and an end that condenses a second checkpoint. The
/// subagent's state is first frozen there, so only a rebuild that reads every
/// shard's new states restores it.
fn replay_subagent_turn(fx: &Fixture) {
    fx.hook(
        "UserPromptSubmit",
        serde_json::json!({"prompt": "delegate"}),
    );
    let read = serde_json::json!({
        "tool_name": "Read",
        "tool_input": {"file_path": fx.repo.join("a.txt").display().to_string()},
        "tool_use_id": "toolu_worktree_subagent",
        "agent_id": "child-1",
    });
    fx.hook("PreToolUse", read);
    fx.hook("SessionEnd", serde_json::json!({"reason": "exit"}));
}

/// Every row of `relation` but its path, rendered.
fn observations(store: &SqliteStore, relation: &str) -> Vec<String> {
    let sql = OBSERVATIONS.replace("{relation}", relation);
    select(store, &sql, &[])
        .iter()
        .map(|row| format!("{row:?}"))
        .collect()
}

/// Each observation's member and path, from both relations, by member.
fn paths(store: &SqliteStore) -> Vec<(String, String)> {
    let sql = "SELECT worktree_name, work_tree FROM session_worktree \
         UNION ALL SELECT worktree_name, work_tree FROM session_worktree_cd \
         ORDER BY 1";
    select(store, sql, &[])
        .iter()
        .map(|row| (row.get_text(0).unwrap(), row.get_text(1).unwrap()))
        .collect()
}

/// Whether each observation, from both relations, carries a head.
fn heads(store: &SqliteStore) -> Vec<bool> {
    let sql = "SELECT head_oid IS NOT NULL FROM session_worktree \
         UNION ALL SELECT head_oid IS NOT NULL FROM session_worktree_cd";
    select(store, sql, &[])
        .iter()
        .map(|row| row.get_i64(0).unwrap() == 1)
        .collect()
}

fn canonical(path: &Path) -> String {
    std::fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Every row `sql` selects from `store`.
fn select(store: &SqliteStore, sql: &str, params: &[Value]) -> Vec<rift_store::Row> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(store.query(sql, params)).unwrap()
}

/// The `rift` binary: the sibling of the `rift-memd` cargo built for this
/// test, since cargo names a binary's path only to its own package's tests.
fn rift_bin() -> PathBuf {
    let memd = Path::new(env!("CARGO_BIN_EXE_rift-memd"));
    let rift = memd.with_file_name(format!("rift{}", std::env::consts::EXE_SUFFIX));
    assert!(
        rift.is_file(),
        "no rift binary beside rift-memd at {}: build both (cargo test -p rift -p rift-memd)",
        rift.display()
    );
    rift
}

/// A repository with one commit, Recall declared on, and one linked member
/// at `member` on its own branch.
fn seed_repository(repo: &Path, member: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(repo, &["add", "a.txt"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::write(
        repo.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    let member = member.display().to_string();
    git(repo, &["worktree", "add", "-q", "-b", "wt/lane-a", &member]);
}

/// Run `git` in `dir` with a fixed identity and no ambient re-targeting.
fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", dir.join("absent-gitconfig"))
        .env("GIT_AUTHOR_NAME", "rift-test")
        .env("GIT_AUTHOR_EMAIL", "t@t")
        .env("GIT_COMMITTER_NAME", "rift-test")
        .env("GIT_COMMITTER_EMAIL", "t@t")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z");
    for key in RETARGETING_ENV {
        cmd.env_remove(key);
    }
    let out = cmd.output().expect("git is on PATH");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
