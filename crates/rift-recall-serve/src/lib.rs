//! The bounded recall tool surface + budget/policy middleware.
//!
//! A **closed** set of ten tools — there is no transcript search and no
//! open-ended session search. This crate is the transport-independent
//! dispatcher plus the real, store-backed handlers, so the whole surface is
//! testable offline. The stdio transport that used to
//! wire this dispatcher into an agent-spawned server process is retired; today `rift
//! recall <verb>` (the skill-CLI channel, `bins/rift/src/recall/serve.rs`) calls
//! [`decode_request`]/[`ServeDispatcher::dispatch`] directly, one process per
//! invocation rather than one long-lived server per session. The dispatcher
//! itself did not change — that is the point of a transport-independent seam.
//!
//! Every dispatch passes through middleware that enforces the per-turn budget
//! (calls and tokens), requires query-aware params on ranking-sensitive tools, gates exact
//! retrieval through policy with a mandatory reason, and produces
//! exactly one `memd.*` event — so every serve decision is audited (the
//! serve↔event bijection). The `returned`/`avoided` figures are sealed at the
//! **accounting seam**: the payload is serialized once, counted with
//! the real tokenizer, and charged against the turn budget in exactly one
//! place, so the figures are measured, not asserted.
//!
//! This file is the wire surface — the closed tool list, the request and
//! response types, the decoder and the dispatcher's middleware. The handlers
//! live under `verbs`, one module per verb; the declared native baselines a
//! document or a listing is priced at live in `pricing`; the view of a sealed
//! envelope and the trust, charge and audit vocabulary every verb shares live
//! in `envelope`. Every public name is re-exported here at the path it always
//! had.
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

/// Turning the prefix of a content hash a caller quotes back into the address
/// it names — or into "not known", which is never "unchanged".
pub mod basis;

/// The family's read-time basis snapshots: the redacted bytes of a revision a
/// window was handed, so a delta against it survives the edit that followed.
pub mod basis_store;
/// What a pull's caller claimed to hold, counted for its audit record.
mod claim;

/// The view of a sealed envelope, and the trust, charge and audit vocabulary
/// every verb answers in.
mod envelope;

/// Where a file changed since the revision a context window holds — the one
/// computation the manifest, the decline of a verification read and the pull
/// verb all read their line numbers from.
pub mod held_diff;

/// The declared native cost of the read or the listing a verb stands in for.
mod pricing;

/// What a delta carries beside the change: when it happened, who made it, what
/// the turn cost, and the command that answers why.
pub mod provenance;

/// The store-backed handlers, one module per verb, over the shaping and
/// accounting seam they share.
mod verbs;

/// Which lines around a changed place a reader is shown, decided without a
/// parser.
pub mod window;

use std::collections::BTreeMap;

use rift_core::Timestamp;
// Re-exported so a caller building an envelope — a schema-4 channel, an
// event, the neutral tags and identities an audit row carries —
// names one type through whichever of `rift-recall-serve` /
// `rift-recall-events` it already depends on — `rift-recall-events` is a
// dev-only dependency for `bins/rift` (its product code has never named the
// crate directly before this), and promoting it to a real one would move the
// crate-graph adjacency the reviewable xtask snapshot locks. This path
// adds no edge: both crates already depend on each other in production.
pub use rift_recall_events::{
    file_handle, grep_cap_handle, grep_cap_matches, read_handle, MemdEvent, ServeChannel,
    TAG_ALREADY_HELD, TAG_BASIS_UNAVAILABLE, TAG_CONVERT_DEADLINE, TAG_DECLARED_BASELINE,
    TAG_PREVENTED, TAG_PREVENTED_FIRST, TAG_PREVENTED_OBSERVED, TAG_TOKENS_UNMEASURED,
};
pub use rift_recall_ledger::grants::CROSS_BRANCH_GRANT_TAG;
use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::worktree::slash_spelled;
use rift_recall_substrate::SubstrateReader;
use rift_recall_types::{AncestryOracle, OmitReason, ProvenanceOracle, Scope};
pub use rift_recall_types::{ContextCandidate, ContextKind, ContextPlan};
/// The answer that stands in for a listing, and the two types it is written
/// from — re-exported by the same route and for the same reason as
/// [`render_deny_frame`]: a hook that answers a name search puts a sentence in
/// front of a model, and every such sentence is written by the one sealed
/// renderer. `bins/rift` reaches it here because this dispatcher is already the
/// validity crate's caller, so no crate edge is added.
pub use rift_recall_validity::trust::{render_search_answer, PathStatus, SearchHit};
use rift_store::CanonicalStore;
use serde_json::Value;

/// The push half of the trust chokepoint, re-exported for the hook path.
///
/// The renderer lives in `rift-recall-validity` beside the pull-side
/// serializer, which is what makes "manifest content is built exclusively
/// through the chokepoint" a fact about one file. `bins/rift` reaches it here
/// rather than depending on the validity crate directly: this dispatcher is
/// already its caller, so the re-export adds no crate edge — the same reason
/// [`injection_audit_envelope`] lives here.
/// The entry types travel with it: the renderer decides what an entry may
/// say, so the hook path names those states rather than spelling a line of its
/// own.
/// The push-side guard travels with them: the hook path decides which entry
/// the manifest's pull hint should name, and "one the guard will render" is
/// part of that question. Asking the guard is the only way to answer it that
/// cannot drift from what the renderer does.
pub use rift_recall_validity::trust::{
    neutral_pushed_span, render_delta_manifest, render_delta_manifest_naming, EntryDetail,
    ManifestEntry,
};

