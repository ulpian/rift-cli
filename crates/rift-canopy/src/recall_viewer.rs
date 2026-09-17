//! Read-only Recall Viewer model, geometry, and terminal rendering.
//!
//! The data worker hands this module immutable, display-safe snapshots. The
//! reducer mutates only session-local navigation/filter/expansion state; there
//! is deliberately no persistence operation for any field here.

use std::collections::BTreeSet;
use std::sync::Arc;

use crate::layout::{choose_mode, single_panel, solve, two_col, LayoutMode, Rect};
use crate::render::frame::{Color, Frame, Mods, Size, Style};
use crate::state::ViewModel;

/// Top-level Canopy surface.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Surface {
    /// The normal Repository/Token dashboard.
    #[default]
    Dashboard,
    /// Read-only Projects/Checkpoints/Timeline inspection surface.
    RecallViewer,
}

/// Independent focus ring for the Recall Viewer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ViewerPane {
    /// Projects rail.
    #[default]
    Projects,
    /// Checkpoint catalogue.
    Checkpoints,
    /// Selected-checkpoint timeline.
    Timeline,
    /// Bottom command panel.
    Command,
}

impl ViewerPane {
    /// Cycle in the locked Projects → Checkpoints → Timeline → Command order.
    #[must_use]
    pub fn cycle(self, forward: bool) -> Self {
        let order = [
            Self::Projects,
            Self::Checkpoints,
            Self::Timeline,
            Self::Command,
        ];
        let index = order.iter().position(|pane| *pane == self).unwrap_or(0);
        let next = if forward {
            (index + 1) % order.len()
        } else {
            (index + order.len() - 1) % order.len()
        };
        order.get(next).copied().unwrap_or_default()
    }
}

/// Worker-request generation. A response applies only when both project and
/// generation still match the viewer state.
pub type ViewerGeneration = u64;

/// A direct launch/deep-link request, resolved by the read-only data worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ViewerLaunch {
    /// Repository path selected by CLI resolution.
    pub repo: String,
    /// Optional checkpoint id/prefix or Git commit-ish.
    pub target: Option<String>,
    /// The repository is a session-only row and must never be registered.
    pub transient: bool,
}

/// Loading posture for a viewer section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ViewerLoadState {
    /// No request has been made.
    #[default]
    Idle,
    /// A request is in flight.
    Loading,
    /// Some independently valid facts arrived; more enrichment is in flight.
    Partial,
    /// The requested data is complete for the current generation.
    Ready,
    /// A safe category/detail. Existing partial facts remain rendered.
    Error(String),
}

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

/// Session-local viewer state. No field is persisted.
#[derive(Clone, Debug)]
pub struct RecallViewerState {
    /// Dashboard focus restored when the viewer closes.
    pub dashboard_focus: crate::Panel,
    /// Focused pane.
    pub pane: ViewerPane,
    /// Content pane restored when command editing closes.
    pub pane_before_command: ViewerPane,
    /// Active repository/project key.
    pub project: Option<String>,
    /// Current catalogue request generation.
    pub catalogue_generation: ViewerGeneration,
    /// Current detail request generation.
    pub detail_generation: ViewerGeneration,
    /// Checkpoint id the current detail generation targets.
    pub detail_target: Option<String>,
    /// Current filter request generation.
    pub filter_generation: ViewerGeneration,
    /// Catalogue loading posture.
    pub catalogue_state: ViewerLoadState,
    /// Detail loading posture.
    pub detail_state: ViewerLoadState,
    /// Full newest-first catalogue.
    pub catalogue: Arc<Vec<ViewerCheckpointRow>>,
    /// Visible indices into [`Self::catalogue`], kept in catalogue order.
    pub visible: Vec<usize>,
    /// Selected visible row index.
    pub selected: usize,
    /// Catalogue viewport top, in rows (not terminal lines).
    pub checkpoint_scroll: usize,
    /// Timeline viewport top, in flattened terminal lines.
    pub timeline_scroll: usize,
    /// Timeline keyboard cursor, in flattened terminal lines.
    pub timeline_cursor: usize,
    /// Selected detail snapshot.
    pub detail: Arc<ViewerDetail>,
    /// Filter text.
    pub filter: String,
    /// `f` placed the filter chip into editing mode.
    pub filter_editing: bool,
    /// Progressive transcript scan status.
    pub filter_progress: Option<ViewerFilterProgress>,
    /// Transcript matches delivered progressively by the worker.
    transcript_matches: BTreeSet<String>,
    /// Checkpoint selected when this filter-editing session began. It remains
    /// provisionally visible while unloaded transcript excerpts are searched,
    /// then either stays selected on a match or yields to the newest match.
    filter_selection_anchor: Option<String>,
    /// Show the whole self-contained compact transcript.
    pub full_session: bool,
    /// Expanded task/shadow/hunk item ids.
    pub expanded: BTreeSet<String>,
    /// New rows seen on refresh without moving the reader.
    pub new_count: usize,
    /// Non-blocking target/refresh/read notice.
    pub notice: Option<String>,
}

impl Default for RecallViewerState {
    fn default() -> Self {
        Self {
            dashboard_focus: crate::Panel::Projects,
            pane: ViewerPane::Projects,
            pane_before_command: ViewerPane::Checkpoints,
            project: None,
            catalogue_generation: 0,
            detail_generation: 0,
            detail_target: None,
            filter_generation: 0,
            catalogue_state: ViewerLoadState::Idle,
            detail_state: ViewerLoadState::Idle,
            catalogue: Arc::new(Vec::new()),
            visible: Vec::new(),
            selected: 0,
            checkpoint_scroll: 0,
            timeline_scroll: 0,
            timeline_cursor: 0,
            detail: Arc::new(ViewerDetail::default()),
            filter: String::new(),
            filter_editing: false,
            filter_progress: None,
            transcript_matches: BTreeSet::new(),
            filter_selection_anchor: None,
            full_session: false,
            expanded: BTreeSet::new(),
            new_count: 0,
            notice: None,
        }
    }
}

impl RecallViewerState {
    /// Selected row in the filtered catalogue.
    #[must_use]
    pub fn selected_row(&self) -> Option<&ViewerCheckpointRow> {
        self.visible
            .get(self.selected)
            .and_then(|index| self.catalogue.get(*index))
    }

    /// Start a new project-scoped catalogue request and clear only data that
    /// cannot belong to the new project.
    pub fn start_project(&mut self, project: String) -> ViewerGeneration {
        self.project = Some(project);
        self.catalogue_generation = self.catalogue_generation.wrapping_add(1).max(1);
        self.detail_generation = self.detail_generation.wrapping_add(1).max(1);
        self.detail_target = None;
        self.filter_generation = self.filter_generation.wrapping_add(1).max(1);
        self.catalogue_state = ViewerLoadState::Loading;
        self.detail_state = ViewerLoadState::Idle;
        self.catalogue = Arc::new(Vec::new());
        self.visible.clear();
        self.selected = 0;
        self.checkpoint_scroll = 0;
        self.timeline_scroll = 0;
        self.timeline_cursor = 0;
        self.detail = Arc::new(ViewerDetail::default());
        self.filter.clear();
        self.filter_editing = false;
        self.filter_progress = None;
        self.transcript_matches.clear();
        self.filter_selection_anchor = None;
        self.full_session = false;
        self.expanded.clear();
        self.new_count = 0;
        self.notice = None;
        self.catalogue_generation
    }

