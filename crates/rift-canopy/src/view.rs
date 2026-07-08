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
    bottom_chrome_rows, choose_mode, chrome_split, single_panel, solve, two_col, LayoutMode, Rect,
    MIN_NOTICE_COLS, MIN_NOTICE_ROWS,
};
use crate::render::frame::{Frame, Style};
use crate::render::{Capabilities, RenderBackend};
use crate::state::ViewModel;
use crate::theme::Theme;
use crate::{EntryKind, Mode, Panel};

/// The Rift accent style (selection + overlay chrome) — the theme's accent.
const ACCENT: Style = Style::fg(crate::theme::ACCENT);

/// The one surface theme (ADR-0023) — code, not config.
const THEME: Theme = Theme::rift_dark();

/// Render the whole dashboard for `vm` into `frame`, choosing the responsive
/// layout tier for the current size (P3 S2). `backend` participates through
/// the plot seam (column-3 graphs) and, once at the top, the Unicode
/// capability — the single glyph-substitution switch the theme's icon table
/// resolves through (draw code never branches on capability elsewhere).
pub fn draw(vm: &ViewModel, backend: &mut dyn RenderBackend, frame: &mut Frame) {
    let area = frame.rect();
    if area.w == 0 || area.h == 0 {
        return;
    }
    let unicode = backend.capabilities().contains(Capabilities::UNICODE);

    // Below the minimum, show only the centered notice (no header/panels).
    if choose_mode(vm.size) == LayoutMode::TooSmall {
        draw_too_small(frame, area);
        return;
    }

    let header = Rect {
        x: area.x,
        y: area.y,
        w: area.w,
        h: 1,
    };
    crate::bars::draw_header(vm, THEME, unicode, frame, header);
    let body = Rect {
        x: area.x,
        y: area.y + 1,
        w: area.w,
        h: area.h.saturating_sub(1),
    };

    if vm.show_wizard {
        let (above, hint, command) = chrome_split(body);
        draw_wizard(vm, frame, above);
        crate::bars::draw_hint_bar(vm, THEME, frame, hint);
        draw_command(vm, frame, command);
        return;
    }

    match choose_mode(vm.size) {
        LayoutMode::TwoCol => draw_two_col(vm, backend, frame, body, unicode),
        LayoutMode::SinglePanel => draw_single(vm, backend, frame, body, unicode),
        // Full3Col (TooSmall handled above).
        _ => draw_full(vm, backend, frame, body, unicode),
    }

    // The suggest panel expands ABOVE the command bar, borrowing body rows as
    // an overlay draw — the fixed command rows and the layout solver are
    // untouched (M5-11); it clamps away on tiny terminals.
    if !vm.suggestions.is_empty() {
        draw_suggest_panel(vm, frame);
    }

    // The dispatch overlay layers over the body (ADR-0021 "Dispatch posture"):
    // results scroll, confirms gate, the running line holds during a dispatch.
    // It sizes itself as a centered modal (ADR-0026) from `vm.size`.
    if vm.overlay.is_some() {
        draw_overlay(vm, frame);
    }

    // The one-shot startup sweep (M55-14): a top-to-bottom reveal as the panels
    // first fill after the splash. At 1.0 (reduced motion / headless / settled)
    // it is a no-op; while sweeping, rows below the reveal line are blanked so
    // the dashboard wipes in rather than popping. Pure dt-tween — the settled
    // frame (which every headless/snapshot render sees) is always the full one.
    apply_startup_sweep(vm, frame, body);
}

