//! The bounded recall tool surface + budget/policy middleware.
//!
//! A **closed** set of ten tools — there is no transcript search and no
//! open-ended session search. This crate is the transport-independent
//! dispatcher plus the real, store-backed handlers, so the whole surface is
//! testable offline. The stdio transport that used to
//! wire this dispatcher into an agent-spawned server process is retired; today `rift
//! recall <verb>` (the skill-CLI channel, `bins/rift/src/recall.rs`) calls
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

/// Where a file changed since the revision a context window holds — the one
/// computation the manifest, the decline of a verification read and the pull
/// verb all read their line numbers from.
pub mod held_diff;

/// What a delta carries beside the change: when it happened, who made it, what
/// the turn cost, and the command that answers why.
pub mod provenance;

/// Which lines around a changed place a reader is shown, decided without a
/// parser.
pub mod window;

use std::collections::{BTreeMap, BTreeSet};

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
use rift_recall_ledger::candidates::{
    load_candidates, load_candidates_with_scope, LoadedCandidate,
};
pub use rift_recall_ledger::grants::CROSS_BRANCH_GRANT_TAG;
use rift_recall_ledger::scope_query::ScopeSet;
use rift_recall_policy::{AuditDecision, PolicyEngine, TurnBudget};
use rift_recall_rank::{rank, RankQuery, SignalWeights};
use rift_recall_substrate::worktree::slash_spelled;
use rift_recall_substrate::SubstrateReader;
use rift_recall_types::{
    path_is_safe, AncestryOracle, ArtifactState, Envelope, Handle, OmitReason, ProvenanceOracle,
    Scope, Trust,
};
pub use rift_recall_types::{ContextCandidate, ContextKind, ContextPlan};
use rift_recall_validity::trust::{
    neutral_inline_span, IntoNeutralDigest, IntoRawBody, TrustedPayload, UntrustedPayload,
};
/// The answer that stands in for a listing, and the two types it is written
/// from — re-exported by the same route and for the same reason as
/// [`render_deny_frame`]: a hook that answers a name search puts a sentence in
/// front of a model, and every such sentence is written by the one sealed
/// renderer. `bins/rift` reaches it here because this dispatcher is already the
/// validity crate's caller, so no crate edge is added.
pub use rift_recall_validity::trust::{render_search_answer, PathStatus, SearchHit};
use rift_recall_validity::{manifest_envelope, Budget, SeenStatus};
use rift_store::CanonicalStore;
use serde_json::{json, Value};

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

/// How many characters of a document a native read would have put in front of
/// the model before the client cut it off.
///
/// The character half of the client's own ceiling, pinned rather than rounded:
/// a terminal result is written to a file and replaced by a pointer past
/// ~30,000 characters (the recorded spike walked the boundary a thousand
/// characters at a time — 29,999 inline, 30,999 as a pointer). It is a
/// ceiling on characters and says nothing about what those characters cost,
/// which is why [`NATIVE_READ_TOKEN_CEILING`] is applied after it and not
/// instead of it.
///
/// **It bounds a shell listing too, and for the same reason.** The pinned page
/// states this for a *command's result*, not for a document — a valid result
/// arrives "Inline up to roughly 30,000 characters", and past that the model
/// is handed a file path and a short preview. So a `find` whose output would
/// overflow it is cut here, which is what makes a listing's ceiling a
/// character count where a glob's is a file count: see [`SearchCeiling`].
pub const NATIVE_INLINE_CHARS: usize = 30_000;

/// The token half of the same ceiling: a `Read` result stops here.
///
/// The two ceilings are not interchangeable, and the corpus is what settled
/// it. A character is worth between roughly a third of a token (lean XML) and
/// a whole one (a compressed container, or dense CJK), so
/// [`NATIVE_INLINE_CHARS`] characters are 5,700 tokens of one document and
/// over 30,000 of another. Whichever binds first is the one a native read
/// would really have paid, so a declared figure is cut by both — and a figure
/// Rift claims credit for has to err low.
pub const NATIVE_READ_TOKEN_CEILING: u32 = 25_000;

/// The **declared** native cost of reading one document, in tokens.
///
/// Declared, not measured, and the distinction is the whole point: the read
/// this figure prices never happened, so there is no second reading to count.
/// What is counted is the bytes as a native read would have presented them —
/// lossy UTF-8, because that is what a client hands a model when it opens a
/// container it cannot parse — cut at [`NATIVE_INLINE_CHARS`] characters and
/// then at [`NATIVE_READ_TOKEN_CEILING`] tokens, because a client that stops
/// at either stops at whichever it reaches first. Every row that carries the
/// result also carries [`TAG_DECLARED_BASELINE`], so a reader can never
/// mistake it for a measurement.
///
/// **PDF declares nothing.** A PDF is not presented as text at all: the client
/// renders its pages and the model sees images, so the honest figure is pages
/// times a vision-token rate, and no artifact in this tree pins that rate. A
/// declared zero is the conservative answer — the verb still serves the
/// sidecar, and claims nothing for having done so — and it is the one the
/// document fixture replaces when the rate is pinned.
#[must_use]
pub fn declared_document_baseline(ext: &str, source: &[u8]) -> u32 {
    if ext.trim_start_matches('.').eq_ignore_ascii_case("pdf") {
        return 0;
    }
    // At most four bytes a character, so this is every byte that can be part
    // of the first `NATIVE_INLINE_CHARS` characters and no more: a 16 MiB
    // container is never decoded whole to price a window this small.
    let head = source
        .get(..NATIVE_INLINE_CHARS.saturating_mul(4))
        .unwrap_or(source);
    let text: String = String::from_utf8_lossy(head)
        .chars()
        .take(NATIVE_INLINE_CHARS)
        .collect();
    rift_tokens::count(&text).min(NATIVE_READ_TOKEN_CEILING)
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

/// Why a request could not be decoded (a protocol error, surfaced to the agent).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The tool name is not one of the eight (no such tool).
    #[error("unknown tool")]
    UnknownTool,
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

/// What [`ServeDispatcher::screen_search`] left of one engine answer: the rows
/// that may be rendered, and how many were kept out and why.
struct Screened {
    /// Ranked paths, rendered.
    paths: Vec<Value>,
    /// Matching lines, rendered.
    rows: Vec<Value>,
    /// How many entries the renderer would not render.
    withheld: usize,
    /// How many distinct paths this repository's policy kept out.
    denied: usize,
}

/// How many paths a native listing of this repository would have put in front
/// of the model before it truncated.
///
/// The pinned figure, not a round one, and the pin is a committed artifact
/// rather than a memory: `crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md`
/// records the `Glob` tool's own page verbatim, and the sentence this constant
/// is read off says "Results are sorted by modification time and capped at 100
/// files". A fixture whose number moved and a constant that did not would be
/// two claims about one tool, so the fixture is read back and compared with
/// this constant by `the_hundred_file_cap_is_the_tools_own_pinned_figure`.
///
/// It bounds what this verb ranks, and it is the ceiling
/// [`declared_search_baseline`] prices a **glob** at — one of the two, because
/// the other shape a name search comes in is not bounded by a file count at
/// all: see [`SearchCeiling`].
pub const NATIVE_LISTING_PATHS: usize = 100;

/// Which native call an answered name search stood in for.
///
/// The axis a declared listing baseline is priced on, because the two shapes
/// are cut off by two different things: a glob tool stops itself after a fixed
/// number of files, and a shell listing prints every match and is cut by how
/// much of a command's result the client reads back inline. Pricing both at
/// the file cap prices the second one as though the first tool had run it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ListingShape {
    /// A glob tool call: the tool itself stops after
    /// [`NATIVE_LISTING_PATHS`] files.
    Glob,
    /// A shell or directory listing: the command prints every match, and the
    /// client stops reading its result at its own inline character ceiling.
    Listing,
}

/// The bound one [`ListingShape`] is cut off by on one client.
///
/// Two units, deliberately, because the two pins are in two units: a file
/// count and a character count. A ceiling is never inferred, only read off a
/// committed artifact — the same discipline that governs what may be written
/// on a channel — so a shape whose ceiling this tree does not pin takes the
/// one the owner ruled it takes, and says so where a reader can see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SearchCeiling {
    /// The glob tool's own documented file cap.
    Files(usize),
    /// The client's own documented inline character ceiling for a result.
    InlineChars(usize),
}

/// The family a name search that belongs to no client is priced under.
///
/// The `search` verb is an operator's own call and not any client's tool call,
/// so it has no family and no client result ceiling. It is priced as a
/// [`Glob`](ListingShape::Glob), whose ceiling does not depend on the family at
/// all — so this names the absence rather than borrowing some client's name for
/// a call that client never made.
pub const NO_CLIENT_FAMILY: &str = "";

/// The families whose own committed artifact states an inline result ceiling,
/// and the figure it states.
///
/// One entry today. A family that later pins its own joins this table and
/// nothing else changes; [`SearchCeiling::of`] is the only reader.
const PINNED_INLINE_CHARS: [(&str, usize); 1] = [("claude-code", NATIVE_INLINE_CHARS)];

impl SearchCeiling {
    /// The ceiling one shape is bounded by on one family.
    ///
    /// **A glob** is bounded by its own tool, whatever the client's result
    /// ceiling is, so every family gets [`NATIVE_LISTING_PATHS`].
    ///
    /// **A listing** is bounded by how much of a command's result the client
    /// reads back inline. Claude Code's page states that figure and is the one
    /// entry in this module's private per-family pin table. Codex and
    /// Antigravity state none
    /// anywhere in this tree, and by the owner's consistency ruling they are
    /// priced at Claude Code's — the aim being one figure across the alpha
    /// clients rather than three. **That is an assumption, and it is stated
    /// here as one:** it says a shell run by those clients is cut off at
    /// roughly the same size, which nothing in this repository proves. It is
    /// the direction consistency points; if either client publishes its own
    /// ceiling, that number becomes its pin and this assumption stops
    /// applying to it.
    #[must_use]
    pub fn of(family: &str, shape: ListingShape) -> Self {
        match shape {
            ListingShape::Glob => Self::Files(NATIVE_LISTING_PATHS),
            ListingShape::Listing => Self::InlineChars(
                PINNED_INLINE_CHARS
                    .iter()
                    .find(|(pinned, _)| *pinned == family)
                    .map_or(NATIVE_INLINE_CHARS, |(_, chars)| *chars),
            ),
        }
    }
}

/// The paths of one native listing, in the order the ceiling cuts them, with
/// everything `refused` says may not be counted left out — inside the fill for
/// a character budget, after the head is taken for a file cap — before the cut
/// rather than after it.
///
/// One function so the screen and the price can never be taken over two
/// different sets: [`priceable_listing`] calls it with a screen, and
/// [`declared_search_baseline`] calls it with none over an already-screened
/// list, which makes the second cut idempotent.
///
/// **`Files`** takes the head as it was given, because a glob tool's own
/// ranking decides which files it prints and Rift's caller hands them in the
/// order it would claim them.
///
/// **`InlineChars`** sorts first. A character budget cuts a *set*, and which
/// paths survive the cut must not depend on a directory walk order nothing in
/// this tree pins — one order, taken lexicographically, is the one a caller
/// and a fixture can both reproduce. Each path is charged its own length plus
/// the newline that separates it, including the first, which errs the count
/// low by exactly one character.
fn listing_within(
    ceiling: SearchCeiling,
    paths: &[String],
    refused: impl Fn(&str) -> bool,
) -> Vec<String> {
    match ceiling {
        SearchCeiling::Files(cap) => paths
            .iter()
            .take(cap)
            .filter(|path| !refused(path))
            .cloned()
            .collect(),
        SearchCeiling::InlineChars(cap) => {
            let mut sorted: Vec<&String> = paths.iter().collect();
            sorted.sort_unstable();
            let mut chars = 0usize;
            let mut kept = Vec::new();
            for path in sorted {
                if refused(path) {
                    continue;
                }
                if chars + path.len() + 1 > cap {
                    break;
                }
                chars += path.len() + 1;
                kept.push(path.clone());
            }
            kept
        }
    }
}

/// The listing a native call would have printed that Rift is willing to stand
/// in for: what the client would have shown for this shape, minus every path
/// this repository refuses to serve.
///
/// **Why the refused paths leave the pool rather than stay in it.** A native
/// listing really would have printed them, so counting them would be defensible
/// arithmetic — and it would still be Rift claiming credit for tokens it did
/// not remove from anybody's context, since a path it will not name is a path
/// the model has to go and find another way. The conservative direction is the
/// only one a figure Rift publishes may err in, so the screen is answered
/// *before* the price and never after it. Two of the screens applied here are
/// the same two this crate's own search screen applies to what it renders —
/// the repository's own `denied_paths`, and the structural safety of the name
/// itself — and `refused` is the caller's own third: the repository's
/// sensitive list belongs to the policy engine's private side, and the one
/// caller that must not apply it (the `search` verb, which deliberately names
/// sensitive paths to the operator who asked) says so by passing a screen that
/// refuses nothing.
///
/// Where the shape's [`SearchCeiling`] is a character budget the screen runs
/// inside the fill, so a refused path neither spends the budget nor stops the
/// listing at it; where it is a file cap the head is taken first and then
/// screened, so a refused path still spends one of the cap's slots — the
/// conservative side, since it can only make the priced listing shorter.
#[must_use]
pub fn priceable_listing(
    policy: &PolicyEngine<'_>,
    shape: ListingShape,
    family: &str,
    paths: &[String],
    refused: impl Fn(&str) -> bool,
) -> Vec<String> {
    listing_within(SearchCeiling::of(family, shape), paths, |path| {
        policy.is_denied_path(path) || !path_is_safe(path) || refused(path)
    })
}

