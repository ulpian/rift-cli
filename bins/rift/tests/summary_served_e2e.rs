//! End-to-end: **a pull includes the stored outline of a file the checkout
//! still holds**, through the shipped binary.
//!
//! The binding is written by the one writer, over the file's redacted bytes,
//! into the family's own `recall.db`; the read it stands beside is seeded at
//! the projection's seam, anchored on the repository's real `HEAD` so the
//! branch scope admits it. `known-state` then answers `summary_included` with
//! the outline, and `read-summary` answers with the same outline — and once
//! the file moves, neither serves it.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use rift_store::{CanonicalStore as _, SqliteStore, Value};

mod common;
use common::rift_cmd;
#[allow(dead_code)] // The hooks suites share these builders; this file uses one.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::DetachedHooks;

const PATH: &str = "src/parse.rs";
const SOURCE: &str = "pub fn parse(input: &str) -> Vec<String> {
    input.split(',').map(str::to_owned).collect()
}

pub struct Parser {
    strict: bool,
}
";
const OUTLINE: &str = "7 lines, 136 bytes, 2 top-level defs\nfn parse L1-L3\nstruct Parser L5-L7\n";

struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn env(&self) -> Vec<(&'static str, String)> {
        let at = |sub: &str| self.home.join(sub).display().to_string();
        vec![
            ("RIFT_CACHE_DIR", at("cache")),
            ("RIFT_CONFIG_DIR", at("config")),
            ("RIFT_FAMILY_DIR", at("families")),
            ("HOME", self.home.display().to_string()),
            ("XDG_DATA_HOME", at("data")),
            ("LOCALAPPDATA", at("data")),
            ("RIFT_HEADLESS", "1".to_owned()),
            ("RIFT_NO_KEYCHAIN", "1".to_owned()),
            ("RIFT_CAPTURE_SYNC", "1".to_owned()),
        ]
    }

    fn json(&self, verb: &[&str]) -> serde_json::Value {
        let mut argv = vec!["--json"];
        argv.extend_from_slice(verb);
        let out = rift_cmd(&self.repo)
            .args(&argv)
            .envs(self.env())
            .stdin(Stdio::null())
            .output()
            .expect("rift runs");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(
            out.status.code(),
            Some(0),
            "rift {argv:?}: {stdout}{stderr}"
        );
        serde_json::from_str(&stdout)
            .unwrap_or_else(|e| panic!("rift {argv:?} printed no JSON: {e}\n{stdout}"))
    }

    /// The family's identity, minted on first ask.
    fn family(&self) -> rift_git::Family {
        let paths = rift_git::discover(&self.repo).expect("the fixture is a repository");
        let env = self.env();
        let lookup = |key: &str| env.iter().find(|(k, _)| *k == key).map(|(_, v)| v.clone());
        let rift_git::FamilyState::Minted(family) =
            rift_git::family::resolve_family_under(lookup, &paths, rift_git::MintPolicy::Allow)
                .unwrap()
        else {
            panic!("the fixture repository mints a family identity");
        };
        family
    }

    /// Set the family up on this machine: its capture projection exists.
    fn provision(&self) {
        let db =
            rift_store::projection_db_for_family_under(&self.home.join("cache"), &self.family());
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        rift_store::open_migrated(&db, &rift_store::default_migrations()).unwrap();
    }

    fn store(&self) -> SqliteStore {
        let db = rift_store::recall_db_for_family_under(&self.home.join("cache"), &self.family());
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        rift_store::open_migrated(&db, &rift_store::recall_migrations()).unwrap()
    }
}

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
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env_remove("GIT_COMMON_DIR")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

/// A repository with one committed code file, its family minted and set up
/// on this machine, and one recorded read of that file anchored on `HEAD`.
fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    let repo = home.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join(PATH), SOURCE).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main"]);
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-q", "-m", "seed"]);
    let f = Fixture {
        _tmp: tmp,
        home,
        repo,
    };
    f.provision();
    seed_read(&f, &git_in(&f.repo, &["rev-parse", "HEAD"]));
    f
}

fn sha256_hex(bytes: &[u8]) -> String {
    rift_recall_substrate::worktree::sha256_hex(bytes)
}

