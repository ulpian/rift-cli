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
//! Sections are **trust-shaped by construction**: they hold sealed
//! [`SectionEntry`] values buildable only from the serve constructors' outputs
//! (see `entry`), and [`PackRequest`]'s per-section admission checks make a
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

mod citations;
mod entry;

pub use citations::{Citation, CitationTarget, Citations, PACK_TAG};
pub use entry::{
    CostNote, CriticalFact, EntryClass, EvidenceRef, FindingKind, FindingShape, ReviewScopeFact,
    ReviewSignal, SectionEntry, SeedDescriptor, Sensitivity, BUDGET_TAG, COST_NOTE, EVIDENCE_TAG,
    FILE_TAG, LINE_OPENERS,
};
use rift_core::{Result, RiftError};

/// Default target pack size (tokens) for the per-turn context pack.
pub const DEFAULT_PACK_TOKENS: u32 = 1200;
/// Default target size (tokens) for a [`PackKind::Review`] evidence pack. A
/// review pack is compiled on demand by `rift review`, not injected every turn,
/// so it targets more of the shared ceiling than the context pack does.
pub const DEFAULT_REVIEW_PACK_TOKENS: u32 = 3000;
/// Default target size (tokens) for a [`PackKind::Investigation`] evidence pack.
pub const DEFAULT_INVESTIGATION_PACK_TOKENS: u32 = 2000;
/// Hard ceiling a pack may never exceed (tokens) — **one ceiling for every
/// [`PackKind`]**. Kinds differ in their default target, never in what they are
/// allowed to spend; raising this is a product decision, not a pack-kind one.
pub const MAX_PACK_TOKENS: u32 = 4000;
/// Ceiling for the duplicated critical-facts tail (the tail anchor band).
const CRITICAL_TAIL_CAP: u32 = 250;
/// Ceiling reserved for the critical-facts top anchor.
const ANCHOR_TOP_CAP: u32 = 250;
/// The tail's graceful-degradation floor: a tail CUT below this is dropped
/// whole rather than served as a rump (100–250 tokens).
pub const TAIL_FLOOR_TOKENS: u32 = 100;

/// Why a pack could not be compiled (fail-loud: truncation is
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
    // The one workspace counting authority; floor of 1 so no pack
    // item is ever budget-free.
    rift_tokens::count(s).max(1)
}

/// The ordered section kinds (placement is part of the API contract).
///
/// The set is shared across [`PackKind`]s — the *layout* (which sections a kind
/// has, in what order, and in what order they degrade) is per-kind data, so the
/// evidence-pack kinds reuse the critical-facts edges, the changed section and
/// the recommendation/cost slots rather than re-deriving them.
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
    /// What a [`PackKind::Review`] pack is about: commit range, counts —
    /// identifiers and numbers from the closed review-scope vocabulary.
    ReviewScope,
    /// A review pack's derived signals: attribution split + confidence,
    /// failing-test history, four-class spend, recall savings.
    ReviewSignals,
    /// What a [`PackKind::Investigation`] pack starts from: a closed seed
    /// descriptor, or — for free text / issue content — the neutral digest the
    /// trust chokepoint produced from it.
    InvestigationSeed,
    /// Findings appended by the user's agents: uncorroborated notes only
    /// never facts.
    InvestigationFindings,
}

/// The pack kinds the compiler can produce. Every kind
/// shares one spine: the same [`SectionEntry`] constructors, the same anchor
/// bands, the same [`MAX_PACK_TOKENS`] hard ceiling and the same measure-then-
/// degrade loop. A kind contributes only its *layout* and its default budget.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PackKind {
    /// The per-turn retrieval pack — the original kind.
    #[default]
    Context,
    /// The `rift review` evidence pack.
    Review,
    /// The `rift investigate` evidence pack.
    Investigation,
}

impl PackKind {
    /// The stable wire tag (also the pack-key domain separator).
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Context => "context",
            Self::Review => "review",
            Self::Investigation => "investigation",
        }
    }

    /// The kind's default target budget. It is a *target*: [`compile`] still
    /// clamps to [`MAX_PACK_TOKENS`] and the remaining turn budget, so no kind
    /// can raise the ceiling by asking for more.
    #[must_use]
    pub const fn default_tokens(self) -> u32 {
        match self {
            Self::Context => DEFAULT_PACK_TOKENS,
            Self::Review => DEFAULT_REVIEW_PACK_TOKENS,
            Self::Investigation => DEFAULT_INVESTIGATION_PACK_TOKENS,
        }
    }
}