/// The **declared** native cost of the search this verb stands in for, in
/// tokens.
///
/// Declared, not measured, and the same distinction as
/// [`declared_document_baseline`]: the listing this prices never ran, so there
/// is no second output to count.
///
/// **The formula, in one sentence, so a reader never has to infer it from the
/// code:** the paths of [`priceable_listing`] — the matches Rift found, minus
/// the ones this repository refuses to name, cut at the ceiling this call's
/// own client documents for this call's own shape — joined one per line as a
/// native listing prints them, counted with [`rift_tokens`], the tokenizer of
/// record. Nothing else enters it: no header, no per-path decoration, no
/// estimate.
///
/// **The ceiling, and where each half of it comes from.** A
/// [`Glob`](ListingShape::Glob) stops at [`NATIVE_LISTING_PATHS`] files, which
/// the tool's own page states. A [`Listing`](ListingShape::Listing) is not
/// bounded by a file count at all: the command prints every match and the
/// client stops reading its result at [`NATIVE_INLINE_CHARS`] characters,
/// which the same page states — so the paths are laid out one per line and cut
/// where that budget runs out. [`SearchCeiling::of`] holds the per-family
/// table and the one assumption in it.
///
/// Callers hand it the screened listing, which is what makes the first
/// sentence true; the two products that price a name search — the hook's
/// worker and the `search` verb — both take it from [`priceable_listing`], and
/// the search fixture holds the two halves against each other on a generated
/// repository.
///
/// It errs low in four ways at once, which is the direction a figure Rift will
/// claim credit for has to err in: Rift's index skips what git ignores and the
/// native listing does not, so the match set priced here is a subset; the paths
/// are repository-relative where the tool prints absolute ones; the tool's own
/// truncation notice is not counted at all; and a path the screen refused is
/// removed from the pool rather than claimed.
///
/// **A content search declares nothing.** Its native counterpart is a content
/// grep, whose output is every matching line of every matching file with no cap
/// at all unless the caller set one — there is no ceiling in this tree to price
/// it against, and a number invented for it would be the one figure in this
/// plane nobody could check. Zero is the honest answer, and the row that
/// carries it says so with [`TAG_BASELINE_NOT_DECLARED`].
#[must_use]
pub fn declared_search_baseline(
    content: bool,
    shape: ListingShape,
    family: &str,
    paths: &[String],
) -> u32 {
    if content {
        return 0;
    }
    let listing = listing_within(SearchCeiling::of(family, shape), paths, |_| false).join("\n");
    rift_tokens::count(&listing)
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
        // 4. dispatch to the store-backed handler.
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

    // --- handlers ----------------------------------------------------------

    fn rank_query(req: &ToolRequest) -> Result<RankQuery, ToolError> {
        RankQuery::new(
            req.task.clone().unwrap_or_default(),
            req.files_in_focus.clone(),
            req.error_signatures.clone(),
        )
        .map_err(|_| ToolError::QueryParamsRequired)
    }

    async fn ranked_candidates<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<(Vec<LoadedCandidate>, ScopeSet), ToolError> {
        let q = Self::rank_query(req)?;
        let (loaded, in_scope) =
            load_candidates_with_scope(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
                .await
                .map_err(|e| ToolError::Internal(e.to_string()))?;
        let cands: Vec<_> = loaded.iter().map(|l| l.candidate.clone()).collect();
        let order = rank(&q, &cands, ctx.now, &SignalWeights::default());
        // Re-order `loaded` to match the ranking (by candidate id).
        let mut by_id: std::collections::BTreeMap<String, LoadedCandidate> = loaded
            .into_iter()
            .map(|l| (l.candidate.id.clone(), l))
            .collect();
        Ok((
            order
                .into_iter()
                .filter_map(|s| by_id.remove(&s.candidate.id))
                .collect(),
            in_scope,
        ))
    }

    /// The containment facts an **enumerating** response carries — when it has
    /// any to carry.
    ///
    /// The omission line grows one cause for containment: a
    /// manifest that quietly held back the family's other line would be a
    /// manifest an agent could not reason about. `out_of_branch` counts what
    /// the branch axis excluded (before the response's own cap, which
    /// `omitted` counts separately); `unknown_head` says the axis could not be
    /// decided *at all*, which is a different sentence from "there was
    /// nothing"; `cross_branch_grant` says a grant widened this serve.
    ///
    /// **Absent when there is nothing to say**, and that is a deliberate
    /// decision rather than a shortcut: the member is worth real tokens out of
    /// the caller's budget on every manifest, the accounting seam charges the
    /// caller for the bytes it ships, and a block reporting three zeroes buys
    /// the agent nothing while pushing a real entry off the end of a capped
    /// manifest. Nothing held back, nothing said.
    fn containment_view<S: CanonicalStore, R: SubstrateReader>(
        set: &ScopeSet,
        ctx: &RecallContext<'_, S, R>,
    ) -> Option<Value> {
        let out_of_branch = set.omitted_out_of_branch();
        let unknown_head = set.unknown_head();
        let granted = ctx.scope.cross.branches();
        (out_of_branch > 0 || unknown_head || granted).then(|| {
            json!({
                "out_of_branch": out_of_branch,
                "unknown_head": unknown_head,
                "cross_branch_grant": granted,
            })
        })
    }

    /// Attach [`Self::containment_view`]'s block to a response document.
    fn with_containment(mut doc: Value, containment: Option<&Value>) -> Value {
        if let (Some(block), Some(obj)) = (containment, doc.as_object_mut()) {
            obj.insert("containment".to_owned(), block.clone());
        }
        doc
    }

    async fn known_state<S: CanonicalStore, R: SubstrateReader>(
        &self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let (ranked, in_scope) = Self::ranked_candidates(req, ctx).await?;
        let containment = Self::containment_view(&in_scope, ctx);
        let known: BTreeSet<&str> = req.known_hashes.iter().map(String::as_str).collect();
        let mut envelopes: Vec<Envelope> = Vec::new();
        let mut baseline = 0u32;

        // Every per-artifact result is built by the sealed `manifest_envelope`
        // constructor — no ArtifactState is constructed in this handler. Inline
        // content (summaries) will draw on this trial allowance;
        // manifest-first, most entries carry no bytes. The trial never touches
        // the turn budget — the single charge happens at the respond seam.
        let mut vbudget = Budget::new(budget.tokens_remaining());
        for lc in &ranked {
            let c = &lc.candidate;
            let handle = format!("file:{}@sha256:{}", c.path, lc.content_sha256);
            let seen = if known.contains(lc.content_sha256.as_str()) {
                SeenStatus::SeenSameHash
            } else {
                SeenStatus::NotSeenOrChanged
            };
            baseline = baseline.saturating_add(c.est_tokens);
            // Provenance: a tracked file blob → CodeFact. Real summaries
            // are resolved here once ingest populates them; until then a live
            // revision with no bound summary is available_by_handle, never "deleted".
            envelopes.push(manifest_envelope(
                &c.path,
                Trust::CodeFact,
                &handle,
                &lc.content_sha256,
                seen,
                None,
                &mut vbudget,
            ));
        }
        // The story behind each entry that moved. Anchors come from the rows
        // already loaded — the loader picked the checkpoint to decide recency —
        // so this costs one query for the commit mirror and nothing else.
        let story = Story::over(ctx, &ranked, &known).await;
        let telling: Vec<Value> = envelopes
            .iter()
            .zip(&ranked)
            .map(|(e, lc)| {
                let mut view = envelope_view(e);
                if !known.contains(lc.content_sha256.as_str()) {
                    story.attach(ctx.provenance, &mut view, &ranked, lc, &known, None);
                }
                view
            })
            .collect();
        let plain: Vec<Value> = envelopes
            .iter()
            .zip(&ranked)
            .map(|(e, lc)| {
                let mut view = envelope_view(e);
                if !known.contains(lc.content_sha256.as_str()) {
                    Story::attach_handle(&mut view, &lc.candidate.path, None);
                }
                view
            })
            .collect();
        // The manifest cap: known_state anchored before the trial,
        // so the trial sees the fresh allowance. Entries are already
        // in rank order — truncation keeps the highest-ranked prefix and says
        // so honestly (`truncated`, `omitted`).
        let cap = manifest_cap(req, budget);
        let n = telling.len();
        let build = |kept: usize, truncated: bool, detail: bool| {
            let views = if detail { &telling } else { &plain };
            Self::with_containment(
                json!({
                    "tool": "known_state",
                    "envelopes": views.get(..kept).unwrap_or(&[]),
                    "truncated": truncated,
                    "omitted": n.saturating_sub(kept),
                }),
                containment.as_ref(),
            )
        };
        let (payload, _kept) = fit_with_provenance(cap, n, &build);
        Ok(Self::respond(
            ToolName::KnownState,
            payload,
            baseline,
            budget,
            manifest_event,
        ))
    }

    async fn diff_since<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let (loaded, in_scope) =
            load_candidates_with_scope(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
                .await
                .map_err(|e| ToolError::Internal(e.to_string()))?;
        let containment = Self::containment_view(&in_scope, ctx);
        let known: BTreeSet<&str> = req.known_hashes.iter().map(String::as_str).collect();
        let story = Story::over(ctx, &loaded, &known).await;
        let mut changed = Vec::new();
        let mut changed_plain = Vec::new();
        let mut unchanged = Vec::new();
        let mut baseline = 0u32;
        for lc in &loaded {
            baseline = baseline.saturating_add(lc.candidate.est_tokens);
            if known.contains(lc.content_sha256.as_str()) {
                unchanged.push(json!(lc.candidate.path));
            } else {
                let mut entry =
                    json!({ "path": lc.candidate.path, "new_sha256": lc.content_sha256 });
                let mut bare = entry.clone();
                story.attach(ctx.provenance, &mut entry, &loaded, lc, &known, None);
                Story::attach_handle(&mut bare, &lc.candidate.path, None);
                changed.push(entry);
                changed_plain.push(bare);
            }
        }
        // The manifest cap bounds the delta list too. Droppable
        // order is the concatenation [changed…, unchanged…] with drops from
        // the tail: unchanged paths (cheap, least informative) go before
        // changed entries (the tool's point). Entry order inside each list is
        // the deterministic id order the candidates load in.
        let cap = manifest_cap(req, budget);
        let n = changed.len().saturating_add(unchanged.len());
        let build = |kept: usize, truncated: bool, detail: bool| {
            let changed_kept = kept.min(changed.len());
            let unchanged_kept = kept.saturating_sub(changed.len());
            let rows = if detail { &changed } else { &changed_plain };
            Self::with_containment(
                json!({
                    "tool": "diff_since",
                    "changed": rows.get(..changed_kept).unwrap_or(&[]),
                    "unchanged": unchanged.get(..unchanged_kept).unwrap_or(&[]),
                    "truncated": truncated,
                    "omitted": n.saturating_sub(kept),
                }),
                containment.as_ref(),
            )
        };
        let (payload, _kept) = fit_with_provenance(cap, n, &build);
        Ok(Self::respond(
            ToolName::DiffSince,
            payload,
            baseline,
            budget,
            manifest_event,
        ))
    }

    async fn file_if_changed<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let path = req.target.clone().unwrap_or_default();
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let known: BTreeSet<&str> = req.known_hashes.iter().map(String::as_str).collect();
        let latest = loaded.iter().find(|l| l.candidate.path == path);
        let (state, baseline) = match latest {
            None => (
                json!({ "state": "invalid", "path": path, "reason": "path_not_found" }),
                0,
            ),
            Some(lc) if known.contains(lc.content_sha256.as_str()) => (
                json!({ "state": "unchanged", "path": path, "known_hash": lc.content_sha256 }),
                lc.candidate.est_tokens,
            ),
            Some(lc) => (
                json!({ "state": "changed", "path": path, "new_hash": lc.content_sha256 }),
                lc.candidate.est_tokens,
            ),
        };
        let payload = json!({ "tool": "file_if_changed", "result": state });
        Ok(Self::respond(
            ToolName::FileIfChanged,
            payload,
            baseline,
            budget,
            manifest_event,
        ))
    }

    /// The pull verb, in its two shapes.
    ///
    /// A caller that names a path and the revision it holds is asking the
    /// question this verb exists for — *what changed in this file since the
    /// bytes I have* — and is answered with the changed lines themselves. A
    /// caller that names neither is asking the older question, which ranks the
    /// candidates and reports the line ranges the projection recorded for the
    /// top one; that shape is untouched, because a caller with no basis has
    /// nothing to diff against and an answer computed against something else
    /// would be an answer to a question nobody asked.
    async fn changed_hunks<S: CanonicalStore, R: SubstrateReader>(
        &self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        if targeted_pull(ToolName::ChangedHunks, req) {
            return self.hunks_against_basis(req, ctx, budget).await;
        }
        Self::ranked_hunks(req, ctx, budget).await
    }

    /// The changed lines of one file against the revision the caller holds.
    ///
    /// Every exit that is not the content one is a **named** non-answer:
    /// `unknown_basis` when the caller's hash names no single revision,
    /// `basis_unavailable` when a revision's bytes cannot be reached,
    /// `binary`, `diffuse` when the file moved in more places than a windowed
    /// answer would be smaller than the file for, and `known_unchanged` when
    /// the caller already holds the current bytes. None of them is silence and
    /// none of them is a guess, because the one failure this verb must never
    /// have is serving one revision's lines under another revision's numbers.
    #[allow(clippy::too_many_lines)] // one answer, one exit per way of not having it
    async fn hunks_against_basis<S: CanonicalStore, R: SubstrateReader>(
        &self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        use rift_recall_validity::{envelope_for, MemoryDigestStore, SummaryStore};

        let path = req.target.clone().unwrap_or_default();
        // The same policy screen every other serve applies, before a
        // candidate is loaded or a byte is read.
        //
        // A policy Rift could not read is not a permissive one. The engine's
        // fail-closed state carries an EMPTY deny list — it says "nothing is
        // known", not "nothing is denied" — so a verb that consults only
        // `is_denied_path` reads an unparseable `serve.toml` as consent, and
        // this content-bearing verb would then be the cheapest way around a
        // gate `retrieve_exact` already closes.
        if self.policy.is_fail_closed() {
            return Ok(Self::refuse_hunks(
                &path,
                OmitReason::PolicyDeniedExact,
                budget,
            ));
        }
        if self.policy.is_denied_path(&path) {
            return Ok(Self::refuse_hunks(
                &path,
                OmitReason::PolicyDeniedExact,
                budget,
            ));
        }
        // The sensitivity rule the pack path applies, on the verb that serves
        // bytes with no reason attached. A sensitive path's exact source is
        // reached through the audited `retrieve_exact` — which demands a
        // reason and books a grant — or not at all; here it degrades to the
        // handle state the sealed constructor gives it, so the answer still
        // says the file changed and where, and the lines stay behind.
        let exact_allowed = !self.policy.exact_source_required(&path);
        let (loaded, in_scope) =
            load_candidates_with_scope(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
                .await
                .map_err(|e| ToolError::Internal(e.to_string()))?;
        let containment = Self::containment_view(&in_scope, ctx);
        let answer = |doc: Value, baseline: u32, budget: &mut TurnBudget| {
            Self::respond(
                ToolName::ChangedHunks,
                Self::with_containment(doc, containment.as_ref()),
                baseline,
                budget,
                manifest_event,
            )
        };
        // Candidates are keyed by FILE REVISION, so one path has a row per
        // revision anyone read and `find` would take whichever the map yielded
        // first — on a real projection that is an arbitrary revision answering
        // as `current_hash`, which is how a caller holding the current bytes
        // was told `basis_unavailable` and a caller holding old ones was told
        // `known_unchanged`. The current revision is the most recently touched
        // one; the id breaks a tie so the choice is deterministic rather than
        // whatever order the rows arrived in.
        let Some(lc) = loaded
            .iter()
            .filter(|l| l.candidate.path == path)
            .max_by(|a, b| {
                a.candidate
                    .last_touched
                    .cmp(&b.candidate.last_touched)
                    .then_with(|| a.candidate.id.cmp(&b.candidate.id))
            })
        else {
            return Ok(answer(
                json!({ "tool": "changed_hunks", "path": path, "state": "invalid", "reason": "path_not_found" }),
                0,
                budget,
            ));
        };
        // What the file IS, not what a checkpoint last carried. The caller
        // named a path in a checkout; the answer it needs is about the bytes a
        // reader would get from that path now. A projection's newest revision
        // trails every edit made since the last checkpoint, and answering from
        // it is how a changed file gets reported as one the caller already
        // holds. The projection stays the fallback for a reader with no
        // checkout to look in.
        let now = held_diff::current_worktree_bytes(ctx.reader, &path);
        let current_hash = now
            .as_ref()
            .map_or_else(|| lc.content_sha256.clone(), |(hash, _)| hash.clone());

        // The caller's basis, through the PATH-BOUND resolver. The first hash
        // that names a revision OF THIS FILE wins; a caller sending several is
        // holding several, and any one of them is a basis this file can be
        // diffed against. An address the family knows for some other file is
        // not one of them: resolving it here would let a caller pair any
        // address it has seen with any path it may name and receive that
        // path's whole content as a single changed place.
        let mut resolved = None;
        for given in &req.known_hashes {
            if let Ok(basis::Basis::Resolved(h)) =
                basis::resolve_basis_for_path(ctx.store, given, &path).await
            {
                resolved = Some(h);
                break;
            }
        }
        let Some(basis_hash) = resolved else {
            return Ok(answer(
                json!({ "tool": "changed_hunks", "path": path, "state": "unknown_basis", "current_hash": current_hash }),
                0,
                budget,
            ));
        };

        // The four-state envelope is the only per-artifact result this handler
        // builds, here as everywhere else: no summary store is bound, so the
        // verdict is `Degraded` and the constructor decides between the bytes
        // and a handle on the budget alone.
        let handle = format!("file:{path}@sha256:{current_hash}");
        // Provenance, never the caller: a tracked file's blob is a code fact,
        // and anything whose address does not parse as one fails closed to
        // untrusted — which is served as counts, never as lines.
        let trust = provenance_trust(&handle);
        let validity = rift_recall_validity::resolve(
            &SummaryStore::new(),
            &MemoryDigestStore::default(),
            &rift_recall_validity::WorktreeSnapshot::new(),
            &format!("summary:{path}"),
        );
        // The sealed constructor decides; this handler reads its verdict
        // through the same read-only projection every other response uses, and
        // names no state of its own. What it gets back is the state's own
        // spelling — so a response can only ever claim the state the
        // constructor put the artifact in.
        let seen_state = |seen, exact: Option<&str>, tokens: u32| {
            let mut vbudget = Budget::new(tokens);
            let env = envelope_for(
                &path,
                trust,
                &handle,
                &current_hash,
                seen,
                &validity,
                exact,
                exact_allowed,
                &mut vbudget,
            );
            envelope_view(&env)
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or("available_by_handle")
                .to_owned()
        };

        if basis_hash == current_hash {
            // The caller holds the current bytes. The whole file is what it
            // did not have to read, and the sealed constructor is what says
            // so — this handler never spells `known_unchanged` itself.
            let doc = json!({
                "tool": "changed_hunks",
                "path": path,
                "state": seen_state(SeenStatus::SeenSameHash, None, 0),
                "basis_sha256": basis_hash,
                "hunks": [],
                "omitted": 0,
                "truncated": false,
            });
            return Ok(answer(doc, lc.candidate.est_tokens, budget));
        }

        // The basis travels the one route a revision nobody kept can travel:
        // content address, the bridge ingest writes, the read-only substrate —
        // so a revision that is not a tracked blob has no bytes here at all.
        // It is fetched WITH THE PATH BOUND, so it cannot be another file's
        // bytes wearing this file's line numbers; the current side is the
        // named file itself, read from the checkout, which is bound to the
        // path by construction.
        //
        // Down the whole ladder: the blob first, then the family's read-time
        // snapshot. The second rung is what gives an **uncommitted** revision
        // a basis at all — and it is redacted, so what it licenses is checked
        // below rather than assumed.
        let current_bytes = match now {
            Some((_, bytes)) => Some(bytes),
            None => {
                held_diff::basis_bytes_for_path(ctx.store, ctx.reader, &path, &current_hash).await
            }
        };
        let (Some(current), Some(basis)) = (
            current_bytes,
            held_diff::resolve_basis_for_path(
                ctx.store,
                ctx.reader,
                ctx.snapshots,
                &path,
                &basis_hash,
            )
            .await,
        ) else {
            return Ok(answer(
                json!({ "tool": "changed_hunks", "path": path, "state": "basis_unavailable", "current_hash": current_hash, "basis_sha256": basis_hash }),
                0,
                budget,
            ));
        };

        let held_diff::RedactedDiff { diff, tainted } = held_diff::held_diff_of(&basis, &current);
        // **A changed place that rests on a mask is served as counts, never as
        // lines.** The basis passed the redaction gate on its way to disk, so
        // a line the gate masked there differs from the live file whether or
        // not the repository changed it — and the lines this verb would serve
        // for it are the *live* bytes of exactly the span that was redacted.
        // The digest is the same one-way serializer untrusted provenance
        // leaves by, under the tag the vocabulary already has for a source
        // that was redacted away.
        if tainted {
            let digest = serialize_untrusted("file_content", &current, &handle);
            let doc = json!({
                "tool": "changed_hunks",
                "path": path,
                "state": "refused",
                "reason": omit_tag(OmitReason::SourceDeletedOrRedacted),
                "basis_sha256": basis_hash,
                "current_hash": current_hash,
                "neutral_digest": serde_json::to_value(&digest).unwrap_or(Value::Null),
            });
            return Ok(answer(doc, 0, budget));
        }
        let (ranges, shift) = match &diff {
            held_diff::HeldDiff::Changed { ranges, shift } => (ranges.clone(), *shift),
            held_diff::HeldDiff::Unchanged => {
                let doc = json!({
                    "tool": "changed_hunks",
                    "path": path,
                    "state": seen_state(SeenStatus::SeenSameHash, None, 0),
                    "basis_sha256": basis_hash,
                    "hunks": [],
                    "omitted": 0,
                    "truncated": false,
                });
                return Ok(answer(doc, lc.candidate.est_tokens, budget));
            }
            held_diff::HeldDiff::Binary => {
                return Ok(answer(
                    json!({ "tool": "changed_hunks", "path": path, "state": "binary", "current_hash": current_hash, "basis_sha256": basis_hash }),
                    0,
                    budget,
                ))
            }
            held_diff::HeldDiff::Unavailable => {
                return Ok(answer(
                    json!({ "tool": "changed_hunks", "path": path, "state": "basis_unavailable", "current_hash": current_hash, "basis_sha256": basis_hash }),
                    0,
                    budget,
                ))
            }
            // More changed places than a windowed answer stays smaller than the
            // file for. The count is the honest whole of what is known, and
            // re-reading is the cheaper of the two remaining options.
            held_diff::HeldDiff::Diffuse { places, shift } => {
                return Ok(answer(
                    json!({ "tool": "changed_hunks", "path": path, "state": "diffuse", "places": places, "shift": shift, "current_hash": current_hash, "basis_sha256": basis_hash }),
                    0,
                    budget,
                ))
            }
        };

        let lines = window::text_lines(&current);
        let windows = window::windows(&lines, &ranges);
        let full_text = String::from_utf8_lossy(&current);
        // The baseline is THIS ONE FILE, counted by the real tokenizer over
        // the bytes the caller would otherwise have read. The seam subtracts
        // what was served, so the claim is literally full minus served.
        let baseline = rift_tokens::count(&full_text);

        // Untrusted provenance never reaches a line of the payload: what the
        // caller gets is the digest's counts, and the bytes stay here.
        if trust.is_untrusted() {
            let withheld = window::render(&lines, &windows);
            let digest = serialize_untrusted("file_content", withheld.as_bytes(), &handle);
            let doc = json!({
                "tool": "changed_hunks",
                "path": path,
                "state": "refused",
                "reason": omit_tag(OmitReason::PolicyDeniedExact),
                "neutral_digest": serde_json::to_value(&digest).unwrap_or(Value::Null),
            });
            return Ok(answer(doc, 0, budget));
        }

        // The story of THIS change: the checkpoint the caller's revision was
        // recorded in, the one the current revision was, and — per window — the
        // recorded hunk that established it, when exactly one did.
        //
        // Both ends are looked up **by revision**, not taken from the candidate
        // row this handler picked to answer `current_hash`. That row is the
        // most recently touched revision of the path, which is the right answer
        // to "what is this file" and the wrong one to "which checkpoint
        // recorded these bytes": a file edited in the work tree and never read
        // again has no checkpoint for its current revision at all, and naming
        // the one that recorded an older revision would put a change in a turn
        // that did not make it. Absent is the honest answer, and the object
        // then carries the end it does know.
        let anchored = |hash: &str| {
            loaded
                .iter()
                .find(|l| l.candidate.path == path && l.content_sha256 == hash)
                .map(|l| l.checkpoint.clone())
        };
        let (from, to) = (anchored(&basis_hash), anchored(&current_hash));
        // The commit belongs to whichever end the capture facts will come from.
        let anchor = to.clone().or_else(|| from.clone());
        let commits = match &anchor {
            Some(id) => {
                rift_recall_ledger::provenance::commit_oids(ctx.store, std::slice::from_ref(id))
                    .await
                    .unwrap_or_default()
            }
            None => BTreeMap::new(),
        };
        let bracket = provenance::Bracket {
            from: from.as_deref(),
            to: to.as_deref(),
            commit: anchor
                .as_ref()
                .and_then(|id| commits.get(id))
                .map(String::as_str),
        };
        let origins = rift_recall_ledger::provenance::hunk_origins(ctx.store, &path, &current_hash)
            .await
            .unwrap_or_default();
        // The line the story is about is the first CHANGED one, not the first
        // line shown: a window opens on context, and `rift why` asked about a
        // context line answers for a line that did not move.
        let first_line = ranges.first().map(|(lo, _)| *lo);

        let n = windows.len();
        let build = |kept: usize, truncated: bool, detail: bool| {
            let shown = windows.get(..kept).unwrap_or(&[]);
            let omitted = n.saturating_sub(kept);
            if kept == 0 {
                // Nothing fits, so nothing is claimed: an all-elision text
                // would read as "this file did not change".
                return Self::with_containment(
                    json!({
                        "tool": "changed_hunks",
                        "path": path,
                        "state": "omitted",
                        "reason": omit_tag(OmitReason::BudgetExhausted),
                        "basis_sha256": basis_hash,
                        "current_hash": current_hash,
                        "hunks": [],
                        "omitted": omitted,
                        "truncated": true,
                    }),
                    containment.as_ref(),
                );
            }
            let text = window::render(&lines, shown);
            let state = seen_state(
                SeenStatus::NotSeenOrChanged,
                Some(text.as_str()),
                budget.tokens_remaining(),
            );
            // The lines ride only on the state that admits them. The
            // constructor decided which one that is; this reads the decision
            // rather than repeating the reasoning behind it.
            let content = (state == "exact_included").then_some(text);
            let mut doc = json!({
                "tool": "changed_hunks",
                "path": path,
                "state": state,
                "basis_sha256": basis_hash,
                "current_hash": current_hash,
                "shift": shift,
                "hunks": shown.iter().map(|w| {
                    let mut h = json!({
                        "lo": w.lo,
                        "hi": w.hi,
                        "window": w.rule.name(),
                    });
                    // Per hunk only where the hunk's own revision is known:
                    // one recorded hunk covering this window, or nothing.
                    if let (true, Some(ck), Some(obj)) = (
                        detail,
                        rift_recall_ledger::provenance::origin_of(&origins, w.lo, w.hi),
                        h.as_object_mut(),
                    ) {
                        obj.insert("checkpoint".to_owned(), json!(ck));
                    }
                    h
                }).collect::<Vec<_>>(),
                "content": content,
                "omitted": omitted,
                "truncated": truncated,
            });
            // The delta's own provenance, once for the file. It downgrades to
            // the tag and the command before a single changed line is dropped:
            // the lines are the answer, and the story is what rides with it.
            if let Some(obj) = doc.as_object_mut() {
                let full = detail
                    .then(|| provenance::object(ctx.provenance, &bracket, &path, first_line))
                    .flatten();
                match full {
                    Some(value) => {
                        obj.insert("provenance".to_owned(), value);
                    }
                    None if bracket.is_anchored() => {
                        let (tag, why) = provenance::downgraded(&path, first_line);
                        obj.insert("provenance".to_owned(), tag);
                        obj.insert("why".to_owned(), why);
                    }
                    None => {}
                }
            }
            // A sensitive path never reaches the content state, and the answer
            // says which gate held the lines back rather than leaving a caller
            // to infer it from a field that is simply absent.
            if let (false, Some(obj)) = (exact_allowed, doc.as_object_mut()) {
                obj.insert(
                    "reason".to_owned(),
                    json!(omit_tag(OmitReason::SensitivePathRequiresReason)),
                );
            }
            Self::with_containment(doc, containment.as_ref())
        };
        let cap = manifest_cap(req, budget);
        let (payload, _kept) = fit_with_provenance(cap, n, &build);
        Ok(Self::respond(
            ToolName::ChangedHunks,
            payload,
            baseline,
            budget,
            manifest_event,
        ))
    }

    /// A refusal on this verb, audited and measured like every other serve. It
    /// names the path the CALLER named — it is the caller's own word, and a
    /// refusal that cannot say which request it refused is not usable.
    fn refuse_hunks(path: &str, reason: OmitReason, budget: &mut TurnBudget) -> ToolResponse {
        let payload = json!({
            "tool": "changed_hunks",
            "path": path,
            "state": "refused",
            "reason": omit_tag(reason),
        });
        let target = path.to_owned();
        let tag = omit_tag(reason).to_owned();
        Self::respond(
            ToolName::ChangedHunks,
            payload,
            0,
            budget,
            move |returned, _| MemdEvent::Refused {
                target,
                reason: tag,
                estimated_tokens_returned: returned,
            },
        )
    }

    async fn ranked_hunks<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        // The ranked candidates restrict which paths are in play; the hunk bytes
        // come from the persisted changed-hunk rows for the top focus path.
        //
        // Containment decides which paths are in play at all, so this response
        // owes the same self-declaration `known_state` and `diff_since` make:
        // an empty hunk list because the family's other line was
        // held back reads exactly like an empty hunk list because nothing
        // changed, and the caller cannot tell the two apart from the payload.
        // The block is attached to BOTH exits — the one that found no path is
        // the one that most needs it.
        let (ranked, in_scope) = Self::ranked_candidates(req, ctx).await?;
        let containment = Self::containment_view(&in_scope, ctx);
        let Some(top) = ranked.first() else {
            let payload = Self::with_containment(
                json!({ "tool": "changed_hunks", "hunks": [], "omitted": 0 }),
                containment.as_ref(),
            );
            return Ok(Self::respond(
                ToolName::ChangedHunks,
                payload,
                0,
                budget,
                manifest_event,
            ));
        };
        let rows = ctx
            .store
            .query(
                "SELECT new_start, new_len FROM n_changed_hunk WHERE file_revision = ?1 \
                 ORDER BY new_start ASC",
                &[rift_store::Value::text(top.candidate.id.as_str())],
            )
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        // Cap at the configured max-hunks and the per-turn token allowance. The
        // per-hunk omission is a NON-CHARGING trial against a local mirror of
        // the remaining budget — the single real charge happens at the respond
        // seam.
        let max_hunks = MAX_CHANGED_HUNKS as usize;
        let mut hunks = Vec::new();
        let mut trial = budget.tokens_remaining();
        let mut omitted = 0u32;
        for (i, r) in rows.iter().enumerate() {
            if i >= max_hunks {
                omitted += 1;
                continue;
            }
            let lo = r.get_i64(0).unwrap_or(0).max(0);
            let len = r.get_i64(1).unwrap_or(0).max(0);
            let view = json!({ "lo": lo, "hi": lo + len });
            let cost = rift_tokens::count(&view.to_string());
            if cost > trial {
                omitted += 1;
                continue;
            }
            trial -= cost;
            hunks.push(view);
        }
        let payload = Self::with_containment(
            json!({
                "tool": "changed_hunks",
                "path": top.candidate.path,
                "hunks": hunks,
                "omitted": omitted,
                "source_hash": top.content_sha256,
            }),
            containment.as_ref(),
        );
        let baseline = top.candidate.est_tokens;
        Ok(Self::respond(
            ToolName::ChangedHunks,
            payload,
            baseline,
            budget,
            manifest_event,
        ))
    }

    async fn read_summary<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        // The summary is hash-bound; serving re-checks validity. We look up
        // the binding for the requested path and compare its bound hash to the
        // current revision: a moved source returns `stale`, never the bytes.
        let path = req.target.clone().unwrap_or_default();
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let current = loaded.iter().find(|l| l.candidate.path == path);
        let rows = ctx
            .store
            .query(
                "SELECT source_sha256 FROM summary_binding WHERE path = ?1 ORDER BY created_at DESC LIMIT 1",
                &[rift_store::Value::text(path.as_str())],
            )
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let bound = rows.first().and_then(|r| r.get_text(0).ok());
        // Each arm yields the result view, the gross baseline the response
        // stands in for, and its event builder; the seam supplies the figures.
        type EventBuilder = Box<dyn FnOnce(u32, u32) -> MemdEvent + Send>;
        let (result, baseline, event): (Value, u32, EventBuilder) = match (bound, current) {
            (Some(bound), Some(lc)) if bound == lc.content_sha256 => {
                // valid: the bound source is still current.
                let summary = format!("summary bound to {bound}");
                let handle = format!("summary:{path}");
                (
                    json!({ "state": "summary", "path": path, "source_hash": bound, "summary": summary }),
                    lc.candidate.est_tokens,
                    Box::new(move |returned, avoided| MemdEvent::SummaryServed {
                        handle,
                        estimated_tokens_returned: returned,
                        estimated_tokens_avoided: avoided,
                    }),
                )
            }
            (Some(_), Some(lc)) => {
                let handle = format!("summary:{path}");
                (
                    json!({ "state": "stale", "path": path, "current_hash": lc.content_sha256 }),
                    0,
                    Box::new(move |returned, _| MemdEvent::Invalidated {
                        handle,
                        estimated_tokens_returned: returned,
                    }),
                )
            }
            _ => (
                json!({ "state": "unavailable", "path": path, "reason": "not_bound" }),
                0,
                Box::new(manifest_event),
            ),
        };
        let payload = json!({ "tool": "read_summary", "result": result });
        Ok(Self::respond(
            ToolName::ReadSummary,
            payload,
            baseline,
            budget,
            event,
        ))
    }

    async fn retrieve_exact<S: CanonicalStore, R: SubstrateReader>(
        &self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let target = req.target.clone().unwrap_or_default();
        // `pack:<key>/evidence:<n>` is a *citation*, not a handle —
        // the agent no longer holds a path to send, so the server resolves the
        // ordinal itself and gates the path it resolves to.
        if let Some(citation) = rift_recall_pack::CitationTarget::parse(&target) {
            return self.retrieve_by_citation(&citation, req, ctx, budget).await;
        }
        // Trust is derived from provenance, never from the caller: a
        // file/summary handle is a code fact; a log handle is untrusted tool
        // output; anything unparseable fails closed to untrusted.
        let trust = provenance_trust(&target);
        // Policy matches the artifact's repo path, not the raw handle string.
        let policy_path = handle_display_path(&target);
        // **BOTH HALVES OF THE ADDRESS, OR NOTHING.** `Handle::File`'s own
        // contract says a resolver must match path AND content address, and
        // this handler matched NEITHER against the record: it gated policy on
        // the caller's path and fetched the caller's hash. Measured, not
        // reasoned — `red_team_a_fabricated_path_with_a_captured_hash_must_not
        // _resolve` served `AKIA-DEADBEEF-PROD` out of a `secrets/**` file
        // under the path `src/a.rs` on the tree this commit edits. That is the
        // hash-only resolution the citation protocol refuses, live in the one
        // handler that hands over raw bytes.
        if !Self::address_is_recorded(ctx, &target).await? {
            // The tag is the ordinary denial tag, deliberately: a distinct
            // "not in the record" tag would answer the attacker's real
            // question — which half of the address was wrong — and turn the
            // refusal into an oracle for probing paths against hashes.
            return Ok(Self::refuse_exact(
                &target,
                trust,
                OmitReason::PolicyDeniedExact,
                budget,
            ));
        }
        match self
            .policy
            .decide_exact(&policy_path, trust, req.reason.as_deref())
        {
            // Trusted classes only reach here (policy denies untrusted).
            AuditDecision::Allow { reason, .. } => self.exact_response(
                ctx,
                budget,
                ExactGrant {
                    audit_name: target.clone(),
                    exact_id: ("path", policy_path.clone()),
                    handle_id: ("handle", target),
                    policy_path,
                    sha: exact_sha_owned(req.target.as_deref().unwrap_or_default()),
                    trust,
                    reason,
                },
            ),
            AuditDecision::Deny(reason) => Ok(Self::refuse_exact(&target, trust, reason, budget)),
        }
    }

    /// Resolve `pack:<key>/evidence:<n>` and serve what it names.
    ///
    /// # The pack key is VERIFIED, never trusted
    ///
    /// There is no stored table to look a key up in, and deliberately so: a
    /// server-side map keyed by a caller-supplied string is a lookup an
    /// attacker can drive by guessing. Instead the table is **rebuilt** from
    /// Rift's own facts — the ranked set as the store holds it now, the policy
    /// version in force, the budget the caller asked for — the pack key is
    /// recomputed from those same inputs, and the ordinal resolves only if the
    /// recomputed key equals the presented one. A key nobody issued and a key
    /// whose world has moved on are therefore the SAME answer, which is what
    /// the protocol asks for (unknown or expired is a REFUSAL, never a path
    /// lookup an attacker can enumerate) and is also the honest one: an
    /// ordinal means something only relative to the pack that published it.
    ///
    /// [`citation_pass`] is the single builder, shared with
    /// [`Self::context_pack`], so the table a pack carries and the table this
    /// resolves against cannot become two tables.
    async fn retrieve_by_citation<S: CanonicalStore, R: SubstrateReader>(
        &self,
        citation: &rift_recall_pack::CitationTarget,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let (ranked, _in_scope) = Self::ranked_candidates(req, ctx).await?;
        let recomputed =
            rift_recall_pack::pack_key(&pack_inputs(req, &ranked, self.policy.version()));
        let table = citation_pass(&self.policy, &ranked);
        // Ordering matters: the key is checked first, so an ordinal is never
        // looked up against a table the caller has not proven they hold.
        let resolved = (recomputed == citation.pack_key)
            .then(|| table.get(citation.ordinal))
            .flatten();
        let Some(cited) = resolved else {
            return Ok(Self::refuse_citation(
                citation,
                None,
                OmitReason::PolicyDeniedExact,
                budget,
            ));
        };
        // The caller may restate the address it read off the pack line. The
        // TABLE is the authority; a restatement that disagrees is refused
        // rather than reconciled, because reconciling it would let the caller
        // choose which bytes an ordinal names — the substitution half of the
        // hash-only attack.
        if citation
            .content_sha256
            .as_deref()
            .is_some_and(|h| h != cited.content_sha256())
        {
            return Ok(Self::refuse_citation(
                citation,
                Some(cited),
                OmitReason::PolicyDeniedExact,
                budget,
            ));
        }
        // Both halves came out of one `n_file_revision` row, so the address is
        // bound by construction here — there is no caller-supplied pair to
        // check. Trust still derives from provenance, through the same single
        // authority the handle route uses.
        let handle = format!("file:{}@sha256:{}", cited.path(), cited.content_sha256());
        let trust = provenance_trust(&handle);
        // **THE EXISTING PATH-BASED POLICY GATE, ON THE RESOLVED PATH, BEFORE
        // ANY BYTES ARE READ.** This is the whole reason an ordinal resolves to
        // an address instead of the caller naming one: the design took the path
        // away from the agent without taking it away from the gate.
        match self
            .policy
            .decide_exact(cited.path(), trust, req.reason.as_deref())
        {
            AuditDecision::Deny(reason) => {
                Ok(Self::refuse_citation(citation, Some(cited), reason, budget))
            }
            AuditDecision::Allow { reason, .. } => self.exact_response(
                ctx,
                budget,
                ExactGrant {
                    // Audited with all four facts the citation protocol requires: pack key,
                    // ordinal, resolved path, and the verdict (this arm).
                    audit_name: citation_audit_name(citation, Some(cited)),
                    // What the AGENT is told. Never the path — a payload that
                    // echoed the resolved name would hand back exactly what the
                    // ordinal exists to withhold.
                    exact_id: (
                        "evidence",
                        format!("{}{}", rift_recall_pack::EVIDENCE_TAG, citation.ordinal),
                    ),
                    handle_id: (
                        "evidence",
                        format!("{}{}", rift_recall_pack::EVIDENCE_TAG, citation.ordinal),
                    ),
                    policy_path: cited.path().to_string(),
                    sha: cited.content_sha256().to_string(),
                    trust,
                    reason,
                },
            ),
        }
    }

    /// Refuse one citation, audited with the pack key, the ordinal, the
    /// resolved path when the table answered, and the verdict.
    ///
    /// What the AGENT sees carries none of it — only the neutral tag — because
    /// a refusal that names the path it withheld has withheld nothing. Every
    /// arm refuses under the same tag on purpose: distinguishing "no such key",
    /// "no such ordinal" and "that ordinal was dropped" would answer the
    /// attacker's real question.
    fn refuse_citation(
        citation: &rift_recall_pack::CitationTarget,
        resolved: Option<&rift_recall_pack::Citation>,
        reason: OmitReason,
        budget: &mut TurnBudget,
    ) -> ToolResponse {
        Self::refuse_exact(
            &citation_audit_name(citation, resolved),
            Trust::CodeFact,
            reason,
            budget,
        )
    }

    /// Serve granted exact bytes — **the one place raw content leaves this
    /// server**, shared by the handle route and the citation route so a second
    /// inline of exact bytes cannot grow up beside the first.
    fn exact_response<S: CanonicalStore, R: SubstrateReader>(
        &self,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
        grant: ExactGrant,
    ) -> Result<ToolResponse, ToolError> {
        let bytes = ctx
            .reader
            .blob(&grant.sha)
            .map_err(|e| ToolError::Internal(e.to_string()))?
            .unwrap_or_default();
        // Chokepoint: the ONLY inline of exact bytes routes through
        // a `TrustedPayload`, so untrusted provenance can never be inlined raw
        // — a type guarantee, not a per-handler convention.
        let body = match TrustedPayload::new(grant.trust, bytes) {
            Ok(t) => t.into_raw_body(),
            // provenance disagrees with the grant → fail closed.
            Err(_) => {
                return Ok(Self::refuse_exact(
                    &grant.audit_name,
                    grant.trust,
                    OmitReason::PolicyDeniedExact,
                    budget,
                ))
            }
        };
        let cost = rift_tokens::count(&body);
        // Downgrade: a sensitive exact that will not fit the remaining
        // budget is served as a handle, never a partial secret. The trial is on
        // the INNER body count (whole-or-nothing) and never charges; the seam
        // charges the serialized response. Still audited.
        if self
            .policy
            .downgrade_for(&grant.policy_path, budget.tokens_remaining() >= cost)
            .is_some()
        {
            let payload = json!({
                "tool": "retrieve_exact",
                "result": { "state": "available_by_handle", grant.handle_id.0: grant.handle_id.1, "reason": "sensitive_over_budget" },
            });
            let target = grant.audit_name;
            return Ok(Self::respond(
                ToolName::RetrieveExact,
                payload,
                0,
                budget,
                move |returned, _| MemdEvent::Refused {
                    target,
                    reason: "sensitive_over_budget".to_string(),
                    estimated_tokens_returned: returned,
                },
            ));
        }
        let payload = json!({
            "tool": "retrieve_exact",
            "result": { "state": "exact", grant.exact_id.0: grant.exact_id.1, "body": body },
        });
        let handle = grant.audit_name;
        let reason = grant.reason;
        Ok(Self::respond(
            ToolName::RetrieveExact,
            payload,
            0,
            budget,
            move |returned, _| MemdEvent::ExactRetrieved {
                handle,
                reason,
                estimated_tokens_returned: returned,
            },
        ))
    }

    /// Whether the append-only record holds the address `target` names — for a
    /// file revision, a row whose **path and `content_sha256` both** match
    /// (`Handle::File`'s written contract, and what `bins/rift`'s
    /// `investigate::resolve_handle` has always done). Non-file handles carry
    /// no repo address to bind, so they pass here and are decided by policy on
    /// provenance alone (untrusted is denied outright).
    ///
    /// Bound against the **scope-loaded** candidate set rather than a bare
    /// `n_file_revision` read, so the containment that governs every other
    /// handler governs this one too: an out-of-scope revision is not a fact
    /// this server may confirm, let alone serve.
    async fn address_is_recorded<S: CanonicalStore, R: SubstrateReader>(
        ctx: &RecallContext<'_, S, R>,
        target: &str,
    ) -> Result<bool, ToolError> {
        let Ok(Handle::File { path, sha256, .. }) = target.parse::<Handle>() else {
            return Ok(true);
        };
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        Ok(loaded
            .iter()
            .any(|l| l.candidate.path == path && l.content_sha256 == sha256))
    }

    /// Build the audited refusal for an exact request. Untrusted content is
    /// surfaced only as a neutral digest + handle; trusted-but-denied content
    /// (missing reason, denied/sensitive path) is refused with a neutral reason
    /// tag. Either way a single terminal `Refused` event is emitted (the serve↔event
    /// bijection), and the refusal payload is measured and charged like any
    /// serve — no response class is exempt at the seam.
    fn refuse_exact(
        target: &str,
        trust: Trust,
        reason: OmitReason,
        budget: &mut TurnBudget,
    ) -> ToolResponse {
        let payload = if trust.is_untrusted() {
            let digest = UntrustedPayload::from_trust(trust, Vec::new()).map_or_else(
                || serialize_untrusted("tool_output", b"", target),
                |p| p.into_neutral_digest(&format!("log:{target}")),
            );
            json!({ "tool": "retrieve_exact", "result": { "state": "refused", "neutral_digest": serde_json::to_value(&digest).unwrap_or(Value::Null) } })
        } else {
            json!({ "tool": "retrieve_exact", "result": { "state": "refused", "reason": omit_tag(reason) } })
        };
        let target = target.to_string();
        let tag = omit_tag(reason).to_string();
        Self::respond(
            ToolName::RetrieveExact,
            payload,
            0,
            budget,
            move |returned, _| MemdEvent::Refused {
                target,
                reason: tag,
                estimated_tokens_returned: returned,
            },
        )
    }

    async fn cost_hotspots<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now, ctx.ancestry)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let total: u64 = loaded
            .iter()
            .map(|l| u64::from(l.candidate.est_tokens))
            .sum();
        let mut by_tokens: Vec<&LoadedCandidate> = loaded.iter().collect();
        by_tokens.sort_by(|a, b| {
            b.candidate
                .est_tokens
                .cmp(&a.candidate.est_tokens)
                .then(a.candidate.path.cmp(&b.candidate.path))
        });
        let top_n = req.top_n.unwrap_or(5) as usize;
        let hotspots: Vec<Value> = by_tokens
            .into_iter()
            .take(top_n)
            .map(|l| {
                let t = u64::from(l.candidate.est_tokens);
                let share = if total == 0 {
                    0.0
                } else {
                    t as f64 / total as f64
                };
                json!({ "area": l.candidate.path, "est_tokens": t, "share": share })
            })
            .collect();
        // Descriptive only — the payload carries no directive.
        let payload = json!({ "tool": "cost_hotspots", "hotspots": hotspots, "note": COST_NOTE });
        Ok(Self::respond(
            ToolName::CostHotspots,
            payload,
            0,
            budget,
            manifest_event,
        ))
    }

    #[allow(clippy::too_many_lines)] // one cohesive assemble: key, sections, render
    async fn context_pack<S: CanonicalStore, R: SubstrateReader>(
        &self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        use rift_recall_pack::{
            CostNote, CriticalFact, EvidenceRef, PackRequest, SectionEntry, Sensitivity,
        };
        use rift_recall_validity::{envelope_for, resolve, MemoryDigestStore, SummaryStore};

        let (ranked, _in_scope) = Self::ranked_candidates(req, ctx).await?;
        let pack_tokens = pack_tokens(req);

        // Pack identity is input-keyed: computed from the immutable
        // inputs BEFORE any rendering, never from the output bytes. The agent
        // sends it back beside an ordinal, and `retrieve_by_citation`
        // recomputes it from these same inputs rather than looking it up.
        let pack_key =
            rift_recall_pack::pack_key(&pack_inputs(req, &ranked, self.policy.version()));

        // Every section entry is built from a sealed-constructor output:
        // `envelope_for` (with `resolve` supplying the validity
        // verdict) for trusted artifacts, the trust chokepoint for untrusted
        // provenance, parse-validated handles for recommendations, and the
        // closed critical/cost vocabularies. No inline strings enter a pack.
        //
        // The summary store is empty until ingest persists digest bindings (a
        // deliberate deferral): `resolve` then verdicts `Degraded`, so the
        // envelope honestly serves known-unchanged / exact-hunks / handles and
        // never fabricates a summary.
        let summaries = SummaryStore::new();
        let digests = MemoryDigestStore::default();
        let snapshot = rift_recall_validity::WorktreeSnapshot::new();
        let mut env_budget = rift_recall_validity::Budget::new(pack_tokens);

        let known: BTreeSet<&str> = req.known_hashes.iter().map(String::as_str).collect();
        let mut preq = PackRequest::new(pack_tokens);
        preq.set_cost_note(SectionEntry::cost_note(CostNote::RepeatedReadAreas))
            .map_err(|e| ToolError::Internal(e.to_string()))?;

        // **THE EVIDENCE ORDINAL PASS + THE CITATION TABLE** — one function
        // (`citation_pass`), shared with `retrieve_by_citation`, so the numbers
        // this pack publishes and the numbers that server resolves are the same
        // numbers by construction rather than by two matching loops.
        let table = citation_pass(&self.policy, &ranked);
        preq.set_citations(table.clone());
        for (rank, lc) in ranked.iter().take(PACK_CANDIDATES).enumerate() {
            let ordinal = u32::try_from(rank).unwrap_or(u32::MAX).saturating_add(1);
            // The table IS the admission list: an artifact the pass refused to
            // cite (policy-denied, or a path no record should hold) is one this
            // pack cannot name, so it is not rendered either. One screen, one
            // place — the two used to be written out twice and could drift.
            if table.get(ordinal).is_none() {
                continue;
            }
            // ADDRESSED, not bare: this pack has no human surface and no
            // ordinal->path map, so the content address is the only thing that
            // makes `evidence:3` resolvable by anyone at all. The hash is right
            // here in the ranked row; leaving it out made a spilled entry
            // identityless the moment `Recommended` degraded away, which
            // `degrade_order` does FIRST.
            // The screens the table applied, and why they are not repeated
            // here: `citation_pass` drops a policy-denied path (never served,
            // not even as a handle) and an anomalous one (absolute, `..`, or
            // control chars — never a legitimate repo artifact, and a
            // newline-bearing path could forge a directive line at any
            // placement). Both leave a gap in the numbering, and the `continue`
            // above is that gap reaching the renderer.
            let evidence = EvidenceRef::addressed(ordinal, &lc.content_sha256);
            let c = &lc.candidate;
            let sensitivity = if self.policy.exact_source_required(&c.path) {
                Sensitivity::Sensitive
            } else {
                Sensitivity::Normal
            };
            let handle_str = format!("file:{}@sha256:{}", c.path, lc.content_sha256);
            // Trust derives from provenance, never the caller.
            let trust = provenance_trust(&handle_str);
            // The recommendation names the artifact by its ordinal.
            // The handle still has to PARSE — a recommendation must stand for a
            // real address or the number names nothing — but its text stops
            // here; only the ordinal and the hash half of the address render.
            if let Ok(h) = handle_str.parse::<Handle>() {
                let _ = preq.push_recommended(SectionEntry::from_handle(&h, ordinal));
            }
            if trust.is_untrusted() {
                // Chokepoint: untrusted provenance enters as a neutral digest
                // only — counts + citation, never payload text. The digest
                // is handed `handle_str` for its own accounting; what the pack
                // renders is `evidence`, so the second of the two
                // retrieval names is an ordinal here too.
                let digest = serialize_untrusted("tool_output", b"", &handle_str);
                preq.push_changed(SectionEntry::from_neutral_cited(&digest, &evidence))
                    .map_err(|e| ToolError::Internal(e.to_string()))?;
                continue;
            }
            let seen = if known.contains(lc.content_sha256.as_str()) {
                SeenStatus::SeenSameHash
            } else {
                SeenStatus::NotSeenOrChanged
            };
            let validity = resolve(
                &summaries,
                &digests,
                &snapshot,
                &format!("summary:{}", c.path),
            );

            // Changed-hunk TEXT: the persisted line ranges slice
            // the revision's bytes; the ONLY raw inline goes through the
            // chokepoint's `TrustedPayload` — untrusted bytes cannot ride.
            let hunk_rows = ctx
                .store
                .query(
                    "SELECT new_start, new_len FROM n_changed_hunk \
                     WHERE file_revision = ?1 ORDER BY new_start ASC",
                    &[rift_store::Value::text(c.id.as_str())],
                )
                .await
                .map_err(|e| ToolError::Internal(e.to_string()))?;
            let is_changed = !hunk_rows.is_empty();
            let mut exact_text: Option<String> = None;
            if is_changed && seen == SeenStatus::NotSeenOrChanged {
                let bytes = ctx
                    .reader
                    .blob(&lc.content_sha256)
                    .map_err(|e| ToolError::Internal(e.to_string()))?
                    .unwrap_or_default();
                if !bytes.is_empty() {
                    if let Ok(t) = TrustedPayload::new(trust, bytes) {
                        let body = t.into_raw_body();
                        let hunks: Vec<rift_recall_ledger::hunks::Hunk> = hunk_rows
                            .iter()
                            .map(|r| {
                                let start =
                                    u32::try_from(r.get_i64(0).unwrap_or(0).max(0)).unwrap_or(0);
                                let len =
                                    u32::try_from(r.get_i64(1).unwrap_or(0).max(0)).unwrap_or(0);
                                rift_recall_ledger::hunks::Hunk {
                                    old_start: 0,
                                    old_len: 0,
                                    new_start: start,
                                    new_len: len,
                                    binary: false,
                                }
                            })
                            .collect();
                        let page = rift_recall_ledger::hunks::serve_hunks(
                            body.as_bytes(),
                            &hunks,
                            &c.path,
                            &lc.content_sha256,
                            env_budget.remaining(),
                        );
                        if !page.hunks.is_empty() {
                            let blocks: Vec<String> = page
                                .hunks
                                .iter()
                                .map(|h| format!("L{}-L{}:\n{}", h.range.lo, h.range.hi, h.text))
                                .collect();
                            exact_text = Some(blocks.join(""));
                        }
                    }
                }
            }

            // The four-state envelope: the SOLE per-artifact constructor
            // per-artifact constructor. Exact inlining is allowed only for non-sensitive
            // paths — a sensitive path is exact-gated (the audited
            // retrieve_exact) or a handle here, never pack-inlined or a
            // lossy summary.
            let exact_allowed = matches!(sensitivity, Sensitivity::Normal);
            let env = envelope_for(
                &c.path,
                trust,
                &handle_str,
                &lc.content_sha256,
                seen,
                &validity,
                exact_text.as_deref(),
                exact_allowed,
                &mut env_budget,
            );
            let entry = SectionEntry::from_envelope(&env, sensitivity, &evidence);
            if is_changed && seen == SeenStatus::NotSeenOrChanged {
                preq.push_changed(entry)
            } else {
                preq.push_unchanged(entry)
            }
            .map_err(|e| ToolError::Internal(e.to_string()))?;

            // Reverted provenance is decisive context — edge-placed,
            // from the closed critical-fact vocabulary.
            let reverted = ctx
                .store
                .query(
                    "SELECT ol.label_id FROM outcome_label ol \
                     JOIN n_file_read r ON r.checkpoint_id = ol.subject_id \
                     WHERE r.file_revision = ?1 AND ol.kind = 'reverted' LIMIT 1",
                    &[rift_store::Value::text(c.id.as_str())],
                )
                .await
                .map_err(|e| ToolError::Internal(e.to_string()))?;
            if !reverted.is_empty() {
                preq.push_critical(SectionEntry::critical(&CriticalFact::RevertedProvenance {
                    evidence: evidence.clone(),
                }))
                .map_err(|e| ToolError::Internal(e.to_string()))?;
            }
        }

        // The pack budget composes with the remaining TURN budget: the
        // effective cap is min(pack target, hard ceiling, remaining).
        // `compile` is a pure trial — the single charge happens at the seam.
        let pack = rift_recall_pack::compile(&preq, budget.tokens_remaining()).map_err(
            |rift_recall_pack::PackError::BudgetTooSmall { needed, cap }| {
                ToolError::PackBudgetTooSmall { needed, cap }
            },
        )?;
        let baseline = ranked
            .iter()
            .map(|l| l.candidate.est_tokens)
            .fold(0u32, u32::saturating_add);
        let sections = u32::try_from(pack.sections.len()).unwrap_or(u32::MAX);
        let total_tokens = pack.total_tokens;
        let payload = json!({
            "tool": "context_pack",
            "sections": pack.sections.iter().map(|s| json!({ "kind": format!("{:?}", s.kind), "lines": s.lines })).collect::<Vec<_>>(),
            "total_tokens": total_tokens,
            "pack_key": pack_key,
        });
        Ok(Self::respond(
            ToolName::ContextPack,
            payload,
            baseline,
            budget,
            move |returned, avoided| MemdEvent::PackBuilt {
                sections,
                total_tokens,
                estimated_tokens_returned: returned,
                estimated_tokens_avoided: avoided,
            },
        ))
    }

    // --- the document verb -------------------------------------------------

    /// Serve the Markdown sidecar Rift holds for a document the caller named.
    ///
    /// The verb is the pull side of the same substitution the hook makes: an
    /// agent that opens `spec.docx` should be reading Markdown, and where no
    /// hook can rewrite the call — Codex, Antigravity, a person at a terminal —
    /// this is how it asks for the same thing. What it hands back is the
    /// **raw** Markdown, in the position of the read it replaces, which is why
    /// it goes through the trust chokepoint rather than around it.
    ///
    /// Four things happen here and nowhere else on this path:
    ///
    /// 1. **The path is gated before the bytes are touched.** A denied path, an
    ///    unreadable policy and a sensitive path are refused with a neutral
    ///    tag; a sensitive document's exact source is reached through the
    ///    audited `retrieve_exact`, which asks for a reason, or not at all.
    /// 2. **The bytes go through the chokepoint.** A `TrustedPayload` is the
    ///    only way raw text leaves this crate, and it cannot be built from an
    ///    untrusted class.
    /// 3. **The state is named by the sealed constructor**, not by this
    ///    handler: the sidecar rides inline only as `exact_included`, and when
    ///    it does not fit, the answer names the file so the caller can read it
    ///    itself. The ceiling for that decision is the document's own declared
    ///    cost — a sidecar is worth inlining exactly while it is cheaper than
    ///    the document it replaces — never below the turn's own allowance.
    /// 4. **The figures are sealed at the accounting seam**, with the declared
    ///    baseline as the gross figure and [`TAG_DECLARED_BASELINE`] on the row
    ///    saying it is a declaration. Nothing is claimed for an answer that
    ///    carried no Markdown.
    fn doc_read(
        &self,
        req: &ToolRequest,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let path = req.target.clone().unwrap_or_default();
        let Some(doc) = req.document.as_ref() else {
            // The wire decoder cannot reach the family's sidecar store, so a
            // request that arrived without one was not resolved by anybody.
            // Answering it would mean this crate going looking for a file.
            return Err(ToolError::Internal(
                "doc_read: no resolved sidecar on the request".to_owned(),
            ));
        };
        let handle = file_handle(&path, &doc.source_sha256);
        // An unreadable policy is not a permissive one, and a denied path is
        // not served — not the document, and not the fact that Rift converted
        // it. Both answer under one tag: which gate held it back is the
        // caller's question to have answered, not an attacker's.
        if self.policy.is_fail_closed() || self.policy.is_denied_path(&path) {
            return Ok(Self::refuse_doc(
                &handle,
                &path,
                doc,
                OmitReason::PolicyDeniedExact,
                budget,
            ));
        }
        // A sensitive path names its own gate, because the caller can act on
        // it: the audited verb that asks for a reason is the way in.
        if self.policy.exact_source_required(&path) {
            return Ok(Self::refuse_doc(
                &handle,
                &path,
                doc,
                OmitReason::SensitivePathRequiresReason,
                budget,
            ));
        }
        let sidecar = match &doc.sidecar {
            SidecarState::Held(sidecar) => sidecar,
            SidecarState::Owed => {
                return Ok(Self::doc_without_a_sidecar(
                    &handle,
                    &path,
                    doc,
                    TAG_SIDECAR_OWED,
                    budget,
                ))
            }
            SidecarState::Declined => {
                return Ok(Self::doc_without_a_sidecar(
                    &handle,
                    &path,
                    doc,
                    TAG_SIDECAR_DECLINED,
                    budget,
                ))
            }
        };
        // Provenance decides, never the caller: a document in the checkout is
        // a code fact, and anything whose address does not parse as one fails
        // closed to untrusted — which cannot become a `TrustedPayload` at all.
        let trust = provenance_trust(&handle);
        let body = match TrustedPayload::new(trust, sidecar.markdown.clone().into_bytes()) {
            Ok(payload) => payload.into_raw_body(),
            Err(_) => {
                return Ok(Self::refuse_doc(
                    &handle,
                    &path,
                    doc,
                    OmitReason::PolicyDeniedExact,
                    budget,
                ))
            }
        };
        let state = Self::doc_state(&path, trust, &handle, doc, &body, budget);
        let inlined = state == "exact_included";
        // Claimed only for an answer that carried the Markdown. A caller sent
        // to read the sidecar itself has been saved something too, and this
        // verb does not know how much, so it books nothing.
        let (baseline, reason) = if inlined {
            (doc.declared_baseline, TAG_DECLARED_BASELINE.to_owned())
        } else {
            (0, omit_tag(OmitReason::BudgetExhausted).to_owned())
        };
        let payload = json!({
            "tool": "doc_read",
            "path": path,
            "state": state,
            "basis_sha256": doc.source_sha256,
            "sidecar": sidecar.path,
            "content": inlined.then_some(body),
            "reason": reason,
        });
        let audited = reason.clone();
        Ok(Self::respond(
            ToolName::DocRead,
            payload,
            baseline,
            budget,
            move |returned, avoided| MemdEvent::DocServed {
                handle,
                reason: audited,
                estimated_tokens_returned: returned,
                estimated_tokens_avoided: avoided,
            },
        ))
    }

    /// The four-state word for one sidecar, from the sealed constructor.
    ///
    /// The ceiling handed to it is the **document's own declared cost**, and
    /// nothing else where there is one. Markdown that costs more than the
    /// container it stands in for has saved nobody anything, and the honest
    /// answer there is the path rather than the text — however much of the turn
    /// is left to spend on it. The allowance does not get a veto in the other
    /// direction either, because the alternative to serving the Markdown is not
    /// silence: it is the caller reading the document itself, at the declared
    /// figure. A document that declares nothing has no such cost to be weighed
    /// against, and there the turn's own allowance is the only ceiling there
    /// is. The answer claims nothing for itself either way, since what is
    /// booked is the declaration net of what it cost. No summary is bound to a
    /// sidecar — its validity is its key — so the constructor decides between
    /// the bytes and a handle on the budget alone.
    fn doc_state(
        path: &str,
        trust: Trust,
        handle: &str,
        doc: &Document,
        body: &str,
        budget: &TurnBudget,
    ) -> String {
        use rift_recall_validity::{envelope_for, MemoryDigestStore, SummaryStore};

        let validity = rift_recall_validity::resolve(
            &SummaryStore::new(),
            &MemoryDigestStore::default(),
            &rift_recall_validity::WorktreeSnapshot::new(),
            &format!("summary:{path}"),
        );
        let mut vbudget = Budget::new(if doc.declared_baseline == 0 {
            budget.tokens_remaining()
        } else {
            doc.declared_baseline
        });
        let envelope = envelope_for(
            path,
            trust,
            handle,
            &doc.source_sha256,
            SeenStatus::NotSeenOrChanged,
            &validity,
            Some(body),
            true,
            &mut vbudget,
        );
        envelope_view(&envelope)
            .get("state")
            .and_then(Value::as_str)
            .unwrap_or("available_by_handle")
            .to_owned()
    }

    /// The answer when Rift holds no Markdown for a document: what it is about
    /// to hold (`sidecar_owed`), or what it has decided it never will
    /// (`sidecar_declined`). Neither is a refusal and neither is silence, and
    /// both are audited like any other serve — a deferral that booked no row
    /// would be a serve nobody could count.
    fn doc_without_a_sidecar(
        handle: &str,
        path: &str,
        doc: &Document,
        tag: &'static str,
        budget: &mut TurnBudget,
    ) -> ToolResponse {
        let payload = json!({
            "tool": "doc_read",
            "path": path,
            "state": "pending",
            "basis_sha256": doc.source_sha256,
            "sidecar": Value::Null,
            "content": Value::Null,
            "reason": tag,
        });
        let handle = handle.to_owned();
        // What the caller received, not what it was worth: an answer that
        // hands over no Markdown still costs the tokens it is written in, and
        // a row booking those away would take the cost of asking out of every
        // honesty figure this plane publishes.
        Self::respond(ToolName::DocRead, payload, 0, budget, move |returned, _| {
            MemdEvent::DocDeferred {
                handle,
                reason: tag.to_owned(),
                estimated_tokens_returned: returned,
            }
        })
    }

    /// A refusal on the document verb, audited and measured like every other
    /// serve. It names the path the CALLER named and nothing about the
    /// document: a refusal that quoted the Markdown it withheld would have
    /// withheld nothing.
    fn refuse_doc(
        handle: &str,
        path: &str,
        doc: &Document,
        reason: OmitReason,
        budget: &mut TurnBudget,
    ) -> ToolResponse {
        let payload = json!({
            "tool": "doc_read",
            "path": path,
            "state": "refused",
            "basis_sha256": doc.source_sha256,
            "sidecar": Value::Null,
            "content": Value::Null,
            "reason": omit_tag(reason),
        });
        let target = handle.to_owned();
        let tag = omit_tag(reason).to_owned();
        Self::respond(ToolName::DocRead, payload, 0, budget, move |returned, _| {
            MemdEvent::Refused {
                target,
                reason: tag,
                estimated_tokens_returned: returned,
            }
        })
    }

    // --- the search verb ---------------------------------------------------

    /// Answer a query with the paths in this checkout it is about, and — when
    /// the caller asked — the lines inside them that match.
    ///
    /// The pull side of the listing answer the hook path pushes: an agent that
    /// would have run a glob and read six files to find one runs this instead,
    /// and where no hook can answer for it — Codex, Antigravity, a person at a
    /// terminal — this is how it asks the same question.
    ///
    /// Four things happen here and nowhere else on this path:
    ///
    /// 1. **The policy screens the paths, not the caller.** A denied path is
    ///    not named and its lines are not read out; a sensitive path may still
    ///    be *named* — a name is not its contents — while its exact lines are
    ///    reached through the audited `retrieve_exact`, which asks for a
    ///    reason, or not at all. What the screens kept out is counted and
    ///    stated.
    /// 2. **A path renders as a neutral span or not at all.** The paths are the
    ///    part of this answer Rift composed rather than the part the caller
    ///    asked for, so they go out through the same renderer every pushed span
    ///    does, and one that will not render is counted rather than quoted.
    /// 3. **A line goes through the trust chokepoint.** Its provenance is a
    ///    tracked file of this checkout that the member's index holds — the
    ///    index excludes what git ignores and what policy calls sensitive, and
    ///    the engine drops what git does not track — so it is a code fact, and
    ///    what is checked here rather than trusted is the shape of the name it
    ///    arrived under. A `TrustedPayload` is the only way its bytes leave
    ///    this crate, and an unsafe name cannot build one.
    /// 4. **The figures are sealed at the accounting seam**, against the
    ///    declared listing baseline — and against nothing at all for a content
    ///    search, which has no declared native cost in this tree.
    fn search(
        &self,
        req: &ToolRequest,
        budget: &mut TurnBudget,
    ) -> Result<ToolResponse, ToolError> {
        let Some(search) = req.search.as_ref() else {
            // The wire decoder cannot open an index, so a request that arrived
            // without one was not resolved by anybody. Answering it would mean
            // this crate going looking for a work tree.
            return Err(ToolError::Internal(
                "search: no resolved index on the request".to_owned(),
            ));
        };
        // An unreadable policy is not a permissive one: with no deny list to
        // consult, the honest answer is no answer rather than every path.
        if self.policy.is_fail_closed() {
            return Ok(Self::search_without_an_answer(
                search,
                "refused",
                omit_tag(OmitReason::PolicyDeniedExact),
                budget,
            ));
        }
        let IndexState::Answered { hits, total, lines } = &search.index else {
            return Ok(Self::search_without_an_answer(
                search,
                "pending",
                TAG_INDEX_PENDING,
                budget,
            ));
        };

        let Screened {
            paths,
            rows,
            withheld,
            denied,
        } = self.screen_search(hits, lines);

        // The droppable sequence is spine-first: a caller that asked for lines
        // is answered with lines, and the ranked paths are how they were found.
        // Dropping from the tail therefore costs the cheaper half of whichever
        // question was actually asked.
        let content = search.content;
        let (spine, tail) = if content {
            (rows, paths)
        } else {
            (paths, rows)
        };
        let total = *total;
        let n = spine.len().saturating_add(tail.len());
        let cap = manifest_cap(req, budget);
        let reason = if content {
            TAG_BASELINE_NOT_DECLARED
        } else {
            TAG_DECLARED_BASELINE
        };
        let build = |kept: usize, truncated: bool| {
            let spine_kept = kept.min(spine.len());
            let tail_kept = kept.saturating_sub(spine.len());
            let a = spine.get(..spine_kept).unwrap_or(&[]);
            let b = tail.get(..tail_kept).unwrap_or(&[]);
            let (lines, paths) = if content { (a, b) } else { (b, a) };
            json!({
                "tool": "search",
                "state": "answered",
                "content": content,
                "paths": paths,
                "lines": lines,
                "total": total,
                "truncated": truncated,
                "omitted": n.saturating_sub(kept),
                "withheld": withheld,
                "denied": denied,
                "reason": reason,
            })
        };
        let (payload, _kept) = fit_to_cap(cap, n, &build);
        Ok(Self::respond(
            ToolName::Search,
            payload,
            search.declared_baseline,
            budget,
            move |returned, avoided| MemdEvent::SearchServed {
                estimated_tokens_returned: returned,
                estimated_tokens_avoided: avoided,
            },
        ))
    }

    /// The two screens every search answer passes, applied once and counted.
    ///
    /// Kept apart from [`Self::search`] because it is the whole of what this
    /// crate decides about a search: everything else there is shaping and
    /// accounting. A path the repository denies is not named and its lines are
    /// not read out; a sensitive path may still be *named* — a name is not its
    /// contents — while its exact lines are reached through the audited
    /// `retrieve_exact`, which asks for a reason, or not at all. A name that is
    /// not a repository artifact, or that the renderer will not render, is
    /// counted rather than quoted, and a line is never attributed to a name the
    /// answer would not print. Both counts are stated in the answer, because a
    /// silently shorter list reads as "there is no such file" and sends the
    /// agent back to reading its way to one.
    fn screen_search(&self, hits: &[SearchPath], lines: &[SearchLine]) -> Screened {
        let mut denied: BTreeSet<&str> = BTreeSet::new();
        let mut withheld = 0usize;
        let mut paths: Vec<Value> = Vec::new();
        for hit in hits {
            if self.policy.is_denied_path(&hit.path) {
                denied.insert(hit.path.as_str());
                continue;
            }
            match renderable_path(&hit.path) {
                Some(span) => paths.push(json!({
                    "path": span,
                    "status": status_word(hit.status),
                })),
                None => withheld = withheld.saturating_add(1),
            }
        }
        let mut rows: Vec<Value> = Vec::new();
        for line in lines {
            if self.policy.is_denied_path(&line.path)
                || self.policy.exact_source_required(&line.path)
            {
                denied.insert(line.path.as_str());
                continue;
            }
            let Some(span) = renderable_path(&line.path) else {
                withheld = withheld.saturating_add(1);
                continue;
            };
            // Provenance decides, never the caller: an anomalous name is not a
            // repository artifact, it fails closed to untrusted, and an
            // untrusted class cannot become a `TrustedPayload` at all.
            let Ok(body) = TrustedPayload::new(
                matched_line_trust(&line.path),
                line.text.clone().into_bytes(),
            ) else {
                withheld = withheld.saturating_add(1);
                continue;
            };
            rows.push(json!({
                "path": span,
                "line": line.line,
                "text": body.into_raw_body(),
            }));
        }
        Screened {
            paths,
            rows,
            withheld,
            denied: denied.len(),
        }
    }

    /// A search that carries no ranked answer: the index is not built yet, or
    /// the policy could not be read.
    ///
    /// Neither is silence and neither claims anything — the baseline is zero
    /// whatever the caller's shape was, because nothing was replaced — and both
    /// are audited like any other serve, since a deferral that booked no row
    /// would be a serve nobody could count.
    fn search_without_an_answer(
        search: &Search,
        state: &'static str,
        reason: &'static str,
        budget: &mut TurnBudget,
    ) -> ToolResponse {
        let payload = json!({
            "tool": "search",
            "state": state,
            "content": search.content,
            "paths": Vec::<Value>::new(),
            "lines": Vec::<Value>::new(),
            "total": 0,
            "truncated": false,
            "omitted": 0,
            "withheld": 0,
            "denied": 0,
            "reason": reason,
        });
        Self::respond(ToolName::Search, payload, 0, budget, move |returned, _| {
            MemdEvent::SearchServed {
                estimated_tokens_returned: returned,
                estimated_tokens_avoided: 0,
            }
        })
    }

    /// The accounting seam: serialize the payload once (the same
    /// bytes the transport ships — serde_json default field order is
    /// load-bearing), count it with the real tokenizer, perform the single
    /// clamped turn-budget charge, and seal the event from those same figures.
    /// `baseline` is the gross ingest-time estimate this response stands in
    /// for; the emitted `avoided` is net (`baseline − returned`, saturating).
    /// Handlers never mutate the budget — every shaping decision upstream is a
    /// non-charging trial against `tokens_remaining()`.
    fn respond(
        tool: ToolName,
        payload: Value,
        baseline: u32,
        budget: &mut TurnBudget,
        event: impl FnOnce(u32, u32) -> MemdEvent,
    ) -> ToolResponse {
        let returned = rift_tokens::count(&payload.to_string());
        let _ = charge(budget, returned);
        let avoided = baseline.saturating_sub(returned);
        let event = event(returned, avoided);
        ToolResponse {
            tool,
            estimated_tokens_returned: returned,
            estimated_tokens_avoided: avoided,
            tokenizer: rift_tokens::provenance(),
            payload,
            event,
        }
    }
}

