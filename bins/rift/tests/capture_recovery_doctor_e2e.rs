//! End-to-end of the doctor's capture-recovery leg through the real binary on
//! a fixture repository: report-only, it names a held recovery and its age,
//! and leaves the hold and the cache exactly as it found them.
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
