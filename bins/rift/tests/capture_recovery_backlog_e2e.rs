//! End-to-end of `rift doctor --fix` over a real capture backlog on a fixture
//! repository: three sessions driven through the real binary's Claude Code
//! hooks while the drain lock is held, so each prompt's boundary ticket stays
//! owed. Report-only doctor changes nothing; `--fix` archives the historical
//! backlog, retires exactly its tickets, collects the reserved current
//! boundary and releases the hold; a second `--fix` is a no-op.
// Capture recovery pins tickets by hard link and descriptor identity, which
// only the unix build implements; elsewhere the doctor reports "capture
// recovery runs on unix only" and there is nothing for these suites to drive.
#![cfg(unix)]
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
        self.seed_prompts();
        // One session ends while the lock is still held: its queued condense
        // is the carrier the gap rides, as in the incident.
        let last = SESSIONS[SESSIONS.len() - 1];
        let transcript = write_agent_transcript(self.tmp.path(), last, &prompt_line());
        self.hook("SessionEnd", last, &transcript);
        self.strand_tickets(lock);
    }

    /// The live machine's shape: no session is mid-end, so no condense can
    /// carry and the gap rides on recovery's own baseline.
    fn seed_backlog_without_carrier(&self) {
        let lock = hold_drain_lock(&self.repo);
        self.seed_prompts();
        self.strand_tickets(lock);
    }

    /// The owner's live shape: every session ended and its condense ran, but
    /// one condense ticket stayed queued beside the old per-session marker and
    /// no completion marker, so no condense is admissible and no session is
    /// active at a current boundary.
    fn seed_ended_backlog_with_legacy_condense(&self) {
        let lock = hold_drain_lock(&self.repo);
        self.seed_prompts();
        for session in SESSIONS {
            let transcript = write_agent_transcript(self.tmp.path(), session, &prompt_line());
            self.hook("SessionEnd", session, &transcript);
        }
        self.hooks.await_workers(|| true);
        let queue = agent_queue_of(&self.repo);
        let stash = self.tmp.path().join("stash");
        std::fs::create_dir_all(&stash).unwrap();
        let boundaries = agent_jobs_named(&self.repo, "boundary-");
        for name in &boundaries {
            std::fs::rename(queue.join(name), stash.join(name)).unwrap();
        }
        let kept = agent_jobs_named(&self.repo, "condense-").remove(0);
        let ticket = std::fs::read_to_string(queue.join(&kept)).unwrap();
        std::fs::copy(queue.join(&kept), stash.join(&kept)).unwrap();
        drop(lock);
        self.drain();
        for name in boundaries.iter().chain([&kept]) {
            std::fs::rename(stash.join(name), queue.join(name)).unwrap();
        }
        for entry in std::fs::read_dir(&queue).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name.starts_with("completed-") {
                std::fs::remove_file(path).unwrap();
            }
        }
        let session = SESSIONS.into_iter().find(|s| ticket.contains(s)).unwrap();
        let key = rift_shadow::sha256_hex(&[session.as_bytes()]);
        std::fs::write(queue.join(format!("condensed-{key}")), b"").unwrap();
    }

    /// Two sessions end while no worker drains: two admissible condenses.
    fn seed_backlog_with_two_carriers(&self) {
        let lock = hold_drain_lock(&self.repo);
        self.seed_prompts();
        for session in &SESSIONS[1..] {
            let transcript = write_agent_transcript(self.tmp.path(), session, &prompt_line());
            self.hook("SessionEnd", session, &transcript);
        }
        self.strand_tickets(lock);
    }

    fn seed_prompts(&self) {
        for session in SESSIONS {
            let transcript = write_agent_transcript(self.tmp.path(), session, &prompt_line());
            self.hook("SessionStart", session, &transcript);
            self.hook("UserPromptSubmit", session, &transcript);
        }
    }

    fn strand_tickets<L>(&self, lock: L) {
        self.hooks.await_workers(|| true);
        // The incident's shape: the capture queue drains, the boundary
        // tickets stay behind for a worker that never took them.
        let queue = agent_queue_of(&self.repo);
        let stash = self.tmp.path().join("stash");
        std::fs::create_dir_all(&stash).unwrap();
        let mut owed = agent_jobs_named(&self.repo, "boundary-");
        owed.extend(agent_jobs_named(&self.repo, "condense-"));
        for name in &owed {
            std::fs::rename(queue.join(name), stash.join(name)).unwrap();
        }
        drop(lock);
        self.drain();
        for name in &owed {
            std::fs::rename(stash.join(name), queue.join(name)).unwrap();
        }
        // The live queue's shape: markers the queue leaves beside its
        // tickets, which recovery must count and never refuse.
        let h64 = "f".repeat(64);
        std::fs::write(queue.join(format!("unavailable-basis-{h64}")), b"").unwrap();
        std::fs::write(queue.join(format!("completed-{h64}.done")), b"{}").unwrap();
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

fn prompt_line() -> String {
    format!(
        "{}\n",
        serde_json::json!({"type": "user", "message": {"role": "user", "content": "add a feature"}})
    )
}

/// The one record named `name` anywhere under `roots`, read whole.
fn record_named(roots: &[PathBuf], name: &str) -> String {
    let found = roots
        .iter()
        .flat_map(|root| tree(root))
        .find(|p| p.ends_with(name));
    std::fs::read_to_string(found.unwrap_or_else(|| panic!("no {name}"))).unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn capture_recovery_fix_archives_a_seeded_backlog_once_and_then_says_complete() {
    let f = Fixture::initialized();
    f.seed_backlog();
    let owed = agent_jobs_named(&f.repo, "boundary-");
    assert!(owed.len() >= 2, "a historical backlog is owed: {owed:?}");

    let before = (tree(&f.repo), tree(&f.cache()));
    let report = f.rift(&["doctor"]);
    assert_eq!(
        (tree(&f.repo), tree(&f.cache())),
        before,
        "{}",
        stdout(&report)
    );

    let fix = f.rift(&["doctor", "--fix"]);
    let said = stdout(&fix);
    assert!(said.contains("capture recovery complete"), "{said}");
    assert!(!f.hold().exists(), "the hold is released: {said}");
    // The historical tickets are retired and the reserved current one is
    // discharged by the observation that collected its baseline.
    let left = agent_jobs_named(&f.repo, "boundary-");
    assert!(left.is_empty(), "{left:?} {said}");
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

    // Report-only names a recovery only while a hold is held; complete means
    // no hold is named and the capture queue is empty.
    let after = stdout(&f.rift(&["doctor"]));
    assert!(!after.contains("capture recovery"), "{after}");
    assert!(after.contains("capture queue:  empty"), "{after}");
}

#[test]
fn capture_recovery_fix_carries_the_gap_on_its_own_baseline_when_no_session_is_ending() {
    let f = Fixture::initialized();
    f.seed_backlog_without_carrier();
    assert!(agent_jobs_named(&f.repo, "condense-").is_empty());
    assert!(agent_jobs_named(&f.repo, "boundary-").len() >= 2);

    let fix = f.rift(&["doctor", "--fix"]);
    let said = stdout(&fix);
    assert!(said.contains("capture recovery complete"), "{said}");
    assert!(!f.hold().exists(), "the hold is released: {said}");
    let left = agent_jobs_named(&f.repo, "boundary-");
    assert!(left.is_empty(), "{left:?} {said}");
    let roots = [f.repo.clone(), f.cache()];
    let intent = record_named(&roots, "archive-intent.json");
    assert!(intent.contains("carrier_baseline_checkpoint"), "{intent}");
    record_named(&roots, "publication-receipt.json");
    let shards = checkpoint_shards(&f.repo);
    assert!(!shards.is_empty(), "the backlog is archived: {said}");

    let again = stdout(&f.rift(&["doctor", "--fix"]));
    assert!(again.contains("nothing to recover"), "{again}");
    assert_eq!(checkpoint_shards(&f.repo), shards, "no new checkpoint");
}

#[test]
fn capture_recovery_fix_names_the_missing_baseline_when_no_session_is_active() {
    let f = Fixture::initialized();
    f.seed_ended_backlog_with_legacy_condense();
    assert_eq!(agent_jobs_named(&f.repo, "condense-").len(), 1);
    assert!(agent_jobs_named(&f.repo, "boundary-").len() >= 2);
    let shards = checkpoint_shards(&f.repo);

    let said = stdout(&f.rift(&["doctor", "--fix"]));
    assert!(said.contains("baseline-unreserved"), "{said}");
    assert!(!said.contains("carrier-owed"), "{said}");
    assert!(
        said.contains("none admissible: LegacyCondensed 1"),
        "{said}"
    );
    assert!(!f.hold().exists(), "the hold is released: {said}");
    assert_eq!(
        checkpoint_shards(&f.repo),
        shards,
        "nothing archived: {said}"
    );
}

#[test]
fn capture_recovery_fix_refuses_two_admissible_carriers_and_never_takes_the_baseline() {
    let f = Fixture::initialized();
    f.seed_backlog_with_two_carriers();
    assert_eq!(agent_jobs_named(&f.repo, "condense-").len(), 2);

    let said = stdout(&f.rift(&["doctor", "--fix"]));
    assert!(said.contains("carrier-owed"), "{said}");
    assert!(said.contains("2 admissible, none picked"), "{said}");
    assert!(!f.hold().exists(), "the hold is released: {said}");
    let roots = [f.repo.clone(), f.cache()];
    let intents = roots.iter().flat_map(|r| tree(r));
    assert!(!intents
        .into_iter()
        .any(|p| p.ends_with("archive-intent.json")));
}
