//! The completeness audit over `MemorySubstrate` shards and a temporary
//! client: every word, every `unverifiable` reason, the budget, the no-leak
//! property and the newest-shard-per-session rule.

use std::path::{Path, PathBuf};

use rift_core::SecretBytes;
use rift_recall_substrate::{CheckpointShard, Dialect, MemorySubstrate};
use rift_shadow::redact::{redact_with, RulePackSet};

use super::{audit, Budget, ClientSources, CompletenessSummary, Finding, Reason, Word};

pub(crate) const SESSION: &str = "01JCOMPLETENESS00001";
pub(crate) const AGENT: &str = "a1aaaaaaaaaaaaaaa";
pub(crate) const SPAWN: &str = "toolu_spawnA";
pub(crate) const SPILL: &str = "toolu_spillA";
/// Two keys the built-in pack masks; either one anywhere in a finding is a leak.
const AWS_SECRET: &str = "AKIAIOSFODNN7EXAMPLE";
const SK_SECRET: &str = "sk-abcdefghijklmnopqrstuvwxyz0123";

/// The client's side: a git dir whose session cache names the transcript,
/// the transcript, its session directory for subagents and spills.
pub(crate) struct Client {
    pub(crate) git_dir: PathBuf,
    pub(crate) main: PathBuf,
    pub(crate) session_dir: PathBuf,
}

impl Client {
    /// A client whose session cache names `main`; nothing written yet.
    pub(crate) fn new(root: &Path) -> Self {
        let git_dir = root.join("git");
        let transcripts = root.join("transcripts");
        std::fs::create_dir_all(&transcripts).unwrap();
        let main = transcripts.join(format!("{SESSION}.jsonl"));
        let session_dir = transcripts.join(SESSION);
        std::fs::create_dir_all(session_dir.join("subagents")).unwrap();
        std::fs::create_dir_all(session_dir.join("tool-results")).unwrap();
        Self {
            git_dir,
            main,
            session_dir,
        }
    }

    /// The session cache, as the hooks leave it: an address, never bytes.
    pub(crate) fn seed_cache(&self) {
        let sessions = self.git_dir.join("rift-sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(
            sessions.join(format!("{SESSION}.json")),
            format!(
                r#"{{"schema":1,"session_id":"{SESSION}","state":"active","agent":"claude-code","current_turn_ordinal":1,"turn_open":false,"compaction_boundary":null,"updated_at_ms":1,"native_id_map":{{}},"transcript_path":{}}}"#,
                serde_json::Value::String(self.main.display().to_string())
            ),
        )
        .unwrap();
    }

