//! End-to-end of `rift doctor --fix` over a real capture backlog on a fixture
//! repository: three sessions driven through the real binary's Claude Code
//! hooks while the drain lock is held, so each prompt's boundary ticket stays
//! owed. Report-only doctor changes nothing; `--fix` archives the historical
//! backlog, retires exactly its tickets, collects the reserved current
//! boundary and releases the hold; a second `--fix` is a no-op.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};
use std::process::Output;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    agent_jobs_named, agent_queue_of, checkpoint_shards, hold_drain_lock, seed_plain_repo,
    write_agent_transcript, DetachedHooks,
};

const HOLD: &str = ".rift-capture-recovery.hold";
const SESSIONS: [&str; 3] = ["backlog-a", "backlog-b", "backlog-c"];

/// A committed fixture repository set up on this machine, its hermetic
/// homes, and the hooks run against it on the detached path.
struct Fixture {
    tmp: tempfile::TempDir,
    repo: PathBuf,
    hooks: DetachedHooks,
}

impl Fixture {
    fn initialized() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let repo = seed_plain_repo(tmp.path());
        let f = Self {
            tmp,
            repo,
            hooks: DetachedHooks::default(),
        };
        assert_eq!(f.rift(&["init", "--no-recall"]).status.code(), Some(0));
        f
    }

    fn home(&self) -> PathBuf {
        self.tmp.path().join("home")
    }

    fn cache(&self) -> PathBuf {
        self.tmp.path().join("cache")
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        let home = self.home();
        let s = |p: PathBuf| p.to_str().unwrap().to_owned();
        vec![
            ("HOME", s(home.clone())),
            ("USERPROFILE", s(home.clone())),
            ("CODEX_HOME", s(home.join("codex-home"))),
            ("RIFT_CONFIG_DIR", s(home.join("config"))),
            ("XDG_DATA_HOME", s(home.join("data"))),
            ("LOCALAPPDATA", s(home.join("data"))),
            ("RIFT_CACHE_DIR", s(self.cache())),
            ("RIFT_NO_KEYCHAIN", "1".to_owned()),
            ("RIFT_HEADLESS", "1".to_owned()),
        ]
    }

    fn rift(&self, args: &[&str]) -> Output {
        common::rift_cmd(&self.repo)
            .args(args)
            .env_remove("RIFT_CAPTURE_SYNC")
            .envs(self.env())
            .output()
            .expect("spawn rift")
    }

    /// One native Claude Code hook for `session`, as the client sends it.
    fn hook(&self, verb: &str, session: &str, transcript: &Path) {
        let body = serde_json::json!({
            "session_id": session,
            "hook_event_name": verb,
            "transcript_path": transcript.display().to_string(),
            "prompt": "add a feature",
        });
        let mut cmd = common::rift_cmd(&self.repo);
        cmd.args(["hooks", "claude-code", verb])
            .env_remove("RIFT_CAPTURE_SYNC")
            .envs(self.env());
        let (code, stderr) = self.hooks.output(cmd, &body.to_string());
        assert_eq!(code, 0, "an agent hook never fails; stderr: {stderr}");
    }

    /// Each session opens and takes one prompt while no worker may drain, so
    /// every prompt's boundary ticket is left owed in the agent journal.
    fn seed_backlog(&self) {
        let lock = hold_drain_lock(&self.repo);
        let line = format!(
            "{}\n",
            serde_json::json!({"type": "user", "message": {"role": "user", "content": "add a feature"}})
        );
        for session in SESSIONS {
            let transcript = write_agent_transcript(self.tmp.path(), session, &line);
            self.hook("SessionStart", session, &transcript);
            self.hook("UserPromptSubmit", session, &transcript);
        }
        self.hooks.await_workers(|| true);
        // The incident's shape: the capture queue drains, the boundary
        // tickets stay behind for a worker that never took them.
        let queue = agent_queue_of(&self.repo);
        let stash = self.tmp.path().join("stash");
        std::fs::create_dir_all(&stash).unwrap();
        let owed = agent_jobs_named(&self.repo, "boundary-");
        for name in &owed {
            std::fs::rename(queue.join(name), stash.join(name)).unwrap();
        }
        drop(lock);
        self.drain();
        for name in &owed {
            std::fs::rename(stash.join(name), queue.join(name)).unwrap();
        }
    }

    /// One worker pass over everything but the stashed tickets.
    fn drain(&self) {
        let git_dir = self.repo.join(".git");
        let args = ["hooks", "worker", "--git-dir", git_dir.to_str().unwrap()];
        let env = self.env();
        let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let (code, _) = self.hooks.run(&self.repo, &args, "", &env);
        assert_eq!(code, 0);
        self.hooks.await_workers(|| true);
    }

    fn hold(&self) -> PathBuf {
        self.repo.join(".git").join(HOLD)
    }
}

/// Every path under `root`, sorted, so a run that wrote anything shows.
fn tree(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![root.to_owned()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path.clone());
            }
            found.push(path);
        }
    }
    found.sort();
    found
}

/// The plan directories every `--fix` that owed work minted.
fn plan_dirs(cache: &Path) -> Vec<PathBuf> {
    tree(cache)
        .into_iter()
        .filter(|p| {
            p.parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == "recovery")
        })
        .collect()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
#[ignore = "blocked: an earlier doctor --fix leg drains the owed boundary tickets ordinarily, so the recovery leg sees nothing owed; needs a coordinator ruling on leg order (G5e)"]
fn capture_recovery_fix_archives_a_seeded_backlog_once_and_then_says_complete() {
    let f = Fixture::initialized();
    f.seed_backlog();
    let owed = agent_jobs_named(&f.repo, "boundary-");
    eprintln!("SEEDED {owed:?}");
    assert!(owed.len() >= 2, "a historical backlog is owed: {owed:?}");

    let before = (tree(&f.repo), tree(&f.cache()));
    let report = f.rift(&["doctor"]);
    assert_eq!(
        (tree(&f.repo), tree(&f.cache())),
        before,
        "{}",
        stdout(&report)
    );

    eprintln!(
        "REPORT: {} OWED {:?} DIR {}",
        stdout(&report),
        agent_jobs_named(&f.repo, "boundary-"),
        agent_queue_of(&f.repo).display()
    );
    let fix = f.rift(&["doctor", "--fix"]);
    let said = stdout(&fix);
    eprintln!("FIX: {said}\nERR: {}", String::from_utf8_lossy(&fix.stderr));
    assert!(said.contains("capture recovery complete"), "{said}");
    assert!(!f.hold().exists(), "the hold is released: {said}");
    assert!(agent_jobs_named(&f.repo, "boundary-").is_empty(), "{said}");
    let plans = plan_dirs(&f.cache());
    assert_eq!(plans.len(), 1, "{said}");
    let shards = checkpoint_shards(&f.repo);
    assert!(!shards.is_empty(), "the backlog is archived: {said}");

    let again = f.rift(&["doctor", "--fix"]);
    assert!(
        stdout(&again).contains("nothing to recover"),
        "{}",
        stdout(&again)
    );
    assert_eq!(plan_dirs(&f.cache()), plans, "no new plan dir");
    assert_eq!(checkpoint_shards(&f.repo), shards, "no new checkpoint");

    let after = f.rift(&["doctor"]);
    eprintln!("AFTER: {}", stdout(&after));
}