/// The per-tool changed-hunk cap (the default ceiling).
pub const MAX_CHANGED_HUNKS: u32 = 8;

/// A granted exact retrieval, ready to serve. A struct rather than seven
/// arguments, and every field is a decision the two routes make differently.
struct ExactGrant {
    /// What the audit row records this retrieval as. For a citation this is
    /// the pack key, the ordinal AND the resolved address (the four
    /// facts, in the one neutral column the sink has for them).
    audit_name: String,
    /// The identity the agent-facing **exact** payload states: the path for a
    /// handle the caller already held, the ordinal for a citation — whose whole
    /// point is that the agent never learns the path.
    exact_id: (&'static str, String),
    /// The identity the over-budget downgrade payload states: the name the
    /// agent would retry with.
    handle_id: (&'static str, String),
    /// The path the policy gate saw (the downgrade re-checks it).
    policy_path: String,
    /// The content address whose bytes are served.
    sha: String,
    /// Provenance-derived trust, never caller-declared.
    trust: Trust,
    /// The caller's mandatory reason, as the audit records it.
    reason: String,
}

/// [`exact_sha`] as an owned string (the grant outlives the borrow).
fn exact_sha_owned(target: &str) -> String {
    exact_sha(target).to_string()
}

/// The audit identity of one citation resolution: `pack:<key>/evidence:<n>`,
/// and — once the table has answered — the address it resolved to.
///
/// It is a composed string rather than new columns because the serve audit sink
/// is an append-only relation whose shape is owned elsewhere; widening it to
/// two more columns is a schema change owned elsewhere, not one this module makes.
fn citation_audit_name(
    citation: &rift_recall_pack::CitationTarget,
    resolved: Option<&rift_recall_pack::Citation>,
) -> String {
    let base = format!(
        "{}{}/{}{}",
        rift_recall_pack::PACK_TAG,
        citation.pack_key,
        rift_recall_pack::EVIDENCE_TAG,
        citation.ordinal
    );
    match resolved {
        Some(c) => format!("{base}=file:{}@sha256:{}", c.path(), c.content_sha256()),
        None => base,
    }
}

/// The query string a pack's identity is keyed on, spelled once so
/// the compile side and the retrieval side cannot drift into two spellings of
/// "the same pack".
fn pack_query(req: &ToolRequest) -> String {
    format!(
        "task:{}|focus:{}|errors:{}",
        req.task.as_deref().unwrap_or(""),
        req.files_in_focus.join(","),
        req.error_signatures.join(","),
    )
}

/// The pack budget a request asks for (pre-clamp — the contract input the pack
/// key is computed over).
fn pack_tokens(req: &ToolRequest) -> u32 {
    if req.token_budget == 0 {
        rift_recall_pack::DEFAULT_PACK_TOKENS
    } else {
        req.token_budget
    }
}

/// The immutable inputs a pack's identity is computed from.
fn pack_inputs(
    req: &ToolRequest,
    ranked: &[LoadedCandidate],
    policy_version: u32,
) -> rift_recall_pack::PackKeyInputs {
    rift_recall_pack::PackKeyInputs {
        query: pack_query(req),
        ranked: ranked
            .iter()
            .map(|l| (l.candidate.id.clone(), l.content_sha256.clone()))
            .collect(),
        policy_version,
        token_budget: pack_tokens(req),
    }
}

/// **THE EVIDENCE ORDINAL PASS and the citation table**, in one function.
///
/// Called by `context_pack` to build the table its pack carries and by
/// `retrieve_by_citation` to rebuild the same table, so the two cannot be two
/// tables. The ordinal is the index in the ranked fact set's own order, never a
/// position in the finished pack, which `compile` reorders and shortens.
///
/// The skips leave a **GAP**: a policy-denied path is never served — not even
/// as a handle, and now not as an ordinal either — and an anomalous path
/// (absolute, `..`, control chars) is never a legitimate repo artifact. Neither
/// renumbers what follows, so a dropped artifact cannot silently rename the
/// next one, and an ordinal in a gap resolves to a refusal.
fn citation_pass(
    policy: &PolicyEngine<'_>,
    ranked: &[LoadedCandidate],
) -> rift_recall_pack::Citations {
    let mut table = rift_recall_pack::Citations::new();
    for (rank, lc) in ranked.iter().take(PACK_CANDIDATES).enumerate() {
        let ordinal = u32::try_from(rank).unwrap_or(u32::MAX).saturating_add(1);
        if policy.is_denied_path(&lc.candidate.path) {
            continue;
        }
        // `cite` itself refuses an unsafe path and half an address, so the
        // anomalous-path screen lives in one place rather than two.
        let _ = table.cite(ordinal, &lc.candidate.path, &lc.content_sha256);
    }
    table
}

/// How many ranked candidates a context pack considers.
///
/// It is part of the **citation protocol**, not a local tuning knob: the
/// ordinal a pack publishes is this pass's index, so an ordinal past this bound
/// names nothing and `retrieve_exact` refuses it.
pub const PACK_CANDIDATES: usize = 8;

/// The default manifest token cap when the request carries no budget
/// (pack parity: mirrors `DEFAULT_PACK_TOKENS`, deliberately
/// below the turn ceiling so an unbudgeted anchor call leaves room for
/// same-turn content serves).
pub const DEFAULT_MANIFEST_TOKENS: u32 = 1200;

/// The hard ceiling an explicit manifest `token_budget` is clamped to
/// (pack parity: mirrors `MAX_PACK_TOKENS`).
pub const MAX_MANIFEST_TOKENS: u32 = 4000;

/// The effective manifest cap: the pack-parity default/max
/// split composed with the remaining turn budget. A non-charging trial —
/// the single charge stays at the respond seam.
fn manifest_cap(req: &ToolRequest, budget: &TurnBudget) -> u32 {
    if req.token_budget == 0 {
        DEFAULT_MANIFEST_TOKENS
    } else {
        req.token_budget.clamp(1, MAX_MANIFEST_TOKENS)
    }
    .min(budget.tokens_remaining())
}

/// The largest droppable-entry prefix whose serialized payload fits `cap`.
/// `build(kept, truncated)` rebuilds the full payload for a
/// prefix of `kept` entries; entries must be ordered so dropping from the
/// tail is the right sacrifice (rank order for the manifest, unchanged-last
/// for the delta list). Deterministic: a fast-path full fit, a binary search
/// for the largest fitting prefix, then a defensive linear walk-down — BPE
/// token counts are not formally monotone under concatenation, so the
/// postcondition (`fits(kept) || kept == 0`) is guaranteed, not assumed.
/// With nothing droppable (`n == 0`) or nothing fitting, the skeleton still
/// ships (the anchor is never stranded); the seam's clamped charge absorbs a
/// skeleton that exceeds a tiny cap.
fn fit_to_cap(cap: u32, n: usize, build: &dyn Fn(usize, bool) -> Value) -> (Value, usize) {
    let full = build(n, false);
    if n == 0 || rift_tokens::count(&full.to_string()) <= cap {
        return (full, n);
    }
    let fits = |k: usize| rift_tokens::count(&build(k, true).to_string()) <= cap;
    // Binary search over [0, n): find the first prefix length that does NOT
    // fit (assuming monotone prefix costs), then step back one.
    let (mut lo, mut hi) = (0usize, n);
    while lo < hi {
        let mid = lo.saturating_add(hi.saturating_sub(lo) / 2);
        if fits(mid) {
            lo = mid.saturating_add(1);
        } else {
            hi = mid;
        }
    }
    let mut kept = lo.saturating_sub(1);
    // Walk down to certainty (non-monotone counts cannot break the bound).
    while kept > 0 && !fits(kept) {
        kept = kept.saturating_sub(1);
    }
    (build(kept, true), kept)
}

/// The largest fitting payload, sacrificing the deltas' **story** before any
/// delta.
///
/// Three attempts in one fixed order, and the order is the whole content of
/// this function: the payload with provenance; the payload with every
/// provenance object downgraded to its tag and its command; and only then
/// [`fit_to_cap`], which starts dropping entries. A delta the caller cannot see
/// is a re-read it has to do; a delta whose story it cannot see is one command
/// away. That is the same sacrifice the sensitive-path rule makes — serve the
/// handle rather than half the thing — applied to the rider instead of the
/// body, and it is decided on the **whole** serialized payload rather than
/// per entry, because the cap is a fact about the payload.
///
/// A non-charging trial like every other shaping decision: the single charge
/// stays at the respond seam.
fn fit_with_provenance(
    cap: u32,
    n: usize,
    build: &dyn Fn(usize, bool, bool) -> Value,
) -> (Value, usize) {
    let told = build(n, false, true);
    if n == 0 || rift_tokens::count(&told.to_string()) <= cap {
        return (told, n);
    }
    let plain = build(n, false, false);
    if rift_tokens::count(&plain.to_string()) <= cap {
        return (plain, n);
    }
    fit_to_cap(cap, n, &|kept, truncated| build(kept, truncated, false))
}

/// The checkpoint each entry of a manifest belongs to, and the commits that
/// anchored them.
///
/// Resolved once per serve rather than per entry. The anchors are already on
/// the rows the handler loaded — the candidate loader picks each revision's
/// most-recent in-scope checkpoint to decide recency, and now carries it — so
/// the only query this costs is the one for the commit mirror, which is asked
/// for the whole set at once.
struct Story {
    /// Checkpoint id → the commit it anchored, where one is mirrored.
    commits: BTreeMap<String, String>,
}

impl Story {
    /// Resolve the story for a loaded candidate set.
    ///
    /// Never fails the serve: a projection that cannot answer the commit
    /// mirror yields an empty map, and every delta then carries its checkpoints
    /// without a commit. An unavailable fact is absent, not fatal.
    async fn over<S: CanonicalStore, R: SubstrateReader>(
        ctx: &RecallContext<'_, S, R>,
        loaded: &[LoadedCandidate],
        known: &BTreeSet<&str>,
    ) -> Self {
        let ids: Vec<String> = loaded
            .iter()
            .filter(|lc| !known.contains(lc.content_sha256.as_str()))
            .map(|lc| lc.checkpoint.clone())
            .collect();
        Self {
            commits: rift_recall_ledger::provenance::commit_oids(ctx.store, &ids)
                .await
                .unwrap_or_default(),
        }
    }

    /// The checkpoint that recorded the revision of `path` the caller says it
    /// holds, when this family recorded one.
    ///
    /// **Path-bound.** A hash the caller holds for some *other* file is not a
    /// basis for this one, and pairing the two would attribute a change to a
    /// checkpoint that never touched the path — the same failure the pull
    /// verb's own resolver exists to prevent.
    fn held_in<'c>(
        loaded: &'c [LoadedCandidate],
        known: &BTreeSet<&str>,
        path: &str,
    ) -> Option<&'c str> {
        loaded
            .iter()
            .find(|l| l.candidate.path == path && known.contains(l.content_sha256.as_str()))
            .map(|l| l.checkpoint.as_str())
    }