    /// The claim marker a `SubagentStop` leaves for `AGENT`, naming `path`.
    fn seed_claim(&self, path: &Path) {
        let dir = self
            .git_dir
            .join("rift-sessions")
            .join(format!("{SESSION}.subagents"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(format!("{AGENT}.json")),
            format!(
                r#"{{"schema_version":1,"agent_id":"{AGENT}","transcript_path":{}}}"#,
                serde_json::Value::String(path.display().to_string())
            ),
        )
        .unwrap();
    }

    fn subagent(&self) -> PathBuf {
        self.session_dir
            .join("subagents")
            .join(format!("agent-{AGENT}.jsonl"))
    }

    fn spill(&self) -> PathBuf {
        self.session_dir.join("tool-results").join("bspill01.txt")
    }

    pub(crate) fn sources(&self, budget: Budget) -> ClientSources {
        ClientSources {
            member_git_dirs: vec![self.git_dir.clone()],
            packs: RulePackSet::builtin(),
            budget,
        }
    }
}

/// The main transcript the client writes: it names the spill in the
/// structured field and spawns the subagent.
fn main_lines(spill: &Path) -> String {
    let spill = serde_json::Value::String(spill.display().to_string());
    [
        r#"{"type":"user","message":{"role":"user","content":"run it"}}"#.to_owned(),
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{SPILL}","content":"<persisted-output>\nOutput too large.\n"}}]}},"toolUseResult":{{"stdout":"head","persistedOutputPath":{spill}}}}}"#
        ),
        format!(
            r#"{{"type":"user","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"{SPAWN}","content":"done"}}]}},"toolUseResult":{{"agentId":"{AGENT}"}}}}"#
        ),
        format!(r#"{{"type":"assistant","message":{{"role":"assistant","content":"the key {AWS_SECRET} and {SK_SECRET}"}}}}"#),
    ]
    .join("\n")
        + "\n"
}

fn subagent_lines() -> String {
    format!(
        "{{\"type\":\"user\",\"isSidechain\":true,\"agentId\":\"{AGENT}\",\"message\":{{\"role\":\"user\",\"content\":\"task\"}}}}\n{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":\"found {AWS_SECRET}\"}}}}\n"
    )
}

fn spill_lines() -> String {
    format!("one\ntwo {SK_SECRET}\nthree\n")
}

pub(crate) fn sealed(bytes: &[u8]) -> Vec<u8> {
    redact_with(&SecretBytes::new(bytes.to_vec()), &RulePackSet::builtin())
        .unwrap()
        .redacted
        .as_slice()
        .to_vec()
}

fn versions_json() -> String {
    serde_json::to_string(&super::pack_versions(&RulePackSet::builtin()).unwrap()).unwrap()
}

fn sha256_hex(bytes: &[u8]) -> String {
    rift_shadow::sha256_hex(&[bytes])
}

/// One sealed shard of `SESSION` at `ordinal`: the main transcript, one
/// task subtree and one spill, each the gate's output over the client file.
pub(crate) fn shard(
    sub: MemorySubstrate,
    id: &str,
    ordinal: u64,
    main: &[u8],
    task: Option<&[u8]>,
    spill: Option<&[u8]>,
) -> MemorySubstrate {
    let spill_manifest = spill.map_or(String::new(), |body| {
        format!(
            r#","spills":[{{"tool_use_id":"{SPILL}","bytes":{},"sha256":"{}"}}]"#,
            body.len(),
            sha256_hex(body)
        )
    });
    let tasks = task.map_or(String::new(), |_| format!(r#","tasks":["{SPAWN}"]"#));
    let mut sub = sub
        .with_shard(CheckpointShard {
            checkpoint_id: id.to_owned(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal,
        })
        .with_artifact(
            id,
            "redaction_report.json",
            format!(
                r#"{{"rule_pack_versions":{},"input_sha256":"{}"}}"#,
                versions_json(),
                "a".repeat(64)
            )
            .as_bytes(),
        )
        .with_artifact(
            id,
            "metadata.json",
            format!(
                r#"{{"canon_version":3,"strategy":"manual-commit","merkle_root":"{}","sessions":["{SESSION}"]{tasks}{spill_manifest}}}"#,
                "9".repeat(64)
            )
            .as_bytes(),
        )
        .with_artifact(id, "transcript.jsonl", main);
    if let Some(body) = task {
        sub = sub
            .with_artifact(
                id,
                &format!("tasks/{SPAWN}/metadata.json"),
                format!(
                    r#"{{"schema_version":1,"tool_use_id":"{SPAWN}","agent_id":"{AGENT}","started_at_ms":1,"ended_at_ms":2}}"#
                )
                .as_bytes(),
            )
            .with_artifact(id, &format!("tasks/{SPAWN}/transcript.jsonl"), body);
    }
    if let Some(body) = spill {
        sub = sub.with_artifact(id, &format!("tool-results/{SPILL}.txt"), body);
    }
    sub
}

/// A client with every file written and a shard sealed over them: the
/// "nothing changed" state every scenario below edits.
fn sealed_session(root: &Path) -> (Client, MemorySubstrate) {
    let client = Client::new(root);
    client.seed_cache();
    std::fs::write(&client.main, main_lines(&client.spill())).unwrap();
    std::fs::write(client.subagent(), subagent_lines()).unwrap();
    std::fs::write(client.spill(), spill_lines()).unwrap();
    let sub = shard(
        MemorySubstrate::local(),
        "cp-1",
        1,
        &sealed(&std::fs::read(&client.main).unwrap()),
        Some(&sealed(&std::fs::read(client.subagent()).unwrap())),
        Some(&sealed(&std::fs::read(client.spill()).unwrap())),
    );
    (client, sub)
}

fn words(summary: &CompletenessSummary) -> Vec<(String, Word, Option<Reason>)> {
    summary
        .findings
        .iter()
        .map(|f| (f.artifact.clone(), f.word, f.reason))
        .collect()
}

fn finding<'a>(summary: &'a CompletenessSummary, artifact: &str) -> &'a Finding {
    summary
        .findings
        .iter()
        .find(|f| f.artifact == artifact)
        .unwrap_or_else(|| panic!("no finding for {artifact}: {:?}", words(summary)))
}

#[test]
fn an_unchanged_client_reads_complete_for_the_transcript_every_task_and_every_spill() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, sub) = sealed_session(tmp.path());
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    assert_eq!(summary.sources_audited, 3, "{:?}", words(&summary));
    assert_eq!(summary.count(Word::Complete), 3, "{:?}", words(&summary));
    assert_eq!(summary.shards_skipped_older, 0);
    assert_eq!(
        summary.pack_versions,
        super::pack_versions(&RulePackSet::builtin()).unwrap()
    );
    let main = finding(&summary, "transcript.jsonl");
    assert_eq!(main.session.as_deref(), Some(SESSION));
    assert_eq!(main.sealed_lines, 4);
    assert_eq!(main.client_lines, 4);
    assert_eq!(main.first_divergent_line, None);
    assert_eq!(
        finding(&summary, &format!("tasks/{SPAWN}/transcript.jsonl"))
            .task
            .as_deref(),
        Some(SPAWN)
    );
}

