//! End-to-end of the doctor's capture-recovery leg through the real binary on
//! a fixture repository: report-only, it names a held recovery and its age,
//! and leaves the hold and the cache exactly as it found them.
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

const HOLD: &str = ".rift-capture-recovery.hold";

struct Fixture {
    home: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl Fixture {
    fn initialized() -> Self {
        let fixture = Self {
            home: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
        };
        rift_git::test_support::fake_repo(fixture.repo.path()).unwrap();
        assert_eq!(
            fixture.rift(&["init", "--no-recall"]).status.code(),
            Some(0)
        );
        fixture
    }

    fn rift(&self, args: &[&str]) -> Output {
        let home = self.home.path();
        common::rift_cmd(self.repo.path())
            .args(args)
            .env("HOME", home)
            .env("USERPROFILE", home)
            .env("CODEX_HOME", home.join("codex-home"))
            .env("RIFT_CONFIG_DIR", home.join("config"))
            .env("RIFT_CACHE_DIR", home.join("cache"))
            .env("RIFT_HEADLESS", "1")
            .output()
            .expect("spawn rift")
    }

    fn hold(&self) -> PathBuf {
        self.repo.path().join(".git").join(HOLD)
    }
}

/// Every path under `root`, sorted, so a report that wrote anything shows.
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

#[test]
fn capture_recovery_report_names_a_held_recovery_and_changes_nothing() {
    let f = Fixture::initialized();
    let quiet = f.rift(&["doctor"]);
    assert!(!String::from_utf8_lossy(&quiet.stdout).contains("capture recovery held"));
    let hold = serde_json::json!({
        "member_key": "ab".repeat(32),
        "operation_id": "fix-1",
        "plan_sha256": "cd".repeat(32),
        "schema_version": 1,
    });
    std::fs::write(f.hold(), serde_json::to_vec(&hold).unwrap()).unwrap();
    let before = std::fs::read(f.hold()).unwrap();
    let mtime = f.hold().metadata().unwrap().modified().unwrap();
    let cache = tree(&f.home.path().join("cache"));
    let out = f.rift(&["doctor"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("capture recovery held"), "{stdout}");
    assert!(stdout.contains("held for "), "{stdout}");
    assert_eq!(std::fs::read(f.hold()).unwrap(), before);
    assert_eq!(f.hold().metadata().unwrap().modified().unwrap(), mtime);
    let after = tree(&f.home.path().join("cache"));
    assert!(
        !after
            .iter()
            .any(|p| p.components().any(|c| c.as_os_str() == "recovery")),
        "the report made no recovery directory"
    );
    assert!(cache.iter().all(|p| after.contains(p)));
}

fn recovery_dirs(home: &Path) -> Vec<PathBuf> {
    tree(&home.join("cache"))
        .into_iter()
        .filter(|p| p.file_name().is_some_and(|n| n == "recovery"))
        .collect()
}

/// The legacy agent journal in the git dir: the first run that resolves the
/// member moves what it holds into the member's journal under the cache root.
fn legacy_agent_queue(f: &Fixture) -> PathBuf {
    let dir = f.repo.path().join(".git").join("rift-agent-queue");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn capture_recovery_fix_with_nothing_owed_writes_no_plan_and_takes_no_hold() {
    let f = Fixture::initialized();
    let cache = tree(&f.home.path().join("cache"));
    let report = f.rift(&["doctor"]);
    assert!(!String::from_utf8_lossy(&report.stdout).contains("capture recovery held"));
    let after_report = tree(&f.home.path().join("cache"));
    assert!(cache.iter().all(|p| after_report.contains(p)));
    assert!(recovery_dirs(f.home.path()).is_empty());
    for _ in 0..2 {
        let out = f.rift(&["doctor", "--fix"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("capture recovery: nothing to recover"),
            "{stdout}"
        );
        assert!(recovery_dirs(f.home.path()).is_empty(), "{stdout}");
        assert!(!f.hold().exists());
    }
}

#[test]
fn capture_recovery_fix_that_refuses_still_releases_its_hold() {
    let f = Fixture::initialized();
    // One unreadable ticket: work is owed, so a plan is minted, and the
    // selection then refuses before any stage record is written.
    std::fs::write(legacy_agent_queue(&f).join("boundary-x.json"), b"{not json").unwrap();
    let out = f.rift(&["doctor", "--fix"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("capture recovery"), "{stdout}");
    assert!(!stdout.contains("nothing to recover"), "{stdout}");
    assert!(
        !f.hold().exists(),
        "the refusal released the hold: {stdout}"
    );
    assert_eq!(recovery_dirs(f.home.path()).len(), 1, "{stdout}");
}