    /// Attach the full object to one entry, or nothing when the delta is not
    /// anchored to a checkpoint.
    fn attach(
        &self,
        oracle: &dyn ProvenanceOracle,
        entry: &mut Value,
        loaded: &[LoadedCandidate],
        lc: &LoadedCandidate,
        known: &BTreeSet<&str>,
        line: Option<u32>,
    ) {
        let bracket = provenance::Bracket {
            from: Self::held_in(loaded, known, &lc.candidate.path),
            to: Some(lc.checkpoint.as_str()),
            commit: self.commits.get(&lc.checkpoint).map(String::as_str),
        };
        let (Some(value), Some(obj)) = (
            provenance::object(oracle, &bracket, &lc.candidate.path, line),
            entry.as_object_mut(),
        ) else {
            return;
        };
        obj.insert("provenance".to_owned(), value);
    }

    /// Attach the downgraded form: the tag, and the command that fetches the
    /// rest. Unconditional, because a caller that saw the full object on a
    /// roomier turn must not read its absence here as "nothing is recorded".
    fn attach_handle(entry: &mut Value, path: &str, line: Option<u32>) {
        let Some(obj) = entry.as_object_mut() else {
            return;
        };
        let (tag, why) = provenance::downgraded(path, line);
        obj.insert("provenance".to_owned(), tag);
        obj.insert("why".to_owned(), why);
    }
}

