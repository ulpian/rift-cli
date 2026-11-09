//! The viewer body's draw entry, its tab strip and hint row, and the cell helpers every pane shares.

use crate::layout::Rect;
use crate::render::frame::{Color, Frame, Style};
use crate::state::ViewModel;

use super::catalogue::draw_checkpoints;
use super::geometry::geometry;
use super::timeline::draw_timeline;
use super::ViewerPane;

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
    for (index, (pane, label)) in [
        (ViewerPane::Checkpoints, "Checkpoints"),
        (ViewerPane::Timeline, "Timeline"),
    ]
    .into_iter()
    .enumerate()
    {
        let x = rect.x + if index == 0 { 0 } else { rect.w / 2 };
        let w = if index == 0 {
            rect.w / 2
        } else {
            rect.w - rect.w / 2
        };
        let selected = vm.viewer.pane == pane
            || (pane == ViewerPane::Checkpoints
                && matches!(vm.viewer.pane, ViewerPane::Projects | ViewerPane::Command));
        let text = if selected {
            format!("[{label}]")
        } else {
            format!(" {label} ")
        };
        put_clipped(frame, x, rect.y, w, &text, vm.theme.key_hint());
    }
}

pub(super) fn warning_style(vm: &ViewModel) -> Style {
    Style::fg(Color::Rgb(0xf0, 0xc4, 0x5c)).with_bg(vm.theme.surface_bg())
}

fn draw_hint(vm: &ViewModel, frame: &mut Frame, rect: Rect) {
    if rect.h == 0 {
        return;
    }
    let text = if vm.viewer.filter_editing {
        "type filter · Enter apply · Esc cancel"
    } else {
        "Tab pane · j/k move · PgUp/PgDn page · f Filter · F Full session · r Dashboard · / command · Right/Left unfold/fold"
    };
    put_clipped(frame, rect.x, rect.y, rect.w, text, vm.theme.dim());
}

pub(super) fn short_id(value: &str) -> &str {
    value.get(..12.min(value.len())).unwrap_or(value)
}