/// The kind's render order, top to tail. Every layout opens with
/// [`SectionKind::CriticalTop`] and closes with [`SectionKind::CriticalTail`] —
/// the Lost-in-the-Middle remedy is spine, not per-kind policy.
const fn placement(kind: PackKind) -> &'static [SectionKind] {
    match kind {
        PackKind::Context => &[
            SectionKind::CriticalTop,
            SectionKind::Changed,
            SectionKind::Unchanged,
            SectionKind::Recommended,
            SectionKind::CostWarning,
            SectionKind::CriticalTail,
        ],
        // Signals outrank changed content deliberately: placement order is
        // also fill order, and a reviewing agent can always read the diff out
        // of the repo — it cannot recompute attribution confidence, failing-
        // test history or savings. The bulky, recoverable thing sits in the
        // U-curve's middle; the dense, unrecoverable thing sits near the top.
        PackKind::Review => &[
            SectionKind::CriticalTop,
            SectionKind::ReviewScope,
            SectionKind::ReviewSignals,
            SectionKind::Changed,
            SectionKind::Recommended,
            SectionKind::CostWarning,
            SectionKind::CriticalTail,
        ],
        PackKind::Investigation => &[
            SectionKind::CriticalTop,
            SectionKind::InvestigationSeed,
            SectionKind::InvestigationFindings,
            SectionKind::Changed,
            SectionKind::Recommended,
            SectionKind::CostWarning,
            SectionKind::CriticalTail,
        ],
    }
}

/// Reverse-rank degrade order over the kind's **body** sections: when the
/// measured pack exceeds the cap, the first section in this list that can give
/// something up does (its last entry spills to a handle, else drops). The edges
/// are not listed — the engine shrinks the tail only after the whole body is
/// exhausted, and truncates the top anchor last of all.
///
/// The [`PackKind::Context`] order is the one the compiler has always used
/// (recommendations, then the cost note, then unchanged, then changed); it is
/// data now rather than a hardcoded `||` chain.
const fn degrade_order(kind: PackKind) -> &'static [SectionKind] {
    match kind {
        PackKind::Context => &[
            SectionKind::Recommended,
            SectionKind::CostWarning,
            SectionKind::Unchanged,
            SectionKind::Changed,
        ],
        // Scope is what the review is *about* and signals are the derived
        // numbers the legacy backend never had: bulk changed content goes first, the answer
        // to "what am I reviewing" goes last.
        PackKind::Review => &[
            SectionKind::Recommended,
            SectionKind::CostWarning,
            SectionKind::Changed,
            SectionKind::ReviewSignals,
            SectionKind::ReviewScope,
        ],
        // Same logic: the seed is the investigation's subject, so it outlives
        // the findings, which outlive the surrounding change context.
        PackKind::Investigation => &[
            SectionKind::Recommended,
            SectionKind::CostWarning,
            SectionKind::Changed,
            SectionKind::InvestigationFindings,
            SectionKind::InvestigationSeed,
        ],
    }
}

impl SectionKind {
    /// The stable wire tag for this section (the serialized vocabulary).
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::CriticalTop => "critical_top",
            Self::Changed => "changed",
            Self::Unchanged => "unchanged",
            Self::Recommended => "recommended",
            Self::CostWarning => "cost_warning",
            Self::CriticalTail => "critical_tail",
            Self::ReviewScope => "review_scope",
            Self::ReviewSignals => "review_signals",
            Self::InvestigationSeed => "investigation_seed",
            Self::InvestigationFindings => "investigation_findings",
        }
    }
}

/// Whether a section is one of the duplicated critical-fact edges (handled by
/// the engine's anchor machinery, never by the body fill).
const fn is_edge(kind: SectionKind) -> bool {
    matches!(kind, SectionKind::CriticalTop | SectionKind::CriticalTail)
}

/// A compiled pack section (the render view; content came from sealed entries).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// The section kind.
    pub kind: SectionKind,
    /// The rendered lines of the section.
    pub lines: Vec<String>,
}