/// Whether this call is a **pull against a basis**: a path and the revision the
/// caller holds, both named.
///
/// It is the one question that decides the shape of a `changed-hunks` call —
/// which handler answers it, and whether ranking (and so a task and a focus
/// set) is required at all — so it is asked in one place and both callers read
/// the same answer.
fn targeted_pull(tool: ToolName, req: &ToolRequest) -> bool {
    matches!(tool, ToolName::ChangedHunks)
        && req.target.as_deref().is_some_and(|t| !t.trim().is_empty())
        && !req.known_hashes.is_empty()
}

/// Render an [`Envelope`] to a JSON view (state tag + hash, never raw bytes for
/// the manifest states).
fn envelope_view(e: &Envelope) -> Value {
    let (state, basis): (&str, Option<String>) = match &e.state {
        ArtifactState::KnownUnchanged { basis_sha256, .. } => {
            ("known_unchanged", Some(basis_sha256.clone()))
        }
        ArtifactState::SummaryIncluded { basis_sha256, .. } => {
            ("summary_included", Some(basis_sha256.clone()))
        }
        ArtifactState::ExactIncluded { basis_sha256, .. } => {
            ("exact_included", Some(basis_sha256.clone()))
        }
        ArtifactState::AvailableByHandle { basis_sha256, .. } => {
            ("available_by_handle", basis_sha256.clone())
        }
    };
    json!({ "path": e.path, "state": state, "basis_sha256": basis })
}

