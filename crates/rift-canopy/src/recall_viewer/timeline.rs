//! The timeline pane: the flattened line list keyboard and mouse address, and its drawing.

use crate::layout::Rect;
use crate::render::frame::{Color, Frame, Mods, Style};
use crate::state::ViewModel;

use super::draw::{cell_safe_text, put_clipped, short_id, warning_style};
use super::geometry::{timeline_controls, ViewerTimelineControls};
use super::model::{SyntaxClass, ViewerItemTone, ViewerLine, ViewerLineKind, ViewerTimelineItem};
use super::state::RecallViewerState;
use super::{ViewerLoadState, ViewerPane};

/// One terminal line of the flattened timeline. It keeps the id of the item
/// it belongs to, so Enter or a click on any of that item's lines toggles it.
#[derive(Clone)]
struct FlatTimelineLine {
    line: ViewerLine,
    prefix: &'static str,
    item_id: Option<String>,
    /// The role of the item this line belongs to; `Neutral` outside an item.
    tone: ViewerItemTone,
    /// The item's headline: the line a fold returns the cursor to.
    heading: bool,
    /// The item has a body to unfold.
    foldable: bool,
}

impl FlatTimelineLine {
    /// A line that belongs to no item: a segment title, its boundary, a spacer.
    fn chrome(kind: ViewerLineKind, text: String) -> Self {
        Self {
            line: ViewerLine::plain(kind, text),
            prefix: "",
            item_id: None,
            tone: ViewerItemTone::Neutral,
            heading: false,
            foldable: false,
        }
    }
}

/// What Right and Left may do to the item under one flattened line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineFold {
    /// Expansion key of the item.
    pub id: String,
    /// Whether the reader has unfolded it.
    pub unfolded: bool,
    /// The flattened line of the item's headline.
    pub heading_line: usize,
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

/// The fold under one flattened line, or `None` when that line belongs to no
/// item or to one with nothing to unfold. Keyboard folding asks this so Right
/// and Left act on the item and fall through to pane movement everywhere else.
#[must_use]
pub fn timeline_fold_at(viewer: &RecallViewerState, line: usize) -> Option<TimelineFold> {
    let lines = flatten_timeline(viewer);
    let id = lines
        .get(line)
        .filter(|flat| flat.foldable)?
        .item_id
        .clone()?;
    let heading_line = lines
        .get(..=line)?
        .iter()
        .rposition(|flat| flat.heading && flat.item_id.as_deref() == Some(id.as_str()))?;
    Some(TimelineFold {
        unfolded: viewer.expanded.contains(&id),
        id,
        heading_line,
    })
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
        out.push(FlatTimelineLine::chrome(
            ViewerLineKind::Heading,
            format!("◆ {}", segment.title),
        ));
        if let Some(boundary) = &segment.boundary {
            out.push(FlatTimelineLine::chrome(
                ViewerLineKind::Dim,
                format!("  ┆ {boundary}"),
            ));
        }
        let mut shown = 0_usize;
        for item in &segment.items {
            if item.full_session_only && !viewer.full_session {
                continue;
            }
            // A blank row before every prompt but the first sets the turns of
            // a conversation apart without inventing a boundary in the data.
            if item.tone == ViewerItemTone::Prompt && shown > 0 {
                out.push(FlatTimelineLine::chrome(
                    ViewerLineKind::Text,
                    String::new(),
                ));
            }
            shown += 1;
            push_item(&mut out, item, viewer.expanded.contains(&item.id));
        }
    }
    out
}

