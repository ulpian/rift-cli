//! Run state an older binary left only in the git dir, read by the real
//! readers and drained by the real worker step once the member is resolved.

use crate::agent_queue::{AgentJob, JobKind};

use super::{run_agent_job, Landed};

/// The job the drain below consumes: one the schema refuses, so its run is
/// complete without a projection and leaves one note on the capture queue.
fn refused_job(work_tree: &std::path::Path) -> AgentJob {
    AgentJob {
        schema_version: 1,
        work_tree: work_tree.to_owned(),
        queued_at_ms: 1,
        attempts: 0,
        kind: JobKind::ToolResultTokens {
            tool_result_id: "d".repeat(64),
            session_id: "s-old".into(),
            tool_name: "Bash".into(),
            channel: "telepathy".into(),
            search: crate::search_measure::MeasuredSearch::Unmeasured,
            tree_key: None,
        },
    }
}

/// Seeded only where an older binary writes — `<git_dir>/rift-sessions` and
/// `<git_dir>/rift-agent-queue` — the session cache and the job are read and
/// drained through the member's run state, and the old tree is left empty: a
/// reader still pointed at the old path would find nothing to read.
#[test]
fn run_state_seeded_only_in_the_old_tree_is_read_and_drained_after_the_move() {
    let checkout = tempfile::tempdir().unwrap();
    let initialized = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(checkout.path())
        .status()
        .unwrap();
    assert!(initialized.success());
    let git_dir = rift_git::discover(checkout.path()).unwrap().git_dir;
    crate::family::resolve_at(checkout.path()).unwrap();

    let old_sessions = git_dir.join("rift-sessions");
    std::fs::create_dir_all(&old_sessions).unwrap();
    let cache = rift_shadow::hooks::SessionCache::new("s-old", "claude-code", "active", 5);
    std::fs::write(
        old_sessions.join("s-old.json"),
        serde_json::to_vec(&cache).unwrap(),
    )
    .unwrap();
    // The job file as the journal names it, written by the journal's own
    // writer into a directory no family owns, then placed in the old tree.
    let scratch = tempfile::tempdir().unwrap();
    let written =
        crate::agent_queue::enqueue(scratch.path(), &refused_job(checkout.path())).unwrap();
    let old_queue = git_dir.join("rift-agent-queue");
    std::fs::create_dir_all(&old_queue).unwrap();
    std::fs::rename(&written, old_queue.join(written.file_name().unwrap())).unwrap();

    let sessions = crate::family::run_state::sessions_dir(&git_dir);
    assert!(!sessions.starts_with(&git_dir), "{}", sessions.display());
    let read = rift_shadow::hooks::read_cache(&sessions, "s-old").unwrap();
    assert_eq!(read.map(|c| c.updated_at_ms), Some(5));

    let jobs = crate::agent_queue::jobs(&git_dir);
    assert_eq!(jobs.len(), 1, "the job moved with the member");
    let (path, job) = &jobs[0];
    let t = crate::hooktime::HookTimer::new("test", false);
    assert_eq!(run_agent_job(&t, &git_dir, path, job), Landed::Nothing);
    assert!(crate::agent_queue::jobs(&git_dir).is_empty(), "drained");
    assert_eq!(crate::capture_queue::peek_errors(&git_dir).len(), 1);

    assert!(!old_sessions.exists() && !old_queue.exists());
    assert!(!git_dir.join("rift-capture-queue").exists());
}