    /// Replace catalogue metadata for the current generation. Selection is
    /// kept by checkpoint id; a refresh that added newer rows records `N new`
    /// but never moves a reader away from their checkpoint.
    pub fn apply_catalogue(
        &mut self,
        rows: Vec<ViewerCheckpointRow>,
        selected_hint: Option<&str>,
        notice: Option<String>,
        complete: bool,
    ) {
        let previous_id = self.selected_row().map(|row| row.checkpoint_id.clone());
        let previous_ids: BTreeSet<&str> = self
            .catalogue
            .iter()
            .map(|row| row.checkpoint_id.as_str())
            .collect();
        let newly_discovered = if previous_ids.is_empty() {
            0
        } else {
            rows.iter()
                .filter(|row| !previous_ids.contains(row.checkpoint_id.as_str()))
                .count()
        };
        self.new_count = self.new_count.saturating_add(newly_discovered);
        self.catalogue = Arc::new(rows);
        self.recompute_visible();
        let desired = previous_id.as_deref().or(selected_hint);
        self.selected = desired
            .and_then(|id| {
                self.visible.iter().position(|index| {
                    self.catalogue
                        .get(*index)
                        .is_some_and(|row| row.checkpoint_id == id)
                })
            })
            .unwrap_or(0)
            .min(self.visible.len().saturating_sub(1));
        self.catalogue_state = if complete {
            ViewerLoadState::Ready
        } else {
            ViewerLoadState::Partial
        };
        if notice.is_some() {
            self.notice = notice;
        }
    }

    /// Move selection and return whether it changed.
    pub fn move_checkpoint(&mut self, down: bool, page: bool) -> bool {
        if self.visible.is_empty() {
            return false;
        }
        let old = self.selected;
        let step = if page { 8 } else { 1 };
        self.selected = if down {
            self.selected
                .saturating_add(step)
                .min(self.visible.len().saturating_sub(1))
        } else {
            self.selected.saturating_sub(step)
        };
        old != self.selected
    }

    /// Jump to first/last visible checkpoint.
    pub fn checkpoint_edge(&mut self, last: bool) -> bool {
        let old = self.selected;
        self.selected = if last {
            self.visible.len().saturating_sub(1)
        } else {
            0
        };
        old != self.selected
    }

    /// Select the latest visible checkpoint and clear the refresh notice.
    pub fn select_latest(&mut self) -> bool {
        let changed = self.selected != 0 && !self.visible.is_empty();
        self.selected = 0;
        self.checkpoint_scroll = 0;
        self.new_count = 0;
        changed
    }

    /// Recompute immediate metadata matches. Transcript matches are unioned as
    /// progressive worker responses arrive.
    pub fn recompute_visible(&mut self) {
        let query = self.filter.to_lowercase();
        let scanning = self
            .filter_progress
            .as_ref()
            .is_some_and(|progress| !progress.complete);
        self.visible = self
            .catalogue
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                query.is_empty()
                    || row.metadata_search.contains(&query)
                    || self.transcript_matches.contains(&row.checkpoint_id)
                    || scanning
                        && self.filter_selection_anchor.as_deref()
                            == Some(row.checkpoint_id.as_str())
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = self.selected.min(self.visible.len().saturating_sub(1));
    }

    /// Enter filter editing and remember the reader's checkpoint before any
    /// immediate metadata filtering can move the visible cursor.
    pub fn begin_filter_editing(&mut self) {
        self.filter_selection_anchor = self.selected_row().map(|row| row.checkpoint_id.clone());
        self.filter_editing = true;
    }

    /// Begin/cancel a progressive transcript scan after filter text changes.
    pub fn changed_filter(&mut self) -> ViewerGeneration {
        let selected_id = self.selected_row().map(|row| row.checkpoint_id.clone());
        if self.filter_selection_anchor.is_none() {
            self.filter_selection_anchor.clone_from(&selected_id);
        }
        self.filter_generation = self.filter_generation.wrapping_add(1).max(1);
        self.transcript_matches.clear();
        self.filter_progress = (!self.filter.is_empty()).then_some(ViewerFilterProgress {
            scanned: 0,
            total: self.catalogue.len(),
            complete: false,
        });
        self.recompute_visible();
        self.selected = selected_id
            .as_deref()
            .and_then(|id| {
                self.visible.iter().position(|index| {
                    self.catalogue
                        .get(*index)
                        .is_some_and(|row| row.checkpoint_id == id)
                })
            })
            .unwrap_or(0);
        self.filter_generation
    }

    /// Merge one progressive filter-scan response.
    pub fn apply_filter_progress(
        &mut self,
        matching_ids: impl IntoIterator<Item = String>,
        progress: ViewerFilterProgress,
    ) {
        let selected_id = self.selected_row().map(|row| row.checkpoint_id.clone());
        self.transcript_matches.extend(matching_ids);
        self.filter_progress = Some(progress);
        self.recompute_visible();
        self.selected = selected_id
            .as_deref()
            .and_then(|id| {
                self.visible.iter().position(|index| {
                    self.catalogue
                        .get(*index)
                        .is_some_and(|row| row.checkpoint_id == id)
                })
            })
            .unwrap_or(0)
            .min(self.visible.len().saturating_sub(1));
    }

    /// Start a selected-detail request and return its generation.
    pub fn start_detail(&mut self, checkpoint_id: String) -> ViewerGeneration {
        self.detail_generation = self.detail_generation.wrapping_add(1).max(1);
        self.detail_target = Some(checkpoint_id);
        self.detail_state = ViewerLoadState::Loading;
        self.timeline_scroll = 0;
        self.timeline_cursor = 0;
        self.detail_generation
    }

    /// Apply selected detail for the current generation.
    pub fn apply_detail(&mut self, detail: ViewerDetail, complete: bool) {
        self.detail_target = Some(detail.checkpoint_id.clone());
        self.detail = Arc::new(detail);
        self.detail_state = if complete {
            ViewerLoadState::Ready
        } else {
            ViewerLoadState::Partial
        };
        self.timeline_scroll = 0;
        self.timeline_cursor = 0;
        self.expanded.clear();
        self.full_session = false;
    }

    /// Toggle one timeline item's expansion state.
    pub fn toggle_expanded(&mut self, id: &str) {
        if !self.expanded.remove(id) {
            self.expanded.insert(id.to_owned());
        }
    }
}

/// Requests that only the read-only viewer worker may execute.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerRequest {
    /// Load metadata-first catalogue and resolve an optional deep link.
    Catalogue {
        /// Project/repository path.
        project: String,
        /// Generation for stale-response rejection.
        generation: ViewerGeneration,
        /// Optional checkpoint/Git target.
        target: Option<String>,
        /// True for refresh; worker may compare/cache identities.
        refresh: bool,
    },
    /// Enrich visible rows with transcript excerpts.
    VisibleExcerpts {
        /// Project/repository path.
        project: String,
        /// Catalogue generation.
        generation: ViewerGeneration,
        /// Checkpoint ids in/near the viewport.
        checkpoint_ids: Vec<String>,
    },
    /// Load one selected checkpoint's timeline detail.
    Detail {
        /// Project/repository path.
        project: String,
        /// Detail generation.
        generation: ViewerGeneration,
        /// Checkpoint id.
        checkpoint_id: String,
    },
    /// Progressively search compact transcript text.
    FilterScan {
        /// Project/repository path.
        project: String,
        /// Filter generation (changing text cancels older scans).
        generation: ViewerGeneration,
        /// Case-insensitive query.
        query: String,
    },
}

