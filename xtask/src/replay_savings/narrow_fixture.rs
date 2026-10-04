//! The narrowing corpus: one window paging through one long file, so each
//! way a repeat can stand against what is held has a pinned count — and the
//! buckets every other figure is published under stay the build without
//! narrowing.

use std::fmt::Write as _;
use std::path::Path;

use super::bucket::Bucket;
use super::classify::NarrowClass;
use super::render::{render_json, render_table};
use super::replay::replay;
use super::report::Report;
use super::tests::SENTINEL;
use super::Options;

/// The corpus replayed against a checkout holding the one file it pages.
fn replayed() -> Report {
    replayed_from("narrow", &[("big.rs", 200)])
}

/// The corpus under `corpus` replayed against a checkout holding `files`
/// under `src/`, each of the given number of lines.
fn replayed_from(corpus: &str, files: &[(&str, u32)]) -> Report {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    for (name, lines) in files {
        let body = (1..=*lines).fold(String::new(), |mut out, i| {
            let _ = writeln!(out, "line {i} of the long file");
            out
        });
        std::fs::write(repo.join("src").join(name), body).unwrap();
    }
    replay(&Options {
        transcripts: Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join("replay-savings")
            .join(corpus),
        repo,
        json: false,
        history: None,
        history_refs: vec!["HEAD".to_owned()],
        artifact: None,
    })
    .unwrap()
}

/// ★ Lines 1-100, then 51-150 (held at its head: narrowable), then 171-190
/// (nothing held: not narrowable), then 141-200 (held in two runs: multi).
/// The buckets are the ones the build without narrowing lands them in; the
/// split sits under them, and only the narrowable page would have been
/// narrowed, its fifty held lines over the floor.
#[test]
fn the_narrowing_corpus_pins_every_class_and_what_enforcing_spares() {
    let report = replayed();
    let calls = |bucket: Bucket| report.bucket(bucket).calls;
    assert_eq!(calls(Bucket::FirstSight), 1);
    assert_eq!(calls(Bucket::RepeatOverlapping), 2);
    assert_eq!(calls(Bucket::RepeatDisjoint), 1);
    assert_eq!(calls(Bucket::Declined), 0, "nothing is wholly held");
    let class = |pool: usize, class: NarrowClass| report.narrowing_split[pool][class.index()].calls;
    assert_eq!(class(0, NarrowClass::Narrowable), 1);
    assert_eq!(class(0, NarrowClass::Multi), 1);
    assert_eq!(class(0, NarrowClass::NotNarrowable), 0);
    assert_eq!(class(1, NarrowClass::NotNarrowable), 1);
    assert_eq!(
        report.narrowing_split[0]
            .iter()
            .map(|tally| tally.tokens)
            .sum::<u64>(),
        report.bucket(Bucket::RepeatOverlapping).tokens,
        "the split adds up to its pool, token for token"
    );
    assert_eq!(report.narrowed_if_enforced.calls, 1);
    assert!(report.narrowed_frame_tokens > 0, "the frame is paid for");
    assert!(
        report.narrowed_if_enforced.tokens
            < report.narrowing_split[0][NarrowClass::Narrowable.index()].tokens,
        "only the held half of the page is spared, net of its frame"
    );
}

/// The added lines print under the pools they split, and leak nothing.
#[test]
fn the_split_prints_under_its_pool_and_names_no_path() {
    let report = replayed();
    let table = render_table(&report);
    let lines: Vec<&str> = table.lines().collect();
    let at = |needle: &str| lines.iter().position(|line| line.contains(needle)).unwrap();
    let pool = at("repeat overlapping held coverage");
    assert!(lines[pool + 1].starts_with("    narrowable: one unheld run"));
    assert!(lines[pool + 2].starts_with("    multi: two or more runs"));
    assert!(lines[pool + 3].starts_with("    not narrowable"));
    assert!(table.contains("narrowed by this build (if enforced), net"));
    assert!(table.contains("narrowing as shipped (enforced on its own line)"));
    let document = render_json(&report);
    for rendered in [&table, &document] {
        assert!(!rendered.contains(SENTINEL));
        assert!(!rendered.contains("big.rs"));
    }
    assert!(document.contains("\"narrowed_if_enforced\""));
}

/// ★ A chain of a wholly held file and a page held at its head
/// (narrowed), a chain of a file never read and a narrowable page (runs
/// whole: one unheld member), then a lone narrowable page (narrowed). Two
/// calls narrowed, one of them a composed chain, and the chain line prints
/// beneath the line it is a share of.
#[test]
fn the_chain_corpus_pins_the_narrowed_calls_that_were_composed_chains() {
    let report = replayed_from(
        "narrow-chain",
        &[("small.rs", 40), ("big.rs", 200), ("new.rs", 40)],
    );
    assert_eq!(report.calls_with_several_reads, 3);
    assert_eq!(report.narrowed_if_enforced.calls, 2);
    assert_eq!(report.narrowed_chains_if_enforced.calls, 1);
    assert!(report.narrowed_chains_if_enforced.tokens > 0);
    assert!(
        report.narrowed_chains_if_enforced.tokens < report.narrowed_if_enforced.tokens,
        "the chain is a share of the narrowed calls, not all of them"
    );
    let table = render_table(&report);
    let lines: Vec<&str> = table.lines().collect();
    let at = |needle: &str| lines.iter().position(|line| line.contains(needle)).unwrap();
    assert_eq!(
        at("of it, composed chains"),
        at("narrowed by this build (if enforced), net") + 1
    );
    assert!(render_json(&report).contains("\"composed_chains\""));
}