/// The word a search answer states a path's git status as.
///
/// Rift's own fixed vocabulary, read off the one enum the pushed frame renders
/// from, so the JSON a caller parses and the sentence a model reads can never
/// name two different sets of states.
fn status_word(status: PathStatus) -> &'static str {
    match status {
        PathStatus::Tracked => "tracked",
        PathStatus::Modified => "modified",
        PathStatus::Staged => "staged",
        PathStatus::Untracked => "untracked",
    }
}

/// A path as a search answer may render it, or `None`.
///
/// Two screens, in this order. The first is structural: an absolute path, one
/// that climbs, or one carrying a control character is not a repository
/// artifact at all, and the citation pass drops one for the same reason — a
/// newline-bearing path could forge a line at any placement. The second is the
/// renderer's, because a path in this answer is a span Rift composed rather
/// than one the caller handed back, and it is judged like every other.
fn renderable_path(path: &str) -> Option<String> {
    path_is_safe(path).then(|| neutral_inline_span(path))?
}

/// The trust class of a line a content search matched.
///
/// A hit's provenance is fixed before it reaches this crate: it is a line of a
/// file the member's path index holds — which excludes what git ignores and
/// what policy calls sensitive — that git tracks, which is the same provenance
/// as any other blob in the work tree. So it is a code fact, exactly as an
/// exact retrieval of the same file would be, and it is served as one rather
/// than digested: an agent that ran the content search this replaces would have
/// been shown these bytes, and withholding a line for the shape of its own text
/// would be a false negative on a search rather than a defence.
///
/// What is checked here rather than trusted is the *name*: an unsafe path fails
/// closed to untrusted, which cannot become a [`TrustedPayload`] at all.
fn matched_line_trust(path: &str) -> Trust {
    if path_is_safe(path) {
        Trust::CodeFact
    } else {
        Trust::ToolOutputUntrusted
    }
}

