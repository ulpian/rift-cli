//! The `no-store-in-destructors` leg: every destructor in the hook-path
//! crates is read directly for a store open or a tokenizer load.
//!
//! A destructor is called by nothing a reader can point at: it runs wherever
//! its value goes out of scope, and on a hook's decision path that is with the
//! agent waiting. The `no-store-on-gate-path` walk follows calls, and since
//! d3af83c0 a bare `drop(x)` is the prelude's, so the walk no longer reaches
//! every `impl Drop` in the tree from one hook. The price of that correction
//! is that a destructor reached by scope alone is reached by no edge at all.
//! This leg closes the blind spot the direct way, with no reachability: every
//! `impl … Drop for …` block in the crates a decision runs through is read,
//! and a forbidden name in it is a finding wherever the block sits.

use std::ops::RangeInclusive;
use std::path::Path;

use crate::ci_meta::production_half;

use super::gate_path::{declared_for_tests, drop_impl_lines, FORBIDDEN_ON_GATE_PATH};
use super::text::{mask_code, token_offsets};
use super::{is_test_surface, rust_corpus, Leg};

/// The crates a hook decision runs through, destructors included: the two the
/// walk covers (the binary and the agent SDK), the hook state machine, and the
/// two recall crates a decision asks its policy and validity questions of.
///
/// A prefix that matches no tracked file is a hard error, not a smaller
/// screen: that is how this leg notices a crate rename.
pub(super) const DROP_CRATES: [&str; 5] = [
    "bins/rift/src/",
    "crates/rift-agent/src/",
    "crates/rift-shadow-hooks/src/",
    "crates/rift-recall-policy/src/",
    "crates/rift-recall-validity/src/",
];

/// **No destructor in a hook-path crate opens a store or loads the tokenizer.**
///
/// The cut is the production one every leg uses: the test surface and every
/// file a parent declares under `#[cfg(test)]` are skipped, and each file is
/// read up to its trailing test module. A `#[cfg(test)]` destructor is not
/// compiled into the hook, so it cannot run there, whatever it opens.
///
/// The screen is a mention, not an edge: each line of every `impl … Drop
/// for …` block is read for a `FORBIDDEN_ON_GATE_PATH` name — the same
/// vocabulary the walk screens for, never a second list. No reachability is
/// asked, which is the point: a destructor's caller is the scope its value
/// dies in, and no call graph spells that.
///
/// # Errors
/// If the corpus cannot be read, a named crate has no tracked file under it,
/// or the finder sees no destructor at all (the finder is then broken, and a
/// screen of nothing is not a pass).
pub fn no_store_in_destructors(root: &Path) -> Result<Leg, String> {
    let corpus = drop_corpus(rust_corpus(root)?)?;
    Ok(screen_destructors(&corpus)?.0)
}

/// The files under [`DROP_CRATES`], after checking each crate is still there.
fn drop_corpus(corpus: Vec<(String, String)>) -> Result<Vec<(String, String)>, String> {
    for prefix in DROP_CRATES {
        if !corpus.iter().any(|(rel, _)| rel.starts_with(prefix)) {
            return Err(format!(
                "gate-intercept: `{prefix}` is named as a hook-path crate and no tracked file \
                 lies under it — the crate moved or was renamed; correct DROP_CRATES, never \
                 shrink the screen silently"
            ));
        }
    }
    Ok(corpus
        .into_iter()
        .filter(|(rel, _)| DROP_CRATES.iter().any(|c| rel.starts_with(c)))
        .collect())
}

/// The screen over an explicit corpus — the seam a planted destructor is
/// proven through — returning the leg and how many destructors it read.
fn screen_destructors(corpus: &[(String, String)]) -> Result<(Leg, usize), String> {
    let for_tests = declared_for_tests(corpus);
    let mut v = Vec::new();
    let (mut destructors, mut files) = (0_usize, 0_usize);
    for (rel, text) in corpus {
        if is_test_surface(rel) || for_tests.contains(rel.as_str()) {
            continue;
        }
        let masked = mask_code(&production_half(text));
        let blocks = drop_impl_lines(&masked);
        if blocks.is_empty() {
            continue;
        }
        files += 1;
        destructors += blocks.len();
        // The production half leads with a newline, so line `n` of the file is
        // element `n` here and the finder's ranges index it directly.
        let lines: Vec<&str> = masked.lines().collect();
        for block in &blocks {
            v.extend(screen_block(rel, &lines, block));
        }
    }
    if destructors == 0 {
        return Err(
            "gate-intercept: no destructor (`impl Drop`) found in any hook-path crate — the \
             finder is broken or the corpus is not the one named, and a screen of nothing is \
             not a pass"
                .to_owned(),
        );
    }
    Ok((
        Leg::new(
            "no-store-in-destructors",
            format!(
                "{destructors} destructor(s) in {files} file(s) screened for {} names; none \
                 opens a store or loads the tokenizer",
                FORBIDDEN_ON_GATE_PATH.len()
            ),
            v,
        ),
        destructors,
    ))
}

