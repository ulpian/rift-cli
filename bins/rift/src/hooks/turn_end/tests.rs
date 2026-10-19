//! The turn-end checkpoint over real `git init` fixtures.

use super::*;
use crate::agent_queue::JobKind;

/// A committed checkout with one live session whose transcript holds `lines`.
fn live_session(root: &Path, lines: usize) -> (rift_git::RepoPaths, std::path::PathBuf) {
    let git = |args: &[&str]| {
        let ok = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=F",
                "-c",
                "user.email=f",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(ok.success());
    };
    git(&["init", "--quiet"]);
    git(&["commit", "--quiet", "--allow-empty", "-m", "fixture"]);
    let repo = rift_git::discover(root).unwrap();
    let transcript = root.join("t.jsonl");
    write_lines(&transcript, lines);
    let mut cache = rift_shadow::hooks::SessionCache::new("s-turn", "claude-code", "active", 1);
    cache.transcript_path = Some(transcript.to_string_lossy().into_owned());
    rift_shadow::hooks::write_cache(
        &crate::family::run_state::sessions_dir(&repo.git_dir),
        &cache,
    )
    .unwrap();
    (repo, transcript)
}

fn write_lines(path: &Path, lines: usize) {
    let body = (0..lines).fold(String::new(), |mut body, i| {
        use std::fmt::Write as _;
        let _ = writeln!(body, "{{\"type\":\"user\",\"n\":{i}}}");
        body
    });
    std::fs::write(path, body).unwrap();
}

fn turn_job(root: &Path, frontier: u64, at: i64) -> (AgentJob, SealTurn) {
    let seal = SealTurn {
        session_id: "s-turn".into(),
        agent: "claude-code".into(),
        frontier,
    };
    let job = AgentJob {
        schema_version: 1,
        work_tree: root.to_owned(),
        queued_at_ms: at,
        attempts: 0,
        kind: JobKind::SealTurn(seal.clone()),
    };
    (job, seal)
}

/// The `turn-end` decisions' checkpoints, oldest first.
fn turn_checkpoints(root: &Path) -> Vec<String> {
    let store =
        rift_store::open_capture_projection(super::super::projection_db_at(root).unwrap()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let rows = rt
        .block_on(store.query(
            "SELECT checkpoint_id FROM archive_decision WHERE reason = 'turn-end' \
             AND archived = 1 ORDER BY decided_at, decision_id",
            &[],
        ))
        .unwrap();
    rows.iter().map(|r| r.get_text(0).unwrap()).collect()
}

fn mint(root: &Path, frontier: u64, at: i64) -> bool {
    let (job, seal) = turn_job(root, frontier, at);
    turn_end_job(&crate::hooktime::HookTimer::new("test", false), &job, &seal).unwrap()
}

#[test]
fn one_turn_end_mints_one_checkpoint_under_the_reason_turn_end() {
    let checkout = tempfile::tempdir().unwrap();
    live_session(checkout.path(), 3);
    assert!(mint(checkout.path(), 60, 1_000));
    assert_eq!(turn_checkpoints(checkout.path()).len(), 1);
}

#[test]
fn a_redelivered_stop_coalesces_and_a_later_turn_end_stays_owed() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, _) = live_session(checkout.path(), 3);
    for (frontier, at) in [(60, 1), (60, 2), (90, 3)] {
        crate::agent_queue::enqueue(&repo.git_dir, &turn_job(checkout.path(), frontier, at).0)
            .unwrap();
    }
    let owed: Vec<u64> = crate::agent_queue::jobs(&repo.git_dir)
        .into_iter()
        .filter_map(|(_, job)| match job.kind {
            JobKind::SealTurn(seal) => Some(seal.frontier),
            _ => None,
        })
        .collect();
    assert_eq!(owed.len(), 2, "{owed:?}");
}

