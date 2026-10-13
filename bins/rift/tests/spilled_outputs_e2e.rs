//! End-to-end: the tool outputs a session's transcripts say were spilled to a
//! file of their own are sealed into its checkpoint shard, through the real
//! post-commit hook.
//!
//! The session's transcript names three spills in the client's structured
//! field: one there (60 K characters, carrying a secret), one already gone,
//! and one whose address escapes the session directory. Its one subagent names
//! a fourth in the envelope-only form a subagent's file records. The first
//! and the fourth must land beside their transcripts, redacted by the same
//! gate; the other two must be recorded, never dropped.
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
    archive_head, run_rift_capture_in, seed_live_session_repo, seed_session_with_transcript,
    shard_blob, shard_blob_bytes, shard_tree_names, sync_env_with_data, write_agent_transcript,
    LIVE_SESSION,
};

const BIG: &str = "toolu_spillBig";
const GONE: &str = "toolu_spillGone";
const ESCAPE: &str = "toolu_spillEscape";
const SPAWN: &str = "toolu_spawnA";
const AGENT: &str = "a1aaaaaaaaaaaaaaa";
const INNER: &str = "toolu_innerRead";
/// A key the built-in pack masks: its survival anywhere in the shard is a leak.
const SECRET: &str = "AKIAIOSFODNN7EXAMPLE";

/// A result line naming its spill in the structured field.
fn structured(call: &str, address: &Path) -> String {
    serde_json::json!({
        "type": "user", "timestamp": "2026-10-11T10:01:00.000Z",
        "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": call,
            "content": "<persisted-output>\nOutput too large.\n",
        }]},
        "toolUseResult": { "stdout": "head", "persistedOutputPath": address.display().to_string() },
    })
    .to_string()
}

/// 60 K characters with the secret on one line.
fn big_output() -> String {
    let mut out = String::new();
    for i in 0..1_000 {
        out.push_str(&format!("{i:05} {}\n", "y".repeat(53)));
    }
    out.push_str(&format!("aws_access_key_id = {SECRET}\n"));
    out
}

struct Fixture {
    repo: PathBuf,
    big: PathBuf,
    inner: PathBuf,
    cache: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(tmp: &Path) -> Self {
        let repo = seed_live_session_repo(tmp);
        let session_dir = tmp.join("agent-transcripts").join(LIVE_SESSION);
        let results = session_dir.join("tool-results");
        std::fs::create_dir_all(&results).unwrap();
        let big = results.join("bbig01.txt");
        std::fs::write(&big, big_output()).unwrap();
        let inner = results.join("binner01.txt");
        std::fs::write(&inner, format!("inner output {SECRET}\n")).unwrap();
        let outside = tmp.join("outside.txt");
        std::fs::write(&outside, "never sealed\n").unwrap();
        let spawn = serde_json::json!({
            "type": "user", "timestamp": "2026-10-11T10:05:00.000Z",
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": SPAWN, "content": [{ "type": "text", "text": "done" }],
            }]},
            "toolUseResult": { "agentId": AGENT, "status": "completed" },
        });
        let main = [
            r#"{"type":"user","timestamp":"2026-10-11T10:00:00.000Z","message":{"role":"user","content":"run it"}}"#.to_owned(),
            structured(BIG, &big),
            structured(GONE, &results.join("bgone01.txt")),
            structured(ESCAPE, &results.join("..").join("..").join("..").join("outside.txt")),
            spawn.to_string(),
        ]
        .join("\n")
            + "\n";
        let main = write_agent_transcript(tmp, LIVE_SESSION, &main);
        seed_session_with_transcript(&repo, LIVE_SESSION, &main);
        let subagents = session_dir.join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let envelope = format!(
            "<persisted-output>\nOutput too large (34.8KB). Full output saved to: {}\n\nPreview (first 2KB):\ninner",
            inner.display()
        );
        let sub = serde_json::json!({
            "type": "user", "isSidechain": true, "agentId": AGENT, "timestamp": "2026-10-11T10:02:00.000Z",
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": INNER, "content": [{ "type": "text", "text": envelope }],
            }]},
        });
        std::fs::write(
            subagents.join(format!("agent-{AGENT}.jsonl")),
            format!("{sub}\n"),
        )
        .unwrap();
        Self {
            cache: tmp.join("cache"),
            data: tmp.join("data"),
            repo,
            big,
            inner,
        }
    }

    fn env(&self) -> [(&'static str, &str); 5] {
        sync_env_with_data(&self.cache, &self.data)
    }

    /// The shard path ending in `suffix`.
    fn path(&self, suffix: &str) -> String {
        shard_tree_names(&self.repo)
            .lines()
            .find(|l| {
                l.ends_with(suffix) && (suffix.starts_with("tasks/") || !l.contains("/tasks/"))
            })
            .unwrap_or_else(|| panic!("{suffix} is sealed: {}", shard_tree_names(&self.repo)))
            .to_owned()
    }

    fn json(&self, suffix: &str) -> serde_json::Value {
        serde_json::from_str(&shard_blob(&self.repo, &self.path(suffix))).unwrap()
    }
}

/// The bytes the gate seals for `file`.
fn redacted(file: &Path) -> Vec<u8> {
    let raw = rift_core::SecretBytes::new(std::fs::read(file).unwrap());
    rift_shadow::sealed_spill(&raw, &rift_shadow::redact::RulePackSet::builtin())
        .unwrap()
        .as_slice()
        .to_vec()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(bytes)
        .iter()
        .fold(String::new(), |mut hex, b| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{b:02x}");
            hex
        })
}

/// Every spill lands where it belongs or is recorded; nothing leaks; verify
/// is green and explain names them.
#[test]
fn a_session_s_spills_are_sealed_beside_their_transcripts() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Fixture::new(tmp.path());
    archive_head(&fx.repo, &fx.env());

    let big = shard_blob_bytes(&fx.repo, &fx.path(&format!("/tool-results/{BIG}.txt")));
    assert_eq!(big, redacted(&fx.big), "sealed bytes are the gate's");
    assert!(big.len() >= 60_000, "all of it: {}", big.len());
    let meta = fx.json("/metadata.json");
    let spills = meta["spills"].as_array().unwrap();
    let entry = |id: &str| spills.iter().find(|s| s["tool_use_id"] == id).unwrap();
    assert_eq!(entry(BIG)["sha256"], sha256_hex(&big));
    assert_eq!(entry(BIG)["bytes"], big.len());
    assert_eq!(entry(GONE)["spill"], "spill_missing");
    assert_eq!(entry(ESCAPE)["spill"], "spill_refused");

    let inner_path = fx.path(&format!("tasks/{SPAWN}/tool-results/{INNER}.txt"));
    assert_eq!(shard_blob_bytes(&fx.repo, &inner_path), redacted(&fx.inner));
    let task = fx.json(&format!("tasks/{SPAWN}/metadata.json"));
    assert_eq!(task["spills"][0]["tool_use_id"], INNER, "{task}");

    for line in shard_tree_names(&fx.repo).lines() {
        assert!(
            !shard_blob(&fx.repo, line).contains(SECRET),
            "{line} leaked"
        );
    }
    let (code, out) = run_rift_capture_in(&fx.repo, &["verify"], "", &fx.env());
    assert_eq!(code, 0, "verify green over spills: {out}");
    let (code, out) = run_rift_capture_in(&fx.repo, &["checkpoint", "explain"], "", &fx.env());
    assert_eq!(code, 0, "{out}");
    for named in [BIG, GONE, INNER, "spill_missing", "spill_refused"] {
        assert!(out.contains(named), "explain names {named}: {out}");
    }
}
