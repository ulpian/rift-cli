use rift_core::RepoId;
use rift_shadow::backend::rewrite::{render_folded, Stream};

use super::super::super::originals::{spool_untimed, RewriteSite, SpoolReceipt, ToolOriginal};
use super::*;

const SESSION: &str = "sess-arch";
const MAIN_CALL: &str = "toolu_mainBash";
const SUB_CALL: &str = "toolu_subBash";
const ORPHAN_CALL: &str = "toolu_orphan";
const AGENT: &str = "a1aaaaaaaaaaaaaaa";

const FOLD: [LineRange; 1] = [LineRange {
    stream: Stream::Stdout,
    start: 1,
    end: 3,
}];

struct Repo {
    _tmp: tempfile::TempDir,
    paths: rift_git::RepoPaths,
    family: RepoId,
}

fn repo() -> Repo {
    let tmp = tempfile::tempdir().unwrap();
    let git = tmp.path().join(".git");
    std::fs::create_dir_all(&git).unwrap();
    Repo {
        paths: rift_git::RepoPaths {
            git_dir: git.clone(),
            common_dir: git,
            work_tree: tmp.path().to_path_buf(),
            worktree_hash: [0; 6],
        },
        _tmp: tmp,
        family: RepoId::new("0000000000000000000000000000beef").unwrap(),
    }
}

fn spool(repo: &Repo, call: &str, agent: Option<&str>, stdout: &str) -> SpoolReceipt {
    let window = "cd".repeat(32);
    let site = RewriteSite {
        repo: &repo.paths,
        session_id: SESSION,
        agent_id: agent,
        window_key: &window,
        family_key: &repo.family,
        tool_use_id: call,
        form: RewriteForm::FoldedOutput,
        replaced: &FOLD,
    };
    let original = ToolOriginal {
        stdout: SecretBytes::new(stdout.as_bytes().to_vec()),
        stderr: SecretBytes::new(Vec::new()),
        interrupted: false,
    };
    spool_untimed(&site, &original).unwrap()
}

/// The result line Claude Code writes for a Bash call, recording `stdout`.
fn result_line(call: &str, stdout: &str) -> String {
    serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": [{
            "type": "tool_result", "tool_use_id": call, "content": stdout,
        }]},
        "toolUseResult": { "stdout": stdout, "stderr": "", "interrupted": false, "isImage": false },
    })
    .to_string()
}

/// The folded text a hook delivers for `stdout`.
fn folded(stdout: &str) -> String {
    let text = ResultText { stdout, stderr: "" };
    let fold_line = |n: u32| format!("[rift fold: {n} lines elided]");
    render_folded(text, &FOLD, fold_line).unwrap().0
}

fn sealed_pair(raw: &str) -> (Vec<u8>, ArchivedTranscript) {
    let packs = RulePackSet::builtin();
    let bytes = raw.as_bytes().to_vec();
    let (pair, _) =
        rift_shadow::archived_transcript(&SecretBytes::new(bytes.clone()), 0, &packs).unwrap();
    (bytes, pair)
}

fn task(transcript: ArchivedTranscript) -> ArchivedTask {
    ArchivedTask {
        tool_use_id: "toolu_spawn".into(),
        agent_id: AGENT.into(),
        started_at_ms: 1,
        ended_at_ms: 2,
        input_tokens: None,
        output_tokens: None,
        transcript: Some(transcript),
        provenance: None,
    }
}

const OUTPUT: &str = "line 0\nline 1\nline 2\nline 3\n";

