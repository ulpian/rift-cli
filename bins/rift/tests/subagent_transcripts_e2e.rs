//! End-to-end: a session's subagent transcripts are sealed into its checkpoint
//! shard, each under `tasks/<tool-use-id>/`, through the real post-commit hook.
//!
//! One subagent announced itself on `SubagentStop` (the claim marker), the
//! other never did and is found by listing the session's `subagents/`
//! directory; the second ran in a linked worktree, which its own hook
//! recorded. Both must land, redacted by the same gate as the main transcript.
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
    agent_hook, archive_head, git_in, json_str, run_rift_capture_in, seed_live_session_repo,
    shard_blob, shard_blob_bytes, shard_tree_names, sync_env_with_data, write_agent_transcript,
    LIVE_SESSION,
};

/// The spawn the session's own transcript records for agent A: a structured
/// `toolUseResult.agentId`, the shape a foreground subagent's result takes.
const AGENT_A: &str = "a1aaaaaaaaaaaaaaa";
/// Agent B's spawn is recorded only as text (`agentId: …`), the shape a
/// background launch's result takes.
const AGENT_B: &str = "a2bbbbbbbbbbbbbbb";
const SPAWN_A: &str = "toolu_spawnA";
const SPAWN_B: &str = "toolu_spawnB";
/// A key the built-in pack masks: its survival anywhere in the shard is a leak.
const SECRET: &str = "AKIAIOSFODNN7EXAMPLE";

