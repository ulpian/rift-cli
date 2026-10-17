//! End-to-end: a turn boundary's working-tree snapshot leaves the agent hook.
//! The hook journals it for the detached worker with `HEAD` and the instant,
//! and the worker writes the boundary rows; under `RIFT_CAPTURE_SYNC` the hook
//! drains that journal itself before it returns.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_jobs_named, agent_queue_of, capture_queue_of, hold_drain_lock, seed_live_session_repo,
    sync_env, DetachedHooks,
};

const SESSION: &str = "e2e-live-1";

/// A live session's repository, its cache root, and the hooks run against it
/// on the detached path.
struct Fixture {
    repo: PathBuf,
    cache: PathBuf,
    hooks: DetachedHooks,
}

impl Fixture {
    fn new(tmp: &Path) -> Self {
        Self {
            repo: seed_live_session_repo(tmp),
            cache: tmp.join("cache"),
            hooks: DetachedHooks::default(),
        }
    }

    /// The hermetic environment with the journal left for a worker.
    fn detached_env(&self) -> [(&'static str, &str); 2] {
        [
            ("RIFT_CACHE_DIR", self.cache.to_str().unwrap()),
            ("RIFT_NO_KEYCHAIN", "1"),
        ]
    }

    /// One Claude Code prompt for the live session — the hook that opens a
    /// turn and so closes the human's window — under exactly `env`.
    fn prompt(&self, env: &[(&str, &str)]) {
        let body = format!(
            r#"{{"schema":1,"session":"{SESSION}","turn":1,"payload":{{"text":"add a feature"}}}}"#
        );
        let mut cmd = common::rift_cmd(&self.repo);
        cmd.args(["hooks", "claude-code", "UserPromptSubmit"])
            .env_remove("RIFT_CAPTURE_SYNC")
            .envs(env.iter().copied());
        let (code, stderr) = self.hooks.output(cmd, &body);
        assert_eq!(code, 0, "an agent hook never fails; stderr: {stderr}");
    }

    /// One worker pass, and the wait for every process the fixture started.
    fn drain(&self) {
        let git_dir = self.repo.join(".git");
        let args = ["hooks", "worker", "--git-dir", git_dir.to_str().unwrap()];
        let (code, _) = self.hooks.run(&self.repo, &args, "", &self.detached_env());
        assert_eq!(code, 0);
        self.hooks.await_workers(|| true);
    }

    /// The journaled job named `name`, as the hook wrote it.
    fn job(&self, name: &str) -> serde_json::Value {
        let bytes = std::fs::read(agent_queue_of(&self.repo).join(name)).unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    /// Every said-once note the capture queue holds, concatenated.
    fn notes(&self) -> String {
        let Ok(dir) = std::fs::read_dir(capture_queue_of(&self.repo)) else {
            return String::new();
        };
        dir.filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|x| x == "error"))
            .filter_map(|path| std::fs::read_to_string(path).ok())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// How many boundary snapshots the session's projection holds.
    fn boundary_rows(&self) -> i64 {
        let repo_id = rift_git::repo_id_for(&self.repo).unwrap();
        let db = rift_store::projection_db_under(&self.cache, &repo_id);
        if !db.exists() {
            return 0;
        }
        let store = rift_store::SqliteStore::open_read_only(&db).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(rift_store::CanonicalStore::count(
            &store,
            "turn_boundary_snapshot",
        ))
        .unwrap()
    }
}

/// The logical identity a context-addressed job file is published under.
fn logical_stem(name: &str) -> &str {
    name.split_once("--").map_or(name, |(stem, _)| stem)
}

/// The dotted paths of every scalar on which two job values disagree.
fn differing_fields(a: &serde_json::Value, b: &serde_json::Value) -> Vec<String> {
    fn walk(prefix: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
        match (a.as_object(), b.as_object()) {
            (Some(a), Some(b)) => {
                let keys: std::collections::BTreeSet<_> = a.keys().chain(b.keys()).collect();
                for key in keys {
                    let path = if prefix.is_empty() {
                        key.clone()
                    } else {
                        format!("{prefix}.{key}")
                    };
                    let null = serde_json::Value::Null;
                    walk(
                        &path,
                        a.get(key).unwrap_or(&null),
                        b.get(key).unwrap_or(&null),
                        out,
                    );
                }
            }
            _ if a != b => out.push(prefix.to_owned()),
            _ => (),
        }
    }
    let mut out = Vec::new();
    walk("", a, b, &mut out);
    out
}

/// The hook that opens a turn journals the snapshot the boundary owes and
/// writes no boundary row of its own; the worker then writes it, and takes
/// the job off the journal.
#[test]
fn a_boundary_hook_journals_its_snapshot_and_writes_no_boundary_row_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Fixture::new(tmp.path());
    std::fs::write(fx.repo.join("a.txt"), "one\nhuman line two\n").unwrap();
    let lock = hold_drain_lock(&fx.repo);
    fx.prompt(&fx.detached_env());

    assert_eq!(
        agent_jobs_named(&fx.repo, "boundary-").len(),
        1,
        "the prompt owes one snapshot"
    );
    assert_eq!(fx.boundary_rows(), 0, "the hook took no snapshot itself");

    drop(lock);
    fx.drain();
    assert_eq!(fx.boundary_rows(), 1, "the worker took it");
    assert!(agent_jobs_named(&fx.repo, "boundary-").is_empty());
}

/// The same prompt delivered twice owes one snapshot. Each delivery is
/// published under its full context, so the second (one logical boundary at a
/// later instant) is a second file rather than an overwrite; the worker takes
/// the first, and the second stays owed under its typed reason, no attempt
/// spent, because the row it would write already exists.
#[test]
fn a_redelivered_boundary_hook_journals_one_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Fixture::new(tmp.path());
    let lock = hold_drain_lock(&fx.repo);
    fx.prompt(&fx.detached_env());
    let first = agent_jobs_named(&fx.repo, "boundary-");
    fx.prompt(&fx.detached_env());

    // A changed instant is a changed context: both deliveries stay owed.
    assert_eq!(first.len(), 1, "{first:?}");
    let both = agent_jobs_named(&fx.repo, "boundary-");
    assert_eq!(both.len(), 2, "{both:?}");
    assert!(both.contains(&first[0]));
    assert_eq!(logical_stem(&both[0]), logical_stem(&both[1]));
    assert_eq!(
        differing_fields(&fx.job(&both[0]), &fx.job(&both[1])),
        ["kind.captured_at_ms", "queued_at_ms"],
        "the two deliveries differ only in their instants"
    );
    drop(lock);
    fx.drain();
    assert_eq!(fx.boundary_rows(), 1);
    let owed = agent_jobs_named(&fx.repo, "boundary-");
    assert_eq!(owed.len(), 1, "the redelivery stays owed: {owed:?}");
    assert_eq!(fx.job(&owed[0])["attempts"], 0, "and no attempt was spent");
    assert!(
        fx.notes()
            .contains("boundary snapshot lacks exact queued input proof"),
        "under its typed reason: {}",
        fx.notes()
    );
}

/// Under `RIFT_CAPTURE_SYNC` the hook drains its own journal, so a caller that
/// must see the boundary when the hook returns still does.
#[test]
fn under_capture_sync_the_snapshot_lands_before_the_hook_returns() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Fixture::new(tmp.path());
    fx.prompt(&sync_env(&fx.cache));

    assert_eq!(fx.boundary_rows(), 1);
    assert!(agent_jobs_named(&fx.repo, "boundary-").is_empty());
    fx.hooks.await_workers(|| true);
}
