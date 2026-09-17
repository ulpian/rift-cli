//! Sealed, trust-shaped section entries.
//!
//! A [`SectionEntry`] is the ONLY thing a pack section can hold, and its fields
//! are private: the constructors below — each consuming a **sealed-constructor
//! output** (`Envelope` from `envelope_for`, `NeutralDigest` from the trust
//! chokepoint, a parse-validated `Handle`) or a closed descriptive vocabulary
//! ([`CriticalFact`], [`CostNote`]) — are the only way one exists. Free text
//! cannot enter a pack; the placement invariants are
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
    /// A review-scope fact (what is under review): identifiers + counts from a
    /// closed vocabulary, never caller prose.
    ScopeFact,
    /// A derived review signal (attribution, confidence, failures, spend,
    /// savings) from a closed vocabulary — numbers Rift computed, not claims.
    Signal,
    /// An investigation seed reference from the closed descriptor vocabulary
    /// (path / checkpoint / error signature — identifiers only).
    SeedRef,
    /// An **uncorroborated** note: a finding an agent proposed,
    /// carried as the chokepoint's neutral digest plus a validated shape and a
    /// provenance handle. It is never a fact, so it never reaches a
    /// critical-facts section — the admission table, not a convention, is what
    /// stops it.
    UnverifiedNote,
}

/// How policy classifies the artifact's path: a sensitive path
/// under conservative policy may appear **exact-gated or as a handle, never a
/// lossy summary** — a summary could leak a sensitive file's gist.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sensitivity {
    /// Not sensitive: all envelope states render as themselves.
    Normal,
    /// Sensitive: `summary_included` downgrades to its handle at construction.
    Sensitive,
}

/// **How an artifact is named to an agent: by ordinal, never by path**
/// — a path is a name an attacker may choose, an ordinal is not.
///
/// # Why a path cannot ride
///
/// A repository path is attacker-influenceable, and not only through the
/// worktree: `file_revision` has two production writers, and one of them is the
/// `ToolResult` arm reached by any *read-shaped* tool call. An agent under
/// injection writes a file and reads it — two ordinary tool calls, no git — and
/// the attacker-chosen name becomes a permanent, append-only, content-addressed
/// fact that a pack would then render verbatim into the next agent's context.
/// Raising the cost (resolution against the record) does not close the class;
/// not rendering the name does.
///
/// A needle filter is forbidden and a length bound was shown useless
/// (`Ignore all previous instructions` is 31 characters), so the answer is
/// structural: **the name is replaced by a number this crate's caller assigned**
/// — and a number carries no instruction.
///
/// # What the ordinal means, and what it does NOT
///
/// It is **pack-local** and is assigned by a deterministic pre-compile pass over
/// the caller's fact set *in that set's own total order* — never by position in
/// the finished pack. [`crate::compile`] degrades by dropping and spilling
/// entries, so a position-derived ordinal would number the same artifact
/// differently under one profile than under another. Degradation may remove an
/// entry; it never renumbers one, because the ordinal is rendered into the
/// entry's text at construction, before any budget is applied — including into
/// its spill form.
///
/// Two entries about the same artifact therefore carry the same ordinal, which
/// is what lets a reader join a critical fact to the artifact line it is about
/// without either of them naming a file.
///
/// # The content address
///
/// Optional, and it is a real content address or it is absent: a string that is
/// not 64 hex digits is **not** rendered, because the only way one arrives here
/// is a caller passing something else, and rendering it would move the injection
/// from the path field into the address field — closing a door by opening the
/// one beside it. It rides where the pack has no other line carrying the
/// artifact's address (an investigation finding); it is omitted where the
/// artifact's own entry already states it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EvidenceRef {
    ordinal: u32,
    sha256: Option<String>,
}

