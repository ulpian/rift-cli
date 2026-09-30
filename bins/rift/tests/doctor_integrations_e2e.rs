//! End-to-end of the doctor's integrations leg: it renders the registry view
//! `rift recall status` renders, nine adapters in registry order, counts what
//! is wired, and says nothing more about an adapter that is merely not wired.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::process::Output;

mod common;

/// The agents the fixture wires; every other adapter stays unwired.
const WIRED: [&str; 3] = ["claude-code", "codex", "antigravity"];

/// A home and a repository under it, both temporary, so no read reaches the
/// developer's own agents.
struct Fixture {
    home: tempfile::TempDir,
    repo: tempfile::TempDir,
}

impl Fixture {
    /// A repository set up on this machine with exactly [`WIRED`] wired.
    fn three_wired() -> Self {
        let fixture = Self {
            home: tempfile::tempdir().unwrap(),
            repo: tempfile::tempdir().unwrap(),
        };
        rift_git::test_support::fake_repo(fixture.repo.path()).unwrap();
        assert_eq!(
            fixture.rift(&["init", "--no-recall"]).status.code(),
            Some(0)
        );
        for agent in WIRED {
            let enable = fixture.rift(&["enable", "--agent", agent]);
            assert_eq!(enable.status.code(), Some(0), "{enable:?}");
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
}

/// The doctor's integrations leg: its header and every line under it, up to
/// the next memory-chain line.
fn integrations_leg(doctor: &str) -> Vec<&str> {
    let mut lines = doctor
        .lines()
        .skip_while(|line| !line.contains("integrations:"));
    let header = lines.next().unwrap_or_else(|| panic!("no leg in {doctor}"));
    let body = lines.take_while(|line| line.starts_with("      "));
    std::iter::once(header).chain(body).collect()
}

#[test]
fn the_doctor_leg_lists_the_nine_adapters_in_registry_order_without_probe_or_connection() {
    let fixture = Fixture::three_wired();
    let doctor = fixture.stdout(&["doctor"]);
    let leg = integrations_leg(&doctor);
    let recall = fixture.json(&["recall", "status", "--json"]);
    let names: Vec<&str> = recall["integrations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["display_name"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 9, "{recall}");
    let rows: Vec<&str> = leg
        .iter()
        .filter_map(|line| {
            names
                .iter()
                .copied()
                .find(|name| line.trim_start().starts_with(&format!("{name} ")))
        })
        .collect();
    assert_eq!(rows, names, "{doctor}");
    for token in ["probe", "connection", "never_seen"] {
        assert!(leg.iter().all(|line| !line.contains(token)), "{doctor}");
    }
}

#[test]
fn three_wired_adapters_read_three_of_nine_with_no_line_for_the_other_six() {
    let fixture = Fixture::three_wired();
    let doctor = fixture.stdout(&["doctor"]);
    let leg = integrations_leg(&doctor);
    assert_eq!(leg[0].trim(), "integrations:   3/9 wired", "{doctor}");
    let trust_lines = leg
        .iter()
        .filter(|line| line.trim_start().starts_with("codex trust:"))
        .count();
    // The header, the table's heading, a row per adapter, and Codex's trust.
    assert_eq!(leg.len(), 2 + 9 + trust_lines, "{doctor}");
    assert!(leg.iter().all(|line| !line.contains("action:")), "{doctor}");
}

#[test]
fn the_doctor_json_carries_the_recall_status_rows_and_no_clients() {
    let fixture = Fixture::three_wired();
    let doctor = fixture.json(&["doctor", "--json"]);
    let recall = fixture.json(&["recall", "status", "--json"]);
    let chain = &doctor["memory_chain"];
    assert!(chain.get("clients").is_none(), "{chain}");
    assert_eq!(chain["integrations"], recall["integrations"]);
}
