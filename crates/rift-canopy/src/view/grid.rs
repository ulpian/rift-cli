//! The responsive grid: the full three-column tiling from the layout model,
//! the two-column and single-panel degraded tiers, the panel-editor grips,
//! the tab strip, the command bar, the card stack and the live panels — and
//! the overflow scrollbar every scrolling region draws on its right edge.

use crate::col2::sanitize_for_terminal;
use crate::layout::{single_panel, two_col, Rect, MIN_NOTICE_COLS, MIN_NOTICE_ROWS};
use crate::layout_model::ColumnRole;
use crate::render::frame::{Frame, Style};
use crate::render::{Capabilities, RenderBackend};
use crate::state::ViewModel;
use crate::{Mode, Panel};

use super::rail::draw_projects;
use super::text::{clip_at, clip_line};
use super::ACCENT;

/// The full three-column grid, tiled from the data-driven layout model
/// (the data-driven layout model). Each physical column renders by its role — the default model
/// places the rail, cards, and panels in columns 1/2/3 at 20/40/40, so this is
/// byte-identical to the shipped grid; a rearranged model moves and
/// resizes the columns and this same loop draws them where the model says.
pub(super) fn draw_full(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    body: Rect,
    unicode: bool,
) {
    if vm.mode != Mode::PanelEditor {
        super::dashboard::draw(vm, backend, frame, unicode);
        return;
    }
    let sl = crate::layout_model::solve_model(&vm.layout, body);
    let editing = vm.mode == crate::input::Mode::PanelEditor;
    for col in &sl.columns {
        if col.rect.h < 2 || col.rect.w == 0 {
            continue;
        }
        // In edit mode every column renders through the one per-unit path, so the
        // grips (computed from the same `solve_column`) sit exactly on the drawn
        // units. In normal mode the homogeneous columns keep their richer stacks
        // (scroll, scrollbars) — byte-identical to before.
        if editing || col.role() == ColumnRole::Mixed {
            draw_column_units(vm, backend, frame, col.rect, &col.units, unicode);
        } else {
            match col.role() {
                ColumnRole::Rail => draw_projects(vm, frame, col.rect, unicode),
                ColumnRole::Cards => {
                    draw_detail(vm, backend, frame, col.rect, unicode, &col.card_kinds());
                }
                ColumnRole::Sections => {
                    draw_graphs(vm, backend, frame, col.rect, &col.section_ids());
                }
                ColumnRole::Mixed | ColumnRole::Empty => {}
            }
        }
    }
    // In panel-editor mode, overlay grips + column dividers over the columns.
    if editing {
        draw_editor_grips(vm, frame, &sl, unicode);
    }
    crate::bars::draw_hint_bar(vm, vm.theme, frame, sl.hint);
    draw_command(vm, frame, sl.command);
}

/// The two-column degraded grid: projects left, detail over graphs right.
pub(super) fn draw_two_col(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    body: Rect,
    unicode: bool,
) {
    if vm.mode != Mode::PanelEditor {
        super::dashboard::draw(vm, backend, frame, unicode);
        return;
    }
    let r = two_col(body);
    draw_projects(vm, frame, r.col1, unicode);
    draw_detail(
        vm,
        backend,
        frame,
        r.col2,
        unicode,
        &crate::cards::CardKind::ALL,
    );
    draw_graphs(vm, backend, frame, r.col3, &crate::col3::SectionId::ALL);
    crate::bars::draw_hint_bar(vm, vm.theme, frame, r.hint);
    draw_command(vm, frame, r.command);
}

/// The single-panel degraded layout: a tab strip + the focused panel.
pub(super) fn draw_single(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    body: Rect,
    unicode: bool,
) {
    if vm.mode != Mode::PanelEditor {
        super::dashboard::draw(vm, backend, frame, unicode);
        return;
    }
    let (tabs, panel, hint, command) = single_panel(body);
    draw_tab_strip(vm, frame, tabs);
    match vm.focus {
        Panel::Detail => draw_detail(
            vm,
            backend,
            frame,
            panel,
            unicode,
            &crate::cards::CardKind::ALL,
        ),
        Panel::Graphs => draw_graphs(vm, backend, frame, panel, &crate::col3::SectionId::ALL),
        // Projects (and Command focus) show the projects panel.
        Panel::Projects | Panel::Command => draw_projects(vm, frame, panel, unicode),
    }
    crate::bars::draw_hint_bar(vm, vm.theme, frame, hint);
    draw_command(vm, frame, command);
}