/// The reason frames a declined read is answered with, re-exported by the
/// same route and for the same reason as [`render_delta_manifest`].
///
/// The hook path composes a decline; the sentence a model reads is written by
/// the sealed renderer beside the manifest's, so there is one place in this
/// tree where pushed text comes into existence. `bins/rift` reaches it here
/// because this dispatcher is already the validity crate's caller — no crate
/// edge is added, and no second renderer is created.
pub use rift_recall_validity::trust::{
    render_delta_frame, render_deny_frame, render_held_output_frame, render_narrow_frame,
};

/// The first-contact frame, re-exported by the same route and for the same
/// reason as [`render_deny_frame`]: the binary supplies the outline it read
/// from its cache, and the sealed renderer writes every word of the sentence.
pub use rift_recall_validity::trust::{
    render_outline_frame, OutlineEntry, OutlinedRead, MAX_OUTLINE_FRAME_DEFINITIONS,
    OUTLINE_FRAME_MIN_PAGE_LINES,
};

/// The two frames a read another window of the repository holds is answered
/// with — its outline with the clause that says why, or the runs that window
/// holds — re-exported by the same route and for the same reason as
/// [`render_deny_frame`].
pub use rift_recall_validity::trust::{render_held_lines_frame, render_held_outline_frame};

/// The document redirect's frame, re-exported by the same route and for the
/// same reason as [`render_deny_frame`].
///
/// A read of a document Rift already holds as Markdown can be answered two
/// ways: by amending the path, which says nothing to the model, or by
/// declining the read and naming the sidecar — and the second one is a
/// sentence a model reads, so it is written where every other such sentence
/// is. `bins/rift` reaches it here for the reason it reaches the other two:
/// this dispatcher is already the validity crate's caller, so no crate edge is
/// added and no second renderer is created.
pub use rift_recall_validity::trust::render_redirect_frame;

/// The guard's turn-boundary vocabulary, re-exported for the same reason and by
/// the same route as [`render_delta_manifest`].
///
/// `bins/rift` reads it to check the one thing neither crate can check alone:
/// that every client family the installer writes hooks for has a declared turn
/// dialect (`bins/rift/tests/role_dialect_coverage.rs`). The table is the
/// guard's own — a second listing would be a second source of truth, which is
/// exactly the failure this export exists to end.
pub use rift_recall_validity::trust::{
    role_marker_spellings, RoleDialect, RoleMarker, ROLE_MARKERS,
};

/// The **pack spine**, re-exported by the same route and for the same reason as
/// [`render_delta_manifest`]: this dispatcher is already `rift-recall-pack`'s
/// caller, so `bins/rift` reading it here adds no crate edge.
///
/// The workflow verbs (`dispatch --for-agent`, `review`, `investigate`)
/// compile evidence packs, and they must compile them through the *same* sealed
/// constructors, layout table and budget ceiling the serve path uses. A second
/// route into the pack crate would be a second place where a section line could
/// come into existence — which is precisely the hole `EvidencePack` being
/// Serialize-only was written to close.
pub use rift_recall_pack as pack;

/// The untrusted-content chokepoint, re-exported by the same route.
///
/// The workflow verbs are pack surfaces, so any bytes they carry that are not
/// Rift's own facts must arrive as a [`NeutralDigest`]
/// — `rift investigate`'s free-text seed being the whole reason
/// `--allow-untrusted-seed` exists. One serializer, one place, reached one way.
pub use rift_recall_validity::trust::{
    is_directive_shaped, serialize_untrusted, NeutralDigest, NeutralFact,
};

/// The boundary map's renderer and the vocabulary it reads, re-exported by the
/// same route and for the same reason as [`render_delta_manifest`].
///
/// A context that has just been compacted is handed the list of what it
/// stopped holding, and the sentence it reads is written by the sealed
/// renderer beside the manifest's — so there stays one place in this tree
/// where pushed text comes into existence. `bins/rift` reaches it here because
/// this dispatcher is already the validity crate's caller: no crate edge is
/// added, and no second renderer is created.
pub use rift_recall_validity::trust::{
    render_recovery_map, render_recovery_map_naming, HeldEntry, HeldVerdict,
};

/// The shape line's source for the recovery map, by the same route: the
/// outline of the revision the checkout holds, read from the store the map
/// already opens.
pub use self::verbs::summaries::current_outline_shape;
pub use rift_recall_validity::OutlineShape;

/// The outline grammar the read gate's outline cache is written and read in:
/// the closed kind set, the path classes, the versions a binding is keyed by,
/// and the read-back of a valid summary's outline. Re-exported so the binary
/// writes the cache from the outline the projection holds and reads it back
/// by the same rules, with no second grammar.
pub use rift_recall_validity::{
    is_markdown_path, is_outlinable_path, line_span, outline_definitions, outline_shape,
    outline_versions, DefinitionKind, FileOutline, Validity,
};

/// The section a subagent's window opens with, naming what its coordinator
/// held at that instant, re-exported by the same route and for the same reason as
/// [`render_recovery_map`]: one sealed renderer writes every pushed sentence,
/// and this dispatcher is already its caller.
pub use rift_recall_validity::trust::{render_coordinator_reads_naming, CoordinatorRead};