#[test]
fn a_second_turn_end_carries_the_first_turns_segment_by_its_oid() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, transcript) = live_session(checkout.path(), 3);
    assert!(mint(checkout.path(), 60, 1_000));
    write_lines(&transcript, 5);
    assert!(mint(checkout.path(), 100, 2_000));
    let ids = turn_checkpoints(checkout.path());
    assert_eq!(ids.len(), 2, "{ids:?}");
    let walker = rift_recall_substrate::ShardWalker::open(&repo.work_tree).unwrap();
    let part = |id: &str, i: usize| walker.read_native_entry(id, &segment_name(i)).unwrap();
    let first = part(&ids[0], 0).expect("the first turn is segmented");
    let carried = part(&ids[1], 0).expect("the second shard keeps part 0");
    assert_eq!(carried.0, first.0, "part 0 carried by its blob oid");
    assert!(part(&ids[1], 1).is_some(), "the second turn appends part 1");
}

/// The session's frontier sidecar, as doctor reads it.
fn recorded_frontier(repo: &rift_git::RepoPaths) -> frontier::TurnFrontier {
    let sessions = crate::family::run_state::sessions_dir(&repo.git_dir);
    match frontier::recorded(&sessions).pop() {
        Some(frontier::Recorded::Frontier(f)) => f,
        other => panic!("no frontier recorded: {other:?}"),
    }
}

#[test]
fn a_turn_end_records_the_frontier_it_sealed_and_a_long_lag_is_said() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, transcript) = live_session(checkout.path(), 3);
    assert!(mint(checkout.path(), 60, 1_000));
    let sealed = recorded_frontier(&repo);
    let read = std::fs::metadata(&transcript).unwrap().len();
    assert_eq!(sealed.checkpoint, turn_checkpoints(checkout.path())[0]);
    assert_eq!((sealed.read_bytes, sealed.sealed_lines), (read, Some(2)));
    assert_eq!(
        sealed.finding(sealed.lag_bytes(&crate::family::run_state::sessions_dir(&repo.git_dir))),
        None
    );
    let grown = "x".repeat(usize::try_from(frontier::LAG_BOUND_BYTES).unwrap() + 1);
    std::fs::write(
        &transcript,
        format!("{}{grown}", std::fs::read_to_string(&transcript).unwrap()),
    )
    .unwrap();
    assert_eq!(
        sealed.finding(sealed.lag_bytes(&crate::family::run_state::sessions_dir(&repo.git_dir))),
        Some("lagging")
    );
}

#[test]
fn a_truncated_transcript_reseals_whole_and_the_frontier_names_the_broken_chain() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, transcript) = live_session(checkout.path(), 3);
    assert!(mint(checkout.path(), 60, 1_000));
    write_lines(&transcript, 1);
    assert!(mint(checkout.path(), 20, 2_000));
    let sealed = recorded_frontier(&repo);
    assert_eq!(
        sealed.fallback,
        Some(frontier::SealFallback::SegmentChainBroken)
    );
    assert_eq!(sealed.finding(None), Some("segment-chain-broken"));
}

/// The capture projection, a runtime and the repository's packs, as the
/// archive opens them.
fn archive_inputs(
    root: &Path,
) -> (
    tokio::runtime::Runtime,
    rift_store::SqliteStore,
    rift_shadow::redact::RulePackSet,
) {
    let store =
        rift_store::open_capture_projection(super::super::projection_db_at(root).unwrap()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let (packs, _) = crate::redactor_packs::capture_rule_packs(root);
    (rt, store, packs)
}

#[test]
fn a_second_turn_end_puts_only_the_new_lines_of_the_main_transcript_through_the_gate() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, transcript) = live_session(checkout.path(), 3);
    assert!(mint(checkout.path(), 60, 1_000));
    write_lines(&transcript, 5);
    let (rt, store, packs) = archive_inputs(checkout.path());
    let inputs = TurnInputs {
        rt: &rt,
        store: &store,
        repo: &repo,
        session: "s-turn",
        packs: &packs,
        end: SealEnd::Turn,
    };
    let mut seen: Vec<(Vec<u8>, usize)> = Vec::new();
    let mut gate = |bytes: &SecretBytes, first_line: usize| {
        seen.push((bytes.as_slice().to_vec(), first_line));
        rift_shadow::redact::redact_with_at(bytes, &packs, first_line)
    };
    let raw = SecretBytes::new(std::fs::read(&transcript).unwrap());
    let (pair, _, read) = seal_turn_pair_through(&inputs, &raw, 0, &mut gate).unwrap();
    // Each turn's cut holds the newest line back until a later line admits
    // it, so the first turn sealed lines 0 and 1 and this one seals 2 and 3.
    let new_lines = "{\"type\":\"user\",\"n\":2}\n{\"type\":\"user\",\"n\":3}\n";
    assert_eq!(seen.len(), 1, "one gate call for the turn");
    assert_eq!(
        seen[0],
        (new_lines.as_bytes().to_vec(), 2),
        "only lines 2 and 3"
    );
    assert!(matches!(
        pair.full,
        rift_shadow::backend::TranscriptBody::Segments(_)
    ));
    assert!(
        read.compact.is_some(),
        "the compact form was cut segmented too"
    );
}