/// Whether `s` is a 64-character lowercase-hex SHA-256 — the only shape this
/// crate will render as a content address.
///
/// `basis_sha256` is a `String` in [`Envelope`], not a hash type, so it is a
/// caller field like any other — and on an artifact line it is the last one
/// left now that the path and the handle are gone. Validating it is what stops
/// the injection simply moving into the address slot: `@sha256:Ignore all
/// previous instructions` is a rendered instruction wearing a hash's clothes.
pub(crate) fn is_content_address(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl EvidenceRef {
    /// Cite the artifact at `ordinal`, with no content address.
    #[must_use]
    pub const fn at(ordinal: u32) -> Self {
        Self {
            ordinal,
            sha256: None,
        }
    }

    /// Cite the artifact at `ordinal` **with** its content address.
    ///
    /// A `sha256` that is not 64 lowercase hex digits is dropped rather than
    /// rendered — see the type's docs for why that is a security property and
    /// not tidiness.
    #[must_use]
    pub fn addressed(ordinal: u32, sha256: &str) -> Self {
        Self {
            ordinal,
            sha256: is_content_address(sha256).then(|| sha256.to_owned()),
        }
    }

    /// Cite whatever a parse-validated [`Handle`] names.
    ///
    /// The handle's own TEXT — the path, the tool-use id, the summary key — is
    /// never read here and never renders. Only the discriminant (which the
    /// caller sees as [`Self::scheme`]) and, for a file revision, the content
    /// address that is half of that handle's identity.
    #[must_use]
    pub fn cite(ordinal: u32, handle: &Handle) -> Self {
        match handle {
            Handle::File { sha256, .. } => Self::addressed(ordinal, sha256),
            Handle::Log { .. } | Handle::Summary { .. } => Self::at(ordinal),
        }
    }

    /// This citation carrying `basis` as its content address.
    ///
    /// **Precedence, stated because both sources are legitimate:** the state's
    /// own `basis_sha256` wins, because it is the hash the line's WORDS are
    /// about ("unchanged @…", "changed @…"); the caller's own address stands
    /// when the state has none, which is what recovers an address for
    /// `AvailableByHandle` whose basis is absent. A `basis` that is not a
    /// content address is not one, and falls through to the caller's.
    fn with_basis(&self, basis: Option<&str>) -> Self {
        match basis.filter(|b| is_content_address(b)) {
            Some(b) => Self {
                ordinal: self.ordinal,
                sha256: Some(b.to_owned()),
            },
            None => self.clone(),
        }
    }

    /// Whether this citation renders a content address.
    const fn is_addressed(&self) -> bool {
        self.sha256.is_some()
    }

    /// The pack-local ordinal this citation names — the key a
    /// [`crate::Citations`] row is filed under, and the number an agent sends
    /// back to `retrieve_exact`.
    #[must_use]
    pub const fn ordinal(&self) -> u32 {
        self.ordinal
    }

    /// The closed scheme tag of `handle` — `file`, `log` or `summary`. Rift's
    /// own three-word vocabulary, derived from the discriminant alone, so it
    /// says what KIND of record backs a citation without quoting the record.
    #[must_use]
    pub const fn scheme(handle: &Handle) -> &'static str {
        match handle {
            Handle::File { .. } => "file",
            Handle::Log { .. } => "log",
            Handle::Summary { .. } => "summary",
        }
    }
}

impl core::fmt::Display for EvidenceRef {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{EVIDENCE_TAG}{}", self.ordinal)?;
        match &self.sha256 {
            Some(h) => write!(f, "@sha256:{h}"),
            None => Ok(()),
        }
    }
}

/// The closed vocabulary of edge-placed critical facts. Facts are *derived
/// provenance*, never caller text — adding a new kind of critical fact means
/// adding a variant here, in review.
///
/// **No variant carries a path**: the two that did now carry an
/// [`EvidenceRef`], and the third carried a path only to hold a scope label its
/// two callers spelled as a constant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CriticalFact {
    /// The artifact's latest provenance carries a `reverted` outcome label.
    RevertedProvenance {
        /// Which artifact, by pack-local ordinal.
        evidence: EvidenceRef,
    },
    /// A path policy classifies as sensitive was touched in the reviewed
    /// range — the one risk a reviewer must not miss in the middle of a pack.
    SensitivePathTouched {
        /// Which artifact, by pack-local ordinal.
        evidence: EvidenceRef,
    },
    /// The attribution split over the pack's range is not confident enough to
    /// read as settled (the number rides so the agent can judge, not just the
    /// flag).
    ///
    /// **Range-scoped, not per-artifact.** It carried a `path` that both
    /// production callers filled with a constant scope label (`"(window
    /// rollup)"`, `"(review scope)"`) — a caller-text field with no fact behind
    /// it, which is a door standing open for the first caller that puts a real
    /// path in it. The rollup this fact reports has no single artifact, so the
    /// field is gone rather than ordinal-ized into a citation of nothing.
    LowAttributionConfidence {
        /// The confidence percentage behind the split.
        percent: u8,
    },
    /// The pack carries findings that are notes, not facts — said at both edges
    /// so the trust boundary survives the U-curve rather than sitting once in
    /// the middle where the reader is least likely to register it.
    ///
    /// It carries **no count**, deliberately. The caller knows how many
    /// findings it *offered*; only the compiler knows how many the finished
    /// pack *holds*, and those differ the moment the budget drops one. So the
    /// number is the compiler's: [`crate::compile`] fills it in from the
    /// assembled pack (and omits the line fully when no note survived),
    /// which makes a stale count unrepresentable rather than merely unlikely.
    /// What the pack LOST is said by the findings section's own budget marker;
    /// between them, present and total are both stated and neither is a claim.
    UnverifiedFindingsPresent,
}

/// The closed vocabulary of review-scope facts: what a [`crate::PackKind::Review`]
/// pack is *about*. Identifiers and counts only — there is no free-text variant,
/// so a caller cannot narrate the scope into the pack.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReviewScopeFact {
    /// The commit range under review, by revision identifier.
    ///
    /// **A DECLARED GAP: two rendered `String`s with no production caller.**
    /// A review census found it — `rg ReviewScopeFact::CommitRange` outside
    /// the pack crate's own tests returns nothing, so today only fixtures build
    /// it. That is the same "a field with no fact behind it" shape that
    /// justified deleting [`CriticalFact::LowAttributionConfidence`]'s path,
    /// and it is NOT deleted here for a reason worth stating rather than
    /// assuming: `rift review --branch` is a shipped flag whose scope this
    /// variant exists to name, and `BranchScope` already resolves the range —
    /// so this is an unwired feature, not a vestige. The name-by-ordinal rule
    /// is unaffected: the fields are revision identifiers, not paths, and `review_scope`
    /// neutralizes them. If the wiring never lands, the variant goes.
    CommitRange {
        /// The base revision (exclusive), as the caller names it.
        base: String,
        /// The head revision (inclusive).
        head: String,
    },
    /// How much sits in that range.
    Volume {
        /// Commits in the range.
        commits: u32,
        /// Capture sessions that touched them.
        sessions: u32,
        /// Distinct files touched.
        files: u32,
    },
}

