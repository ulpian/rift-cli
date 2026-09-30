//! End-to-end of integration health on the command line: `rift status`,
//! `rift recall status` and `rift doctor` print the same finding in the same
//! words and carry the same `--json` array, a healthy repository says so on
//! each, and no command, `--fix` included, writes a byte of what the finding
//! was read from.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;

mod common;

/// A home and a repository under it, both temporary: the process's `HOME`,
/// Codex's home, Rift's config and cache all point inside `home`, so no read
/// reaches the developer's own agents.
struct Fixture {
    home: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl Fixture {
    /// A repository set up on this machine with Claude Code, and only Claude
    /// Code, wired to the binary under test.
    fn wired() -> Self {
        let fixture = Self {
            home: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
        };
        rift_git::test_support::fake_repo(fixture.repo.path()).unwrap();
        assert_eq!(
            fixture.rift(&["init", "--no-recall"]).status.code(),
            Some(0)
        );
        let enable = fixture.rift(&["enable", "--agent", "claude-code"]);
        assert_eq!(enable.status.code(), Some(0), "{enable:?}");
        // `init` wires whatever agents the inherited `PATH` shows; the two
        // others are taken back out so the fixture does not depend on the
        // machine it runs on.
        for agent in ["codex", "antigravity"] {
            let disable = fixture.rift(&["disable", "--agent", agent]);
            assert_eq!(disable.status.code(), Some(0), "{disable:?}");
        }
        fixture
    }

    /// `rift <args>`, hermetic.
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

    /// Stdout of a command that must exit 0.
    fn stdout(&self, args: &[&str]) -> String {
        let out = self.rift(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    }

    /// The `--json` document of a command that must exit 0.
    fn json(&self, args: &[&str]) -> serde_json::Value {
        serde_json::from_str(&self.stdout(args)).unwrap()
    }

    /// Claude Code's hook file.
    fn hook_file(&self) -> PathBuf {
        self.repo.path().join(".claude/settings.local.json")
    }

    /// Point Claude Code's hooks at a binary that does not exist.
    fn break_the_hook_binary(&self) -> String {
        let missing = self.home.path().join("gone").join("rift");
        let missing = common::shell_spelling(&missing);
        let text = std::fs::read_to_string(self.hook_file()).unwrap();
        let spelled = common::shell_spelling(Path::new(common::rift_bin()));
        assert!(text.contains(&spelled), "{text}");
        std::fs::write(self.hook_file(), text.replace(&spelled, &missing)).unwrap();
        missing
    }

    /// Every file the health read takes as input, with its bytes: the hook
    /// files, the binary of record and Codex's home.
    fn inputs(&self) -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = BTreeMap::new();
        for dir in [
            self.repo.path().join(".claude"),
            self.home.path().to_path_buf(),
        ] {
            collect(&dir, &mut files);
        }
        files.retain(|path, _| {
            path.starts_with(self.repo.path().join(".claude"))
                || path.starts_with(self.home.path().join("codex-home"))
                || path.file_name().is_some_and(|name| name == "binary.json")
        });
        files
    }
}

/// Every regular file under `dir`, with its bytes.
fn collect(dir: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
        } else if let Ok(bytes) = std::fs::read(&path) {
            files.insert(path, bytes);
        }
    }
}

/// The sentence printed under `headline` in `text`.
fn sentence_under(text: &str, headline: &str) -> String {
    let mut lines = text.lines();
    lines
        .by_ref()
        .find(|line| line.trim_end().ends_with(headline))
        .unwrap_or_else(|| panic!("no {headline:?} in {text}"));
    lines.next().unwrap().trim().to_owned()
}

#[test]
fn a_missing_hook_binary_reads_the_same_in_status_and_doctor() {
    let fixture = Fixture::wired();
    let missing = fixture.break_the_hook_binary();

    let status = fixture.stdout(&["status"]);
    let doctor = fixture.stdout(&["doctor"]);
    let headline = "Claude Code: hook binary missing";
    let sentence = sentence_under(&status, headline);
    assert_eq!(sentence, sentence_under(&doctor, headline));
    assert!(
        sentence.contains("`rift enable --agent claude-code`"),
        "{sentence}"
    );

    let status = fixture.json(&["status", "--json"]);
    let doctor = fixture.json(&["doctor", "--json"]);
    assert_eq!(status["health"], doctor["integration_health"]);
    let health = status["health"].as_array().unwrap();
    let element = health
        .iter()
        .find(|element| element["reason"] == "hook-binary-missing")
        .unwrap_or_else(|| panic!("{health:?}"));
    assert_eq!(element["agent"], "claude-code");
    assert_eq!(element["severity"], "error");
    assert_eq!(element["sentence"], sentence.as_str());
    assert_eq!(element["detail"], missing.as_str());
    for key in ["checkpoints", "sessions", "captured_events"] {
        assert!(status[key].is_i64(), "{key} kept: {status}");
    }
}

#[test]
fn a_healthy_repository_prints_none_and_an_empty_array() {
    let fixture = Fixture::wired();
    for args in [["status"], ["doctor"]] {
        let text = fixture.stdout(&args);
        assert!(
            text.lines()
                .any(|line| line == "  integration health: none"),
            "{args:?}: {text}"
        );
    }
    let status = fixture.json(&["status", "--json"]);
    let doctor = fixture.json(&["doctor", "--json"]);
    assert_eq!(status["health"], serde_json::json!([]));
    assert_eq!(doctor["integration_health"], serde_json::json!([]));
}

#[test]
fn recall_status_and_doctor_print_the_same_health_sentence_and_array() {
    let fixture = Fixture::wired();
    fixture.break_the_hook_binary();

    let recall = fixture.stdout(&["recall", "status"]);
    let doctor = fixture.stdout(&["doctor"]);
    let headline = "Claude Code: hook binary missing";
    assert_eq!(
        sentence_under(&recall, headline),
        sentence_under(&doctor, headline)
    );

    let recall = fixture.json(&["recall", "status", "--json"]);
    let doctor = fixture.json(&["doctor", "--json"]);
    assert_eq!(recall["health"], doctor["integration_health"]);
    assert!(
        recall["health"]
            .as_array()
            .is_some_and(|health| !health.is_empty()),
        "{recall}"
    );
}

#[test]
fn a_healthy_repository_reads_none_on_recall_status_between_support_and_policy() {
    let fixture = Fixture::wired();
    let text = fixture.stdout(&["recall", "status"]);
    let line = |needle: &str| {
        text.lines()
            .position(|line| line.starts_with(needle))
            .unwrap_or_else(|| panic!("no {needle:?} in {text}"))
    };
    let health = line("  integration health: none");
    assert!(line("Support truth") < health, "{text}");
    assert!(health < line("Serving policy"), "{text}");
    let json = fixture.json(&["recall", "status", "--json"]);
    assert_eq!(json["health"], serde_json::json!([]));
}

#[test]
fn reading_health_writes_none_of_its_inputs_even_under_fix() {
    let fixture = Fixture::wired();
    fixture.break_the_hook_binary();
    let before = fixture.inputs();
    assert!(
        before.contains_key(&fixture.hook_file()),
        "{:?}",
        before.keys()
    );
    for args in [
        &["status", "--json"][..],
        &["recall", "status", "--json"][..],
        &["doctor", "--json"][..],
        &["doctor", "--fix", "--json"][..],
    ] {
        let out = fixture.rift(args);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        assert_eq!(fixture.inputs(), before, "{args:?} wrote an input");
    }
}
