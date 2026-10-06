//! A cold rebuild restores the window journal a session froze into its
//! checkpoint.
//!
//! One Claude Code session is replayed through the shipped `rift` binary with
//! the capture worker drained inline: a file read, then asked for again, so
//! the gate declines the repeat; a `PreCompact`, so the window is emptied; and
//! a session end that condenses the window's rows into the checkpoint's
//! `window_journal.jsonl`. `rift-memd metrics --repo` ingests that checkpoint
//! and the `n_window_row` rows are recorded; then the cache root moves to an
//! empty directory, `rift-memd rebuild --repo` reads the same checkpoint, and
//! the rows are recorded again. The two sets must be one set.
//!
//! Every column is compared, `at` included: it is the instant the hook
//! stamped when it wrote the row, frozen into the checkpoint, so both passes
//! read the same value and no wall clock is consulted to check it.
//!
//! Hermetic as `rebuild_roundtrip_e2e` is: a fixture repository in a tempdir
//! and every root either binary could resolve redirected. The `rift` binary
//! is the sibling of `rift-memd` in the same target directory, which
//! `cargo test -p rift -p rift-memd` builds before any test starts.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rift_store::{CanonicalStore, Row, SqliteStore, Value};

/// The native session id the replay delivers on every hook.
const SESSION: &str = "window-journal-1";

/// Environment that re-targets a child git or rift at another repository.
const RETARGETING_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_COMMON_DIR",
    "GIT_INDEX_FILE",
    "GIT_CEILING_DIRECTORIES",
];

/// Every column of `n_window_row`, in a fixed order, keyed by the row id.
const ALL_ROWS: &str = "SELECT row_id, window_key, subagent, kind, turn, at, path, \
     content_sha256, hash_old, range_lo, range_hi, total_lines, sites, shift, full, \
     reason, file_revision, checkpoint_id, schema_version \
     FROM n_window_row ORDER BY row_id";

/// One window row as the assertions read it.
#[derive(Debug)]
struct Kinded {
    window_key: String,
    kind: String,
    reason: Option<String>,
    file_revision: Option<String>,
}