#[test]
fn a_line_the_client_appended_after_the_seal_is_an_append_not_a_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, sub) = sealed_session(tmp.path());
    let mut bytes = std::fs::read(&client.main).unwrap();
    bytes.extend_from_slice(br#"{"type":"user","message":{"role":"user","content":"more"}}"#);
    bytes.push(b'\n');
    std::fs::write(&client.main, bytes).unwrap();
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    let main = finding(&summary, "transcript.jsonl");
    assert_eq!(main.word, Word::ClientAppendedAfterSeal);
    assert_eq!((main.sealed_lines, main.client_lines), (4, 5));
    assert_eq!(main.first_divergent_line, None);
    assert_eq!(summary.count(Word::Complete), 2);
}

#[test]
fn an_edited_sealed_line_is_a_mismatch_naming_the_first_divergent_index() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, sub) = sealed_session(tmp.path());
    let text = std::fs::read_to_string(&client.main).unwrap();
    let edited = text.replacen("run it", "run it again", 1);
    std::fs::write(&client.main, edited).unwrap();
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    let main = finding(&summary, "transcript.jsonl");
    assert_eq!(main.word, Word::Mismatch);
    assert_eq!(main.first_divergent_line, Some(0));
    assert_eq!((main.sealed_lines, main.client_lines), (4, 4));
}

#[test]
fn a_client_file_that_is_gone_is_said_so_for_a_claimed_task_and_for_the_main() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, sub) = sealed_session(tmp.path());
    // The claim keeps the address after the listing can no longer find it.
    client.seed_claim(&client.subagent());
    std::fs::remove_file(client.subagent()).unwrap();
    std::fs::remove_file(&client.main).unwrap();
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    assert_eq!(
        summary.count(Word::ClientFileGone),
        2,
        "{:?}",
        words(&summary)
    );
    assert_eq!(
        finding(&summary, &format!("tasks/{SPAWN}/transcript.jsonl")).word,
        Word::ClientFileGone
    );
    assert_eq!(
        finding(&summary, "transcript.jsonl").word,
        Word::ClientFileGone
    );
    // The spill's address lived in the gone transcript: no address, not gone.
    let spill = finding(&summary, &format!("tool-results/{SPILL}.txt"));
    assert_eq!(
        (spill.word, spill.reason),
        (Word::Unverifiable, Some(Reason::NoAddress))
    );
}

