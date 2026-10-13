//! What a terminal result showed the window: the `matched` rows a Claude
//! Code `Bash` result leaves, member by member, and every reason it leaves
//! none.

use std::fmt::Write as _;
use std::path::Path;

use rift_agent::window_journal::output_match::{encode_line_hashes, file_line_hashes};
use rift_agent::window_journal::{Interval, Line};
use rift_shadow::hooks::AdapterRegistry;

use super::fixtures::{hook, lines_of, observing};
use crate::hooks::gate::linehash_cache::{linehash_cache_dir, linehash_cache_name};
use crate::hooks::gate::output_match::output_match_record;

/// Thirty distinct lines, tagged so two files never share one.
fn source(tag: &str) -> String {
    (1..=30).fold(String::new(), |mut out, n| {
        let _ = writeln!(out, "let {tag}_{n} = compute_{tag}({n});");
        out
    })
}

/// A checkout with `a.rs` and `sub/b.rs` in it.
fn checkout() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().unwrap();
    rift_git::test_support::fake_repo(tmp.path()).unwrap();
    std::fs::write(tmp.path().join("a.rs"), source("a")).unwrap();
    std::fs::create_dir_all(tmp.path().join("sub")).unwrap();
    std::fs::write(tmp.path().join("sub/b.rs"), source("b")).unwrap();
    tmp
}

fn bash(work_tree: &Path, command: &str, stdout: &str) -> serde_json::Value {
    serde_json::json!({
        "tool_name": "Bash",
        "cwd": work_tree.display().to_string(),
        "tool_input": { "command": command },
        "tool_response": { "stdout": stdout, "stderr": "", "interrupted": false },
    })
}

fn lines(text: &str, lo: usize, hi: usize) -> String {
    text.lines()
        .skip(lo - 1)
        .take(hi - lo + 1)
        .flat_map(|l| [l, "\n"])
        .collect()
}

/// Run the step on a `PostToolUse` of `payload` under `policy`.
fn record(
    work_tree: &Path,
    payload: serde_json::Value,
    policy: &rift_recall_policy::ServePolicy,
) -> Vec<Line> {
    let raw = hook("claude-code", "PostToolUse", "s-out", payload);
    output_match_record(
        &AdapterRegistry::with_builtin(),
        &raw,
        &work_tree.join(".git"),
        work_tree,
        policy,
        &observing(),
    );
    lines_of(work_tree, "s-out", None)
}

/// One matched row as `(member, path, runs)`.
type MatchedRow = (u32, String, Vec<(u32, u32)>);

/// The matched rows as `(member, path, runs)`.
fn matched(rows: &[Line]) -> Vec<MatchedRow> {
    rows.iter()
        .filter_map(|line| match line {
            Line::Matched {
                member, path, runs, ..
            } => Some((
                *member,
                path.clone(),
                runs.iter().map(|r: &Interval| (r.lo, r.hi)).collect(),
            )),
            _ => None,
        })
        .collect()
}

