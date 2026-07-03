//! Context-pack compiler — the *Lost in the Middle* remedies as an API.
//!
//! The paper's U-curve means a smaller pack of unordered facts still buries the
//! decisive fact in the dead middle. So the compiler **enforces placement**, the
//! agent does not: critical facts at the top, then changed files, then
//! unchanged-but-relevant (summaries + handles), then recommended retrievals,
//! then a cost warning, then the critical facts **repeated at the tail**
//! (primacy + recency). It enforces a hard token ceiling (a pack can never
//! exceed [`MAX_PACK_TOKENS`]) and is deterministic for identical inputs, so an
//! unchanged pack re-serves byte-identically (cache-friendly stable prefix).
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

/// Default target pack size (tokens).
pub const DEFAULT_PACK_TOKENS: u32 = 1200;
/// Hard ceiling a pack may never exceed (tokens).
pub const MAX_PACK_TOKENS: u32 = 4000;
/// Tokens reserved for the duplicated critical-facts tail.
const CRITICAL_TAIL_CAP: u32 = 250;

fn est_tokens(s: &str) -> u32 {
    u32::try_from(s.chars().count() / 4)
        .unwrap_or(u32::MAX)
        .max(1)
}

/// A unit of pack content with its estimated token cost.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackItem {
    /// The rendered text.
    pub text: String,
    /// Estimated tokens.
    pub est_tokens: u32,
}

impl PackItem {
    /// Build an item, estimating its token cost from its text.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let est_tokens = est_tokens(&text);
        Self { text, est_tokens }
    }
}

/// The ordered section kinds (placement is part of the API contract).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SectionKind {
    /// Critical facts, at the top (primacy).
    CriticalTop,
    /// Changed files / hunks.
    Changed,
    /// Unchanged-but-relevant summaries + handles.
    Unchanged,
    /// Recommended retrievals (handles only).
    Recommended,
    /// Cost warning (awareness only).
    CostWarning,
    /// Critical facts repeated at the tail (recency).
    CriticalTail,
}

/// A compiled pack section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// The section kind.
    pub kind: SectionKind,
    /// The lines of the section.
    pub lines: Vec<String>,
}

/// The inputs to compile a pack.
#[derive(Clone, Debug, Default)]
pub struct PackRequest {
    /// Critical facts (duplicated top and tail).
    pub critical_facts: Vec<String>,
    /// Changed-file items.
    pub changed: Vec<PackItem>,
    /// Unchanged-but-relevant items (summaries + handles).
    pub unchanged: Vec<PackItem>,
    /// Recommended retrieval handles.
    pub recommended: Vec<String>,
    /// Optional cost warning line.
    pub cost_warning: Option<String>,
    /// The token budget (clamped to [`MAX_PACK_TOKENS`]).
    pub token_budget: u32,
}

/// A compiled, ordered, budgeted pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pack {
    /// Sections in placement order.
    pub sections: Vec<Section>,
    /// Total estimated tokens (≤ effective budget).
    pub total_tokens: u32,
}