#[test]
fn no_session_cache_and_no_subagent_file_or_claim_are_no_address() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, sub) = sealed_session(tmp.path());
    std::fs::remove_file(
        client
            .git_dir
            .join("rift-sessions")
            .join(format!("{SESSION}.json")),
    )
    .unwrap();
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    assert_eq!(
        summary.count(Word::Unverifiable),
        3,
        "{:?}",
        words(&summary)
    );
    assert_eq!(summary.reason_count(Reason::NoAddress), 3);

    // The cache back, the subagent file gone and never claimed: the task alone.
    client.seed_cache();
    std::fs::remove_file(client.subagent()).unwrap();
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    let task = finding(&summary, &format!("tasks/{SPAWN}/transcript.jsonl"));
    assert_eq!(
        (task.word, task.reason),
        (Word::Unverifiable, Some(Reason::NoAddress))
    );
    assert_eq!(summary.count(Word::Complete), 2);
}

#[test]
fn a_shard_sealed_with_other_pack_versions_is_unverifiable_and_names_both_lists() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, _) = sealed_session(tmp.path());
    let sub = shard(
        MemorySubstrate::local(),
        "cp-old-packs",
        1,
        &sealed(&std::fs::read(&client.main).unwrap()),
        None,
        None,
    )
    .with_artifact(
        "cp-old-packs",
        "redaction_report.json",
        format!(
            r#"{{"rule_pack_versions":["builtin@0.0.0"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        )
        .as_bytes(),
    );
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    let main = finding(&summary, "transcript.jsonl");
    assert_eq!(
        (main.word, main.reason),
        (Word::Unverifiable, Some(Reason::PacksChanged))
    );
    let json = summary.to_json();
    assert_eq!(json["findings"][0]["reason"], "packs_changed");
    assert_eq!(
        json["findings"][0]["sealed_pack_versions"][0],
        "builtin@0.0.0"
    );
    assert_eq!(
        json["pack_versions"],
        serde_json::json!(summary.pack_versions)
    );
}

#[test]
fn a_budget_of_one_source_leaves_the_second_over_budget_and_still_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, sub) = sealed_session(tmp.path());
    let budget = Budget {
        sources: 1,
        ..Budget::default()
    };
    let summary = audit(&sub, &client.sources(budget)).unwrap();
    assert_eq!(summary.sources_audited, 3, "every source is counted");
    assert_eq!(summary.count(Word::Complete), 1);
    assert_eq!(summary.reason_count(Reason::OverBudget), 2);

    // The byte budget too: a file past the per-file bound is never read.
    let budget = Budget {
        per_file: 8,
        ..Budget::default()
    };
    let summary = audit(&sub, &client.sources(budget)).unwrap();
    assert_eq!(
        summary.reason_count(Reason::OverBudget),
        2,
        "{:?}",
        words(&summary)
    );
    // …and a transcript never read names no spill address.
    assert_eq!(summary.reason_count(Reason::NoAddress), 1);
    let budget = Budget {
        total_bytes: std::fs::read(&client.main).unwrap().len() as u64,
        ..Budget::default()
    };
    let summary = audit(&sub, &client.sources(budget)).unwrap();
    assert_eq!(summary.count(Word::Complete), 1, "{:?}", words(&summary));
    assert_eq!(summary.reason_count(Reason::OverBudget), 2);
}

#[test]
fn an_address_that_cannot_be_read_as_a_file_is_unreadable() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, sub) = sealed_session(tmp.path());
    std::fs::remove_file(client.subagent()).unwrap();
    std::fs::create_dir(client.subagent()).unwrap();
    client.seed_claim(&client.subagent());
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    let task = finding(&summary, &format!("tasks/{SPAWN}/transcript.jsonl"));
    assert_eq!(
        (task.word, task.reason),
        (Word::Unverifiable, Some(Reason::Unreadable))
    );
}

#[test]
fn an_edited_spill_is_a_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, sub) = sealed_session(tmp.path());
    std::fs::write(client.spill(), "one\nchanged\nthree\n").unwrap();
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    let spill = finding(&summary, &format!("tool-results/{SPILL}.txt"));
    assert_eq!(spill.word, Word::Mismatch);
    assert_eq!(spill.first_divergent_line, Some(1));
    // A spill has no appends: a longer client body is a mismatch too.
    std::fs::write(client.spill(), format!("{}four\n", spill_lines())).unwrap();
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    assert_eq!(
        finding(&summary, &format!("tool-results/{SPILL}.txt")).word,
        Word::Mismatch
    );
}