/// The content hash a `file:<path>@sha256:<hash>` handle/path encodes (for blob
/// fetch); falls back to the whole string when no `sha256:` marker is present.
#[must_use]
pub fn exact_sha(target: &str) -> &str {
    target
        .rsplit_once("@sha256:")
        .map_or(target, |(_, h)| h.split(':').next().unwrap_or(h))
}

/// Derive the trust class of an exact-retrieval target from **provenance**, not
/// the caller: a `file:`/`summary:` handle is a structural code fact;
/// a `log:` handle is untrusted tool output; anything that does not parse as a
/// known handle fails closed to untrusted (so unknown provenance is never raw).
#[must_use]
pub fn provenance_trust(target: &str) -> Trust {
    match target.parse::<Handle>() {
        Ok(Handle::File { .. } | Handle::Summary { .. }) => Trust::CodeFact,
        // a log handle is untrusted tool output; anything unparseable fails closed.
        Ok(Handle::Log { .. }) | Err(_) => Trust::ToolOutputUntrusted,
    }
}

/// The repo-relative path a target names, for policy matching (denied/sensitive
/// globs match the artifact path, not the raw handle string). Non-`file:` handles
/// have no repo path; their id stands in (policy denies untrusted regardless).
#[must_use]
pub fn handle_display_path(target: &str) -> String {
    match target.parse::<Handle>() {
        Ok(Handle::File { path, .. }) => path,
        Ok(Handle::Log { tool_use_id }) => tool_use_id,
        Ok(Handle::Summary { key }) => key,
        Err(_) => target.to_string(),
    }
}