/// The closed vocabulary of derived review signals — the numbers Rift computed
/// from its own planes (attribution, four-class cost, savings, outcome labels),
/// which is exactly the data the legacy backend's review never had. Every variant is
/// arithmetic: a *judgment* is unrepresentable here, because judgments belong to
/// the user's agent — Rift compiles, agents reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewSignal {
    /// The attribution split over the range, in lines.
    AttributionSplit {
        /// Lines attributed to the agent.
        ai_lines: u32,
        /// Lines attributed to a human edit of agent output.
        mixed_lines: u32,
        /// Lines attributed to the human alone.
        human_lines: u32,
    },
    /// Confidence in that split, in whole percent (clamped to `0..=100`).
    AttributionConfidence {
        /// The confidence percentage.
        percent: u8,
    },
    /// Failing-test history over the range.
    FailingTests {
        /// Test runs that failed.
        failures: u32,
        /// Commits the window covers.
        window_commits: u32,
    },
    /// Four-class token spend over the range.
    TokenSpend {
        /// Fresh input tokens.
        input: u64,
        /// Output tokens.
        output: u64,
        /// Cache-read tokens.
        cache_read: u64,
        /// Cache-write tokens.
        cache_write: u64,
    },
    /// Recall savings reported NET of what injection itself spent.
    /// The net is signed: a pack must be able to say the savings were negative.
    RecallSavings {
        /// Tokens the recall plane avoided re-reading.
        tokens_avoided: u32,
        /// Tokens injection spent to achieve that.
        tokens_injected: u32,
    },
}

/// The closed vocabulary of investigation seeds — what an investigation starts
/// from. Identifiers only, and deliberately **no text variant**: free text and
/// issue content reach a pack solely as the trust chokepoint's neutral digest
/// (behind `--allow-untrusted-seed`), so "seed as prose" is not
/// something the type system can express, flag or no flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SeedDescriptor {
    /// A repo-relative path.
    ///
    /// **It carries no path**, and that is deliberate rather than an
    /// omission. A seed has no content address — `investigate`'s own classifier
    /// says so: *"A seed carries no hash, so this arm can only match the
    /// path"* — so it cannot ride as `evidence:<n>@sha256:<hex>` the way a
    /// cited artifact does. And `rift investigate` returns `pack.render()` AS
    /// its human output, so there is no second surface to park an ordinal→path
    /// map on. What rides is therefore the KIND of seed and nothing else.
    ///
    /// The payload is gone rather than kept-and-unrendered because a closed
    /// vocabulary whose variant holds a field no constructor may render is a
    /// trap: the next reader re-renders it, and the sealing was the whole
    /// point. The considered alternative — give the seed a content address and
    /// make a hashless seed untrusted — was rejected on the merits: the
    /// attacker who writes the file also reads it, so they control the name
    /// AND the content, and both halves would match.
    Path,
    /// A checkpoint, by id.
    Checkpoint {
        /// The checkpoint id.
        id: String,
    },
    /// An error signature, by the content hash of its text. The signature TEXT
    /// is untrusted tool output; its hash is not, so the hash is what rides.
    ErrorSignature {
        /// Hex SHA-256 of the signature text.
        sha256: String,
    },
}

/// The semantic node kinds an investigation finding may propose.
/// Kept deliberately equal to the set the designed semantic plane names — an
/// investigation finding is a candidate semantic node, not a fifth kind of thing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FindingKind {
    /// A decision the investigation reached.
    Decision,
    /// A lesson worth carrying forward.
    Lesson,
    /// An intent recovered from the record.
    Intent,
    /// An error signature the investigation identified.
    ErrorSignature,
}

impl FindingKind {
    /// The stable lowercase tag this kind renders as.
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::Decision => "decision",
            Self::Lesson => "lesson",
            Self::Intent => "intent",
            Self::ErrorSignature => "error_signature",
        }
    }
}

/// The validated *shape* of a finding an agent proposed through the
/// `investigate` skill. Rift validates the shape and stamps the provenance
/// handle; it never validates the claim — which is exactly why the result is a
/// note and not a fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindingShape {
    /// Which semantic node kind the finding proposes.
    pub kind: FindingKind,
    /// The evidence the finding cites, as a parse-validated handle. It is
    /// required, not optional: a finding that cites nothing cannot be built, so
    /// evidence-gating starts at the type instead of at a policy check.
    ///
    /// **Carried for RESOLUTION, and unrenderable by contract.**
    /// The caller resolves it against the append-only record — `file_revision`
    /// on path AND content address, `tool_call` on id, `summary_binding` on key
    /// — and the pack renders only what [`EvidenceRef::cite`] extracts from it:
    /// an ordinal, the scheme discriminant, and (for a file revision) the hash
    /// half of its identity. The path half never reaches a rendered line, here
    /// or anywhere else in [`SectionEntry`].
    pub evidence: Handle,
}