/// Antigravity's two persistent-message sentences — the prepared result for a
/// declined read and the bounded `ContentOffset` plan — re-exported by the same
/// route and for the same reason: the hook binary calls them, and only the
/// sealed renderer writes them.
pub use rift_recall_validity::trust::{render_prepared_result, render_structural_read_plan};

pub use self::envelope::{
    exact_sha, grant_tagged_envelope, handle_display_path, injection_audit_envelope, omit_tag,
    provenance_trust,
};
pub use self::pricing::{
    declared_document_baseline, declared_search_baseline, priceable_listing, ListingShape,
    SearchCeiling, NATIVE_INLINE_CHARS, NATIVE_LISTING_PATHS, NATIVE_READ_TOKEN_CEILING,
    NO_CLIENT_FAMILY,
};
use self::verbs::targeted_pull;
pub use self::verbs::{
    DEFAULT_MANIFEST_TOKENS, MAX_CHANGED_HUNKS, MAX_MANIFEST_TOKENS, PACK_CANDIDATES,
};

/// The closed set of recall tools. This is the **whole** list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolName {
    /// Manifest: changed/unchanged files, hotspots, recommended retrievals.
    KnownState,
    /// Changed files + hunks since a checkpoint/commit/session.
    DiffSince,
    /// `unchanged` / `changed` / `invalid` for `(path, known_hash)`.
    FileIfChanged,
    /// Exact changed hunks under a token budget.
    ChangedHunks,
    /// Hash-bound summary by handle.
    ReadSummary,
    /// Exact source/tool output by handle (reason required; audited).
    RetrieveExact,
    /// Repeated reads, expensive calls, trends (awareness only).
    CostHotspots,
    /// Ordered, budgeted, position-aware evidence pack.
    ContextPack,
    /// The Markdown sidecar Rift holds for a document the caller names.
    ///
    /// The odd one out, and deliberately: its answer is not in the projection
    /// and not in the object database — it is a file in the family's sidecar
    /// store, which this crate cannot reach and must not learn to. The binary
    /// resolves it and hands it in through [`ToolRequest::document`]; this
    /// crate gates the path, runs the bytes through the trust chokepoint,
    /// names the state through the sealed constructor and seals the figures.
    DocRead,
    /// The paths in this checkout that a query is about, ranked — and, when the
    /// caller asks for them, the lines inside those paths that match.
    ///
    /// The second verb whose answer is not in the projection: the path index is
    /// a file in the member's own run state and the lines are read out of the
    /// work tree, neither of which this crate may touch. The binary opens the
    /// index, gathers the family's frecency, runs the engine and hands the
    /// result in through [`ToolRequest::search`]; this crate screens the paths
    /// against the serving policy, renders them as neutral spans, runs the
    /// matched lines through the trust chokepoint and seals the figures.
    Search,
}

/// The complete, bounded tool surface.
pub const TOOL_SURFACE: &[ToolName] = &[
    ToolName::KnownState,
    ToolName::DiffSince,
    ToolName::FileIfChanged,
    ToolName::ChangedHunks,
    ToolName::ReadSummary,
    ToolName::RetrieveExact,
    ToolName::CostHotspots,
    ToolName::ContextPack,
    ToolName::DocRead,
    ToolName::Search,
];

/// The fixed, neutral cost-awareness note (descriptive, never a directive).
/// Re-exported from the pack crate's closed [`rift_recall_pack::CostNote`]
/// vocabulary so the two surfaces can never drift.
pub const COST_NOTE: &str = rift_recall_pack::COST_NOTE;

/// The neutral tag on a read decision about a first contact: the first read
/// of a large file in a window, which the file's bound outline would answer.
/// Its own tag, so every surface can size the class, and take it out of a
/// figure, apart from the re-reads the read gate declines.
pub const TAG_FIRST_CONTACT: &str = "first_contact";

/// The neutral tag on a read of bytes a family decline of the same window had
/// been credited with: the read ran, and what it delivered is that decline's
/// cost. Its own tag so the row is told from the decline it is charged
/// against, and so every surface can net the two.
pub const TAG_FAMILY_FOLLOW_UP: &str = "family_held:followup";

/// The neutral tag on a document verb that had nothing to hand over yet: the
/// family holds no sidecar for these bytes, and the conversion is owed.
pub const TAG_SIDECAR_OWED: &str = "sidecar_owed";

/// The neutral tag on a document verb whose document the family decided
/// against — a page image with no text layer, a container that cannot be
/// parsed. Nothing will convert it later either.
pub const TAG_SIDECAR_DECLINED: &str = "sidecar_declined";

/// The neutral tag on a search this checkout has no index for yet. Neither a
/// refusal nor an empty answer: the rebuild is owed, it runs in the detached
/// worker, and the next call is answered.
pub const TAG_INDEX_PENDING: &str = "index_pending";

/// The neutral tag on a search answer that claims nothing, because nothing in
/// this tree declares what the native call it replaced would have cost — see
/// [`declared_search_baseline`].
pub const TAG_BASELINE_NOT_DECLARED: &str = "baseline_not_declared";

/// Why a request could not be decoded (a protocol error, surfaced to the agent).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The tool name is not one of the eight (no such tool).
    #[error("unknown tool")]
    UnknownTool,
}

