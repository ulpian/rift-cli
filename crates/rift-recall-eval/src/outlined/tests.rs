use super::*;
use crate::{evaluate_go_no_go, reconcile, Dimension, GaThresholds, Verdict};

/// What a line of source costs, the figure every read floor is derived at.
const TOKENS_PER_LINE: u32 = 4;

/// The budget a first-contact frame is held under: a frame that grows past it
/// stops being a map and starts being a page.
const FRAME_BUDGET_TOKENS: u32 = 400;

/// ★ The condition's claims on its corpus: the decisive fact always arrives,
/// no outcome dimension degrades, and the outlines stand in for more than
/// they and the narrower reads cost.
#[test]
fn the_outlined_condition_is_non_degraded_and_saves_tokens_on_its_corpus() {
    let runs: Vec<PairedRun> = outlined_corpus().iter().map(replay_outlined).collect();
    for run in &runs {
        assert!(run.baseline.success, "{}", run.name);
        assert!(
            run.delta.success,
            "{}: the fact survives the outline",
            run.name
        );
        assert!(run.delta.tokens < run.baseline.tokens, "{}", run.name);
    }
    if let Verdict::NoGo(dims) = evaluate_go_no_go(&GaThresholds::V1, &runs) {
        for degraded in [
            Dimension::SuccessDegraded,
            Dimension::TestsDegraded,
            Dimension::HumanModDegraded,
        ] {
            assert!(!dims.contains(&degraded), "{dims:?}");
        }
    }
}

/// The leg's own claim reconciles with what the replay measured, and never
/// claims more.
#[test]
fn the_outlined_claim_reconciles_with_what_the_replay_measured() {
    for scenario in outlined_corpus() {
        let rec = reconcile(&replay_outlined(&scenario));
        assert!(rec.within_tolerance && !rec.over_estimate, "{rec:?}");
    }
}

/// A file under both floors is read whole, never outlined.
#[test]
fn a_small_file_is_never_outlined() {
    let small = ScenarioFile {
        path: "src/small.rs".to_owned(),
        content: "fn a() {\n    1\n}\n".repeat(50),
        changed: false,
    };
    assert_eq!(outline_frame_for(&small), None);
}

/// The replay is a fixed function of its corpus.
#[test]
fn the_outlined_replay_is_deterministic() {
    for scenario in outlined_corpus() {
        assert_eq!(replay_outlined(&scenario), replay_outlined(&scenario));
    }
}

/// ★ The first-page floor is the outline frame's break-even, measured: the
/// frame at its definition cap, on long realistic names, counted with the
/// tokenizer every other figure is, at four tokens a line. A reword that
/// grows the frame reds this test, and the constant follows the frame; the
/// frame stays inside its budget.
#[test]
fn the_outline_frame_pays_for_itself_at_its_page_floor() {
    let names: Vec<String> = (0..20)
        .map(|at| format!("impl fmt::Display for LayoutModelColumn{at}"))
        .collect();
    let entries: Vec<OutlineEntry<'_>> = names
        .iter()
        .zip(1u32..)
        .map(|(name, at)| OutlineEntry {
            kind: rift_recall_serve::DefinitionKind::Impl,
            name,
            first_line: at * 30,
            last_line: at * 30 + 27,
            tokens: Some(200 + at),
        })
        .collect();
    let frame = render_outline_frame(&OutlinedRead {
        path: "crates/rift-canopy/src/layout_model.rs",
        hash: &"3f9a1c2e8b7d".repeat(6),
        lines: 637,
        bytes: 23_449,
        whole_tokens: Some(5_704),
        definitions: 20,
        markdown: false,
        entries: &entries,
        page_end: Some(200),
    })
    .unwrap();
    let tokens = rift_tokens::count(&frame);
    assert!(tokens < FRAME_BUDGET_TOKENS, "{tokens} tokens: {frame}");
    assert_eq!(
        rift_recall_serve::OUTLINE_FRAME_MIN_PAGE_LINES,
        tokens / TOKENS_PER_LINE + 1,
        "the frame at the cap costs {tokens} tokens"
    );
}

/// Every frame the condition renders on its corpus stays inside the budget.
#[test]
fn every_corpus_frame_stays_inside_the_budget() {
    for scenario in outlined_corpus() {
        for file in &scenario.files {
            let frame = outline_frame_for(file).expect("every corpus file is large");
            assert!(rift_tokens::count(&frame) < FRAME_BUDGET_TOKENS, "{frame}");
        }
    }
}