/// Which entry classes a section admits — the placement invariants as data.
/// `Unchanged` NEVER admits exact content (honest-cache:
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
        SectionKind::ReviewScope => matches!(class, EntryClass::ScopeFact),
        SectionKind::ReviewSignals => matches!(class, EntryClass::Signal),
        // A free-text / issue-content seed is admissible only as the neutral
        // digest the chokepoint made of it (injection-wrapped);
        // exact content is refused outright.
        SectionKind::InvestigationSeed => matches!(
            class,
            EntryClass::SeedRef | EntryClass::NeutralDigest | EntryClass::Handle
        ),
        // An uncorroborated note is servable only as a note. It
        // lives here and nowhere else — above all, not in the critical edges.
        // A bare `Handle` is admitted alongside it for the same reason the
        // recommendation section is made of them: a retrieval pointer asserts
        // nothing, so "the rest is over here" stays sayable without a claim.
        SectionKind::InvestigationFindings => {
            matches!(class, EntryClass::UnverifiedNote | EntryClass::Handle)
        }
    }
}

/// The inputs to compile a pack. Fields are private and entries are admitted
/// through the per-section methods below, each of which enforces two things —
/// the section's placement invariant *and* that the pack kind even has that
/// section — so an invariant-violating request never exists.
#[derive(Clone, Debug, Default)]
pub struct PackRequest {
    kind: PackKind,
    critical: Vec<SectionEntry>,
    /// Body sections in *insertion* order; render and degrade order come from
    /// the kind's layout, never from how the caller happened to fill them.
    body: Vec<(SectionKind, Vec<SectionEntry>)>,
    token_budget: u32,
    /// The ordinal→address table for the citations this request's entries
    /// carry. Filled by the caller's pre-compile ordinal pass, and
    /// handed straight through to the compiled [`Pack`] — [`compile`] never
    /// touches it, which is what makes "degradation drops, it never renumbers"
    /// true of the table as well as of the rendered lines.
    citations: Citations,
}

fn refuse(kind: SectionKind, class: EntryClass) -> RiftError {
    RiftError::validity(format!(
        "placement invariant: {kind:?} does not admit {class:?} entries"
    ))
}

fn refuse_section(pack: PackKind, section: SectionKind) -> RiftError {
    RiftError::validity(format!(
        "layout invariant: a {} pack has no {section:?} section",
        pack.tag()
    ))
}

/// The entries a request holds for `section` (an absent section is empty).
fn slot_of(body: &[(SectionKind, Vec<SectionEntry>)], section: SectionKind) -> &[SectionEntry] {
    body.iter()
        .find(|(k, _)| *k == section)
        .map_or(&[][..], |(_, v)| v.as_slice())
}

impl PackRequest {
    /// A [`PackKind::Context`] request targeting `token_budget` tokens (clamped
    /// at compile time).
    #[must_use]
    pub fn new(token_budget: u32) -> Self {
        Self::for_kind(PackKind::Context, token_budget)
    }

    /// A request for `kind`, targeting `token_budget` tokens (clamped at
    /// compile time by the shared [`MAX_PACK_TOKENS`] ceiling).
    #[must_use]
    pub fn for_kind(kind: PackKind, token_budget: u32) -> Self {
        Self {
            kind,
            token_budget,
            ..Self::default()
        }
    }

    /// The kind this request compiles to.
    #[must_use]
    pub const fn kind(&self) -> PackKind {
        self.kind
    }

    /// Both admission gates, checked before anything is mutated: the kind must
    /// have the section, and the section must admit the class.
    fn check(&self, section: SectionKind, class: EntryClass) -> Result<()> {
        if !placement(self.kind).contains(&section) {
            return Err(refuse_section(self.kind, section));
        }
        if !admits(section, class) {
            return Err(refuse(section, class));
        }
        Ok(())
    }

    /// Admit `e` into `section` after both gates pass.
    fn admit(&mut self, section: SectionKind, e: SectionEntry) -> Result<()> {
        self.check(section, e.class())?;
        if let Some((_, v)) = self.body.iter_mut().find(|(k, _)| *k == section) {
            v.push(e);
        } else {
            self.body.push((section, vec![e]));
        }
        Ok(())
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
        self.admit(SectionKind::Changed, e)
    }

    /// Admit an unchanged-but-relevant entry. **Exact content is refused** —
    /// unchanged context is summaries + handles + hashes only.
    ///
    /// # Errors
    /// [`RiftError::Validity`] if the class is not admissible in `Unchanged`,
    /// or if the pack kind has no `Unchanged` section.
    pub fn push_unchanged(&mut self, e: SectionEntry) -> Result<()> {
        self.admit(SectionKind::Unchanged, e)
    }

    /// Admit a recommended retrieval (parse-validated handles only).
    ///
    /// # Errors
    /// [`RiftError::Validity`] unless the entry is a [`EntryClass::Handle`].
    pub fn push_recommended(&mut self, e: SectionEntry) -> Result<()> {
        self.admit(SectionKind::Recommended, e)
    }

