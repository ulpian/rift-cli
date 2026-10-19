//! The fold step books what folding a result would spare and never more.

use std::fmt::Write as _;

use rift_shadow::hooks::AdapterRegistry;

use super::fixtures::{audits, hook, observing};
use crate::agent_queue::ServeAuditKind;

fn checkout() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    rift_git::test_support::fake_repo(tmp.path()).unwrap();
    tmp
}

fn listing(rows: usize) -> String {
    (0..rows).fold(String::from("total 4096\n"), |mut out, n| {
        let _ = writeln!(
            out,
            "-rw-r--r--  1 dev  staff  {} snap_{n:04}.json",
            1000 + n
        );
        out
    })
}

fn fold(
    root: &std::path::Path,
    command: &str,
    stdout: &str,
    mode: rift_recall_policy::Mode,
) -> bool {
    let mut effective = observing();
    effective.fold_command_output = mode;
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "cwd": root.display().to_string(),
        "tool_input": { "command": command },
        "tool_response": { "stdout": stdout, "stderr": "", "interrupted": false },
    });
    let raw = hook("claude-code", "PostToolUse", "s-fold", payload);
    super::super::fold_output::fold_output_record(
        &AdapterRegistry::with_builtin(),
        &raw,
        &root.join(".git"),
        root,
        &effective,
    )
}

#[test]
fn fold_output_books_the_bytes_a_fold_would_spare_as_its_own_observed_row() {
    let tmp = checkout();
    let root = tmp.path();
    let stdout = listing(400);
    assert!(fold(
        root,
        "ls -l snapshots",
        &stdout,
        rift_recall_policy::Mode::Observe
    ));
    let rows = audits(root);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].decision, ServeAuditKind::ToolOutputFolded);
    assert!(rows[0].frame.is_empty() && rows[0].basis.is_none());
    let plan = rift_agent::fold::fold_plan("ls -l snapshots", &stdout);
    let frame = rift_recall_serve::render_fold_frame(plan.ranges[0].1 - plan.ranges[0].0);
    let credit = plan.folded_bytes - frame.len() - 1;
    assert_eq!(rows[0].total, Some(u32::try_from(credit).unwrap()));
}

#[test]
fn fold_output_books_nothing_when_off_piped_through_tail_or_too_small() {
    let tmp = checkout();
    let root = tmp.path();
    let stdout = listing(400);
    assert!(!fold(
        root,
        "ls -l snapshots",
        &stdout,
        rift_recall_policy::Mode::Off
    ));
    assert!(!fold(
        root,
        "ls -l snapshots | tail -400",
        &stdout,
        rift_recall_policy::Mode::Observe
    ));
    assert!(!fold(
        root,
        "ls -l snapshots",
        &listing(40),
        rift_recall_policy::Mode::Observe
    ));
    assert!(audits(root).is_empty());
}

#[test]
fn the_fold_frame_is_a_line_the_originals_spool_reads_as_a_fold_of_its_count() {
    let frame = rift_recall_serve::render_fold_frame(258);
    assert_eq!(
        rift_shadow::backend::rewrite::parse_fold_line(&frame),
        Some(258)
    );
}
