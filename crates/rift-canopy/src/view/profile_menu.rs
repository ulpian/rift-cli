//! The profile menu: the hub of live profiles with their `[settings]` chips,
//! the per-profile settings page, the members editor and its add stage, the
//! naming input and the inline delete confirm — one centered modal whose row
//! and chip solvers the hit-test shares.

use crate::col2::sanitize_for_terminal;
use crate::layout::Rect;
use crate::profile_menu::{MenuStage, SettingsRow};
use crate::render::frame::{Frame, Style};
use crate::state::ViewModel;

use super::modal::{draw_modal_backdrop, draw_mode_close, modal_rect, mode_chip_style, ModalFrame};
use super::overlay::severity_style;
use super::text::clip_line;
use super::ACCENT;

/// The hub's `[settings]` chip text (one source for draw + measurement).
pub const PROFILE_SETTINGS_CHIP: &str = "[settings]";

/// The members editor's action chips, in display order (the rail pattern:
/// chips act on the cursor row) — one source for draw, hit, and the target
/// mapping.
pub const MEMBERS_CHIPS: [&str; 4] = ["[↑]", "[↓]", "[remove]", "[ add ]"];

/// The hub's trailing `[ + new profile ]` row text.
pub const PROFILE_NEW_ROW: &str = " [ + new profile ]";

/// The names a list stage draws, with the note shown when there are none.
struct NameRows {
    names: Vec<String>,
    empty_note: &'static str,
}

/// The profile menu's row rect for profile index `i`: one full-width
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

/// The `[settings]` chip's rect on a hub row — right-aligned
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

/// The members editor's action-chip rects on its trailing row (index
/// `members_len`) — one solver for the draw and the hit-test (modal-surface parity).
/// Empty when the frame cannot host the row.
#[must_use]
pub fn members_action_chips(modal: Rect, members_len: usize) -> Vec<(Rect, usize, &'static str)> {
    let rr = profile_menu_row_rect(modal, members_len);
    if rr.h == 0 {
        return Vec::new();
    }
    let mut x = rr.x + 1;
    let mut out = Vec::with_capacity(MEMBERS_CHIPS.len());
    for (i, text) in MEMBERS_CHIPS.iter().enumerate() {
        let w = u16::try_from(text.chars().count()).unwrap_or(0);
        if x + w > rr.x + rr.w {
            break; // too narrow: trailing chips clip away whole
        }
        out.push((
            Rect {
                x,
                y: rr.y,
                w,
                h: 1,
            },
            i,
            *text,
        ));
        x += w + 1;
    }
    out
}

/// The hit target one members chip resolves to (keyed by its text, the same
/// constant the draw uses — the mapping cannot drift).
#[must_use]
pub fn members_chip_target(text: &str) -> crate::hit::Target {
    match text {
        "[↑]" => crate::hit::Target::MemberUp,
        "[↓]" => crate::hit::Target::MemberDown,
        "[remove]" => crate::hit::Target::MemberRemove,
        _ => crate::hit::Target::MemberAddOpen,
    }
}

/// Draw the profile menu (hub-and-spoke, per the profile hub): the shared
/// centered modal frame + scrim and, per stage — the hub (live profiles with
/// `[settings]` chips and the `[ + new profile ]` row), the per-profile
/// settings page, the members editor and its add stage, a naming input
/// (create / rename / duplicate, with the inline name-rule error), or the
/// inline delete confirm. Every stage carries the close `[x]`. Pure drawing
/// over the snapshot; every name passes the shared sanitizer.
pub(super) fn draw_profile_menu(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
    let rect = modal_rect(vm.size);
    if rect.w < 8 || rect.h < 5 {
        return;
    }
    draw_modal_backdrop(vm, frame, rect);
    let inner = frame.border(rect, &menu_title(vm), ACCENT);
    // The close affordance, on every stage: the pointer twin of
    // Esc, resolving to the MODE's own close target.
    draw_mode_close(vm, frame, rect, &crate::hit::Target::ProfileMenuClose);
    let menu = ModalFrame {
        rect,
        inner,
        unicode,
    };
    match &vm.profile_menu.stage {
        MenuStage::Settings { profile } => draw_settings_page(vm, frame, menu, profile),
        MenuStage::Members { .. } => {
            draw_name_rows(vm, frame, menu, &members(vm), vm.profile_menu.member_cursor);
            draw_members_chips(vm, frame, menu);
        }
        MenuStage::MembersAdd { .. } => {
            draw_name_rows(vm, frame, menu, &candidates(vm), vm.profile_menu.add_cursor);
        }
        _ => draw_hub(vm, frame, menu),
    }
    draw_stage_line(vm, frame, inner);
}