    /// Admit a review-scope fact — what this review is about.
    ///
    /// # Errors
    /// [`RiftError::Validity`] unless this is a [`PackKind::Review`] request
    /// and the entry came from [`SectionEntry::review_scope`].
    pub fn push_review_scope(&mut self, e: SectionEntry) -> Result<()> {
        self.admit(SectionKind::ReviewScope, e)
    }

    /// Admit a derived review signal.
    ///
    /// # Errors
    /// [`RiftError::Validity`] unless this is a [`PackKind::Review`] request
    /// and the entry came from [`SectionEntry::review_signal`].
    pub fn push_review_signal(&mut self, e: SectionEntry) -> Result<()> {
        self.admit(SectionKind::ReviewSignals, e)
    }

    /// Admit an investigation seed: a closed [`SeedDescriptor`], a handle, or —
    /// for free text and issue content — the neutral digest the chokepoint made
    /// of it. Prose has no third path in.
    ///
    /// # Errors
    /// [`RiftError::Validity`] unless this is a [`PackKind::Investigation`]
    /// request and the class is admissible in the seed section.
    pub fn push_seed(&mut self, e: SectionEntry) -> Result<()> {
        self.admit(SectionKind::InvestigationSeed, e)
    }

    /// Admit an unverified finding note. Findings live in exactly
    /// one section; the critical edges refuse them, so an uncorroborated claim
    /// can never be duplicated top-and-tail as though it were decisive.
    ///
    /// # Errors
    /// [`RiftError::Validity`] unless this is a [`PackKind::Investigation`]
    /// request and the entry came from [`SectionEntry::unverified_finding`]
    /// (or is a bare handle).
    pub fn push_finding(&mut self, e: SectionEntry) -> Result<()> {
        self.admit(SectionKind::InvestigationFindings, e)
    }

    /// Attach the ordinal→address table for the citations this request's
    /// entries carry. The compiled pack carries it; it is never
    /// serialized, so it stays the server's half of the exchange.
    pub fn set_citations(&mut self, citations: Citations) {
        self.citations = citations;
    }

    /// The table attached so far.
    #[must_use]
    pub const fn citations(&self) -> &Citations {
        &self.citations
    }

    /// Set the descriptive cost note (closed vocabulary; directive-free).
    /// Setting it twice keeps the last note — the slot holds at most one.
    ///
    /// # Errors
    /// [`RiftError::Validity`] unless the entry is a [`EntryClass::CostNote`].
    pub fn set_cost_note(&mut self, e: SectionEntry) -> Result<()> {
        // Gate first: a refused note must not clear the note already set.
        self.check(SectionKind::CostWarning, e.class())?;
        if let Some((_, v)) = self
            .body
            .iter_mut()
            .find(|(k, _)| *k == SectionKind::CostWarning)
        {
            v.clear();
        }
        self.admit(SectionKind::CostWarning, e)
    }
}

/// A compiled, ordered, budgeted pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pack {
    /// Which kind of pack this is (its layout produced the section order).
    pub kind: PackKind,
    /// Sections in placement order.
    pub sections: Vec<Section>,
    /// Total tokens, **measured post-render over the serialized text** (never
    /// a sum of per-item estimates); ≤ the effective cap.
    pub total_tokens: u32,
    /// The tokenizer that measured `total_tokens` ("skeptical buyer"
    /// provenance).
    pub tokenizer: &'static str,
    /// **The citation table** — private, and reachable only through
    /// [`Self::citations`], because the one thing that must never happen to it
    /// is being picked up by a serializer along with the rest of the struct.
    /// [`Self::to_evidence`] is the only wire form of a pack and has no field
    /// for it.
    citations: Citations,
}