/// A tool invocation. Optional fields are unused by tools that do not need them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolRequest {
    /// The task/goal (required for ranking-sensitive tools).
    pub task: Option<String>,
    /// Files in focus (required for ranking-sensitive tools).
    pub files_in_focus: Vec<String>,
    /// Error signatures currently in play (drives the error-overlap signal).
    pub error_signatures: Vec<String>,
    /// Token budget for the response.
    pub token_budget: u32,
    /// Content hashes the agent already has (drives `known_unchanged`).
    pub known_hashes: Vec<String>,
    /// The handle/path for retrieval/conditional tools.
    pub target: Option<String>,
    /// Reason (required for exact retrieval).
    pub reason: Option<String>,
    /// `top_n` for cost hotspots.
    pub top_n: Option<u32>,
    /// What the **binary** resolved for [`ToolName::DocRead`]: the document's
    /// content address, the declared native cost of reading it, and the
    /// sidecar the family holds for it (when it holds one).
    ///
    /// Never decoded from params — see [`decode_request`]. `None` on every
    /// other verb, and on a document call that reached this crate through a
    /// decoder, which is answered as an internal error rather than served.
    pub document: Option<Document>,
    /// What the **binary** resolved for [`ToolName::Search`]: what the member's
    /// path index had to say about the query, and the declared native cost of
    /// the listing the answer stands in for.
    ///
    /// Never decoded from params — see [`decode_request`]. `None` on every
    /// other verb, and on a search call that reached this crate through a
    /// decoder, which is answered as an internal error rather than served.
    pub search: Option<Search>,
    /// What the **binary** read out of the calling context window's own
    /// journal: for each revision, keyed `(path, content sha256)`, the turn the
    /// window last read it in and still holds it from.
    ///
    /// A `known_unchanged` entry cites it as `last_seen_turn`; a revision with
    /// no entry is cited with no turn, which is also what every entry gets when
    /// the pull could not be attributed to a window. It never decides a state —
    /// `known_hashes` does that — so a wrong map could mislabel a claim but
    /// could not make one. Never decoded from params, like the two fields
    /// above: a caller that could state it would be stating the evidence for
    /// its own claim. Carried on the request rather than on [`RecallContext`]
    /// because it belongs to one caller's pull, as `known_hashes` does.
    pub seen_turns: BTreeMap<(String, String), u64>,
}

/// The document half of a [`ToolRequest`]: everything about one document that
/// only the binary can know, resolved before the verb is dispatched.
///
/// The identity is the document's own bytes: the sidecar's key is
/// `sha256(source) ‖ engine ‖ caps`, so a sidecar cannot be stale — an edited
/// document has a different key and the family simply holds nothing for it yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    /// SHA-256 (hex) of the document's bytes, as the caller's checkout holds
    /// them now.
    pub source_sha256: String,
    /// The **declared** native cost of reading this document, in tokens — see
    /// [`declared_document_baseline`]. Never a measurement.
    pub declared_baseline: u32,
    /// What the family's sidecar store answers for this document.
    pub sidecar: SidecarState,
}

/// What the family's sidecar store has to say about one document.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SidecarState {
    /// The family holds the Markdown, and this member can see it.
    Held(Sidecar),
    /// The family holds none yet. The conversion is owed, not refused: it runs
    /// in the detached worker, and the next call to this verb is answered.
    #[default]
    Owed,
    /// The family decided this document has no useful Markdown — a scanned
    /// page image, an encrypted or malformed container. Nothing will convert it
    /// later either, so a caller told this can stop asking.
    Declined,
}

/// One converted document, as this member sees it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Sidecar {
    /// Where this member sees it, relative to the work tree — the path a
    /// follow-up read names when the Markdown does not travel inline.
    pub path: String,
    /// The Markdown itself, exactly as Rift wrote it.
    pub markdown: String,
}

/// The search half of a [`ToolRequest`]: what the member's own path index had
/// to say about one query, resolved before the verb is dispatched.
///
/// It arrives resolved for the same reason the document half does — the index
/// is a file under the checkout's run state and the lines are bytes in its work
/// tree, and this crate opens neither — and it arrives *whole* rather than as a
/// handle to ask again with, so the answer this verb seals is the answer the
/// engine gave, with nothing between them that could rank it a second time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Search {
    /// Whether the caller asked for matching lines as well as paths
    /// (`--content`). It decides which native call this answer stands in for,
    /// and so which baseline it may claim.
    pub content: bool,
    /// The **declared** native cost of the listing this answer replaces, in
    /// tokens — see [`declared_search_baseline`]. Never a measurement.
    pub declared_baseline: u32,
    /// What the index answered, or that there is not one yet.
    pub index: IndexState,
}

/// What the member's path index had to say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum IndexState {
    /// The index answered: these paths, best first, out of `total` that
    /// matched, and — for a content call — these lines inside them.
    Answered {
        /// The ranked paths, best first, already cut to the caller's ceiling.
        hits: Vec<SearchPath>,
        /// How many paths matched before that cut. Always stated, so a
        /// truncated answer is never mistaken for the whole set.
        total: usize,
        /// The matching lines, in the order their paths ranked. Empty unless
        /// the caller asked for content.
        lines: Vec<SearchLine>,
    },
    /// There is no index for this checkout yet and building one inline would
    /// have cost more than the search saves, so a rebuild has been asked for
    /// and this call is answered with the fact rather than with a walk.
    #[default]
    Pending,
}

/// One ranked path, as the engine scored it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPath {
    /// Repository-relative, as the index holds it.
    pub path: String,
    /// What git says about it — Rift's own fixed word, never a caller's label.
    pub status: PathStatus,
}

