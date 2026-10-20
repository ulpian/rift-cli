//! The `one-gate-chokepoint` leg: one file composes an agent's hook response.

use std::path::Path;

use crate::ci_meta::production_half;

use super::gate_path::declared_for_tests;
use super::{is_test_surface, line_of, rust_corpus, Leg};

/// The literals a hook response is made of. A file that spells one of these is
/// either the chokepoint, a test, a pinned artifact, or a violation.
const RESPONSE_LITERALS: [&str; 6] = [
    "permissionDecision",
    "\"decision\"",
    "updatedInput",
    "updatedToolOutput",
    "injectSteps",
    "hookSpecificOutput",
];

/// The one file entitled to compose an agent's hook response.
const CHOKEPOINT: &str = "crates/rift-agent/src/respond/mod.rs";

/// Where the exemptions live.
const CHOKEPOINT_ALLOW: &str = "ci/intercept-chokepoint-allow.txt";

/// One allow-list entry: a site that spells a response literal for an
/// unrelated reason.
#[derive(Debug)]
struct Allowed {
    path: String,
    literal: String,
    count: usize,
    seen: usize,
}

/// **One file composes an agent's hook response.**
///
/// The screen: in the production half of every tracked `.rs` file under
/// `crates/` and `bins/` other than the chokepoint, no literal from
/// `RESPONSE_LITERALS` may appear. Integration tests, benches and pinned
/// fixtures are outside the corpus — a test asserting the bytes an agent
/// receives has to name them, and a fixture is the artifact those bytes were
/// pinned from.
///
/// Comments and string bodies are NOT masked here, deliberately and unlike the
/// other legs: what this leg is looking for IS a string literal, and a response
/// field named in a comment beside the code that writes it is exactly the shape
/// worth reading before it becomes code.
///
/// An entry in `CHOKEPOINT_ALLOW` exempts an exact count of one literal in
/// one file. A count that no longer matches — in either direction — is a
/// violation, so the list shrinks with the sites it covers.
///
/// # Errors
/// If the corpus or the allow-list cannot be read.
pub fn one_gate_chokepoint(root: &Path) -> Result<Leg, String> {
    let allow = std::fs::read_to_string(root.join(CHOKEPOINT_ALLOW))
        .map_err(|e| format!("read {CHOKEPOINT_ALLOW}: {e}"))?;
    screen_chokepoint(&rust_corpus(root)?, &allow)
}

/// The screen over an explicit corpus and allow-list text — the seam a shape
/// test plants its violation through, so a test needs no repository of its own
/// (the same seam, for the same reason, as the retired-vocabulary screen's).
fn screen_chokepoint(corpus: &[(String, String)], allow_text: &str) -> Result<Leg, String> {
    let mut allow = read_allow_list(allow_text)?;
    // The same production cut every other leg takes: a file its parent
    // declares under `#[cfg(test)]` is test code wherever it lives, so a test
    // module moved out to its own file is not read as a second composer.
    let for_tests = declared_for_tests(corpus);
    let mut v = Vec::new();
    let mut allowed_sites = 0_usize;
    for (rel, text) in corpus {
        if rel == CHOKEPOINT || is_test_surface(rel) || for_tests.contains(rel.as_str()) {
            continue;
        }
        let production = production_half(text);
        for literal in RESPONSE_LITERALS {
            let hits: Vec<usize> = production
                .match_indices(literal)
                .map(|(at, _)| at)
                .collect();
            if hits.is_empty() {
                continue;
            }
            if let Some(entry) = allow
                .iter_mut()
                .find(|a| a.path == *rel && a.literal == literal)
            {
                entry.seen = hits.len();
                allowed_sites += hits.len();
                if entry.count == hits.len() {
                    continue;
                }
                v.push(format!(
                    "{rel}: `{literal}` occurs {} time(s), but {CHOKEPOINT_ALLOW} allows {} — \
                     re-read the site and correct the count, or move the code into {CHOKEPOINT}",
                    hits.len(),
                    entry.count
                ));
                continue;
            }
            for at in hits {
                v.push(format!(
                    "{rel}:{}: `{literal}` outside {CHOKEPOINT} — compose the response there, \
                     or, if this is not a hook response, say so in {CHOKEPOINT_ALLOW}",
                    // The production half is prefixed with one newline by the
                    // horizon split, so its offsets are one line ahead.
                    line_of(&production, at).saturating_sub(1)
                ));
            }
        }
    }
    for entry in &allow {
        if entry.seen == 0 {
            v.push(format!(
                "{CHOKEPOINT_ALLOW}: `{}` in {} no longer occurs — delete the entry",
                entry.literal, entry.path
            ));
        }
    }
    Ok(Leg::new(
        "one-gate-chokepoint",
        format!(
            "{} files screened; {CHOKEPOINT} is the only composer; {allowed_sites} allow-listed \
             site(s) spell a response literal for an unrelated reason",
            corpus.len()
        ),
        v,
    ))
}