/// The session's call is sealed beside the session transcript with its line,
/// the subagent's beside the subagent's, the orphan beside the session's as
/// `original_line_absent`; every body is the spooled one, its digest the
/// receipt's.
#[test]
fn each_original_is_sealed_beside_the_transcript_that_records_its_call() {
    let repo = repo();
    let main_receipt = spool(&repo, MAIN_CALL, None, OUTPUT);
    let sub_receipt = spool(&repo, SUB_CALL, Some(AGENT), OUTPUT);
    let orphan_receipt = spool(&repo, ORPHAN_CALL, None, "orphan\nout\nput\n");
    let packs = RulePackSet::builtin();
    let spool = Spool::load(&repo.paths.common_dir, SESSION, &packs);

    let main_raw = format!(
        "{{\"type\":\"user\"}}\n{}\n",
        result_line(MAIN_CALL, &folded(OUTPUT))
    );
    let (raw, mut main) = sealed_pair(&main_raw);
    main.originals = spool.claimed_by(&raw, main.full.as_slice());
    let (raw, mut sub) = sealed_pair(&format!("{}\n", result_line(SUB_CALL, &folded(OUTPUT))));
    sub.originals = spool.claimed_by(&raw, sub.full.as_slice());
    let mut tasks = vec![task(sub)];
    spool.attach_orphans(Some(&mut main), &mut tasks);

    let by_id = |pair: &ArchivedTranscript, id: &str| {
        pair.originals
            .iter()
            .find(|o| o.tool_use_id == id)
            .cloned()
            .unwrap()
    };
    let own = by_id(&main, MAIN_CALL);
    assert_eq!((own.state, own.line), (OriginalState::Sealed, Some(1)));
    assert_eq!(
        own.rewritten,
        Some(Rewritten::Delivered(RewriteForm::FoldedOutput))
    );
    let digest =
        |o: &ArchivedOriginal| rift_shadow::sha256_hex(&[o.body.as_ref().unwrap().as_slice()]);
    assert_eq!(digest(&own), main_receipt.sha256);
    let inner = by_id(tasks[0].transcript.as_ref().unwrap(), SUB_CALL);
    assert_eq!((inner.state, inner.line), (OriginalState::Sealed, Some(0)));
    assert_eq!(inner.agent_id.as_deref(), Some(AGENT));
    assert_eq!(digest(&inner), sub_receipt.sha256);
    let orphan = by_id(&main, ORPHAN_CALL);
    assert_eq!(
        (orphan.state, orphan.line, orphan.rewritten),
        (OriginalState::LineAbsent, None, None)
    );
    assert_eq!(digest(&orphan), orphan_receipt.sha256);
    assert_eq!(
        main.originals.len(),
        2,
        "the subagent's call is not the session's"
    );
    let left = std::fs::read_dir(session_spool_dir(&repo.paths.common_dir, SESSION))
        .unwrap()
        .count();
    assert_eq!(left, 3, "the archive never deletes a spooled record");
}

/// A client that recorded the original itself delivered no rewrite.
#[test]
fn a_transcript_holding_the_original_itself_says_spooled_not_delivered() {
    let repo = repo();
    spool(&repo, MAIN_CALL, None, OUTPUT);
    let spool = Spool::load(&repo.paths.common_dir, SESSION, &RulePackSet::builtin());
    let (raw, pair) = sealed_pair(&format!("{}\n", result_line(MAIN_CALL, OUTPUT)));
    let claimed = spool.claimed_by(&raw, pair.full.as_slice());
    assert_eq!(claimed[0].rewritten, Some(Rewritten::SpooledNotDelivered));
}

/// A record whose body no longer matches its header, or that is not in the
/// grammar at all, is sealed as far as it can be and marked corrupt; a link
/// is never followed.
#[test]
fn a_corrupt_record_is_sealed_as_far_as_it_can_be() {
    let repo = repo();
    spool(&repo, MAIN_CALL, None, OUTPUT);
    let dir = session_spool_dir(&repo.paths.common_dir, SESSION);
    let path = dir.join(format!("{MAIN_CALL}{SPOOL_SUFFIX}"));
    let mut bytes = std::fs::read(&path).unwrap();
    let last = bytes.len() - 3;
    bytes[last] ^= 1;
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    std::fs::write(
        dir.join(format!("toolu_garbage{SPOOL_SUFFIX}")),
        b"no grammar here",
    )
    .unwrap();
    let spool = Spool::load(&repo.paths.common_dir, SESSION, &RulePackSet::builtin());
    let (_, mut pair) = sealed_pair("{}\n");
    spool.attach_orphans(Some(&mut pair), &mut []);
    assert_eq!(pair.originals.len(), 2);
    for o in &pair.originals {
        assert_eq!(o.state, OriginalState::SpoolCorrupt, "{}", o.tool_use_id);
        assert!(
            o.body.is_some(),
            "{} is sealed as far as it can be",
            o.tool_use_id
        );
    }
    let garbage = pair
        .originals
        .iter()
        .find(|o| o.tool_use_id == "toolu_garbage")
        .unwrap();
    assert_eq!(garbage.form, None);
    assert_eq!(
        garbage.body.as_ref().unwrap().as_slice(),
        b"no grammar here"
    );
}

