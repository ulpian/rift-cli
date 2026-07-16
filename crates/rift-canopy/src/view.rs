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
    bottom_chrome_rows, choose_mode, single_panel, two_col, LayoutMode, Rect, MIN_NOTICE_COLS,
    MIN_NOTICE_ROWS,
};
use crate::layout_model::ColumnRole;
use crate::render::frame::{Frame, Style};
use crate::render::{Capabilities, RenderBackend};
use crate::state::ViewModel;
use crate::{EntryKind, Mode, Panel};

/// The Rift accent style (selection + overlay chrome) — the theme's accent.
const ACCENT: Style = Style::fg(crate::theme::ACCENT);

/// The one surface theme (ADR-0023) — code, not config.
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
    crate::bars::draw_header(vm, vm.theme, unicode, frame, header);
    let body = Rect {
        x: area.x,
        y: area.y + 1,
        w: area.w,
        h: area.h.saturating_sub(1),
    };

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

    // The profile menu (ADR-0037): a mode, not an overlay — a centered frame
    // over the shared modal geometry and scrim, its state on the snapshot.
    if vm.mode == crate::input::Mode::ProfileMenu {
        draw_profile_menu(vm, unicode, frame);
    }

    // The colour picker (ADR-0039): two stages over the same modal geometry;
    // the dashboard beneath is the live preview.
    if vm.mode == crate::input::Mode::ColourPicker {
        draw_colour_picker(vm, unicode, frame);
    }

    // The dispatch overlay layers over the body (ADR-0021 "Dispatch posture"):
    // results scroll, confirms gate, the running line holds during a dispatch.
    // It sizes itself as a centered modal (ADR-0026) from `vm.size`.
    if vm.overlay.is_some() {
        draw_overlay(vm, unicode, frame);
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
    let text_fg = crate::theme::lerp_color(crate::theme::MUTED, vm.theme.body_fg(), t);
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

/// The primary-action chip string for a message modal (`[ Add a project ]`), or
/// `None` for a dismiss-only message. One source for draw + measurement, so the
/// drawn cells and the hit-test width agree.
fn primary_chip(action: crate::dispatch::MessageAction) -> Option<String> {
    action.primary_label().map(|label| format!("[ {label} ]"))
}

/// The primary-action button rect on a message modal (ADR-0029) — bottom-left of
/// the modal's inner content, shared by [`draw_overlay`] and the hit-test so the
/// chip cells and the click target are the same cells. `None` for a dismiss-only
/// message or a modal too small to host it.
#[must_use]
pub fn modal_primary_rect(inner: Rect, action: crate::dispatch::MessageAction) -> Option<Rect> {
    let chip = primary_chip(action)?;
    if inner.w == 0 || inner.h == 0 {
        return None;
    }
    let w = u16::try_from(chip.chars().count())
        .unwrap_or(0)
        .min(inner.w);
    Some(Rect {
        x: inner.x,
        y: inner.y + inner.h - 1,
        w,
        h: 1,
    })
}

/// The profile menu's row rect for profile index `i` (ADR-0037): one full-width
/// row per live profile, starting one row below the modal's inner top. One
/// source for the renderer and the hit-test, so a click lands exactly on the
/// drawn row; zero-sized when the row would overrun the frame's usable body
/// (the bottom rows are reserved for the input/confirm line).
#[must_use]
pub fn profile_menu_row_rect(modal: Rect, i: usize) -> Rect {
    let empty = Rect {
        x: modal.x,
        y: modal.y,
        w: 0,
        h: 0,
    };
    if modal.w < 4 || modal.h < 5 {
        return empty;
    }
    let inner = Rect {
        x: modal.x + 1,
        y: modal.y + 1,
        w: modal.w - 2,
        h: modal.h - 2,
    };
    let Ok(dy) = u16::try_from(i + 1) else {
        return empty;
    };
    // The last two inner rows host the naming/confirm/hint line and a gap.
    if dy >= inner.h.saturating_sub(2) {
        return empty;
    }
    Rect {
        x: inner.x,
        y: inner.y + dy,
        w: inner.w,
        h: 1,
    }
}

/// The `[settings]` chip's rect on a hub row (ADR-0044 §4) — right-aligned
/// inside the row, one source for the draw and the hit-test. Zero-sized when
/// the row is (or is too narrow to host a chip beside a name).
#[must_use]
pub fn profile_menu_settings_rect(modal: Rect, i: usize) -> Rect {
    let rr = profile_menu_row_rect(modal, i);
    let chip_w = u16::try_from(PROFILE_SETTINGS_CHIP.chars().count()).unwrap_or(0);
    if rr.h == 0 || rr.w < chip_w.saturating_add(20) {
        return Rect {
            x: rr.x,
            y: rr.y,
            w: 0,
            h: 0,
        };
    }
    Rect {
        x: rr.x + rr.w - chip_w - 1,
        y: rr.y,
        w: chip_w,
        h: 1,
    }
}

/// The hub's `[settings]` chip text (one source for draw + measurement).
pub const PROFILE_SETTINGS_CHIP: &str = "[settings]";

/// The hub's trailing `[ + new profile ]` row text.
pub const PROFILE_NEW_ROW: &str = " [ + new profile ]";

/// A mode-modal's close `[x]` style: press-flash aware, muted at rest — the
/// pointer twin of `Esc` (ADR-0046; the target closes the MODE, never an
/// overlay).
fn menu_close_style(vm: &ViewModel, target: &crate::hit::Target) -> Style {
    let bg = crate::chrome::press_flash_bg(vm.theme, vm.button_flash(target))
        .unwrap_or(vm.theme.surface_bg());
    Style::fg(crate::theme::MUTED).with_bg(bg)
}

/// Draw the profile menu (ADR-0037, hub-and-spoke per ADR-0044): the shared
/// centered modal frame + scrim and, per stage — the hub (live profiles with
/// `[settings]` chips and the `[ + new profile ]` row), the per-profile
/// settings page, a naming input (create / rename / duplicate, with the
/// inline name-rule error), or the inline delete confirm. Every stage carries
/// the close `[x]` (ADR-0046). Pure drawing over the snapshot; every name
/// passes the shared sanitizer.
fn draw_profile_menu(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
    use crate::profile_menu::{MenuStage, SettingsRow};
    let rect = modal_rect(vm.size);
    if rect.w < 8 || rect.h < 5 {
        return;
    }
    draw_scrim(frame, rect);
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            frame.set(rect.x + dx, rect.y + dy, ' ', Style::DEFAULT);
        }
    }
    let settings_profile = match &vm.profile_menu.stage {
        MenuStage::Settings { profile } => Some(profile.clone()),
        _ => None,
    };
    let title = settings_profile
        .as_deref()
        .and_then(|id| vm.profiles.iter().find(|p| p.profile_id == id))
        .map_or_else(
            || "profiles".to_string(),
            |p| format!("profile · {}", sanitize_for_terminal(&p.name)),
        );
    let inner = frame.border(rect, &title, ACCENT);
    // The close affordance, on every stage (ADR-0046 §2): the pointer twin of
    // Esc, resolving to the MODE's own close target.
    if let Some(cr) = modal_close_rect(rect) {
        let style = menu_close_style(vm, &crate::hit::Target::ProfileMenuClose);
        frame.put_str(cr.x, cr.y, "[x]", style);
    }

    match &vm.profile_menu.stage {
        MenuStage::Settings { profile } => {
            // The settings page: one activatable row per SettingsRow::ALL.
            for (i, row) in SettingsRow::ALL.iter().enumerate() {
                let rr = profile_menu_row_rect(rect, i);
                if rr.h == 0 {
                    break;
                }
                let on_cursor = i == vm.profile_menu.settings_cursor;
                let suffix = match row {
                    SettingsRow::StartupPin => {
                        if vm.startup_profile.as_deref() == Some(profile.as_str()) {
                            "  [on]"
                        } else {
                            "  [off]"
                        }
                    }
                    _ => "",
                };
                let line = format!(" {}{suffix}", row.label());
                let style = if on_cursor {
                    Style::fg(vm.theme.body_fg()).with_bg(crate::theme::SELECT_BG)
                } else if matches!(row, SettingsRow::Delete) {
                    severity_style(crate::dispatch::Severity::Warning)
                } else {
                    Style::fg(vm.theme.body_fg())
                };
                if on_cursor {
                    for dx in 0..rr.w {
                        frame.set(rr.x + dx, rr.y, ' ', style);
                    }
                }
                clip_line(frame, inner, rr.y - inner.y, &line, style);
            }
        }
        _ => {
            // The hub: the live profiles, each with its `[settings]` chip,
            // then the `[ + new profile ]` row.
            for (i, p) in vm.profiles.iter().enumerate() {
                let rr = profile_menu_row_rect(rect, i);
                if rr.h == 0 {
                    break; // the frame is full; the rest clip away whole
                }
                let is_active = p.profile_id == vm.active_profile;
                let on_cursor = i == vm.profile_menu.cursor
                    && matches!(vm.profile_menu.stage, MenuStage::Browse);
                let marker = if is_active {
                    crate::theme::Icon::Profile.glyph(unicode)
                } else {
                    ' '
                };
                let line = format!(" {marker} {}", sanitize_for_terminal(&p.name));
                let style = if on_cursor {
                    Style::fg(vm.theme.body_fg()).with_bg(crate::theme::SELECT_BG)
                } else if is_active {
                    ACCENT
                } else {
                    Style::fg(vm.theme.body_fg())
                };
                if on_cursor {
                    for dx in 0..rr.w {
                        frame.set(rr.x + dx, rr.y, ' ', style);
                    }
                }
                clip_line(frame, inner, rr.y - inner.y, &line, style);
                // The row's settings chip — a click twin for `s` (ADR-0046).
                let cr = profile_menu_settings_rect(rect, i);
                if cr.w > 0 {
                    let target = crate::hit::Target::ProfileSettings(i);
                    let bg = crate::chrome::press_flash_bg(vm.theme, vm.button_flash(&target))
                        .or_else(|| on_cursor.then_some(crate::theme::SELECT_BG))
                        .unwrap_or(vm.theme.surface_bg());
                    frame.put_str(
                        cr.x,
                        cr.y,
                        PROFILE_SETTINGS_CHIP,
                        Style::fg(crate::theme::MUTED).with_bg(bg),
                    );
                }
            }
            // The `[ + new profile ]` row — the click twin for `n`.
            let new_rr = profile_menu_row_rect(rect, vm.profiles.len());
            if new_rr.h > 0 {
                let target = crate::hit::Target::ProfileNew;
                let bg = crate::chrome::press_flash_bg(vm.theme, vm.button_flash(&target))
                    .unwrap_or(vm.theme.surface_bg());
                clip_line(
                    frame,
                    inner,
                    new_rr.y - inner.y,
                    PROFILE_NEW_ROW,
                    Style::fg(crate::theme::MUTED).with_bg(bg),
                );
            }
        }
    }

    // The stage line, on the inner bottom row: stage hints, the naming input,
    // or the inline delete confirm (an explicit second act — ADR-0037 §3).
    if inner.h == 0 {
        return;
    }
    let stage_dy = inner.h - 1;
    let muted = Style::fg(crate::theme::MUTED);
    match &vm.profile_menu.stage {
        MenuStage::Browse => {
            clip_line(
                frame,
                inner,
                stage_dy,
                " ⏎ switch · s settings · n new · d delete · esc close",
                muted,
            );
        }
        MenuStage::Settings { .. } => {
            clip_line(frame, inner, stage_dy, " ⏎ open · esc back", muted);
        }
        MenuStage::NamingCreate
        | MenuStage::NamingRename { .. }
        | MenuStage::NamingDuplicate { .. } => {
            let verb = match &vm.profile_menu.stage {
                MenuStage::NamingCreate => "new profile",
                MenuStage::NamingDuplicate { .. } => "duplicate as",
                _ => "rename to",
            };
            let line = format!(" {verb}: {}▏", sanitize_for_terminal(&vm.command_line));
            clip_line(frame, inner, stage_dy, &line, ACCENT);
            if vm.profile_menu.name_error && stage_dy > 0 {
                clip_line(
                    frame,
                    inner,
                    stage_dy - 1,
                    " name must be 1–24 chars and unique",
                    severity_style(crate::dispatch::Severity::Warning),
                );
            }
        }
        MenuStage::ConfirmDelete { profile, .. } => {
            let name = vm
                .profiles
                .iter()
                .find(|p| &p.profile_id == profile)
                .map(|p| sanitize_for_terminal(&p.name))
                .unwrap_or_default();
            let line = format!(" delete {name}? ⏎ confirm · esc cancel");
            clip_line(
                frame,
                inner,
                stage_dy,
                &line,
                severity_style(crate::dispatch::Severity::Warning),
            );
        }
    }
}