/// Parse the allow-list's text.
fn read_allow_list(text: &str) -> Result<Vec<Allowed>, String> {
    let mut out = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let fields: Vec<&str> = line.split('|').map(str::trim).collect();
        let [path, literal, count, reason] = fields.as_slice() else {
            return Err(format!(
                "{CHOKEPOINT_ALLOW}:{}: expected `path | literal | count | reason`",
                index + 1
            ));
        };
        let count: usize = count
            .parse()
            .map_err(|_| format!("{CHOKEPOINT_ALLOW}:{}: `{count}` is not a count", index + 1))?;
        if reason.len() < 20 {
            return Err(format!(
                "{CHOKEPOINT_ALLOW}:{}: the reason must say why, in a sentence",
                index + 1
            ));
        }
        out.push(Allowed {
            path: (*path).to_owned(),
            literal: (*literal).to_owned(),
            count,
            seen: 0,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A corpus entry, spelled the way a caller would.
    fn file(rel: &str, text: &str) -> (String, String) {
        (rel.to_owned(), text.to_owned())
    }

    /// The allow-list text every shape test starts from: valid, and empty of
    /// entries, so a test that expects a finding is not accidentally exempted.
    const NO_ALLOWANCES: &str = "# nothing is allowed here\n";

    /// ★ A hook response composed anywhere but the one file is a finding.
    #[test]
    fn a_second_place_that_composes_a_hook_response_is_caught() {
        let corpus = vec![file(
            "bins/rift/src/sneaky.rs",
            "fn answer() -> String {\n    \"hookSpecificOutput\".to_owned()\n}\n",
        )];
        let leg = screen_chokepoint(&corpus, NO_ALLOWANCES).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("bins/rift/src/sneaky.rs:2"),
            "the finding names the line: {:#?}",
            leg.violations
        );
    }

    /// The chokepoint itself, and the test surfaces that have to name the bytes
    /// an agent receives, are outside the corpus.
    #[test]
    fn the_chokepoint_and_the_test_surfaces_are_not_findings() {
        let body = "let _ = \"permissionDecision\";\n";
        let corpus = vec![
            file(CHOKEPOINT, body),
            file("bins/rift/tests/hooks_e2e.rs", body),
            file("crates/rift-agent-codex/fixtures/pinned.rs", body),
        ];
        let leg = screen_chokepoint(&corpus, NO_ALLOWANCES).expect("the screen runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
    }

    /// A file's trailing test module is not production code, and the horizon
    /// this leg uses is the one the other meta-checks use.
    #[test]
    fn a_literal_below_the_test_horizon_is_not_a_finding() {
        let corpus = vec![file(
            "bins/rift/src/thing.rs",
            "fn real() {}\n#[cfg(test)]\nmod tests {\n    const X: &str = \"updatedInput\";\n}\n",
        )];
        let leg = screen_chokepoint(&corpus, NO_ALLOWANCES).expect("the screen runs");
        assert!(leg.violations.is_empty(), "{:#?}", leg.violations);
    }

    #[test]
    fn a_test_module_file_its_parent_declares_under_cfg_test_is_not_a_finding() {
        let corpus = vec![
            file(
                "bins/rift/src/hooks/agent.rs",
                "fn real() {}\n\n#[cfg(test)]\nmod injection_tests;\n",
            ),
            file(
                "bins/rift/src/hooks/agent/injection_tests.rs",
                "const X: &str = \"hookSpecificOutput\";\n",
            ),
            file(
                "bins/rift/src/hooks/other.rs",
                "const Y: &str = \"hookSpecificOutput\";\n",
            ),
        ];
        let leg = screen_chokepoint(&corpus, NO_ALLOWANCES).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("other.rs"),
            "{:#?}",
            leg.violations
        );
    }

    /// ★ An allow-list entry covers an exact count, so a real occurrence added
    /// beside an exempt one is still reported.
    #[test]
    fn an_allow_list_entry_covers_a_count_and_not_a_file() {
        let allow = "bins/rift/src/find.rs | \"decision\" | 1 | \
                     the serialized name of a finding kind, nothing to do with a hook\n";
        let one = vec![file(
            "bins/rift/src/find.rs",
            "fn kind(s: &str) {\n    let _ = \"decision\";\n}\n",
        )];
        assert!(screen_chokepoint(&one, allow)
            .expect("the screen runs")
            .violations
            .is_empty());
        let two = vec![file(
            "bins/rift/src/find.rs",
            "fn kind(s: &str) {\n    let _ = \"decision\";\n    let _ = \"decision\";\n}\n",
        )];
        let leg = screen_chokepoint(&two, allow).expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(
            leg.violations[0].contains("occurs 2 time(s)"),
            "{:#?}",
            leg.violations
        );
    }

    /// The list shrinks: an entry whose site is gone is itself a finding.
    #[test]
    fn an_allowance_for_a_site_that_no_longer_exists_is_a_finding() {
        let allow = "bins/rift/src/gone.rs | \"decision\" | 1 | \
                     a site that has since been deleted from the tree\n";
        let leg = screen_chokepoint(&[file("bins/rift/src/kept.rs", "fn f() {}\n")], allow)
            .expect("the screen runs");
        assert_eq!(leg.violations.len(), 1, "{:#?}", leg.violations);
        assert!(leg.violations[0].contains("no longer occurs"));
    }

    /// An allowance with no reason is not an allowance.
    #[test]
    fn an_allowance_must_say_why() {
        let err = read_allow_list("a.rs | \"decision\" | 1 | because\n")
            .expect_err("a bare word is not a reason");
        assert!(err.contains("say why"), "{err}");
        let err = read_allow_list("a.rs | \"decision\" | 1\n").expect_err("four fields or none");
        assert!(err.contains("expected"), "{err}");
    }
}