#[test]
fn the_commit_road_still_seals_the_main_transcript_whole_after_a_turn_end() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, transcript) = live_session(checkout.path(), 3);
    assert!(mint(checkout.path(), 60, 1_000));
    write_lines(&transcript, 5);
    let (rt, store, packs) = archive_inputs(checkout.path());
    let timer = crate::hooktime::HookTimer::new("test", false);
    let assembly =
        super::super::assemble_session(&timer, &rt, &store, &repo, "claude-code", "s-turn")
            .unwrap();
    let raw = SecretBytes::new(std::fs::read(&transcript).unwrap());
    let (whole, _) = rift_shadow::archived_transcript(&raw, 0, &packs).unwrap();
    let pair = assembly.transcript.expect("the transcript is sealed");
    assert!(matches!(
        pair.full,
        rift_shadow::backend::TranscriptBody::Whole(_)
    ));
    assert_eq!(pair.full.as_slice(), whole.full.as_slice());
    assert_eq!(pair.compact.as_slice(), whole.compact.as_slice());
    assert!(assembly.turn_read.is_none());
}

/// The session's final archive, as its `SessionEnd` hook runs it.
fn end_session(root: &Path, at: i64) -> String {
    let (rt, store, _) = archive_inputs(root);
    let decision = ArchiveDecision {
        archive: true,
        reason: ArchiveReason::SessionEnd,
        strategy: StrategyKind::Scheduled,
    };
    let id = super::super::archive::archive_in_git_hook(
        &rt,
        &store,
        root,
        None,
        "s-turn",
        "claude-code",
        decision,
        at,
    )
    .unwrap()
    .expect("the session end archives");
    id.as_str().to_owned()
}

#[test]
fn a_session_end_after_turn_ends_seals_only_the_tail_and_carries_every_turn_part() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, transcript) = live_session(checkout.path(), 3);
    assert!(mint(checkout.path(), 60, 1_000));
    write_lines(&transcript, 5);
    assert!(mint(checkout.path(), 100, 2_000));
    write_lines(&transcript, 7);
    let (rt, store, packs) = archive_inputs(checkout.path());
    let inputs = TurnInputs {
        rt: &rt,
        store: &store,
        repo: &repo,
        session: "s-turn",
        packs: &packs,
        end: SealEnd::Session,
    };
    let mut seen: Vec<usize> = Vec::new();
    let mut gate = |bytes: &SecretBytes, first_line: usize| {
        seen.push(first_line);
        rift_shadow::redact::redact_with_at(bytes, &packs, first_line)
    };
    let raw = SecretBytes::new(std::fs::read(&transcript).unwrap());
    let (pair, _, _) = seal_turn_pair_through(&inputs, &raw, 0, &mut gate).unwrap();
    assert_eq!(seen, vec![4], "lines 4 to 6 only: the turns sealed 0 to 3");
    let (whole, _) = rift_shadow::archived_transcript(&raw, 0, &packs).unwrap();
    assert_eq!(pair.full.as_slice(), whole.full.as_slice());
    let turns = turn_checkpoints(checkout.path());
    let end = end_session(checkout.path(), 3_000);
    let walker = rift_recall_substrate::ShardWalker::open(&repo.work_tree).unwrap();
    let part = |id: &str, i: usize| walker.read_native_entry(id, &segment_name(i)).unwrap();
    for i in 0..2 {
        assert_eq!(part(&end, i).unwrap().0, part(&turns[1], i).unwrap().0);
    }
    assert!(part(&end, 2).is_some(), "the tail is the final part");
}

