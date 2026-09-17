//! Compiling a window's facts into a **budgeted evidence pack** — the shape
//! `rift dispatch --for-agent` emits instead of prose, building on the recall
//! plane's landed pack kinds.
//!
//! # Nothing here decides a pack's vocabulary
//!
//! `PackKind`, the per-kind layout table, the section vocabulary and the
//! placement invariants all landed with item 5 and are not re-decided here.
//! This module's only job is to hand facts to sealed constructors: every line
//! in the compiled pack exists because a `SectionEntry::*` constructor produced
//! it, which is what makes "no un-neutralized content can be in a pack" a
//! property of `rift-recall-pack` rather than a promise made here.
//!
//! # Why a dispatch pack is a REVIEW pack
//!
//! Dispatch `--for-agent` is "the same window, as evidence an agent can act on",
//! and the facts it holds — a volume scope, an attribution split, token spend,
//! recall savings, sensitive-path criticals — are *exactly* the Review kind's
//! sealed section vocabulary. Minting a fourth kind for the same facts would
//! give one artifact two names and two layout tables to keep in step.
//!
//! # The route to the pack crate
//!
//! Reached through `rift_recall_serve::pack`, not by a direct dependency: the
//! dispatcher is already the pack crate's caller, so the re-export adds no
//! crate edge (the identical reasoning, and the identical route, as the trust
//! vocabulary `bins/rift` already reads through that crate).

use rift_core::{Result, RiftError};
use rift_recall_serve::pack::{
    compile, CriticalFact, EvidenceRef, PackKeyInputs, PackKind, PackRequest, ReviewScopeFact,
    ReviewSignal, SectionEntry, DEFAULT_REVIEW_PACK_TOKENS, MAX_PACK_TOKENS,
};

use crate::workflows::{FactsPolicy, Window, WindowFacts};

/// Compile `facts` into a Review-kind evidence pack.
///
/// Returns the rendered pack text (what a human or an agent reads) and its
/// serialized [`EvidencePack`](rift_recall_serve::pack::EvidencePack) as JSON.
///
/// # Errors
/// [`RiftError::Validity`] if a fact is offered to a section that does not
/// admit it — which cannot happen for the fixed set below, and is propagated
/// rather than unwrapped because a placement invariant is exactly the thing
/// that must never be silently swallowed.
pub(crate) fn dispatch_pack(
    window: &Window,
    facts: &WindowFacts,
) -> Result<(String, serde_json::Value)> {
    let mut req = PackRequest::for_kind(PackKind::Review, DEFAULT_REVIEW_PACK_TOKENS);

    // Criticals first — the facts that must survive every degradation pass,
    // and the only ones the layout duplicates at both edges.
    //
    // The sensitive files are selected by WALKING the ordinal pass rather than
    // by looking each `sensitive_paths` entry up in it: the ordinal is then the
    // position by construction, so there is no arm in which a critical fact is
    // dropped for want of a number, and none in which it cites the wrong one.
    for f in facts.evidence().iter().filter(|f| f.sensitive) {
        req.push_critical(SectionEntry::critical(
            &CriticalFact::SensitivePathTouched {
                evidence: EvidenceRef::at(f.ordinal),
            },
        ))?;
    }
    if let Some(milli) = facts.attribution.confidence_milli {
        // A low-confidence attribution is a critical fact rather than a signal:
        // an agent that reads the split without knowing how much to trust it
        // has been told something more misleading than nothing.
        if milli < 700 {
            let percent = u8::try_from(milli / 10).unwrap_or(100);
            req.push_critical(SectionEntry::critical(
                &CriticalFact::LowAttributionConfidence { percent },
            ))?;
        }
    }

    req.push_review_scope(SectionEntry::review_scope(&ReviewScopeFact::Volume {
        commits: clamp_u32(facts.checkpoints.len()),
        sessions: clamp_u32(facts.sessions.len()),
        files: clamp_u32(facts.files.len()),
    }))?;

    let a = &facts.attribution;
    req.push_review_signal(SectionEntry::review_signal(
        ReviewSignal::AttributionSplit {
            ai_lines: clamp_u32_i64(a.agent_lines),
            mixed_lines: clamp_u32_i64(a.mixed_lines),
            human_lines: clamp_u32_i64(a.human_lines),
        },
    ))?;
    if let Some(milli) = a.confidence_milli {
        req.push_review_signal(SectionEntry::review_signal(
            ReviewSignal::AttributionConfidence {
                percent: u8::try_from(milli / 10).unwrap_or(100),
            },
        ))?;
    }
    // `u64`, not a saturating `u32`: this repository's cache-read total already
    // exceeds `u32::MAX`, and a saturated number rendered as exact is a lie the
    // pack has no way to mark.
    req.push_review_signal(SectionEntry::review_signal(ReviewSignal::TokenSpend {
        input: widen(facts.tokens.uncached_input),
        output: widen(facts.tokens.output),
        cache_read: widen(facts.tokens.cached_input),
        cache_write: widen(facts.tokens.cache_creation),
    }))?;
    if let Some(s) = facts.savings {
        req.push_review_signal(SectionEntry::review_signal(ReviewSignal::RecallSavings {
            tokens_avoided: clamp_u32_i64(s.avoided_upper_bound),
            tokens_injected: clamp_u32_i64(s.injected),
        }))?;
    }

    // `turn_remaining` is the per-turn ceiling a serve would have to fit inside.
    // A CLI invocation has no turn, so the ceiling is the shared maximum — the
    // pack is still clamped by its own budget, which is the smaller number.
    let pack = compile(&req, MAX_PACK_TOKENS).map_err(RiftError::from)?;
    let inputs = key_inputs(window, facts);
    let evidence = pack.to_evidence(&inputs);
    let json = serde_json::to_value(&evidence).map_err(|e| RiftError::Serde(e.to_string()))?;
    Ok((pack.render(), json))
}