/// The parent transcript: two `Agent` dispatches and their two results.
fn main_transcript() -> String {
    [
        r#"{"type":"user","uuid":"u1","timestamp":"2026-10-11T10:00:00.000Z","message":{"role":"user","content":"spawn two"}}"#.to_owned(),
        format!(r#"{{"type":"assistant","uuid":"a1","timestamp":"2026-10-11T10:00:01.000Z","message":{{"id":"m1","role":"assistant","content":[{{"type":"tool_use","id":"{SPAWN_A}","name":"Agent","input":{{"subagent_type":"Explore","prompt":"look"}}}},{{"type":"tool_use","id":"{SPAWN_B}","name":"Agent","input":{{"subagent_type":"general-purpose","prompt":"work"}}}}]}}}}"#),
        format!(r#"{{"type":"user","uuid":"u2","timestamp":"2026-10-11T10:05:00.000Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{SPAWN_A}","content":[{{"type":"text","text":"done"}}]}}]}},"toolUseResult":{{"agentId":"{AGENT_A}","status":"completed"}}}}"#),
        format!(r#"{{"type":"user","uuid":"u3","timestamp":"2026-10-11T10:05:01.000Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{SPAWN_B}","content":[{{"type":"text","text":"Async agent launched successfully.\nagentId: {AGENT_B} (internal ID)"}}]}}]}}}}"#),
    ]
    .join("\n")
        + "\n"
}

/// One subagent's own transcript, run in `cwd`, carrying a secret.
fn subagent_transcript(agent: &str, cwd: &Path, first: &str, last: &str) -> String {
    let cwd = json_str(cwd);
    [
        format!(r#"{{"type":"user","isSidechain":true,"agentId":"{agent}","uuid":"s1","timestamp":"{first}","cwd":{cwd},"message":{{"role":"user","content":"task for {agent}"}}}}"#),
        format!(r#"{{"type":"assistant","isSidechain":true,"agentId":"{agent}","uuid":"s2","timestamp":"{last}","cwd":{cwd},"message":{{"id":"m-{agent}","role":"assistant","content":[{{"type":"text","text":"found key {SECRET}"}}]}}}}"#),
    ]
    .join("\n")
        + "\n"
}

/// The fixture: the repository, its linked member, the session's transcript
/// and both subagent files where Claude Code keeps them.
struct Fixture {
    repo: PathBuf,
    linked: PathBuf,
    main: PathBuf,
    sub_a: PathBuf,
    sub_b: PathBuf,
    cache: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(tmp: &Path) -> Self {
        let repo = seed_live_session_repo(tmp);
        let linked = tmp.join("member-b");
        git_in(
            &repo,
            &["worktree", "add", "-q", linked.to_str().unwrap(), "-b", "b"],
        );
        let main = write_agent_transcript(tmp, LIVE_SESSION, &main_transcript());
        let subagents = main.with_extension("").join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let sub_a = subagents.join(format!("agent-{AGENT_A}.jsonl"));
        let sub_b = subagents.join(format!("agent-{AGENT_B}.jsonl"));
        std::fs::write(
            &sub_a,
            subagent_transcript(
                AGENT_A,
                &repo,
                "2026-10-11T10:00:02.000Z",
                "2026-10-11T10:04:59.500Z",
            ),
        )
        .unwrap();
        std::fs::write(
            &sub_b,
            subagent_transcript(
                AGENT_B,
                &linked,
                "2026-10-11T10:00:03.000Z",
                "2026-10-11T10:09:00.000Z",
            ),
        )
        .unwrap();
        Self {
            cache: tmp.join("cache"),
            data: tmp.join("data"),
            repo,
            linked,
            main,
            sub_a,
            sub_b,
        }
    }

    fn env(&self) -> [(&'static str, &str); 5] {
        sync_env_with_data(&self.cache, &self.data)
    }

    /// Agent A's `SubagentStop`, as Claude Code sends it: the parent's session
    /// id, the agent id and type, and the subagent's own transcript address.
    fn subagent_stop_a(&self) {
        let payload = format!(
            r#"{{"hook_event_name":"SubagentStop","agent_id":"{AGENT_A}","agent_type":"Explore","agent_transcript_path":{},"transcript_path":{}}}"#,
            json_str(&self.sub_a),
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

    /// Agent B's own tool call from its linked worktree: the hook that records
    /// where the subagent ran. B never sends `SubagentStop`.
    fn tool_call_b_in_linked_member(&self) {
        let payload = format!(
            r#"{{"hook_event_name":"PreToolUse","agent_id":"{AGENT_B}","agent_type":"general-purpose","tool_name":"Glob","tool_use_id":"toolu_b_glob","tool_input":{{"pattern":"*.txt"}},"cwd":{},"transcript_path":{}}}"#,
            json_str(&self.linked),
            json_str(&self.main),
        );
        agent_hook(
            &self.linked,
            "PreToolUse",
            LIVE_SESSION,
            1,
            &payload,
            &self.env(),
        );
    }

    fn task_path(&self, tool_use_id: &str, artifact: &str) -> String {
        shard_tree_names(&self.repo)
            .lines()
            .find(|l| l.ends_with(&format!("tasks/{tool_use_id}/{artifact}")))
            .unwrap_or_else(|| {
                panic!(
                    "tasks/{tool_use_id}/{artifact} is sealed: {}",
                    shard_tree_names(&self.repo)
                )
            })
            .to_owned()
    }

    fn task_metadata(&self, tool_use_id: &str) -> serde_json::Value {
        let path = self.task_path(tool_use_id, "metadata.json");
        serde_json::from_str(&shard_blob(&self.repo, &path)).unwrap()
    }
}

/// The bytes the gate seals for `file`: the same `redact_with` the main
/// transcript goes through, over the built-in pack this repository carries.
fn redacted(file: &Path) -> Vec<u8> {
    let raw = rift_core::SecretBytes::new(std::fs::read(file).unwrap());
    let packs = rift_shadow::redact::RulePackSet::builtin();
    rift_shadow::redact::redact_with(&raw, &packs)
        .unwrap()
        .redacted
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

/// Both subagents land: A through its claim, B through the listing; each
/// subtree's transcript is byte-for-byte the gate's output over the fixture,
/// and each metadata names its spawn, type, parent, timestamps and worktree.
#[test]
fn a_session_with_two_subagents_seals_both_subtrees() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Fixture::new(tmp.path());
    fx.subagent_stop_a();
    fx.tool_call_b_in_linked_member();
    archive_head(&fx.repo, &fx.env());

    for (spawn, file) in [(SPAWN_A, &fx.sub_a), (SPAWN_B, &fx.sub_b)] {
        let full = shard_blob_bytes(&fx.repo, &fx.task_path(spawn, "transcript.jsonl"));
        assert_eq!(full, redacted(file), "{spawn}: sealed bytes are the gate's");
        let compact = shard_blob(&fx.repo, &fx.task_path(spawn, "transcript.compact.jsonl"));
        assert!(!compact.contains(SECRET), "{spawn}: compact leaked");
        let meta = fx.task_metadata(spawn);
        assert_eq!(meta["tool_use_id"], spawn);
        assert_eq!(meta["parent_session"], LIVE_SESSION);
        assert_eq!(meta["spawn"], "matched");
        assert_eq!(meta["bytes"], full.len());
        assert_eq!(meta["sha256"], sha256_hex(&full));
    }
    let a = fx.task_metadata(SPAWN_A);
    assert_eq!(a["agent_id"], AGENT_A);
    assert_eq!(a["subagent_type"], "Explore");
    assert_eq!(a["started_at_ms"], 1_791_712_802_000_i64);
    assert_eq!(a["ended_at_ms"], 1_791_713_099_500_i64);
    let b = fx.task_metadata(SPAWN_B);
    assert_eq!(b["agent_id"], AGENT_B);
    assert_eq!(b["subagent_type"], "general-purpose");
    assert_eq!(b["worktree"]["linked"], true, "{b}");
    assert_eq!(b["worktree"]["name"], "member-b", "{b}");

    let all = shard_tree_names(&fx.repo);
    for line in all.lines().filter(|l| l.contains("/tasks/")) {
        let blob = shard_blob(&fx.repo, line);
        assert!(!blob.contains(SECRET), "{line} leaked the secret");
    }
    let (code, out) = run_rift_capture_in(&fx.repo, &["verify"], "", &fx.env());
    assert_eq!(code, 0, "verify green over task subtrees: {out}");
    let (code, out) = run_rift_capture_in(&fx.repo, &["checkpoint", "explain"], "", &fx.env());
    assert_eq!(code, 0, "{out}");
    assert!(
        out.contains(SPAWN_A) && out.contains(SPAWN_B) && out.contains("Explore"),
        "explain lists the subagents: {out}"
    );
}

/// A subagent whose file is gone by archive time is recorded, never dropped
/// in silence: its subtree carries metadata naming the loss and no transcript.
#[test]
fn a_claimed_subagent_whose_file_is_gone_is_recorded_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Fixture::new(tmp.path());
    fx.subagent_stop_a();
    std::fs::remove_file(&fx.sub_a).unwrap();
    archive_head(&fx.repo, &fx.env());

    let meta = fx.task_metadata(SPAWN_A);
    assert_eq!(meta["task_transcript"], "missing", "{meta}");
    assert!(
        !shard_tree_names(&fx.repo).contains(&format!("tasks/{SPAWN_A}/transcript.jsonl")),
        "a missing file seals no transcript"
    );
}
