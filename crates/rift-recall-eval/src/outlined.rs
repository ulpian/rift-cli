//! The **outlined condition**: a session's first reads of large files
//! answered with each file's bound outline, measured against the same reads
//! delivering the files whole.
//!
//! It asks the question the first-contact lever has to answer before it may
//! act: a model handed a map instead of a file has to make a second, narrower
//! read for anything it needs, so the outline saves tokens only where the map
//! and that narrower read together cost less than the file — and it must
//! never cost the fact.
//!
//! What is real here, and what is modelled:
//!
//! * **Real.** The outline — the shipped extractor's, of the scenario's own
//!   bytes; the token counts it is printed with — the real tokenizer, as the
//!   worker binds them; the frame — the one renderer that renders it in
//!   production; the floors — the shipped policy's.
//! * **Modelled.** Which definition the agent reads next. The scripted agent
//!   reads the range of the definition holding what it needs, and where no
//!   definition holds it, repeats the call for the whole file — the escape
//!   hatch, priced rather than assumed away. Choosing that definition from
//!   the names the frame lists is the premise, stated as one.

use rift_recall_policy::{DEFAULT_FIRST_CONTACT_MIN_BYTES, DEFAULT_FIRST_CONTACT_MIN_LINES};
use rift_recall_serve::{line_span, render_outline_frame, FileOutline, OutlineEntry, OutlinedRead};
use rift_recall_substrate::worktree::sha256_hex;

use crate::{PairedRun, RunMetrics, Scenario, ScenarioFile};

/// The condition's name in the report.
pub const CONDITION: &str = "outlined";

/// The corpus this condition runs on: the served corpus's four shapes, each
/// file grown to a large one of five definitions — the files the lever acts
/// on, which the served corpus's hundred-line files never are.
#[must_use]
pub fn outlined_corpus() -> Vec<Scenario> {
    [(10, 0), (20, 1), (40, 0), (15, 2)]
        .into_iter()
        .map(|(n, fact_pos)| outlined_scenario(n, fact_pos))
        .collect()
}

/// One scenario: `n` files of five hundred-line definitions, the decisive fact
/// inside the third definition of file `fact_pos`.
fn outlined_scenario(n: usize, fact_pos: usize) -> Scenario {
    let files = (0..n)
        .map(|i| {
            let mut content = format!("// file {i}\n");
            for part in 0..5 {
                content.push_str(&format!("fn part_{part}() {{\n"));
                for line in 0..98 {
                    if i == fact_pos && part == 2 && line == 40 {
                        content.push_str(
                            "    // THE_DECISIVE_FACT: retry policy moved to backoff.rs\n",
                        );
                    }
                    content.push_str(&format!("    let value_{line} = {line} * 42;\n"));
                }
                content.push_str("}\n");
            }
            ScenarioFile {
                path: format!("src/f{i}.rs"),
                content,
                changed: false,
            }
        })
        .collect();
    Scenario {
        name: format!("outlined_n{n}_pos{fact_pos}"),
        files,
        decisive_fact: "THE_DECISIVE_FACT".to_owned(),
        token_budget: 400,
    }
}

fn count(text: &str) -> u64 {
    u64::from(rift_tokens::count(text))
}

/// Whether a file is large enough for the lever, by the shipped floors.
fn is_large(file: &ScenarioFile) -> bool {
    let lines = u32::try_from(file.content.lines().count()).unwrap_or(u32::MAX);
    lines >= DEFAULT_FIRST_CONTACT_MIN_LINES
        || u64::try_from(file.content.len()).unwrap_or(u64::MAX) >= DEFAULT_FIRST_CONTACT_MIN_BYTES
}