impl Pack {
    /// This pack's ordinal→address table, keyed under the pack's own
    /// [`pack_key_for`] identity by whoever holds it.
    ///
    /// Pack-local and server-side: an agent sends back `evidence:<n>` and the
    /// **server** turns it into `(path, content_sha256)`, applies the existing
    /// path-based policy gate to the resolved path, and binds both halves. The
    /// path never crosses to the agent's side of the exchange in either
    /// direction.
    #[must_use]
    pub const fn citations(&self) -> &Citations {
        &self.citations
    }

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
/// The effective cap is `min(token_budget, MAX_PACK_TOKENS, turn_remaining)` —
/// a pack can never blow the per-turn ceiling. Anchors are
/// reserved first (top ≤ 250 tokens; tail 100–250 with graceful degradation:
/// a tail CUT below [`TAIL_FLOOR_TOKENS`] is dropped whole). Sections fill in
/// rank order; an inline entry that does not fit **spills to its handle**
/// (never a truncated slice — sensitive-safe). The total is then
/// **re-measured over the rendered text** and, while it exceeds the cap,
/// content degrades in reverse-rank order (lowest-priority section first,
/// last entry first: spill, then drop), then the drop announcement is given
/// up, then the tail shrinks, then the top truncates — a strictly-shrinking,
/// terminating loop.
///
/// A pack that dropped anything says so once, in a counted
/// `drop_marker` line, and reserves that line's cost before filling — an
/// entry the budget took is never taken silently (the fail-loud rule applied
/// to partial loss, not only to total failure).
///
/// # Errors
/// [`PackError::BudgetTooSmall`] when the cap cannot hold even the request's
/// cheapest coherent content — fail-loud, never a silently empty pack
/// (the documented caller fallback is `changed_hunks` directly).
pub fn compile(req: &PackRequest, turn_remaining: u32) -> core::result::Result<Pack, PackError> {
    let cap = req
        .token_budget
        .clamp(1, MAX_PACK_TOKENS)
        .min(turn_remaining);

    // Fail-loud floor: the cheapest thing this request wants to say must fit.
    // (A fully empty request compiles to an empty pack — nothing to hold.)
    match cheapest_entry_tokens(req) {
        None => return Ok(empty_pack(req)),
        Some(needed) if needed > cap => {
            return Err(PackError::BudgetTooSmall { needed, cap });
        }
        Some(_) => {}
    }

    // 1) Top anchor: critical facts in order, within the anchor band and cap.
    let (top, top_cost) = anchor_top(&req.critical, ANCHOR_TOP_CAP.min(cap));

    // 2) Tail reserve: the top anchors repeat within the 100–250 band.
    let tail_full: u32 = top.iter().map(SectionEntry::est_tokens).sum();
    let reserve_tail = tail_full
        .min(CRITICAL_TAIL_CAP)
        .min(cap.saturating_sub(top_cost));

    // 3) Fill the kind's body sections, in layout order, within the estimate
    //    budget, spilling to handles — and counting what neither form could fit.
    let body = fill_body_announced(req, cap.saturating_sub(top_cost + reserve_tail));

    // 4) Tail: the longest top-prefix within the reserve; a CUT tail below
    //    the floor is dropped whole (deterministic graceful degradation).
    let tail = materialize_tail(&top, reserve_tail);

    // 5) Post-render re-measure (over the serialized text, never estimate
    //    sums) + reverse-rank degrade until the measurement fits the cap.
    let placed = Placed {
        top,
        body,
        tail,
        announce: true,
    };
    measure_and_degrade(req, cap, placed)
}

/// The estimate of the cheapest entry the request wants to say — its first
/// critical fact or any body entry — which is the floor a cap must clear
/// before compiling is worth attempting. `None` for a request with nothing in
/// it.
fn cheapest_entry_tokens(req: &PackRequest) -> Option<u32> {
    req.critical
        .first()
        .map(SectionEntry::est_tokens)
        .into_iter()
        .chain(
            req.body
                .iter()
                .flat_map(|(_, v)| v.iter().map(SectionEntry::est_tokens)),
        )
        .min()
}

/// The pack an empty request compiles to: no sections, nothing measured, and
/// the request's citation table handed through as it came.
fn empty_pack(req: &PackRequest) -> Pack {
    Pack {
        kind: req.kind,
        sections: Vec::new(),
        total_tokens: 0,
        tokenizer: rift_tokens::provenance(),
        citations: req.citations.clone(),
    }
}

/// The top anchor: the critical facts in their given order whose estimates
/// fit `band`, skipping any single fact too large for what remains, with the
/// estimate they spend.
fn anchor_top(critical: &[SectionEntry], band: u32) -> (Vec<SectionEntry>, u32) {
    let mut top: Vec<SectionEntry> = Vec::new();
    let mut top_cost = 0u32;
    for fact in critical {
        if top_cost + fact.est_tokens() <= band {
            top_cost += fact.est_tokens();
            top.push(fact.clone());
        }
    }
    (top, top_cost)
}

/// Fill the body within `est_room`, and when that fill dropped anything,
/// fill it again with the drop announcement's own cost reserved.
///
/// If anything was dropped, the pack owes the reader a line saying so — and
/// the fill is greedy, so it leaves no room to say it in. Re-fill with the
/// announcement's own cost reserved, exactly as the tail is reserved. The
/// reserve comes out of the SLACK the first pass left in the usual case; it
/// costs an entry only when there was no slack, and that entry is then
/// counted by the very line it paid for.
fn fill_body_announced(req: &PackRequest, est_room: u32) -> Vec<Slot> {
    let body = fill_body(req, est_room);
    let dropped = total_dropped(&body);
    if dropped == 0 {
        return body;
    }
    let reserve = est_tokens(&drop_marker(dropped));
    fill_body(req, est_room.saturating_sub(reserve))
}

/// Measure the assembled pack over its rendered text and degrade it one step
/// at a time until the measurement fits `cap` — a strictly shrinking,
/// terminating loop, because every step removes something or reports that
/// nothing is left to remove.
///
/// # Errors
/// [`PackError::BudgetTooSmall`] when nothing is left to degrade and the
/// minimal anchor still measures over the cap.
fn measure_and_degrade(
    req: &PackRequest,
    cap: u32,
    mut placed: Placed,
) -> core::result::Result<Pack, PackError> {
    loop {
        let pack = assemble(
            req.kind,
            &placed.top,
            &placed.body,
            &placed.tail,
            placed.announce,
        );
        let measured = rift_tokens::count(&pack.render());
        if measured <= cap {
            return Ok(Pack {
                total_tokens: measured,
                // The table is the request's, handed through untouched. The
                // degrade steps may have dropped entries; they may not have
                // renumbered any, and they cannot renumber what they never hold.
                citations: req.citations.clone(),
                ..pack
            });
        }
        if !placed.degrade_step(req.kind) {
            // Nothing left but the minimal anchor and it still measures over
            // the cap (estimate/measure divergence corner): fail loud.
            return Err(PackError::BudgetTooSmall {
                needed: measured,
                cap,
            });
        }
    }
}

/// Fill the kind's body sections in layout order within `room` estimate
/// tokens, spilling an entry to its handle when the entry itself will not fit
/// and counting what fits in neither form. A pure function of `(req, room)`, so
/// it can be run twice — the second time with the announcement's cost reserved.
fn fill_body(req: &PackRequest, room: u32) -> Vec<Slot> {
    let mut used = 0u32;
    placement(req.kind)
        .iter()
        .copied()
        .filter(|k| !is_edge(*k))
        .map(|kind| {
            let mut slot = Slot {
                kind,
                entries: Vec::new(),
                dropped: 0,
            };
            for item in slot_of(&req.body, kind) {
                if used.saturating_add(item.est_tokens()) <= room {
                    used += item.est_tokens();
                    slot.entries.push(item.clone());
                    continue;
                }
                match item.spill() {
                    Some(spilled) if used.saturating_add(spilled.est_tokens()) <= room => {
                        used += spilled.est_tokens();
                        slot.entries.push(spilled);
                    }
                    // Neither the entry nor its handle fits: it is gone, and a
                    // pack that loses content silently lies by omission.
                    _ => slot.dropped += 1,
                }
            }
            slot
        })
        .collect()
}

/// How many entries the body gave up outright, across every section.
fn total_dropped(body: &[Slot]) -> u32 {
    body.iter()
        .fold(0u32, |acc, s| acc.saturating_add(s.dropped))
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

/// A body section under compilation: the entries that survived, and how many
/// the budget gave up outright. A **spill is not a drop** — a spilled entry is
/// still in the pack, wearing its handle, and says so in its own line — so only
/// whole-entry losses are counted here.
#[derive(Clone, Debug)]
struct Slot {
    kind: SectionKind,
    entries: Vec<SectionEntry>,
    dropped: u32,
}

/// The pack's placed parts between filling and the final measurement: the
/// state the degrade loop shrinks one step at a time.
struct Placed {
    top: Vec<SectionEntry>,
    body: Vec<Slot>,
    tail: Vec<SectionEntry>,
    /// Whether the drop announcement still renders; given up only after the
    /// whole body is gone.
    announce: bool,
}

impl Placed {
    /// Take one degrade step, in the pack's fixed order. `true` if anything
    /// changed.
    ///
    /// Reverse-rank, from the kind's degrade order: lowest-priority section
    /// first, last entry first; spill an inline entry if that shrinks it,
    /// else drop it outright. The announcement was already paid for when the
    /// body was filled, so it is given up only after the whole body is gone —
    /// i.e. for a pack that is nothing but its anchors, where there is no
    /// content left to annotate and the reader can see there is none. Only
    /// then does the tail shrink, and the top anchor truncates last of all.
    fn degrade_step(&mut self, kind: PackKind) -> bool {
        degrade_order(kind).iter().any(|section| {
            self.body
                .iter_mut()
                .find(|s| s.kind == *section)
                .is_some_and(Slot::degrade_last)
        }) || silence(&mut self.announce)
            || shrink_tail(&mut self.tail)
            || (self.top.len() > 1 && self.top.pop().is_some())
    }
}

impl Slot {
    /// Degrade this section's LAST entry: swap it for its cheaper handle spill
    /// when it has one, else remove it — and count the removal. `true` if
    /// anything changed.
    fn degrade_last(&mut self) -> bool {
        let Some(last) = self.entries.last() else {
            return false;
        };
        match last.spill() {
            Some(spilled) if spilled.est_tokens() < last.est_tokens() => {
                if let Some(slot) = self.entries.last_mut() {
                    *slot = spilled;
                }
                true
            }
            _ => {
                let popped = self.entries.pop().is_some();
                if popped {
                    self.dropped += 1;
                }
                popped
            }
        }
    }
}

/// The counted line a pack renders when the budget dropped entries (advisory
/// A5): one line, in the pack's own grammar, saying how much of what the caller
/// offered did not survive.
///
/// **Counted, not itemized**, and the reason is measured rather than assumed. A
/// marker per section was built first and cost too much for what it bought:
/// [`Pack::render`] emits no section headers, so four consecutive
/// `budget: N item(s) dropped` lines are ambiguous to the reader they are for,
/// and at a 180-token review budget they displaced two of the five signals the
/// review pack exists to carry. One counted line tells the agent the thing it
/// can act on — this pack is not the whole story — for the price of a line.
fn drop_marker(dropped: u32) -> String {
    format!("{BUDGET_TAG}{dropped} item(s) dropped")
}

/// Give up the drop announcements. This is reached only when the whole body is
/// already empty (every section ahead of it in the degrade chain declined),
/// i.e. for a pack that is nothing but its anchors — where there is no content
/// left to annotate and the reader can see there is none. Returns whether
/// anything was actually given up, so the degrade chain can move on.
fn silence(announce: &mut bool) -> bool {
    let was = *announce;
    *announce = false;
    was
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

/// Assemble the ordered sections from the surviving entries, in the kind's
/// placement order (placement is the compiler's, not the caller's). An empty
/// section is omitted — except the critical top, whose presence (even empty) is
/// part of the shape every pack renders.
fn assemble(
    kind: PackKind,
    top: &[SectionEntry],
    body: &[Slot],
    tail: &[SectionEntry],
    announce: bool,
) -> Pack {
    // What the pack HOLDS, counted here rather than taken from the caller: the
    // edge fact that says "these are notes, not facts" states this number, and
    // the caller's number went stale the moment the budget dropped a finding
    // (advisory A4). Recounted every iteration of the degrade loop, so it is
    // right for the pack that is finally returned, not for an earlier draft.
    let notes = body
        .iter()
        .flat_map(|s| s.entries.iter())
        .filter(|e| e.class() == EntryClass::UnverifiedNote)
        .count();
    let notes = u32::try_from(notes).unwrap_or(u32::MAX);
    // What the budget took, across the whole body: entries that fit in neither
    // their own form nor their handle. Announced in the cost/awareness slot —
    // "the budget cost you six items" is precisely a cost warning, and that
    // slot exists in every kind's layout, so the statement has a home even when
    // every section it describes has been emptied.
    let dropped = total_dropped(body);
    let mut sections: Vec<Section> = Vec::new();
    for section in placement(kind).iter().copied() {
        let entries: &[SectionEntry] = match section {
            SectionKind::CriticalTop => top,
            SectionKind::CriticalTail => tail,
            other => body
                .iter()
                .find(|s| s.kind == other)
                .map_or(&[][..], |s| s.entries.as_slice()),
        };
        let mut lines: Vec<String> = entries
            .iter()
            .filter_map(|e| e.with_live_count(notes))
            .map(|e| e.text().to_string())
            .collect();
        if announce && dropped > 0 && section == SectionKind::CostWarning {
            lines.push(drop_marker(dropped));
        }
        if section == SectionKind::CriticalTop || !lines.is_empty() {
            sections.push(Section {
                kind: section,
                lines,
            });
        }
    }
    Pack {
        kind,
        sections,
        total_tokens: 0, // measured by the caller over render()
        tokenizer: rift_tokens::provenance(),
        citations: Citations::new(),
    }
}

/// The wire schema version of a serialized [`EvidencePack`]. A reader matches
/// it exactly; an unknown version is refused whole, never partially read.
pub const EVIDENCE_PACK_SCHEMA_VERSION: u32 = 1;

/// One section of a serialized pack: its wire tag and its rendered lines.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EvidenceSection {
    /// The section's stable wire tag ([`SectionKind::tag`]).
    pub section: &'static str,
    /// The section's rendered lines, in placement order.
    pub lines: Vec<String>,
}

/// A compiled pack in serialized form — the artifact the `review`/`investigate`
/// verbs will write and the only shape of a pack that can cross a machine
/// boundary (see `docs/sync-format.md`).
///
/// **Serialize-only, deliberately.** There is no `Deserialize` impl and there
/// will not be one by accident: every line in a section exists because a sealed
/// [`SectionEntry`] constructor produced it, and a decoded pack would
/// be section text that never passed one — the same hole a second
/// `RedactedBytes` constructor would open. A future reader validates on the way
/// in and reads this as data; it never reconstitutes a [`Pack`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct EvidencePack {
    /// Wire schema version ([`EVIDENCE_PACK_SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// Which kind of pack this is ([`PackKind::tag`]).
    pub kind: &'static str,
    /// The input-keyed identity ([`pack_key_for`]) — computed from the pack's
    /// immutable inputs, never from these bytes.
    pub pack_key: String,
    /// The tokenizer that measured `total_tokens` (counting-authority provenance).
    pub tokenizer: &'static str,
    /// Tokens, measured over the rendered text; ≤ [`MAX_PACK_TOKENS`].
    pub total_tokens: u32,
    /// The sections, in placement order.
    pub sections: Vec<EvidenceSection>,
}

impl Pack {
    /// The serialized form of this pack, keyed by the immutable inputs it was
    /// compiled from. The key is computed here from `inputs` (domain-separated
    /// by kind) rather than hashed out of the rendered bytes — a rendered hash
    /// is a post-hoc equality check, not an identity.
    #[must_use]
    pub fn to_evidence(&self, inputs: &PackKeyInputs) -> EvidencePack {
        EvidencePack {
            schema_version: EVIDENCE_PACK_SCHEMA_VERSION,
            kind: self.kind.tag(),
            pack_key: pack_key_for(self.kind, inputs),
            tokenizer: self.tokenizer,
            total_tokens: self.total_tokens,
            sections: self
                .sections
                .iter()
                .map(|s| EvidenceSection {
                    section: s.kind.tag(),
                    lines: s.lines.clone(),
                })
                .collect(),
        }
    }
}

/// The immutable inputs that identify a pack. `ranked` is the
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

/// The input-keyed pack identity: `PackKey = hash(query,
/// ranked_ids+hashes, policy_ver, budget)` — over the immutable inputs,
/// computable **before** rendering, so identical inputs re-serve
/// byte-identically and any pack cache is content-addressed by inputs. The
/// rendered output is never the key (an output hash is a post-hoc equality
/// check, not a cache key). Domain-separated + length-prefixed, so no two
/// distinct input sets collide via concatenation ambiguity.
#[must_use]
pub fn pack_key(inputs: &PackKeyInputs) -> String {
    pack_key_for(PackKind::Context, inputs)
}

/// The pack-key domain string per kind. The context domain is **frozen** —
/// context packs are already keyed by it (the serve golden pins one) — so the
/// evidence kinds extend the domain rather than parameterizing it, and the same
/// inputs under two kinds can never collide.
const fn packkey_domain(kind: PackKind) -> &'static [u8] {
    match kind {
        PackKind::Context => b"rift.packkey.v1\x00",
        PackKind::Review => b"rift.packkey.v1.review\x00",
        PackKind::Investigation => b"rift.packkey.v1.investigation\x00",
    }
}

/// [`pack_key`] for an arbitrary [`PackKind`]: the same input-keyed identity,
/// domain-separated by kind. `pack_key(i) == pack_key_for(PackKind::Context, i)`
/// for every `i` — pinned by test.
#[must_use]
pub fn pack_key_for(kind: PackKind, inputs: &PackKeyInputs) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(packkey_domain(kind));
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