/// With no transcript at all, an orphan waits in the spool for the next
/// archive rather than vanishing.
#[test]
fn an_orphan_with_nowhere_to_go_stays_spooled() {
    let repo = repo();
    spool(&repo, ORPHAN_CALL, None, OUTPUT);
    let spool = Spool::load(&repo.paths.common_dir, SESSION, &RulePackSet::builtin());
    spool.attach_orphans(None, &mut []);
    assert!(session_spool_dir(&repo.paths.common_dir, SESSION)
        .join(format!("{ORPHAN_CALL}{SPOOL_SUFFIX}"))
        .exists());
    assert!(
        Spool::load(&repo.paths.common_dir, "../x", &RulePackSet::builtin())
            .records
            .is_empty()
    );
}

/// A record that is a link is never read through.
#[cfg(unix)]
#[test]
fn a_linked_record_is_never_followed() {
    let repo = repo();
    spool(&repo, MAIN_CALL, None, OUTPUT);
    let dir = session_spool_dir(&repo.paths.common_dir, SESSION);
    let elsewhere = repo.paths.work_tree.join("planted.txt");
    std::fs::write(&elsewhere, "planted").unwrap();
    std::os::unix::fs::symlink(&elsewhere, dir.join(format!("toolu_link{SPOOL_SUFFIX}"))).unwrap();
    let spool = Spool::load(&repo.paths.common_dir, SESSION, &RulePackSet::builtin());
    let linked = spool
        .records
        .iter()
        .find(|r| r.tool_use_id == "toolu_link")
        .unwrap();
    assert_eq!(linked.state, OriginalState::SpoolCorrupt);
    assert!(linked.body.is_none());
}

#[test]
fn changed_packs_cannot_turn_a_fallback_record_into_a_sealed_original() {
    let repo = repo();
    spool(&repo, MAIN_CALL, None, "REDACTED notes\none\ntwo\nthree");
    let packs = RulePackSet::builtin()
        .with_vendor_pattern("osc", r"^\[?REDACTED", "acme@1")
        .unwrap();
    let spool = Spool::load(&repo.paths.common_dir, SESSION, &packs);
    let record = spool
        .records
        .iter()
        .find(|r| r.tool_use_id == MAIN_CALL)
        .unwrap();
    assert_eq!(record.state, OriginalState::Unsealed);
    assert!(record.body.is_none());
    let (_, mut pair) = sealed_pair("{}\n");
    spool.attach_orphans(Some(&mut pair), &mut []);
    let meta = rift_shadow::backend::ArchivedTask {
        tool_use_id: "task".into(),
        agent_id: "a1".into(),
        started_at_ms: 1,
        ended_at_ms: 2,
        input_tokens: None,
        output_tokens: None,
        transcript: Some(pair),
        provenance: None,
    }
    .metadata_json();
    let meta: serde_json::Value = serde_json::from_slice(&meta).unwrap();
    assert_eq!(meta["originals"][0]["original"], "original_unsealed");
    assert!(meta["originals"][0].get("sha256").is_none());
    assert!(session_spool_dir(&repo.paths.common_dir, SESSION)
        .join(format!("{MAIN_CALL}{SPOOL_SUFFIX}"))
        .exists());
}

#[test]
fn corrupt_spool_evidence_stays_corrupt_when_its_content_needs_fallback() {
    let repo = repo();
    spool(&repo, MAIN_CALL, None, "REDACTED notes\none\ntwo\nthree");
    let path = session_spool_dir(&repo.paths.common_dir, SESSION)
        .join(format!("{MAIN_CALL}{SPOOL_SUFFIX}"));
    let bytes = std::fs::read(&path).unwrap();
    let split = bytes.iter().position(|b| *b == b'\n').unwrap();
    let mut header: serde_json::Value = serde_json::from_slice(&bytes[..split]).unwrap();
    header["sha256"] = "0".repeat(64).into();
    let mut corrupt = serde_json::to_vec(&header).unwrap();
    corrupt.extend_from_slice(&bytes[split..]);
    std::fs::remove_file(&path).unwrap();
    std::fs::write(&path, corrupt).unwrap();
    let packs = RulePackSet::builtin()
        .with_vendor_pattern("osc", r"^\[?REDACTED", "acme@1")
        .unwrap();
    let spool = Spool::load(&repo.paths.common_dir, SESSION, &packs);
    let record = spool
        .records
        .iter()
        .find(|r| r.tool_use_id == MAIN_CALL)
        .unwrap();
    assert_eq!(record.state, OriginalState::SpoolCorrupt);
    let body = record.body.as_ref().unwrap();
    assert!(rift_shadow::redact::is_fallback_placeholder(
        std::str::from_utf8(body.as_slice()).unwrap()
    ));
}