/// The frame's title: the page's or editor's profile by name, else `profiles`.
fn menu_title(vm: &ViewModel) -> String {
    let named = |profile: &str, prefix: &str| {
        profile_name(vm, profile).map(|name| format!("{prefix} · {name}"))
    };
    match &vm.profile_menu.stage {
        MenuStage::Settings { profile } => named(profile, "profile"),
        MenuStage::Members { profile } | MenuStage::MembersAdd { profile } => {
            named(profile, "projects")
        }
        _ => None,
    }
    .unwrap_or_else(|| "profiles".to_string())
}

/// A live profile's sanitized display name by stable id.
fn profile_name(vm: &ViewModel, profile: &str) -> Option<String> {
    vm.profiles
        .iter()
        .find(|p| p.profile_id == profile)
        .map(|p| sanitize_for_terminal(&p.name))
}

/// The members editor's rows: the profile's ordered view.
fn members(vm: &ViewModel) -> NameRows {
    NameRows {
        names: vm
            .profile_menu
            .members
            .iter()
            .map(|p| p.name.clone())
            .collect(),
        empty_note: " (no projects in this profile)",
    }
}

/// The add stage's rows: catalogue projects not yet in this view.
fn candidates(vm: &ViewModel) -> NameRows {
    NameRows {
        names: vm
            .profile_menu
            .candidates()
            .iter()
            .map(|p| p.name.clone())
            .collect(),
        empty_note: " (every catalogue project is already here)",
    }
}

/// The settings page: one activatable row per `SettingsRow::ALL`, the
/// startup pin reading its state and the delete row in the warning colour.
fn draw_settings_page(vm: &ViewModel, frame: &mut Frame, menu: ModalFrame, profile: &str) {
    for (i, row) in SettingsRow::ALL.iter().enumerate() {
        let rr = profile_menu_row_rect(menu.rect, i);
        if rr.h == 0 {
            break;
        }
        let on_cursor = i == vm.profile_menu.settings_cursor;
        let pinned = vm.startup_profile.as_deref() == Some(profile);
        let suffix = match (row, pinned) {
            (SettingsRow::StartupPin, true) => "  [on]",
            (SettingsRow::StartupPin, false) => "  [off]",
            _ => "",
        };
        let line = format!(
            "{}{}{suffix}",
            crate::chrome::row_prefix(on_cursor),
            row.label()
        );
        let style = match row {
            _ if on_cursor => vm.theme.selection(),
            SettingsRow::Delete => severity_style(crate::dispatch::Severity::Warning),
            _ => Style::fg(vm.theme.body_fg()),
        };
        draw_menu_row(frame, menu, rr, &line, style, on_cursor);
    }
}

/// A list stage's rows — the members editor and its add stage: the muted
/// note when there is nothing to list, else one cursor-marked row per name.
fn draw_name_rows(
    vm: &ViewModel,
    frame: &mut Frame,
    menu: ModalFrame,
    rows: &NameRows,
    cursor: usize,
) {
    if rows.names.is_empty() {
        clip_line(
            frame,
            menu.inner,
            1,
            rows.empty_note,
            Style::fg(crate::theme::MUTED),
        );
    }
    for (i, name) in rows.names.iter().enumerate() {
        let rr = profile_menu_row_rect(menu.rect, i);
        if rr.h == 0 {
            break;
        }
        let on_cursor = i == cursor;
        let line = format!(
            "{}{}",
            crate::chrome::row_prefix(on_cursor),
            sanitize_for_terminal(name)
        );
        draw_menu_row(frame, menu, rr, &line, row_style(vm, on_cursor), on_cursor);
    }
}

/// The members editor's action-chip row (the rail pattern — chips act on the
/// cursor row, the click twins of d / ⇧↑/↓ / a — modal-surface parity).
fn draw_members_chips(vm: &ViewModel, frame: &mut Frame, menu: ModalFrame) {
    for (chip_rect, _, text) in members_action_chips(menu.rect, vm.profile_menu.members.len()) {
        let style = mode_chip_style(vm, &members_chip_target(text));
        frame.put_str(chip_rect.x, chip_rect.y, text, style);
    }
}

/// The hub: the live profiles, each with its `[settings]` chip, then the
/// `[ + new profile ]` row — the click twin for `n`.
fn draw_hub(vm: &ViewModel, frame: &mut Frame, menu: ModalFrame) {
    for (i, p) in vm.profiles.iter().enumerate() {
        let rr = profile_menu_row_rect(menu.rect, i);
        if rr.h == 0 {
            break; // the frame is full; the rest clip away whole
        }
        draw_hub_row(vm, frame, menu, rr, i, p);
    }
    let new_rr = profile_menu_row_rect(menu.rect, vm.profiles.len());
    if new_rr.h > 0 {
        let style = mode_chip_style(vm, &crate::hit::Target::ProfileNew);
        clip_line(
            frame,
            menu.inner,
            new_rr.y - menu.inner.y,
            PROFILE_NEW_ROW,
            style,
        );
    }
}

