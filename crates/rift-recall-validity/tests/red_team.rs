//! Phase-7 prompt-injection red-team harness (Stage 3, pulled into M3).
//!
//! The append-only corpus (`fixtures/injection_corpus.jsonl`) of hostile
//! untrusted payloads is run through the **universal untrusted-content serve
//! chokepoint** — [`rift_recall_validity::trust::serialize_untrusted`] — the one
//! serializer every serving path routes tool-output/transcript bytes through
//! (ADR-0003). The invariant: no directive/role/exfil span survives verbatim into
//! a served artifact, and a directive is always detected + noted (never quoted).
//!
//! This is a CI **gate** (`.github/workflows/ci.yml` job `red-team`): a leak
//! blocks merge. Adding a payload can only strengthen it — the corpus is
//! append-only.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use rift_recall_validity::trust::serialize_untrusted;
use rift_recall_validity::{Extractor, LogExtractor};

const CORPUS: &str = include_str!("../fixtures/injection_corpus.jsonl");

#[derive(serde::Deserialize)]
struct Case {
    payload: String,
    forbidden: Vec<String>,
}

fn corpus() -> Vec<Case> {
    CORPUS
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("bad corpus line {l:?}: {e}")))
        .collect()
}

#[test]
fn corpus_is_substantive_and_well_formed() {
    let cases = corpus();
    assert!(
        cases.len() >= 10,
        "the red-team corpus must be substantive (got {})",
        cases.len()
    );
    for c in &cases {
        assert!(!c.payload.is_empty());
        assert!(
            !c.forbidden.is_empty(),
            "each case must name forbidden spans"
        );
    }
}

#[test]
fn no_forbidden_span_survives_the_untrusted_serve_chokepoint() {
    for c in corpus() {
        // Both untrusted provenance kinds route through the same serializer.
        for kind in ["tool_output", "transcript"] {
            let digest = serialize_untrusted(kind, c.payload.as_bytes(), "log:redteam");
            let json = serde_json::to_string(&digest).expect("digest serializes");
            for span in &c.forbidden {
                assert!(
                    !json.contains(span.as_str()),
                    "chokepoint leaked forbidden span {span:?} for payload {:?}\n  digest: {json}",
                    c.payload
                );
            }
            // A directive/role-shaped payload is detected and noted — without
            // quoting the offending span (the note is a count, never content).
            assert!(
                digest.redaction_note.is_some(),
                "directive not detected for payload {:?}",
                c.payload
            );
            let note = digest.redaction_note.unwrap_or_default();
            for span in &c.forbidden {
                assert!(
                    !note.contains(span.as_str()),
                    "the redaction note quoted a forbidden span {span:?}"
                );
            }
        }
    }
}

#[test]
fn log_extractor_summary_never_reveals_an_exfil_secret() {
    // The deterministic log-severity extractor keeps *severity-marked* lines
    // (dedup + count) but must never surface a non-severity injection directive.
    // (Untrusted content is served as a neutral digest, not this extractor; this
    // guards the trusted-summary rung against non-severity directive echo.)
    let log = LogExtractor;
    for c in corpus() {
        let out = String::from_utf8_lossy(&log.extract(c.payload.as_bytes())).to_string();
        for span in &c.forbidden {
            // Non-severity directive spans are dropped entirely. A span that
            // happens to ride a severity line (e.g. an "error:" line) is the
            // documented residual; assert only the pure-directive spans vanish.
            let severity_riding = c.payload.lines().any(|line| {
                let l = line.to_ascii_lowercase();
                line.contains(span.as_str())
                    && ["error", "panic", "fail", "exception", "fatal"]
                        .iter()
                        .any(|m| l.contains(m))
            });
            if !severity_riding {
                assert!(
                    !out.contains(span.as_str()),
                    "log extractor leaked non-severity directive {span:?}"
                );
            }
        }
    }
}