/// One matching line inside one tracked file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchLine {
    /// Repository-relative path of the file the line is in.
    pub path: String,
    /// Its one-based line number.
    pub line: u32,
    /// The line itself, without its terminator, already cut to the engine's
    /// per-line ceiling.
    pub text: String,
}

/// A successful tool response.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResponse {
    /// The tool that produced it.
    pub tool: ToolName,
    /// Estimated tokens returned — the honest tokenizer count of the serialized
    /// payload the transport ships (the accounting seam's measure).
    pub estimated_tokens_returned: u32,
    /// Estimated tokens avoided vs full re-read — net: the gross ingest-time
    /// baseline minus `estimated_tokens_returned`, saturating.
    pub estimated_tokens_avoided: u32,
    /// The tokenizer that produced `estimated_tokens_returned`.
    pub tokenizer: &'static str,
    /// The tool-specific payload (JSON; serialized to the agent by the transport).
    pub payload: Value,
    /// The event the caller must hand to `rift hooks memd`.
    pub event: MemdEvent,
    /// The rule `estimated_tokens_avoided` was booked under, decided by the
    /// dispatcher from the verb, the request and the reader.
    pub avoided_basis: AvoidedBasis,
}

/// The rule a response's `estimated_tokens_avoided` was booked under — the
/// word a surface prints beside the figure so a reader knows what it rests on.
///
/// A closed set of five, decided by the verb, the request and whether the
/// reader is bound to a checkout, before any store is read: it names the rule
/// that applied, not how much it credited, so a stated-hash call that matched
/// nothing still says `stated_recorded` beside its zero. The per-entry cases
/// the rule withholds credit for are tagged on the entry itself, where the
/// caller can see which file it was.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvoidedBasis {
    /// `stated_recorded`: the caller stated hashes, and a file is credited
    /// only where the checkout holds the bytes the answer describes and this
    /// family recorded reading the revision the caller holds — unchanged
    /// files at their cost, and, on the answers that carry changed lines, a
    /// changed file at its current cost less the lines returned.
    StatedRecorded,
    /// `none_stated`: a crediting verb called without a single stated hash; it
    /// books zero, because a list is not a set of spared reads.
    NoneStated,
    /// `unverified`: the caller stated hashes, but the reader is bound to no
    /// checkout, so the answer is the projection's view, never checked against
    /// the file, and books zero.
    Unverified,
    /// `declared`: the baseline is the declared native cost of the read the
    /// answer replaced — a document or a path listing.
    Declared,
    /// `not_credited`: the verb books zero by design — an exact retrieval, the
    /// cost view, or the ranked hunk shape, which answers about no file the
    /// caller said it held.
    NotCredited,
}

/// A tool failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolError {
    /// Per-turn tool-call ceiling hit.
    BudgetCallsExhausted,
    /// A ranking-sensitive tool was called without query-aware params.
    QueryParamsRequired,
    /// Policy denied (carries the reason to surface).
    Denied(OmitReason),
    /// The effective pack cap (`min(budget, hard ceiling, turn remaining)`)
    /// cannot hold a coherent pack (fail-loud; the documented
    /// fallback is calling `rift_changed_hunks` directly).
    PackBudgetTooSmall {
        /// The cheapest coherent content the request carried.
        needed: u32,
        /// The effective cap that could not hold it.
        cap: u32,
    },
    /// An internal read/build error (identifier/reason only — never content).
    Internal(String),
}

/// The read-only context the handlers query: the recall projection + substrate,
/// bounded to one [`Scope`], evaluated at an injected `now`.
pub struct RecallContext<'a, S: CanonicalStore, R: SubstrateReader> {
    /// The recall.db projection.
    pub store: &'a S,
    /// The read-only substrate (for exact blob resolution).
    pub reader: &'a R,
    /// The family's read-time basis snapshots — the ladder's second rung, and
    /// the only route to a revision that was never committed.
    ///
    /// `None` is a supported state and not a defect: a caller with no family
    /// identity to reach the store by (an in-memory fixture, a checkout whose
    /// family cannot be proven from here) simply has the rung it always had.
    pub snapshots: Option<&'a crate::basis_store::BasisStore>,
    /// The query scope (containment).
    pub scope: Scope,
    /// Decides the scope's branch axis: is a checkpoint's anchor commit on this
    /// caller's line? Asked once per serve, through the one
    /// chokepoint. A caller with no repository to walk passes
    /// [`rift_recall_types::NoAncestry`], which answers *unknown* — and unknown
    /// excludes and reports, it never widens.
    pub ancestry: &'a dyn AncestryOracle,
    /// Where a delta's **story** comes from: the capture projection's record of
    /// when a checkpoint was taken, which session and agent family took it and
    /// what that turn cost, plus the subject of the commit that anchored it.
    ///
    /// Injected for the same reason `ancestry` is. Those facts live in the
    /// other projection and in git, neither of which a serving handler may
    /// open, and `recall.db` never joins a capture table — the two rebuild
    /// independently. A caller with no capture projection passes
    /// [`NoProvenance`](rift_recall_types::NoProvenance), and the deltas it
    /// gets back simply carry no provenance object.
    pub provenance: &'a dyn ProvenanceOracle,
    /// The injected clock (recency + label as-of).
    pub now: Timestamp,
}

/// The transport-independent Recall dispatcher. Holds the
/// policy engine; callers thread the per-turn budget through each dispatch
/// through each dispatch, while the dispatcher itself stays stateless.
pub struct ServeDispatcher<'a> {
    policy: PolicyEngine<'a>,
}

