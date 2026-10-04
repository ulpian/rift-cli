//! The shapes corpus: one window of shell calls the grammar refuses, one per
//! shape, each returning a result of its own size, so every shape has a
//! pinned count and the ranked order is pinned with them.

use std::path::Path;

use serde_json::Value;

use super::render::{render_json, render_table};
use super::replay::replay;
use super::report::Report;
use super::shell_shape::ranked;
use super::tests::SENTINEL;
use super::Options;

/// The shapes naming a work-tree file, in the order their results shrink —
/// the last two return the same bytes, so the tie falls to declaration order.
const NAMED_IN_RANK_ORDER: [&str; 26] = [
    "piped_into_filter:grep",
    "sed_script",
    "nl_or_bat_option",
    "numbered_print_piped",
    "head_tail_option",
    "sed_option",
    "several_paths",
    "search_flags",
    "search_several_paths",
    "git_show",
    "git_diff",
    "git_log_patch_or_blame",
    "piped_into_filter:wc",
    "piped_into_head_tail",
    "chain_member:cargo",
    "chain_composition",
    "redirect",
    "quoting_or_shell_character",
    "heredoc",
    "wrapper",
    "script_interpreter",
    "size_only",
    "pager_or_dump",
    "other",
    "cat_numbered",
    "cat_option",
];

/// The shapes only on the loose bound: neither names a file by a word of its
/// own, so they rank after every shape that does.
const LOOSE_ONLY_IN_RANK_ORDER: [&str; 2] = ["git_show_rev_path", "piped_from:cargo"];

/// The corpus replayed against a checkout holding the two files it names.
fn replayed() -> Report {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/a.rs"), "line one\nline two\n").unwrap();
    std::fs::write(repo.join("src/b.rs"), "line three\n").unwrap();
    replay(&Options {
        transcripts: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("replay-savings")
            .join("shapes"),
        repo,
        json: false,
        history: None,
        history_refs: vec!["HEAD".to_owned()],
        artifact: None,
    })
    .unwrap()
}

#[test]
fn every_refused_shape_is_counted_once_and_ranked_by_its_tokens() {
    let report = replayed();
    let rows = ranked(&report.invisible_shell_shapes);
    let keys: Vec<String> = rows.iter().map(|(shape, _)| shape.key()).collect();
    let expected: Vec<&str> = NAMED_IN_RANK_ORDER
        .iter()
        .chain(&LOOSE_ONLY_IN_RANK_ORDER)
        .copied()
        .collect();
    assert_eq!(keys, expected);
    for (shape, row) in &rows {
        let named = NAMED_IN_RANK_ORDER.contains(&shape.key().as_str());
        assert_eq!(row.all.calls, 1, "{}", shape.key());
        assert_eq!(row.named.calls, u64::from(named), "{}", shape.key());
    }
    let (_, numbered) = rows[24];
    let (_, option) = rows[25];
    assert_eq!(numbered, option, "the tie the declaration order breaks");
}

#[test]
fn the_shapes_add_up_to_the_two_invisible_bounds() {
    let report = replayed();
    let shapes = report.invisible_shell_shapes.values();
    let all: u64 = shapes.clone().map(|row| row.all.tokens).sum();
    let named: u64 = shapes.map(|row| row.named.tokens).sum();
    assert_eq!(all, report.invisible_shell.tokens, "the loose bound");
    assert_eq!(
        named, report.invisible_shell_named_file.tokens,
        "the tight one"
    );
    assert_eq!(report.invisible_shell.calls, 28);
    assert_eq!(report.invisible_shell_named_file.calls, 26);
}

#[test]
fn the_ranked_section_prints_under_the_invisible_lines_and_names_no_command() {
    let report = replayed();
    let table = render_table(&report);
    let lines: Vec<&str> = table.lines().collect();
    let at = |needle: &str| lines.iter().position(|line| line.contains(needle)).unwrap();
    let named = at("of them, naming a file this checkout has");
    assert!(lines[named + 1].starts_with("    refused, by shape"));
    assert!(lines[named + 2].starts_with("      piped_into_filter:grep "));
    assert!(lines[named + 29].starts_with("      piped_from:cargo "));
    assert!(lines[named + 30].contains("shell calls / of them recognised"));
    let document: Value = serde_json::from_str(&render_json(&report)).unwrap();
    let rows = document["invisible_shell_read_shapes"].as_array().unwrap();
    assert_eq!(rows.len(), 28);
    assert_eq!(rows[0]["shape"], "piped_into_filter:grep");
    assert_eq!(rows[27]["naming_a_work_tree_file"]["calls"], 0);
    let rendered = [table, render_json(&report)];
    for text in &rendered {
        assert!(!text.contains(SENTINEL), "a prompt reached the output");
        for fragment in ["src/", "a.rs", "HEAD", "$EXTRA", "NR<5", "see;"] {
            assert!(!text.contains(fragment), "`{fragment}` reached the output");
        }
    }
}

/// A backslash the shell keeps as written is not a reason in itself: a line
/// refused for something else is counted under that, and only a line the
/// backslash alone keeps out of the grammar is counted as quoting.
#[test]
fn a_literal_backslash_is_counted_as_quoting_only_when_it_is_the_reason() {
    use super::shell_shape::{shape_of, Shape};
    assert_eq!(shape_of(r#"grep -rn "a\|b" src"#), Shape::SearchFlags);
    assert_eq!(shape_of(r"grep -rn 'a\|b' src"), Shape::SearchFlags);
    assert_eq!(shape_of(r#"sed -n "/a\|b/p" src/a.rs"#), Shape::SedScript);
    assert_eq!(
        shape_of(r#"grep "a\|b" src/a.rs"#),
        Shape::QuotingOrShellCharacter
    );
    assert_eq!(
        shape_of(r#"grep "a\|b" src/a.rs 2>/dev/null"#),
        Shape::QuotingOrShellCharacter
    );
    assert_eq!(
        shape_of(r#"grep -rn "a\$b" src"#),
        Shape::QuotingOrShellCharacter,
        "an escaped dollar is the shell's to resolve"
    );
}