/// The closed vocabulary of cost-awareness notes (descriptive only — a
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

/// The tag an artifact line opens with, ahead of its citation.
pub const FILE_TAG: &str = "file: ";

/// The tag every [`EvidenceRef`] renders behind — the one spelling of a
/// pack-local citation, so a reader (and a test) has a single token to look for.
pub const EVIDENCE_TAG: &str = "evidence:";

/// The tag the counted "what this section lost" line opens with.
pub const BUDGET_TAG: &str = "budget: ";

/// The closed set of line openers a compiled pack can produce — the pack's
/// grammar, as data.
///
/// Every line of every [`crate::PackKind`] begins with one of these, and none
/// of them is reachable from a caller-supplied field: each constructor renders
/// its identifiers *behind* its tag, so an attacker who controls a path, a
/// handle, a revision name or a seed can land text inside a structural line but
/// can never begin one. The one non-tag member is [`COST_NOTE`], a fixed
/// sentence with no caller input at all.
///
/// The exception, named rather than hidden: the exact-content state renders a
/// tagged header line followed by TRUSTED source bytes, which passed the
/// chokepoint and are the one thing in a pack allowed to span lines.
pub const LINE_OPENERS: [&str; 10] = [
    "CRITICAL: ",
    FILE_TAG,
    BUDGET_TAG,
    "retrieve: ",
    "untrusted ",
    "scope: ",
    "signal: ",
    "seed: ",
    "finding: unverified ",
    COST_NOTE,
];

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

/// The shared rendering of a [`NeutralDigest`]: its kind, its descriptive
/// counts, an optional retrieval name and (if one fired) the note that a
/// directive was withheld. Every untrusted-provenance entry — a tool result, a
/// transcript, an investigation seed, an agent's finding — renders through
/// this one body, so there is a single place where "what an untrusted artifact
/// is allowed to say" is decided.
///
/// **`retrieve` is a parameter, and the digest's own `handle()` is not read
/// here**. The serve path filled that handle with
/// `file:{path}@sha256:{sha}`, which made this body the second of the two
/// renderers that still put a repo path in front of an agent. A caller now
/// passes the name it wants rendered — a pack-local citation for an artifact
/// the pack cites, `None` for a digest that names nothing retrievable — so a
/// path cannot arrive through a field this function reads.
fn neutral_body(d: &NeutralDigest, retrieve: Option<&str>) -> String {
    let facts: Vec<String> = d
        .facts()
        .iter()
        .map(|f| format!("{}={}", f.key(), f.value()))
        .collect();
    let note = d
        .redaction_note()
        .map(|n| format!(" [{n}]"))
        .unwrap_or_default();
    let retrieve = retrieve.map_or_else(String::new, |r| format!(" - retrieve {r}"));
    format!("{}: {}{retrieve}{note}", d.kind(), facts.join(", "))
}

/// One trust-shaped pack line (or block): private fields, sealed constructors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionEntry {
    text: String,
    est_tokens: u32,
    class: EntryClass,
    /// The handle line this entry degrades to under budget pressure (the
    /// spill: inline content becomes `available_by_handle`, NEVER a truncated
    /// slice of itself). Prepared at construction from the same sealed
    /// inputs; `None` for entries already minimal (handles, facts, notes).
    spill_text: Option<String>,
    /// Whether this entry states how many unverified notes the pack carries —
    /// a number only the finished pack knows, so [`crate::compile`] re-renders
    /// it at assembly (see [`Self::with_live_count`]).
    counts_findings: bool,
}