/// Worker responses folded by the pure reducer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewerEvent {
    /// Catalogue metadata or visible-row enrichment.
    Catalogue {
        /// Project/repository path.
        project: String,
        /// Matching catalogue generation.
        generation: ViewerGeneration,
        /// Newest-first rows.
        rows: Vec<ViewerCheckpointRow>,
        /// Deep-link selection resolved off-thread.
        selected: Option<String>,
        /// Precise non-blocking target/read notice.
        notice: Option<String>,
        /// `false` while visible excerpt enrichment remains.
        complete: bool,
    },
    /// Selected-checkpoint detail.
    Detail {
        /// Project/repository path.
        project: String,
        /// Matching detail generation.
        generation: ViewerGeneration,
        /// Immutable detail.
        detail: ViewerDetail,
        /// `false` for a partial independently-valid snapshot.
        complete: bool,
    },
    /// One progressive transcript-filter scan batch.
    FilterProgress {
        /// Project/repository path.
        project: String,
        /// Matching filter generation.
        generation: ViewerGeneration,
        /// Newly matching checkpoint ids.
        matching_ids: Vec<String>,
        /// Progress counters.
        progress: ViewerFilterProgress,
    },
    /// Safe worker error for one request class.
    Error {
        /// Project/repository path.
        project: String,
        /// Request generation.
        generation: ViewerGeneration,
        /// Area affected.
        area: ViewerErrorArea,
        /// Safe error category/detail.
        message: String,
    },
}

/// Viewer request class for error routing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewerErrorArea {
    /// Catalogue/refresh.
    Catalogue,
    /// Selected detail.
    Detail,
    /// Progressive filter scan.
    Filter,
}

/// Shared renderer/hit-test geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewerGeometry {
    /// Projects panel.
    pub projects: Rect,
    /// Checkpoint panel.
    pub checkpoints: Rect,
    /// Timeline panel.
    pub timeline: Rect,
    /// Single-column tab strip.
    pub tabs: Rect,
    /// Hint row.
    pub hint: Rect,
    /// Command panel.
    pub command: Rect,
}

/// Shared timeline action/content geometry. Keeping this beside the viewer
/// layout ensures the visible chips and their mouse targets never drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewerTimelineControls {
    /// Toggle between the checkpoint-new compact slice and the full compact
    /// session artifact.
    pub full_session: Rect,
    /// Return to the dashboard surface.
    pub dashboard: Rect,
    /// Scrollable timeline content below the action row(s).
    pub content: Rect,
}

/// Resolve the two timeline action chips and remaining scrollable content.
#[must_use]
pub fn timeline_controls(timeline: Rect) -> ViewerTimelineControls {
    let inner = if timeline.w < 2 || timeline.h < 2 {
        timeline
    } else {
        Rect {
            x: timeline.x + 1,
            y: timeline.y + 1,
            w: timeline.w - 2,
            h: timeline.h - 2,
        }
    };
    let empty = Rect {
        x: inner.x,
        y: inner.y,
        w: 0,
        h: 0,
    };
    if inner.h == 0 || inner.w == 0 {
        return ViewerTimelineControls {
            full_session: empty,
            dashboard: empty,
            content: inner,
        };
    }
    let full_width = 16.min(inner.w);
    let dashboard_width = 13.min(inner.w);
    let stacked =
        inner.w < full_width.saturating_add(dashboard_width).saturating_add(1) && inner.h > 1;
    let full_session = Rect {
        x: inner.x,
        y: inner.y,
        w: full_width,
        h: 1,
    };
    let dashboard = Rect {
        x: if stacked {
            inner.x
        } else {
            inner.x + inner.w.saturating_sub(dashboard_width)
        },
        y: inner.y + u16::from(stacked),
        w: dashboard_width,
        h: 1,
    };
    let control_rows = 1 + u16::from(stacked);
    ViewerTimelineControls {
        full_session,
        dashboard,
        content: Rect {
            x: inner.x,
            y: inner.y.saturating_add(control_rows),
            w: inner.w,
            h: inner.h.saturating_sub(control_rows),
        },
    }
}

/// Resolve the locked responsive viewer layout. Full mode always uses the
/// default 20/40/40 solver, independent of Panel Editor state.
#[must_use]
pub fn geometry(size: Size, pane: ViewerPane) -> ViewerGeometry {
    let body = Rect {
        x: 0,
        y: 1,
        w: size.w,
        h: size.h.saturating_sub(1),
    };
    match choose_mode(size) {
        LayoutMode::Full3Col => {
            let rects = solve(body);
            ViewerGeometry {
                projects: rects.col1,
                checkpoints: rects.col2,
                timeline: rects.col3,
                tabs: Rect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0,
                },
                hint: rects.hint,
                command: rects.command,
            }
        }
        LayoutMode::TwoCol => {
            let rects = two_col(body);
            ViewerGeometry {
                projects: rects.col1,
                checkpoints: rects.col2,
                timeline: rects.col3,
                tabs: Rect {
                    x: 0,
                    y: 0,
                    w: 0,
                    h: 0,
                },
                hint: rects.hint,
                command: rects.command,
            }
        }
        LayoutMode::SinglePanel => {
            let (tabs, panel, hint, command) = single_panel(body);
            let empty = Rect {
                x: panel.x,
                y: panel.y,
                w: 0,
                h: 0,
            };
            ViewerGeometry {
                projects: if pane == ViewerPane::Projects {
                    panel
                } else {
                    empty
                },
                checkpoints: if pane == ViewerPane::Checkpoints {
                    panel
                } else {
                    empty
                },
                timeline: if pane == ViewerPane::Timeline {
                    panel
                } else {
                    empty
                },
                tabs,
                hint,
                command,
            }
        }
        LayoutMode::TooSmall => ViewerGeometry {
            projects: body,
            checkpoints: body,
            timeline: body,
            tabs: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            hint: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            command: Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
        },
    }
}

/// Catalogue rows are deliberately multi-line.
pub const CHECKPOINT_ROW_HEIGHT: u16 = 5;

/// Draw the Recall Viewer body. Header, overlays and startup sweep remain owned
/// by `view::draw`, exactly as for the dashboard.
pub(crate) fn draw(vm: &ViewModel, frame: &mut Frame, unicode: bool) {
    let geometry = geometry(vm.size, vm.viewer.pane);
    if geometry.tabs.h > 0 {
        draw_tabs(vm, frame, geometry.tabs);
    }
    if geometry.projects.w > 0 {
        crate::view::draw_projects_read_only(vm, frame, geometry.projects, unicode);
    }
    if geometry.checkpoints.w > 0 {
        draw_checkpoints(vm, frame, geometry.checkpoints);
    }
    if geometry.timeline.w > 0 {
        draw_timeline(vm, frame, geometry.timeline, unicode);
    }
    draw_hint(vm, frame, geometry.hint);
    crate::view::draw_command(vm, frame, geometry.command);
}