/// One checkpoint anchored on `head` that read `PATH` at its committed bytes.
fn seed_read(f: &Fixture, head: &str) {
    let store = f.store();
    let sha = sha256_hex(SOURCE.as_bytes());
    let rows: [(&str, &[&str], Vec<Value>); 4] = [
        (
            "n_checkpoint",
            &[
                "node_id",
                "dialect",
                "ordinal",
                "prev_checkpoint",
                "strategy",
                "merkle_root",
                "developer",
            ],
            vec![
                Value::text("c1"),
                Value::text("rift"),
                Value::Int(1),
                Value::Null,
                Value::Null,
                Value::Null,
                Value::Null,
            ],
        ),
        (
            "n_checkpoint_commit",
            &["node_id", "commit_oid", "schema_version"],
            vec![Value::text("c1"), Value::text(head), Value::Int(1)],
        ),
        (
            "n_file_revision",
            &["node_id", "path", "blob_oid", "content_sha256", "size"],
            vec![
                Value::text("rev"),
                Value::text(PATH),
                Value::text(&sha),
                Value::text(&sha),
                Value::Int(136),
            ],
        ),
        (
            "n_file_read",
            &[
                "node_id",
                "checkpoint_id",
                "tool_call_id",
                "file_revision",
                "input_tokens",
                "read_ordinal",
                "byte_range_lo",
                "byte_range_hi",
            ],
            vec![
                Value::text("read"),
                Value::text("c1"),
                Value::text("tc"),
                Value::text("rev"),
                Value::Int(900),
                Value::Int(1),
                Value::Null,
                Value::Null,
            ],
        ),
    ];
    let rt = runtime();
    for (table, columns, values) in rows {
        rt.block_on(store.insert_or_ignore(table, columns, &values))
            .unwrap();
    }
}

