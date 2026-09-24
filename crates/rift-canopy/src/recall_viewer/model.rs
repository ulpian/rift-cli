//! The display-safe snapshot types the read-only data worker hands the viewer.

/// Factual timestamp provenance shown beside each checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerTimestampProvenance {
    /// Stored in checkpoint metadata.
    Recorded,
    /// Derived from the checkpoint's projected sessions.
    SessionDerived,
    /// Derived only from the anchor commit.
    AnchorCommit,
    /// No date is available.
    Unknown,
}

impl ViewerTimestampProvenance {
    /// Compact visible token.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Recorded => "recorded",
            Self::SessionDerived => "session",
            Self::AnchorCommit => "commit",
            Self::Unknown => "unknown",
        }
    }
}

/// One actual Git storage origin for a catalogue row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerOrigin {
    /// Fully qualified storage ref.
    pub storage_ref: String,
    /// Durable dialect label (`rift` or frozen legacy display label).
    pub dialect: String,
    /// `branch` or `per-checkpoint-ref`.
    pub kind: String,
    /// Carrying ref tip, when readable.
    pub tip_oid: Option<String>,
}

/// One checkpoint catalogue row, already sanitized and bounded by the worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerCheckpointRow {
    /// Full checkpoint id.
    pub checkpoint_id: String,
    /// Effective factual epoch timestamp. `None` is rendered as unknown; the
    /// projection's `-1` sentinel never crosses this type.
    pub effective_at_ms: Option<i64>,
    /// Why that timestamp is factual.
    pub timestamp_provenance: ViewerTimestampProvenance,
    /// Compact ISO UTC string, preformatted off-thread.
    pub compact_utc: Option<String>,
    /// Last non-empty user/assistant text in the checkpoint-new slice.
    pub excerpt: Option<String>,
    /// Excerpt enrichment is still pending.
    pub excerpt_loading: bool,
    /// Anchor commit oid, when known.
    pub anchor_commit: Option<String>,
    /// Every physical storage origin.
    pub origins: Vec<ViewerOrigin>,
    /// Canonical-chain position, or `None` for an unchained shard.
    pub chain_position: Option<u64>,
    /// Local branches containing the anchor, already batch-derived.
    pub local_branches: Vec<String>,
    /// Capture/checkpoint strategy.
    pub strategy: Option<String>,
    /// The worktree the shard was ARCHIVED in, from `checkpoint_worktree`
    /// (worktree provenance, first-archiver semantics).
    ///
    /// `None` is **unknown**, and every shard written before provenance
    /// labelling is `None`: a shard
    /// without the `metadata.json` member projects no row, and sealed shards
    /// forbids back-filling one. The badge prints `unknown` for it — never
    /// `main`, never blank (worktree provenance).
    pub worktree: Option<String>,
    /// Projection posture (`projected`, `unprojected`, `partial`, ...).
    pub projection: String,
    /// Safe issue badges; corrupt rows remain selectable.
    pub issues: Vec<String>,
    /// Lowercase metadata search haystack. It is never rendered.
    pub metadata_search: String,
}

/// Canopy-owned syntax classes. Syntect remains fully in the data plane.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyntaxClass {
    /// Plain/default source text.
    #[default]
    Plain,
    /// Language keyword/control token.
    Keyword,
    /// String or character literal.
    String,
    /// Comment/doc text.
    Comment,
    /// Number/constant.
    Number,
    /// Type/module/entity name.
    Type,
    /// Function/method name.
    Function,
    /// Operator/punctuation.
    Operator,
}

/// One highlighted span in a timeline line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerSpan {
    /// Sanitized display text.
    pub text: String,
    /// Semantic class selected off-thread.
    pub class: SyntaxClass,
}

/// Semantic row kind. Diff kinds own row backgrounds and override syntax
/// backgrounds; syntax contributes foreground only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewerLineKind {
    /// Ordinary timeline text.
    #[default]
    Text,
    /// Section/fact label.
    Heading,
    /// De-emphasized context/notice.
    Dim,
    /// Diff file header.
    DiffFile,
    /// Diff hunk header.
    DiffHunk,
    /// Added source line.
    Addition,
    /// Removed source line.
    Removal,
    /// Unchanged source context.
    Context,
    /// Explicit collapsed-lines gap.
    Gap,
    /// Warning/corrupt/unavailable fact.
    Warning,
}

/// One render-ready timeline line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewerLine {
    /// Old-side 1-based line number, if this is a diff row.
    pub old_line: Option<u32>,
    /// New-side 1-based line number, if this is a diff row.
    pub new_line: Option<u32>,
    /// Semantic row class.
    pub kind: ViewerLineKind,
    /// Highlighted text spans. Empty means a blank row.
    pub spans: Vec<ViewerSpan>,
}

impl ViewerLine {
    /// Plain one-span line.
    #[must_use]
    pub fn plain(kind: ViewerLineKind, text: impl Into<String>) -> Self {
        Self {
            old_line: None,
            new_line: None,
            kind,
            spans: vec![ViewerSpan {
                text: text.into(),
                class: SyntaxClass::Plain,
            }],
        }
    }
}

/// One collapsible/expandable item inside a timeline segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerTimelineItem {
    /// Stable session-local expansion key.
    pub id: String,
    /// Item headline.
    pub title: String,
    /// Optional real timestamp, preformatted as compact UTC.
    pub timestamp: Option<String>,
    /// Always-visible factual summary.
    pub summary: Vec<ViewerLine>,
    /// Default body (compact transcript slice, three-line hunk context, ...).
    pub body: Vec<ViewerLine>,
    /// Expanded body, when more data is safely available.
    pub expanded_body: Option<Vec<ViewerLine>>,
    /// Body starts collapsed (nested tasks/shadow context).
    pub collapsed: bool,
    /// This transcript record precedes `compact_transcript_start` and is shown
    /// only in Full session mode.
    pub full_session_only: bool,
}

/// A chronology-safe timeline segment. Ordering exists only inside each
/// segment where `ordered_by_time` says a real timestamp supports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerTimelineSegment {
    /// Segment title.
    pub title: String,
    /// Boundary statement explaining the segment's ordering semantics.
    pub boundary: Option<String>,
    /// Whether items have been sorted by their own real timestamps.
    pub ordered_by_time: bool,
    /// Segment items.
    pub items: Vec<ViewerTimelineItem>,
}

/// Selected-checkpoint detail assembled wholly on the data worker.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewerDetail {
    /// Selected checkpoint id.
    pub checkpoint_id: String,
    /// Segmented timeline. Segment order is product order, not invented time.
    pub segments: Vec<ViewerTimelineSegment>,
    /// Safe top-level notice for missing/corrupt detail.
    pub notice: Option<String>,
}

/// Progressive transcript-filter scan status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerFilterProgress {
    /// Shards scanned.
    pub scanned: usize,
    /// Total shards to scan.
    pub total: usize,
    /// The worker reached the current generation's end.
    pub complete: bool,
}