/// One hub row: the active profile's glyph, the name, and the row's
/// `[settings]` chip — a click twin for `s` (modal-surface parity) that
/// takes the selection wash when the cursor rests on its row.
fn draw_hub_row(
    vm: &ViewModel,
    frame: &mut Frame,
    menu: ModalFrame,
    rr: Rect,
    i: usize,
    p: &crate::profile::ProfileEntry,
) {
    let is_active = p.profile_id == vm.active_profile;
    let on_cursor =
        i == vm.profile_menu.cursor && matches!(vm.profile_menu.stage, MenuStage::Browse);
    let marker = if is_active {
        crate::theme::Icon::Profile.glyph(menu.unicode)
    } else {
        ' '
    };
    let line = format!(
        "{}{marker} {}",
        crate::chrome::row_prefix(on_cursor),
        sanitize_for_terminal(&p.name)
    );
    let style = match (on_cursor, is_active) {
        (true, _) => vm.theme.selection(),
        (false, true) => ACCENT,
        (false, false) => Style::fg(vm.theme.body_fg()),
    };
    draw_menu_row(frame, menu, rr, &line, style, on_cursor);
    let cr = profile_menu_settings_rect(menu.rect, i);
    if cr.w == 0 {
        return;
    }
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

/// One list row: the cursor pill under the cursor's row (the shared cue
/// every list surface draws), then the text clipped to the inner box.
fn draw_menu_row(
    frame: &mut Frame,
    menu: ModalFrame,
    rr: Rect,
    line: &str,
    style: Style,
    on_cursor: bool,
) {
    if on_cursor {
        crate::chrome::cursor_row(frame, rr, style);
    }
    clip_line(frame, menu.inner, rr.y - menu.inner.y, line, style);
}

/// A plain list row's style: the selection wash under the cursor, body text elsewhere.
fn row_style(vm: &ViewModel, on_cursor: bool) -> Style {
    if on_cursor {
        vm.theme.selection()
    } else {
        Style::fg(vm.theme.body_fg())
    }
}

/// The stage line, on the inner bottom row: stage hints, the naming input,
/// or the inline delete confirm (an explicit second act — the profile menu
/// and header identity).
fn draw_stage_line(vm: &ViewModel, frame: &mut Frame, inner: Rect) {
    if inner.h == 0 {
        return;
    }
    let stage_dy = inner.h - 1;
    let muted = Style::fg(crate::theme::MUTED);
    let hint = match &vm.profile_menu.stage {
        MenuStage::Browse => " ⏎ switch · s settings · n new · d delete · esc close",
        MenuStage::Settings { .. } => " ⏎ open · esc back",
        MenuStage::Members { .. } => " d remove · ⇧↑/↓ reorder · a add · esc back",
        MenuStage::MembersAdd { .. } => " ⏎ add · esc back",
        MenuStage::NamingCreate
        | MenuStage::NamingRename { .. }
        | MenuStage::NamingDuplicate { .. } => {
            draw_naming_line(vm, frame, inner, stage_dy);
            return;
        }
        MenuStage::ConfirmDelete { profile, .. } => {
            let name = profile_name(vm, profile).unwrap_or_default();
            let line = format!(" delete {name}? ⏎ confirm · esc cancel");
            let warning = severity_style(crate::dispatch::Severity::Warning);
            clip_line(frame, inner, stage_dy, &line, warning);
            return;
        }
    };
    clip_line(frame, inner, stage_dy, hint, muted);
}

/// The naming input on the stage line — `new profile:`, `rename to:` or
/// `duplicate as:` with the typed name and a caret — and, one row above it,
/// the inline name-rule error while the name is refused.
fn draw_naming_line(vm: &ViewModel, frame: &mut Frame, inner: Rect, stage_dy: u16) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::fixtures::{profiled_state, render_to_text};

    #[test]
    fn profile_menu_lists_profiles_with_the_stage_line() {
        let mut s = profiled_state();
        crate::input::enter_profile_menu(&mut s.app);
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("profiles"), "the menu frame titles: {text}");
        assert!(text.contains("default"), "the live profile lists");
        assert!(text.contains("n new"), "the browse stage line teaches keys");
        // The hub's mouse affordances (the profile hub / modal-surface parity): a settings
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

    /// The members stage line is drawn from its own string rather than the
    /// keymap tables, so it is held to the same spelling separately: reorder
    /// is Shift with an arrow, the only form the dashboard receives.
    #[test]
    fn members_stage_line_teaches_reorder_with_the_arrow_keys() {
        use crate::profile_menu::MenuStage;
        let mut s = profiled_state();
        crate::input::enter_profile_menu(&mut s.app);
        s.app.profile_menu.stage = MenuStage::Members {
            profile: "pf0".into(),
        };
        let text = render_to_text(&s.snapshot());
        assert!(text.contains("⇧↑/↓ reorder"), "{text}");
        assert!(!text.contains("⇧j"), "{text}");
    }
}
