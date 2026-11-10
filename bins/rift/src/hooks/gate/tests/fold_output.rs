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

/// The call id the fold tests spool under.
const CALL: &str = "toolu_fold";

/// A long listing with a forged test summary and an error inside its run.
fn listing_with_markers() -> String {
    let mut out = listing(200);
    out.push_str("test result: ok. 3 passed; 0 failed\nerror: snap_9999.json unreadable\n");
    out.push_str(&listing(200));
    out
}

/// Run the fold step on `stdout` at `mode`, after a held substitution when
/// `held_applied`; the folded result object, if any, and the queued rows.
fn fold_step(
    root: &std::path::Path,
    stdout: &str,
    mode: rift_recall_policy::Mode,
    held_applied: bool,
) -> (
    Option<serde_json::Value>,
    Vec<crate::agent_queue::ServeAudit>,
) {
    fold_streams_step(root, (stdout, ""), mode, held_applied)
}

/// [`fold_step`] over both streams of the result.
fn fold_streams_step(
    root: &std::path::Path,
    (stdout, stderr): (&str, &str),
    mode: rift_recall_policy::Mode,
    held_applied: bool,
) -> (
    Option<serde_json::Value>,
    Vec<crate::agent_queue::ServeAudit>,
) {
    let mut effective = observing();
    effective.fold_command_output = mode;
    let payload = serde_json::json!({
        "tool_name": "Bash",
        "tool_use_id": CALL,
        "cwd": root.display().to_string(),
        "tool_input": { "command": "cd crates && cargo build && ls -l snapshots" },
        "tool_response": { "stdout": stdout, "stderr": stderr, "interrupted": false },
    });
    let raw = hook("claude-code", "PostToolUse", "s-fold", payload);
    let registry = AdapterRegistry::with_builtin();
    let call = super::super::fold_output::FoldCall {
        registry: &registry,
        raw: &raw,
        git_dir: &root.join(".git"),
        work_tree: root,
        held_applied,
    };
    let (_, folded) = super::super::fold_output::fold_output_step(&call, &effective);
    (folded, audits(root))
}

/// ★ An enforced fold is the original with only the qualifying runs folded:
/// every kept line byte for byte, the forged summary and the error kept, the
/// original spooled under the fold form and the delivery matching it, and
/// the row booked as acted.
#[test]
fn fold_output_enforce_folds_only_qualifying_runs_after_spooling_the_original() {
    use rift_shadow::backend::rewrite::{
        check_rendering, parse_fold_line, RenderCheck, ResultText, RewriteForm,
    };
    let tmp = super::fixtures::minted(checkout());
    let root = tmp.path();
    let stdout = listing_with_markers();
    let (folded, rows) = fold_step(root, &stdout, rift_recall_policy::Mode::Enforce, false);
    let folded = folded.expect("the listing folds under enforce");
    let delivered = folded["stdout"].as_str().unwrap();
    assert!(delivered.len() < stdout.len());
    assert!(delivered.contains("\ntest result: ok. 3 passed; 0 failed\n"));
    assert!(delivered.contains("\nerror: snap_9999.json unreadable\n"));
    let original: Vec<&str> = stdout.split('\n').collect();
    assert!(delivered
        .split('\n')
        .all(|line| parse_fold_line(line).is_some() || original.contains(&line)));
    assert!(delivered.ends_with(&original[original.len() - 20..].join("\n")));
    let (header, _) = super::fixtures::spooled(root, "s-fold", CALL);
    assert_eq!(header.form, RewriteForm::FoldedOutput);
    let check = check_rendering(
        header.form,
        ResultText {
            stdout: &stdout,
            stderr: "",
        },
        ResultText {
            stdout: delivered,
            stderr: "",
        },
        &header.replaced,
    );
    assert_eq!(check, RenderCheck::Matches);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].decision, ServeAuditKind::ToolOutputFoldApplied);
}

/// ★ Observe folds nothing; a result a held substitution already rewrote is
/// never folded too, under enforce, and both book only the observed row.
#[test]
fn fold_output_folds_nothing_when_observing_or_after_a_held_substitution() {
    let stdout = listing_with_markers();
    for (mode, held_applied) in [
        (rift_recall_policy::Mode::Observe, false),
        (rift_recall_policy::Mode::Enforce, true),
    ] {
        let tmp = super::fixtures::minted(checkout());
        let (folded, rows) = fold_step(tmp.path(), &stdout, mode, held_applied);
        assert_eq!(folded, None);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].decision, ServeAuditKind::ToolOutputFolded);
    }
}

/// ★ A refused spool (the original was already spooled for this call) folds
/// nothing and books the observed row.
#[test]
fn fold_output_enforce_without_a_spooled_original_delivers_the_result_unchanged() {
    let tmp = super::fixtures::minted(checkout());
    let root = tmp.path();
    let stdout = listing_with_markers();
    let (first, _) = fold_step(root, &stdout, rift_recall_policy::Mode::Enforce, false);
    assert!(first.is_some());
    let (again, rows) = fold_step(root, &stdout, rift_recall_policy::Mode::Enforce, false);
    assert_eq!(again, None);
    assert_eq!(
        rows.last().unwrap().decision,
        ServeAuditKind::ToolOutputFolded
    );
}

/// ★ A chained build whose bulk went to stderr folds there too: the error and
/// its span are kept, stdout is delivered as written, and the spool checks
/// both streams of the delivery against the original.
#[test]
fn fold_output_enforce_folds_a_build_written_to_stderr_and_keeps_its_error() {
    use rift_shadow::backend::rewrite::{check_rendering, RenderCheck, ResultText};
    let tmp = super::fixtures::minted(checkout());
    let root = tmp.path();
    let mut stderr = String::new();
    for n in 0..300 {
        let _ = writeln!(stderr, "   Compiling a-dependency-named-{n} v0.{n}.1");
    }
    stderr.push_str("error[E0308]: mismatched types\n  --> src/lib.rs:4:5\n");
    let stdout = "built\n";
    let (folded, rows) = fold_streams_step(
        root,
        (stdout, &stderr),
        rift_recall_policy::Mode::Enforce,
        false,
    );
    let folded = folded.expect("the stderr build log folds under enforce");
    assert_eq!(folded["stdout"].as_str(), Some(stdout));
    let delivered = folded["stderr"].as_str().unwrap();
    assert!(delivered.len() < stderr.len() / 4);
    assert!(delivered.contains("error[E0308]: mismatched types"));
    assert!(delivered.contains("--> src/lib.rs:4:5"));
    let (header, _) = super::fixtures::spooled(root, "s-fold", CALL);
    let check = check_rendering(
        header.form,
        ResultText {
            stdout,
            stderr: &stderr,
        },
        ResultText {
            stdout,
            stderr: delivered,
        },
        &header.replaced,
    );
    assert_eq!(check, RenderCheck::Matches);
    assert_eq!(rows[0].decision, ServeAuditKind::ToolOutputFoldApplied);
}