impl ToolName {
    /// The stable dispatcher verb token (`rift_<tool>`).
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::KnownState => "rift_known_state",
            Self::DiffSince => "rift_diff_since",
            Self::FileIfChanged => "rift_file_if_changed",
            Self::ChangedHunks => "rift_changed_hunks",
            Self::ReadSummary => "rift_read_summary",
            Self::RetrieveExact => "rift_retrieve_exact",
            Self::CostHotspots => "rift_cost_hotspots",
            Self::ContextPack => "rift_context_pack",
            Self::DocRead => "rift_doc_read",
            Self::Search => "rift_search",
        }
    }

    /// Whether answering this verb can need BYTES out of the substrate — a
    /// blob from git's object store, or the named file as it stands now.
    ///
    /// Three handlers reach for `ctx.reader`: the pull verb (the basis blob
    /// and the current file), audited exact retrieval, and pack compilation.
    /// The other five answer from the projection alone, and the document verb
    /// answers from neither ([`is_scope_bound`](Self::is_scope_bound)), so a
    /// caller that only asks those never has to open a repository to be
    /// answered — which is why this is a property of the verb rather than a
    /// decision each caller repeats.
    /// `substrate_readers_are_the_verbs_that_reach_for_bytes` keeps the list
    /// honest.
    #[must_use]
    pub const fn may_read_the_substrate(self) -> bool {
        matches!(
            self,
            Self::ChangedHunks | Self::RetrieveExact | Self::ContextPack
        )
    }

    /// Whether this verb's answer is bounded by a serving [`Scope`].
    ///
    /// Every projection-backed verb is: containment is what decides which of
    /// the family's facts a caller standing in one member may be told. The
    /// document verb is not, and that is a fact about where its answer lives
    /// rather than a permission: a sidecar is keyed by the document's own
    /// bytes, it is the same file for every member of the family, and there is
    /// no branch axis to widen or withhold. A caller that offered to widen one
    /// would be advertising a lever with nothing behind it.
    #[must_use]
    pub const fn is_scope_bound(self) -> bool {
        !matches!(self, Self::DocRead | Self::Search)
    }

    /// Whether this verb's `target` is a **repository path**, and so is
    /// re-spelled by [`decode_request`] before anything compares it to a row.
    ///
    /// The other two targets are not paths and must not be touched.
    /// [`Self::RetrieveExact`] takes a handle or a pack citation — Rift's own
    /// composed strings, already `/`-spelled where they carry a path, and
    /// rewriting one would change an address rather than a spelling.
    /// [`Self::Search`] takes a *query*, where a backslash is a character the
    /// caller meant.
    #[must_use]
    pub const fn target_is_path(self) -> bool {
        matches!(
            self,
            Self::FileIfChanged | Self::ChangedHunks | Self::ReadSummary | Self::DocRead
        )
    }

    /// Parse a wire name back to a tool (unknown names are protocol errors).
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        TOOL_SURFACE.iter().copied().find(|t| t.wire_name() == name)
    }

    /// The **skill-verb** spelling: the same closed surface as
    /// [`wire_name`](Self::wire_name), spelled for the CLI
    /// (`rift recall <verb> --json`). Mechanically derived from the wire name
    /// (drop the `rift_` prefix, `_` → `-`) and pinned by a test, so the two
    /// spellings of one surface can never drift apart.
    #[must_use]
    pub fn verb_name(self) -> &'static str {
        match self {
            Self::KnownState => "known-state",
            Self::DiffSince => "diff-since",
            Self::FileIfChanged => "file-if-changed",
            Self::ChangedHunks => "changed-hunks",
            Self::ReadSummary => "read-summary",
            Self::RetrieveExact => "retrieve-exact",
            Self::CostHotspots => "cost-hotspots",
            Self::ContextPack => "context-pack",
            Self::DocRead => "doc-read",
            Self::Search => "search",
        }
    }

    /// Parse a skill-verb spelling back to a tool (unknown verbs are errors —
    /// the surface is closed).
    #[must_use]
    pub fn from_verb(verb: &str) -> Option<Self> {
        TOOL_SURFACE.iter().copied().find(|t| t.verb_name() == verb)
    }

    /// Whether this tool requires query-aware params (`task`, `files_in_focus`).
    #[must_use]
    pub fn is_ranking_sensitive(self) -> bool {
        matches!(
            self,
            Self::KnownState | Self::ContextPack | Self::ChangedHunks
        )
    }

    /// Whether this tool can return exact content **and so requires a
    /// reason**. It is the reason requirement that is being asked about, not
    /// the bytes: the document verb serves a sidecar the caller named by path,
    /// which is the read it replaces rather than a retrieval past a gate, and
    /// it asks for no reason and books no grant.
    #[must_use]
    pub fn is_exact(self) -> bool {
        matches!(self, Self::RetrieveExact)
    }
}