/// The pack's identity inputs.
///
/// The `ranked` list is the window's checkpoints in the order the report walks
/// them, each paired with its own id — deterministic, and it makes the key a
/// function of *which window* rather than of when the report ran. `query` names
/// the verb and the resolved bounds, so two different windows can never share a
/// key.
fn key_inputs(window: &Window, facts: &WindowFacts) -> PackKeyInputs {
    PackKeyInputs {
        query: format!("dispatch:{}", window.key_text()),
        ranked: facts
            .checkpoints
            .iter()
            .map(|c| (c.checkpoint_id.clone(), c.checkpoint_id.clone()))
            .collect(),
        policy_version: 1,
        token_budget: DEFAULT_REVIEW_PACK_TOKENS,
    }
}

/// Counts are `usize` on this side of the seam and `u32` on the pack's; a
/// saturating conversion reports "very many" rather than wrapping to a small
/// number, which is the only failure mode that would be silent.
fn clamp_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// A stored `i64` count as the `u64` the pack's token signal takes. Negative
/// is impossible for a token total and becomes zero rather than a huge number.
fn widen(n: i64) -> u64 {
    u64::try_from(n).unwrap_or(0)
}

/// The same, for the sums that arrive as `i64` out of SQL.
fn clamp_u32_i64(n: i64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflows::{AttributionRollup, CheckpointFact, Savings, TokenClasses};

    fn cp(id: &str, at: i64) -> CheckpointFact {
        CheckpointFact {
            checkpoint_id: id.to_owned(),
            at: Some(at),
            derived: false,
        }
    }

    fn facts() -> WindowFacts {
        WindowFacts {
            checkpoints: vec![cp("c1", 10), cp("c2", 20)],
            files: vec![("src/a.rs".to_owned(), 2)],
            attribution: AttributionRollup {
                agent_lines: 40,
                mixed_lines: 5,
                human_lines: 10,
                agent_removed_by_human: 1,
                confidence_milli: Some(880),
            },
            tokens: TokenClasses {
                uncached_input: 100,
                cached_input: 200,
                cache_creation: 30,
                output: 50,
            },
            savings: Some(Savings {
                avoided_upper_bound: 900,
                injected: 100,
                returned: 20,
            }),
            ..WindowFacts::default()
        }
    }

    #[test]
    fn a_dispatch_pack_is_a_review_pack_and_stays_inside_the_ceiling() {
        let w = Window::resolve(Some("2026-08-01"), Some("2026-08-17")).unwrap();
        let (rendered, json) = dispatch_pack(&w, &facts()).unwrap();
        assert_eq!(json["kind"], "review");
        assert_eq!(json["schema_version"], 1);
        let total = json["total_tokens"].as_u64().unwrap_or(u64::MAX);
        assert!(
            total <= u64::from(MAX_PACK_TOKENS),
            "a pack may never exceed the shared ceiling: {total}"
        );
        assert!(!rendered.is_empty());
    }

    #[test]
    fn the_same_window_and_facts_compile_to_the_same_pack_key() {
        let w = Window::resolve(Some("2026-08-01"), Some("2026-08-17")).unwrap();
        let a = dispatch_pack(&w, &facts()).unwrap();
        let b = dispatch_pack(&w, &facts()).unwrap();
        assert_eq!(a.0, b.0, "the rendered pack is a function of its inputs");
        assert_eq!(a.1, b.1);

        // …and a different window is a different pack, so the key identifies
        // the question and not just the answer.
        let other = Window::resolve(Some("2026-08-02"), Some("2026-08-17")).unwrap();
        let c = dispatch_pack(&other, &facts()).unwrap();
        assert_ne!(a.1["pack_key"], c.1["pack_key"]);
    }

    #[test]
    fn a_sensitive_path_is_a_critical_fact_not_a_signal() {
        let w = Window::resolve(None, None).unwrap();
        let mut f = facts();
        // Sensitive paths are a SUBSET of the touched files in production
        // (`sensitive_touched` filters `files`), so the fixture holds the same
        // shape — it is what gives `.env` an ordinal to be cited by.
        f.files.push((".env".to_owned(), 1));
        f.sensitive_paths = Some(vec![".env".to_owned()]);
        let (rendered, json) = dispatch_pack(&w, &f).unwrap();
        // **BY ORDINAL, NEVER BY NAME.** `files` is sorted by touch
        // count then path, so `.env` (1 touch) follows `src/a.rs` (2).
        assert!(rendered.contains("evidence:2"), "got: {rendered}");
        assert!(
            !rendered.contains(".env"),
            "a path reached an agent-facing pack: {rendered}"
        );
        assert!(
            !serde_json::to_string(&json)
                .unwrap_or_default()
                .contains(".env"),
            "a path reached the serialized pack: {json}"
        );
        let sections: Vec<String> = json["sections"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["section"].as_str().unwrap_or_default().to_owned())
            .collect();
        assert!(
            sections.iter().any(|s| s.contains("critical")),
            "a sensitive path must ride the critical edges: {sections:?}"
        );
    }

    /// A hostile repository filename reaches a pack only through a sealed
    /// constructor, which neutralizes it — the property the whole pack spine
    /// exists to hold, asserted from the caller's side because that is where a
    /// future caller would try to bypass it.
    #[test]
    fn a_hostile_path_in_a_pack_is_neutralized_by_the_sealed_constructor() {
        let w = Window::resolve(None, None).unwrap();
        let mut f = facts();
        let hostile = "\u{1b}[2J\u{1b}]0;pwned\u{7}\u{202e}.env";
        f.files.push((hostile.to_owned(), 1));
        f.sensitive_paths = Some(vec![hostile.to_owned()]);
        let (rendered, json) = dispatch_pack(&w, &f).unwrap();
        let wire = serde_json::to_string(&json).unwrap_or_default();
        for needle in ["\u{1b}", "\u{7}", "\u{202e}"] {
            assert!(
                !rendered.contains(needle),
                "a control sequence reached the rendered pack"
            );
            assert!(
                !wire.contains(needle),
                "a control sequence reached the serialized pack"
            );
        }
    }
}