#[test]
fn only_the_newest_shard_naming_a_session_is_audited_and_older_ones_are_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, _) = sealed_session(tmp.path());
    let whole = std::fs::read(&client.main).unwrap();
    let older: Vec<u8> = whole
        .split_inclusive(|b| *b == b'\n')
        .take(2)
        .flatten()
        .copied()
        .collect();
    // The older shard sealed a prefix (an older archive); the newer the whole.
    let sub = shard(
        MemorySubstrate::local(),
        "cp-older",
        1,
        &sealed(&older),
        None,
        None,
    );
    let sub = shard(sub, "cp-newer", 2, &sealed(&whole), None, None);
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    assert_eq!(summary.findings.len(), 1, "{:?}", words(&summary));
    assert_eq!(summary.findings[0].checkpoint, "cp-newer");
    assert_eq!(summary.findings[0].word, Word::Complete);
    assert_eq!(summary.shards_skipped_older, 1);
}

#[test]
fn no_finding_carries_a_byte_of_the_client_file_raw_or_redacted() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, sub) = sealed_session(tmp.path());
    let text = std::fs::read_to_string(&client.main).unwrap();
    std::fs::write(&client.main, text.replacen("the key", "a key", 1)).unwrap();
    std::fs::write(client.spill(), "one\nedited\n").unwrap();
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    assert_eq!(summary.count(Word::Mismatch), 2, "{:?}", words(&summary));
    let rendered = format!("{}\n{}\n{:?}", summary.to_json(), summary.human(), summary);
    for needle in [
        AWS_SECRET,
        SK_SECRET,
        "[REDACTED",
        "the key",
        "edited",
        "transcripts/",
    ] {
        assert!(
            !rendered.contains(needle),
            "{needle} leaked into {rendered}"
        );
    }
}

/// The chain is the family's, a session cache the capturing member's: a
/// verify run from a member whose own run state never saw the session finds
/// the address in the member that did.
#[test]
fn a_session_captured_by_another_member_is_found_through_that_member_s_run_state() {
    let tmp = tempfile::tempdir().unwrap();
    let (client, sub) = sealed_session(tmp.path());
    client.seed_claim(&client.subagent());
    std::fs::remove_file(client.subagent()).unwrap();
    let empty_member = tmp.path().join("other-git");
    std::fs::create_dir_all(&empty_member).unwrap();
    let mut sources = client.sources(Budget::default());
    sources.member_git_dirs = vec![empty_member, client.git_dir.clone()];
    let summary = audit(&sub, &sources).unwrap();
    assert_eq!(summary.count(Word::Complete), 2, "{:?}", words(&summary));
    assert_eq!(
        finding(&summary, &format!("tasks/{SPAWN}/transcript.jsonl")).word,
        Word::ClientFileGone,
        "the claim is found in the capturing member too"
    );
    // The verifying member alone, which never captured it: no address.
    let summary = audit(&sub, &client.sources(Budget::default())).unwrap();
    assert_eq!(summary.count(Word::Complete), 2);
    sources.member_git_dirs.truncate(1);
    let summary = audit(&sub, &sources).unwrap();
    assert_eq!(
        summary.reason_count(Reason::NoAddress),
        3,
        "{:?}",
        words(&summary)
    );
}

#[test]
fn the_vocabulary_is_closed_and_each_word_spells_itself_once() {
    let words: Vec<&str> = Word::ALL.iter().map(|w| w.as_str()).collect();
    assert_eq!(
        words,
        [
            "complete",
            "client_file_gone",
            "client_appended_after_seal",
            "mismatch",
            "unverifiable"
        ]
    );
    let reasons: Vec<&str> = Reason::ALL.iter().map(|r| r.as_str()).collect();
    assert_eq!(
        reasons,
        ["no_address", "packs_changed", "over_budget", "unreadable"]
    );
    let empty = CompletenessSummary::empty();
    assert_eq!(
        empty.human(),
        "0 complete, 0 appended after seal, 0 client file gone, 0 mismatch, 0 unverifiable \
         (no_address 0, packs_changed 0, over_budget 0, unreadable 0)"
    );
}