impl AvoidedBasis {
    /// The basis a call of `tool` with `req` is booked under, through a
    /// reader that is (`checkout`) or is not bound to a checkout.
    #[must_use]
    pub fn of(tool: ToolName, req: &ToolRequest, checkout: bool) -> Self {
        let credits_stated = match tool {
            ToolName::KnownState
            | ToolName::DiffSince
            | ToolName::FileIfChanged
            | ToolName::ReadSummary
            | ToolName::ContextPack => true,
            ToolName::ChangedHunks => targeted_pull(tool, req),
            ToolName::DocRead | ToolName::Search => return Self::Declared,
            ToolName::RetrieveExact | ToolName::CostHotspots => false,
        };
        match (credits_stated, req.known_hashes.is_empty(), checkout) {
            (false, _, _) => Self::NotCredited,
            (true, true, _) => Self::NoneStated,
            (true, false, false) => Self::Unverified,
            (true, false, true) => Self::StatedRecorded,
        }
    }

    /// The word the envelopes print.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StatedRecorded => "stated_recorded",
            Self::NoneStated => "none_stated",
            Self::Unverified => "unverified",
            Self::Declared => "declared",
            Self::NotCredited => "not_credited",
        }
    }
}

impl<'a> ServeDispatcher<'a> {
    /// Construct with a policy engine. The turn budget is owned by the caller;
    /// a `known_state` call anchors it (see [`ServeDispatcher::dispatch`]).
    #[must_use]
    pub fn new(policy: PolicyEngine<'a>) -> Self {
        Self { policy }
    }

    /// Select exactly one boundary payload, independently of native transport.
    ///
    /// Priority is fixed here rather than in adapters: a pending replacement
    /// closes the call Rift just declined; recovery restores a compacted
    /// window; a new session receives its pointer; an ordinary turn receives
    /// its delta. Empty candidates are never encoded. Equal-kind candidates
    /// are resolved lexically so caller iteration order cannot change output.
    #[must_use]
    pub fn plan_context<I>(candidates: I) -> ContextPlan
    where
        I: IntoIterator<Item = ContextCandidate>,
    {
        fn priority(kind: ContextKind) -> u8 {
            match kind {
                ContextKind::ReadReplacement => 0,
                ContextKind::RecoveryMap => 1,
                ContextKind::SessionPointer => 2,
                ContextKind::DeltaManifest => 3,
            }
        }

        let Some(selected) = candidates
            .into_iter()
            .filter(|candidate| !candidate.text.is_empty())
            .min_by(|a, b| {
                priority(a.kind)
                    .cmp(&priority(b.kind))
                    .then_with(|| a.text.cmp(&b.text))
            })
        else {
            return ContextPlan::None;
        };
        ContextPlan::Deliver {
            kind: selected.kind,
            delivered_tokens: rift_tokens::count(&selected.text),
            text: selected.text,
            avoided_tokens: selected.avoided_tokens,
            fallback: selected.fallback,
        }
    }

    /// Dispatch a tool call through budget + policy middleware, then the
    /// store-backed handler. Produces exactly one `memd.*` event. `budget` is the
    /// caller-held per-turn budget; a `rift_known_state` call **anchors** it
    /// (resets the draw-down) so every subsequent call in the turn shares one
    /// allowance.
    ///
    /// # Errors
    /// [`ToolError`] if the call exceeds budget, omits required query params, is
    /// denied by policy, or hits an internal read error.
    pub async fn dispatch<S: CanonicalStore, R: SubstrateReader>(
        &self,
        tool: ToolName,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        // 0. anchor the turn on the manifest-first entry point: a
        //    known_state call resets the turn budget before it draws down.
        if matches!(tool, ToolName::KnownState) {
            budget.anchor();
        }
        // 1. per-turn call ceiling — deny before any work.
        if !budget.try_call() {
            return Err(ToolError::BudgetCallsExhausted);
        }
        // 2. query-aware requirement — for the calls that actually rank.
        //    A caller that named a path AND the revision it holds has asked
        //    about one file, and ranking never runs, so demanding a task and a
        //    focus set there would refuse the one command the manifest's own
        //    hint teaches an agent to run.
        if tool.is_ranking_sensitive()
            && !targeted_pull(tool, req)
            && (req.task.as_deref().unwrap_or("").trim().is_empty()
                || req.files_in_focus.is_empty())
        {
            return Err(ToolError::QueryParamsRequired);
        }
        // The rule the figure is booked under is decided on what the caller
        // stated, before a prefix is resolved away.
        let basis = AvoidedBasis::of(tool, req, ctx.reader.worktree_root().is_some());
        // Counted over every revision the caller named, before any is
        // resolved away; a call that makes no claim asks nothing here.
        let claim = claim::pull_claim(ctx.store, &req.known_hashes, basis).await;
        // 3. resolve the hashes the caller says it holds, once, for every
        //    verb whose answer turns on one. A hash reaches a handler as a
        //    full content address or not at all: the handlers compare
        //    addresses, and a prefix compared as an address matches nothing,
        //    so this is the one place a shortened hash means anything. An
        //    ambiguous or unheld prefix is dropped, which is the same answer
        //    the exact comparison downstream would have given it — the file
        //    reads as changed. A store error is dropped for the same reason.
        //    The pull verb is the exception, and resolves its own: the
        //    manifest verbs answer an unresolved hash by treating the file as
        //    changed, which dropping the entry does exactly; the pull verb has
        //    to say `unknown_basis` out loud, so it needs the verdict rather
        //    than the filtered set.
        let resolved;
        let req = if req.known_hashes.is_empty() || matches!(tool, ToolName::ChangedHunks) {
            req
        } else {
            resolved = ToolRequest {
                known_hashes: basis::resolve_known_hashes(ctx.store, &req.known_hashes).await,
                ..req.clone()
            };
            &resolved
        };
        // 4. dispatch to the store-backed handler, and stamp the rule its
        //    figure was booked under and what the caller claimed to hold.
        let resp = self.route(tool, req, ctx, budget).await?;
        let event = match claim {
            Some(claim) => resp.event.with_pull_claim(claim),
            None => resp.event,
        };
        Ok(ToolResponse {
            avoided_basis: basis,
            event,
            ..resp
        })
    }

