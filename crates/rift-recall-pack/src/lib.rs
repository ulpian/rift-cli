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
//!
//! Sections are **trust-shaped by construction** (ADR-0016): they hold sealed
//! [`SectionEntry`] values buildable only from the serve constructors' outputs
//! (see [`entry`]), and [`PackRequest`]'s per-section admission checks make a
//! placement-violating pack unrepresentable — `UnchangedButRelevant` can never
//! hold exact content, recommendations are parse-validated handles only, and
//! the cost warning comes from a closed descriptive vocabulary.
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

mod entry;

pub use entry::{CostNote, CriticalFact, EntryClass, SectionEntry, Sensitivity, COST_NOTE};
use rift_core::{Result, RiftError};

/// Default target pack size (tokens).
pub const DEFAULT_PACK_TOKENS: u32 = 1200;
/// Hard ceiling a pack may never exceed (tokens).
pub const MAX_PACK_TOKENS: u32 = 4000;
/// Ceiling for the duplicated critical-facts tail (P6 S2 anchor band).
const CRITICAL_TAIL_CAP: u32 = 250;
/// Ceiling reserved for the critical-facts top anchor.
const ANCHOR_TOP_CAP: u32 = 250;
/// The tail's graceful-degradation floor: a tail CUT below this is dropped
/// whole rather than served as a rump (PRD story 2: 100–250 tokens).
pub const TAIL_FLOOR_TOKENS: u32 = 100;

/// Why a pack could not be compiled (fail-loud, PRD story 7: truncation is
/// visible, never silent).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PackError {
    /// The effective cap (`min(budget, hard ceiling, turn remaining)`) cannot
    /// hold even a minimal coherent pack.
    BudgetTooSmall {
        /// The cheapest coherent content the request carries.
        needed: u32,
        /// The effective cap that could not hold it.
        cap: u32,
    },
}

impl core::fmt::Display for PackError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::BudgetTooSmall { needed, cap } => write!(
                f,
                "pack budget too small: cap {cap} tokens cannot hold minimal content ({needed})"
            ),
        }
    }
}

impl core::error::Error for PackError {}

impl From<PackError> for RiftError {
    fn from(e: PackError) -> Self {
        Self::validity(e.to_string())
    }
}

pub(crate) fn est_tokens(s: &str) -> u32 {
    // The one workspace counting authority (ADR-0006); floor of 1 so no pack
    // item is ever budget-free.
    rift_tokens::count(s).max(1)
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

/// A compiled pack section (the render view; content came from sealed entries).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// The section kind.
    pub kind: SectionKind,
    /// The rendered lines of the section.
    pub lines: Vec<String>,
}

/// Which entry classes a section admits — the placement invariants (PRD-M6
/// stories 4/5/6) as data. `Unchanged` NEVER admits exact content (honest-cache:
/// unchanged content is summaries + handles + hashes only); recommendations are
/// handles only; the cost slot takes only the closed [`CostNote`] vocabulary.
const fn admits(kind: SectionKind, class: EntryClass) -> bool {
    match kind {
        SectionKind::CriticalTop | SectionKind::CriticalTail => {
            matches!(class, EntryClass::CriticalFact)
        }
        SectionKind::Changed => matches!(
            class,
            EntryClass::ExactContent
                | EntryClass::SummaryBound
                | EntryClass::Handle
                | EntryClass::NeutralDigest
                | EntryClass::KnownUnchanged
        ),
        SectionKind::Unchanged => matches!(
            class,
            EntryClass::KnownUnchanged
                | EntryClass::SummaryBound
                | EntryClass::Handle
                | EntryClass::NeutralDigest
        ),
        SectionKind::Recommended => matches!(class, EntryClass::Handle),
        SectionKind::CostWarning => matches!(class, EntryClass::CostNote),
    }
}

