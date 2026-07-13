//! Sealed, trust-shaped section entries (ADR-0016).
//!
//! A [`SectionEntry`] is the ONLY thing a pack section can hold, and its fields
//! are private: the constructors below — each consuming a **sealed-constructor
//! output** (`Envelope` from `envelope_for`, `NeutralDigest` from the trust
//! chokepoint, a parse-validated `Handle`) or a closed descriptive vocabulary
//! ([`CriticalFact`], [`CostNote`]) — are the only way one exists. Free text
//! cannot enter a pack; the placement invariants (PRD-M6 stories 4/5/6) are
//! checked where entries are *admitted to a section* ([`crate::PackRequest`]),
//! so a violating pack is unrepresentable, not merely untested.

use rift_recall_types::{ArtifactState, Envelope, Handle, OmitReason};
use rift_recall_validity::trust::NeutralDigest;

/// The provenance class an entry was built from — the discriminant the
/// placement invariants are written against. Deliberately coarse: it names
/// *what kind of artifact* produced the entry, never its content.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryClass {
    /// A `known_unchanged` envelope: hash claim only, zero content.
    KnownUnchanged,
    /// A validity-bound summary (`summary_included`): digest, never raw bytes.
    SummaryBound,
    /// Exact content (`exact_included`): raw bytes of a TRUSTED artifact that
    /// passed the chokepoint — the only class carrying source text.
    ExactContent,
    /// A retrieval handle (`available_by_handle` or a recommendation).
    Handle,
    /// A neutral digest of untrusted bytes (counts + handle, no payload text).
    NeutralDigest,
    /// An edge-placed critical fact from the closed [`CriticalFact`] vocabulary.
    CriticalFact,
    /// The descriptive cost note from the closed [`CostNote`] vocabulary.
    CostNote,
}

/// How policy classifies the artifact's path (PRD story 6): a sensitive path
/// under conservative policy may appear **exact-gated or as a handle, never a
/// lossy summary** — a summary could leak a sensitive file's gist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sensitivity {
    /// Not sensitive: all envelope states render as themselves.
    Normal,
    /// Sensitive: `summary_included` downgrades to its handle at construction.
    Sensitive,
}

/// The closed vocabulary of edge-placed critical facts. Facts are *derived
/// provenance*, never caller text — adding a new kind of critical fact means
/// adding a variant here, in review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CriticalFact {
    /// The artifact's latest provenance carries a `reverted` outcome label.
    RevertedProvenance {
        /// Repo-relative path of the affected artifact.
        path: String,
    },
}

/// The closed vocabulary of cost-awareness notes (W7: descriptive only — a
/// directive is unrepresentable, there is no free-text variant).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CostNote {
    /// The fixed repeated-read awareness note.
    RepeatedReadAreas,
}

/// The fixed, neutral cost-awareness line [`CostNote::RepeatedReadAreas`]
/// renders (shared with the cost-hotspots tool so the vocabulary stays single).
pub const COST_NOTE: &str =
    "Informational only: these areas account for the most repeated read tokens.";

/// A stable, neutral tag for an omit reason (identifiers only, never content).
const fn omit_tag(reason: OmitReason) -> &'static str {
    match reason {
        OmitReason::BudgetExhausted => "budget_exhausted",
        OmitReason::PolicyDeniedExact => "policy_denied_exact",
        OmitReason::SensitivePathRequiresReason => "sensitive_path_requires_reason",
        OmitReason::SummaryInvalidSourceChanged => "summary_invalid_source_changed",
        OmitReason::SourceDeletedOrRedacted => "source_deleted_or_redacted",
        OmitReason::NotRequested => "not_requested",
    }
}

/// One trust-shaped pack line (or block): private fields, sealed constructors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionEntry {
    text: String,
    est_tokens: u32,
    class: EntryClass,
    /// The handle line this entry degrades to under budget pressure (P6 S2's
    /// spill: inline content becomes `available_by_handle`, NEVER a truncated
    /// slice of itself). Prepared at construction from the same sealed
    /// inputs; `None` for entries already minimal (handles, facts, notes).
    spill_text: Option<String>,
}

impl SectionEntry {
    fn new(text: String, class: EntryClass) -> Self {
        Self::with_spill(text, class, None)
    }

    fn with_spill(text: String, class: EntryClass, spill_text: Option<String>) -> Self {
        let est_tokens = crate::est_tokens(&text);
        Self {
            text,
            est_tokens,
            class,
            spill_text,
        }
    }

