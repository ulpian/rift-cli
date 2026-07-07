//! The pure dashboard renderer: `view::draw(&ViewModel, backend, &mut Frame)`.
//!
//! The single renderer both backends flush — no `&mut AppState`, no I/O, no
//! engine types — so it is a deterministic function of the snapshot and every
//! panel step's headless/snapshot test reuses it. The backend participates
//! only through the [`RenderBackend::plot`] seam (sub-cell graphs choose
//! braille or the block floor there); everything else is pure [`Frame`]
//! drawing.

use crate::col2::sanitize_for_terminal;
use crate::layout::{
    choose_mode, command_split, single_panel, solve, two_col, LayoutMode, Rect, MIN_NOTICE_COLS,
    MIN_NOTICE_ROWS,
};
use crate::render::frame::{Color, Frame, Style};
use crate::render::RenderBackend;
use crate::state::ViewModel;
use crate::{EntryKind, Mode, Panel, ProjectStatus};

/// The Rift accent color (title + selection).
const ACCENT: Style = Style::fg(Color::Rgb(0x6c, 0xe0, 0x9a));

/// Render the whole dashboard for `vm` into `frame`, choosing the responsive
/// layout tier for the current size (P3 S2). `backend` participates only
/// through the plot seam (column-3 graphs).
pub fn draw(vm: &ViewModel, backend: &mut dyn RenderBackend, frame: &mut Frame) {
    let area = frame.rect();
    if area.w == 0 || area.h == 0 {
        return;
    }

    // Below the minimum, show only the centered notice (no title/panels).
    if choose_mode(vm.size) == LayoutMode::TooSmall {
        draw_too_small(frame, area);
        return;
    }

    draw_title(vm, frame, area);
    let body = Rect {
        x: area.x,
        y: area.y + 1,
        w: area.w,
        h: area.h.saturating_sub(1),
    };

    if vm.show_wizard {
        let (above, command) = command_split(body);
        draw_wizard(vm, frame, above);
        draw_command(vm, frame, command);
        return;
    }

    match choose_mode(vm.size) {
        LayoutMode::TwoCol => draw_two_col(vm, backend, frame, body),
        LayoutMode::SinglePanel => draw_single(vm, backend, frame, body),
        // Full3Col (TooSmall handled above).
        _ => draw_full(vm, backend, frame, body),
    }

    // The dispatch overlay layers over the body (ADR-0021 "Dispatch posture"):
    // results scroll, confirms gate, the running line holds during a dispatch.
    if vm.overlay.is_some() {
        draw_overlay(vm, frame, body);
    }
}

/// Where the dispatch overlay sits: over the body, clear of the command bar,
/// with a small margin. Shared with `hit_test` so mouse targets and pixels
/// can never disagree.
#[must_use]
pub fn overlay_rect(body: Rect) -> Rect {
    let margin_x = 2u16.min(body.w / 8);
    let h_avail = body.h.saturating_sub(crate::layout::COMMAND_ROWS);
    Rect {
        x: body.x + margin_x,
        y: body.y + 1,
        w: body.w.saturating_sub(margin_x * 2),
        h: h_avail.saturating_sub(2),
    }
}

/// The overlay rect derived from the full frame area (the hit-test entry —
/// it re-derives the body split `draw` uses).
#[must_use]
pub fn overlay_rect_for(size: crate::render::frame::Size) -> Rect {
    let body = Rect {
        x: 0,
        y: 1,
        w: size.w,
        h: size.h.saturating_sub(1),
    };
    overlay_rect(body)
}

/// The confirm overlay's two button labels (shared with hit-testing).
pub const CONFIRM_BUTTONS: (&str, &str) = ("[ Enter: confirm ]", "[ Esc: cancel ]");