/// The closed set of characters an identifier field may not carry into a
/// rendered pack line. Three hazards, one rule — each becomes a single space:
///
/// * **line breakers** — the C0/C1 controls ([`char::is_control`], so `\n`,
///   `\r`, VT, FF and NEL) *plus* `U+2028` LINE SEPARATOR and `U+2029`
///   PARAGRAPH SEPARATOR. Guarding controls alone was not enough: `str::lines`
///   does not split on U+2028/U+2029, so the compiler counted one line where
///   an agent's renderer showed two — a line the compiler never framed.
/// * **reorderers** — the bidi controls (`U+061C`, `U+200E`, `U+200F`,
///   `U+202A`–`U+202E`, `U+2066`–`U+2069`), which make a line *display* in an
///   order its bytes do not have.
/// * **hiders** — the zero-width and invisible formatting characters
///   (`U+00AD`, `U+180E`, `U+200B`–`U+200D`, `U+2060`–`U+2064`,
///   `U+206A`–`U+206F`, `U+FEFF`, `U+FFF9`–`U+FFFB`, `U+1D173`–`U+1D17A`) and
///   the whole TAG block (`U+E0000`–`U+E007F`), whose code points spell ASCII
///   while rendering as nothing at all — so `Sy<ZWSP>stem:` reads as `System:`
///   to a human and matches no marker a defense looks for.
///
/// What this is **not**, stated so no caller over-reads it: it is not a Unicode
/// general-category implementation, and it is not a homoglyph defense. A
/// Cyrillic `С` still renders as `С` and combining marks still combine. Neither
/// can break, hide or reverse a line, and neither can *open* one — every
/// constructor below puts a compiler tag in front of the field, which is the
/// half of the defense that does not depend on enumerating anything.
pub(crate) fn is_invisible_or_line_breaking(c: char) -> bool {
    c.is_control()
        || matches!(c,
            '\u{00AD}'                  // soft hyphen
            | '\u{061C}'                // arabic letter mark (bidi)
            | '\u{180E}'                // mongolian vowel separator
            | '\u{200B}'..='\u{200F}'   // ZWSP, ZWNJ, ZWJ, LRM, RLM
            | '\u{2028}' | '\u{2029}'   // line / paragraph separator
            | '\u{202A}'..='\u{202E}'   // bidi embeddings + overrides
            | '\u{2060}'..='\u{2064}'   // word joiner, invisible operators
            | '\u{2066}'..='\u{206F}'   // bidi isolates + deprecated formatting
            | '\u{FEFF}'                // zero-width no-break space / BOM
            | '\u{FFF9}'..='\u{FFFB}'   // interlinear annotation
            | '\u{1D173}'..='\u{1D17A}' // musical formatting
            | '\u{E0000}'..='\u{E007F}' // TAG block (invisible ASCII spellings)
        )
}

