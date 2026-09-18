//! The colour picker: stage one lists the five theme slots with each slot's
//! current swatch, stage two the presets beside a solid swatch box plus the
//! reset row — over the profile menu's row geometry, so the two modal
//! surfaces cannot drift apart. The dashboard behind the scrim is the live
//! preview.

use crate::col2::sanitize_for_terminal;
use crate::colour_picker::{PickerStage, RESET_ROW};
use crate::layout::Rect;
use crate::render::frame::{Frame, Style};
use crate::state::ViewModel;

use super::modal::{draw_modal_backdrop, draw_mode_close, modal_rect};
use super::profile_menu::profile_menu_row_rect;
use super::text::clip_line;
use super::ACCENT;

/// The colour picker's row rect for row index `i` — the same shape
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

/// Draw the colour picker: stage one lists the five slots with each
/// slot's current swatch (or `default`); stage two lists the ten presets —
/// name beside a solid swatch box — plus the reset row. The swatch is
/// colour-only, so on mono/ASCII the name alone carries the choice. Pure
/// drawing over the snapshot; the dashboard behind the scrim is the preview.
pub(super) fn draw_colour_picker(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
    let rect = modal_rect(vm.size);
    if rect.w < 8 || rect.h < 5 {
        return;
    }
    draw_modal_backdrop(vm, frame, rect);
    let inner = frame.border(rect, &picker_title(vm), ACCENT);
    // The close affordance: the picker's own close — back to
    // the profile menu whence it came, the pointer twin of Esc-to-the-end.
    draw_mode_close(vm, frame, rect, &crate::hit::Target::PickerClose);
    let swatch = if unicode { "██" } else { "##" };
    let hint = match vm.colour_picker.stage {
        PickerStage::Slots => {
            draw_slot_rows(vm, frame, rect, swatch);
            " ⏎ choose · esc menu"
        }
        PickerStage::Presets => {
            draw_preset_rows(vm, frame, rect, swatch);
            " ⏎ apply (live) · esc slots"
        }
    };
    if inner.h > 0 {
        clip_line(
            frame,
            inner,
            inner.h - 1,
            hint,
            Style::fg(crate::theme::MUTED),
        );
    }
}

/// The frame's title carries the TARGET profile — the page edits that
/// profile, active or not — and, on the preset stage, the slot being set.
fn picker_title(vm: &ViewModel) -> String {
    let target_name = vm
        .profiles
        .iter()
        .find(|p| p.profile_id == vm.colour_picker.target_profile)
        .map(|p| sanitize_for_terminal(&p.name));
    match (vm.colour_picker.stage, target_name) {
        (PickerStage::Slots, Some(n)) => format!("colours · {n}"),
        (PickerStage::Slots, None) => "colours".to_string(),
        (PickerStage::Presets, Some(n)) => {
            format!("colours · {n} · {}", vm.colour_picker.slot().key())
        }
        (PickerStage::Presets, None) => format!("colours · {}", vm.colour_picker.slot().key()),
    }
}

/// Stage one: a row per theme slot, its key beside the slot's current value —
/// the TARGET's overrides, not the live theme, so a non-active profile's page
/// tells the truth. A swatch in its override, or the word `default`.
fn draw_slot_rows(vm: &ViewModel, frame: &mut Frame, modal: Rect, swatch: &str) {
    for (i, slot) in crate::theme::ThemeSlot::ALL.iter().enumerate() {
        let rr = colour_picker_row_rect(modal, i);
        if rr.h == 0 {
            break;
        }
        let on_cursor = i == vm.colour_picker.slot_cursor;
        let style = picker_row_style(vm, frame, rr, on_cursor);
        let x = frame.put_str(
            rr.x,
            rr.y,
            &format!("{}{:<12}", crate::chrome::row_prefix(on_cursor), slot.key()),
            style,
        );
        match vm.colour_picker.target_overrides.get(*slot) {
            Some(c) => {
                frame.put_str(x, rr.y, swatch, Style::fg(c).with_bg(style.bg));
            }
            None => {
                frame.put_str(x, rr.y, "default", vm.theme.dim().with_bg(style.bg));
            }
        }
    }
}

/// Stage two: a row per preset — the name beside a box of the colour itself —
/// and the reset row beside the presets.
fn draw_preset_rows(vm: &ViewModel, frame: &mut Frame, modal: Rect, swatch: &str) {
    for i in 0..=RESET_ROW {
        let rr = colour_picker_row_rect(modal, i);
        if rr.h == 0 {
            break;
        }
        let on_cursor = i == vm.colour_picker.preset_cursor;
        let style = picker_row_style(vm, frame, rr, on_cursor);
        let prefix = crate::chrome::row_prefix(on_cursor);
        match crate::theme::PRESETS.get(i) {
            Some((name, colour)) => {
                let x = frame.put_str(rr.x, rr.y, &format!("{prefix}{name:<8}"), style);
                frame.put_str(x, rr.y, swatch, Style::fg(*colour).with_bg(style.bg));
            }
            None => {
                frame.put_str(rr.x, rr.y, &format!("{prefix}reset to default"), style);
            }
        }
    }
}

/// A picker row's style — and, under the cursor, the shared cursor pill the
/// row is drawn over (the cue every list surface carries onto the floor tiers).
fn picker_row_style(vm: &ViewModel, frame: &mut Frame, rr: Rect, on_cursor: bool) -> Style {
    if !on_cursor {
        return Style::fg(vm.theme.body_fg());
    }
    let style = vm.theme.selection();
    crate::chrome::cursor_row(frame, rr, style);
    style
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::draw;
    use crate::view::fixtures::profiled_state;

    #[test]
    fn picker_lists_presets_as_name_beside_a_swatch() {
        // the colour picker and its presets: stage two is one row per preset — the NAME beside a
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
            let sx = rr.x + 2 + 8;
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
        // Target-aware: the slot stage reads the TARGET's
        // overrides — here a profile whose border is overridden — not the
        // live theme, so a non-active page tells the truth.
        let mut state = profiled_state();
        let mut overrides = crate::theme::ThemeOverrides::NONE;
        overrides.set(
            crate::theme::ThemeSlot::Border,
            Some(crate::theme::PRESETS[1].1),
        );
        state.app.mode = crate::input::Mode::ColourPicker;
        state.app.colour_picker.target_profile = "pf0".into();
        state.app.colour_picker.target_overrides = overrides;
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
        let sx = rr.x + 2 + 12;
        let cell = frame.get(sx, rr.y).unwrap();
        assert_eq!(cell.ch, '#');
        assert_eq!(cell.style.fg, crate::theme::PRESETS[1].1);
    }
}
