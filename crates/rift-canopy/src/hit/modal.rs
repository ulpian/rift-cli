//! The modal surfaces' mouse planes: Project Settings, the profile menu, the
//! colour picker, the destination explorer and the dispatch overlay. Each
//! resolves through the geometry `view` draws with (`modal_rect`,
//! `modal_close_rect`, the row and chip rects), so a modal's drawn cells and
//! its click regions are one solver's output.

use crate::dispatch::{Overlay, OverlayControl};
use crate::layout::Rect;
use crate::profile_menu::{MenuStage, SettingsRow};
use crate::render::frame::Size;
use crate::state::ViewModel;
use crate::view::{
    colour_picker_row_rect, colour_picker_rows, explorer_button_rects, explorer_dotfiles_rect,
    explorer_list_area, explorer_up_rect, members_action_chips, members_chip_target,
    modal_close_rect, modal_primary_rect, modal_rect, profile_menu_row_rect,
    profile_menu_settings_rect, project_settings_action_rects, project_settings_cancel_rect,
    CONFIRM_BUTTONS,
};

use super::{contains, inner_of, local_row, on_scrollbar, ScrollRegion, Target};

/// Hit-test the open Project Settings modal: the close X, the raw-transcript
/// cancel while that acknowledgement is up, then one of the right-aligned
/// action chips; the rest of the frame is an inert body and outside it nothing
/// is clickable.
pub(super) fn hit_project_settings(
    vm: &ViewModel,
    size: Size,
    col: u16,
    row: u16,
) -> Option<Target> {
    let modal = crate::dashboard::geometry(size).0;
    if !contains(modal, col, row) {
        return None;
    }
    if modal_close_rect(modal).is_some_and(|rect| contains(rect, col, row)) {
        return Some(Target::ProjectSettingsClose);
    }
    let cancel = project_settings_cancel_rect(modal, &vm.project_settings);
    if cancel.is_some_and(|rect| contains(rect, col, row)) {
        return Some(Target::ProjectSettingsCancel);
    }
    let action = project_settings_action_rects(modal, &vm.project_settings)
        .into_iter()
        .find(|(_, rect)| contains(*rect, col, row));
    match action {
        Some((index, _)) => Some(Target::ProjectSettingsAction(index)),
        None => Some(Target::OverlayBody),
    }
}

/// Hit-test the open profile menu: the close `[x]` on every stage, then the
/// stage's own rows and chips — a click on a listed row activates it (the
/// mouse equivalent of moving the cursor and pressing `Enter`); the rest of
/// the frame is inert; outside the frame nothing is clickable. The row
/// geometry is [`crate::view::profile_menu_row_rect`] — the same cells the
/// renderer paints.
pub(super) fn hit_profile_menu(vm: &ViewModel, size: Size, col: u16, row: u16) -> Option<Target> {
    let rect = modal_rect(size);
    if rect.w < 8 || rect.h < 3 || !contains(rect, col, row) {
        return None;
    }
    // The close `[x]`, on every stage — the mode's own close.
    if modal_close_rect(rect).is_some_and(|cr| contains(cr, col, row)) {
        return Some(Target::ProfileMenuClose);
    }
    let target = match &vm.profile_menu.stage {
        MenuStage::Browse => hub_row_at(vm, rect, col, row),
        MenuStage::Settings { .. } => {
            menu_row_at(rect, SettingsRow::ALL.len(), col, row).map(Target::SettingsRow)
        }
        MenuStage::Members { .. } => members_row_at(vm, rect, col, row),
        MenuStage::MembersAdd { .. } => {
            let n_candidates = vm.profile_menu.candidates().len();
            menu_row_at(rect, n_candidates, col, row).map(Target::MemberAddRow)
        }
        // The naming and confirm stages own the input; no row switches.
        _ => None,
    };
    Some(target.unwrap_or(Target::OverlayBody))
}

