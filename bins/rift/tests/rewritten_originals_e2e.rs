//! End-to-end: the originals a hook spooled before rewriting a session's Bash
//! results are sealed into its checkpoint shard, through the real post-commit
//! hook, and `rift verify` holds every delivered text to its original.
//!
//! The spool records are written in the hook's own grammar
//! (`rift_shadow::backend::SpoolHeader`, a body through the same gate) at the
//! hook's own path, as `spool_original` writes them; its screens and modes are
//! the unit tests'. The transcripts are then written as Claude Code writes
//! them: the session's call and its subagent's call each record the folded
//! text the hook delivered, and a third spooled call has no line anywhere.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

use std::path::{Path, PathBuf};

use rift_shadow::backend::rewrite::{
    original_record_json, render_folded, LineRange, ResultText, RewriteForm, Stream,
};
use rift_shadow::backend::SpoolHeader;

mod common;
#[allow(dead_code)] // The hooks suites share these builders; each file uses a subset.
#[path = "common/hooks_fixture.rs"]
mod hooks_fixture;
use hooks_fixture::{
    archive_head, run_rift_capture, run_rift_capture_in, seed_live_session_repo,
    seed_session_with_transcript, shard_blob, shard_blob_bytes, shard_tree_names,
    sync_env_with_data, write_agent_transcript, LIVE_SESSION,
};

const MAIN: &str = "toolu_mainBash";
const SUB: &str = "toolu_subBash";
const ORPHAN: &str = "toolu_orphanBash";
const SPAWN: &str = "toolu_spawnA";
const AGENT: &str = "a1aaaaaaaaaaaaaaa";
/// A key the built-in pack masks: its survival anywhere in the shard is a leak.
const SECRET: &str = "AKIAIOSFODNN7EXAMPLE";

/// The lines each rewrite folded: the second through the fourth of stdout.
const CUT: [LineRange; 1] = [LineRange {
    stream: Stream::Stdout,
    start: 1,
    end: 4,
}];

/// A Bash output whose folded lines carry the secret.
fn output(tag: &str) -> String {
    format!("{tag} begins\nexport AWS_KEY={SECRET}\nnoise 2\nnoise 3\n{tag} ends\n")
}

/// The text the hook delivered in place of `stdout`.
fn folded(stdout: &str) -> String {
    render_folded(ResultText { stdout, stderr: "" }, &CUT)
        .unwrap()
        .0
}

/// Spool one original as the hook does; returns the spooled body.
fn spool(common_dir: &Path, call: &str, agent: Option<&str>, stdout: &str) -> Vec<u8> {
    let raw = rift_core::SecretBytes::new(original_record_json(stdout, "", false));
    let packs = rift_shadow::redact::RulePackSet::builtin();
    let body = rift_shadow::redact::redact_with(&raw, &packs)
        .unwrap()
        .redacted;
    let header = SpoolHeader {
        tool_use_id: call.to_owned(),
        agent_id: agent.map(str::to_owned),
        window_key: "ab".repeat(32),
        family_key: "0000000000000000000000000000beef".to_owned(),
        form: RewriteForm::FoldedOutput,
        replaced: CUT.to_vec(),
        sha256: rift_shadow::sha256_hex(&[body.as_slice()]),
        bytes: u64::try_from(body.as_slice().len()).unwrap(),
    };
    let dir = common_dir
        .join("rift-family")
        .join("spool")
        .join(LIVE_SESSION);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{call}.original")), header.record(&body)).unwrap();
    body.as_slice().to_vec()
}

/// A Bash result line as Claude Code writes one, recording `stdout`.
fn result_line(call: &str, stdout: &str, sidechain: bool) -> serde_json::Value {
    let mut line = serde_json::json!({
        "type": "user", "timestamp": "2026-10-13T10:01:00.000Z",
        "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": call, "content": stdout,
        }]},
        "toolUseResult": { "stdout": stdout, "stderr": "", "interrupted": false, "isImage": false },
    });
    if sidechain {
        line["isSidechain"] = true.into();
        line["agentId"] = AGENT.into();
    }
    line
}

struct Fixture {
    repo: PathBuf,
    bodies: [Vec<u8>; 3],
    cache: PathBuf,
    data: PathBuf,
}

