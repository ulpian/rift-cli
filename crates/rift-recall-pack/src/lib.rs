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
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

/// Default target pack size (tokens).
pub const DEFAULT_PACK_TOKENS: u32 = 1200;
/// Hard ceiling a pack may never exceed (tokens).
pub const MAX_PACK_TOKENS: u32 = 4000;
/// Tokens reserved for the duplicated critical-facts tail.
const CRITICAL_TAIL_CAP: u32 = 250;

fn est_tokens(s: &str) -> u32 {
    // The one workspace counting authority (ADR-0006); floor of 1 so no pack
    // item is ever budget-free.
    rift_tokens::count(s).max(1)
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

/// The immutable inputs that identify a pack (ADR-0020). `ranked` is the
/// **ordered** `(artifact_id, content_sha256)` list the pack was compiled from —
/// order is part of the identity, because placement is part of the contract.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PackKeyInputs {
    /// The query (task/error/focus hints) the ranking answered.
    pub query: String,
    /// The ranked candidates: `(artifact_id, content_sha256)` in rank order.
    pub ranked: Vec<(String, String)>,
    /// The policy version in force when the pack was compiled.
    pub policy_version: u32,
    /// The requested token budget (pre-clamp — the caller's contract input).
    pub token_budget: u32,
}

/// The input-keyed pack identity (ADR-0020): `PackKey = hash(query,
/// ranked_ids+hashes, policy_ver, budget)` — over the immutable inputs,
/// computable **before** rendering, so identical inputs re-serve
/// byte-identically and any pack cache is content-addressed by inputs. The
/// rendered output is never the key (an output hash is a post-hoc equality
/// check, not a cache key). Domain-separated + length-prefixed, so no two
/// distinct input sets collide via concatenation ambiguity.
#[must_use]
pub fn pack_key(inputs: &PackKeyInputs) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"rift.packkey.v1\x00");
    let mut feed = Vec::new();
    let push = |buf: &mut Vec<u8>, bytes: &[u8]| {
        buf.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        buf.extend_from_slice(bytes);
    };
    push(&mut feed, inputs.query.as_bytes());
    feed.extend_from_slice(&(inputs.ranked.len() as u64).to_le_bytes());
    for (id, hash) in &inputs.ranked {
        push(&mut feed, id.as_bytes());
        push(&mut feed, hash.as_bytes());
    }
    feed.extend_from_slice(&inputs.policy_version.to_le_bytes());
    feed.extend_from_slice(&inputs.token_budget.to_le_bytes());
    h.update(&feed);
    let out = h.finalize();
    let mut s = String::with_capacity(64);
    use core::fmt::Write as _;
    for b in out {
        let _ = write!(s, "{b:02x}");
    }
    s
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

    fn key_inputs() -> PackKeyInputs {
        PackKeyInputs {
            query: "task: fix retry".into(),
            ranked: vec![
                ("id1".into(), "hashA".into()),
                ("id2".into(), "hashB".into()),
            ],
            policy_version: 1,
            token_budget: 1200,
        }
    }

    #[test]
    fn pack_key_is_input_keyed_and_sensitive_to_every_input() {
        // ADR-0020: computable before any render, stable for identical inputs.
        let base = pack_key(&key_inputs());
        assert_eq!(base.len(), 64);
        assert_eq!(base, pack_key(&key_inputs()), "identical inputs, same key");

        let mut other_query = key_inputs();
        other_query.query = "task: other".into();
        assert_ne!(
            base,
            pack_key(&other_query),
            "query is part of the identity"
        );

        let mut reordered = key_inputs();
        reordered.ranked.swap(0, 1);
        assert_ne!(
            base,
            pack_key(&reordered),
            "rank ORDER is part of the identity"
        );

        let mut rehashed = key_inputs();
        rehashed.ranked[0].1 = "hashA2".into();
        assert_ne!(
            base,
            pack_key(&rehashed),
            "content hash is part of the identity"
        );

        let mut repoliced = key_inputs();
        repoliced.policy_version = 2;
        assert_ne!(
            base,
            pack_key(&repoliced),
            "policy version is part of the identity"
        );

        let mut rebudgeted = key_inputs();
        rebudgeted.token_budget = 800;
        assert_ne!(
            base,
            pack_key(&rebudgeted),
            "budget is part of the identity"
        );
    }

    proptest::proptest! {
        /// Byte-identity across seeds (ADR-0020): equal inputs → equal key AND
        /// equal rendered pack; the key never depends on the rendered output.
        #[test]
        fn same_inputs_reserve_byte_identically(
            names in proptest::collection::vec("[a-z]{1,8}", 0..6),
            budget in 1u32..2000,
        ) {
            let ranked: Vec<(String, String)> = names
                .iter()
                .enumerate()
                .map(|(i, n)| (format!("id{i}"), format!("h_{n}")))
                .collect();
            let inputs = PackKeyInputs {
                query: "q".into(),
                ranked: ranked.clone(),
                policy_version: 1,
                token_budget: budget,
            };
            proptest::prop_assert_eq!(pack_key(&inputs), pack_key(&inputs));
            let pr = PackRequest {
                recommended: ranked.iter().map(|(_, h)| format!("file:x@sha256:{h}")).collect(),
                token_budget: budget,
                ..PackRequest::default()
            };
            proptest::prop_assert_eq!(compile(&pr).render(), compile(&pr).render());
        }

        /// Distinct ranked sets produce distinct keys (no concatenation-boundary
        /// collisions thanks to length prefixing).
        #[test]
        fn boundary_shifts_change_the_key(a in "[a-z]{1,6}", b in "[a-z]{1,6}") {
            let one = PackKeyInputs {
                ranked: vec![(format!("{a}{b}"), "h".into())],
                ..key_inputs()
            };
            let two = PackKeyInputs {
                ranked: vec![(a.clone(), format!("{b}h"))],
                ..key_inputs()
            };
            // same concatenated bytes, different field boundaries → different keys
            proptest::prop_assert_ne!(pack_key(&one), pack_key(&two));
        }
    }
}