/// The inputs to compile a pack. Fields are private and entries are admitted
/// through the per-section methods below, each of which enforces its section's
/// placement invariant — so an invariant-violating request never exists.
#[derive(Clone, Debug, Default)]
pub struct PackRequest {
    critical: Vec<SectionEntry>,
    changed: Vec<SectionEntry>,
    unchanged: Vec<SectionEntry>,
    recommended: Vec<SectionEntry>,
    cost_note: Option<SectionEntry>,
    token_budget: u32,
}

fn refuse(kind: SectionKind, class: EntryClass) -> RiftError {
    RiftError::validity(format!(
        "placement invariant: {kind:?} does not admit {class:?} entries"
    ))
}

impl PackRequest {
    /// A request targeting `token_budget` tokens (clamped at compile time).
    #[must_use]
    pub fn new(token_budget: u32) -> Self {
        Self {
            token_budget,
            ..Self::default()
        }
    }

    /// Admit an edge-placed critical fact (duplicated top and tail).
    ///
    /// # Errors
    /// [`RiftError::Validity`] unless the entry is a [`EntryClass::CriticalFact`].
    pub fn push_critical(&mut self, e: SectionEntry) -> Result<()> {
        if !admits(SectionKind::CriticalTop, e.class()) {
            return Err(refuse(SectionKind::CriticalTop, e.class()));
        }
        self.critical.push(e);
        Ok(())
    }

    /// Admit a changed-file entry (exact hunk content, summary, handle, or a
    /// neutral digest of untrusted provenance).
    ///
    /// # Errors
    /// [`RiftError::Validity`] if the class is not admissible in `Changed`.
    pub fn push_changed(&mut self, e: SectionEntry) -> Result<()> {
        if !admits(SectionKind::Changed, e.class()) {
            return Err(refuse(SectionKind::Changed, e.class()));
        }
        self.changed.push(e);
        Ok(())
    }

    /// Admit an unchanged-but-relevant entry. **Exact content is refused** —
    /// unchanged context is summaries + handles + hashes only (PRD story 5).
    ///
    /// # Errors
    /// [`RiftError::Validity`] if the class is not admissible in `Unchanged`.
    pub fn push_unchanged(&mut self, e: SectionEntry) -> Result<()> {
        if !admits(SectionKind::Unchanged, e.class()) {
            return Err(refuse(SectionKind::Unchanged, e.class()));
        }
        self.unchanged.push(e);
        Ok(())
    }

    /// Admit a recommended retrieval (parse-validated handles only).
    ///
    /// # Errors
    /// [`RiftError::Validity`] unless the entry is a [`EntryClass::Handle`].
    pub fn push_recommended(&mut self, e: SectionEntry) -> Result<()> {
        if !admits(SectionKind::Recommended, e.class()) {
            return Err(refuse(SectionKind::Recommended, e.class()));
        }
        self.recommended.push(e);
        Ok(())
    }

    /// Set the descriptive cost note (closed vocabulary; directive-free).
    ///
    /// # Errors
    /// [`RiftError::Validity`] unless the entry is a [`EntryClass::CostNote`].
    pub fn set_cost_note(&mut self, e: SectionEntry) -> Result<()> {
        if !admits(SectionKind::CostWarning, e.class()) {
            return Err(refuse(SectionKind::CostWarning, e.class()));
        }
        self.cost_note = Some(e);
        Ok(())
    }
}

/// A compiled, ordered, budgeted pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pack {
    /// Sections in placement order.
    pub sections: Vec<Section>,
    /// Total tokens, **measured post-render over the serialized text** (never
    /// a sum of per-item estimates); ≤ the effective cap.
    pub total_tokens: u32,
    /// The tokenizer that measured `total_tokens` ("skeptical buyer"
    /// provenance, ADR-0006).
    pub tokenizer: &'static str,
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

