//! The timeline pane: the flattened line list keyboard and mouse address, and its drawing.

use crate::layout::Rect;
use crate::render::frame::{Color, Frame, Mods, Style};
use crate::state::ViewModel;

use super::draw::{cell_safe_text, put_clipped, short_id, warning_style};
use super::geometry::{timeline_controls, ViewerTimelineControls};
use super::model::{SyntaxClass, ViewerLine, ViewerLineKind};
use super::state::RecallViewerState;
use super::{ViewerLoadState, ViewerPane};

/// One terminal line of the flattened timeline. It keeps the id of the item
/// it belongs to, so Enter or a click on any of that item's lines toggles it.
#[derive(Clone)]
struct FlatTimelineLine {
    line: ViewerLine,
    prefix: &'static str,
    item_id: Option<String>,
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

/// Draw the timeline pane: its frame and two action chips, then either the
/// flattened rows from the scroll offset or the one line saying why there are
/// none.
pub(super) fn draw_timeline(vm: &ViewModel, frame: &mut Frame, rect: Rect, unicode: bool) {
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
    draw_timeline_chips(vm, frame, &controls);
    let content = controls.content;
    if let Some((text, style)) = detail_placeholder(vm) {
        put_clipped(frame, content.x, content.y, content.w, text, style);
        return;
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
            content.x,
            content.y,
            content.w,
            notice,
            vm.theme.dim(),
        );
        return;
    }
    draw_timeline_rows(vm, frame, content, &lines, unicode);
}

/// Draw the `Full session` and `Dashboard` chips at the rectangles
/// [`timeline_controls`] solved, the same ones the mouse plane hit-tests.
fn draw_timeline_chips(vm: &ViewModel, frame: &mut Frame, controls: &ViewerTimelineControls) {
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
}

/// The line that stands in for a detail with no segments yet: loading, or the
/// safe error. A partial detail that already has segments renders them
/// instead, so facts that arrived are never hidden behind a status line.
fn detail_placeholder(vm: &ViewModel) -> Option<(&str, Style)> {
    if !vm.viewer.detail.segments.is_empty() {
        return None;
    }
    match &vm.viewer.detail_state {
        ViewerLoadState::Loading => Some(("loading checkpoint detail …", vm.theme.dim())),
        ViewerLoadState::Error(error) => Some((error.as_str(), warning_style(vm))),
        _ => None,
    }
}

/// Draw the flattened lines that fit in `content` from the scroll offset,
/// marking the keyboard cursor's line, and the scrollbar when they overflow.
fn draw_timeline_rows(
    vm: &ViewModel,
    frame: &mut Frame,
    content: Rect,
    lines: &[FlatTimelineLine],
    unicode: bool,
) {
    let start = vm.viewer.timeline_scroll.min(lines.len().saturating_sub(1));
    for (screen_row, line) in lines
        .iter()
        .skip(start)
        .take(usize::from(content.h))
        .enumerate()
    {
        let Ok(dy) = u16::try_from(screen_row) else {
            break;
        };
        draw_timeline_line(
            vm,
            frame,
            Rect {
                x: content.x,
                y: content.y + dy,
                w: content.w,
                h: 1,
            },
            line,
            unicode,
            start + screen_row == vm.viewer.timeline_cursor,
        );
    }
    if let Some(bar) = crate::chrome::scrollbar(content, start, lines.len()) {
        crate::chrome::draw_scrollbar(frame, &bar, vm.theme, unicode);
    }
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

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::recall_viewer::{ViewerDetail, ViewerTimelineItem, ViewerTimelineSegment};

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
}