/// Draw the dispatch overlay: a bordered, scrollable pane over the body.
fn draw_overlay(vm: &ViewModel, frame: &mut Frame, body: Rect) {
    use crate::dispatch::Overlay;
    let Some(overlay) = &vm.overlay else { return };
    let rect = overlay_rect(body);
    if rect.w < 8 || rect.h < 3 {
        return; // clamped away on tiny terminals; the bar still works
    }
    // Blank the region so the panels underneath never bleed through.
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            frame.set(rect.x + dx, rect.y + dy, ' ', Style::DEFAULT);
        }
    }
    match overlay {
        Overlay::Results {
            title,
            lines,
            scroll,
            footer,
            ..
        } => {
            let inner = frame.border(rect, &sanitize_for_terminal(title), ACCENT);
            let footer_rows = usize::from(footer.is_some());
            let visible = usize::from(inner.h).saturating_sub(footer_rows);
            for (i, line) in lines.iter().skip(*scroll).take(visible).enumerate() {
                let Ok(dy) = u16::try_from(i) else { break };
                clip_line(frame, inner, dy, line, Style::DEFAULT);
            }
            if let Some(f) = footer {
                let dy = inner.h.saturating_sub(1);
                clip_line(frame, inner, dy, &sanitize_for_terminal(f), ACCENT);
            }
        }
        Overlay::Confirm { request, repo } => {
            let inner = frame.border(rect, "confirm", ACCENT);
            clip_line(
                frame,
                inner,
                0,
                &sanitize_for_terminal(&format!("will run: {}", request.verb_line())),
                Style::DEFAULT,
            );
            clip_line(
                frame,
                inner,
                1,
                &sanitize_for_terminal(&format!("target repository: {repo}")),
                Style::DEFAULT,
            );
            let buttons = format!("{}   {}", CONFIRM_BUTTONS.0, CONFIRM_BUTTONS.1);
            clip_line(frame, inner, 3, &buttons, ACCENT);
        }
        Overlay::Running { title } => {
            let inner = frame.border(rect, "running", ACCENT);
            clip_line(
                frame,
                inner,
                0,
                &sanitize_for_terminal(&format!("running {title} …")),
                Style::DEFAULT,
            );
        }
    }
}

/// The Rift accent header row.
fn draw_title(vm: &ViewModel, frame: &mut Frame, area: Rect) {
    let mut header = String::from("RIFT — Canopy");
    if let Some(s) = &vm.status {
        header.push_str("   ");
        header.push_str(s);
    }
    frame.put_str(area.x, area.y, &header, ACCENT);
}

/// The full 20/40/40 three-column grid.
fn draw_full(vm: &ViewModel, backend: &mut dyn RenderBackend, frame: &mut Frame, body: Rect) {
    let r = solve(body);
    if r.col1.h >= 2 {
        draw_projects(vm, frame, r.col1);
        draw_detail(vm, frame, r.col2);
        draw_graphs(vm, backend, frame, r.col3);
    }
    draw_command(vm, frame, r.command);
}

/// The two-column degraded grid: projects left, detail over graphs right.
fn draw_two_col(vm: &ViewModel, backend: &mut dyn RenderBackend, frame: &mut Frame, body: Rect) {
    let r = two_col(body);
    draw_projects(vm, frame, r.col1);
    draw_detail(vm, frame, r.col2);
    draw_graphs(vm, backend, frame, r.col3);
    draw_command(vm, frame, r.command);
}

/// The single-panel degraded layout: a tab strip + the focused panel.
fn draw_single(vm: &ViewModel, backend: &mut dyn RenderBackend, frame: &mut Frame, body: Rect) {
    let (tabs, panel, command) = single_panel(body);
    draw_tab_strip(vm, frame, tabs);
    match vm.focus {
        Panel::Detail => draw_detail(vm, frame, panel),
        Panel::Graphs => draw_graphs(vm, backend, frame, panel),
        // Projects (and Command focus) show the projects panel.
        Panel::Projects | Panel::Command => draw_projects(vm, frame, panel),
    }
    draw_command(vm, frame, command);
}

/// The `[Projects] Detail Graphs` tab strip for the single-panel layout.
fn draw_tab_strip(vm: &ViewModel, frame: &mut Frame, tabs: Rect) {
    if tabs.h == 0 {
        return;
    }
    let mut strip = String::new();
    for (panel, label) in [
        (Panel::Projects, "Projects"),
        (Panel::Detail, "Detail"),
        (Panel::Graphs, "Graphs"),
    ] {
        if vm.focus == panel {
            strip.push('[');
            strip.push_str(label);
            strip.push(']');
        } else {
            strip.push(' ');
            strip.push_str(label);
            strip.push(' ');
        }
    }
    clip_at(frame, tabs.x, tabs.y, tabs.w, &strip, ACCENT);
}