fn draw_tabs(vm: &ViewModel, frame: &mut Frame, rect: Rect) {
    let mut text = String::new();
    for (pane, label) in [
        (ViewerPane::Projects, "Projects"),
        (ViewerPane::Checkpoints, "Checkpoints"),
        (ViewerPane::Timeline, "Timeline"),
    ] {
        if vm.viewer.pane == pane {
            text.push('[');
            text.push_str(label);
            text.push(']');
        } else {
            text.push(' ');
            text.push_str(label);
            text.push(' ');
        }
    }
    put_clipped(frame, rect.x, rect.y, rect.w, &text, vm.theme.key_hint());
}

#[allow(clippy::too_many_lines)] // one linear draw pass over catalogue header and visible rows
fn draw_checkpoints(vm: &ViewModel, frame: &mut Frame, rect: Rect) {
    let focused = vm.viewer.pane == ViewerPane::Checkpoints;
    let title = format!("checkpoints · {}", vm.viewer.visible.len());
    let inner = crate::chrome::panel(
        frame,
        rect,
        &title,
        vm.theme.border(crate::theme::Section::Repository, focused),
        vm.theme.title(crate::theme::Section::Repository),
    );
    if inner.h == 0 {
        return;
    }
    let filter = if vm.viewer.filter_editing {
        format!("[ Filter: {}▏ ]", vm.viewer.filter)
    } else if vm.viewer.filter.is_empty() {
        "[ Filter ]".to_owned()
    } else {
        format!("[ Filter: {} ]", vm.viewer.filter)
    };
    put_clipped(
        frame,
        inner.x,
        inner.y,
        inner.w,
        &filter,
        if vm.viewer.filter_editing {
            vm.theme.selection()
        } else {
            vm.theme.button(false)
        },
    );
    let mut header_rows = 1u16;
    if let Some(progress) = &vm.viewer.filter_progress {
        let text = if progress.complete {
            format!("search complete · {}/{}", progress.scanned, progress.total)
        } else {
            format!(
                "searching transcripts · {}/{}",
                progress.scanned, progress.total
            )
        };
        if inner.h > 1 {
            put_clipped(frame, inner.x, inner.y + 1, inner.w, &text, vm.theme.dim());
            header_rows = 2;
        }
    } else if vm.viewer.new_count > 0 && inner.h > 1 {
        let text = format!("{} new · [ Latest ]", vm.viewer.new_count);
        put_clipped(
            frame,
            inner.x,
            inner.y + 1,
            inner.w,
            &text,
            vm.theme.key_hint(),
        );
        header_rows = 2;
    }
    let list = Rect {
        x: inner.x,
        y: inner.y.saturating_add(header_rows),
        w: inner.w,
        h: inner.h.saturating_sub(header_rows),
    };
    if vm.viewer.visible.is_empty() {
        let text = match &vm.viewer.catalogue_state {
            ViewerLoadState::Loading => "loading checkpoint catalogue …",
            ViewerLoadState::Error(_) => "catalogue unavailable",
            _ if vm.viewer.filter.is_empty() => "no durable checkpoints",
            _ => "no checkpoint matches yet",
        };
        put_clipped(frame, list.x, list.y, list.w, text, vm.theme.dim());
        if let ViewerLoadState::Error(error) = &vm.viewer.catalogue_state {
            if list.h > 1 {
                put_clipped(frame, list.x, list.y + 1, list.w, error, warning_style(vm));
            }
        }
        return;
    }
    let capacity = usize::from(list.h / CHECKPOINT_ROW_HEIGHT).max(1);
    let selected = vm
        .viewer
        .selected
        .min(vm.viewer.visible.len().saturating_sub(1));
    let start = vm
        .viewer
        .checkpoint_scroll
        .min(vm.viewer.visible.len().saturating_sub(capacity));
    let end = (start + capacity).min(vm.viewer.visible.len());
    for visible_index in start..end {
        let Some(catalogue_index) = vm.viewer.visible.get(visible_index) else {
            continue;
        };
        let Some(row) = vm.viewer.catalogue.get(*catalogue_index) else {
            continue;
        };
        let Ok(offset) = u16::try_from(visible_index - start) else {
            break;
        };
        let y = list.y + offset.saturating_mul(CHECKPOINT_ROW_HEIGHT);
        draw_checkpoint_row(
            vm,
            frame,
            Rect {
                x: list.x,
                y,
                w: list.w,
                h: CHECKPOINT_ROW_HEIGHT.min(list.y.saturating_add(list.h).saturating_sub(y)),
            },
            row,
            visible_index == selected,
        );
    }
}

/// A catalogue row's badge strip: capture strategy, projection posture, the
/// parse-issue count when the row has any, and the worktree the shard was
/// **archived in** (worktree provenance).
///
/// `wt:unknown` is a fact, not a placeholder (worktree provenance): an absent
/// `checkpoint_worktree` row means the shard carries no `worktree` member, as
/// every shard written before provenance labelling does, and sealed shards are
/// never rewritten, so one can never be back-filled. Printing `main` there — or
/// nothing — would turn "we do
/// not know" into a claim about where the archive ran.
///
/// The worktree goes **last** because this strip is clipped to the pane's
/// width and the row that loses its tail should lose provenance before it
/// loses a warning: `!parse:n` says the row is corrupt, and a badge must
/// never be what pushes that off the screen.
fn checkpoint_badges(row: &ViewerCheckpointRow) -> String {
    let strategy = row.strategy.as_deref().unwrap_or("strategy ?");
    let worktree = row.worktree.as_deref().unwrap_or("unknown");
    let mut strip = format!("{strategy} · {}", row.projection);
    if !row.issues.is_empty() {
        strip.push_str(&format!(" · !parse:{}", row.issues.len()));
    }
    strip.push_str(&format!(" · wt:{worktree}"));
    strip
}

#[allow(clippy::too_many_lines)] // the five-line row is kept visually atomic
fn draw_checkpoint_row(
    vm: &ViewModel,
    frame: &mut Frame,
    rect: Rect,
    row: &ViewerCheckpointRow,
    selected: bool,
) {
    let style = if selected {
        vm.theme.selection()
    } else {
        vm.theme.body()
    };
    if selected {
        crate::chrome::fill(frame, rect, style);
    }
    let marker = if selected { '>' } else { ' ' };
    let date = row.compact_utc.as_deref().unwrap_or("date unknown");
    put_clipped(
        frame,
        rect.x,
        rect.y,
        rect.w,
        &format!("{marker} {date} · {}", row.timestamp_provenance.label()),
        style,
    );
    if rect.h > 1 {
        let excerpt = row.excerpt.as_deref().unwrap_or(if row.excerpt_loading {
            "… loading excerpt"
        } else {
            "(no user/assistant text)"
        });
        put_clipped(
            frame,
            rect.x + 2,
            rect.y + 1,
            rect.w.saturating_sub(2),
            excerpt,
            style,
        );
    }
    if rect.h > 2 {
        let anchor = row.anchor_commit.as_deref().map_or("—", short_id);
        put_clipped(
            frame,
            rect.x + 2,
            rect.y + 2,
            rect.w.saturating_sub(2),
            &format!("cp {} · commit {anchor}", short_id(&row.checkpoint_id)),
            vm.theme.dim().with_bg_if(selected, crate::theme::SELECT_BG),
        );
    }
    if rect.h > 3 {
        let origin = row
            .origins
            .first()
            .map_or("origin unavailable".to_owned(), |origin| {
                let more = row.origins.len().saturating_sub(1);
                format!(
                    "{}/{} {}{}",
                    origin.dialect,
                    if origin.kind == "branch" { "br" } else { "ref" },
                    origin.storage_ref,
                    if more > 0 {
                        format!(" +{more}")
                    } else {
                        String::new()
                    }
                )
            });
        let chain = row
            .chain_position
            .map_or("unchained".to_owned(), |position| {
                format!("chain #{position}")
            });
        put_clipped(
            frame,
            rect.x + 2,
            rect.y + 3,
            rect.w.saturating_sub(2),
            &format!("{origin} · {chain}"),
            vm.theme.dim(),
        );
    }
    if rect.h > 4 {
        let branch = row
            .local_branches
            .first()
            .map_or("no containing branch".to_owned(), |name| {
                let more = row.local_branches.len().saturating_sub(1);
                format!(
                    "{name}{}",
                    if more > 0 {
                        format!(" +{more}")
                    } else {
                        String::new()
                    }
                )
            });
        let badges = checkpoint_badges(row);
        put_clipped(
            frame,
            rect.x + 2,
            rect.y + 4,
            rect.w.saturating_sub(2),
            &format!("{branch} · {badges}"),
            if row.issues.is_empty() {
                vm.theme.dim()
            } else {
                warning_style(vm)
            },
        );
    }
}