/// Append one item: its headline, its always-visible summary, then its body
/// when it is open. `unfolded` is the reader's own choice for this item.
fn push_item(out: &mut Vec<FlatTimelineLine>, item: &ViewerTimelineItem, unfolded: bool) {
    let foldable = item.expanded_body.is_some() || item.collapsed;
    let open = !item.collapsed || unfolded;
    let marker = match (foldable, open) {
        (false, _) => "•",
        (true, true) => "▼",
        (true, false) => "▶",
    };
    let timestamp = item
        .timestamp
        .as_deref()
        .map_or(String::new(), |time| format!(" · {time}"));
    let of_item = |line: ViewerLine, prefix: &'static str, heading: bool| FlatTimelineLine {
        line,
        prefix,
        item_id: Some(item.id.clone()),
        tone: item.tone,
        heading,
        foldable,
    };
    out.push(of_item(
        ViewerLine::plain(
            ViewerLineKind::Heading,
            format!("  {marker} {}{timestamp}", item.title),
        ),
        "",
        true,
    ));
    out.extend(
        item.summary
            .iter()
            .map(|line| of_item(line.clone(), "    ", false)),
    );
    if !open {
        return;
    }
    let body = item
        .expanded_body
        .as_ref()
        .filter(|_| unfolded)
        .unwrap_or(&item.body);
    out.extend(body.iter().map(|line| of_item(line.clone(), "    ", false)));
}

fn draw_timeline_line(
    vm: &ViewModel,
    frame: &mut Frame,
    rect: Rect,
    flat: &FlatTimelineLine,
    unicode: bool,
    selected: bool,
) {
    let row_style = line_style(vm, flat, selected);
    if matches!(
        flat.line.kind,
        ViewerLineKind::Addition | ViewerLineKind::Removal
    ) || selected
    {
        crate::chrome::fill(frame, rect, row_style);
    }
    let marker = diff_marker(flat.line.kind, unicode);
    let mut x = draw_lead(vm, frame, rect, flat, (marker, unicode, selected));
    let right = rect.x.saturating_add(rect.w);
    for span in &flat.line.spans {
        if x >= right {
            break;
        }
        x = draw_span(vm, frame, (x, right, rect.y), flat, span, selected);
    }
    if flat.line.kind == ViewerLineKind::Gap && x.saturating_add(2) <= right {
        frame.put_str(x, rect.y, &format!(" {marker}"), row_style);
    }
}

const fn diff_marker(kind: ViewerLineKind, unicode: bool) -> char {
    match kind {
        ViewerLineKind::Addition => '+',
        ViewerLineKind::Removal => '-',
        ViewerLineKind::DiffHunk => '@',
        ViewerLineKind::Gap if unicode => '…',
        ViewerLineKind::Gap => '.',
        _ => ' ',
    }
}

/// Draw what precedes a line's text and return where the text starts: the
/// old/new line-number gutter on a diff row, a rail in the item's colour on a
/// conversational body row, the plain indent otherwise.
fn draw_lead(
    vm: &ViewModel,
    frame: &mut Frame,
    rect: Rect,
    flat: &FlatTimelineLine,
    (marker, unicode, selected): (char, bool, bool),
) -> u16 {
    let row_style = line_style(vm, flat, selected);
    if matches!(
        flat.line.kind,
        ViewerLineKind::Addition
            | ViewerLineKind::Removal
            | ViewerLineKind::Context
            | ViewerLineKind::DiffHunk
            | ViewerLineKind::Gap
    ) {
        let number = |line: Option<u32>| line.map_or("    ".to_owned(), |n| format!("{n:>4}"));
        let gutter = format!(
            "{} {} {marker} ",
            number(flat.line.old_line),
            number(flat.line.new_line)
        );
        let clipped: String = gutter.chars().take(usize::from(rect.w)).collect();
        return frame.put_str(rect.x, rect.y, &clipped, row_style);
    }
    if flat.prefix.is_empty() {
        return rect.x;
    }
    let Some(color) = tone_color(vm, flat.tone) else {
        return frame.put_str(rect.x, rect.y, flat.prefix, row_style);
    };
    let mut rail_style = Style::fg(color);
    if selected {
        rail_style = rail_style.with_bg(crate::theme::SELECT_BG);
    }
    let rail = if unicode { "  │ " } else { "  | " };
    let clipped: String = rail.chars().take(usize::from(rect.w)).collect();
    frame.put_str(rect.x, rect.y, &clipped, rail_style)
}