/// Blank the body rows below the sweep's reveal line (a no-op at rest).
fn apply_startup_sweep(vm: &ViewModel, frame: &mut Frame, body: Rect) {
    let sweep = vm.sweep.clamp(0.0, 1.0);
    if sweep >= 1.0 || body.h == 0 {
        return;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let revealed = (f32::from(body.h) * sweep) as u16;
    for dy in revealed..body.h {
        for dx in 0..body.w {
            frame.set(body.x + dx, body.y + dy, ' ', Style::DEFAULT);
        }
    }
}

/// The suggest panel's row cap (a bounded list; adapts down to the body).
pub const SUGGEST_MAX_ROWS: usize = 8;

/// Where the suggest panel sits: directly above the bottom chrome (hint bar +
/// command bar), bounded by [`SUGGEST_MAX_ROWS`], the suggestion count, and
/// the body's spare rows — zero-sized when there are none (the panel clamps
/// away; the bar's own line remains). Shared with `hit_test` so clicks
/// resolve to the drawn rows.
#[must_use]
pub fn suggest_panel_rect(size: crate::render::frame::Size, rows: usize) -> Rect {
    let body = Rect {
        x: 0,
        y: 1,
        w: size.w,
        h: size.h.saturating_sub(1),
    };
    let chrome = bottom_chrome_rows(body.h);
    let avail = body.h.saturating_sub(chrome + 1);
    let want = u16::try_from(rows.min(SUGGEST_MAX_ROWS))
        .unwrap_or(0)
        .saturating_add(2);
    let h = want.min(avail);
    if h < 3 || body.w < 12 {
        return Rect {
            x: body.x,
            y: body.y,
            w: 0,
            h: 0,
        };
    }
    let y = body
        .y
        .saturating_add(body.h)
        .saturating_sub(chrome)
        .saturating_sub(h);
    Rect {
        x: body.x,
        y,
        w: body.w,
        h,
    }
}

/// Draw the suggest panel: `verb  usage` + a muted one-line description per
/// row, the highlight bar, and the ADR-0022 fade — unselected text ramps from
/// the muted tone to the settled foreground as `suggest_fade` advances
/// (reduced motion and ASCII land at 1.0, snapshotting the final frame).
/// Every string passes the shared sanitizer (labels are user-controlled).
fn draw_suggest_panel(vm: &ViewModel, frame: &mut Frame) {
    let rect = suggest_panel_rect(vm.size, vm.suggestions.len());
    if rect.h == 0 {
        return;
    }
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            frame.set(rect.x + dx, rect.y + dy, ' ', Style::DEFAULT);
        }
    }
    let inner = frame.border(rect, "suggest", ACCENT);
    let t = vm.suggest_fade.clamp(0.0, 1.0);
    let text_fg = crate::theme::lerp_color(crate::theme::MUTED, crate::theme::BODY_FG, t);
    let visible = usize::from(inner.h);
    for (i, sug) in vm.suggestions.iter().take(visible).enumerate() {
        let Ok(dy) = u16::try_from(i) else { break };
        let selected = i == vm.suggest_highlight;
        let marker = if selected { '>' } else { ' ' };
        let line = format!(
            "{marker} {:<30} {}",
            sanitize_for_terminal(&format!("{}  {}", sug.insert, sug.usage)),
            sanitize_for_terminal(sug.description)
        );
        let style = if selected { ACCENT } else { Style::fg(text_fg) };
        clip_line(frame, inner, dy, &line, style);
    }
}

/// The smallest readable modal box (ADR-0026): a modal clamps *up* to this even
/// when 80% of a small terminal would be fewer cells.
pub const MIN_MODAL_COLS: u16 = 40;
/// The smallest readable modal box height — see [`MIN_MODAL_COLS`].
pub const MIN_MODAL_ROWS: u16 = 12;