#[allow(clippy::too_many_lines)] // one linear draw pass over controls, status, and flattened rows
fn draw_timeline(vm: &ViewModel, frame: &mut Frame, rect: Rect, unicode: bool) {
    let focused = vm.viewer.pane == ViewerPane::Timeline;
    let title = vm.viewer.selected_row().map_or_else(
        || "timeline".to_owned(),
        |row| format!("timeline · {}", short_id(&row.checkpoint_id)),
    );
    let inner = crate::chrome::panel(
        frame,
        rect,
        &title,
        vm.theme.border(crate::theme::Section::Recall, focused),
        vm.theme.title(crate::theme::Section::Recall),
    );
    if inner.h == 0 {
        return;
    }
    let controls = timeline_controls(rect);
    put_clipped(
        frame,
        controls.full_session.x,
        controls.full_session.y,
        controls.full_session.w,
        "[ Full session ]",
        vm.theme.button(vm.viewer.full_session),
    );
    put_clipped(
        frame,
        controls.dashboard.x,
        controls.dashboard.y,
        controls.dashboard.w,
        "[ Dashboard ]",
        vm.theme.button(false),
    );
    match &vm.viewer.detail_state {
        ViewerLoadState::Loading if vm.viewer.detail.segments.is_empty() => {
            put_clipped(
                frame,
                controls.content.x,
                controls.content.y,
                controls.content.w,
                "loading checkpoint detail …",
                vm.theme.dim(),
            );
            return;
        }
        ViewerLoadState::Error(error) if vm.viewer.detail.segments.is_empty() => {
            put_clipped(
                frame,
                controls.content.x,
                controls.content.y,
                controls.content.w,
                error,
                warning_style(vm),
            );
            return;
        }
        _ => {}
    }
    let lines = flatten_timeline(&vm.viewer);
    if lines.is_empty() {
        let notice = vm
            .viewer
            .detail
            .notice
            .as_deref()
            .unwrap_or("no selected checkpoint detail");
        put_clipped(
            frame,
            controls.content.x,
            controls.content.y,
            controls.content.w,
            notice,
            vm.theme.dim(),
        );
        return;
    }
    let start = vm.viewer.timeline_scroll.min(lines.len().saturating_sub(1));
    for (screen_row, line) in lines
        .iter()
        .skip(start)
        .take(usize::from(controls.content.h))
        .enumerate()
    {
        let Ok(dy) = u16::try_from(screen_row) else {
            break;
        };
        draw_timeline_line(
            vm,
            frame,
            Rect {
                x: controls.content.x,
                y: controls.content.y + dy,
                w: controls.content.w,
                h: 1,
            },
            line,
            unicode,
            start + screen_row == vm.viewer.timeline_cursor,
        );
    }
    if let Some(bar) = crate::chrome::scrollbar(controls.content, start, lines.len()) {
        crate::chrome::draw_scrollbar(frame, &bar, vm.theme, unicode);
    }
}

#[derive(Clone)]
struct FlatTimelineLine {
    line: ViewerLine,
    prefix: &'static str,
    item_id: Option<String>,
}

fn flatten_timeline(viewer: &RecallViewerState) -> Vec<FlatTimelineLine> {
    let mut out = Vec::new();
    for segment in &viewer.detail.segments {
        out.push(FlatTimelineLine {
            line: ViewerLine::plain(ViewerLineKind::Heading, format!("◆ {}", segment.title)),
            prefix: "",
            item_id: None,
        });
        if let Some(boundary) = &segment.boundary {
            out.push(FlatTimelineLine {
                line: ViewerLine::plain(ViewerLineKind::Dim, format!("  ┆ {boundary}")),
                prefix: "",
                item_id: None,
            });
        }
        for item in &segment.items {
            if item.full_session_only && !viewer.full_session {
                continue;
            }
            let expanded = !item.collapsed || viewer.expanded.contains(&item.id);
            let marker = if item.expanded_body.is_some() || item.collapsed {
                if expanded {
                    "▼"
                } else {
                    "▶"
                }
            } else {
                "•"
            };
            let timestamp = item
                .timestamp
                .as_deref()
                .map_or(String::new(), |time| format!(" · {time}"));
            out.push(FlatTimelineLine {
                line: ViewerLine::plain(
                    ViewerLineKind::Heading,
                    format!("  {marker} {}{timestamp}", item.title),
                ),
                prefix: "",
                item_id: Some(item.id.clone()),
            });
            for line in &item.summary {
                out.push(FlatTimelineLine {
                    line: line.clone(),
                    prefix: "    ",
                    item_id: Some(item.id.clone()),
                });
            }
            if expanded {
                let body = item
                    .expanded_body
                    .as_ref()
                    .filter(|_| viewer.expanded.contains(&item.id))
                    .unwrap_or(&item.body);
                for line in body {
                    out.push(FlatTimelineLine {
                        line: line.clone(),
                        prefix: "    ",
                        item_id: Some(item.id.clone()),
                    });
                }
            }
        }
    }
    out
}

/// Number of terminal lines in the currently-expanded timeline.
#[must_use]
pub fn timeline_line_count(viewer: &RecallViewerState) -> usize {
    flatten_timeline(viewer).len()
}

/// Expansion key under one flattened timeline line, if that line belongs to a
/// task/shadow/hunk item. Used by keyboard and mouse activation.
#[must_use]
pub fn timeline_item_at(viewer: &RecallViewerState, line: usize) -> Option<String> {
    flatten_timeline(viewer)
        .get(line)
        .and_then(|flat| flat.item_id.clone())
}