pub(super) fn put_clipped(frame: &mut Frame, x: u16, y: u16, width: u16, text: &str, style: Style) {
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
pub(super) fn cell_safe_text(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\n' | '\r' | '\t' => ' ',
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => '\u{fffd}',
            character if character.is_control() => '\u{fffd}',
            character => character,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::recall_viewer::fixtures::seeded_row;
    use crate::recall_viewer::{
        RecallViewerState, Surface, SyntaxClass, ViewerCheckpointRow, ViewerDetail,
        ViewerFilterProgress, ViewerLine, ViewerLineKind, ViewerLoadState, ViewerOrigin,
        ViewerSpan, ViewerTimelineItem, ViewerTimelineSegment, ViewerTimestampProvenance,
    };
    use crate::render::frame::Size;

    #[test]
    fn terminal_cell_boundary_neutralizes_controls_and_bidi() {
        let safe = cell_safe_text("a\tb\n\u{1b}[31m\u{202e}x");
        assert_eq!(safe, "a b \u{fffd}[31m\u{fffd}x");
        assert!(!safe.chars().any(char::is_control));
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

    /// A viewer with every catalogue posture, every syntax class on every line
    /// kind, a collapsible item already expanded and the full session shown.
    fn rich_viewer() -> RecallViewerState {
        let mut viewer = RecallViewerState {
            project: Some("/repo".to_owned()),
            catalogue_state: ViewerLoadState::Ready,
            detail_state: ViewerLoadState::Ready,
            catalogue: Arc::new(rich_catalogue()),
            visible: vec![0, 1, 2, 3],
            detail: Arc::new(rich_detail()),
            full_session: true,
            ..RecallViewerState::default()
        };
        viewer.expanded.insert("collapsed".to_owned());
        viewer
    }

    /// One row per timestamp provenance: complete, excerpt still loading,
    /// commit-dated with a parse issue, and wholly unknown.
    fn rich_catalogue() -> Vec<ViewerCheckpointRow> {
        let mut loading = seeded_row("checkpoint-loading", "loading");
        loading.timestamp_provenance = ViewerTimestampProvenance::SessionDerived;
        loading.excerpt_loading = true;

        let mut commit_time = seeded_row("checkpoint-commit", "commit");
        commit_time.timestamp_provenance = ViewerTimestampProvenance::AnchorCommit;
        commit_time.origins = vec![ViewerOrigin {
            storage_ref: "refs/checkpoints/one".to_owned(),
            dialect: "legacy".to_owned(),
            kind: "per-checkpoint-ref".to_owned(),
            tip_oid: None,
        }];
        commit_time.issues = vec!["metadata".to_owned()];

        let unknown = seeded_row("checkpoint-unknown", "unknown");
        vec![complete_row(), loading, commit_time, unknown]
    }

    /// A row with every fact present: two origins, a chain position and two
    /// containing branches, so each `+n` suffix is drawn.
    fn complete_row() -> ViewerCheckpointRow {
        let mut complete = seeded_row("checkpoint-complete", "complete main");
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
        complete
    }

    /// A time-ordered segment holding a collapsible diff and an ordinary
    /// event, then a segment shown only in full-session mode.
    fn rich_detail() -> ViewerDetail {
        let lines = every_style_line();
        ViewerDetail {
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
                            tone: crate::recall_viewer::ViewerItemTone::Neutral,
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
                            tone: crate::recall_viewer::ViewerItemTone::Neutral,
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
                        tone: crate::recall_viewer::ViewerItemTone::Neutral,
                    }],
                },
            ],
            notice: None,
        }
    }

    /// One numbered line per line kind, each carrying one span per syntax
    /// class, so every foreground meets every row background.
    fn every_style_line() -> Vec<ViewerLine> {
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
        line_kinds
            .into_iter()
            .map(|kind| ViewerLine {
                old_line: Some(7),
                new_line: Some(9),
                kind,
                spans: syntax_spans.clone(),
            })
            .collect::<Vec<_>>()
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
    }

    #[test]
    fn a_transcript_scan_renders_safely_while_running_and_once_complete() {
        let mut filtering = rich_viewer();
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
    }

    #[test]
    fn a_refresh_that_found_new_rows_renders_its_notice_safely() {
        let mut refreshed = rich_viewer();
        refreshed.filter_progress = None;
        refreshed.new_count = 3;
        let _ = render_state(refreshed, Size::new(120, 40), ViewerPane::Checkpoints, true);
    }

    #[test]
    fn every_empty_catalogue_posture_renders_safely() {
        for (catalogue_state, filter) in [
            (ViewerLoadState::Loading, ""),
            (
                ViewerLoadState::Error("safe catalogue error".to_owned()),
                "",
            ),
            (ViewerLoadState::Ready, "no-match"),
        ] {
            let mut empty = rich_viewer();
            empty.catalogue = Arc::new(Vec::new());
            empty.visible.clear();
            empty.catalogue_state = catalogue_state;
            empty.filter = filter.to_owned();
            let _ = render_state(empty, Size::new(120, 40), ViewerPane::Checkpoints, false);
        }
    }

    #[test]
    fn every_empty_detail_posture_renders_safely() {
        for detail_state in [
            ViewerLoadState::Loading,
            ViewerLoadState::Error("safe detail error".to_owned()),
            ViewerLoadState::Ready,
        ] {
            let mut empty = rich_viewer();
            empty.detail_state = detail_state;
            empty.detail = Arc::new(ViewerDetail {
                checkpoint_id: "checkpoint-complete".to_owned(),
                segments: Vec::new(),
                notice: Some("no detail facts".to_owned()),
            });
            let _ = render_state(empty, Size::new(120, 40), ViewerPane::Timeline, true);
        }
    }

    #[test]
    fn the_collapsed_checkpoint_slice_renders_safely() {
        let mut collapsed = rich_viewer();
        collapsed.full_session = false;
        collapsed.expanded.clear();
        let _ = render_state(collapsed, Size::new(120, 40), ViewerPane::Timeline, false);
    }
}