/// Where a modal sits (ADR-0026): a box at 80% × 80% of the terminal, clamped
/// *up* to the readable minimum and *down* to the region between the 1-row
/// header and the bottom chrome (hint + command bars) — so those bars always
/// stay visible while modals come and go — then centered in that region. On the
/// narrow `SinglePanel`/`TooSmall` tiers it degrades to near-full-screen so its
/// content stays usable. This is the single source of truth for both
/// [`draw_overlay`] and the pure hit-test, so a resize can never desync a
/// modal's pixels from its click targets.
#[must_use]
pub fn modal_rect(size: crate::render::frame::Size) -> Rect {
    // The region a modal may occupy: below the header, above the bottom chrome.
    let body_h = size.h.saturating_sub(1);
    let avail_y = 1u16.min(size.h);
    let avail_h = body_h.saturating_sub(bottom_chrome_rows(body_h));
    let avail_w = size.w;

    // Target = 80% of the terminal (95% near-full on the narrow tiers), clamped
    // up to the readable minimum and down to the region so chrome stays put.
    let pct: u32 = if matches!(
        choose_mode(size),
        LayoutMode::SinglePanel | LayoutMode::TooSmall
    ) {
        95
    } else {
        80
    };
    let want_w = u16::try_from(u32::from(size.w) * pct / 100).unwrap_or(size.w);
    let want_h = u16::try_from(u32::from(size.h) * pct / 100).unwrap_or(size.h);
    let w = want_w.max(MIN_MODAL_COLS).min(avail_w);
    let h = want_h.max(MIN_MODAL_ROWS).min(avail_h);

    // Center within the available region.
    let x = avail_w.saturating_sub(w) / 2;
    let y = avail_y.saturating_add(avail_h.saturating_sub(h) / 2);
    Rect { x, y, w, h }
}

/// The confirm overlay's two button labels (shared with hit-testing).
pub const CONFIRM_BUTTONS: (&str, &str) = ("[ Enter: confirm ]", "[ Esc: cancel ]");

/// Dim the dashboard behind an open modal (ADR-0026): a themed background wash
/// over every cell outside `modal`, glyphs kept so the body reads as *behind*
/// the modal rather than gone. Colour-only, so the capability tiers drop it to
/// no-dim on mono / ASCII / `NO_COLOR` — there the modal's border alone
/// separates it. The modal owns input while open, so the dimmed body is inert.
fn draw_scrim(frame: &mut Frame, modal: Rect) {
    let area = frame.rect();
    let modal_bottom = modal.y.saturating_add(modal.h);
    let modal_right = modal.x.saturating_add(modal.w);
    for y in area.y..area.y.saturating_add(area.h) {
        let in_modal_rows = y >= modal.y && y < modal_bottom;
        for x in area.x..area.x.saturating_add(area.w) {
            if in_modal_rows && x >= modal.x && x < modal_right {
                continue; // the modal paints its own rect below
            }
            if let Some(cell) = frame.get(x, y) {
                frame.set(x, y, cell.ch, cell.style.with_bg(crate::theme::SCRIM_BG));
            }
        }
    }
}

/// The clickable close `[x]` on a modal's top-right border (ADR-0027) — one
/// shared source for the draw and the hit-test, so the glyph and its click
/// target are the same cells. `None` when the modal is too narrow to host it.
#[must_use]
pub fn modal_close_rect(modal: Rect) -> Option<Rect> {
    (modal.w >= 8).then(|| Rect {
        x: modal.x + modal.w - 4,
        y: modal.y,
        w: 3,
        h: 1,
    })
}

/// The style for a modal button (ADR-0027/0028): the accent, washed with the
/// focus background when the focus ring rests on it, and the press-flash
/// background (which wins) while it is pressed. Colour-only, so the mono/ASCII
/// tiers keep the label glyphs and drop the wash.
fn overlay_button_style(
    vm: &ViewModel,
    target: &crate::hit::Target,
    control: crate::dispatch::OverlayControl,
) -> Style {
    if let Some(bg) = crate::chrome::press_flash_bg(vm.button_flash(target)) {
        ACCENT.with_bg(bg)
    } else if vm.focused_overlay_control() == Some(control) {
        ACCENT.with_bg(crate::theme::SELECT_BG)
    } else {
        ACCENT
    }
}