/// Hit-test the open colour picker: a click on a listed row (a slot,
/// a preset, or the reset row) selects it — the mouse twin of moving the cursor
/// and pressing `Enter`. The rest of the frame is inert; outside it, nothing.
pub(super) fn hit_colour_picker(vm: &ViewModel, size: Size, col: u16, row: u16) -> Option<Target> {
    let rect = modal_rect(size);
    if rect.w < 8 || rect.h < 3 || !contains(rect, col, row) {
        return None;
    }
    // The close `[x]` — the picker's own close.
    if modal_close_rect(rect).is_some_and(|cr| contains(cr, col, row)) {
        return Some(Target::PickerClose);
    }
    let picked = (0..colour_picker_rows(vm.colour_picker.stage)).find(|&i| {
        let rr = colour_picker_row_rect(rect, i);
        rr.h > 0 && contains(rr, col, row)
    });
    Some(picked.map_or(Target::OverlayBody, Target::PickerRow))
}

/// Hit-test the open destination explorer: the close X, the action
/// chips, the back row, the list's overflow scrollbar, then an entry row — each
/// resolved through the EXACT solvers `view::draw_explorer` drew with, so a
/// click lands on the cell the operator sees. The scroll offset maps the
/// viewport row back to its visible index, so a scrolled list stays honest. The
/// rest of the frame is an inert body; outside it, nothing.
pub(super) fn hit_explorer(vm: &ViewModel, size: Size, col: u16, row: u16) -> Option<Target> {
    // The same floor `draw_explorer` refuses to paint below — a frame too short
    // to draw the surface must not resolve clicks on affordances that are not
    // on screen (notably the close X, whose rect exists from 8 columns).
    let rect = modal_rect(size);
    if rect.w < 8 || rect.h < 5 || !contains(rect, col, row) {
        return None;
    }
    // The close `[x]` — the mode's own close, never the overlay's.
    if modal_close_rect(rect).is_some_and(|cr| contains(cr, col, row)) {
        return Some(Target::ExplorerClose);
    }
    let chip = explorer_button_rects(rect)
        .into_iter()
        .find(|(chip, _, _)| contains(*chip, col, row));
    if let Some((_, target, _)) = chip {
        return Some(target);
    }
    // The dot-toggle chip sits INSIDE the back row, so it outranks it.
    let dots = explorer_dotfiles_rect(rect);
    if dots.w > 0 && contains(dots, col, row) {
        return Some(Target::ExplorerDotfiles);
    }
    let up = explorer_up_rect(rect);
    if up.h > 0 && contains(up, col, row) {
        return Some(Target::ExplorerUp);
    }
    // The list's overflow scrollbar sits on the viewport's right edge and wins
    // over the row beneath it (the column-3 / rail precedent).
    let list = explorer_list_area(rect);
    let shown = vm.explorer.visible_len();
    if on_scrollbar(list, vm.explorer.scroll, shown, col, row) {
        return Some(Target::Scrollbar(ScrollRegion::Modal));
    }
    let Some(dy) = local_row(list, col, row) else {
        return Some(Target::OverlayBody);
    };
    let idx = vm.explorer.scroll.saturating_add(usize::from(dy));
    if idx < shown {
        return Some(Target::ExplorerRow(idx));
    }
    Some(Target::OverlayBody)
}

/// Resolve a click while an overlay owns the surface (the modal geometry,
/// focus-and-close, message-routing and scrollbar rules):
/// the close X, the confirm/cancel buttons, a message's primary action, the
/// modal-body scrollbar, else the (scrollable) body — the same cells
/// `view::draw_overlay` paints. A click outside the modal is inert (`None`).
pub(super) fn hit_overlay(overlay: &Overlay, size: Size, col: u16, row: u16) -> Option<Target> {
    let rect = modal_rect(size);
    if rect.w < 8 || rect.h < 3 || !contains(rect, col, row) {
        return None;
    }
    // The close X, top-right, for modals that carry a Close control.
    let closable = overlay.controls().contains(&OverlayControl::Close);
    if closable && modal_close_rect(rect).is_some_and(|cr| contains(cr, col, row)) {
        return Some(Target::OverlayClose);
    }
    let inner = inner_of(rect);
    let control = match overlay {
        Overlay::Confirm { .. } => confirm_button_at(inner, col, row),
        // A message modal's primary action (the wizard's *add project*).
        Overlay::Alert(message) => {
            let primary = modal_primary_rect(inner, message.action);
            if primary.is_some_and(|pr| contains(pr, col, row)) {
                Some(Target::WizardAdd)
            } else {
                None
            }
        }
        // The modal-body scrollbar (a long Results listing overflows the box).
        Overlay::Results {
            lines,
            scroll,
            footer,
            ..
        } => {
            let body = Rect {
                h: inner.h.saturating_sub(u16::from(footer.is_some())),
                ..inner
            };
            if on_scrollbar(body, *scroll, lines.len(), col, row) {
                Some(Target::Scrollbar(ScrollRegion::Modal))
            } else {
                None
            }
        }
        _ => None,
    };
    Some(control.unwrap_or(Target::OverlayBody))
}