fn draw_timeline_line(
    vm: &ViewModel,
    frame: &mut Frame,
    rect: Rect,
    flat: &FlatTimelineLine,
    unicode: bool,
    selected: bool,
) {
    let row_style = line_style(vm, flat.line.kind, selected);
    if matches!(
        flat.line.kind,
        ViewerLineKind::Addition | ViewerLineKind::Removal
    ) || selected
    {
        crate::chrome::fill(frame, rect, row_style);
    }
    let marker = match flat.line.kind {
        ViewerLineKind::Addition => '+',
        ViewerLineKind::Removal => '-',
        ViewerLineKind::DiffHunk => '@',
        ViewerLineKind::Gap => {
            if unicode {
                '…'
            } else {
                '.'
            }
        }
        _ => ' ',
    };
    let mut x = rect.x;
    if matches!(
        flat.line.kind,
        ViewerLineKind::Addition
            | ViewerLineKind::Removal
            | ViewerLineKind::Context
            | ViewerLineKind::DiffHunk
            | ViewerLineKind::Gap
    ) {
        let old = flat
            .line
            .old_line
            .map_or("    ".to_owned(), |line| format!("{line:>4}"));
        let new = flat
            .line
            .new_line
            .map_or("    ".to_owned(), |line| format!("{line:>4}"));
        let gutter = format!("{old} {new} {marker} ");
        x = frame.put_str(
            x,
            rect.y,
            &gutter.chars().take(usize::from(rect.w)).collect::<String>(),
            row_style,
        );
    } else if !flat.prefix.is_empty() {
        x = frame.put_str(x, rect.y, flat.prefix, row_style);
    }
    let right = rect.x.saturating_add(rect.w);
    for span in &flat.line.spans {
        if x >= right {
            break;
        }
        let style = syntax_style(vm, span.class, flat.line.kind, selected);
        let text: String = cell_safe_text(&span.text)
            .chars()
            .take(usize::from(right - x))
            .collect();
        x = frame.put_str(x, rect.y, &text, style);
    }
    if flat.line.kind == ViewerLineKind::Gap && x.saturating_add(2) <= right {
        frame.put_str(x, rect.y, &format!(" {marker}"), row_style);
    }
}

fn line_style(vm: &ViewModel, kind: ViewerLineKind, selected: bool) -> Style {
    let mut style = match kind {
        ViewerLineKind::Heading | ViewerLineKind::DiffFile => vm.theme.key_hint(),
        ViewerLineKind::Dim | ViewerLineKind::Context | ViewerLineKind::Gap => vm.theme.dim(),
        ViewerLineKind::Warning => warning_style(vm),
        ViewerLineKind::Addition => vm.theme.body().with_bg(Color::Rgb(0x13, 0x34, 0x25)),
        ViewerLineKind::Removal => vm.theme.body().with_bg(Color::Rgb(0x3b, 0x1d, 0x22)),
        ViewerLineKind::DiffHunk => vm.theme.dim().with_bg(vm.theme.surface_bg()),
        ViewerLineKind::Text => vm.theme.body(),
    };
    if selected {
        style = style.with_bg(crate::theme::SELECT_BG).with_mods(Mods::BOLD);
    }
    style
}

fn syntax_style(
    vm: &ViewModel,
    class: SyntaxClass,
    line_kind: ViewerLineKind,
    selected: bool,
) -> Style {
    let fg = match class {
        SyntaxClass::Plain => vm.theme.body_fg(),
        SyntaxClass::Keyword | SyntaxClass::Function => vm.theme.button_accent(),
        SyntaxClass::String => vm.theme.graph_hi(),
        SyntaxClass::Comment => crate::theme::MUTED,
        SyntaxClass::Number => vm.theme.graph_calls(),
        SyntaxClass::Type => vm.theme.graph_accent(),
        SyntaxClass::Operator => vm.theme.border_idle(),
    };
    let mut style = Style::fg(fg);
    style = match line_kind {
        ViewerLineKind::Addition => style.with_bg(Color::Rgb(0x13, 0x34, 0x25)),
        ViewerLineKind::Removal => style.with_bg(Color::Rgb(0x3b, 0x1d, 0x22)),
        _ => style,
    };
    if selected {
        style = style.with_bg(crate::theme::SELECT_BG);
    }
    style
}

fn warning_style(vm: &ViewModel) -> Style {
    Style::fg(Color::Rgb(0xf0, 0xc4, 0x5c)).with_bg(vm.theme.surface_bg())
}

fn draw_hint(vm: &ViewModel, frame: &mut Frame, rect: Rect) {
    if rect.h == 0 {
        return;
    }
    let text = if vm.viewer.filter_editing {
        "type filter · Enter apply · Esc cancel"
    } else {
        "Tab pane · j/k move · PgUp/PgDn page · f Filter · F Full session · r Dashboard · / command"
    };
    put_clipped(frame, rect.x, rect.y, rect.w, text, vm.theme.dim());
}

fn short_id(value: &str) -> &str {
    value.get(..12.min(value.len())).unwrap_or(value)
}

fn put_clipped(frame: &mut Frame, x: u16, y: u16, width: u16, text: &str, style: Style) {
    if width == 0 {
        return;
    }
    let clipped: String = cell_safe_text(text)
        .chars()
        .take(usize::from(width))
        .collect();
    frame.put_str(x, y, &clipped, style);
}

/// Last-line defence at the terminal-cell boundary. Data-plane parsers already
/// neutralize ANSI/C0/C1 and bidi controls, but a render snapshot is a public
/// model and may also come from a test or future adapter. No control character
/// is ever installed into a terminal cell; tabs/newlines become one inert cell
/// so clipping geometry remains exact.
fn cell_safe_text(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\n' | '\r' | '\t' => ' ',
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => '\u{fffd}',
            character if character.is_control() => '\u{fffd}',
            character => character,
        })
        .collect()
}

trait StyleExt {
    fn with_bg_if(self, condition: bool, color: Color) -> Self;
}

