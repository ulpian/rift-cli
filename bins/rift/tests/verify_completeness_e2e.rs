//! End-to-end: `rift verify` compares what the newest checkpoint sealed with
//! what the client still holds, through the real binary.
//!
//! A session with a main transcript and one subagent transcript is condensed
//! by the real post-commit hook; `rift verify --json` then reports both
//! `complete`. A line appended to the client file after the seal is an
//! append, an edited sealed line a mismatch naming its index, a deleted
//! subagent file a gone client file — and none of it moves the exit code,
//! because a completeness finding is reported, never a verdict.
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
    agent_hook, archive_head, json_str, run_rift_capture_in, seed_live_session_repo,
    seed_session_with_transcript, sync_env_with_data, write_agent_transcript, LIVE_SESSION,
};

const AGENT: &str = "a1aaaaaaaaaaaaaaa";
const SPAWN: &str = "toolu_spawnA";
/// Two keys the built-in pack masks: either one's survival in any output is a leak.
const AWS_SECRET: &str = "AKIAIOSFODNN7EXAMPLE";
const SK_SECRET: &str = "sk-abcdefghijklmnopqrstuvwxyz0123";

fn main_transcript() -> String {
    [
        r#"{"type":"user","uuid":"u1","timestamp":"2026-10-11T10:00:00.000Z","message":{"role":"user","content":"spawn one"}}"#.to_owned(),
        format!(r#"{{"type":"assistant","uuid":"a1","timestamp":"2026-10-11T10:00:01.000Z","message":{{"id":"m1","role":"assistant","content":[{{"type":"tool_use","id":"{SPAWN}","name":"Agent","input":{{"subagent_type":"Explore","prompt":"look"}}}}]}}}}"#),
        format!(r#"{{"type":"user","uuid":"u2","timestamp":"2026-10-11T10:05:00.000Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{SPAWN}","content":[{{"type":"text","text":"done"}}]}}]}},"toolUseResult":{{"agentId":"{AGENT}","status":"completed"}}}}"#),
        format!(r#"{{"type":"assistant","uuid":"a2","timestamp":"2026-10-11T10:06:00.000Z","message":{{"id":"m2","role":"assistant","content":[{{"type":"text","text":"the file holds {AWS_SECRET} and {SK_SECRET}"}}]}}}}"#),
    ]
    .join("\n")
        + "\n"
}

fn subagent_transcript() -> String {
    [
        format!(r#"{{"type":"user","isSidechain":true,"agentId":"{AGENT}","uuid":"s1","timestamp":"2026-10-11T10:00:02.000Z","message":{{"role":"user","content":"task"}}}}"#),
        format!(r#"{{"type":"assistant","isSidechain":true,"agentId":"{AGENT}","uuid":"s2","timestamp":"2026-10-11T10:04:59.500Z","message":{{"id":"m-{AGENT}","role":"assistant","content":[{{"type":"text","text":"found key {AWS_SECRET}"}}]}}}}"#),
    ]
    .join("\n")
        + "\n"
}

struct Fixture {
    repo: PathBuf,
    main: PathBuf,
    sub: PathBuf,
    cache: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(tmp: &Path) -> Self {
        let repo = seed_live_session_repo(tmp);
        let main = write_agent_transcript(tmp, LIVE_SESSION, &main_transcript());
        // The session cache names the transcript, as the hooks would have left it.
        seed_session_with_transcript(&repo, LIVE_SESSION, &main);
        let subagents = main.with_extension("").join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let sub = subagents.join(format!("agent-{AGENT}.jsonl"));
        std::fs::write(&sub, subagent_transcript()).unwrap();
        Self {
            cache: tmp.join("cache"),
            data: tmp.join("data"),
            repo,
            main,
            sub,
        }
    }

    fn env(&self) -> [(&'static str, &str); 5] {
        sync_env_with_data(&self.cache, &self.data)
    }

    /// The subagent's `SubagentStop`, as Claude Code sends it: the claim that
    /// keeps the subagent's address after its file is gone, which the
    /// listing alone could not.
    fn subagent_stop(&self) {
        let payload = format!(
            r#"{{"hook_event_name":"SubagentStop","agent_id":"{AGENT}","agent_type":"Explore","agent_transcript_path":{},"transcript_path":{}}}"#,
            json_str(&self.sub),
            json_str(&self.main),
        );
        agent_hook(
            &self.repo,
            "SubagentStop",
            LIVE_SESSION,
            1,
            &payload,
            &self.env(),
        );
    }

    /// `rift verify --json`: the exit code, the completeness object and the
    /// whole stdout (the leak assertion reads all of it).
    fn verify(&self) -> (i32, serde_json::Value, String) {
        let (code, out) = run_rift_capture_in(&self.repo, &["verify", "--json"], "", &self.env());
        // The loud run prints its repo and projection lines before the document.
        let at = out
            .find("{\n")
            .unwrap_or_else(|| panic!("no json in {out}"));
        let json = &out[at..];
        let doc: serde_json::Value =
            serde_json::from_str(json).unwrap_or_else(|e| panic!("{e}: {out}"));
        (code, doc["completeness"].clone(), out)
    }

    fn finding<'a>(doc: &'a serde_json::Value, artifact: &str) -> &'a serde_json::Value {
        doc["findings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["artifact"] == artifact)
            .unwrap_or_else(|| panic!("no finding for {artifact} in {doc}"))
    }
}

fn assert_no_leak(out: &str) {
    for needle in [AWS_SECRET, SK_SECRET, "[REDACTED"] {
        assert!(
            !out.contains(needle),
            "{needle} leaked into verify output: {out}"
        );
    }
}

/// Both transcripts complete; then an append, an edit and a deletion each get
/// their word, the exit code stays 0, and no secret reaches the output.
#[test]
fn verify_reports_the_client_s_state_against_the_sealed_transcripts() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Fixture::new(tmp.path());
    fx.subagent_stop();
    archive_head(&fx.repo, &fx.env());
    let task_artifact = format!("tasks/{SPAWN}/transcript.jsonl");

    let (code, doc, out) = fx.verify();
    assert_eq!(code, 0, "{out}");
    assert_no_leak(&out);
    assert_eq!(doc["complete"], 2, "{doc}");
    assert_eq!(doc["sources_audited"], 2, "{doc}");
    assert_eq!(
        Fixture::finding(&doc, "transcript.jsonl")["outcome"],
        "complete"
    );
    assert_eq!(
        Fixture::finding(&doc, &task_artifact)["outcome"],
        "complete"
    );
    assert_eq!(Fixture::finding(&doc, &task_artifact)["task"], SPAWN);
    assert!(
        doc["pack_versions"]
            .as_array()
            .is_some_and(|v| !v.is_empty()),
        "{doc}"
    );

    let mut appended = std::fs::read_to_string(&fx.main).unwrap();
    appended.push_str(r#"{"type":"user","uuid":"u3","message":{"role":"user","content":"more"}}"#);
    appended.push('\n');
    std::fs::write(&fx.main, &appended).unwrap();
    let (code, doc, out) = fx.verify();
    assert_eq!(code, 0, "{out}");
    assert_no_leak(&out);
    let main = Fixture::finding(&doc, "transcript.jsonl");
    assert_eq!(main["outcome"], "client_appended_after_seal", "{doc}");
    assert_eq!(main["sealed_lines"], 4);
    assert_eq!(main["client_lines"], 5);
    assert_eq!(doc["client_appended_after_seal"], 1);

    std::fs::write(&fx.main, appended.replacen("spawn one", "spawn two", 1)).unwrap();
    let (code, doc, out) = fx.verify();
    assert_eq!(code, 0, "mismatch is reported, never a verdict: {out}");
    assert_no_leak(&out);
    let main = Fixture::finding(&doc, "transcript.jsonl");
    assert_eq!(main["outcome"], "mismatch", "{doc}");
    assert_eq!(main["first_divergent_line"], 0);
    assert_eq!(doc["mismatch"], 1);

    std::fs::remove_file(&fx.sub).unwrap();
    let (code, doc, out) = fx.verify();
    assert_eq!(code, 0, "{out}");
    assert_no_leak(&out);
    assert_eq!(
        Fixture::finding(&doc, &task_artifact)["outcome"],
        "client_file_gone",
        "{doc}"
    );
    assert_eq!(doc["client_file_gone"], 1);
    assert!(
        out.contains("\"completeness\""),
        "the json carries the audit: {out}"
    );
}