#[test]
fn a_session_end_with_no_turn_end_seal_is_sealed_whole_as_before() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, transcript) = live_session(checkout.path(), 3);
    let (rt, store, packs) = archive_inputs(checkout.path());
    let inputs = TurnInputs {
        rt: &rt,
        store: &store,
        repo: &repo,
        session: "s-turn",
        packs: &packs,
        end: SealEnd::Session,
    };
    let raw = SecretBytes::new(std::fs::read(&transcript).unwrap());
    assert!(
        seal_turn_pair(&inputs, &raw, 0).is_none(),
        "the caller seals whole"
    );
    let end = end_session(checkout.path(), 1_000);
    let walker = rift_recall_substrate::ShardWalker::open(&repo.work_tree).unwrap();
    assert!(walker
        .read_native_entry(&end, &segment_name(0))
        .unwrap()
        .is_none());
    assert!(metadata_of(&repo, &end).get("reconciled").is_none());
}

#[test]
fn a_session_end_over_a_transcript_shorter_than_its_frontier_reseals_whole_and_says_so() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, transcript) = live_session(checkout.path(), 3);
    assert!(mint(checkout.path(), 60, 1_000));
    write_lines(&transcript, 1);
    let end = end_session(checkout.path(), 2_000);
    let walker = rift_recall_substrate::ShardWalker::open(&repo.work_tree).unwrap();
    assert!(walker
        .read_native_entry(&end, &segment_name(0))
        .unwrap()
        .is_none());
    let sealed = recorded_frontier(&repo);
    assert_eq!(sealed.checkpoint, end);
    assert_eq!(sealed.finding(None), Some("segment-chain-broken"));
}

/// The final shard's metadata document.
fn metadata_of(repo: &rift_git::RepoPaths, checkpoint: &str) -> serde_json::Value {
    let walker = rift_recall_substrate::ShardWalker::open(&repo.work_tree).unwrap();
    let (_, bytes) = walker
        .read_native_entry(checkpoint, "metadata.json")
        .unwrap()
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn a_session_end_whose_parts_match_the_whole_seal_records_reconciled() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, transcript) = live_session(checkout.path(), 3);
    assert!(mint(checkout.path(), 60, 1_000));
    write_lines(&transcript, 7);
    let end = end_session(checkout.path(), 2_000);
    let meta = metadata_of(&repo, &end);
    assert_eq!(meta["reconciled"], true);
    assert!(meta.get("supersedes_segments").is_none());
    assert_eq!(recorded_frontier(&repo).finding(None), None);
}