/// Draw one span clipped to `right` and return the next free column. Plain
/// text takes the row's own style, so a headline, a dim note and a warning
/// keep their colour; only classified source tokens are painted by syntax.
fn draw_span(
    vm: &ViewModel,
    frame: &mut Frame,
    (mut x, right, y): (u16, u16, u16),
    flat: &FlatTimelineLine,
    span: &super::model::ViewerSpan,
    selected: bool,
) -> u16 {
    let row_style = line_style(vm, flat, selected);
    let text = cell_safe_text(&span.text);
    let counted = flat.heading && flat.tone == ViewerItemTone::CodeChange;
    for word in text.split_inclusive(' ') {
        if x >= right {
            break;
        }
        let style = match span.class {
            SyntaxClass::Plain if counted => {
                count_style(word.trim_end(), selected).unwrap_or(row_style)
            }
            SyntaxClass::Plain => row_style,
            class => syntax_style(vm, class, flat.line.kind, selected),
        };
        let clipped: String = word.chars().take(usize::from(right - x)).collect();
        x = frame.put_str(x, y, &clipped, style);
    }
    x
}

/// A folded change still shows how much it added and removed: the `+N` and
/// `-N` at the end of its headline keep the colours the unfolded diff uses.
fn count_style(word: &str, selected: bool) -> Option<Style> {
    let digits = |rest: &str| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit());
    let color = match (word.strip_prefix('+'), word.strip_prefix('-')) {
        (Some(rest), _) if digits(rest) => crate::theme::HEALTH_OK,
        (_, Some(rest)) if digits(rest) => crate::theme::HEALTH_DOWN,
        _ => return None,
    };
    let style = Style::fg(color).with_mods(Mods::BOLD);
    Some(if selected {
        style.with_bg(crate::theme::SELECT_BG)
    } else {
        style
    })
}

/// The colour that marks an item's role. The four hues are the Recall
/// channel strokes, so a theme override recolours the transcript with the
/// rest of the dashboard; tool traffic stays grey so the conversation and the
/// changes are what stand out.
fn tone_color(vm: &ViewModel, tone: ViewerItemTone) -> Option<Color> {
    let [recall, assistant, prompt, change] = vm.theme.recall_channel_colors();
    match tone {
        ViewerItemTone::Neutral => None,
        ViewerItemTone::Prompt => Some(prompt),
        ViewerItemTone::Assistant => Some(assistant),
        ViewerItemTone::CodeChange => Some(change),
        ViewerItemTone::Recall => Some(recall),
        ViewerItemTone::Tool | ViewerItemTone::ToolResult | ViewerItemTone::System => {
            Some(crate::theme::STATUS_IDLE)
        }
    }
}

const fn quiet(tone: ViewerItemTone) -> bool {
    matches!(
        tone,
        ViewerItemTone::Tool | ViewerItemTone::ToolResult | ViewerItemTone::System
    )
}

fn line_style(vm: &ViewModel, flat: &FlatTimelineLine, selected: bool) -> Style {
    let mut style = match (flat.line.kind, tone_color(vm, flat.tone)) {
        (ViewerLineKind::Heading, Some(color)) if flat.heading && quiet(flat.tone) => {
            Style::fg(color)
        }
        (ViewerLineKind::Heading, Some(color)) if flat.heading => {
            Style::fg(color).with_mods(Mods::BOLD)
        }
        (ViewerLineKind::Text, Some(_)) if quiet(flat.tone) => vm.theme.dim(),
        (kind, _) => kind_style(vm, kind),
    };
    if selected {
        style = style.with_bg(crate::theme::SELECT_BG).with_mods(Mods::BOLD);
    }
    style
}