/// The centered "terminal too small" notice.
fn draw_too_small(frame: &mut Frame, area: Rect) {
    let msg = format!("Terminal too small (need ≥ {MIN_NOTICE_COLS}×{MIN_NOTICE_ROWS})");
    let len = u16::try_from(msg.chars().count()).unwrap_or(u16::MAX);
    let x = area.x + area.w.saturating_sub(len) / 2;
    let y = area.y + area.h / 2;
    clip_at(frame, x, y, area.w, &msg, Style::DEFAULT);
}

/// The first-run hint, spanning the full column area so it stays readable.
fn draw_wizard(vm: &ViewModel, frame: &mut Frame, above: Rect) {
    let inner = frame.border(above, "projects", border_style(vm.focus == Panel::Projects));
    clip_line(
        frame,
        inner,
        0,
        "No projects registered yet.",
        Style::DEFAULT,
    );
    clip_line(
        frame,
        inner,
        1,
        "Press 'a' to add a project, or run `rift setup`.",
        Style::DEFAULT,
    );
}

/// The border style for a panel: accented when focused, plain otherwise.
fn border_style(focused: bool) -> Style {
    if focused {
        ACCENT
    } else {
        Style::DEFAULT
    }
}

/// Draw the bottom command / add-project bar (mode-aware), with a block cursor.
fn draw_command(vm: &ViewModel, frame: &mut Frame, command: Rect) {
    if command.h == 0 {
        return;
    }
    let (btitle, prefix) = match vm.mode {
        Mode::AddProject => ("add project (path)", ""),
        _ => ("command", "/"),
    };
    let inner = frame.border(command, btitle, border_style(vm.focus == Panel::Command));
    let line = format!("{prefix}{}\u{2588}", vm.command_line);
    clip_line(frame, inner, 0, &line, Style::DEFAULT);
    // A parse error renders inline under the input — the caret run under the
    // offending span plus the reason (and the nearest-verb hint), drawn over
    // the bar's bottom border row so no layout shifts.
    if let Some(err) = &vm.command_error {
        // The bar draws a virtual `/` before the buffer: spans shift right by
        // the prefix so the caret run sits under the typed glyphs.
        let start =
            (err.span.0 + prefix.chars().count()).min(usize::from(inner.w).saturating_sub(1));
        let len = err.span.1.saturating_sub(err.span.0).max(1);
        let mut marker = " ".repeat(start);
        marker.push_str(&"^".repeat(len.min(usize::from(inner.w).saturating_sub(start))));
        marker.push(' ');
        marker.push_str(&sanitize_for_terminal(&err.reason));
        if let Some(hint) = err.hint {
            marker.push_str(&format!(" — did you mean `{hint}`?"));
        }
        let y = command.y.saturating_add(command.h.saturating_sub(1));
        clip_at(frame, inner.x, y, inner.w, &marker, ACCENT);
    }
}

/// Column 1 — the project list, windowed to keep the selection in view.
fn draw_projects(vm: &ViewModel, frame: &mut Frame, rect: Rect) {
    let inner = frame.border(rect, "projects", border_style(vm.focus == Panel::Projects));
    if vm.projects.is_empty() {
        clip_line(
            frame,
            inner,
            0,
            "No projects yet — press 'a'",
            Style::DEFAULT,
        );
        return;
    }
    let (start, end) = visible_window(vm.selected, vm.projects.len(), usize::from(inner.h));
    let label_w = usize::from(inner.w).saturating_sub(4); // "> ● "
    for (row, p) in vm.projects.iter().enumerate().take(end).skip(start) {
        let Ok(dy) = u16::try_from(row.saturating_sub(start)) else {
            break;
        };
        let selected = row == vm.selected;
        let style = if selected { ACCENT } else { Style::DEFAULT };
        let cursor = if selected { '>' } else { ' ' };
        // Linked worktrees render as indented children of their repository.
        let indent = match p.kind {
            EntryKind::Repo => "",
            EntryKind::Worktree => "  ",
        };
        // The label is untrusted (a directory name); sanitize before eliding.
        let label = elide(
            &sanitize_for_terminal(&p.name),
            label_w.saturating_sub(indent.len()),
        );
        let line = format!("{cursor} {indent}{} {label}", p.status.glyph());
        clip_line(frame, inner, dy, &line, style);
    }
}