/// The centered "terminal too small" notice.
pub(super) fn draw_too_small(frame: &mut Frame, area: Rect) {
    let msg = format!("Terminal too small (need ≥ {MIN_NOTICE_COLS}×{MIN_NOTICE_ROWS})");
    let len = u16::try_from(msg.chars().count()).unwrap_or(u16::MAX);
    let x = area.x + area.w.saturating_sub(len) / 2;
    let y = area.y + area.h / 2;
    clip_at(frame, x, y, area.w, &msg, Style::DEFAULT);
}

/// Draw the bottom command / add-project bar (mode-aware), with a block cursor.
/// Restyled as chrome: themed border + title, the prompt in the
/// key-hint accent, the input in body text.
pub(crate) fn draw_command(vm: &ViewModel, frame: &mut Frame, command: Rect) {
    if command.h == 0 {
        return;
    }
    let (btitle, prefix) = match vm.mode {
        Mode::AddProject => ("add project (path)", ""),
        _ => ("command", "/"),
    };
    let focused = vm.focus == Panel::Command;
    let inner = crate::chrome::panel(
        frame,
        command,
        btitle,
        vm.theme.border(crate::theme::Section::Command, focused),
        vm.theme.title(crate::theme::Section::Command),
    );
    let px = frame.put_str(inner.x, inner.y, prefix, vm.theme.key_hint());
    let line = format!("{}\u{2588}", vm.command_line);
    let room = (inner.x + inner.w).saturating_sub(px);
    let clipped: String = line.chars().take(usize::from(room)).collect();
    frame.put_str(px, inner.y, &clipped, vm.theme.body());
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

/// Draw an overflow scrollbar on `area`'s right edge when its content
/// exceeds the viewport, tracking `(offset, content_len)`. Chrome — the bar's
/// presence honestly means "there is more"; a fitting region draws nothing.
/// Returns whether a bar was drawn (so the caller can reserve for it / hit-test).
pub(super) fn draw_overflow_bar(
    frame: &mut Frame,
    area: Rect,
    offset: usize,
    content_len: usize,
    theme: crate::theme::Theme,
    unicode: bool,
) -> bool {
    if let Some(bar) = crate::chrome::scrollbar(area, offset, content_len) {
        crate::chrome::draw_scrollbar(frame, &bar, theme, unicode);
        true
    } else {
        false
    }
}

/// Draw a column's `units` in order through the one shared per-unit path — the
/// grid's edit-mode render and the mixed-column render. Each unit
/// draws via its own `draw_one`; the geometry is the shared `solve_column`, so
/// the grips (same solver) sit exactly on the drawn units.
fn draw_column_units(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    rect: Rect,
    units: &[crate::layout_model::Unit],
    unicode: bool,
) {
    let (slots, _) = crate::layout_model::solve_column(units, rect, 0);
    for slot in &slots {
        draw_unit(
            vm,
            backend,
            frame,
            slot.unit,
            slot.rect,
            slot.collapsed,
            unicode,
        );
    }
}

/// Draw one unit into `rect` — the rail, a card, or a panel (panel-editor mode).
fn draw_unit(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    unit: crate::layout_model::Unit,
    rect: Rect,
    collapsed: bool,
    unicode: bool,
) {
    use crate::layout_model::Unit;
    match unit {
        Unit::Rail => draw_projects(vm, frame, rect, unicode),
        Unit::Card(k) => {
            if let Some(detail) = vm.detail.as_deref() {
                let ctx = crate::cards::CardCtx {
                    detail,
                    activity: &vm.activity,
                    focused: vm.focus == Panel::Detail,
                    unicode,
                    pressed: vm.pressed.clone(),
                    press_pulse: vm.press_pulse,
                };
                crate::cards::draw_one(&ctx, vm.theme, k, rect, collapsed, backend, frame);
            }
        }
        Unit::Section(s) => crate::col3::draw_one(
            &vm.col3,
            s,
            vm.focus == Panel::Graphs,
            unicode,
            collapsed,
            rect,
            backend,
            frame,
        ),
    }
}

/// Draw the panel-editor grips: a grip glyph on each unit's top border
/// row (accent-washed; the cursor's unit and a picked unit stand out) and a
/// highlighted divider on each shared column border. Grips are computed from the
/// same `solve_column` the edit-mode render uses, so they sit on the drawn units.
/// Colour + one glyph only, so the paint-never-moves-a-lit-cell property holds.
fn draw_editor_grips(
    vm: &ViewModel,
    frame: &mut Frame,
    sl: &crate::layout_model::SolvedLayout,
    unicode: bool,
) {
    let grip = if unicode { '⣿' } else { ':' };
    for (ci, col) in sl.columns.iter().enumerate() {
        // The right divider (between this column and the next) — a highlighted bar.
        if ci + 1 < sl.columns.len() && col.rect.w > 0 {
            let x = col.rect.x + col.rect.w.saturating_sub(1);
            for dy in 0..col.rect.h {
                frame.set(x, col.rect.y + dy, if unicode { '┃' } else { '|' }, ACCENT);
            }
        }
        let (slots, _) = crate::layout_model::solve_column(&col.units, col.rect, 0);
        for (ui, slot) in slots.iter().enumerate() {
            if slot.rect.w == 0 || slot.rect.h == 0 {
                continue;
            }
            let is_cursor = ci == vm.editor.cursor.col && ui == vm.editor.cursor.unit;
            let style = if is_cursor && vm.editor.picked {
                vm.theme.selection()
            } else if is_cursor {
                ACCENT
            } else {
                vm.theme.dim()
            };
            // The grip sits at the unit's top-left border corner (chrome, never a
            // data cell — the border glyph is already there).
            frame.set(slot.rect.x, slot.rect.y, grip, style);
            // A short handle run to read as a bar, clipped to the border width.
            let run = slot.rect.w.min(4);
            for dx in 1..run {
                frame.set(slot.rect.x + dx, slot.rect.y, grip, style);
            }
        }
    }
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

/// Column 2 — the card stack: repository · models · activity, each
/// individually bordered and themed, sized by the
/// shared stack solver. Every projection-derived string is escape-sanitized in
/// [`crate::cards`] before it can reach a cell (the security model). No project
/// selected → a single placeholder line.
pub(super) fn draw_detail(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    rect: Rect,
    unicode: bool,
    units: &[crate::cards::CardKind],
) {
    let Some(detail) = vm.detail.as_deref() else {
        clip_line(frame, rect, 0, "no project selected", vm.theme.dim());
        return;
    };
    let ctx = crate::cards::CardCtx {
        detail,
        activity: &vm.activity,
        focused: vm.focus == Panel::Detail,
        unicode,
        pressed: vm.pressed.clone(),
        press_pulse: vm.press_pulse,
    };
    crate::cards::draw(
        &ctx,
        vm.theme,
        units,
        rect,
        vm.detail_scroll,
        backend,
        frame,
    );
    // The card-stack overflow scrollbar: shown only when the column is
    // too short to host every card (the collapse-solve's lossy tier — cards are
    // one row each there, so the row/card counts line up).
    draw_overflow_bar(
        frame,
        rect,
        vm.detail_scroll,
        crate::cards::content_len(units),
        vm.theme,
        unicode,
    );
}

/// Column 3 — the live panels: token usage, system (the donut + domain
/// metrics), recall, and the two workflow cards — each an individually-bordered themed
/// card filling the column, solved by the shared stack. No outer border (like
/// the column-2 cards); focus accents the panel borders.
pub(super) fn draw_graphs(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    rect: Rect,
    ids: &[crate::col3::SectionId],
) {
    let unicode = backend.capabilities().contains(Capabilities::UNICODE);
    crate::col3::draw(
        &vm.col3,
        ids,
        vm.focus == Panel::Graphs,
        unicode,
        rect,
        vm.graphs_scroll,
        backend,
        frame,
    );
    // The overflow scrollbar: drawn on the column's right edge only
    // when the fully-expanded panels exceed the column — the visible sign of the
    // "j/k scroll" the hint bar teaches.
    draw_overflow_bar(
        frame,
        rect,
        vm.graphs_scroll,
        usize::from(crate::col3::content_rows(ids)),
        vm.theme,
        unicode,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{choose_mode, LayoutMode};
    use crate::render::frame::Size;
    use crate::state::AppState;
    use crate::view::draw;
    use crate::view::fixtures::{render_to_text, sized, state_with_detail, vm};
    use crate::{EntryKind, Project, ProjectStatus};

    #[test]
    fn draws_home_boxes_tabs_and_command_bar() {
        let text = render_to_text(&vm());
        for label in [
            "RIFT",
            "Canopy",
            "? Help",
            "q Quit",
            "[Home]",
            "Models",
            "Activity",
            "Token usage",
            "Recall viewer",
            "Settings",
            "rift-cli",
            "repository",
            "recall",
            "command",
            "← Projects",
        ] {
            assert!(text.contains(label), "missing {label}: {text}");
        }
        for retired in ["─ projects ", "─ dispatch ", "─ recap ", "─ system "] {
            assert!(
                !text.contains(retired),
                "retired permanent panel: {retired}"
            );
        }
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
        use crate::Panel;
        let accent = crate::theme::Section::Repository.accent();

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
        s.app.focus = Panel::Detail;
        let mut f = Frame::new(s.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&s.snapshot(), &mut b, &mut f);

        // The focused Repository card uses its section accent.
        let repo = crate::dashboard::home_rects(s.size).0;
        let c1 = f.get(repo.x, repo.y).unwrap();
        assert_eq!(c1.ch, '╭');
        assert_eq!(
            c1.style.fg, accent,
            "the focused panel's border is accented"
        );

        // Recall is unfocused and retains the idle border tone.
        let recall = crate::dashboard::home_recall_content(s.size);
        let c2 = f.get(recall.x, recall.y).unwrap();
        assert_eq!(c2.ch, '╭');
        assert_eq!(
            c2.style.fg,
            s.snapshot().theme.border_idle(),
            "an unfocused panel's border uses the idle tone"
        );
    }

    #[test]
    fn medium_home_stacks_repository_above_recall() {
        assert_eq!(choose_mode(Size::new(60, 20)), LayoutMode::TwoCol);
        let text = render_to_text(&sized(60, 20).snapshot());
        let repository = text
            .lines()
            .position(|line| line.contains("repository") || line.contains("· idle"));
        let recall = text.lines().position(|line| line.contains("recall"));
        assert!(
            repository.is_some() && recall.is_some() && repository < recall,
            "repository must precede Recall: {text}"
        );
        assert!(!text.contains("─ projects "));
    }

    #[test]
    fn narrow_tabs_switch_the_visible_content() {
        assert_eq!(choose_mode(Size::new(40, 16)), LayoutMode::SinglePanel);
        let mut s = sized(40, 16);
        s.app.dashboard_tab = crate::dashboard::DashboardTab::Models;
        let models = render_to_text(&s.snapshot());
        assert!(
            models.contains("[Models]"),
            "active tab is visible: {models}"
        );
        assert!(models.contains("models"));
        s.app.dashboard_tab = crate::dashboard::DashboardTab::Tokens;
        s.app.set_focus(crate::Panel::Graphs);
        let tokens = render_to_text(&s.snapshot());
        assert!(
            tokens.contains("[Tokens]"),
            "active tab is visible: {tokens}"
        );
        assert!(tokens.contains("no token activity"));
        assert!(!tokens.contains("[Models]"));
    }

    #[test]
    fn too_small_shows_notice_and_recovers() {
        let text = render_to_text(&sized(20, 5).snapshot());
        assert!(text.contains("too small"), "the notice is shown when tiny");
        // Enlarging recovers the full dashboard.
        let text = render_to_text(&sized(120, 40).snapshot());
        assert!(!text.contains("too small"));
        assert!(text.contains("repository"));
    }

    #[test]
    fn model_name_with_ansi_escape_is_sanitized() {
        // A hostile model name carrying an SGR escape must not reach any cell as a
        // control character — the ESC is stripped, the inert text remains.
        let mut s = state_with_detail(ProjectStatus::Idle, "evil\x1b[31mmodel");
        s.app.dashboard_tab = crate::dashboard::DashboardTab::Models;
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
    fn uninitialized_project_shows_project_settings_entry_point() {
        let mut s = state_with_detail(ProjectStatus::NotInitialized, "unused");
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("not initialized"));
        assert!(text.contains("Project Settings"));
    }

    #[test]
    fn detail_renders_sections_when_ready() {
        let mut s = state_with_detail(ProjectStatus::Idle, "claude-opus");
        s.app.dashboard_tab = crate::dashboard::DashboardTab::Models;
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("models"));
        assert!(text.contains("claude-opus"));
        s.app.dashboard_tab = crate::dashboard::DashboardTab::Activity;
        let activity = render_to_text(&s.snapshot());
        assert!(activity.contains("activity"));
        assert!(!text.contains("│ settings"));
        assert!(!text.contains("│ features"));
        assert!(!text.contains("│ integrations"));
        // The retired capture card draws no title. Matched on the card's
        // border-and-title shape, not the bare word: the dispatch panel's
        // "no capture projection here" placeholder contains it too, and an
        // assertion those two states cannot tell apart proves nothing.
        assert!(
            !text.contains("- capture -"),
            "the capture card was retired from the stack: {text}"
        );
    }

    proptest::proptest! {
        /// No arbitrary detail string (model name) ever leaves a control character
        /// in a rendered cell — the sanitize gate holds for any bytes.
        #[test]
        fn no_control_chars_in_any_cell_for_arbitrary_model(model in "(?s).*") {
            let mut s = state_with_detail(ProjectStatus::Idle, &model);
        s.app.dashboard_tab = crate::dashboard::DashboardTab::Models;
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