/// Draw the dispatch overlay as a centered modal (ADR-0026): a themed scrim over
/// the dashboard, then a bordered, scrollable pane. Geometry comes from the one
/// shared [`modal_rect`], so pixels and hit-test targets can never disagree.
fn draw_overlay(vm: &ViewModel, frame: &mut Frame) {
    use crate::dispatch::Overlay;
    let Some(overlay) = &vm.overlay else { return };
    let rect = modal_rect(vm.size);
    if rect.w < 8 || rect.h < 3 {
        return; // clamped away on tiny terminals; the bar still works
    }
    draw_scrim(frame, rect);
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
            // Confirm / Cancel as two focus-ring buttons (ADR-0027): the focused
            // one carries the focus wash, a pressed one the flash (ADR-0028).
            // The gap (3) and columns mirror the hit-test exactly.
            let brow = inner.y + 3;
            let confirm_w = u16::try_from(CONFIRM_BUTTONS.0.chars().count()).unwrap_or(0);
            frame.put_str(
                inner.x,
                brow,
                CONFIRM_BUTTONS.0,
                overlay_button_style(
                    vm,
                    &crate::hit::Target::OverlayConfirm,
                    crate::dispatch::OverlayControl::Confirm,
                ),
            );
            let cancel_x = inner.x.saturating_add(confirm_w).saturating_add(3);
            frame.put_str(
                cancel_x,
                brow,
                CONFIRM_BUTTONS.1,
                overlay_button_style(
                    vm,
                    &crate::hit::Target::OverlayCancel,
                    crate::dispatch::OverlayControl::Cancel,
                ),
            );
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
    // The universal close X on the top-right border (ADR-0027), for modals that
    // carry a Close control (Results / Confirm; Running has none). The focus
    // wash marks it when the ring rests on Close; the flash when it is pressed.
    if overlay
        .controls()
        .contains(&crate::dispatch::OverlayControl::Close)
    {
        if let Some(cr) = modal_close_rect(rect) {
            let style = overlay_button_style(
                vm,
                &crate::hit::Target::OverlayClose,
                crate::dispatch::OverlayControl::Close,
            );
            frame.put_str(cr.x, cr.y, "[x]", style);
        }
    }
}

/// The full 20/40/40 three-column grid.
fn draw_full(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    body: Rect,
    unicode: bool,
) {
    let r = solve(body);
    if r.col1.h >= 2 {
        draw_projects(vm, frame, r.col1, unicode);
        draw_detail(vm, backend, frame, r.col2, unicode);
        draw_graphs(vm, backend, frame, r.col3);
    }
    crate::bars::draw_hint_bar(vm, THEME, frame, r.hint);
    draw_command(vm, frame, r.command);
}

/// The two-column degraded grid: projects left, detail over graphs right.
fn draw_two_col(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    body: Rect,
    unicode: bool,
) {
    let r = two_col(body);
    draw_projects(vm, frame, r.col1, unicode);
    draw_detail(vm, backend, frame, r.col2, unicode);
    draw_graphs(vm, backend, frame, r.col3);
    crate::bars::draw_hint_bar(vm, THEME, frame, r.hint);
    draw_command(vm, frame, r.command);
}

