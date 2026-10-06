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