/// Bind the outline of `PATH` as the detached worker will: redacted bytes,
/// the real tokenizer, the one writer.
fn bind_outline(f: &Fixture) -> String {
    let source =
        rift_shadow::redact::redact(&rift_core::SecretBytes::new(SOURCE.as_bytes().to_vec()))
            .unwrap()
            .redacted;
    let counts = rift_recall_ledger::validity_store::OutlineCounts::measure(
        PATH,
        &source,
        &rift_tokens::count,
        rift_tokens::provenance(),
    )
    .unwrap();
    let file = rift_recall_ledger::validity_store::OutlineSource {
        path: PATH,
        source: &source,
        policy_version: rift_recall_policy::SERVE_POLICY_VERSION,
        refuses_path: &|_| false,
    };
    let store = f.store();
    runtime()
        .block_on(rift_recall_ledger::validity_store::put_file_outline(
            &store,
            &rift_store::PlainCipher,
            &file,
            &counts,
        ))
        .unwrap()
        .expect("a code file binds")
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

fn known_state() -> [&'static str; 6] {
    [
        "recall",
        "known-state",
        "--task",
        "review the parser",
        "--file",
        PATH,
    ]
}

fn entry_for(doc: &serde_json::Value) -> serde_json::Value {
    doc["result"]["envelopes"]
        .as_array()
        .unwrap_or_else(|| panic!("known-state lists envelopes: {doc}"))
        .iter()
        .find(|e| e["path"] == serde_json::json!(PATH))
        .cloned()
        .unwrap_or_else(|| panic!("{PATH} is ranked: {doc}"))
}

#[test]
fn known_state_includes_the_bound_outline_of_a_file_the_checkout_holds() {
    let f = fixture();
    let before = entry_for(&f.json(&known_state()));
    assert_eq!(before["state"], serde_json::json!("available_by_handle"));

    let key = bind_outline(&f);
    let after = entry_for(&f.json(&known_state()));
    assert_eq!(
        after["state"],
        serde_json::json!("summary_included"),
        "{after}"
    );
    assert_eq!(after["summary"], serde_json::json!(OUTLINE));
    assert_eq!(after["summary_key"], serde_json::json!(key));
    assert_eq!(
        after["basis_sha256"],
        serde_json::json!(sha256_hex(SOURCE.as_bytes()))
    );

    let summary = f.json(&["recall", "read-summary", PATH]);
    assert_eq!(
        summary["result"]["result"]["state"],
        serde_json::json!("summary")
    );
    assert_eq!(
        summary["result"]["result"]["summary"],
        serde_json::json!(OUTLINE)
    );
}

#[test]
fn a_moved_file_is_served_no_outline() {
    let f = fixture();
    bind_outline(&f);
    std::fs::write(
        f.repo.join(PATH),
        format!("{SOURCE}\nconst EXTRA: u8 = 1;\n"),
    )
    .unwrap();
    let entry = entry_for(&f.json(&known_state()));
    assert_ne!(
        entry["state"],
        serde_json::json!("summary_included"),
        "{entry}"
    );
    let summary = f.json(&["recall", "read-summary", PATH]);
    assert_eq!(
        summary["result"]["result"]["state"],
        serde_json::json!("stale")
    );
}

/// Repository policy, committed with the seed so the condense reads Recall on.
const POLICY: &str = "schema_version = 1\n[project]\nname = \"outlines\"\n\
[features]\ntotal_recall = true\n";

/// A session journal the capture path accepts as live. Fixed timestamp.
const JOURNAL: &str = r#"{"schema":1,"session_id":"s-outline","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":true,"compaction_boundary":null,"updated_at_ms":1783000000000,"native_id_map":{}}"#;

impl Fixture {
    fn rift(&self, args: &[&str], sync: bool) -> std::process::Child {
        let mut cmd = rift_cmd(&self.repo);
        cmd.args(args)
            .envs(self.env())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if !sync {
            cmd.env_remove("RIFT_CAPTURE_SYNC");
        }
        cmd.spawn().expect("rift runs")
    }

    /// The live session reads `PATH` through the agent hook.
    fn agent_read(&self) {
        use std::io::Write as _;
        let body = serde_json::json!({
            "schema": 1,
            "session": "s-outline",
            "turn": 1,
            "payload": {
                "tool_name": "Read",
                "tool_input": { "file_path": self.repo.join(PATH).display().to_string() },
            },
        })
        .to_string();
        let mut child = self.rift(&["hooks", "claude-code", "PostToolUse"], true);
        child
            .stdin
            .take()
            .unwrap()
            .write_all(body.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success(), "a hook never fails");
    }

    fn post_commit_inline(&self) -> std::process::Child {
        let git_dir = self.repo.join(".git");
        self.rift(
            &[
                "hooks",
                "git",
                "post-commit",
                "--git-dir",
                git_dir.to_str().unwrap(),
            ],
            true,
        )
    }

    /// The environment of a detached run: the fixture's, without the
    /// variable that makes a hook drain its queue inline.
    fn detached_env(&self) -> Vec<(&'static str, String)> {
        self.env()
            .into_iter()
            .filter(|(key, _)| *key != "RIFT_CAPTURE_SYNC")
            .collect()
    }

    /// Run `rift <args>` in its own process group, leaving any worker it
    /// spawns detached for `hooks` to wait on.
    fn detached(&self, hooks: &DetachedHooks, args: &[&str]) {
        let env = self.detached_env();
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let (code, stderr) = hooks.run(&self.repo, args, "", &env);
        assert_eq!(code, 0, "rift {args:?} never fails: {stderr}");
    }

    fn git_dir_arg(&self) -> String {
        self.repo.join(".git").display().to_string()
    }

    fn count(&self, sql: &str) -> i64 {
        let rows = runtime().block_on(self.store().query(sql, &[])).unwrap();
        rows[0].get_i64(0).unwrap()
    }
}

/// A repository whose live session has read `PATH` and committed, with the
/// commit not yet condensed.
fn session_fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().to_path_buf();
    let repo = home.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::create_dir_all(repo.join(".rift")).unwrap();
    std::fs::create_dir_all(home.join("nohooks")).unwrap();
    git_in(&repo, &["init", "-q", "-b", "main"]);
    git_in(
        &repo,
        &[
            "config",
            "core.hooksPath",
            home.join("nohooks").to_str().unwrap(),
        ],
    );
    std::fs::write(repo.join(PATH), SOURCE).unwrap();
    std::fs::write(repo.join(".rift/config.toml"), POLICY).unwrap();
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-q", "-m", "seed"]);
    let sessions = common::seed_sessions_dir(&repo.join(".git"));
    std::fs::create_dir_all(&sessions).unwrap();
    std::fs::write(sessions.join("s-outline.json"), JOURNAL).unwrap();
    let f = Fixture {
        _tmp: tmp,
        home,
        repo,
    };
    f.agent_read();
    std::fs::write(f.repo.join("notes.txt"), "work\n").unwrap();
    git_in(&f.repo, &["add", "notes.txt"]);
    git_in(&f.repo, &["commit", "-q", "-m", "work"]);
    f
}

