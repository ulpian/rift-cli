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
/// Tokens reserved for the duplicated critical-facts tail.
const CRITICAL_TAIL_CAP: u32 = 250;

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
    let tail_cost = req
        .critical
        .iter()
        .map(SectionEntry::est_tokens)
        .sum::<u32>()
        .min(CRITICAL_TAIL_CAP);
    let reserve_tail = tail_cost.min(budget);

    // 1. critical facts top
    let mut top = Section {
        kind: SectionKind::CriticalTop,
        lines: Vec::new(),
    };
    for fact in &req.critical {
        if used + fact.est_tokens() + reserve_tail <= budget {
            used += fact.est_tokens();
            top.lines.push(fact.text().to_string());
        }
    }
    sections.push(top);

    // helper to fill a section greedily within remaining budget (minus tail reserve)
    let fill =
        |kind: SectionKind, items: &[SectionEntry], used: &mut u32, sections: &mut Vec<Section>| {
            let mut sec = Section {
                kind,
                lines: Vec::new(),
            };
            for item in items {
                if *used + item.est_tokens() + reserve_tail <= budget {
                    *used += item.est_tokens();
                    sec.lines.push(item.text().to_string());
                }
            }
            if !sec.lines.is_empty() {
                sections.push(sec);
            }
        };

    // 2. changed, 3. unchanged, 4. recommended
    fill(SectionKind::Changed, &req.changed, &mut used, &mut sections);
    fill(
        SectionKind::Unchanged,
        &req.unchanged,
        &mut used,
        &mut sections,
    );
    fill(
        SectionKind::Recommended,
        &req.recommended,
        &mut used,
        &mut sections,
    );

    // 5. cost warning
    if let Some(w) = &req.cost_note {
        if used + w.est_tokens() + reserve_tail <= budget {
            used += w.est_tokens();
            sections.push(Section {
                kind: SectionKind::CostWarning,
                lines: vec![w.text().to_string()],
            });
        }
    }

    // 6. critical facts tail (duplicated for recency)
    if !req.critical.is_empty() {
        used += reserve_tail;
        sections.push(Section {
            kind: SectionKind::CriticalTail,
            lines: req.critical.iter().map(|f| f.text().to_string()).collect(),
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
mod tests;