/// The hub's rows: a row's `[settings]` chip outranks the row it sits on (it
/// is inside it), and the `[ + new profile ]` row follows the listed profiles.
fn hub_row_at(vm: &ViewModel, rect: Rect, col: u16, row: u16) -> Option<Target> {
    let n_profiles = vm.profiles.len();
    let chip = (0..n_profiles).find(|&i| {
        let chip = profile_menu_settings_rect(rect, i);
        chip.w > 0 && contains(chip, col, row)
    });
    if let Some(i) = chip {
        return Some(Target::ProfileSettings(i));
    }
    if let Some(i) = menu_row_at(rect, n_profiles, col, row) {
        return Some(Target::ProfileRow(i));
    }
    let new_row = profile_menu_row_rect(rect, n_profiles);
    if new_row.h > 0 && contains(new_row, col, row) {
        return Some(Target::ProfileNew);
    }
    None
}

/// The members editor: the action chips first (they sit on their own trailing
/// row), then the member row under the click.
fn members_row_at(vm: &ViewModel, rect: Rect, col: u16, row: u16) -> Option<Target> {
    let n_members = vm.profile_menu.members.len();
    let chip = members_action_chips(rect, n_members)
        .into_iter()
        .find(|(chip, _, _)| contains(*chip, col, row));
    if let Some((_, _, text)) = chip {
        return Some(members_chip_target(text));
    }
    menu_row_at(rect, n_members, col, row).map(Target::MemberRow)
}

/// The listed row under `(col, row)` among the menu's first `n_rows` rows —
/// a row the frame clipped away (zero height) never answers.
fn menu_row_at(rect: Rect, n_rows: usize, col: u16, row: u16) -> Option<usize> {
    (0..n_rows).find(|&i| {
        let rr = profile_menu_row_rect(rect, i);
        rr.h > 0 && contains(rr, col, row)
    })
}