/// The first-contact frame for one file, from its outline and the counts the
/// worker would bind, or `None` when the lever would let the read run.
#[must_use]
pub fn outline_frame_for(file: &ScenarioFile) -> Option<String> {
    if !is_large(file) {
        return None;
    }
    let outline = FileOutline::of(&file.path, file.content.as_bytes())?;
    let entries: Vec<OutlineEntry<'_>> = outline
        .definitions()
        .iter()
        .map(|d| OutlineEntry {
            kind: d.kind(),
            name: d.name(),
            first_line: d.first_line(),
            last_line: d.last_line(),
            tokens: Some(rift_tokens::count(line_span(
                &file.content,
                d.first_line(),
                d.last_line(),
            ))),
        })
        .collect();
    let hash = sha256_hex(file.content.as_bytes());
    render_outline_frame(&OutlinedRead {
        path: &file.path,
        hash: &hash,
        lines: outline.lines(),
        bytes: outline.bytes(),
        whole_tokens: Some(rift_tokens::count(&file.content)),
        definitions: u32::try_from(outline.definitions().len()).unwrap_or(u32::MAX),
        markdown: false,
        entries: &entries,
        page_end: None,
    })
}

/// What the agent reads after the frame when it needs something in the file:
/// the definition holding it, or — where none does — the whole file again.
fn narrower_read(file: &ScenarioFile, needle: &str) -> String {
    let Some(outline) = FileOutline::of(&file.path, file.content.as_bytes()) else {
        return file.content.clone();
    };
    outline
        .definitions()
        .iter()
        .map(|d| line_span(&file.content, d.first_line(), d.last_line()))
        .find(|span| span.contains(needle))
        .map_or_else(|| file.content.clone(), str::to_owned)
}

/// The status quo: every file read whole on first contact.
fn baseline_leg(scenario: &Scenario) -> RunMetrics {
    let tokens: u64 = scenario.files.iter().map(|f| count(&f.content)).sum();
    let found = scenario
        .files
        .iter()
        .any(|f| f.content.contains(&scenario.decisive_fact));
    RunMetrics {
        tokens,
        cost: tokens,
        success: found,
        tests_passed: found,
        human_mod_ok: true,
        latency_steps: u32::try_from(scenario.files.len()).unwrap_or(u32::MAX),
    }
}

/// The outlined leg, and the gross figures its decisions book: what the
/// outlines stood in for, and what they cost.
fn outlined_leg(scenario: &Scenario) -> (RunMetrics, u64, u64) {
    let (mut tokens, mut steps, mut avoided, mut returned) = (0u64, 0u32, 0u64, 0u64);
    let mut view = String::new();
    for file in &scenario.files {
        steps = steps.saturating_add(1);
        let Some(frame) = outline_frame_for(file) else {
            tokens = tokens.saturating_add(count(&file.content));
            view.push_str(&file.content);
            continue;
        };
        tokens = tokens.saturating_add(count(&frame));
        returned = returned.saturating_add(count(&frame));
        avoided = avoided.saturating_add(count(&file.content));
        if file.content.contains(&scenario.decisive_fact) {
            let more = narrower_read(file, &scenario.decisive_fact);
            steps = steps.saturating_add(1);
            tokens = tokens.saturating_add(count(&more));
            avoided = avoided.saturating_sub(count(&more));
            view.push_str(&more);
        }
    }
    let found = view.contains(&scenario.decisive_fact);
    let metrics = RunMetrics {
        tokens,
        cost: tokens,
        success: found,
        tests_passed: found,
        human_mod_ok: true,
        latency_steps: steps,
    };
    (metrics, avoided, returned)
}

/// Replay one scenario as the outlined condition, paired against the same
/// first reads delivering every file whole.
///
/// The emitted claim is avoided net of the frames, with the narrower reads
/// the outlines sent the agent to taken out of what was avoided — the figure
/// [`crate::reconcile`] holds against the measured `baseline − delta`.
#[must_use]
pub fn replay_outlined(scenario: &Scenario) -> PairedRun {
    let baseline = baseline_leg(scenario);
    let (delta, avoided, returned) = outlined_leg(scenario);
    PairedRun {
        name: format!("{CONDITION}_{}", scenario.name),
        baseline,
        delta,
        emitted_tokens_avoided: avoided.saturating_sub(returned),
    }
}

#[cfg(test)]
mod tests;
