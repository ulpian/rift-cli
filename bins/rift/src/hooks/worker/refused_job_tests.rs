//! A journaled job whose row the schema refuses: the one failure a retry can
//! never change, and so the one the worker drops instead of requeueing.

use crate::agent_queue::{AgentJob, JobKind};

use super::{run_agent_job, Landed};

/// A tool-result job carrying a channel outside the closed vocabulary is
/// dropped on its first run, not requeued with an attempt counted, and the
/// refusal is said once on the capture-note channel, naming the relation and
/// the column.
#[test]
fn a_refused_tool_result_job_is_dropped_with_one_note_and_never_requeued() {
    let checkout = tempfile::tempdir().unwrap();
    let initialized = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(checkout.path())
        .status()
        .unwrap();
    assert!(initialized.success());
    let git_dir = rift_git::discover(checkout.path()).unwrap().git_dir;
    let job = AgentJob {
        schema_version: 1,
        work_tree: checkout.path().to_owned(),
        queued_at_ms: 1,
        attempts: 0,
        kind: JobKind::ToolResultTokens {
            tool_result_id: "c".repeat(64),
            session_id: "s-1".into(),
            tool_name: "Bash".into(),
            channel: "telepathy".into(),
            search: crate::search_measure::MeasuredSearch::Unmeasured,
            tree_key: None,
        },
    };
    let path = crate::agent_queue::enqueue(&git_dir, &job).unwrap();
    let t = crate::hooktime::HookTimer::new("test", false);
    assert_eq!(
        run_agent_job(
            &t,
            &git_dir,
            &path,
            &job,
            &crate::capture_queue::acquire_owner(&git_dir).unwrap()
        ),
        Landed::Nothing
    );
    assert!(!path.exists(), "the job file is consumed");
    assert!(
        crate::agent_queue::jobs(&git_dir).is_empty(),
        "dropped, not requeued"
    );
    let notes = crate::capture_queue::peek_errors(&git_dir);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(
        notes[0].contains("tool_result_tokens: check(channel)"),
        "{notes:?}"
    );
}