    /// The budget-pressure degrade for this entry: its handle form, when it
    /// has one that is actually cheaper. A whole-entry swap — content is
    /// never truncated (sensitive-safe by construction, PRD story 6).
    #[must_use]
    pub(crate) fn spill(&self) -> Option<Self> {
        let text = self.spill_text.clone()?;
        let spilled = Self::new(text, EntryClass::Handle);
        (spilled.est_tokens < self.est_tokens).then_some(spilled)
    }

    /// Build an entry from a four-state envelope — the output of the sealed
    /// `envelope_for` constructor (ADR-0016). Every state renders as
    /// identifiers/digests; `exact_included` is the only state whose content
    /// rides along (it was trust- and budget-gated inside `envelope_for`).
    /// Under [`Sensitivity::Sensitive`] a summary downgrades to its handle:
    /// exact-or-handle, never a lossy summary (PRD story 6).
    #[must_use]
    pub fn from_envelope(e: &Envelope, sensitivity: Sensitivity) -> Self {
        match &e.state {
            ArtifactState::KnownUnchanged {
                handle: _,
                basis_sha256,
                ..
            } => Self::new(
                format!(
                    "{} unchanged @sha256:{} (known_unchanged; not re-inlined)",
                    e.path, basis_sha256
                ),
                EntryClass::KnownUnchanged,
            ),
            ArtifactState::SummaryIncluded {
                handle,
                basis_sha256,
                summary,
                ..
            } => match sensitivity {
                Sensitivity::Sensitive => Self::new(
                    format!(
                        "{} available: {handle} (sensitive; summary withheld)",
                        e.path
                    ),
                    EntryClass::Handle,
                ),
                Sensitivity::Normal => Self::with_spill(
                    format!("{} summary@sha256:{basis_sha256}: {summary}", e.path),
                    EntryClass::SummaryBound,
                    Some(format!("{} available: {handle} (budget_exhausted)", e.path)),
                ),
            },
            ArtifactState::ExactIncluded {
                handle,
                basis_sha256,
                content,
                ..
            } => Self::with_spill(
                format!("{} changed @sha256:{basis_sha256}:\n{content}", e.path),
                EntryClass::ExactContent,
                Some(format!("{} available: {handle} (budget_exhausted)", e.path)),
            ),
            ArtifactState::AvailableByHandle { handle, reason, .. } => Self::new(
                format!("{} available: {handle} ({})", e.path, omit_tag(*reason)),
                EntryClass::Handle,
            ),
        }
    }

    /// Build an entry from a neutral digest — the output of the trust
    /// chokepoint (`serialize_untrusted`, ADR-0003). The digest type itself
    /// carries no payload text, so neither can the entry.
    #[must_use]
    pub fn from_neutral(d: &NeutralDigest) -> Self {
        let facts: Vec<String> = d
            .facts
            .iter()
            .map(|f| format!("{}={}", f.key, f.value))
            .collect();
        let note = d
            .redaction_note
            .as_deref()
            .map(|n| format!(" [{n}]"))
            .unwrap_or_default();
        Self::new(
            format!(
                "untrusted {}: {} - retrieve {}{note}",
                d.kind,
                facts.join(", "),
                d.handle
            ),
            EntryClass::NeutralDigest,
        )
    }

    /// Build a retrieval-handle entry from a parse-validated [`Handle`] —
    /// free-text "handles" cannot ride (they would not have parsed).
    #[must_use]
    pub fn from_handle(h: &Handle) -> Self {
        Self::new(format!("retrieve: {h}"), EntryClass::Handle)
    }

    /// Build an edge-placed critical fact from the closed vocabulary.
    #[must_use]
    pub fn critical(fact: &CriticalFact) -> Self {
        let text = match fact {
            CriticalFact::RevertedProvenance { path } => {
                format!("CRITICAL: {path}'s last provenance was reverted - verify before reuse")
            }
        };
        Self::new(text, EntryClass::CriticalFact)
    }

    /// Build the descriptive cost note from the closed vocabulary.
    #[must_use]
    pub fn cost_note(note: CostNote) -> Self {
        let text = match note {
            CostNote::RepeatedReadAreas => COST_NOTE.to_string(),
        };
        Self::new(text, EntryClass::CostNote)
    }

    /// The rendered line/block.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The entry's token estimate (ADR-0006's counting authority).
    #[must_use]
    pub const fn est_tokens(&self) -> u32 {
        self.est_tokens
    }

    /// The provenance class (drives the placement invariants).
    #[must_use]
    pub const fn class(&self) -> EntryClass {
        self.class
    }
}