/// Column 2 — the detail panel (project sections + nic-gauge), scrollable. Every
/// projection-derived string (label, path, model name) is escape-sanitized before
/// it can reach a cell, so untrusted capture data can never inject a terminal
/// sequence (`05-security`).
fn draw_detail(vm: &ViewModel, frame: &mut Frame, rect: Rect) {
    let inner = frame.border(rect, "detail", border_style(vm.focus == Panel::Detail));
    let lines = match &vm.detail {
        Some(d) => detail_line_kinds(d, vm.toggle_cursor, vm.focus == Panel::Detail),
        None => vec![DetailLine::plain("no project selected")],
    };
    render_lines(frame, inner, vm.detail_scroll, &lines);
}

/// What a detail-panel row is, for hit-testing. Only feature-toggle rows are
/// actionable; everything else is inert text.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineKind {
    /// Inert text (header, section labels, values).
    Plain,
    /// A feature-toggle row, carrying its toggle index (`0..`).
    Toggle(usize),
}

/// One rendered detail-panel line: its text plus what it represents. Built once
/// by [`detail_line_kinds`] and consumed by both the renderer (draws `text`) and
/// [`crate::hit`] (maps a clicked screen-row to its `kind`), so a click always
/// resolves to the exact toggle the renderer drew.
#[derive(Clone)]
pub(crate) struct DetailLine {
    /// What this row is.
    pub kind: LineKind,
    /// The (already-sanitized) text drawn on this row.
    pub text: String,
}

impl DetailLine {
    /// An inert text line.
    fn plain(text: impl Into<String>) -> Self {
        Self {
            kind: LineKind::Plain,
            text: text.into(),
        }
    }
}

/// Build the sectioned detail body (header · models · settings · capture ·
/// features). An uninitialized project shows an init hint instead. `toggle_cursor`
/// + `focused` drive the feature-toggle cursor highlight.
pub(crate) fn detail_line_kinds(
    d: &crate::col2::DetailView,
    toggle_cursor: usize,
    focused: bool,
) -> Vec<DetailLine> {
    let mut lines: Vec<DetailLine> = Vec::new();
    let kind = match d.kind {
        EntryKind::Repo => "repo",
        EntryKind::Worktree => "worktree",
    };
    lines.push(DetailLine::plain(format!(
        "{} ({kind})",
        sanitize_for_terminal(&d.label)
    )));
    lines.push(DetailLine::plain(format!(
        "path: {}",
        sanitize_for_terminal(&d.path)
    )));
    lines.push(DetailLine::plain(format!(
        "status: {}",
        status_label(d.status)
    )));

    if matches!(d.status, ProjectStatus::NotInitialized) {
        lines.push(DetailLine::plain(""));
        lines.push(DetailLine::plain("not initialized — run `rift init`"));
        return lines;
    }

    lines.push(DetailLine::plain(""));
    lines.push(DetailLine::plain("models"));
    if d.models.is_empty() {
        lines.push(DetailLine::plain("  (none yet)"));
    } else {
        for m in &d.models {
            lines.push(DetailLine::plain(format!(
                "  {} ×{}",
                sanitize_for_terminal(&m.model),
                m.calls
            )));
        }
    }

    lines.push(DetailLine::plain(""));
    lines.push(DetailLine::plain("settings"));
    lines.push(DetailLine::plain(format!(
        "  sensitive paths: {}",
        d.settings.sensitive_path_count
    )));
    lines.push(DetailLine::plain(format!(
        "  raw transcripts: {}",
        on_off(d.settings.raw_transcripts)
    )));
    lines.push(DetailLine::plain(format!(
        "  tool calls: {}",
        cap_skip(d.settings.captures_tool_calls)
    )));
    lines.push(DetailLine::plain(format!(
        "  token usage: {}",
        cap_skip(d.settings.captures_token_usage)
    )));

    lines.push(DetailLine::plain(""));
    lines.push(DetailLine::plain("capture"));
    lines.push(DetailLine::plain(format!("  events: {}", d.capture.events)));
    lines.push(DetailLine::plain(format!(
        "  sessions: {}",
        d.capture.sessions
    )));
    lines.push(DetailLine::plain(format!(
        "  checkpoints: {}",
        d.capture.checkpoints
    )));

    lines.push(DetailLine::plain(""));
    lines.push(DetailLine::plain("features"));
    for (i, row) in d.toggles.rows.iter().enumerate() {
        // A cursor marker only when the detail panel is focused; variants indent.
        let cursor = if focused && i == toggle_cursor {
            '>'
        } else {
            ' '
        };
        let indent = if row.indent { "  " } else { "" };
        let mut text = format!(
            "{cursor} {indent}{} {}",
            row.state.glyph(),
            row.feature.label()
        );
        // A failed toggle shows its (sanitized) reason inline.
        if let crate::col2::ToggleState::Failed(reason) = &row.state {
            text.push_str(" — ");
            text.push_str(&sanitize_for_terminal(reason));
        }
        lines.push(DetailLine {
            kind: LineKind::Toggle(i),
            text,
        });
    }
    lines
}