/// The confirm and cancel buttons on a Confirm modal's button row — inner
/// dy=3, mirroring `view::draw_overlay` — measured by their labels' widths
/// with the three-cell gap between them.
fn confirm_button_at(inner: Rect, col: u16, row: u16) -> Option<Target> {
    if row != inner.y.saturating_add(3) {
        return None;
    }
    let (confirm, cancel) = CONFIRM_BUTTONS;
    let confirm_w = u16::try_from(confirm.chars().count()).unwrap_or(0);
    let cancel_w = u16::try_from(cancel.chars().count()).unwrap_or(0);
    if col >= inner.x && col < inner.x.saturating_add(confirm_w) {
        return Some(Target::OverlayConfirm);
    }
    let kx = inner.x.saturating_add(confirm_w).saturating_add(3);
    if col >= kx && col < kx.saturating_add(cancel_w) {
        return Some(Target::OverlayCancel);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hit::fixtures::{dir, explorer_state, profiled_state, project, state_with_detail};
    use crate::hit::hit_test;
    use crate::render::frame::Size;
    use crate::state::AppState;

    #[test]
    fn project_settings_mouse_plane_is_limited_to_action_chips() {
        let mut s = state_with_detail(120, 40);
        let mut view = s
            .snapshot()
            .detail
            .as_deref()
            .unwrap()
            .project_settings
            .as_ref()
            .clone();
        view.integrations = (0..9)
            .map(|index| crate::NativeAgentIntegrationRow {
                agent: format!("agent-{index}"),
                display_name: format!("Agent {index}"),
                detection: crate::NativeAgentDetection::NotDetected,
                executable_name: None,
                installations: Vec::new(),
                wiring: crate::NativeAgentWiring::Missing,
                hook_wiring: None,
                alpha_supported: true,
                selected: false,
                remedies: Vec::new(),
            })
            .collect();
        s.app.project_settings.open(view);
        s.app.mode = crate::input::Mode::ProjectSettings;
        let v = s.snapshot();
        let modal = crate::dashboard::geometry(v.size).0;
        let close = crate::view::modal_close_rect(modal).unwrap();
        assert_eq!(
            hit_test(&v, close.x, close.y),
            Some(Target::ProjectSettingsClose)
        );
        let chips = crate::view::project_settings_action_rects(modal, &v.project_settings);
        assert!(!chips.is_empty());
        for (index, chip) in chips {
            assert_eq!(
                hit_test(&v, chip.x, chip.y),
                Some(Target::ProjectSettingsAction(index))
            );
            if chip.x > modal.x + 1 {
                assert_eq!(
                    hit_test(&v, chip.x - 1, chip.y),
                    Some(Target::OverlayBody),
                    "the label/gap is not an activation target"
                );
            }
        }
        s.app.project_settings.confirm_raw = true;
        let v = s.snapshot();
        let cancel = crate::view::project_settings_cancel_rect(modal, &v.project_settings).unwrap();
        assert_eq!(
            hit_test(&v, cancel.x, cancel.y),
            Some(Target::ProjectSettingsCancel)
        );
    }

    #[test]
    fn settings_chips_and_new_button_hit_their_drawn_cells() {
        // modal-surface parity: every hub act has a click twin — the chips, the
        // `[ + new profile ]` row, and the close X on EVERY stage.
        let mut s = profiled_state(120, 40);
        crate::input::enter_profile_menu(&mut s.app);
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        for i in 0..2usize {
            let chip = crate::view::profile_menu_settings_rect(modal, i);
            assert!(chip.w > 0, "the chip renders at this width");
            assert_eq!(
                hit_test(&v, chip.x, chip.y),
                Some(Target::ProfileSettings(i))
            );
            // The rest of the row still switches.
            let rr = crate::view::profile_menu_row_rect(modal, i);
            assert_eq!(hit_test(&v, rr.x, rr.y), Some(Target::ProfileRow(i)));
        }
        let new_rr = crate::view::profile_menu_row_rect(modal, 2);
        assert_eq!(hit_test(&v, new_rr.x, new_rr.y), Some(Target::ProfileNew));
        let cr = crate::view::modal_close_rect(modal).unwrap();
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::ProfileMenuClose));
        // The settings stage: rows resolve, and the X still closes.
        s.app.profile_menu.stage = crate::profile_menu::MenuStage::Settings {
            profile: "pf0".into(),
        };
        let v = s.snapshot();
        for i in 0..crate::profile_menu::SettingsRow::ALL.len() {
            let rr = crate::view::profile_menu_row_rect(modal, i);
            assert_eq!(hit_test(&v, rr.x, rr.y), Some(Target::SettingsRow(i)));
        }
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::ProfileMenuClose));
    }

    #[test]
    fn profile_menu_rows_hit_their_drawn_cells_and_the_body_is_inert() {
        let mut s = profiled_state(120, 40);
        crate::input::enter_profile_menu(&mut s.app);
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        for i in 0..2usize {
            let rr = crate::view::profile_menu_row_rect(modal, i);
            assert!(rr.h > 0);
            assert_eq!(hit_test(&v, rr.x, rr.y), Some(Target::ProfileRow(i)));
            assert_eq!(
                hit_test(&v, rr.x + rr.w - 1, rr.y),
                Some(Target::ProfileRow(i))
            );
        }
        // Inside the frame but off the rows: inert body. Outside: nothing.
        assert_eq!(
            hit_test(&v, modal.x + 1, modal.y + modal.h - 2),
            Some(Target::OverlayBody)
        );
        assert_eq!(hit_test(&v, modal.x.saturating_sub(2), modal.y + 2), None);
    }

    #[test]
    fn colour_picker_rows_hit_their_drawn_cells() {
        let mut s = profiled_state(120, 40);
        crate::input::enter_profile_menu(&mut s.app);
        s.app.mode = crate::input::Mode::ColourPicker;
        s.app.colour_picker.reset();
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        // Slots stage: five rows.
        for i in 0..crate::theme::ThemeSlot::ALL.len() {
            let rr = crate::view::colour_picker_row_rect(modal, i);
            assert!(rr.h > 0);
            assert_eq!(hit_test(&v, rr.x, rr.y), Some(Target::PickerRow(i)));
        }
        // Presets stage: eleven rows (ten presets + reset).
        s.app.colour_picker.stage = crate::colour_picker::PickerStage::Presets;
        let v = s.snapshot();
        for i in 0..=crate::colour_picker::RESET_ROW {
            let rr = crate::view::colour_picker_row_rect(modal, i);
            if rr.h == 0 {
                continue; // a short frame clips trailing rows whole
            }
            assert_eq!(
                hit_test(&v, rr.x + rr.w - 1, rr.y),
                Some(Target::PickerRow(i))
            );
        }
        // Off the rows: an inert body. Outside the frame: nothing.
        assert_eq!(
            hit_test(&v, modal.x + 1, modal.y + modal.h - 2),
            Some(Target::OverlayBody)
        );
        assert_eq!(hit_test(&v, modal.x.saturating_sub(2), modal.y + 2), None);
    }

    #[test]
    fn profile_menu_rows_go_inert_outside_browse() {
        let mut s = profiled_state(120, 40);
        crate::input::enter_profile_menu(&mut s.app);
        s.app.profile_menu.stage = crate::profile_menu::MenuStage::NamingCreate;
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        let rr = crate::view::profile_menu_row_rect(modal, 0);
        assert_eq!(
            hit_test(&v, rr.x, rr.y),
            Some(Target::OverlayBody),
            "naming owns the input; rows are not switch targets"
        );
    }

    // ---- the destination explorer's mouse plane ----------------------------

    #[test]
    fn explorer_pixels_equal_targets_on_every_drawn_affordance() {
        let mut s = explorer_state(4);
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        // The close X — the MODE's own, never `OverlayClose`.
        let cr = crate::view::modal_close_rect(modal).unwrap();
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::ExplorerClose));
        assert_eq!(
            hit_test(&v, cr.x + cr.w - 1, cr.y),
            Some(Target::ExplorerClose)
        );
        // The back row — and the dot-toggle chip that sits inside it, which
        // must outrank the row it overlaps (both ends of the chip).
        let up = crate::view::explorer_up_rect(modal);
        assert_eq!(hit_test(&v, up.x, up.y), Some(Target::ExplorerUp));
        let dots = crate::view::explorer_dotfiles_rect(modal);
        assert!(dots.w > 0, "the dot-toggle chip renders at this width");
        assert_eq!(hit_test(&v, dots.x, dots.y), Some(Target::ExplorerDotfiles));
        assert_eq!(
            hit_test(&v, dots.x + dots.w - 1, dots.y),
            Some(Target::ExplorerDotfiles)
        );
        assert_eq!(
            hit_test(&v, dots.x - 1, dots.y),
            Some(Target::ExplorerUp),
            "one cell left of the chip is the back row again"
        );
        // Every listed row, both ends.
        for i in 0..4usize {
            let rr = crate::view::explorer_row_rect(modal, i, 0);
            assert!(rr.h > 0);
            assert_eq!(hit_test(&v, rr.x, rr.y), Some(Target::ExplorerRow(i)));
            assert_eq!(
                hit_test(&v, rr.x + rr.w - 1, rr.y),
                Some(Target::ExplorerRow(i))
            );
        }
        // Both chips.
        for (chip, target, _) in crate::view::explorer_button_rects(modal) {
            assert_eq!(hit_test(&v, chip.x, chip.y), Some(target.clone()));
            assert_eq!(
                hit_test(&v, chip.x + chip.w - 1, chip.y),
                Some(target.clone()),
                "{target:?} resolves across its whole label"
            );
        }
        // A viewport row past the listing is inert body — never a phantom row.
        let empty = crate::view::explorer_row_rect(modal, 5, 0);
        assert!(empty.h > 0, "the viewport row exists…");
        assert_eq!(
            hit_test(&v, empty.x, empty.y),
            Some(Target::OverlayBody),
            "…but there is no fifth entry to click"
        );
        // Outside the frame nothing is clickable (the scrim is inert).
        assert_eq!(hit_test(&v, modal.x.saturating_sub(2), modal.y + 2), None);
    }

    #[test]
    fn a_scrolled_explorer_maps_the_viewport_row_back_to_its_visible_index() {
        let mut s = explorer_state(200);
        s.app.explorer.scroll = 20;
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        let list = crate::view::explorer_list_area(modal);
        // The viewport's top row is entry 20, not entry 0 (the profile menu's
        // zero-sizing would have made this a wrong-row click).
        assert_eq!(hit_test(&v, list.x, list.y), Some(Target::ExplorerRow(20)));
        assert_eq!(
            hit_test(&v, list.x, list.y + list.h - 1),
            Some(Target::ExplorerRow(20 + usize::from(list.h) - 1))
        );
    }

    #[test]
    fn the_explorer_scrollbar_resolves_where_it_is_drawn_and_only_on_overflow() {
        let mut s = explorer_state(200);
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        let list = crate::view::explorer_list_area(modal);
        let bar = crate::chrome::scrollbar(list, 0, 200).expect("200 rows overflow");
        for dy in 0..bar.track.h {
            assert_eq!(
                hit_test(&v, bar.track.x, bar.track.y + dy),
                Some(Target::Scrollbar(ScrollRegion::Modal)),
            );
        }
        // A fitting listing has no bar — the same cell is just a row.
        let mut s = explorer_state(2);
        let v = s.snapshot();
        assert_eq!(
            hit_test(&v, bar.track.x, bar.track.y),
            Some(Target::ExplorerRow(0)),
            "no overflow, no bar, no scrollbar target"
        );
    }

    #[test]
    fn overlay_owns_its_region_and_buttons_resolve() {
        let mut s = AppState::new(vec![project("a")], Size::new(100, 32));
        s.app.splash.skip();
        s.app.overlay = Some(crate::dispatch::Overlay::Confirm {
            request: crate::dispatch::DispatchRequest::Rebuild,
            repo: "/r/a".into(),
        });
        let v = s.snapshot();
        // The modal is a centered box; its geometry comes from the
        // one shared `modal_rect(size)` the renderer also uses.
        let size = Size::new(100, 32);
        let rect = crate::view::modal_rect(size);
        let inner = inner_of(rect);
        let button_row = inner.y + 3;
        // The Confirm button's first cell.
        assert_eq!(
            hit_test(&v, inner.x, button_row),
            Some(Target::OverlayConfirm)
        );
        // The Cancel button (after the confirm label + the 3-cell gap).
        let confirm_w = u16::try_from(crate::view::CONFIRM_BUTTONS.0.chars().count()).unwrap();
        assert_eq!(
            hit_test(&v, inner.x + confirm_w + 3, button_row),
            Some(Target::OverlayCancel)
        );
        // Any other overlay cell is the body.
        assert_eq!(
            hit_test(&v, inner.x + 1, inner.y),
            Some(Target::OverlayBody)
        );
        // Outside the modal nothing is clickable while it is up (the scrim
        // click is inert): a corner well outside the centered box.
        assert_eq!(hit_test(&v, 1, size.h - 1), None);
    }

    #[test]
    fn results_overlay_has_no_buttons() {
        let mut s = AppState::new(vec![project("a")], Size::new(100, 32));
        s.app.splash.skip();
        s.app.overlay = Some(crate::dispatch::Overlay::Results {
            title: "rift status".into(),
            lines: vec!["ok".into()],
            scroll: 0,
            footer: None,
            truncated: false,
        });
        let v = s.snapshot();
        let inner = inner_of(crate::view::modal_rect(Size::new(100, 32)));
        assert_eq!(
            hit_test(&v, inner.x, inner.y + 3),
            Some(Target::OverlayBody),
            "no confirm target on a results overlay"
        );
    }

    #[test]
    fn the_close_x_is_clickable_and_pixels_equal_targets() {
        let mut s = AppState::new(vec![project("a")], Size::new(100, 32));
        s.app.splash.skip();
        s.app.open_overlay(crate::dispatch::Overlay::Confirm {
            request: crate::dispatch::DispatchRequest::Rebuild,
            repo: "/r/a".into(),
        });
        let v = s.snapshot();
        let rect = crate::view::modal_rect(Size::new(100, 32));
        let cr = crate::view::modal_close_rect(rect).expect("wide modal has a close X");
        // Every cell the X is drawn into resolves to OverlayClose.
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::OverlayClose));
        assert_eq!(
            hit_test(&v, cr.x + cr.w - 1, cr.y),
            Some(Target::OverlayClose)
        );
        // Just left of the X is the modal body, not the close.
        assert_eq!(hit_test(&v, cr.x - 1, cr.y), Some(Target::OverlayBody));
    }

    #[test]
    fn the_wizard_primary_button_resolves_where_it_is_drawn() {
        // The empty registry raises the wizard modal; its *add project* chip is
        // clickable and its drawn cells equal the hit region (the message-routing rule).
        let mut s = AppState::new(vec![], Size::new(100, 32));
        s.app.splash.skip();
        assert!(s.app.showing_wizard());
        let v = s.snapshot();
        let inner = inner_of(crate::view::modal_rect(Size::new(100, 32)));
        let pr = crate::view::modal_primary_rect(inner, crate::dispatch::MessageAction::AddProject)
            .expect("the wizard has a primary button");
        // Both ends of the chip resolve to WizardAdd.
        assert_eq!(hit_test(&v, pr.x, pr.y), Some(Target::WizardAdd));
        assert_eq!(hit_test(&v, pr.x + pr.w - 1, pr.y), Some(Target::WizardAdd));
        // The close X is still reachable on the wizard (dismiss-only fallback).
        let cr =
            crate::view::modal_close_rect(crate::view::modal_rect(Size::new(100, 32))).unwrap();
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::OverlayClose));
    }

    #[test]
    fn a_running_modal_has_no_close_x() {
        let mut s = AppState::new(vec![project("a")], Size::new(100, 32));
        s.app.splash.skip();
        s.app.open_overlay(crate::dispatch::Overlay::running("x"));
        let v = s.snapshot();
        let rect = crate::view::modal_rect(Size::new(100, 32));
        let cr = crate::view::modal_close_rect(rect).unwrap();
        // Running carries no Close control, so that cell is inert body, not close.
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::OverlayBody));
    }

    proptest::proptest! {
        /// …including over the destination explorer, whose scroll-aware solvers
        /// are the first geometry in the tree to take an offset — at any size,
        /// with any listing length and any offset (the destination explorer).
        #[test]
        fn explorer_hit_test_never_panics(
            w in 0u16..300,
            h in 0u16..120,
            col in 0u16..300,
            row in 0u16..120,
            entries in 0usize..40,
            scroll in 0usize..60,
        ) {
            let mut s = AppState::new(vec![project("a")], Size::new(w, h));
            s.app.splash.skip();
            crate::input::enter_explorer(&mut s.app);
            s.app.explorer.entries = (0..entries).map(|i| dir(&format!("d{i}"))).collect();
            s.app.explorer.scroll = scroll;
            let v = s.snapshot();
            let _ = hit_test(&v, col, row);
        }
    }
}
