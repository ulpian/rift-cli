//! End-to-end: an Antigravity session's transcript is sealed into its
//! checkpoint shard through the real binary.
//!
//! Antigravity names the transcript `transcriptPath` (camelCase) on every hook.
//! The bridge copies that address to the session cache — and nothing else: the
//! hook path never opens the file, which this walk proves by writing the file
//! only after the hooks have returned — and the post-commit archive seals the
//! file as `transcript.jsonl` / `transcript.compact.jsonl` through the
//! redaction gate, like every other agent's transcript.
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
    agent_hook_raw, archive_head, json_str, run_rift_capture_in, seed_plain_repo, shard_blob,
    shard_blob_bytes, shard_path_of, shard_tree_names, sync_env_with_data,
};

const CONVERSATION: &str = "agy-e2e-transcript-1";
/// Two keys the built-in pack masks (an AWS access key id and an `sk-` vendor
/// key): either one's survival anywhere in the shard is a leak.
const AWS_SECRET: &str = "AKIAIOSFODNN7EXAMPLE";
const SK_SECRET: &str = "sk-abcdefghijklmnopqrstuvwxyz0123";

/// The transcript the client would have written: synthetic lines in the file
/// the hooks name, one of them carrying the two secrets.
fn transcript_lines() -> String {
    [
        r#"{"role":"user","content":"read alpha.txt"}"#.to_owned(),
        format!(
            r#"{{"role":"assistant","content":"the file holds {AWS_SECRET} and {SK_SECRET}"}}"#
        ),
        r#"{"role":"tool","name":"view_file","content":"one"}"#.to_owned(),
    ]
    .join("\n")
        + "\n"
}

/// The repository, the cache and substrate roots the binary runs under, and
/// the transcript's address where Antigravity keeps it: the conversation's
/// `brain/<conversation>/` directory, under it
/// `.system_generated/logs/transcript_full.jsonl`.
struct Fixture {
    repo: PathBuf,
    brain: PathBuf,
    transcript: PathBuf,
    cache: PathBuf,
    data: PathBuf,
}

impl Fixture {
    fn new(tmp: &Path) -> Self {
        let brain = tmp.join("brain").join(CONVERSATION);
        let transcript = brain
            .join(".system_generated")
            .join("logs")
            .join("transcript_full.jsonl");
        Self {
            repo: seed_plain_repo(tmp),
            brain,
            transcript,
            cache: tmp.join("cache"),
            data: tmp.join("data"),
        }
    }

    fn env(&self) -> [(&'static str, &str); 5] {
        sync_env_with_data(&self.cache, &self.data)
    }

    /// One hook with the field surface Antigravity records (fixture O1 of
    /// `rift-agent-antigravity/fixtures/antigravity-tool-field-surface.md`):
    /// the common fields, `transcriptPath` among them, plus `extra`.
    fn hook(&self, verb: &str, extra: &str) {
        let body = format!(
            r#"{{"artifactDirectoryPath":{},"conversationId":"{CONVERSATION}","modelName":"gemini-3.6-flash-medium","transcriptPath":{},"workspacePaths":[{}],{extra}}}"#,
            json_str(&self.brain),
            json_str(&self.transcript),
            json_str(&self.repo),
        );
        agent_hook_raw(&self.repo, "antigravity", verb, &body, &self.env());
    }

    fn pre_invocation(&self) {
        self.hook("PreInvocation", r#""invocationNum":0,"initialNumSteps":1"#);
    }

    fn view_file(&self) {
        let path = json_str(&self.repo.join("a.txt"));
        self.hook(
            "PreToolUse",
            &format!(
                r#""stepIdx":5,"toolCall":{{"args":{{"AbsolutePath":{path},"toolAction":"Viewing a.txt first time","toolSummary":"View a.txt"}},"name":"view_file"}}"#
            ),
        );
    }
}

/// The bytes the gate seals for `file`: the same `redact_with` the archive
/// runs, over the built-in pack this repository carries.
fn redacted(file: &Path) -> Vec<u8> {
    let raw = rift_core::SecretBytes::new(std::fs::read(file).unwrap());
    let packs = rift_shadow::redact::RulePackSet::builtin();
    rift_shadow::redact::redact_with(&raw, &packs)
        .unwrap()
        .redacted
        .as_slice()
        .to_vec()
}

/// The hooks carry the address; the archive seals the file; neither secret
/// survives in any artifact of the shard; `verify` accepts the shard.
#[test]
fn an_antigravity_sessions_transcript_is_sealed_into_its_checkpoint_shard() {
    let tmp = tempfile::tempdir().unwrap();
    let fx = Fixture::new(tmp.path());
    fx.pre_invocation();
    fx.view_file();
    assert!(
        !fx.transcript.exists(),
        "the hooks only carried the address: nothing opened the file"
    );
    std::fs::create_dir_all(fx.transcript.parent().unwrap()).unwrap();
    std::fs::write(&fx.transcript, transcript_lines()).unwrap();

    archive_head(&fx.repo, &fx.env());

    let names = shard_tree_names(&fx.repo);
    for artifact in ["transcript.jsonl", "transcript.compact.jsonl"] {
        assert!(
            names.lines().any(|l| l.ends_with(artifact)),
            "{artifact} is sealed in the shard: {names}"
        );
    }
    let full = shard_blob_bytes(&fx.repo, &shard_path_of(&fx.repo, "transcript.jsonl"));
    assert_eq!(
        full,
        redacted(&fx.transcript),
        "the sealed bytes are the gate's output over the file the hooks named"
    );
    for line in names.lines() {
        let blob = shard_blob(&fx.repo, line);
        assert!(!blob.contains(AWS_SECRET), "{line} leaked the AWS key");
        assert!(!blob.contains(SK_SECRET), "{line} leaked the sk- key");
    }
    let meta = shard_blob(&fx.repo, &shard_path_of(&fx.repo, "metadata.json"));
    assert!(
        meta.contains(r#""compact_transcript":"transcript.compact.jsonl""#),
        "metadata names the compact artifact: {meta}"
    );
    let (code, out) = run_rift_capture_in(&fx.repo, &["verify"], "", &fx.env());
    assert_eq!(code, 0, "verify green over the Antigravity shard: {out}");
}