/// The colour picker's row rect for row index `i` (ADR-0039) — the same shape
/// as [`profile_menu_row_rect`] (one source for draw + hit), reused so the two
/// modal surfaces cannot drift apart.
#[must_use]
pub fn colour_picker_row_rect(modal: Rect, i: usize) -> Rect {
    profile_menu_row_rect(modal, i)
}

/// The number of rows the picker's current stage lists (slots, or presets +
/// the reset row) — shared by the renderer and the hit-test.
#[must_use]
pub fn colour_picker_rows(stage: crate::colour_picker::PickerStage) -> usize {
    match stage {
        crate::colour_picker::PickerStage::Slots => crate::theme::ThemeSlot::ALL.len(),
        crate::colour_picker::PickerStage::Presets => crate::theme::PRESETS.len() + 1,
    }
}

/// Draw the colour picker (ADR-0039): stage one lists the five slots with each
/// slot's current swatch (or `default`); stage two lists the ten presets —
/// name beside a solid swatch box — plus the reset row. The swatch is
/// colour-only, so on mono/ASCII the name alone carries the choice. Pure
/// drawing over the snapshot; the dashboard behind the scrim is the preview.
fn draw_colour_picker(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
    use crate::colour_picker::{PickerStage, RESET_ROW};
    let rect = modal_rect(vm.size);
    if rect.w < 8 || rect.h < 5 {
        return;
    }
    draw_scrim(frame, rect);
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            frame.set(rect.x + dx, rect.y + dy, ' ', Style::DEFAULT);
        }
    }
    let title = match vm.colour_picker.stage {
        PickerStage::Slots => "colours".to_string(),
        PickerStage::Presets => format!("colours · {}", vm.colour_picker.slot().key()),
    };
    let inner = frame.border(rect, &title, ACCENT);
    let swatch = if unicode { "██" } else { "##" };

    match vm.colour_picker.stage {
        PickerStage::Slots => {
            for (i, slot) in crate::theme::ThemeSlot::ALL.iter().enumerate() {
                let rr = colour_picker_row_rect(rect, i);
                if rr.h == 0 {
                    break;
                }
                let on_cursor = i == vm.colour_picker.slot_cursor;
                let style = if on_cursor {
                    Style::fg(vm.theme.body_fg()).with_bg(crate::theme::SELECT_BG)
                } else {
                    Style::fg(vm.theme.body_fg())
                };
                if on_cursor {
                    for dx in 0..rr.w {
                        frame.set(rr.x + dx, rr.y, ' ', style);
                    }
                }
                let x = frame.put_str(rr.x, rr.y, &format!(" {:<12}", slot.key()), style);
                // The slot's current value: a swatch in its override, or the
                // word `default` — honest about what the slot holds today.
                match vm.theme.overrides().get(*slot) {
                    Some(c) => {
                        frame.put_str(x, rr.y, swatch, Style::fg(c).with_bg(style.bg));
                    }
                    None => {
                        frame.put_str(x, rr.y, "default", vm.theme.dim().with_bg(style.bg));
                    }
                }
            }
        }
        PickerStage::Presets => {
            for i in 0..=RESET_ROW {
                let rr = colour_picker_row_rect(rect, i);
                if rr.h == 0 {
                    break;
                }
                let on_cursor = i == vm.colour_picker.preset_cursor;
                let style = if on_cursor {
                    Style::fg(vm.theme.body_fg()).with_bg(crate::theme::SELECT_BG)
                } else {
                    Style::fg(vm.theme.body_fg())
                };
                if on_cursor {
                    for dx in 0..rr.w {
                        frame.set(rr.x + dx, rr.y, ' ', style);
                    }
                }
                match crate::theme::PRESETS.get(i) {
                    // A preset row: the name beside a box of the colour itself.
                    Some((name, colour)) => {
                        let x = frame.put_str(rr.x, rr.y, &format!(" {name:<8}"), style);
                        frame.put_str(x, rr.y, swatch, Style::fg(*colour).with_bg(style.bg));
                    }
                    // The reset row sits beside the presets (ADR-0039 §2).
                    None => {
                        frame.put_str(rr.x, rr.y, " reset to default", style);
                    }
                }
            }
        }
    }

    if inner.h > 0 {
        let hint = match vm.colour_picker.stage {
            PickerStage::Slots => " ⏎ choose · esc menu",
            PickerStage::Presets => " ⏎ apply (live) · esc slots",
        };
        clip_line(
            frame,
            inner,
            inner.h - 1,
            hint,
            Style::fg(crate::theme::MUTED),
        );
    }
}