/// "on"/"off" for a boolean setting.
fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

/// "captured"/"skipped" for a capture toggle.
fn cap_skip(b: bool) -> &'static str {
    if b {
        "captured"
    } else {
        "skipped"
    }
}

/// Column 3 — the six live-graph sections (M4-10): solved into a weighted
/// stack, each honest about absent data, drawn through the plot seam.
fn draw_graphs(vm: &ViewModel, backend: &mut dyn RenderBackend, frame: &mut Frame, rect: Rect) {
    let inner = frame.border(rect, "graphs", border_style(vm.focus == Panel::Graphs));
    crate::col3::draw(&vm.col3, inner, backend, frame);
}

/// Render `lines` into `area` starting at `offset` (the scroll top), clipping to
/// the area's width and height.
fn render_lines(frame: &mut Frame, area: Rect, offset: usize, lines: &[DetailLine]) {
    for (dy, line) in lines.iter().skip(offset).enumerate() {
        let Ok(y) = u16::try_from(dy) else { break };
        if y >= area.h {
            break;
        }
        clip_line(frame, area, y, &line.text, Style::DEFAULT);
    }
}

/// Print `text` on row `dy` of `area`, truncated to the area's width and dropped
/// when it falls below the area — so a panel never bleeds into its neighbor.
fn clip_line(frame: &mut Frame, area: Rect, dy: u16, text: &str, style: Style) {
    if dy >= area.h {
        return;
    }
    let max = usize::from(area.w);
    let clipped: String = text.chars().take(max).collect();
    frame.put_str(area.x, area.y + dy, &clipped, style);
}

/// Print `text` at `(x, y)`, clipped to `width` cells.
fn clip_at(frame: &mut Frame, x: u16, y: u16, width: u16, text: &str, style: Style) {
    let clipped: String = text.chars().take(usize::from(width)).collect();
    frame.put_str(x, y, &clipped, style);
}

/// Truncate `s` to at most `w` display cells, appending an ellipsis when it is cut.
fn elide(s: &str, w: usize) -> String {
    let count = s.chars().count();
    if count <= w {
        return s.to_string();
    }
    match w {
        0 => String::new(),
        1 => "…".to_string(),
        _ => {
            let mut out: String = s.chars().take(w - 1).collect();
            out.push('…');
            out
        }
    }
}

/// The visible `[start, end)` slice of a `len`-item list of `height` rows that
/// keeps `selected` in view (scroll-to-selection). Deterministic in its inputs.
/// Shared with [`crate::hit`] so a click resolves to the same project row the
/// renderer drew.
pub(crate) fn visible_window(selected: usize, len: usize, height: usize) -> (usize, usize) {
    if height == 0 || len == 0 {
        return (0, 0);
    }
    let h = height.min(len);
    // Pin so `selected` stays within the window; clamp the top to the last page.
    let start = selected.saturating_sub(h - 1).min(len - h);
    (start, start + h)
}