/// Compile a deterministic, edge-placed, budget-bounded pack (P6 S2).
///
/// The effective cap is `min(token_budget, MAX_PACK_TOKENS, turn_remaining)` —
/// a pack can never blow the per-turn ceiling (PRD story 9). Anchors are
/// reserved first (top ≤ 250 tokens; tail 100–250 with graceful degradation:
/// a tail CUT below [`TAIL_FLOOR_TOKENS`] is dropped whole). Sections fill in
/// rank order; an inline entry that does not fit **spills to its handle**
/// (never a truncated slice — sensitive-safe, PRD story 6). The total is then
/// **re-measured over the rendered text** and, while it exceeds the cap,
/// content degrades in reverse-rank order (lowest-priority section first,
/// last entry first: spill, then drop), then the tail shrinks, then the top
/// truncates — a strictly-shrinking, terminating loop (PRD story 8).
///
/// # Errors
/// [`PackError::BudgetTooSmall`] when the cap cannot hold even the request's
/// cheapest coherent content — fail-loud, never a silently empty pack
/// (PRD story 7; the documented caller fallback is `changed_hunks` directly).
pub fn compile(req: &PackRequest, turn_remaining: u32) -> core::result::Result<Pack, PackError> {
    let cap = req
        .token_budget
        .clamp(1, MAX_PACK_TOKENS)
        .min(turn_remaining);

    // Fail-loud floor: the cheapest thing this request wants to say must fit.
    // (An entirely empty request compiles to an empty pack — nothing to hold.)
    let cheapest = req
        .critical
        .first()
        .map(SectionEntry::est_tokens)
        .into_iter()
        .chain(
            [&req.changed, &req.unchanged, &req.recommended]
                .into_iter()
                .flat_map(|v| v.iter().map(SectionEntry::est_tokens)),
        )
        .chain(req.cost_note.iter().map(SectionEntry::est_tokens))
        .min();
    match cheapest {
        None => {
            return Ok(Pack {
                sections: Vec::new(),
                total_tokens: 0,
                tokenizer: rift_tokens::provenance(),
            })
        }
        Some(needed) if needed > cap => {
            return Err(PackError::BudgetTooSmall { needed, cap });
        }
        Some(_) => {}
    }

    // 1) Top anchor: critical facts in order, within the anchor band and cap.
    let anchor_band = ANCHOR_TOP_CAP.min(cap);
    let mut top: Vec<SectionEntry> = Vec::new();
    let mut top_cost = 0u32;
    for fact in &req.critical {
        if top_cost + fact.est_tokens() <= anchor_band {
            top_cost += fact.est_tokens();
            top.push(fact.clone());
        }
    }

    // 2) Tail reserve: the top anchors repeat within the 100–250 band.
    let tail_full: u32 = top.iter().map(SectionEntry::est_tokens).sum();
    let reserve_tail = tail_full
        .min(CRITICAL_TAIL_CAP)
        .min(cap.saturating_sub(top_cost));

    // 3) Fill by rank within the estimate budget, spilling to handles.
    let est_room = cap.saturating_sub(top_cost + reserve_tail);
    let mut est_used = 0u32;
    let mut fill = |items: &[SectionEntry]| -> Vec<SectionEntry> {
        let mut out = Vec::new();
        for item in items {
            if est_used + item.est_tokens() <= est_room {
                est_used += item.est_tokens();
                out.push(item.clone());
            } else if let Some(spilled) = item.spill() {
                if est_used + spilled.est_tokens() <= est_room {
                    est_used += spilled.est_tokens();
                    out.push(spilled);
                }
            }
        }
        out
    };
    let mut changed = fill(&req.changed);
    let mut unchanged = fill(&req.unchanged);
    let mut recommended = fill(&req.recommended);
    let mut cost_note = req.cost_note.clone().filter(|w| {
        let fits = est_used + w.est_tokens() <= est_room;
        if fits {
            est_used += w.est_tokens();
        }
        fits
    });

    // 4) Tail: the longest top-prefix within the reserve; a CUT tail below
    //    the floor is dropped whole (deterministic graceful degradation).
    let mut tail = materialize_tail(&top, reserve_tail);

    // 5) Post-render re-measure (over the serialized text, never estimate
    //    sums) + reverse-rank degrade until the measurement fits the cap.
    let mut measured;
    loop {
        let pack = assemble(&top, &changed, &unchanged, &recommended, &cost_note, &tail);
        measured = rift_tokens::count(&pack.render());
        if measured <= cap {
            return Ok(Pack {
                total_tokens: measured,
                ..pack
            });
        }
        // Reverse-rank: lowest-priority section first, last entry first;
        // spill an inline entry if that shrinks it, else drop it outright.
        let degraded = degrade_last(&mut recommended)
            || cost_note.take().is_some()
            || degrade_last(&mut unchanged)
            || degrade_last(&mut changed)
            || shrink_tail(&mut tail)
            || (top.len() > 1 && top.pop().is_some());
        if !degraded {
            // Nothing left but the minimal anchor and it still measures over
            // the cap (estimate/measure divergence corner): fail loud.
            return Err(PackError::BudgetTooSmall {
                needed: measured,
                cap,
            });
        }
    }
}