    /// Hand a call to the handler of its verb.
    async fn route<S: CanonicalStore, R: SubstrateReader>(
        &self,
        tool: ToolName,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        match tool {
            ToolName::KnownState => self.known_state(req, ctx, budget).await,
            ToolName::DiffSince => Self::diff_since(req, ctx, budget).await,
            ToolName::FileIfChanged => Self::file_if_changed(req, ctx, budget).await,
            ToolName::ChangedHunks => self.changed_hunks(req, ctx, budget).await,
            ToolName::ReadSummary => Self::read_summary(req, ctx, budget).await,
            ToolName::RetrieveExact => self.retrieve_exact(req, ctx, budget).await,
            ToolName::CostHotspots => Self::cost_hotspots(req, ctx, budget).await,
            ToolName::ContextPack => self.context_pack(req, ctx, budget).await,
            // No `ctx`: the document verb reads neither the projection nor the
            // object database, and saying so here is what keeps that true.
            ToolName::DocRead => self.doc_read(req, budget),
            // No `ctx` either, and for the same reason: the ranking happened
            // in the binary, over a file and a work tree this crate cannot
            // reach. What is left here is the screening, the rendering and the
            // accounting — which is precisely what has to happen in one place.
            ToolName::Search => self.search(req, budget),
        }
    }
}

/// Decode a tool name + JSON params into a typed `(ToolName, ToolRequest)`.
///
/// **Total:** never panics on arbitrary `params` (the request-decoder fuzz
/// invariant); missing/ill-typed fields default. Unknown tool names are a
/// protocol error ([`DecodeError::UnknownTool`]).
///
/// **The lookup half of the one `/` spelling.** Every path a caller states —
/// the focus set, and a `target` on a verb whose target is a path
/// ([`ToolName::target_is_path`]) — leaves here
/// [`slash_spelled`], the same
/// normaliser the ingest keys rows with. Rows and lookups then agree by
/// construction rather than by both happening to be typed the same way, so a
/// Windows caller who types `bins\rift\src\hooks.rs` is answered about the
/// file rather than told `path_not_found`. This is the one decoder every
/// serving channel goes through, which is why it is the one place this is
/// done.
///
/// # Errors
/// [`DecodeError::UnknownTool`] if `tool_name` is not a known tool.
pub fn decode_request(
    tool_name: &str,
    params: &Value,
) -> Result<(ToolName, ToolRequest), DecodeError> {
    let tool = ToolName::from_wire(tool_name).ok_or(DecodeError::UnknownTool)?;
    let str_at = |k: &str| params.get(k).and_then(Value::as_str).map(String::from);
    let str_vec = |k: &str| {
        params
            .get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let u32_at = |k: &str| {
        params
            .get(k)
            .and_then(Value::as_u64)
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
    };
    let path_at = |k: &str| str_at(k).map(|p| slash_spelled(&p).into_owned());
    let req = ToolRequest {
        task: str_at("task"),
        files_in_focus: {
            let named: Vec<String> = str_vec("files_in_focus");
            named
                .iter()
                .map(|p| slash_spelled(p).into_owned())
                .collect()
        },
        error_signatures: str_vec("error_signatures"),
        token_budget: u32_at("token_budget").unwrap_or(0),
        known_hashes: str_vec("known_hashes"),
        target: if tool.target_is_path() {
            path_at("target")
        } else {
            str_at("target")
        },
        reason: str_at("reason"),
        top_n: u32_at("top_n"),
        // NOT DECODED, and this line is the guarantee. Every other field is
        // something a caller may state; the document input is something the
        // binary RESOLVED — the sidecar's bytes, read out of the family's own
        // store under the document's content address. A params object that
        // could carry it would let a caller hand this surface the bytes it is
        // asking the surface to serve, under a path of the caller's choosing.
        document: None,
        // NOT DECODED either, and for the same reason one step further on: the
        // search input is the ranked answer itself — paths out of the member's
        // index and lines out of its work tree. A params object that could
        // carry it would let a caller state which paths matched and what the
        // lines in them say, which is the whole of what this verb answers.
        search: None,
        // NOT DECODED: the turns are the binary's reading of the window's own
        // journal, the evidence behind a claim the caller makes; letting the
        // caller state them would let a claim vouch for itself.
        seen_turns: BTreeMap::new(),
    };
    Ok((tool, req))
}

/// Decode a **skill-verb** invocation (`rift recall <verb>`) into
/// the dispatcher's typed `(ToolName, ToolRequest)`: the caller renders its
/// argv into the params object and delegates to [`decode_request`], keeping one
/// decoder for every serving channel.
///
/// **Total:** never panics on arbitrary `params`.
///
/// # Errors
/// [`DecodeError::UnknownTool`] if `verb` is not one of the eight skill verbs.
pub fn decode_verb(verb: &str, params: &Value) -> Result<(ToolName, ToolRequest), DecodeError> {
    let tool = ToolName::from_verb(verb).ok_or(DecodeError::UnknownTool)?;
    decode_request(tool.wire_name(), params)
}

#[cfg(test)]
mod tests;