/// A short human label for a project's live status.
pub(crate) fn status_label(status: ProjectStatus) -> &'static str {
    match status {
        ProjectStatus::Running { .. } => "running",
        ProjectStatus::PickingUp => "picking up",
        ProjectStatus::Idle => "idle",
        ProjectStatus::NotInitialized => "not initialized",
        ProjectStatus::Error(_) => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;
    use crate::state::{update, AppState, Event, ProjectData};
    use crate::{
        CaptureSummary, DetailView, EntryKind, ModelUsage, Project, ProjectStatus, SettingsSummary,
    };

    fn vm() -> ViewModel {
        let mut s = AppState::new(
            vec![
                Project {
                    name: "rift-cli".into(),
                    path: "/r/rift-cli".into(),
                    status: ProjectStatus::Running {
                        since_ms: 0,
                        active_sessions: 1,
                    },
                    kind: EntryKind::Repo,
                },
                Project {
                    name: "rift-site".into(),
                    path: "/r/rift-site".into(),
                    status: ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
            ],
            Size::new(120, 40),
        );
        s.app.splash.skip();
        s.snapshot()
    }

    fn render_to_text(vm: &ViewModel) -> String {
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(vm, &mut b, &mut f);
        let mut out = String::new();
        for y in 0..f.height() {
            for x in 0..f.width() {
                out.push(f.get(x, y).map_or(' ', |c| c.ch));
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn draws_three_panels_and_command_bar() {
        let text = render_to_text(&vm());
        assert!(text.contains("RIFT — Canopy"));
        assert!(text.contains("projects"));
        assert!(text.contains("rift-cli"));
        assert!(text.contains("detail"));
        assert!(text.contains("graphs"));
        // The six col-3 sections render their honest placeholders (M4-10) —
        // the old fake-zero savings/tokens lines are gone.
        assert!(text.contains("token usage"));
        assert!(text.contains("no token activity yet"));
        assert!(text.contains("recall savings"));
        // The col-2 context bar is retired (ADR-0012): the col-3 nic-gauge
        // section owns context now.
        assert!(text.contains("nic-gauge"));
    }

    #[test]
    fn wizard_when_empty() {
        let mut s = AppState::new(vec![], Size::new(100, 30));
        s.app.splash.skip();
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("No projects registered"));
    }

    #[test]
    fn add_project_bar_shows_path() {
        let mut s = AppState::new(vec![], Size::new(100, 30));
        s.app.splash.skip();
        s.app.mode = Mode::AddProject;
        s.app.command_line = "/tmp/repo".into();
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("/tmp/repo"));
        assert!(text.contains("add project"));
    }

    #[test]
    fn focused_panel_border_is_highlighted() {
        use crate::render::frame::Color;
        use crate::Panel;
        let accent = Color::Rgb(0x6c, 0xe0, 0x9a);

        let mut s = AppState::new(
            vec![Project {
                name: "p".into(),
                path: "/r/p".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(120, 40),
        );
        s.app.splash.skip();
        s.app.focus = Panel::Projects;
        let mut f = Frame::new(s.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&s.snapshot(), &mut b, &mut f);

        // col1's top-left corner sits at (0, 1), just below the title row.
        let c1 = f.get(0, 1).unwrap();
        assert_eq!(c1.ch, '╭');
        assert_eq!(
            c1.style.fg, accent,
            "the focused panel's border is accented"
        );

        // At width 120 col1 is 24 wide, so col2's border begins at x = 24.
        let c2 = f.get(24, 1).unwrap();
        assert_eq!(c2.ch, '╭');
        assert_ne!(
            c2.style.fg, accent,
            "an unfocused panel's border is not accented"
        );
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (2, 3), (5, 2)] {
            let mut s = AppState::new(vec![], Size::new(w, h));
            s.app.splash.skip();
            let mut f = Frame::new(Size::new(w, h));
            let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
            draw(&s.snapshot(), &mut b, &mut f); // must not panic
        }
    }

    /// A populated state at the given size, past the splash.
    fn sized(w: u16, h: u16) -> AppState {
        let mut s = AppState::new(
            vec![
                Project {
                    name: "alpha".into(),
                    path: "/r/alpha".into(),
                    status: ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
                Project {
                    name: "beta".into(),
                    path: "/r/beta".into(),
                    status: ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
            ],
            Size::new(w, h),
        );
        s.app.splash.skip();
        s
    }

    #[test]
    fn two_col_stacks_detail_over_graphs() {
        assert_eq!(choose_mode(Size::new(60, 20)), LayoutMode::TwoCol);
        let text = render_to_text(&sized(60, 20).snapshot());
        assert!(text.contains("projects"));
        let detail_row = text.lines().position(|l| l.contains("detail"));
        let graphs_row = text.lines().position(|l| l.contains("graphs"));
        assert!(
            detail_row < graphs_row && detail_row.is_some(),
            "detail sits above graphs in two-column mode ({detail_row:?} < {graphs_row:?})"
        );
    }

    #[test]
    fn single_panel_tab_switches_active() {
        assert_eq!(choose_mode(Size::new(40, 16)), LayoutMode::SinglePanel);
        let mut s = sized(40, 16);

        s.app.focus = Panel::Detail;
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("[Detail]"), "focused tab is bracketed");
        // The detail panel leads with the selected project's header (its status).
        assert!(text.contains("status:"), "the detail panel is shown");

        s.app.focus = Panel::Graphs;
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("[Graphs]"));
        assert!(text.contains("token usage"), "the graphs panel is shown");
    }

    #[test]
    fn too_small_shows_notice_and_recovers() {
        let text = render_to_text(&sized(20, 5).snapshot());
        assert!(text.contains("too small"), "the notice is shown when tiny");
        // Enlarging recovers the full dashboard.
        let text = render_to_text(&sized(120, 40).snapshot());
        assert!(!text.contains("too small"));
        assert!(text.contains("projects"));
    }

    #[test]
    fn long_names_truncate_with_ellipsis() {
        let mut s = sized(120, 40);
        s.app.projects = vec![Project {
            name: "a-really-long-project-name-that-cannot-possibly-fit".into(),
            path: "/r/x".into(),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }];
        let text = render_to_text(&s.snapshot());
        assert!(text.contains('…'), "an over-long label is ellipsized");
    }

    /// A single-project state whose selected project's rich detail carries `model`.
    fn state_with_detail(status: ProjectStatus, model: &str) -> AppState {
        let mut s = AppState::new(
            vec![Project {
                name: "p".into(),
                path: "/r/p".into(),
                status,
                kind: EntryKind::Repo,
            }],
            Size::new(120, 40),
        );
        s.app.splash.skip();
        update(
            &mut s,
            Event::ProjectData(ProjectData {
                project: "/r/p".into(),
                status,
                detail: Some(DetailView {
                    path: "/r/p".into(),
                    label: "p".into(),
                    kind: EntryKind::Repo,
                    status,
                    models: vec![ModelUsage {
                        model: model.into(),
                        calls: 3,
                    }],
                    settings: SettingsSummary::default(),
                    capture: CaptureSummary::default(),
                    toggles: crate::col2::ToggleView::default(),
                }),
            }),
        );
        s
    }

    #[test]
    fn model_name_with_ansi_escape_is_sanitized() {
        // A hostile model name carrying an SGR escape must not reach any cell as a
        // control character — the ESC is stripped, the inert text remains.
        let mut s = state_with_detail(ProjectStatus::Idle, "evil\x1b[31mmodel");
        let mut f = Frame::new(s.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&s.snapshot(), &mut b, &mut f);
        for y in 0..f.height() {
            for x in 0..f.width() {
                if let Some(cell) = f.get(x, y) {
                    assert!(
                        !cell.ch.is_control(),
                        "cell ({x},{y}) holds a control char {:?}",
                        cell.ch
                    );
                }
            }
        }
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("evil[31mmodel"), "the inert text survives");
    }

    #[test]
    fn uninitialized_project_shows_init_hint() {
        let mut s = state_with_detail(ProjectStatus::NotInitialized, "unused");
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("not initialized"));
        assert!(text.contains("rift init"));
    }

    #[test]
    fn detail_renders_sections_when_ready() {
        let mut s = state_with_detail(ProjectStatus::Idle, "claude-opus");
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("models"));
        assert!(text.contains("claude-opus"));
        assert!(text.contains("settings"));
        assert!(text.contains("capture"));
    }

    proptest::proptest! {
        /// Every terminal size yields a renderable layout — no panic, at any size.
        #[test]
        fn every_size_yields_a_renderable_layout(w in 0u16..400, h in 0u16..400) {
            let mut s = sized(w, h);
            let mut f = Frame::new(Size::new(w, h));
            let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
            draw(&s.snapshot(), &mut b, &mut f); // must not panic at any size
        }

        /// No arbitrary detail string (model name) ever leaves a control character
        /// in a rendered cell — the sanitize gate holds for any bytes.
        #[test]
        fn no_control_chars_in_any_cell_for_arbitrary_model(model in ".*") {
            let mut s = state_with_detail(ProjectStatus::Idle, &model);
            let mut f = Frame::new(s.size);
            let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
            draw(&s.snapshot(), &mut b, &mut f);
            for y in 0..f.height() {
                for x in 0..f.width() {
                    if let Some(cell) = f.get(x, y) {
                        proptest::prop_assert!(!cell.ch.is_control());
                    }
                }
            }
        }
    }
}
