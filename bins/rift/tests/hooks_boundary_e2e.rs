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
    agent_jobs_named, hold_drain_lock, seed_live_session_repo, sync_env, DetachedHooks,
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

/// The same prompt delivered twice owes the one snapshot its first delivery
/// journaled.
#[test]
fn a_redelivered_boundary_hook_journals_one_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Fixture::new(tmp.path());
    let lock = hold_drain_lock(&fx.repo);
    fx.prompt(&fx.detached_env());
    let first = agent_jobs_named(&fx.repo, "boundary-");
    fx.prompt(&fx.detached_env());

    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(agent_jobs_named(&fx.repo, "boundary-"), first);
    drop(lock);
    fx.drain();
    assert_eq!(fx.boundary_rows(), 1);
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