impl Fixture {
    /// The session as the hooks left it; `main_delivered` is what the
    /// session's transcript records for its rewritten call.
    fn new(tmp: &Path, main_delivered: &str) -> Self {
        let repo = seed_live_session_repo(tmp);
        let common = repo.join(".git");
        let bodies = [
            spool(&common, MAIN, None, &output("main")),
            spool(&common, SUB, Some(AGENT), &output("sub")),
            spool(&common, ORPHAN, None, &output("orphan")),
        ];
        let spawn = serde_json::json!({
            "type": "user", "timestamp": "2026-10-13T10:05:00.000Z",
            "message": { "role": "user", "content": [{
                "type": "tool_result", "tool_use_id": SPAWN, "content": [{ "type": "text", "text": "done" }],
            }]},
            "toolUseResult": { "agentId": AGENT, "status": "completed" },
        });
        let main = [
            r#"{"type":"user","timestamp":"2026-10-13T10:00:00.000Z","message":{"role":"user","content":"run it"}}"#.to_owned(),
            result_line(MAIN, main_delivered, false).to_string(),
            spawn.to_string(),
        ]
        .join("\n")
            + "\n";
        let main = write_agent_transcript(tmp, LIVE_SESSION, &main);
        seed_session_with_transcript(&repo, LIVE_SESSION, &main);
        let subagents = tmp
            .join("agent-transcripts")
            .join(LIVE_SESSION)
            .join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let sub = result_line(SUB, &folded(&output("sub")), true);
        std::fs::write(
            subagents.join(format!("agent-{AGENT}.jsonl")),
            format!("{sub}\n"),
        )
        .unwrap();
        Self {
            cache: tmp.join("cache"),
            data: tmp.join("data"),
            repo,
            bodies,
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

/// Each original lands beside the transcript recording its call, the orphan
/// is sealed and named, nothing leaks, the spool is kept, verify is green and
/// explain names them.
#[test]
fn spooled_originals_are_sealed_beside_their_transcripts_and_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Fixture::new(tmp.path(), &folded(&output("main")));
    archive_head(&fx.repo, &fx.env());

    let sealed = |suffix: &str| shard_blob_bytes(&fx.repo, &fx.path(suffix));
    assert_eq!(
        sealed(&format!("/tool-originals/{MAIN}.json")),
        fx.bodies[0]
    );
    assert_eq!(
        sealed(&format!("tasks/{SPAWN}/tool-originals/{SUB}.json")),
        fx.bodies[1]
    );
    assert_eq!(
        sealed(&format!("/tool-originals/{ORPHAN}.json")),
        fx.bodies[2]
    );
    let meta = fx.json("/metadata.json");
    let originals = meta["originals"].as_array().unwrap();
    let entry = |id: &str| originals.iter().find(|o| o["tool_use_id"] == id).unwrap();
    assert_eq!(entry(MAIN)["line"], 1);
    assert_eq!(entry(MAIN)["rewritten"], "folded_output");
    assert_eq!(
        entry(MAIN)["sha256"],
        rift_shadow::sha256_hex(&[&fx.bodies[0]])
    );
    assert_eq!(entry(ORPHAN)["original"], "original_line_absent");
    assert!(originals.iter().all(|o| o["tool_use_id"] != SUB), "{meta}");
    let task = fx.json(&format!("tasks/{SPAWN}/metadata.json"));
    assert_eq!(task["originals"][0]["tool_use_id"], SUB, "{task}");
    assert_eq!(task["originals"][0]["line"], 0, "{task}");

    for line in shard_tree_names(&fx.repo).lines() {
        assert!(
            !shard_blob(&fx.repo, line).contains(SECRET),
            "{line} leaked"
        );
    }
    let spool = fx.repo.join(".git/rift-family/spool").join(LIVE_SESSION);
    assert_eq!(
        std::fs::read_dir(spool).unwrap().count(),
        3,
        "the archive keeps the spool"
    );
    let (code, out) = run_rift_capture_in(&fx.repo, &["verify"], "", &fx.env());
    assert_eq!(code, 0, "verify green over originals: {out}");
    let (code, out) = run_rift_capture_in(&fx.repo, &["checkpoint", "explain"], "", &fx.env());
    assert_eq!(code, 0, "{out}");
    for named in [MAIN, SUB, ORPHAN, "original_line_absent", "folded_output"] {
        assert!(out.contains(named), "explain names {named}: {out}");
    }
}

/// A transcript recording a text that is not the fold of its original fails
/// verify, naming the original.
#[test]
fn a_delivered_text_that_is_not_the_original_s_rendering_fails_verify() {
    let tmp = tempfile::tempdir().unwrap();
    let wrong = folded(&output("main")).replace("main ends", "main ended");
    let fx = Fixture::new(tmp.path(), &wrong);
    archive_head(&fx.repo, &fx.env());
    let (code, out) = run_rift_capture(&fx.repo, &["verify"], None, &fx.env());
    assert_eq!(code, 1, "{out}");
    assert!(
        out.contains(&format!("tool-originals/{MAIN}.json")),
        "{out}"
    );
}