impl StyleExt for Style {
    fn with_bg_if(self, condition: bool, color: Color) -> Self {
        if condition {
            self.with_bg(color)
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, search: &str) -> ViewerCheckpointRow {
        ViewerCheckpointRow {
            checkpoint_id: id.to_owned(),
            effective_at_ms: None,
            timestamp_provenance: ViewerTimestampProvenance::Unknown,
            compact_utc: None,
            excerpt: None,
            excerpt_loading: false,
            anchor_commit: None,
            origins: Vec::new(),
            chain_position: None,
            local_branches: Vec::new(),
            strategy: None,
            projection: "unprojected".into(),
            issues: Vec::new(),
            metadata_search: search.to_owned(),
            worktree: None,
        }
    }

    /// ★ worktree provenance, the catalogue's half: a checkpoint with no
    /// `checkpoint_worktree` row badges `unknown` — never `main`, never a
    /// blank — and a checkpoint that has one badges that member by name. This
    /// is the first reader of the relation.
    #[test]
    fn a_checkpoint_without_a_worktree_row_badges_unknown_never_main() {
        let mut r = row("c1", "");
        r.strategy = Some("native".to_owned());
        r.projection = "projected".into();

        assert_eq!(
            checkpoint_badges(&r),
            "native · projected · wt:unknown",
            "no projected row ⇒ unknown, spelled out"
        );
        assert!(
            checkpoint_badges(&r).ends_with("wt:unknown"),
            "provenance trails the strip, so a clip sheds it first"
        );
        assert!(
            !checkpoint_badges(&r).contains("main"),
            "absence never borrows the main work tree's name"
        );

        r.worktree = Some("swt-d".to_owned());
        assert_eq!(checkpoint_badges(&r), "native · projected · wt:swt-d");
        // The main work tree is a member like any other: it is named only
        // because the shard said so.
        r.worktree = Some("main".to_owned());
        assert_eq!(checkpoint_badges(&r), "native · projected · wt:main");

        // The parse-issue count still trails the strip, so a corrupt row
        // keeps its warning AND its provenance.
        r.worktree = None;
        r.issues = vec!["metadata: bad".to_owned()];
        assert_eq!(
            checkpoint_badges(&r),
            "native · projected · !parse:1 · wt:unknown",
            "the corruption warning outranks the badge when the row is clipped"
        );
    }

    #[test]
    fn viewer_focus_order_wraps_both_directions() {
        assert_eq!(ViewerPane::Projects.cycle(true), ViewerPane::Checkpoints);
        assert_eq!(ViewerPane::Command.cycle(true), ViewerPane::Projects);
        assert_eq!(ViewerPane::Projects.cycle(false), ViewerPane::Command);
    }

    #[test]
    fn terminal_cell_boundary_neutralizes_controls_and_bidi() {
        let safe = cell_safe_text("a\tb\n\u{1b}[31m\u{202e}x");
        assert_eq!(safe, "a b \u{fffd}[31m\u{fffd}x");
        assert!(!safe.chars().any(char::is_control));
    }

    #[test]
    fn filter_preserves_selection_when_it_still_matches() {
        let mut viewer = RecallViewerState::default();
        viewer.apply_catalogue(
            vec![row("new", "alpha"), row("old", "beta")],
            Some("old"),
            None,
            true,
        );
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("old")
        );
        viewer.begin_filter_editing();
        viewer.filter = "beta".into();
        viewer.changed_filter();
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("old")
        );
        viewer.filter = "alpha".into();
        viewer.changed_filter();
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("old"),
            "the reader stays put while its unloaded transcript is scanned"
        );
        viewer.apply_filter_progress(
            ["new".to_owned()],
            ViewerFilterProgress {
                scanned: 2,
                total: 2,
                complete: true,
            },
        );
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("new")
        );
    }

    #[test]
    fn filter_restores_the_anchor_when_its_transcript_matches() {
        let mut viewer = RecallViewerState::default();
        viewer.apply_catalogue(
            vec![row("new", "alpha"), row("old", "beta")],
            Some("old"),
            None,
            true,
        );
        viewer.begin_filter_editing();
        viewer.filter = "needle only in transcript".into();
        viewer.changed_filter();
        viewer.apply_filter_progress(
            ["old".to_owned()],
            ViewerFilterProgress {
                scanned: 2,
                total: 2,
                complete: true,
            },
        );
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("old")
        );
    }

    #[test]
    fn refresh_preserves_reader_and_counts_new_prefix() {
        let mut viewer = RecallViewerState::default();
        viewer.apply_catalogue(vec![row("b", ""), row("a", "")], Some("a"), None, true);
        viewer.apply_catalogue(
            vec![row("d", ""), row("c", ""), row("b", ""), row("a", "")],
            None,
            None,
            true,
        );
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("a")
        );
        assert_eq!(viewer.new_count, 2);
        viewer.select_latest();
        assert_eq!(
            viewer.selected_row().map(|row| row.checkpoint_id.as_str()),
            Some("d")
        );
    }

    #[test]
    fn full_layout_is_fixed_twenty_forty_forty() {
        let geometry = geometry(Size::new(120, 40), ViewerPane::Timeline);
        assert_eq!(
            (
                geometry.projects.w,
                geometry.checkpoints.w,
                geometry.timeline.w
            ),
            (24, 48, 48)
        );
    }

    #[test]
    fn two_column_layout_stacks_catalogue_and_timeline() {
        // One column below the 20+28+28 full-grid floor selects the degraded
        // two-column solver; 76 itself is intentionally Full3Col.
        let geometry = geometry(Size::new(75, 30), ViewerPane::Projects);
        assert_eq!(
            geometry.projects.h,
            geometry.checkpoints.h + geometry.timeline.h
        );
        assert_eq!(geometry.checkpoints.x, geometry.timeline.x);
    }

    #[test]
    fn timeline_action_chips_never_overlap_the_scrollable_body() {
        let wide = timeline_controls(Rect {
            x: 10,
            y: 2,
            w: 50,
            h: 20,
        });
        assert!(wide.full_session.x + wide.full_session.w <= wide.dashboard.x);
        assert!(wide.full_session.y < wide.content.y);
        assert!(wide.dashboard.y < wide.content.y);

        let narrow = timeline_controls(Rect {
            x: 0,
            y: 0,
            w: 24,
            h: 12,
        });
        assert!(narrow.full_session.y < narrow.dashboard.y);
        assert!(narrow.dashboard.y < narrow.content.y);
    }

    #[test]
    fn full_session_toggle_reveals_only_boundary_preceding_items() {
        let mut viewer = RecallViewerState {
            detail: Arc::new(ViewerDetail {
                checkpoint_id: "cp".to_owned(),
                segments: vec![ViewerTimelineSegment {
                    title: "Transcript".to_owned(),
                    boundary: None,
                    ordered_by_time: false,
                    items: vec![
                        ViewerTimelineItem {
                            id: "before".to_owned(),
                            title: "before".to_owned(),
                            timestamp: None,
                            summary: Vec::new(),
                            body: Vec::new(),
                            expanded_body: None,
                            collapsed: false,
                            full_session_only: true,
                        },
                        ViewerTimelineItem {
                            id: "slice".to_owned(),
                            title: "slice".to_owned(),
                            timestamp: None,
                            summary: Vec::new(),
                            body: Vec::new(),
                            expanded_body: None,
                            collapsed: false,
                            full_session_only: false,
                        },
                    ],
                }],
                notice: None,
            }),
            ..RecallViewerState::default()
        };
        assert!(flatten_timeline(&viewer)
            .iter()
            .all(|line| { line.item_id.as_deref() != Some("before") }));
        viewer.full_session = true;
        assert!(flatten_timeline(&viewer)
            .iter()
            .any(|line| { line.item_id.as_deref() == Some("before") }));
    }

    fn render_state(
        mut viewer: RecallViewerState,
        size: Size,
        pane: ViewerPane,
        unicode: bool,
    ) -> Frame {
        let project = crate::Project {
            name: "repo".to_owned(),
            path: "/repo".to_owned(),
            status: crate::ProjectStatus::Idle,
            kind: crate::EntryKind::Repo,
        };
        let mut state = crate::state::AppState::new(vec![project], size);
        state.app.splash.skip();
        state.app.overlay = None;
        state.app.surface = Surface::RecallViewer;
        viewer.pane = pane;
        state.app.viewer = viewer;
        let view = state.snapshot();
        let mut frame = Frame::new(size);
        draw(&view, &mut frame, unicode);
        assert_eq!(
            frame.cells().len(),
            usize::from(size.w) * usize::from(size.h)
        );
        assert!(frame.cells().iter().all(|cell| !cell.ch.is_control()));
        frame
    }

    #[allow(clippy::too_many_lines)]
    fn rich_viewer() -> RecallViewerState {
        let mut complete = row("checkpoint-complete", "complete main");
        complete.effective_at_ms = Some(1_787_913_600_000);
        complete.timestamp_provenance = ViewerTimestampProvenance::Recorded;
        complete.compact_utc = Some("2026-08-28 12:00Z".to_owned());
        complete.excerpt = Some("implemented the read-only viewer".to_owned());
        complete.anchor_commit = Some("0123456789abcdef".to_owned());
        complete.origins = vec![
            ViewerOrigin {
                storage_ref: "refs/heads/rift/checkpoints/v1".to_owned(),
                dialect: "rift".to_owned(),
                kind: "branch".to_owned(),
                tip_oid: Some("tip-a".to_owned()),
            },
            ViewerOrigin {
                storage_ref: "refs/checkpoints/legacy".to_owned(),
                dialect: "legacy".to_owned(),
                kind: "per-checkpoint-ref".to_owned(),
                tip_oid: None,
            },
        ];
        complete.chain_position = Some(42);
        complete.local_branches = vec!["main".to_owned(), "release".to_owned()];
        complete.strategy = Some("native".to_owned());
        complete.projection = "projected".to_owned();

        let mut loading = row("checkpoint-loading", "loading");
        loading.timestamp_provenance = ViewerTimestampProvenance::SessionDerived;
        loading.excerpt_loading = true;

        let mut commit_time = row("checkpoint-commit", "commit");
        commit_time.timestamp_provenance = ViewerTimestampProvenance::AnchorCommit;
        commit_time.origins = vec![ViewerOrigin {
            storage_ref: "refs/checkpoints/one".to_owned(),
            dialect: "legacy".to_owned(),
            kind: "per-checkpoint-ref".to_owned(),
            tip_oid: None,
        }];
        commit_time.issues = vec!["metadata".to_owned()];

        let unknown = row("checkpoint-unknown", "unknown");
        let catalogue = vec![complete, loading, commit_time, unknown];

        let syntax_spans = [
            SyntaxClass::Plain,
            SyntaxClass::Keyword,
            SyntaxClass::String,
            SyntaxClass::Comment,
            SyntaxClass::Number,
            SyntaxClass::Type,
            SyntaxClass::Function,
            SyntaxClass::Operator,
        ]
        .into_iter()
        .map(|class| ViewerSpan {
            text: " token ".to_owned(),
            class,
        })
        .collect::<Vec<_>>();
        let line_kinds = [
            ViewerLineKind::Text,
            ViewerLineKind::Heading,
            ViewerLineKind::Dim,
            ViewerLineKind::DiffFile,
            ViewerLineKind::DiffHunk,
            ViewerLineKind::Addition,
            ViewerLineKind::Removal,
            ViewerLineKind::Context,
            ViewerLineKind::Gap,
            ViewerLineKind::Warning,
        ];
        let lines = line_kinds
            .into_iter()
            .map(|kind| ViewerLine {
                old_line: Some(7),
                new_line: Some(9),
                kind,
                spans: syntax_spans.clone(),
            })
            .collect::<Vec<_>>();
        let detail = ViewerDetail {
            checkpoint_id: "checkpoint-complete".to_owned(),
            segments: vec![
                ViewerTimelineSegment {
                    title: "Recorded".to_owned(),
                    boundary: Some("ordered by factual time".to_owned()),
                    ordered_by_time: true,
                    items: vec![
                        ViewerTimelineItem {
                            id: "collapsed".to_owned(),
                            title: "collapsible diff".to_owned(),
                            timestamp: Some("12:00:00Z".to_owned()),
                            summary: lines.clone(),
                            body: vec![ViewerLine::plain(ViewerLineKind::Text, "compact body")],
                            expanded_body: Some(lines.clone()),
                            collapsed: true,
                            full_session_only: false,
                        },
                        ViewerTimelineItem {
                            id: "ordinary".to_owned(),
                            title: "ordinary event".to_owned(),
                            timestamp: None,
                            summary: Vec::new(),
                            body: lines,
                            expanded_body: None,
                            collapsed: false,
                            full_session_only: false,
                        },
                    ],
                },
                ViewerTimelineSegment {
                    title: "Earlier compact session".to_owned(),
                    boundary: None,
                    ordered_by_time: false,
                    items: vec![ViewerTimelineItem {
                        id: "full-session".to_owned(),
                        title: "earlier turn".to_owned(),
                        timestamp: None,
                        summary: Vec::new(),
                        body: vec![ViewerLine::plain(ViewerLineKind::Text, "earlier body")],
                        expanded_body: None,
                        collapsed: false,
                        full_session_only: true,
                    }],
                },
            ],
            notice: None,
        };

        let mut viewer = RecallViewerState {
            project: Some("/repo".to_owned()),
            catalogue_state: ViewerLoadState::Ready,
            detail_state: ViewerLoadState::Ready,
            catalogue: Arc::new(catalogue),
            visible: vec![0, 1, 2, 3],
            detail: Arc::new(detail),
            full_session: true,
            ..RecallViewerState::default()
        };
        viewer.expanded.insert("collapsed".to_owned());
        viewer
    }

    #[test]
    fn factual_viewer_state_matrix_renders_safely_on_every_layout_tier() {
        let rich = rich_viewer();
        for (size, pane) in [
            (Size::new(120, 40), ViewerPane::Projects),
            (Size::new(75, 30), ViewerPane::Checkpoints),
            (Size::new(40, 20), ViewerPane::Projects),
            (Size::new(40, 20), ViewerPane::Checkpoints),
            (Size::new(40, 20), ViewerPane::Timeline),
            (Size::new(10, 4), ViewerPane::Timeline),
        ] {
            for unicode in [false, true] {
                let frame = render_state(rich.clone(), size, pane, unicode);
                assert_eq!(frame.size(), size);
            }
        }

        let mut filtering = rich.clone();
        filtering.filter = "checkpoint".to_owned();
        filtering.filter_editing = true;
        filtering.filter_progress = Some(ViewerFilterProgress {
            scanned: 2,
            total: 4,
            complete: false,
        });
        let _ = render_state(
            filtering.clone(),
            Size::new(120, 40),
            ViewerPane::Checkpoints,
            true,
        );
        filtering.filter_progress = Some(ViewerFilterProgress {
            scanned: 4,
            total: 4,
            complete: true,
        });
        filtering.filter_editing = false;
        let _ = render_state(
            filtering,
            Size::new(120, 40),
            ViewerPane::Checkpoints,
            false,
        );

        let mut refreshed = rich.clone();
        refreshed.filter_progress = None;
        refreshed.new_count = 3;
        let _ = render_state(refreshed, Size::new(120, 40), ViewerPane::Checkpoints, true);

        for (catalogue_state, filter) in [
            (ViewerLoadState::Loading, ""),
            (
                ViewerLoadState::Error("safe catalogue error".to_owned()),
                "",
            ),
            (ViewerLoadState::Ready, "no-match"),
        ] {
            let mut empty = rich.clone();
            empty.catalogue = Arc::new(Vec::new());
            empty.visible.clear();
            empty.catalogue_state = catalogue_state;
            empty.filter = filter.to_owned();
            let _ = render_state(empty, Size::new(120, 40), ViewerPane::Checkpoints, false);
        }

        for detail_state in [
            ViewerLoadState::Loading,
            ViewerLoadState::Error("safe detail error".to_owned()),
            ViewerLoadState::Ready,
        ] {
            let mut empty = rich.clone();
            empty.detail_state = detail_state;
            empty.detail = Arc::new(ViewerDetail {
                checkpoint_id: "checkpoint-complete".to_owned(),
                segments: Vec::new(),
                notice: Some("no detail facts".to_owned()),
            });
            let _ = render_state(empty, Size::new(120, 40), ViewerPane::Timeline, true);
        }

        let mut collapsed = rich;
        collapsed.full_session = false;
        collapsed.expanded.clear();
        let _ = render_state(collapsed, Size::new(120, 40), ViewerPane::Timeline, false);
    }
}