/// A fixture repository and the redirected roots both binaries run under.
struct Fixture {
    _tmp: tempfile::TempDir,
    repo: PathBuf,
    home: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        seed_repository(&repo);
        Self {
            _tmp: tmp,
            repo,
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

    /// One Claude Code hook of the session through `rift`, under the `live`
    /// cache root: its stdout.
    fn hook(&self, verb: &str, body: serde_json::Value) -> String {
        let mut body = body;
        body["session_id"] = SESSION.into();
        body["hook_event_name"] = verb.into();
        body["cwd"] = self.repo.display().to_string().into();
        let mut cmd = Command::new(rift_bin());
        cmd.current_dir(&self.repo)
            .args(["hooks", "claude-code", verb])
            .envs(self.env("live"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        for key in RETARGETING_ENV {
            cmd.env_remove(key);
        }
        let mut child = cmd.spawn().unwrap();
        let stdin = body.to_string();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0), "a hook never fails: {verb}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// `rift-memd <verb> --repo` over the fixture under `cache`: its stdout.
    fn memd(&self, cache: &str, verb: &str) -> String {
        let repo = self.repo.display().to_string();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_rift-memd"));
        cmd.current_dir(&self.home)
            .args([verb, "--repo", &repo])
            .envs(self.env(cache));
        for key in RETARGETING_ENV {
            cmd.env_remove(key);
        }
        let out = cmd.output().unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(
            out.status.success(),
            "rift-memd {verb}: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        stdout
    }
}

/// ★ The window rows a production ingest projects and the rows a cold
/// rebuild reads back from the checkpoint are one set, and the set holds the
/// session's read, its decline and its eviction.
#[test]
fn a_cold_rebuild_restores_every_window_row_the_live_ingest_projected() {
    let fx = Fixture::new();
    replay_session(&fx);

    let live = open_named(&fx.memd("live", "metrics"), "recall.db:");
    let before = select(&live, ALL_ROWS, &[]);
    let rebuilt = open_named(&fx.memd("rebuilt", "rebuild"), "rebuilt:");
    let after = select(&rebuilt, ALL_ROWS, &[]);

    assert_eq!(after, before, "the rebuild restores the window rows");
    assert_read_deny_and_evict(&rebuilt, &kinded(&rebuilt));
}

/// The session, replayed hook by hook: a read, its declined repeat, a
/// compaction and the end that condenses.
fn replay_session(fx: &Fixture) {
    fx.hook("SessionStart", serde_json::json!({"source": "startup"}));
    fx.hook("UserPromptSubmit", serde_json::json!({"prompt": "read it"}));
    decline_a_repeat_read(fx);
    fx.hook(
        "PreCompact",
        serde_json::json!({"trigger": "manual", "custom_instructions": ""}),
    );
    fx.hook("SessionEnd", serde_json::json!({"reason": "exit"}));
}

/// `booked.txt` read whole, then read again: the repeat is declined.
fn decline_a_repeat_read(fx: &Fixture) {
    let file = fx.repo.join("booked.txt");
    let body = (1..=40).fold(String::new(), |mut body, n| {
        let _ = writeln!(body, "line {n}");
        body
    });
    std::fs::write(&file, &body).unwrap();
    let read = serde_json::json!({
        "tool_name": "Read",
        "tool_input": {"file_path": file.display().to_string(), "offset": 1, "limit": 40},
        "tool_use_id": "toolu_window_read",
    });
    fx.hook("PreToolUse", read.clone());
    let mut done = read.clone();
    done["tool_response"] = serde_json::json!({
        "file": {"filePath": file.display().to_string(), "content": body},
    });
    fx.hook("PostToolUse", done);
    let repeat = fx.hook("PreToolUse", read);
    assert!(
        repeat.contains(r#""permissionDecision":"deny""#),
        "{repeat}"
    );
}

/// The read, the deny and the eviction are all in one window; the deny names
/// the revision the read did, and that revision is a file-revision node the
/// graph projected; the eviction says the client compacted.
fn assert_read_deny_and_evict(store: &SqliteStore, rows: &[Kinded]) {
    let one = |kind: &str| {
        let found: Vec<&Kinded> = rows.iter().filter(|row| row.kind == kind).collect();
        let [row] = found.as_slice() else {
            panic!("one {kind} row in {rows:?}");
        };
        *row
    };
    let (read, deny, evict) = (one("read"), one("deny"), one("evict"));
    assert_eq!(deny.window_key, read.window_key, "{rows:?}");
    assert_eq!(evict.window_key, read.window_key, "{rows:?}");
    let revision = read.file_revision.as_deref().expect("the read's revision");
    assert_eq!(deny.file_revision.as_deref(), Some(revision), "{rows:?}");
    let node = select(
        store,
        "SELECT path FROM n_file_revision WHERE node_id = ?",
        &[Value::text(revision)],
    );
    let [node] = node.as_slice() else {
        panic!("the read's revision {revision} is one n_file_revision node: {node:?}");
    };
    assert!(
        node.get_text(0).unwrap().ends_with("booked.txt"),
        "{node:?}"
    );
    assert_eq!(evict.reason.as_deref(), Some("pre_compact"), "{rows:?}");
}

/// The rows of `store`'s window journal, by row id, as the assertions read
/// them.
fn kinded(store: &SqliteStore) -> Vec<Kinded> {
    let sql = "SELECT window_key, kind, reason, file_revision FROM n_window_row ORDER BY row_id";
    select(store, sql, &[])
        .iter()
        .map(|row| Kinded {
            window_key: row.get_text(0).unwrap(),
            kind: row.get_text(1).unwrap(),
            reason: row.get_opt_text(2).unwrap(),
            file_revision: row.get_opt_text(3).unwrap(),
        })
        .collect()
}

/// The store a `rift-memd` report names on its `label` line, open.
fn open_named(report: &str, label: &str) -> SqliteStore {
    let path = report
        .lines()
        .find_map(|line| line.strip_prefix(label))
        .unwrap_or_else(|| panic!("the report names {label}: {report}"));
    SqliteStore::open_read_only(path.trim()).unwrap()
}

/// Every row `sql` selects from `store`.
fn select(store: &SqliteStore, sql: &str, params: &[Value]) -> Vec<Row> {
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

/// A repository with one commit, Recall declared on, and declines asked for
/// Claude Code in its own policy file.
fn seed_repository(repo: &Path) {
    git(repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(repo, &["add", "a.txt"]);
    git(repo, &["commit", "-q", "-m", "seed"]);
    let policies = repo.join(".rift").join("policies");
    std::fs::create_dir_all(&policies).unwrap();
    std::fs::write(
        repo.join(".rift/config.toml"),
        "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
    )
    .unwrap();
    std::fs::write(
        policies.join("serve.toml"),
        "[interception]\nmode = \"enforce\"\nmin_slice_lines = 1\n\n\
         [interception.agents]\nclaude-code = \"enforce\"\n",
    )
    .unwrap();
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
