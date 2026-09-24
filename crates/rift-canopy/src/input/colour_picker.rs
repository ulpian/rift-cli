//! Colour-picker mode: the slots stage, the presets stage, and the live
//! application of a preset.

use super::{Action, CanopyKey, Mode};
use crate::App;

/// The colour-picker reducer: stage one moves over the five slots,
/// stage two over the ten presets + the reset row. `Enter` selects (a preset
/// application returns the typed action — the state layer re-themes in the same
/// frame and appends the superseding event); `Esc` steps back
/// (presets → slots → the profile menu). Pure over `App.colour_picker`.
pub(super) fn on_key_colour_picker(app: &mut App, key: CanopyKey) -> Action {
    use crate::colour_picker::{PickerStage, RESET_ROW};
    // Ctrl-C still quits the whole app, even mid-pick.
    if matches!(key, CanopyKey::Quit) {
        app.quit = true;
        return Action::Quit;
    }
    match app.colour_picker.stage {
        PickerStage::Slots => {
            let last = crate::theme::ThemeSlot::ALL.len() - 1;
            match key {
                CanopyKey::Char('j') | CanopyKey::Down => {
                    app.colour_picker.slot_cursor = (app.colour_picker.slot_cursor + 1).min(last);
                }
                CanopyKey::Char('k') | CanopyKey::Up => {
                    app.colour_picker.slot_cursor = app.colour_picker.slot_cursor.saturating_sub(1);
                }
                CanopyKey::Enter => {
                    app.colour_picker.stage = PickerStage::Presets;
                    app.colour_picker.preset_cursor = 0;
                }
                CanopyKey::Esc => {
                    // Back to the profile menu (its browse state is intact).
                    app.mode = Mode::ProfileMenu;
                    app.colour_picker.reset();
                }
                _ => {}
            }
            Action::None
        }
        PickerStage::Presets => match key {
            CanopyKey::Char('j') | CanopyKey::Down => {
                app.colour_picker.preset_cursor =
                    (app.colour_picker.preset_cursor + 1).min(RESET_ROW);
                Action::None
            }
            CanopyKey::Char('k') | CanopyKey::Up => {
                app.colour_picker.preset_cursor = app.colour_picker.preset_cursor.saturating_sub(1);
                Action::None
            }
            CanopyKey::Esc => {
                app.colour_picker.stage = PickerStage::Slots;
                Action::None
            }
            // A selection applies LIVE and the picker stays open — picking
            // again supersedes again; Esc afterwards keeps it (the colour picker and its presets).
            CanopyKey::Enter => {
                let slot = app.colour_picker.slot();
                let colour = crate::theme::PRESETS
                    .get(app.colour_picker.preset_cursor)
                    .map(|(_, c)| *c); // the RESET_ROW maps to None
                Action::SetThemeSlot { slot, colour }
            }
            _ => Action::None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::super::fixtures::with_profiles;
    use super::super::on_key;
    use super::*;

    /// Open the picker the profile hub way: the settings page's Colours row
    /// (Browse no longer carries `c`).
    fn open_picker(app: &mut App) {
        on_key(app, CanopyKey::Char('p'));
        on_key(app, CanopyKey::Char('s'));
        on_key(app, CanopyKey::Char('j')); // settings cursor → colours
        on_key(app, CanopyKey::Enter);
    }

    #[test]
    fn c_in_the_menu_opens_the_picker_and_esc_steps_all_the_way_back() {
        use crate::colour_picker::PickerStage;
        use crate::profile_menu::MenuStage;
        let mut app = with_profiles();
        open_picker(&mut app);
        assert_eq!(app.mode, Mode::ColourPicker);
        assert_eq!(app.colour_picker.stage, PickerStage::Slots);
        // Enter a slot's presets, then Esc steps back one stage at a time:
        // presets → slots → the settings page it opened from (browsing never
        // dead-ends — the menu stage stays intact underneath).
        on_key(&mut app, CanopyKey::Enter);
        assert_eq!(app.colour_picker.stage, PickerStage::Presets);
        on_key(&mut app, CanopyKey::Esc);
        assert_eq!(app.colour_picker.stage, PickerStage::Slots);
        on_key(&mut app, CanopyKey::Esc);
        assert_eq!(app.mode, Mode::ProfileMenu);
        assert_eq!(
            app.profile_menu.stage,
            MenuStage::Settings {
                profile: "pf0".into()
            },
            "the picker returns to the settings page it came from"
        );
    }

    #[test]
    fn picker_cursors_move_and_clamp_in_both_stages() {
        use crate::colour_picker::{PickerStage, RESET_ROW};
        use crate::theme::ThemeSlot;
        let mut app = with_profiles();
        open_picker(&mut app);
        // Slots: clamp at both ends.
        on_key(&mut app, CanopyKey::Char('k'));
        assert_eq!(app.colour_picker.slot_cursor, 0);
        for _ in 0..10 {
            on_key(&mut app, CanopyKey::Char('j'));
        }
        assert_eq!(app.colour_picker.slot_cursor, ThemeSlot::ALL.len() - 1);
        // Presets: the last row is the reset row.
        on_key(&mut app, CanopyKey::Enter);
        assert_eq!(app.colour_picker.stage, PickerStage::Presets);
        for _ in 0..20 {
            on_key(&mut app, CanopyKey::Down);
        }
        assert_eq!(app.colour_picker.preset_cursor, RESET_ROW);
    }

    #[test]
    fn enter_on_a_preset_applies_and_the_reset_row_maps_to_none() {
        use crate::theme::{ThemeSlot, PRESETS};
        let mut app = with_profiles();
        open_picker(&mut app);
        on_key(&mut app, CanopyKey::Char('j')); // slot → border
        on_key(&mut app, CanopyKey::Enter);
        on_key(&mut app, CanopyKey::Char('j')); // preset → blue
        assert_eq!(
            on_key(&mut app, CanopyKey::Enter),
            Action::SetThemeSlot {
                slot: ThemeSlot::Border,
                colour: Some(PRESETS[1].1)
            }
        );
        assert_eq!(
            app.mode,
            Mode::ColourPicker,
            "the picker stays open — picking again supersedes again"
        );
        // The reset row sits past the presets and applies None.
        for _ in 0..20 {
            on_key(&mut app, CanopyKey::Down);
        }
        assert_eq!(
            on_key(&mut app, CanopyKey::Enter),
            Action::SetThemeSlot {
                slot: ThemeSlot::Border,
                colour: None
            }
        );
    }
}
