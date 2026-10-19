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
