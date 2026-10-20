//! End-to-end: one read followed through the four stages on the fixture
//! repository — the hook's receipt, the checkpoint that froze it, the recall
//! ingest that made it queryable, and the answer a repeat read got — and the
//! surfaces that report each read's furthest stage (CFX-6 slice 4).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::Path;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
#[allow(dead_code)] // The read-gate suites share these builders; each file uses a subset.
#[path = "common/read_gate_fixture.rs"]
mod read_gate_fixture;

use hooks_fixture::{
    archive_head, git_in, now_ms, run_rift_stdout, seed_live_session_repo, LIVE_SESSION,
};
use read_gate_fixture::{declined_with, enforce_for, hook, read_whole_of, windows};

/// The environment both fixtures' hooks run under, in each one's shape.
struct Env {
    gate: [(String, String); 3],
}

impl Env {
    fn new(cache: &Path) -> Self {
        Self {
            gate: read_gate_fixture::gate_env(cache),
        }
    }

    fn borrowed(&self) -> Vec<(&str, &str)> {
        self.gate
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect()
    }

    /// One `Read` of `file` through the real claude-code `PreToolUse` hook.
    fn read(&self, repo: &Path, turn: u32, file: &Path) -> String {
        let payload = read_whole_of(file);
        hook(
            repo,
            "claude-code",
            "PreToolUse",
            LIVE_SESSION,
            turn,
            &payload,
            &self.gate,
        )
    }

    /// Freeze the live journal into a checkpoint (a commit's post-commit
    /// archive) and let recall ingest it.
    fn capture_and_ingest(&self, repo: &Path, message: &str) {
        git_in(repo, &["commit", "-q", "--allow-empty", "-m", message]);
        archive_head(repo, &self.borrowed());
        let (code, text) = run_rift_stdout(repo, &["memory", "cost-hotspots"], &self.borrowed());
        assert_eq!(code, 0, "ingest: {text}");
    }

    fn read_stages(&self, repo: &Path) -> serde_json::Value {
        let (code, text) = run_rift_stdout(repo, &["recall", "status", "--json"], &self.borrowed());
        assert_eq!(code, 0, "recall status: {text}");
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        value["read_stages"].clone()
    }

    fn doctor(&self, repo: &Path) -> String {
        run_rift_stdout(repo, &["doctor"], &self.borrowed()).1
    }
}

#[test]
fn read_stages_follow_a_read_to_a_hash_bound_answer_and_doctor_stays_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = Env::new(&tmp.path().join("cache"));
    let file = repo.join("stage.txt");
    std::fs::write(&file, "a\nb\nc\nd\n").unwrap();

    // Receipt: the first read runs and is journaled.
    assert_eq!(declined_with(&env.read(&repo, 1, &file)), None);
    // Durable capture, then query readiness.
    env.capture_and_ingest(&repo, "capture the first read");

    // Final delivery: the unchanged repeat is answered from the bound fact...
    let repeat = env.read(&repo, 2, &file);
    assert!(declined_with(&repeat).is_some(), "unchanged: {repeat}");
    // ...and an edited file is never answered as unchanged.
    std::fs::write(&file, "a\nb\nc\nd\ne\n").unwrap();
    assert_eq!(declined_with(&env.read(&repo, 2, &file)), None);
    env.capture_and_ingest(&repo, "capture the repeat and the edited read");

    let stages = env.read_stages(&repo);
    assert_eq!(stages["reads"], 2, "two revisions read: {stages}");
    assert_eq!(stages["furthest"]["queryable"], 2, "{stages}");
    assert_eq!(
        stages["delivered"], 1,
        "only the unchanged repeat: {stages}"
    );
    assert_eq!(stages["never_queryable"], 0, "{stages}");
    assert!(
        stages["joins"]["path_hash"].as_u64().unwrap() >= 1,
        "{stages}"
    );
    assert_eq!(stages["joins"]["ambiguous"], 0, "{stages}");
    assert!(stages["lag"]["archival"]["p50_ms"].is_i64(), "{stages}");
    assert!(stages["lag"]["delivery"]["p50_ms"].is_i64(), "{stages}");
    // Query lag is not asserted: ingest stamps `ingest_watermark.ingested_at`
    // with the constant `graph::INGEST_TS` (0), so readiness has no time of
    // its own yet and both reads count as out-of-order (CFX-6 open follow-up).
    let doctor = env.doctor(&repo);
    assert!(!doctor.contains("read stages:"), "{doctor}");
}

/// Age the one read line of the repository's one window by ten minutes, the
/// way a read whose turn never sealed looks once the sweep has passed it.
fn age_the_read(repo: &Path) {
    let [window] = windows(repo).try_into().unwrap();
    let path = repo.join(".rift").join("state").join("reads").join(window);
    let now = u64::try_from(now_ms()).unwrap();
    let aged: Vec<String> = std::fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(|line| {
            let mut row: serde_json::Value = serde_json::from_str(line).unwrap();
            for value in row.as_object_mut().unwrap().values_mut() {
                // The write time is the one field within a minute of now.
                if value.as_u64().is_some_and(|ms| now.abs_diff(ms) < 60_000) {
                    *value = (value.as_u64().unwrap() - 10 * 60 * 1000).into();
                }
            }
            row.to_string()
        })
        .collect();
    std::fs::write(&path, aged.join("\n") + "\n").unwrap();
}

#[test]
fn read_stages_name_a_read_whose_capture_never_happened_in_doctor() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = seed_live_session_repo(tmp.path());
    enforce_for(&repo, &["claude-code"]);
    let env = Env::new(&tmp.path().join("cache"));
    // Both stores exist, but the read below is never archived.
    env.capture_and_ingest(&repo, "an empty capture");
    let file = repo.join("blocked.txt");
    std::fs::write(&file, "x\ny\n").unwrap();
    assert_eq!(declined_with(&env.read(&repo, 1, &file)), None);
    age_the_read(&repo);

    let stages = env.read_stages(&repo);
    assert_eq!(stages["reads"], 1, "{stages}");
    assert_eq!(stages["furthest"]["received"], 1, "{stages}");
    assert_eq!(stages["never_queryable"], 1, "{stages}");
    let doctor = env.doctor(&repo);
    assert!(
        doctor.contains("read stages:") && doctor.contains("never became queryable"),
        "{doctor}"
    );
}