fn kind_style(vm: &ViewModel, kind: ViewerLineKind) -> Style {
    match kind {
        ViewerLineKind::Heading | ViewerLineKind::DiffFile => vm.theme.key_hint(),
        ViewerLineKind::Dim | ViewerLineKind::Context | ViewerLineKind::Gap => vm.theme.dim(),
        ViewerLineKind::Warning => warning_style(vm),
        ViewerLineKind::Addition => vm.theme.body().with_bg(Color::Rgb(0x13, 0x34, 0x25)),
        ViewerLineKind::Removal => vm.theme.body().with_bg(Color::Rgb(0x3b, 0x1d, 0x22)),
        ViewerLineKind::DiffHunk => vm.theme.dim().with_bg(vm.theme.surface_bg()),
        ViewerLineKind::Text => vm.theme.body(),
    }
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
                            tone: crate::recall_viewer::ViewerItemTone::Neutral,
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
                            tone: crate::recall_viewer::ViewerItemTone::Neutral,
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

    fn toned(id: &str, tone: ViewerItemTone, collapsed: bool) -> ViewerTimelineItem {
        ViewerTimelineItem {
            id: id.to_owned(),
            title: id.to_owned(),
            timestamp: None,
            summary: Vec::new(),
            body: vec![
                ViewerLine::plain(ViewerLineKind::Text, "first"),
                ViewerLine::plain(ViewerLineKind::Text, "second"),
            ],
            expanded_body: None,
            collapsed,
            full_session_only: false,
            tone,
        }
    }

    fn viewer_of(items: Vec<ViewerTimelineItem>) -> RecallViewerState {
        RecallViewerState {
            detail: Arc::new(ViewerDetail {
                checkpoint_id: "cp".to_owned(),
                segments: vec![ViewerTimelineSegment {
                    title: "Transcript".to_owned(),
                    boundary: None,
                    ordered_by_time: false,
                    items,
                }],
                notice: None,
            }),
            ..RecallViewerState::default()
        }
    }

    #[test]
    fn a_fold_is_addressed_from_any_line_of_its_item_and_only_where_one_exists() {
        let mut viewer = viewer_of(vec![
            toned("said", ViewerItemTone::Assistant, false),
            toned("change", ViewerItemTone::CodeChange, true),
        ]);
        // 0 segment title · 1-3 the open item · 4 the folded change's headline.
        assert_eq!(timeline_fold_at(&viewer, 0), None);
        assert_eq!(timeline_fold_at(&viewer, 2), None, "nothing to unfold");
        let folded = timeline_fold_at(&viewer, 4).unwrap();
        assert_eq!((folded.id.as_str(), folded.unfolded), ("change", false));
        assert_eq!(timeline_fold_at(&viewer, 5), None, "past the last line");

        viewer.expanded.insert("change".to_owned());
        let from_body = timeline_fold_at(&viewer, 6).unwrap();
        assert!(from_body.unfolded);
        assert_eq!(from_body.heading_line, 4);
    }

    #[test]
    fn a_blank_row_sets_each_later_prompt_apart_and_never_opens_the_segment() {
        let viewer = viewer_of(vec![
            toned("first prompt", ViewerItemTone::Prompt, false),
            toned("reply", ViewerItemTone::Assistant, false),
            toned("second prompt", ViewerItemTone::Prompt, false),
        ]);
        let lines = flatten_timeline(&viewer);
        let blank: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, flat)| flat.item_id.is_none() && flat.line.spans[0].text.is_empty())
            .map(|(index, _)| index)
            .collect();
        assert_eq!(blank, [7], "one spacer, before the second prompt only");
        assert!(lines[1].heading && lines[1].tone == ViewerItemTone::Prompt);
        assert!(!lines[2].heading && lines[2].tone == ViewerItemTone::Prompt);
    }

    #[test]
    fn a_folded_change_keeps_the_diff_colours_on_its_line_counts() {
        assert!(count_style("+12", false).is_some());
        assert!(count_style("-3", false).is_some());
        for word in ["+", "-", "+x", "--1", "src/a-1.rs", "12"] {
            assert!(count_style(word, false).is_none(), "{word}");
        }
        assert_ne!(count_style("+1", false), count_style("-1", false));
    }
}