/// The detached worker's condense of the fixture's commit, waited out.
fn condense_detached(f: &Fixture) {
    let hooks = DetachedHooks::default();
    let git_dir = f.git_dir_arg();
    f.detached(
        &hooks,
        &["hooks", "git", "post-commit", "--git-dir", &git_dir],
    );
    hooks.await_workers(|| true);
}

fn chain_and_checkpoints(f: &Fixture) -> (usize, String) {
    let order = rift_git::chain::read_order(&f.repo.join(".git")).unwrap();
    let checkpoints = git_in(
        &f.repo,
        &["rev-list", "--count", "refs/heads/rift/checkpoints/v1"],
    );
    (order.len(), checkpoints)
}

#[test]
fn the_condense_binds_the_outline_a_pull_then_serves_until_the_file_moves() {
    let f = session_fixture();
    condense_detached(&f);

    let entry = entry_for(&f.json(&known_state()));
    assert_eq!(
        entry["state"],
        serde_json::json!("summary_included"),
        "{entry}"
    );
    assert_eq!(entry["summary"], serde_json::json!(OUTLINE));
    let summary = f.json(&["recall", "read-summary", PATH]);
    assert_eq!(
        summary["result"]["result"]["summary"],
        serde_json::json!(OUTLINE)
    );

    let moved = format!("{SOURCE}\npub const EXTRA: u8 = 1;\n");
    std::fs::write(f.repo.join(PATH), &moved).unwrap();
    let stale = f.json(&["recall", "read-summary", PATH]);
    assert_eq!(
        stale["result"]["result"],
        serde_json::json!({
            "state": "stale",
            "path": PATH,
            "current_hash": sha256_hex(moved.as_bytes()),
        })
    );
    assert!(!f.json(&known_state()).to_string().contains(OUTLINE));
}

#[test]
fn two_workers_racing_over_one_commit_record_one_checkpoint_one_entry_and_one_outline() {
    let f = session_fixture();
    let hooks = DetachedHooks::default();
    let git_dir = f.git_dir_arg();
    // The hook spawns its worker and returns; a second worker races it for
    // the drain lock while it runs.
    f.detached(
        &hooks,
        &["hooks", "git", "post-commit", "--git-dir", &git_dir],
    );
    f.detached(&hooks, &["hooks", "worker", "--git-dir", &git_dir]);
    hooks.await_workers(|| true);

    let (entries, checkpoints) = chain_and_checkpoints(&f);
    assert_eq!(checkpoints, "1", "one condense, under one lock");
    assert_eq!(entries, 1, "one chain entry, through the compare-and-set");
    assert_eq!(
        f.count(&format!(
            "SELECT COUNT(*) FROM summary_binding WHERE path = '{PATH}'"
        )),
        1
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM summary_token_count"), 3);
}

#[test]
fn a_condense_drained_inline_in_the_hook_binds_nothing_while_its_archive_and_entry_land() {
    let f = session_fixture();
    assert!(
        f.post_commit_inline().wait().unwrap().success(),
        "a hook never fails git"
    );
    let (entries, checkpoints) = chain_and_checkpoints(&f);
    assert_eq!(checkpoints, "1", "the archive landed");
    assert_eq!(entries, 1, "and its chain entry");
    assert_eq!(f.count("SELECT COUNT(*) FROM summary_binding"), 0);
    assert_eq!(f.count("SELECT COUNT(*) FROM summary_token_count"), 0);
}