/// The single-panel degraded layout: a tab strip + the focused panel.
fn draw_single(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    body: Rect,
    unicode: bool,
) {
    let (tabs, panel, hint, command) = single_panel(body);
    draw_tab_strip(vm, frame, tabs);
    match vm.focus {
        Panel::Detail => draw_detail(vm, backend, frame, panel, unicode),
        Panel::Graphs => draw_graphs(vm, backend, frame, panel),
        // Projects (and Command focus) show the projects panel.
        Panel::Projects | Panel::Command => draw_projects(vm, frame, panel, unicode),
    }
    crate::bars::draw_hint_bar(vm, THEME, frame, hint);
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

/// The border style for a panel: accented when focused, the theme's idle
/// slate otherwise (M55-04 — the borders join the surface theme).
fn border_style(focused: bool) -> Style {
    if focused {
        ACCENT
    } else {
        Style::fg(crate::theme::BORDER_IDLE)
    }
}

/// Draw the bottom command / add-project bar (mode-aware), with a block cursor.
/// Restyled as chrome (M55-04): themed border + title, the prompt in the
/// key-hint accent, the input in body text.
fn draw_command(vm: &ViewModel, frame: &mut Frame, command: Rect) {
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
        THEME.border(crate::theme::Section::Command, focused),
        THEME.title(crate::theme::Section::Command),
    );
    let px = frame.put_str(inner.x, inner.y, prefix, THEME.key_hint());
    let line = format!("{}\u{2588}", vm.command_line);
    let room = (inner.x + inner.w).saturating_sub(px);
    let clipped: String = line.chars().take(usize::from(room)).collect();
    frame.put_str(px, inner.y, &clipped, THEME.body());
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

/// Column 1 — the projects rail (M55-05): count badge in the title, per-row
/// kind icon + label + trailing status dot, the selection as a filled pill.
/// Windowed to keep the selection in view; every label passes the sanitizer.
fn draw_projects(vm: &ViewModel, frame: &mut Frame, rect: Rect, unicode: bool) {
    use crate::theme::{Icon, Section};
    let focused = vm.focus == Panel::Projects;
    let inner = crate::chrome::panel(
        frame,
        rect,
        "projects",
        THEME.border(Section::Projects, focused),
        THEME.title(Section::Projects),
    );
    // The registered-project count as a badge on the title row, after the title
    // (user story 8: the count is a fact you see, not a scroll you take).
    if rect.w >= 16 && rect.h >= 2 {
        let bx = rect.x + 12; // just past ` projects ` (title starts at x+3)
        crate::chrome::badge(
            frame,
            bx,
            rect.y,
            &vm.projects.len().to_string(),
            THEME.badge(Section::Projects.accent()),
        );
    }
    if vm.projects.is_empty() {
        clip_line(frame, inner, 0, "No projects yet — press 'a'", THEME.body());
        return;
    }
    let (start, end) = visible_window(vm.selected, vm.projects.len(), usize::from(inner.h));
    // `> ▣ label … ●` — cursor, kind icon, label, trailing status dot.
    let label_w = usize::from(inner.w).saturating_sub(6);
    for (row, p) in vm.projects.iter().enumerate().take(end).skip(start) {
        let Ok(dy) = u16::try_from(row.saturating_sub(start)) else {
            break;
        };
        let y = inner.y + dy;
        let selected = row == vm.selected;
        // The selection pill: the row filled in the selection wash (BOLD keeps
        // it visible on the colourless floors, beside the `>` marker).
        let (row_style, marker) = if selected {
            // The selection pulse (M55-14): the pill flashes toward the accent
            // on a cursor move, then decays to the resting selection wash.
            let pill = if vm.select_pulse > 0.0 {
                let bg = crate::theme::lerp_color(
                    crate::theme::ACCENT,
                    crate::theme::SELECT_BG,
                    1.0 - vm.select_pulse.clamp(0.0, 1.0),
                );
                THEME.selection().with_bg(bg)
            } else {
                THEME.selection()
            };
            crate::chrome::fill(
                frame,
                Rect {
                    x: inner.x,
                    y,
                    w: inner.w,
                    h: 1,
                },
                pill,
            );
            (pill, '>')
        } else {
            (THEME.body(), ' ')
        };
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
        frame.set(inner.x, y, marker, row_style);
        let kind_style = if selected { row_style } else { THEME.dim() };
        let mut x = frame.put_str(inner.x + 1, y, indent, row_style);
        frame.set(x, y, Icon::for_kind(p.kind).glyph(unicode), kind_style);
        x = x.saturating_add(1);
        frame.put_str(x + 1, y, &label, row_style);
        // The trailing status dot, pinned to the right edge in status colour.
        if inner.w >= 2 {
            let dot_x = inner.x + inner.w - 1;
            let mut dot = THEME.status_dot(p.status);
            if selected {
                dot = dot.with_bg(crate::theme::SELECT_BG);
            }
            frame.set(dot_x, y, Icon::for_status(p.status).glyph(unicode), dot);
        }
    }
}

/// Column 2 — the card stack (M55-08): repository · models · settings · capture
/// · features · activity, each individually bordered and themed, sized by the
/// shared stack solver. Every projection-derived string is escape-sanitized in
/// [`crate::cards`] before it can reach a cell (`05-security`). No project
/// selected → a single placeholder line.
fn draw_detail(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    rect: Rect,
    unicode: bool,
) {
    let Some(detail) = vm.detail.as_deref() else {
        clip_line(frame, rect, 0, "no project selected", THEME.dim());
        return;
    };
    let ctx = crate::cards::CardCtx {
        detail,
        activity: &vm.activity,
        runtime: &vm.feature_runtime,
        toggle_cursor: vm.toggle_cursor,
        focused: vm.focus == Panel::Detail,
        unicode,
    };
    crate::cards::draw(&ctx, THEME, rect, backend, frame);
}

/// Column 3 — the three live panels (M55-11/12/13): token usage, system (the
/// donut + domain metrics), recall — each an individually-bordered themed
/// card filling the column, solved by the shared stack. No outer border (like
/// the column-2 cards); focus accents the panel borders.
fn draw_graphs(vm: &ViewModel, backend: &mut dyn RenderBackend, frame: &mut Frame, rect: Rect) {
    let unicode = backend.capabilities().contains(Capabilities::UNICODE);
    crate::col3::draw(
        &vm.col3,
        vm.focus == Panel::Graphs,
        unicode,
        rect,
        backend,
        frame,
    );
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
        // The M55-04 header: mark + product + the right-aligned actions.
        assert!(text.contains("RIFT"));
        assert!(text.contains("Canopy"));
        assert!(text.contains("? Help"));
        assert!(text.contains("q Quit"));
        assert!(text.contains("projects"));
        assert!(text.contains("rift-cli"));
        // Column 2 is the M55-08 card stack, not a single "detail" panel.
        assert!(text.contains("repository"));
        assert!(text.contains("capture"));
        // The hint bar teaches the focused (projects) keymap.
        assert!(text.contains("j/k navigate"));
        // Column 3 regroups into the M55-11/12/13 three panels, each honest
        // about absent data (never a fake zero).
        assert!(text.contains("token usage"));
        assert!(text.contains("no token activity yet"));
        assert!(text.contains("system"));
        assert!(text.contains("recall"));
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
    fn rail_shows_count_badge_status_dots_and_selection_pill() {
        use crate::render::frame::Color;
        use crate::theme::{status_color, SELECT_BG};
        let mut s = AppState::new(
            vec![
                Project {
                    name: "alpha".into(),
                    path: "/r/alpha".into(),
                    status: ProjectStatus::Running {
                        since_ms: 0,
                        active_sessions: 1,
                    },
                    kind: EntryKind::Repo,
                },
                Project {
                    name: "beta".into(),
                    path: "/r/beta".into(),
                    status: ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
            ],
            Size::new(120, 40),
        );
        s.app.splash.skip();
        let mut f = Frame::new(s.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&s.snapshot(), &mut b, &mut f);

        // The count badge shows "2" on the title row (col1 spans x=0..24).
        let title: String = (0..24).map(|x| f.get(x, 1).unwrap().ch).collect();
        assert!(title.contains('2'), "the project count badge: {title}");

        // Row 0 of the rail (inner y=2) is the selected `alpha` — its trailing
        // status dot is running-green, and the pill wash tints the row.
        let dot_x = 22; // col1 inner right edge (width 24 → inner 1..23)
        assert_eq!(
            f.get(dot_x, 2).unwrap().style.fg,
            status_color(ProjectStatus::Running {
                since_ms: 0,
                active_sessions: 1
            }),
            "the running dot is green"
        );
        assert_eq!(
            f.get(2, 2).unwrap().style.bg,
            SELECT_BG,
            "the selected row carries the pill wash"
        );
        // Row 1 (`beta`, unselected) has no pill wash.
        assert_eq!(f.get(2, 3).unwrap().style.bg, Color::Default);
        // …and its idle dot is the idle slate.
        assert_eq!(
            f.get(dot_x, 3).unwrap().style.fg,
            status_color(ProjectStatus::Idle)
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
        // Both columns are short here and collapse to headlines — the card
        // stack's `<name> · <status>` sits above the col-3 panels' placeholder.
        let detail_row = text.lines().position(|l| l.contains("· idle"));
        let graphs_row = text.lines().position(|l| l.contains("no token activity"));
        assert!(
            detail_row < graphs_row && detail_row.is_some(),
            "the card stack sits above graphs in two-column mode ({detail_row:?} < {graphs_row:?})"
        );
    }

    #[test]
    fn single_panel_tab_switches_active() {
        assert_eq!(choose_mode(Size::new(40, 16)), LayoutMode::SinglePanel);
        let mut s = sized(40, 16);

        s.app.focus = Panel::Detail;
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("[Detail]"), "focused tab is bracketed");
        // The detail column is the card stack; the (collapsed) repository card
        // headline shows the project name + status.
        assert!(text.contains("· idle"), "the card stack is shown");

        s.app.focus = Panel::Graphs;
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("[Graphs]"));
        // The col-3 panels are short here → collapsed to the token headline.
        assert!(
            text.contains("no token activity"),
            "the graphs panel is shown"
        );
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

    #[test]
    fn modal_is_centered_and_insets_the_dashboard() {
        // At a full-grid size the modal is 80% × 80%, centered, and clear of the
        // 1-row header and the bottom chrome (ADR-0026) — an inset box, not the
        // whole screen.
        let r = modal_rect(Size::new(120, 40));
        assert_eq!((r.x, r.y, r.w, r.h), (12, 2, 96, 32));
    }

    #[test]
    fn modal_degrades_to_near_full_on_narrow_tiers() {
        // On the narrow SinglePanel tier the modal widens past the 80% grid
        // fraction toward near-full-screen so its content stays usable.
        let size = Size::new(44, 30); // SinglePanel: MIN_COL1 <= w < MIN_COL1+MIN_COL2
        assert_eq!(
            crate::layout::choose_mode(size),
            crate::layout::LayoutMode::SinglePanel
        );
        let r = modal_rect(size);
        assert!(r.w > size.w * 80 / 100, "wider than the 80% grid modal");
    }

    proptest::proptest! {
        /// The modal rect (ADR-0026) is always within the frame, never covers the
        /// header row, and clears the bottom chrome — at every size above the
        /// too-small floor — so the header and command bars always stay put and
        /// the draw/hit-test share one rect that can never leave the screen.
        #[test]
        fn modal_rect_fits_and_clears_chrome(w in 24u16..400, h in 6u16..400) {
            let size = Size::new(w, h);
            proptest::prop_assume!(
                crate::layout::choose_mode(size) != crate::layout::LayoutMode::TooSmall
            );
            let r = modal_rect(size);
            proptest::prop_assert!(r.x + r.w <= size.w, "within the frame width");
            proptest::prop_assert!(r.y >= 1, "never covers the header row");
            let chrome = crate::layout::bottom_chrome_rows(size.h - 1);
            proptest::prop_assert!(r.y + r.h <= size.h - chrome, "clears the bottom chrome");
        }

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
