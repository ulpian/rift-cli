//! Panel-editor mode: the layout cursor, pickup and drop, resize and column
//! moves.

use super::{Action, CanopyKey, Mode};
use crate::App;

/// Enter panel-editor mode: reset the cursor to the origin, clamped
/// into the current layout. Shared by the `e` key and the header toggle.
pub fn enter_panel_editor(app: &mut App) {
    app.mode = Mode::PanelEditor;
    app.editor.reset();
    app.editor.clamp(&app.layout);
}

/// The panel-editor reducer: the cursor moves over the layout's units
/// and, when a unit is picked up, reorders it within its column or moves it
/// across columns; `<`/`>` resize, `[`/`]` move whole columns, `r` resets, `Esc`
/// cancels a pickup then exits. All pure over `App.layout` / `App.editor`; the
/// persist write is emitted by the state layer on exit when the layout changed.
pub(super) fn on_key_editor(app: &mut App, key: CanopyKey) -> Action {
    use crate::editor;
    app.editor.clamp(&app.layout);
    let picked = app.editor.picked;
    match key {
        // Ctrl-C still quits the whole app, even mid-edit.
        CanopyKey::Quit => {
            app.quit = true;
            return Action::Quit;
        }
        // Esc cancels a live pickup first, then (a second press) exits the mode.
        CanopyKey::Esc => {
            if picked {
                app.editor.picked = false;
            } else {
                app.mode = Mode::Normal;
                app.editor.reset();
            }
        }
        // Enter / Space / `p` pick up the cursor's unit, or drop a picked one.
        CanopyKey::Enter | CanopyKey::Char(' ' | 'p') => {
            app.editor.picked = !picked;
        }
        CanopyKey::Char('h') | CanopyKey::Left => {
            if picked {
                editor::move_unit(&mut app.editor, &mut app.layout, false);
            } else {
                editor::move_cursor_col(&mut app.editor, &app.layout, false);
            }
        }
        CanopyKey::Char('l') | CanopyKey::Right => {
            if picked {
                editor::move_unit(&mut app.editor, &mut app.layout, true);
            } else {
                editor::move_cursor_col(&mut app.editor, &app.layout, true);
            }
        }
        CanopyKey::Char('j') | CanopyKey::Down => {
            if picked {
                editor::reorder_within(&mut app.editor, &mut app.layout, true);
            } else {
                editor::move_cursor_unit(&mut app.editor, &app.layout, true);
            }
        }
        CanopyKey::Char('k') | CanopyKey::Up => {
            if picked {
                editor::reorder_within(&mut app.editor, &mut app.layout, false);
            } else {
                editor::move_cursor_unit(&mut app.editor, &app.layout, false);
            }
        }
        // Resize the divider right of the cursor's column (only when not picked).
        CanopyKey::Char('>') if !picked => editor::resize(&app.editor, &mut app.layout, true),
        CanopyKey::Char('<') if !picked => editor::resize(&app.editor, &mut app.layout, false),
        // Move the whole column left/right (only when not picked).
        CanopyKey::Char(']') if !picked => {
            editor::move_column(&mut app.editor, &mut app.layout, true);
        }
        CanopyKey::Char('[') if !picked => {
            editor::move_column(&mut app.editor, &mut app.layout, false);
        }
        // Reset to the canonical layout.
        CanopyKey::Char('r') => {
            app.layout = crate::layout_model::LayoutModel::default();
            app.editor.reset();
        }
        _ => {}
    }
    Action::None
}