/// A message severity's frame colour (ADR-0029): red error, amber warning, green
/// success, accent info. Colour-only, so the mono/ASCII tiers drop it and the
/// border glyphs stand — the one styling axis decided at draw, not per call site.
fn severity_style(severity: crate::dispatch::Severity) -> Style {
    use crate::dispatch::Severity;
    Style::fg(match severity {
        Severity::Error => crate::theme::STATUS_ERROR,
        Severity::Warning => crate::theme::HEALTH_WARN,
        Severity::Success | Severity::Info => crate::theme::ACCENT,
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
    if let Some(bg) = crate::chrome::press_flash_bg(vm.theme, vm.button_flash(target)) {
        ACCENT.with_bg(bg)
    } else if vm.focused_overlay_control() == Some(control) {
        ACCENT.with_bg(crate::theme::SELECT_BG)
    } else {
        ACCENT
    }
}

/// Draw a message modal (ADR-0029) into `rect`: the severity-coloured frame, the
/// sanitized body lines, and — for a message with a primary action — the
/// bottom-left action chip (the wizard's *add project*). The universal close X
/// is drawn by the shared tail of [`draw_overlay`].
fn draw_message(vm: &ViewModel, frame: &mut Frame, rect: Rect, msg: &crate::dispatch::Message) {
    let style = severity_style(msg.severity);
    let inner = frame.border(rect, &sanitize_for_terminal(&msg.title), style);
    for (i, line) in msg.body.iter().enumerate() {
        let Ok(dy) = u16::try_from(i) else { break };
        if dy >= inner.h {
            break;
        }
        clip_line(
            frame,
            inner,
            dy,
            &sanitize_for_terminal(line),
            Style::DEFAULT,
        );
    }
    if let (Some(chip), Some(pr)) = (
        primary_chip(msg.action),
        modal_primary_rect(inner, msg.action),
    ) {
        let bstyle = overlay_button_style(
            vm,
            &crate::hit::Target::WizardAdd,
            crate::dispatch::OverlayControl::Primary,
        );
        frame.put_str(pr.x, pr.y, &chip, bstyle);
    }
}

/// Draw a Results modal (ADR-0021): the bordered title, the scrolled body lines,
/// the nonzero-class footer, and the modal-body overflow scrollbar (ADR-0031)
/// when the listing exceeds the box.
fn draw_results(
    frame: &mut Frame,
    theme: crate::theme::Theme,
    unicode: bool,
    rect: Rect,
    title: &str,
    lines: &[String],
    scroll: usize,
    footer: Option<&str>,
) {
    let inner = frame.border(rect, &sanitize_for_terminal(title), ACCENT);
    let footer_rows = usize::from(footer.is_some());
    let visible = usize::from(inner.h).saturating_sub(footer_rows);
    for (i, line) in lines.iter().skip(scroll).take(visible).enumerate() {
        let Ok(dy) = u16::try_from(i) else { break };
        clip_line(frame, inner, dy, line, Style::DEFAULT);
    }
    if let Some(f) = footer {
        let dy = inner.h.saturating_sub(1);
        clip_line(frame, inner, dy, &sanitize_for_terminal(f), ACCENT);
    }
    // The modal-body scrollbar over the body's rows (a long listing overflows the
    // 80/80 box — ADR-0026); its presence means "there is more".
    let body = Rect {
        h: u16::try_from(visible).unwrap_or(inner.h),
        ..inner
    };
    draw_overflow_bar(frame, body, scroll, lines.len(), theme, unicode);
}

/// Draw the dispatch overlay as a centered modal (ADR-0026): a themed scrim over
/// the dashboard, then a bordered, scrollable pane. Geometry comes from the one
/// shared [`modal_rect`], so pixels and hit-test targets can never disagree.
fn draw_overlay(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
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
        } => draw_results(
            frame,
            vm.theme,
            unicode,
            rect,
            title,
            lines,
            *scroll,
            footer.as_deref(),
        ),
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
        // A message (ADR-0029): an Alert (blocking) or a Notice (auto-dismissing)
        // renders from `{ title, body, severity, action }` through one path.
        Overlay::Alert(msg) | Overlay::Notice(msg) => draw_message(vm, frame, rect, msg),
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

/// The full three-column grid, tiled from the data-driven layout model
/// (ADR-0033). Each physical column renders by its role — the default model
/// places the rail, cards, and panels in columns 1/2/3 at 20/40/40, so this is
/// byte-identical to the shipped grid; a rearranged model (M5.9) moves and
/// resizes the columns and this same loop draws them where the model says.
fn draw_full(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    body: Rect,
    unicode: bool,
) {
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

/// Draw a column's `units` in order through the one shared per-unit path — the
/// grid's edit-mode render and the mixed-column render (ADR-0033/0034). Each unit
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

/// Draw one unit into `rect` — the rail, a card, or a panel (ADR-0034).
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
                    runtime: &vm.feature_runtime,
                    toggle_cursor: vm.toggle_cursor,
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

/// Draw the panel-editor grips (ADR-0034): a grip glyph on each unit's top border
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

/// Column 1 — the projects rail (M55-05): count badge in the title, per-row
/// kind icon + label + trailing status dot, the selection as a filled pill.
/// Windowed to keep the selection in view; every label passes the sanitizer.
/// The rail's bottom action bar (M56-05): the list area (rows above the bar) and
/// the clickable `[glyph]` buttons — `+` add always, and `↑`/`↓`/`✕` (reorder
/// up/down, remove selected) when a project is selected. One shared source for
/// `draw_projects` and the hit-test, so the chips and their click targets are
/// the same cells. Returns `(list_area, buttons)`; no buttons when the rail is
/// too short to spare a row.
pub(crate) fn rail_action_layout(
    inner: Rect,
    has_selection: bool,
) -> (Rect, Vec<(Rect, crate::hit::Target)>) {
    if inner.h < 2 {
        return (inner, Vec::new());
    }
    let bar_y = inner.y + inner.h - 1;
    let list = Rect {
        h: inner.h - 1,
        ..inner
    };
    let mut targets = vec![crate::hit::Target::RailAdd];
    if has_selection {
        targets.push(crate::hit::Target::RailReorderUp);
        targets.push(crate::hit::Target::RailReorderDown);
        targets.push(crate::hit::Target::RailRemove);
    }
    // Chips abut (`[+][↑][↓][✕]`) so all four fit even in a ~14-cell rail; a
    // gap would clip the rightmost (remove) at narrow widths.
    let chip = 3u16;
    let right = inner.x + inner.w;
    let mut x = inner.x;
    let mut buttons = Vec::with_capacity(targets.len());
    for t in targets {
        if x + chip > right {
            break;
        }
        buttons.push((
            Rect {
                x,
                y: bar_y,
                w: chip,
                h: 1,
            },
            t,
        ));
        x += chip;
    }
    (list, buttons)
}

/// The glyph inside a rail action chip (M56-05); the ASCII floor substitutes.
fn rail_action_glyph(target: &crate::hit::Target, unicode: bool) -> char {
    use crate::hit::Target;
    match target {
        Target::RailAdd => '+',
        Target::RailReorderUp => {
            if unicode {
                '↑'
            } else {
                '^'
            }
        }
        Target::RailReorderDown => {
            if unicode {
                '↓'
            } else {
                'v'
            }
        }
        Target::RailRemove => {
            if unicode {
                '✕'
            } else {
                'x'
            }
        }
        _ => '?',
    }
}

/// Draw the rail action chips, each `[glyph]` — muted at rest, flashing toward
/// the accent when pressed (ADR-0028).
fn draw_rail_actions(
    vm: &ViewModel,
    frame: &mut Frame,
    buttons: &[(Rect, crate::hit::Target)],
    unicode: bool,
) {
    for (r, target) in buttons {
        let g = rail_action_glyph(target, unicode);
        let style = match crate::chrome::press_flash_bg(vm.theme, vm.button_flash(target)) {
            Some(bg) => vm.theme.dim().with_bg(bg),
            None => vm.theme.dim(),
        };
        frame.put_str(r.x, r.y, &format!("[{g}]"), style);
    }
}

fn draw_projects(vm: &ViewModel, frame: &mut Frame, rect: Rect, unicode: bool) {
    use crate::theme::{Icon, Section};
    let focused = vm.focus == Panel::Projects;
    let inner = crate::chrome::panel(
        frame,
        rect,
        "projects",
        vm.theme.border(Section::Projects, focused),
        vm.theme.title(Section::Projects),
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
            vm.theme.badge(Section::Projects.accent()),
        );
    }
    let (list, buttons) = rail_action_layout(inner, !vm.projects.is_empty());
    if vm.projects.is_empty() {
        clip_line(
            frame,
            list,
            0,
            "No projects yet — press 'a' or [+]",
            vm.theme.body(),
        );
        draw_rail_actions(vm, frame, &buttons, unicode);
        return;
    }
    let (start, end) = visible_window(vm.selected, vm.projects.len(), usize::from(list.h));
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
                vm.theme.selection().with_bg(bg)
            } else {
                vm.theme.selection()
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
            (vm.theme.body(), ' ')
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
        let kind_style = if selected { row_style } else { vm.theme.dim() };
        let mut x = frame.put_str(inner.x + 1, y, indent, row_style);
        frame.set(x, y, Icon::for_kind(p.kind).glyph(unicode), kind_style);
        x = x.saturating_add(1);
        frame.put_str(x + 1, y, &label, row_style);
        // The trailing status dot, pinned to the right edge in status colour.
        if inner.w >= 2 {
            let dot_x = inner.x + inner.w - 1;
            let mut dot = vm.theme.status_dot(p.status);
            if selected {
                dot = dot.with_bg(crate::theme::SELECT_BG);
            }
            frame.set(dot_x, y, Icon::for_status(p.status).glyph(unicode), dot);
        }
    }
    draw_rail_actions(vm, frame, &buttons, unicode);
    // The rail overflow scrollbar (ADR-0031): tracks the selection window when
    // more projects exist than the list has rows — the visual half of the
    // silent paging `visible_window` already does.
    let bar_area = Rect {
        x: rect.x,
        y: list.y,
        w: rect.w,
        h: list.h,
    };
    draw_overflow_bar(frame, bar_area, start, vm.projects.len(), vm.theme, unicode);
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
    units: &[crate::cards::CardKind],
) {
    let Some(detail) = vm.detail.as_deref() else {
        clip_line(frame, rect, 0, "no project selected", vm.theme.dim());
        return;
    };
    let ctx = crate::cards::CardCtx {
        detail,
        activity: &vm.activity,
        runtime: &vm.feature_runtime,
        toggle_cursor: vm.toggle_cursor,
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
    // The card-stack overflow scrollbar (ADR-0031): shown only when the column is
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

/// Column 3 — the three live panels (M55-11/12/13): token usage, system (the
/// donut + domain metrics), recall — each an individually-bordered themed
/// card filling the column, solved by the shared stack. No outer border (like
/// the column-2 cards); focus accents the panel borders.
fn draw_graphs(
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
    // The overflow scrollbar (ADR-0031): drawn on the column's right edge only
    // when the fully-expanded panels exceed the column — the visible sign of the
    // "j/k scroll" M57-05 wired.
    draw_overflow_bar(
        frame,
        rect,
        vm.graphs_scroll,
        usize::from(crate::col3::content_rows(ids)),
        vm.theme,
        unicode,
    );
}

/// Draw an overflow scrollbar on `area`'s right edge (ADR-0031) when its content
/// exceeds the viewport, tracking `(offset, content_len)`. Chrome — the bar's
/// presence honestly means "there is more"; a fitting region draws nothing.
/// Returns whether a bar was drawn (so the caller can reserve for it / hit-test).
fn draw_overflow_bar(
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

    /// A view-model in a picker/menu-capable state (one live profile).
    fn profiled_state() -> AppState {
        let mut s = AppState::new(
            vec![Project {
                name: "rift-cli".into(),
                path: "/r/rift-cli".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(120, 40),
        );
        s.app.splash.skip();
        s.app.set_profiles(
            vec![crate::profile::ProfileEntry {
                profile_id: "pf0".into(),
                name: "default".into(),
                created_at: 0,
            }],
            "pf0".into(),
        );
        s
    }

    #[test]
    fn profile_menu_lists_profiles_with_the_stage_line() {
        let mut s = profiled_state();
        crate::input::enter_profile_menu(&mut s.app);
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("profiles"), "the menu frame titles: {text}");
        assert!(text.contains("default"), "the live profile lists");
        assert!(text.contains("n new"), "the browse stage line teaches keys");
        // The hub's mouse affordances (ADR-0044 §4 / ADR-0046): a settings
        // chip per row, the new-profile row, and the close X.
        assert!(text.contains(PROFILE_SETTINGS_CHIP), "chips render: {text}");
        assert!(text.contains("[ + new profile ]"), "{text}");
        assert!(text.contains("[x]"), "the close X renders: {text}");
    }

    #[test]
    fn settings_page_lists_rows_with_the_pin_state() {
        use crate::profile_menu::{MenuStage, SettingsRow};
        let mut s = profiled_state();
        crate::input::enter_profile_menu(&mut s.app);
        s.app.profile_menu.stage = MenuStage::Settings {
            profile: "pf0".into(),
        };
        s.app.startup_profile = Some("pf0".into());
        let text = render_to_text(&s.snapshot());
        assert!(
            text.contains("profile · default"),
            "the frame titles the page's profile: {text}"
        );
        for row in SettingsRow::ALL {
            assert!(text.contains(row.label()), "row {row:?} renders: {text}");
        }
        assert!(
            text.contains("startup pin  [on]"),
            "the pin row shows its state: {text}"
        );
        assert!(text.contains("⏎ open · esc back"), "{text}");
        // Unpinned, the same row reads [off].
        s.app.startup_profile = None;
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("startup pin  [off]"), "{text}");
    }

    #[test]
    fn picker_lists_presets_as_name_beside_a_swatch() {
        // ADR-0039 §1: stage two is one row per preset — the NAME beside a
        // solid swatch box — plus the reset row. On the colour tiers the
        // swatch carries the preset's own RGB; on mono the name alone carries
        // (the glyphs still render; colour resolves away by construction).
        let mut s = profiled_state();
        crate::input::enter_profile_menu(&mut s.app);
        s.app.mode = crate::input::Mode::ColourPicker;
        s.app.colour_picker.stage = crate::colour_picker::PickerStage::Presets;
        let v = s.snapshot();
        let mut f = Frame::new(v.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::UNICODE);
        draw(&v, &mut b, &mut f);
        let modal = modal_rect(v.size);
        for (i, (name, colour)) in crate::theme::PRESETS.iter().enumerate() {
            let rr = colour_picker_row_rect(modal, i);
            assert!(rr.h > 0, "every preset row fits the frame");
            let row: String = (rr.x..rr.x + rr.w)
                .map(|x| f.get(x, rr.y).map_or(' ', |c| c.ch))
                .collect();
            assert!(row.contains(name), "row {i} names its preset: {row}");
            // The swatch box sits right after the padded name, in the colour.
            let sx = rr.x + 1 + 8;
            let cell = f.get(sx, rr.y).unwrap();
            assert_eq!(cell.ch, '#', "a solid swatch box (ASCII backend)");
            assert_eq!(cell.style.fg, *colour, "the swatch IS the colour");
        }
        // The reset row sits beside the presets.
        let rr = colour_picker_row_rect(modal, crate::colour_picker::RESET_ROW);
        let row: String = (rr.x..rr.x + rr.w)
            .map(|x| f.get(x, rr.y).map_or(' ', |c| c.ch))
            .collect();
        assert!(row.contains("reset to default"), "{row}");
    }

    #[test]
    fn picker_slot_stage_shows_current_values() {
        let mut state = profiled_state();
        let mut overrides = crate::theme::ThemeOverrides::NONE;
        overrides.set(
            crate::theme::ThemeSlot::Border,
            Some(crate::theme::PRESETS[1].1),
        );
        state.set_theme(overrides);
        state.app.mode = crate::input::Mode::ColourPicker;
        let view_model = state.snapshot();
        let mut frame = Frame::new(view_model.size);
        let mut backend = crate::render::AsciiBackend::new(crate::render::Capabilities::UNICODE);
        draw(&view_model, &mut backend, &mut frame);
        let modal = modal_rect(view_model.size);
        let text: String = (0..frame.height())
            .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
            .map(|(x, y)| frame.get(x, y).map_or(' ', |c| c.ch))
            .collect();
        assert!(text.contains("background"), "the slots list");
        assert!(text.contains("default"), "an unset slot reads default");
        // The overridden border slot shows a swatch in its colour.
        let rr = colour_picker_row_rect(modal, 1);
        let sx = rr.x + 1 + 12;
        let cell = frame.get(sx, rr.y).unwrap();
        assert_eq!(cell.ch, '#');
        assert_eq!(cell.style.fg, crate::theme::PRESETS[1].1);
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
    fn a_long_rail_shows_a_scrollbar_a_short_one_does_not() {
        use crate::render::frame::Size;
        // More projects than rows → the rail carries an overflow scrollbar (its
        // track/thumb glyph on the right edge); a fitting rail carries none.
        let many: Vec<Project> = (0..30)
            .map(|i| Project {
                name: format!("proj-{i}"),
                path: format!("/r/{i}"),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            })
            .collect();
        let mut s = AppState::new(many, Size::new(90, 20));
        s.app.splash.skip();
        let text = render_to_text(&s.snapshot());
        let has_bar = |t: &str| t.contains('█') || t.contains('│') || t.contains('#');
        assert!(
            has_bar(&text),
            "the overflowing rail shows a scrollbar:\n{text}"
        );

        let mut few = AppState::new(
            vec![Project {
                name: "solo".into(),
                path: "/r/solo".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(90, 20),
        );
        few.app.splash.skip();
        // The projects rail itself carries no thumb glyph when a single row fits
        // (the rail is column 1; a `█` there would be the bar). Scan column-1 rows.
        let vm = few.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&vm, &mut b, &mut f);
        let col1 = crate::layout::solve(Rect {
            x: 0,
            y: 1,
            w: 90,
            h: 19,
        })
        .col1;
        let edge = col1.x + col1.w - 1;
        for y in col1.y..(col1.y + col1.h) {
            assert_ne!(
                f.get(edge, y).map(|c| c.ch),
                Some('#'),
                "a fitting rail draws no scrollbar thumb"
            );
        }
    }

    #[test]
    fn wizard_when_empty() {
        // The first-run wizard renders as a modal (ADR-0029), with the message,
        // the clickable *add project* chip, and the universal close X.
        let mut s = AppState::new(vec![], Size::new(100, 30));
        s.app.splash.skip();
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("No projects registered"), "the message shows");
        assert!(
            text.contains("Add a project"),
            "the primary action chip shows"
        );
        assert!(text.contains("[x]"), "the universal close X shows");
    }

    #[test]
    fn severity_frames_each_message_class() {
        use crate::dispatch::Severity;
        assert_eq!(
            severity_style(Severity::Error).fg,
            crate::theme::STATUS_ERROR
        );
        assert_eq!(
            severity_style(Severity::Warning).fg,
            crate::theme::HEALTH_WARN
        );
        assert_eq!(severity_style(Severity::Success).fg, crate::theme::ACCENT);
        assert_eq!(severity_style(Severity::Info).fg, crate::theme::ACCENT);
    }

    #[test]
    fn primary_rect_is_bottom_left_only_for_an_action() {
        use crate::dispatch::MessageAction;
        let inner = Rect {
            x: 4,
            y: 2,
            w: 30,
            h: 8,
        };
        let pr = modal_primary_rect(inner, MessageAction::AddProject).expect("has a primary");
        assert_eq!(
            pr.y,
            inner.y + inner.h - 1,
            "the chip sits on the bottom row"
        );
        assert_eq!(pr.x, inner.x, "left-aligned");
        assert!(modal_primary_rect(inner, MessageAction::Dismiss).is_none());
    }

    #[test]
    fn an_error_alert_frames_the_message_in_red() {
        let mut s = AppState::new(vec![], Size::new(100, 30));
        s.app.splash.skip();
        s.open_alert(crate::dispatch::Message::error(
            "disk full",
            vec!["no space left on device".into()],
        ));
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&vm, &mut b, &mut f);
        let rect = modal_rect(vm.size);
        // The top-left border cell carries the error severity colour.
        assert_eq!(
            f.get(rect.x, rect.y).map(|c| c.style.fg),
            Some(crate::theme::STATUS_ERROR),
            "an error Alert frames red"
        );
        // A dismiss-only alert has no primary chip.
        let text = render_to_text(&vm);
        assert!(text.contains("no space left on device"));
        assert!(
            !text.contains("Add a project"),
            "no primary on a plain alert"
        );
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