/// The longest prefix of the top anchors fitting `reserve`; a CUT prefix
/// below [`TAIL_FLOOR_TOKENS`] degrades to no tail at all (never a rump).
fn materialize_tail(top: &[SectionEntry], reserve: u32) -> Vec<SectionEntry> {
    let mut tail: Vec<SectionEntry> = Vec::new();
    let mut cost = 0u32;
    for fact in top {
        if cost + fact.est_tokens() > reserve {
            break;
        }
        cost += fact.est_tokens();
        tail.push(fact.clone());
    }
    if tail.len() < top.len() && cost < TAIL_FLOOR_TOKENS {
        tail.clear();
    }
    tail
}

/// Degrade a section's LAST entry: swap it for its cheaper handle spill when
/// it has one, else remove it. `true` if anything changed.
fn degrade_last(entries: &mut Vec<SectionEntry>) -> bool {
    let Some(last) = entries.last() else {
        return false;
    };
    match last.spill() {
        Some(spilled) if spilled.est_tokens() < last.est_tokens() => {
            if let Some(slot) = entries.last_mut() {
                *slot = spilled;
            }
            true
        }
        _ => entries.pop().is_some(),
    }
}

/// Shrink the tail by one fact from the end; dropping below the floor (when
/// the tail no longer mirrors the full top) clears it whole.
fn shrink_tail(tail: &mut Vec<SectionEntry>) -> bool {
    if tail.is_empty() {
        return false;
    }
    tail.pop();
    let cost: u32 = tail.iter().map(SectionEntry::est_tokens).sum();
    if !tail.is_empty() && cost < TAIL_FLOOR_TOKENS {
        tail.clear();
    }
    true
}

/// Assemble the ordered sections from the surviving entries (placement is the
/// compiler's, not the caller's).
fn assemble(
    top: &[SectionEntry],
    changed: &[SectionEntry],
    unchanged: &[SectionEntry],
    recommended: &[SectionEntry],
    cost_note: &Option<SectionEntry>,
    tail: &[SectionEntry],
) -> Pack {
    let mut sections: Vec<Section> = Vec::new();
    let mut push = |kind: SectionKind, entries: &[SectionEntry]| {
        let lines: Vec<String> = entries.iter().map(|e| e.text().to_string()).collect();
        if kind == SectionKind::CriticalTop || !lines.is_empty() {
            sections.push(Section { kind, lines });
        }
    };
    push(SectionKind::CriticalTop, top);
    push(SectionKind::Changed, changed);
    push(SectionKind::Unchanged, unchanged);
    push(SectionKind::Recommended, recommended);
    if let Some(w) = cost_note {
        push(SectionKind::CostWarning, core::slice::from_ref(w));
    }
    push(SectionKind::CriticalTail, tail);
    Pack {
        sections,
        total_tokens: 0, // measured by the caller over render()
        tokenizer: rift_tokens::provenance(),
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
mod tests;