/// Charge `cost` against the turn budget, returning the amount actually charged
/// (clamped to the remaining budget so the ceiling is never breached). Called
/// from exactly one place — the respond seam (structurally tested).
fn charge(budget: &mut TurnBudget, cost: u32) -> u32 {
    let allowed = cost.min(budget.tokens_remaining());
    let _ = budget.try_tokens(allowed);
    allowed
}

/// The manifest-shaped event builder every list/awareness tool hands to the
/// respond seam (the figures are supplied by the seam).
fn manifest_event(returned: u32, avoided: u32) -> MemdEvent {
    MemdEvent::ManifestServed {
        estimated_tokens_returned: returned,
        estimated_tokens_avoided: avoided,
    }
}

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

/// The two reason frames a declined read is answered with, re-exported by the
/// same route and for the same reason as [`render_delta_manifest`].
///
/// The hook path composes a decline; the sentence a model reads is written by
/// the sealed renderer beside the manifest's, so there is one place in this
/// tree where pushed text comes into existence. `bins/rift` reaches it here
/// because this dispatcher is already the validity crate's caller — no crate
/// edge is added, and no second renderer is created.
pub use rift_recall_validity::trust::{render_delta_frame, render_deny_frame};

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

/// The audit envelope for an **injected** serve — one that carries no tool
/// response because the agent never asked for it (the session pointer, or the
/// per-turn delta manifest). Injection is pure
/// spend: `tokens` were measured by the real tokenizer and nothing was avoided,
/// so the savings ledger records the cost and claims no credit. The envelope is
/// authored by the events crate, so an injected serve and a requested one are
/// audited through the same contract.
///
/// It is audited under its **own** verb, `memd.manifest_injected`. Sharing
/// `memd.manifest_served` with the pull side made the two indistinguishable in
/// `memd_audit_event`, and *by construction* rather than by bad luck: a row's
/// discriminating columns are the verb, the two token figures, a handle and a
/// reason; an injection sets none of the last three, so under a shared verb it
/// is a pull row that avoided nothing, and the relation holds no fact that says
/// otherwise. Netting injected spend against served credit therefore meant
/// inferring the side from *outside* the row — a missing per-call
/// sibling, `tokens_avoided = 0` — which is a guess about history, not a
/// measurement, however well it scores on any one database. One verb had been
/// spent on two meanings, and no query can un-spend it. The split makes the
/// netting a query.
///
/// The split is new rows only; the relation is append-only and nothing existing
/// was rewritten, so a `manifest_served` row predating the split may still be
/// either side and no query can tell. What is checked, rather than asserted, is
/// the going-forward half: this is the one envelope builder both injection
/// sites use (the session pointer and the per-turn manifest),
/// and each site has a test that reads back the row its **production** path
/// wrote and asserts the verb on it.
///
/// `occurrence` keys the row. An injected pointer is byte-identical in every
/// session, so a payload-only key would count the first injection and silently
/// discard every later one — understating spend, which inflates net savings.
/// Passing the session (stable within one injection, distinct across sessions)
/// keeps retries idempotent while letting each session's cost land.
#[must_use]
pub fn injection_audit_envelope(tokens: u32, occurrence: &str) -> String {
    MemdEvent::ManifestInjected {
        estimated_tokens_returned: tokens,
    }
    .to_hook_json_keyed(occurrence)
}

/// The audit envelope for a serve a **cross-branch grant** widened.
///
/// It is the ordinary per-invocation envelope with one member added to its
/// payload: `reason`, set to the fixed tag [`CROSS_BRANCH_GRANT_TAG`], which is
/// the column `memd_audit_event.reason` — a frozen v4 relation whose contract
/// says that column holds a **neutral tag** — can carry. That is the whole
/// point of doing it here rather than at the writer: the value is a constant of
/// this crate, so no path exists by which a caller's text could reach a
/// neutral-tag column. The person's own words go to `serve_grant_event`
/// instead, through `rift_recall_ledger::grants`, and are never re-served.
///
/// The idempotency key stays the one the event minted, over the untagged
/// payload. Deliberately: the key's job is to make one invocation one row —
/// the CLI channel supplies a fresh id per invocation for exactly that — and a
/// fixed tag adds no identity for it to carry.
#[must_use]
pub fn grant_tagged_envelope(event: &MemdEvent, occurrence: &str) -> String {
    let envelope = event.to_hook_json_keyed(occurrence);
    let Ok(mut doc) = serde_json::from_str::<Value>(&envelope) else {
        return envelope;
    };
    if let Some(payload) = doc.get_mut("payload").and_then(Value::as_object_mut) {
        payload.insert(
            "reason".to_owned(),
            Value::String(CROSS_BRANCH_GRANT_TAG.to_owned()),
        );
    }
    serde_json::to_string(&doc).unwrap_or(envelope)
}

/// A stable, neutral tag for an omit reason (never leaks content). Public
/// because it is the audit/CLI vocabulary for a refusal: every transport
/// reports the same tag for the same decision.
#[must_use]
pub fn omit_tag(reason: OmitReason) -> &'static str {
    match reason {
        OmitReason::BudgetExhausted => "budget_exhausted",
        OmitReason::PolicyDeniedExact => "policy_denied_exact",
        OmitReason::SensitivePathRequiresReason => "sensitive_path_requires_reason",
        OmitReason::SummaryInvalidSourceChanged => "summary_invalid_source_changed",
        OmitReason::SourceDeletedOrRedacted => "source_deleted_or_redacted",
        OmitReason::NotRequested => "not_requested",
    }
}

#[cfg(test)]
mod tests;