#[test]
fn a_session_end_with_a_divergent_part_writes_the_whole_seal_and_names_the_mismatch() {
    let checkout = tempfile::tempdir().unwrap();
    let (repo, transcript) = live_session(checkout.path(), 3);
    assert!(mint(checkout.path(), 60, 1_000));
    write_lines(&transcript, 7);
    let (rt, store, packs) = archive_inputs(checkout.path());
    let inputs = TurnInputs {
        rt: &rt,
        store: &store,
        repo: &repo,
        session: "s-turn",
        packs: &packs,
        end: SealEnd::Session,
    };
    // The tail is sealed from other bytes, as a gate gone wrong would.
    let mut planted = |bytes: &SecretBytes, first_line: usize| {
        let other = String::from_utf8(bytes.as_slice().to_vec()).unwrap();
        let other = SecretBytes::new(other.replace("\"n\"", "\"m\"").into_bytes());
        rift_shadow::redact::redact_with_at(&other, &packs, first_line)
    };
    let raw = SecretBytes::new(std::fs::read(&transcript).unwrap());
    let (pair, _, read) = seal_turn_pair_through(&inputs, &raw, 0, &mut planted).unwrap();
    let (whole, _) = rift_shadow::archived_transcript(&raw, 0, &packs).unwrap();
    assert!(matches!(
        pair.full,
        rift_shadow::backend::TranscriptBody::Whole(_)
    ));
    assert_eq!(pair.full.as_slice(), whole.full.as_slice());
    assert_eq!(
        pair.reconciliation,
        Some(rift_shadow::backend::Reconciliation::Superseded)
    );
    frontier::record(&repo.git_dir, "s-turn", "cp-end", read, 2_000);
    assert_eq!(
        recorded_frontier(&repo).finding(None),
        Some("segment-mismatch")
    );
}

/// Append `lines` assistant records of about 1 KB each, numbered from `from`.
fn append_turns(path: &Path, from: usize, lines: usize) {
    use std::io::Write as _;
    let text = "the quick brown fox reads src/lib.rs and edits fn main ".repeat(17);
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    for n in from..from + lines {
        let line = serde_json::json!({
            "type": "assistant",
            "n": n,
            "message": {"role": "assistant", "content": [{"type": "text", "text": text}]},
        });
        writeln!(file, "{line}").unwrap();
    }
}

/// This process's CPU seconds (user + system) so far, read through `ps`
/// because the crate forbids the unsafe `getrusage` call.
fn cpu_seconds() -> f64 {
    let out = std::process::Command::new("ps")
        .args(["-o", "time=", "-p", &std::process::id().to_string()])
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    text.trim()
        .split(':')
        .fold(0.0, |acc, part| acc * 60.0 + part.parse::<f64>().unwrap())
}

/// Loose plus packed object count and KiB, as `git count-objects -v` says.
fn objects(root: &Path) -> (u64, u64) {
    let out = std::process::Command::new("git")
        .args(["count-objects", "-v"])
        .current_dir(root)
        .output()
        .unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    let field = |name: &str| -> u64 {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.trim().parse().ok())
            .unwrap_or(0)
    };
    (
        field("count:") + field("in-pack:"),
        field("size:") + field("size-pack:"),
    )
}

/// One step's wall, CPU and object growth, printed on one line.
fn timed(root: &Path, label: &str, step: impl FnOnce()) {
    let (objects_before, kib_before) = objects(root);
    let (cpu, started) = (cpu_seconds(), std::time::Instant::now());
    step();
    let (wall, cpu) = (started.elapsed(), cpu_seconds() - cpu);
    let (objects_after, kib_after) = objects(root);
    eprintln!(
        "{label}: wall={wall:?} cpu={cpu:.2}s objects=+{} kib=+{}",
        objects_after - objects_before,
        kib_after - kib_before
    );
}

#[test]
#[ignore = "a timing on a 28 MB transcript, run by hand in a release build"]
fn a_turn_end_mint_on_a_28_mb_transcript_is_timed() {
    let checkout = tempfile::tempdir().unwrap();
    let root = checkout.path();
    let (_, transcript) = live_session(root, 0);
    append_turns(&transcript, 0, 28_000);
    let bytes = std::fs::metadata(&transcript).unwrap().len();
    eprintln!("bytes={bytes}");
    timed(root, "first turn end", || assert!(mint(root, 1, 1_000)));
    for turn in 1..=3 {
        append_turns(&transcript, 28_000 + 40 * (turn - 1), 40);
        let n = u64::try_from(turn).unwrap();
        let at = 1_000 + i64::try_from(turn).unwrap() * 1_000;
        timed(root, &format!("turn end {}", turn + 1), || {
            assert!(mint(root, 1 + n, at));
        });
    }
    append_turns(&transcript, 28_120, 40);
    timed(root, "session end", || {
        end_session(root, 9_000);
    });
}