impl Pack {
    /// The rendered pack text (for serving / snapshotting).
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        for s in &self.sections {
            for line in &s.lines {
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }
}

/// Compile a deterministic, edge-placed, budget-bounded pack.
///
/// The critical-facts top and tail are always included (truncated only if they
/// alone exceed budget). Changed → unchanged → recommended are filled greedily
/// within the remaining budget; anything that does not fit is dropped (its
/// content remains available by handle elsewhere). The result never exceeds
/// `min(request.token_budget, MAX_PACK_TOKENS)`.
#[must_use]
pub fn compile(req: &PackRequest) -> Pack {
    let budget = req.token_budget.clamp(1, MAX_PACK_TOKENS);
    let mut used: u32 = 0;
    let mut sections: Vec<Section> = Vec::new();

    // Reserve the tail up front so recency facts are never crowded out.
    let tail_lines: Vec<String> = req.critical_facts.clone();
    let tail_cost = tail_lines
        .iter()
        .map(|l| est_tokens(l))
        .sum::<u32>()
        .min(CRITICAL_TAIL_CAP);
    let reserve_tail = tail_cost.min(budget);

    // 1. critical facts top
    let mut top = Section {
        kind: SectionKind::CriticalTop,
        lines: Vec::new(),
    };
    for fact in &req.critical_facts {
        let cost = est_tokens(fact);
        if used + cost + reserve_tail <= budget {
            used += cost;
            top.lines.push(fact.clone());
        }
    }
    sections.push(top);

    // helper to fill a section greedily within remaining budget (minus tail reserve)
    let fill =
        |kind: SectionKind, items: &[PackItem], used: &mut u32, sections: &mut Vec<Section>| {
            let mut sec = Section {
                kind,
                lines: Vec::new(),
            };
            for item in items {
                if *used + item.est_tokens + reserve_tail <= budget {
                    *used += item.est_tokens;
                    sec.lines.push(item.text.clone());
                }
            }
            if !sec.lines.is_empty() {
                sections.push(sec);
            }
        };

    // 2. changed, 3. unchanged
    fill(SectionKind::Changed, &req.changed, &mut used, &mut sections);
    fill(
        SectionKind::Unchanged,
        &req.unchanged,
        &mut used,
        &mut sections,
    );

    // 4. recommended retrievals (handles are cheap; one est per handle)
    let rec_items: Vec<PackItem> = req
        .recommended
        .iter()
        .map(|h| PackItem::new(format!("retrieve: {h}")))
        .collect();
    fill(
        SectionKind::Recommended,
        &rec_items,
        &mut used,
        &mut sections,
    );

    // 5. cost warning
    if let Some(w) = &req.cost_warning {
        let cost = est_tokens(w);
        if used + cost + reserve_tail <= budget {
            used += cost;
            sections.push(Section {
                kind: SectionKind::CostWarning,
                lines: vec![w.clone()],
            });
        }
    }

    // 6. critical facts tail (duplicated for recency)
    if !tail_lines.is_empty() {
        used += reserve_tail;
        sections.push(Section {
            kind: SectionKind::CriticalTail,
            lines: tail_lines,
        });
    }

    Pack {
        sections,
        total_tokens: used.min(budget),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> PackRequest {
        PackRequest {
            critical_facts: vec!["CRITICAL: retry policy changed".into()],
            changed: vec![PackItem::new("src/retry.rs: 3 hunks +12/-4")],
            unchanged: vec![PackItem::new("BillingClient.charge() signature unchanged")],
            recommended: vec!["file:src/retry.rs@sha256:abc".into()],
            cost_warning: Some("this repo re-reads 22% of input tokens".into()),
            token_budget: 1200,
        }
    }

    #[test]
    fn sections_are_in_placement_order() {
        let pack = compile(&req());
        let kinds: Vec<_> = pack.sections.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            vec![
                SectionKind::CriticalTop,
                SectionKind::Changed,
                SectionKind::Unchanged,
                SectionKind::Recommended,
                SectionKind::CostWarning,
                SectionKind::CriticalTail,
            ]
        );
    }

    #[test]
    fn critical_facts_appear_top_and_tail() {
        let pack = compile(&req());
        let first = &pack.sections.first().unwrap();
        let last = &pack.sections.last().unwrap();
        assert_eq!(first.kind, SectionKind::CriticalTop);
        assert_eq!(last.kind, SectionKind::CriticalTail);
        assert_eq!(
            first.lines, last.lines,
            "critical facts duplicated at the edges"
        );
    }

    #[test]
    fn never_exceeds_budget() {
        let mut r = req();
        r.token_budget = 10; // tiny
        r.changed = (0..100)
            .map(|i| PackItem::new(format!("changed file {i} with lots of content here")))
            .collect();
        let pack = compile(&r);
        assert!(
            pack.total_tokens <= 10,
            "pack must not exceed budget: {}",
            pack.total_tokens
        );
    }

    #[test]
    fn never_exceeds_hard_ceiling() {
        let mut r = req();
        r.token_budget = 1_000_000; // absurd request
        r.changed = (0..5000)
            .map(|i| PackItem::new(format!("big changed item number {i} blah blah blah")))
            .collect();
        let pack = compile(&r);
        assert!(pack.total_tokens <= MAX_PACK_TOKENS);
    }

    #[test]
    fn deterministic_for_identical_inputs() {
        let a = compile(&req());
        let b = compile(&req());
        assert_eq!(a, b);
        assert_eq!(a.render(), b.render());
    }
}