/// Replace every [`is_invisible_or_line_breaking`] character with a space, so
/// an attacker-influenceable field — a repo path, a retrieval handle, a
/// summary digest — renders as exactly **one** visible line whose display order
/// is its byte order. It therefore cannot forge a standalone role-shaped line
/// at any placement, including the duplicated critical edges, and
/// cannot smuggle text past a reader's eyes.
///
/// Applied to identifier-class fields only. Legitimately multi-line **trusted**
/// content (exact hunk text, which passed the trust chokepoint) is never routed
/// through it — that content is the one thing in a pack allowed to span lines.
fn sanitize_field(s: &str) -> String {
    s.chars()
        .map(|c| {
            if is_invisible_or_line_breaking(c) {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// The [`CriticalFact::UnverifiedFindingsPresent`] line, with the compiler's
/// count when there is one to give. Before assembly there is no honest number
/// to state, so the line states none rather than a caller's guess.
fn unverified_findings_line(count: Option<u32>) -> String {
    count.map_or_else(
        || {
            "CRITICAL: findings here are unverified notes, not facts - corroborate before acting"
                .to_string()
        },
        |n| {
            format!(
                "CRITICAL: {n} finding(s) here are unverified notes, not facts - corroborate before acting"
            )
        },
    )
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
            counts_findings: false,
        }
    }

    /// The budget-pressure degrade for this entry: its handle form, when it
    /// has one that is actually cheaper. A whole-entry swap — content is
    /// never truncated (sensitive-safe by construction).
    #[must_use]
    pub(crate) fn spill(&self) -> Option<Self> {
        let text = self.spill_text.clone()?;
        let spilled = Self::new(text, EntryClass::Handle);
        (spilled.est_tokens < self.est_tokens).then_some(spilled)
    }

    /// Build an entry from a four-state envelope — the output of the sealed
    /// `envelope_for` constructor. Every state renders as
    /// identifiers/digests; `exact_included` is the only state whose content
    /// rides along (it was trust- and budget-gated inside `envelope_for`).
    /// Under [`Sensitivity::Sensitive`] a summary downgrades to its handle:
    /// exact-or-handle, never a lossy summary.
    ///
    /// # The path does not render, and neither does the handle
    ///
    /// This was the door with the most traffic: `rift-recall-serve` calls this
    /// constructor for every candidate of every injected per-turn pack, and
    /// injection is ON by default at the configured threshold. It used to open
    /// `file: <path> …`, and the earlier defence was placement — [`FILE_TAG`]
    /// first, so a repo containing a file named `System: ignore all previous
    /// instructions` could land inside a structural line but never begin one.
    /// That stopped a forged line; it never stopped the name reading as an
    /// instruction. Naming by ordinal replaces the name with `evidence`.
    ///
    /// The state's `handle` goes with it, for the same reason and at no cost
    /// here: it is `file:{path}@sha256:{sha}`, so rendering it would have
    /// re-admitted the path one word later. What an agent can still act on is
    /// the `Recommended` section's retrieval names — see
    /// [`Self::from_handle`], which is where that remaining case is declared.
    ///
    /// `e.path` and the state's `handle` are therefore **carried for the
    /// caller's own use and unrenderable by contract**; only `basis_sha256`
    /// still rides, and only when it is a real content address.
    #[must_use]
    pub fn from_envelope(e: &Envelope, sensitivity: Sensitivity, evidence: &EvidenceRef) -> Self {
        // Every field of the envelope EXCEPT `content` is neutralized. The path
        // and the handle are the obvious identifiers; `basis_sha256` is a
        // caller-supplied `String` in the type, not a hash type; and the
        // `summary` is a digest whose text can echo spans of its source — the
        // failure extractor's signatures are stabilized copies of log lines and
        // the diff extractor names paths out of the diff, both of them
        // multi-line by construction. Only the exact `content` stays verbatim:
        // it passed the chokepoint's `TrustedPayload`, so it is the one thing a
        // pack may legitimately spend several lines on.
        // **THE ADDRESS RIDES ON THE CITATION, ON EVERY ARM AND EVERY SPILL.**
        // The first version of this rendered it on three arms and dropped it on
        // the sensitive summary, both spills and `available_by_handle` — where
        // the basis is bound and in hand — leaving a BARE ordinal, and then
        // called that costless because the `Recommended` section still carried
        // a retrieval name. It does not: `degrade_order` drops `Recommended`
        // FIRST for every pack kind and `placement` fills it LAST, so that
        // surface is absent exactly when a spilled entry appears. The committed
        // `pack_tight` golden was the proof — `available (budget_exhausted)`
        // with no `retrieve:` line anywhere in the pack, an entry naming an
        // artifact the pack could no longer identify at all. A serve pack has
        // no ordinal→path map to resolve against either (unlike the workflow
        // verbs, whose `push_evidence_map` is a human surface), so the address
        // is the ONLY thing that makes a citation resolvable there.
        let cite = |basis: Option<&str>| format!("{FILE_TAG}{}", evidence.with_basis(basis));
        match &e.state {
            ArtifactState::KnownUnchanged { basis_sha256, .. } => {
                let cited = evidence.with_basis(Some(basis_sha256));
                Self::new(
                    if cited.is_addressed() {
                        format!("{FILE_TAG}{cited} unchanged (known_unchanged; not re-inlined)")
                    } else {
                        // The hash IS the claim in this state, so a missing one
                        // is said out loud rather than left as an absence a
                        // reader has to notice.
                        //
                        // Keyed on the CITATION and not on `basis_sha256`,
                        // because those are different questions the moment a
                        // caller supplies its own address: a malformed basis
                        // beside a valid caller address would otherwise render
                        // a line that carries `@sha256:…` and says the address
                        // is unavailable in the same breath. No such caller
                        // exists today; the contradiction is unrepresentable
                        // rather than merely unreached.
                        format!(
                            "{FILE_TAG}{cited} unchanged \
                             (known_unchanged; content address unavailable)"
                        )
                    },
                    EntryClass::KnownUnchanged,
                )
            }
            ArtifactState::SummaryIncluded {
                basis_sha256,
                summary,
                ..
            } => {
                let cite = cite(Some(basis_sha256));
                match sensitivity {
                    Sensitivity::Sensitive => Self::new(
                        format!("{cite} available (sensitive; summary withheld)"),
                        EntryClass::Handle,
                    ),
                    // **A DECLARED, DORMANT GAP — NOT CLOSED.** This is
                    // the one remaining field on an artifact line that renders
                    // caller TEXT, and this file's own note above says why it
                    // matters: a summary is a digest whose text can echo spans
                    // of its source (the failure extractor stabilizes log
                    // lines, the diff extractor names paths out of the diff).
                    // It is unreachable on the shipped serve path only because
                    // `SummaryStore::new()` is always empty there, so `resolve`
                    // verdicts `Degraded` and this arm is never taken —
                    // dormant, which is not the same as closed. When summaries
                    // are persisted, this arm needs the name-by-ordinal treatment
                    // (or a chokepoint) BEFORE it goes live.
                    Sensitivity::Normal => Self::with_spill(
                        format!("{cite} summary: {}", sanitize_field(summary)),
                        EntryClass::SummaryBound,
                        Some(format!("{cite} available (budget_exhausted)")),
                    ),
                }
            }
            ArtifactState::ExactIncluded {
                basis_sha256,
                content,
                ..
            } => {
                let cite = cite(Some(basis_sha256));
                Self::with_spill(
                    format!("{cite} changed:\n{content}"),
                    EntryClass::ExactContent,
                    Some(format!("{cite} available (budget_exhausted)")),
                )
            }
            ArtifactState::AvailableByHandle {
                basis_sha256,
                reason,
                ..
            } => {
                let cite = cite(basis_sha256.as_deref());
                Self::new(
                    format!("{cite} available ({})", omit_tag(*reason)),
                    EntryClass::Handle,
                )
            }
        }
    }

    /// Build an entry from a neutral digest — the output of the trust
    /// chokepoint (`serialize_untrusted`). The digest type itself
    /// carries no payload text, so neither can the entry.
    ///
    /// # The digest's handle does not render
    ///
    /// This constructor was the untrusted-provenance twin of
    /// [`Self::from_handle`]: `rift-recall-serve` handed the chokepoint
    /// `file:{path}@sha256:{sha}` as the digest's retrieval handle and this
    /// line printed it, which put an attacker-influenceable repo path in front
    /// of an agent on the highest-volume pack in the product. The rule now replaces
    /// it with a pack-local citation — see [`Self::from_neutral_cited`], which
    /// is what the serve path calls. A digest that names nothing retrievable
    /// (an untrusted investigation seed, an agent's finding body) renders here,
    /// with no retrieval clause at all rather than a name that resolves to
    /// nothing.
    #[must_use]
    pub fn from_neutral(d: &NeutralDigest) -> Self {
        // The whole line is identifiers + counts (the digest carries no payload
        // text); neutralize it so a caller-supplied field can never break the
        // line even if it slipped a control char past its own validation.
        Self::new(
            sanitize_field(&format!("untrusted {}", neutral_body(d, None))),
            EntryClass::NeutralDigest,
        )
    }

    /// [`Self::from_neutral`] for a digest that stands for an artifact the pack
    /// **cites**: the retrieval clause renders the pack-local citation, never
    /// the digest's handle.
    ///
    /// The citation is the same [`EvidenceRef`] the artifact's own lines carry,
    /// so a reader can join the untrusted digest to the file line it is about
    /// without either of them naming a file — and the number is what the agent
    /// sends back to `retrieve_exact`, which resolves it through the compiled
    /// pack's [`crate::Citations`] table and gates the resolved path on policy
    /// before any bytes move.
    #[must_use]
    pub fn from_neutral_cited(d: &NeutralDigest, cite: &EvidenceRef) -> Self {
        Self::new(
            sanitize_field(&format!(
                "untrusted {}",
                neutral_body(d, Some(&cite.to_string()))
            )),
            EntryClass::NeutralDigest,
        )
    }

    /// Build an investigation-seed entry from the closed descriptor vocabulary.
    /// Every line opens with the `seed:` tag, so a hostile identifier lands
    /// inside a structural line rather than starting one.
    #[must_use]
    pub fn seed(descriptor: &SeedDescriptor) -> Self {
        let text = match descriptor {
            SeedDescriptor::Path => "seed: path".to_owned(),
            SeedDescriptor::Checkpoint { id } => format!("seed: checkpoint {}", sanitize_field(id)),
            SeedDescriptor::ErrorSignature { sha256 } => {
                format!("seed: error signature @sha256:{}", sanitize_field(sha256))
            }
        };
        Self::new(text, EntryClass::SeedRef)
    }

    /// Build an **unverified** finding note: the chokepoint's
    /// neutral digest of what an agent proposed, tagged with the validated
    /// shape and a citation of the evidence it had to name. The finding's own
    /// words never render — only counts and citations do — which is what makes
    /// a hostile "finding" inert rather than merely discouraged. The note says
    /// `unverified` in every rendering; promotion to a fact would be a
    /// corroboration gate, and it does not live in the pack compiler.
    ///
    /// # The handle does not render, and this is the door that was open
    ///
    /// It used to: `shape.evidence`'s `Display` IS
    /// `file:{path}@sha256:{sha}`, so a finding citing a captured
    /// instruction-shaped path wrote that instruction into the pack. Resolution
    /// against the append-only record raised the cost — the path had to have
    /// been captured — and two ordinary tool calls (write a file, read it) pay
    /// it. So the handle is cited by `evidence`: its ordinal, its scheme, and
    /// its content address; never its name.
    #[must_use]
    pub fn unverified_finding(
        shape: &FindingShape,
        evidence: &EvidenceRef,
        digest: &NeutralDigest,
    ) -> Self {
        Self::new(
            sanitize_field(&format!(
                "finding: unverified {} (cites {} {evidence}) - {}",
                shape.kind.tag(),
                EvidenceRef::scheme(&shape.evidence),
                // The finding's OWN citation is `evidence` above. The digest's
                // handle names the transcript the note came out of — a
                // caller-supplied string that `rift investigate` fills with a
                // constant or a `log:` tool-use id, and that nothing validates.
                // It was the ninth renderer the census found:
                // closed 2026-08-29 by never reading it here, so the only
                // retrievable name on this line is the ordinal in `evidence`.
                neutral_body(digest, None),
            )),
            EntryClass::UnverifiedNote,
        )
    }

    /// Build a retrieval-handle entry from a parse-validated [`Handle`] and the
    /// pack-local ordinal that artifact was assigned — free-text "handles"
    /// cannot ride (they would not have parsed).
    ///
    /// # The retrieval name is an ordinal
    ///
    /// This line was the last place a repo path reached an agent-facing pack,
    /// and the reason it survived the name-by-ordinal rule is that **this line's whole job is
    /// to be a name the agent sends back**: a recommendation exists so the agent
    /// can call `retrieve_exact` with it, `Handle::File`'s address is `path` AND
    /// `sha256`, and resolving on the hash alone is a security regression
    /// (exact retrieval is policy-gated on the path, so a hash-only name lets a
    /// caller fetch a sensitive file the gate never sees). A pack-local ordinal
    /// could not be sent back either, because the server held no table to
    /// resolve one against.
    ///
    /// It holds one now. Each compiled pack carries a [`crate::Citations`] table
    /// keyed by its `PackKey`; the agent sends `pack:<key>/evidence:<n>`;
    /// `retrieve_exact` resolves the ordinal to `(path, content_sha256)`,
    /// applies the **existing path-based policy gate to the resolved path**
    /// before any bytes are read, and binds both halves against the record. The
    /// table is pack-local and never rides the wire, so the path is gone from
    /// the agent's side of the exchange without the gate losing sight of it.
    ///
    /// `h` is therefore **carried for the caller's own resolution and
    /// unrenderable by contract** — only the ordinal, and (for a file revision)
    /// the hash half of that handle's identity, reach the rendered line. The
    /// address is taken *from the handle* rather than from a caller-built
    /// citation, so a recommendation cannot advertise an address its handle
    /// does not have.
    #[must_use]
    pub fn from_handle(h: &Handle, ordinal: u32) -> Self {
        Self::new(
            format!("retrieve: {}", EvidenceRef::cite(ordinal, h)),
            EntryClass::Handle,
        )
    }

    /// Build an edge-placed critical fact from the closed vocabulary.
    ///
    /// **Nothing here is caller text any more.** The two artifact-scoped facts
    /// name a pack-local [`EvidenceRef`] — digits and a validated hex address —
    /// and the range-scoped one is a number. There is therefore no field at
    /// these placements for a repo filename to arrive in, which is a stronger
    /// statement than the neutralization it replaces: `sanitize_field` stopped a
    /// hostile name forging a second role-shaped line at the duplicated critical
    /// edges, but it could not stop the name itself reading as an
    /// instruction inside the line.
    #[must_use]
    pub fn critical(fact: &CriticalFact) -> Self {
        let text = match fact {
            CriticalFact::RevertedProvenance { evidence } => {
                format!("CRITICAL: {evidence}'s last provenance was reverted - verify before reuse")
            }
            CriticalFact::SensitivePathTouched { evidence } => {
                format!("CRITICAL: {evidence} is a sensitive path touched in this range")
            }
            CriticalFact::LowAttributionConfidence { percent } => format!(
                "CRITICAL: attribution over this range is {}% confident - the split is not settled",
                (*percent).min(100),
            ),
            // The count is the compiler's, not the caller's: this renders
            // countless and `with_live_count` fills it in at assembly.
            CriticalFact::UnverifiedFindingsPresent => {
                return Self {
                    counts_findings: true,
                    ..Self::new(unverified_findings_line(None), EntryClass::CriticalFact)
                }
            }
        };
        Self::new(text, EntryClass::CriticalFact)
    }

    /// Re-render an edge fact against what the compiled pack actually **holds**
    /// (advisory A4). Only [`CriticalFact::UnverifiedFindingsPresent`] answers
    /// to this; every other entry passes through unchanged.
    ///
    /// `None` means the fact does not apply and the line is dropped: a pack
    /// that carries no unverified note must not warn about unverified notes,
    /// however many the caller offered. The offered-but-dropped ones are
    /// counted by the findings section's own budget marker, so nothing goes
    /// unsaid — it is just said by whichever part of the pack knows it.
    pub(crate) fn with_live_count(&self, findings: u32) -> Option<Self> {
        if !self.counts_findings {
            return Some(self.clone());
        }
        (findings > 0).then(|| Self {
            counts_findings: true,
            ..Self::new(
                unverified_findings_line(Some(findings)),
                EntryClass::CriticalFact,
            )
        })
    }

    /// Build a review-scope entry from the closed vocabulary. Every line opens
    /// with the `scope:` tag, so a hostile revision *identifier* lands inside a
    /// structural line rather than starting one (the sanitizer keeps it on one
    /// line; the tag keeps it off the front).
    #[must_use]
    pub fn review_scope(fact: &ReviewScopeFact) -> Self {
        let text = match fact {
            ReviewScopeFact::CommitRange { base, head } => format!(
                "scope: commits {}..{}",
                sanitize_field(base),
                sanitize_field(head)
            ),
            ReviewScopeFact::Volume {
                commits,
                sessions,
                files,
            } => format!("scope: {commits} commit(s), {sessions} session(s), {files} file(s)"),
        };
        Self::new(text, EntryClass::ScopeFact)
    }

    /// Build a review-signal entry from the closed vocabulary. Signals are pure
    /// arithmetic over Rift's own planes — there is no identifier to sanitize
    /// and no prose to admit.
    #[must_use]
    pub fn review_signal(signal: ReviewSignal) -> Self {
        let text = match signal {
            ReviewSignal::AttributionSplit {
                ai_lines,
                mixed_lines,
                human_lines,
            } => format!(
                "signal: attribution ai={ai_lines} mixed={mixed_lines} human={human_lines} lines"
            ),
            ReviewSignal::AttributionConfidence { percent } => {
                format!("signal: attribution confidence {}%", percent.min(100))
            }
            ReviewSignal::FailingTests {
                failures,
                window_commits,
            } => format!("signal: failing tests {failures} across {window_commits} commit(s)"),
            ReviewSignal::TokenSpend {
                input,
                output,
                cache_read,
                cache_write,
            } => format!(
                "signal: tokens in={input} out={output} cache_read={cache_read} cache_write={cache_write}"
            ),
            ReviewSignal::RecallSavings {
                tokens_avoided,
                tokens_injected,
            } => format!(
                "signal: recall savings {tokens_avoided} avoided - {tokens_injected} injected = {} net",
                i64::from(tokens_avoided) - i64::from(tokens_injected)
            ),
        };
        Self::new(text, EntryClass::Signal)
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

    /// The entry's token estimate (from the counting authority).
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