/// Every forbidden name one `impl … Drop for …` block mentions, each as a
/// finding naming the site, the type and the name.
fn screen_block(rel: &str, lines: &[&str], block: &RangeInclusive<usize>) -> Vec<String> {
    let target = drop_target(lines, *block.start());
    let mut out = Vec::new();
    for line_no in block.clone() {
        let Some(line) = lines.get(line_no) else {
            continue;
        };
        for needle in FORBIDDEN_ON_GATE_PATH {
            if token_offsets(line, needle).is_empty() {
                continue;
            }
            out.push(format!(
                "{rel}:{line_no}: the destructor of `{target}` mentions `{needle}` — a \
                 destructor runs wherever its value is dropped, a decision path included, with \
                 the agent waiting on it; leave this to the worker that runs afterwards"
            ));
        }
    }
    out
}

/// The type the `impl … Drop for …` block whose `{` opens line `brace_line`
/// is written for: the words after the `Drop for` pair the finder matched, up
/// to a `where` clause or the brace.
///
/// Read back from the brace to the `impl` that opened the header, so a header
/// split over lines is read whole, by the same pair the finder matched on —
/// two readings of one header is how a type would be misnamed in a finding.
fn drop_target(lines: &[&str], brace_line: usize) -> String {
    let mut header: Vec<&str> = Vec::new();
    let mut at = brace_line;
    while let Some(line) = lines.get(at) {
        header.push(line);
        if at == 0 || !token_offsets(line, "impl").is_empty() {
            break;
        }
        at -= 1;
    }
    header.reverse();
    let joined = header.join(" ");
    let before_brace = joined
        .split_once('{')
        .map_or(joined.as_str(), |(head, _)| head);
    let words: Vec<&str> = before_brace.split_whitespace().collect();
    let Some(drop_at) = words
        .windows(2)
        .position(|pair| matches!(pair, [ty, "for"] if *ty == "Drop" || ty.ends_with("::Drop")))
    else {
        return "<unparsed impl header>".to_owned();
    };
    words
        .get(drop_at + 2..)
        .unwrap_or_default()
        .iter()
        .take_while(|word| **word != "where")
        .copied()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::super::gate_path::{screen_gate_path, GATE_CRATES};
    use super::*;

    /// Every decision entry point, defined and calling nothing, so the
    /// reachability leg runs and reaches none of the files beside it.
    const ROOTS: &str = "fn window_journal_step() {}\nfn read_gate() {}\nfn edit_record() {}\n\
                         fn gate_policy() {}\nfn gate_step() {}\nfn edit_step() {}\nfn gate() {}\n\
                         fn respond_with_decision() {}\nfn doc_gate() {}\nfn doc_gate_in() {}\n\
                         fn document_verify() {}\n";

    /// A file holding one destructor: `header` opens the impl block on line 2
    /// and `body` is its one statement, on line 4.
    fn destructor(header: &str, body: &str) -> String {
        format!(
            "struct Target;\n{header} {{\n    fn drop(&mut self) {{\n        {body}\n    }}\n}}\n"
        )
    }

    /// ★ A destructor that opens a store, or loads the tokenizer, is caught
    /// wherever it sits — in a file no decision entry point reaches, which the
    /// reachability leg rightly does not see. The two legs' division is pinned
    /// here: the same corpus is green for the walk and red for this screen.
    #[test]
    fn a_planted_destructor_that_opens_a_store_is_caught_wherever_it_sits() {
        let roots = (
            "bins/rift/src/hooks/gate/mod.rs".to_owned(),
            ROOTS.to_owned(),
        );
        for (rel, header, body, target, needle) in [
            (
                "bins/rift/src/elsewhere.rs",
                "impl Drop for Guard",
                "open_migrated();",
                "Guard",
                "open_migrated",
            ),
            (
                "crates/rift-recall-validity/src/trust/budget.rs",
                "impl<'a> Drop for Budget<'a>",
                "let _ = rift_tokens::count(t);",
                "Budget<'a>",
                "rift_tokens::",
            ),
        ] {
            let corpus = vec![roots.clone(), (rel.to_owned(), destructor(header, body))];
            let walked: Vec<(String, String)> = corpus
                .iter()
                .filter(|(rel, _)| GATE_CRATES.iter().any(|c| rel.starts_with(c)))
                .cloned()
                .collect();
            let (walk, _) = screen_gate_path(&walked).expect("the walk runs");
            assert!(
                walk.violations.is_empty(),
                "{rel}: the walk does not see it: {:#?}",
                walk.violations
            );

            let (leg, screened) = screen_destructors(&corpus).expect("the screen runs");
            assert_eq!(screened, 1, "{rel}: {}", leg.detail);
            assert_eq!(leg.violations.len(), 1, "{rel}: {:#?}", leg.violations);
            let finding = &leg.violations[0];
            for expected in [
                &format!("{rel}:4"),
                &format!("`{target}`"),
                &format!("`{needle}`"),
            ] {
                assert!(finding.contains(expected.as_str()), "{expected}: {finding}");
            }
        }
    }

    /// The three destructors the hook-path crates have today — a lock file
    /// removed, a timing line printed — are not findings, and all three are
    /// counted.
    #[test]
    fn a_destructor_that_only_removes_a_file_or_prints_is_not_a_finding() {
        let unlock = "let _ = std::fs::remove_file(&self.path);";
        let report =
            "if self.enabled {\n            eprintln!(\"{}\", self.report_line());\n        }";
        let corpus = vec![
            (
                "bins/rift/src/push_queue.rs".to_owned(),
                destructor("impl Drop for QueueLock", unlock),
            ),
            (
                "bins/rift/src/capture_queue.rs".to_owned(),
                destructor("impl Drop for QueueLock", unlock),
            ),
            (
                "bins/rift/src/hooktime.rs".to_owned(),
                destructor("impl Drop for HookTimer", report),
            ),
        ];
        let (leg, screened) = screen_destructors(&corpus).expect("the screen runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        assert_eq!(screened, 3, "{}", leg.detail);
        assert!(
            leg.detail
                .starts_with("3 destructor(s) in 3 file(s) screened for 9 names"),
            "{}",
            leg.detail
        );
    }

    /// ★ A destructor compiled only for tests never runs in a hook, so it is
    /// not screened: one inside the trailing `#[cfg(test)] mod tests`, cut
    /// away with the production half, and one in a file its parent declares
    /// under `#[cfg(test)]`. The same file declared plainly is screened and
    /// caught, so it is the declaration that hides it and not the finder.
    #[test]
    fn a_destructor_under_cfg_test_is_not_screened() {
        let clean = destructor(
            "impl Drop for QueueLock",
            "let _ = std::fs::remove_file(&self.path);",
        );
        let planted = destructor("impl Drop for Seed", "open_migrated();");
        for (declaration, gated) in [("#[cfg(test)] mod seeds;", true), ("mod seeds;", false)] {
            let corpus = vec![
                (
                    "bins/rift/src/main.rs".to_owned(),
                    format!("{declaration}\n{clean}\n#[cfg(test)]\nmod tests {{\n{planted}}}\n"),
                ),
                ("bins/rift/src/seeds.rs".to_owned(), planted.clone()),
            ];
            let (leg, screened) = screen_destructors(&corpus).expect("the screen runs");
            if gated {
                assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
                assert_eq!(screened, 1, "only the production destructor is read");
                continue;
            }
            assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
            assert!(
                leg.violations[0].contains("bins/rift/src/seeds.rs:4"),
                "{:#?}",
                leg.violations
            );
            assert_eq!(screened, 2, "{}", leg.detail);
        }
    }

    /// The type is read from the header the finder matched, even one split
    /// over lines with a `where` clause after it.
    #[test]
    fn the_type_a_destructor_belongs_to_is_read_from_a_header_split_over_lines() {
        let corpus = vec![(
            "bins/rift/src/wrap.rs".to_owned(),
            "impl<T>\n    std::ops::Drop for Wrapper<T>\nwhere\n    T: Send,\n{\n    \
             fn drop(&mut self) {\n        open_migrated();\n    }\n}\n"
                .to_owned(),
        )];
        let (leg, _) = screen_destructors(&corpus).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("wrap.rs:7: the destructor of `Wrapper<T>`"),
            "{:#?}",
            leg.violations
        );
    }

    /// The leg on the real tree: green, and it finds the destructors known to
    /// be there. A count that falls to zero is the finder breaking, not a
    /// cleaner tree, and the leg says so rather than passing.
    #[test]
    fn the_leg_screens_the_real_tree_and_finds_the_known_destructors() {
        let leg = no_store_in_destructors(&crate::workspace_root()).expect("the leg runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
        let screened: usize = leg
            .detail
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .expect("the detail opens with the count");
        assert!(screened >= 3, "{}", leg.detail);

        let err =
            screen_destructors(&[("bins/rift/src/x.rs".to_owned(), "fn f() {}\n".to_owned())])
                .expect_err("no destructor at all stops the gate");
        assert!(err.contains("destructor"), "{err}");
    }

    /// A hook-path crate that has moved stops the gate rather than shrinking
    /// the screen to the crates still found.
    #[test]
    fn a_drop_crate_prefix_that_matches_nothing_is_a_hard_error() {
        let corpus: Vec<(String, String)> = DROP_CRATES
            .iter()
            .skip(1)
            .map(|c| (format!("{c}lib.rs"), String::new()))
            .collect();
        let err = drop_corpus(corpus).expect_err("a missing crate stops the gate");
        assert!(err.contains("`bins/rift/src/`"), "{err}");
    }

    /// Every crate the decision walk covers is also read for destructors, so
    /// the two lists cannot drift apart in the direction that loses a crate.
    #[test]
    fn the_hook_path_crates_include_every_crate_the_walk_covers() {
        for walked in GATE_CRATES {
            assert!(
                DROP_CRATES.contains(&walked),
                "{walked} is walked but not screened"
            );
        }
    }
}