/// ★ A chain no grammar recognises books each member the lines it printed,
/// in member order, and the `cd` and the label own nothing.
#[test]
fn an_unrecognised_chain_books_each_member_its_own_lines() {
    let tmp = checkout();
    let root = tmp.path();
    let mut stdout = lines(&source("a"), 4, 13);
    stdout.push_str("=== b ===\n");
    stdout.push_str("7:let b_7 = compute_b(7);\n21:let b_21 = compute_b(21);\n");
    let command = "cd sub && awk 'NR>=4 && NR<=13' ../a.rs | cat; echo '=== b ==='; grep -n 'b_7\\|b_21' b.rs";
    let rows = record(
        root,
        bash(root, command, &stdout),
        &rift_recall_policy::ServePolicy::default(),
    );
    assert_eq!(
        matched(&rows),
        [
            (1, "a.rs".to_owned(), vec![(4, 13)]),
            (3, "sub/b.rs".to_owned(), vec![(7, 7), (21, 21)]),
        ]
    );
    let row = rows
        .iter()
        .find(|l| l.kind() == "matched")
        .unwrap()
        .to_json_line();
    assert!(row.contains(r#""basis":"output_matched""#), "{row}");
}

/// ★ A search over a directory books the files its printed paths name.
#[test]
fn a_search_over_a_named_directory_books_the_files_it_printed() {
    let tmp = checkout();
    let root = tmp.path();
    let stdout = "sub/b.rs:3:let b_3 = compute_b(3);\nsub/b.rs:30:let b_30 = compute_b(30);\n";
    let rows = record(
        root,
        bash(root, "grep -rn compute_b sub/ | head", stdout),
        &rift_recall_policy::ServePolicy::default(),
    );
    assert_eq!(
        matched(&rows),
        [(0, "sub/b.rs".to_owned(), vec![(3, 3), (30, 30)])]
    );
}

/// ★ Zero coverage for an unaligned segment: output that carries a file no
/// member names books nothing, however exactly it matches.
#[test]
fn output_no_member_names_books_nothing() {
    let tmp = checkout();
    let root = tmp.path();
    let stdout = lines(&source("a"), 1, 20);
    let rows = record(
        root,
        bash(root, "cargo run -q --example dump", &stdout),
        &rift_recall_policy::ServePolicy::default(),
    );
    assert!(matched(&rows).is_empty(), "{rows:?}");
    let rows = record(
        root,
        bash(root, "cd sub && cat ../a.rs", ""),
        &rift_recall_policy::ServePolicy::default(),
    );
    assert!(matched(&rows).is_empty(), "a silent member books nothing");
}

/// The lever at `off`, a result kept in a file, and a family other than
/// Claude Code each write nothing.
#[test]
fn an_off_lever_a_spilled_result_or_another_family_writes_nothing() {
    let tmp = checkout();
    let root = tmp.path();
    let stdout = lines(&source("a"), 1, 20);
    let mut off = rift_recall_policy::ServePolicy::default();
    off.interception.recognise_by_output = rift_recall_policy::Mode::Off;
    assert!(matched(&record(root, bash(root, "cat a.rs | cat", &stdout), &off)).is_empty());
    let mut spilled = bash(root, "cat a.rs | cat", &stdout);
    spilled["tool_response"]["persistedOutputPath"] = serde_json::json!("/tmp/x.txt");
    assert!(matched(&record(
        root,
        spilled,
        &rift_recall_policy::ServePolicy::default()
    ))
    .is_empty());
    let raw = hook(
        "codex",
        "PostToolUse",
        "s-out",
        bash(root, "cat a.rs | cat", &stdout),
    );
    assert!(!output_match_record(
        &AdapterRegistry::with_builtin(),
        &raw,
        &root.join(".git"),
        root,
        &rift_recall_policy::ServePolicy::default(),
        &observing(),
    ));
}

/// ★ The family's line-hash cache is what the hook matches against when it
/// holds the revision: a cache describing other lines under this revision's
/// name is believed, which is why only the worker writes it.
#[test]
fn the_hook_reads_the_cache_the_worker_wrote() {
    let tmp = checkout();
    let root = tmp.path();
    let bytes = std::fs::read(root.join("a.rs")).unwrap();
    let sha = rift_agent::content_sha256(&bytes).to_hex();
    let planted = source("z");
    let dir = linehash_cache_dir(&root.join(".git"));
    std::fs::create_dir_all(&dir).unwrap();
    let hashes = file_line_hashes(planted.as_bytes());
    let byte_count = u64::try_from(bytes.len()).unwrap();
    std::fs::write(
        dir.join(linehash_cache_name(&sha)),
        encode_line_hashes(&hashes, byte_count),
    )
    .unwrap();
    let rows = record(
        root,
        bash(root, "cat a.rs | cat", &lines(&planted, 2, 9)),
        &rift_recall_policy::ServePolicy::default(),
    );
    assert_eq!(matched(&rows), [(0, "a.rs".to_owned(), vec![(2, 9)])]);
}
