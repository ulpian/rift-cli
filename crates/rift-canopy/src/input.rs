//! The pure interaction reducer: keys → state transitions + side-effect actions.
//!
//! All of Canopy's interactivity lives here as a terminal-free function over
//! [`App`], so the dashboard's behavior (selection, the add-project prompt, the
//! slash palette, quitting) is unit- and property-testable without a real
//! terminal. The xre backend ([`crate::xre`]) only translates its event stream
//! into [`CanopyKey`] and renders the resulting state — it holds no logic of its
//! own.

use crate::App;

/// Which editing/interaction mode the dashboard is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Navigating: arrow/`j`/`k` select, `a` adds a project, `/` opens the palette.
    Normal,
    /// Typing a slash command into the bottom palette.
    Command,
    /// Typing a path to register a new project.
    AddProject,
    /// Rearranging the dashboard layout (ADR-0034): grips on every panel, the
    /// cursor moves/reorders/resizes, `Esc` or the toggle exits.
    PanelEditor,
    /// Managing profiles (ADR-0037): a centered menu over the shared modal
    /// geometry — browse/switch/create/rename/delete, `Esc` steps back.
    ProfileMenu,
    /// Picking a profile's theme colours (ADR-0039): two stages — the five
    /// slots, then the ten presets + reset — with the dashboard as the live
    /// preview; `Esc` steps back (presets → slots → the profile menu).
    ColourPicker,
}

/// A terminal-agnostic key, translated from the backend's event stream so the
/// reducer stays pure and testable without a real terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanopyKey {
    /// A printable character.
    Char(char),
    /// Confirm the current line / selection.
    Enter,
    /// Cancel the current mode.
    Esc,
    /// Delete the last character of the edit buffer.
    Backspace,
    /// Move the selection / scroll up.
    Up,
    /// Move the selection / scroll down.
    Down,
    /// Cycle focus to the next panel.
    Tab,
    /// Cycle focus to the previous panel (Shift-Tab).
    BackTab,
    /// Move focus one column left.
    Left,
    /// Move focus one column right.
    Right,
    /// Scroll the focused panel up by a page.
    PageUp,
    /// Scroll the focused panel down by a page.
    PageDown,
    /// Reorder the selected item up (Shift-Up).
    ShiftUp,
    /// Reorder the selected item down (Shift-Down).
    ShiftDown,
    /// A normalized quit request (e.g. Ctrl-C).
    Quit,
}

/// A mouse button — a terminal-agnostic mirror of the backend's, translated in
/// the live driver so the reducer and hit-testing stay engine-free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    /// The primary (left) button.
    Left,
    /// The secondary (right) button.
    Right,
    /// The middle button / wheel click.
    Middle,
}

/// A mouse action at a cell coordinate. Only presses and the scroll wheel drive
/// interaction today; moves/drags/releases are modeled so the source can forward
/// them, but the reducer ignores them (no per-move work).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseKind {
    /// A button was pressed.
    Down(MouseButton),
    /// A button was released.
    Up(MouseButton),
    /// The pointer moved with a button held.
    Drag(MouseButton),
    /// The pointer moved with no button held.
    Moved,
    /// The wheel scrolled up (away from the user).
    ScrollUp,
    /// The wheel scrolled down (toward the user).
    ScrollDown,
}

/// A mouse event in terminal cells from the top-left — the engine-free form the
/// reducer consumes (via [`crate::state::Event::Mouse`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MouseInput {
    /// What happened.
    pub kind: MouseKind,
    /// Column (cell x), 0-based from the left.
    pub col: u16,
    /// Row (cell y), 0-based from the top.
    pub row: u16,
}

/// What the event loop should do after a key is handled. The state mutation has
/// already been applied to [`App`]; this is only the externally-visible effect.
///
/// `RemoveProject`/`ReorderProject` carry no target: they act on the **currently
/// selected** project, which the reducer resolves to a path when it builds the
/// append-only [`crate::PersistOp`] (keeping the key handler free of ids/paths).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Nothing beyond the state mutation already applied.
    None,
    /// Exit the dashboard.
    Quit,
    /// Register a project at the given (user-entered) path.
    AddProject(String),
    /// Remove (hide) the currently selected project.
    RemoveProject,
    /// Reorder the currently selected project up (`true`) or down (`false`).
    ReorderProject(bool),
    /// Flip the feature toggle under the column-2 cursor.
    ToggleFeature,
    /// Retry (reinstall) the feature toggle under the column-2 cursor.
    RetryFeature,
    /// Submit the command buffer (the reducer parses; errors render inline).
    RunLine(String),
    /// The confirm overlay's Enter/Confirm — dispatch the pending request.
    ConfirmDispatch,
    /// Switch to the profile under the menu cursor (ADR-0037: the whole view
    /// follows — membership, selection, arrangement).
    SwitchProfile,
    /// Create a profile with the validated name (copying the active profile's
    /// project list at creation — ADR-0036).
    CreateProfile(String),
    /// Rename a profile (by stable id) to the validated name.
    RenameProfile {
        /// The stable id of the profile being renamed.
        profile: String,
        /// The validated new display name.
        name: String,
    },
    /// The menu's confirmed delete for a profile (by stable id) — only reached
    /// through the inline confirm (an explicit second act).
    DeleteProfile(String),
    /// Duplicate a profile under the validated new name — the full clone of
    /// its effective state (ADR-0045 §1).
    DuplicateProfile {
        /// The stable id of the profile being cloned.
        source: String,
        /// The validated new display name.
        name: String,
    },
    /// Flip the startup pin for a profile (ADR-0045 §2): pin it when it is
    /// not the pin, unpin when it is (the state layer resolves which).
    ToggleStartupPin(String),
    /// Load a profile's members + the catalogue through the loader seam
    /// (entering the members editor, and after every membership op — the
    /// reducer only *requests* loads).
    LoadMembers(String),
    /// Remove the member under the editor cursor from the edited profile.
    MemberRemove,
    /// Move the member under the editor cursor up (`true`) or down.
    MemberReorder(bool),
    /// Include the add-stage candidate under the cursor in the edited profile.
    MemberAdd,
    /// The picker opened for a profile (ADR-0045 §5): the state layer seeds
    /// the target's overrides — purely from the live theme when the target is
    /// active, else through the loader seam.
    OpenColours(String),
    /// Apply one theme-slot selection for the active profile (ADR-0039):
    /// `None` colour is the reset row. The state layer re-themes the running
    /// dashboard in the same frame and appends the superseding event.
    SetThemeSlot {
        /// The slot being repainted.
        slot: crate::theme::ThemeSlot,
        /// The chosen preset's colour, or `None` for reset-to-default.
        colour: Option<crate::render::frame::Color>,
    },
}

/// Apply one key to the dashboard, returning the effect the event loop must run.
///
/// While the splash is still playing, any key dismisses it (and is otherwise
/// swallowed). Otherwise the key is dispatched by the current [`Mode`].
pub fn on_key(app: &mut App, key: CanopyKey) -> Action {
    if !app.splash.done() {
        app.splash.skip();
        return Action::None;
    }
    // An open overlay owns the keys (ADR-0021): Results scrolls/dismisses,
    // Confirm accepts ONLY Enter/Esc (every other key inert — a buffered
    // keystroke can never confirm), Running swallows everything.
    if app.overlay.is_some() {
        return on_key_overlay(app, key);
    }
    match app.mode {
        Mode::Normal => on_key_normal(app, key),
        Mode::Command | Mode::AddProject => on_key_editing(app, key),
        Mode::PanelEditor => on_key_editor(app, key),
        Mode::ProfileMenu => on_key_profile_menu(app, key),
        Mode::ColourPicker => on_key_colour_picker(app, key),
    }
}

fn on_key_normal(app: &mut App, key: CanopyKey) -> Action {
    use crate::Panel;
    match key {
        CanopyKey::Char('a') => {
            app.mode = Mode::AddProject;
            app.command_line.clear();
            Action::None
        }
        CanopyKey::Char('/') => {
            app.set_focus(Panel::Command);
            app.mode = Mode::Command;
            app.command_line.clear();
            Action::None
        }
        CanopyKey::Tab => {
            app.cycle_focus(true);
            Action::None
        }
        CanopyKey::BackTab => {
            app.cycle_focus(false);
            Action::None
        }
        CanopyKey::Left => {
            app.move_focus(true);
            Action::None
        }
        CanopyKey::Right => {
            app.move_focus(false);
            Action::None
        }
        CanopyKey::Char('j') | CanopyKey::Down => {
            nav_vertical(app, true, false);
            Action::None
        }
        CanopyKey::Char('k') | CanopyKey::Up => {
            nav_vertical(app, false, false);
            Action::None
        }
        CanopyKey::PageDown => {
            nav_vertical(app, true, true);
            Action::None
        }
        CanopyKey::PageUp => {
            nav_vertical(app, false, true);
            Action::None
        }
        // Remove / reorder act on the selected project — only when the projects
        // column is focused and a selection exists (else inert).
        CanopyKey::Char('d' | 'x')
            if app.focus == Panel::Projects && app.selected_project().is_some() =>
        {
            Action::RemoveProject
        }
        CanopyKey::ShiftUp if app.focus == Panel::Projects && app.selected_project().is_some() => {
            Action::ReorderProject(true)
        }
        CanopyKey::ShiftDown
            if app.focus == Panel::Projects && app.selected_project().is_some() =>
        {
            Action::ReorderProject(false)
        }
        // Enter on the rail opens the selection: selection already follows the
        // cursor, so "open" is focusing the detail column (M55-05).
        CanopyKey::Enter if app.focus == Panel::Projects => {
            app.set_focus(Panel::Detail);
            Action::None
        }
        // Column-2 feature toggles: Space / Enter / `t` flip the cursored toggle,
        // `r` retries a failed one. Only when the detail panel is focused.
        CanopyKey::Char(' ' | 't') | CanopyKey::Enter if app.focus == Panel::Detail => {
            Action::ToggleFeature
        }
        CanopyKey::Char('r') if app.focus == Panel::Detail => Action::RetryFeature,
        // `e` enters the panel editor (ADR-0034) — the freed mnemonic (it opened
        // an editor; now it opens the *layout* editor), from any focus.
        CanopyKey::Char('e') => {
            enter_panel_editor(app);
            Action::None
        }
        // `p` opens the profile menu (ADR-0037) — same act as clicking the
        // header identity button; inert until the profile plane is wired.
        CanopyKey::Char('p') => {
            enter_profile_menu(app);
            Action::None
        }
        CanopyKey::Char('q') | CanopyKey::Quit => {
            app.quit = true;
            Action::Quit
        }
        // Other keys (incl. reorder/remove without a focused selection) are inert.
        _ => Action::None,
    }
}

/// Enter panel-editor mode (ADR-0034): reset the cursor to the origin, clamped
/// into the current layout. Shared by the `e` key and the header toggle.
pub fn enter_panel_editor(app: &mut App) {
    app.mode = Mode::PanelEditor;
    app.editor.reset();
    app.editor.clamp(&app.layout);
}

/// The panel-editor reducer (ADR-0034): the cursor moves over the layout's units
/// and, when a unit is picked up, reorders it within its column or moves it
/// across columns; `<`/`>` resize, `[`/`]` move whole columns, `r` resets, `Esc`
/// cancels a pickup then exits. All pure over `App.layout` / `App.editor`; the
/// persist write is emitted by the state layer on exit when the layout changed.
fn on_key_editor(app: &mut App, key: CanopyKey) -> Action {
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

/// Enter profile-menu mode (ADR-0037): reset the menu's state machine and open
/// the cursor on the active profile's row. Shared by the `p` key and the header
/// identity button. Inert until the profile plane is wired (no live profiles).
pub fn enter_profile_menu(app: &mut App) {
    if app.profiles.is_empty() {
        return;
    }
    app.mode = Mode::ProfileMenu;
    app.profile_menu.reset();
    app.profile_menu.cursor = app
        .profiles
        .iter()
        .position(|p| p.profile_id == app.active_profile)
        .unwrap_or(0);
    app.command_line.clear();
}

/// Open the settings page for a profile (ADR-0044 §3) — the one entry both
/// the `s` key and the row chip use.
pub(crate) fn enter_profile_settings(app: &mut App, profile: String) {
    app.profile_menu.stage = crate::profile_menu::MenuStage::Settings { profile };
    app.profile_menu.settings_cursor = 0;
    app.profile_menu.name_error = false;
    app.command_line.clear();
}

/// Activate one settings row for `profile` (ADR-0044 §3) — the one dispatch
/// both `Enter` and a row click use.
pub(crate) fn activate_settings_row(
    app: &mut App,
    profile: &str,
    row: crate::profile_menu::SettingsRow,
) -> Action {
    use crate::profile_menu::{MenuStage, SettingsRow};
    match row {
        SettingsRow::Name => {
            if let Some(p) = app.profiles.iter().find(|p| p.profile_id == profile) {
                app.profile_menu.stage = MenuStage::NamingRename {
                    profile: p.profile_id.clone(),
                };
                app.profile_menu.name_error = false;
                // Prefill with the current name — a rename edits, not retypes.
                app.command_line.clone_from(&p.name);
            }
            Action::None
        }
        SettingsRow::Colours => {
            // The picker binds to THIS page's profile (ADR-0045 §5) and
            // returns here on Esc — the menu stage stays intact underneath.
            app.mode = Mode::ColourPicker;
            app.colour_picker.reset();
            app.colour_picker.target_profile = profile.to_string();
            Action::OpenColours(profile.to_string())
        }
        SettingsRow::Projects => {
            // The members editor (M512-06): the stage opens now; the lists
            // land through the loader seam (the reducer only requests).
            app.profile_menu.stage = MenuStage::Members {
                profile: profile.to_string(),
            };
            app.profile_menu.member_cursor = 0;
            app.profile_menu.add_cursor = 0;
            Action::LoadMembers(profile.to_string())
        }
        SettingsRow::Duplicate => {
            app.profile_menu.stage = MenuStage::NamingDuplicate {
                source: profile.to_string(),
            };
            app.profile_menu.name_error = false;
            app.command_line.clear();
            Action::None
        }
        SettingsRow::StartupPin => Action::ToggleStartupPin(profile.to_string()),
        SettingsRow::Delete => {
            // The removal guards refuse here first (ADR-0036 §5): never the
            // active profile, never the last one — the confirm can't open.
            if profile != app.active_profile && app.profiles.len() > 1 {
                app.profile_menu.stage = MenuStage::ConfirmDelete {
                    profile: profile.to_string(),
                    back_to_settings: true,
                };
            }
            Action::None
        }
    }
}

/// The profile-menu reducer (ADR-0037, hub-and-spoke per ADR-0044): Browse
/// moves over the live profiles (`Enter` switches, `s` opens the cursor row's
/// settings, `n` creates, `d` deletes behind the inline confirm); the settings
/// page moves over [`crate::profile_menu::SettingsRow::ALL`] and `Enter`
/// activates; naming reuses the editing-mode buffer (`App::command_line`, the
/// AddProject precedent) and validates against the ADR-0037 §4 name rules
/// before any op is emitted. `Esc` steps one stage back toward Browse, then
/// closes. Rename and colours live in settings — Browse no longer carries
/// `r`/`c`.
#[allow(clippy::too_many_lines)] // one arm per menu stage — a dispatch table
fn on_key_profile_menu(app: &mut App, key: CanopyKey) -> Action {
    use crate::profile_menu::{MenuStage, SettingsRow};
    // Ctrl-C still quits the whole app, even mid-menu.
    if matches!(key, CanopyKey::Quit) {
        app.quit = true;
        return Action::Quit;
    }
    let last = app.profiles.len().saturating_sub(1);
    match app.profile_menu.stage.clone() {
        MenuStage::Browse => match key {
            CanopyKey::Char('j') | CanopyKey::Down => {
                app.profile_menu.cursor = (app.profile_menu.cursor + 1).min(last);
                Action::None
            }
            CanopyKey::Char('k') | CanopyKey::Up => {
                app.profile_menu.cursor = app.profile_menu.cursor.saturating_sub(1);
                Action::None
            }
            CanopyKey::Enter => Action::SwitchProfile,
            CanopyKey::Char('n') => {
                app.profile_menu.stage = MenuStage::NamingCreate;
                app.profile_menu.name_error = false;
                app.command_line.clear();
                Action::None
            }
            CanopyKey::Char('s') => {
                if let Some(p) = app.profiles.get(app.profile_menu.cursor) {
                    enter_profile_settings(app, p.profile_id.clone());
                }
                Action::None
            }
            CanopyKey::Char('d') => {
                // The in-hub shortcut keeps the M5.10 grammar; the guards
                // refuse here first (ADR-0036 §5).
                if let Some(p) = app.profiles.get(app.profile_menu.cursor) {
                    if p.profile_id != app.active_profile && app.profiles.len() > 1 {
                        app.profile_menu.stage = MenuStage::ConfirmDelete {
                            profile: p.profile_id.clone(),
                            back_to_settings: false,
                        };
                    }
                }
                Action::None
            }
            CanopyKey::Esc => {
                app.mode = Mode::Normal;
                app.profile_menu.reset();
                app.command_line.clear();
                Action::None
            }
            _ => Action::None,
        },
        MenuStage::Settings { profile } => {
            let rows_last = SettingsRow::ALL.len() - 1;
            match key {
                CanopyKey::Char('j') | CanopyKey::Down => {
                    app.profile_menu.settings_cursor =
                        (app.profile_menu.settings_cursor + 1).min(rows_last);
                    Action::None
                }
                CanopyKey::Char('k') | CanopyKey::Up => {
                    app.profile_menu.settings_cursor =
                        app.profile_menu.settings_cursor.saturating_sub(1);
                    Action::None
                }
                CanopyKey::Enter => SettingsRow::ALL
                    .get(app.profile_menu.settings_cursor.min(rows_last))
                    .copied()
                    .map_or(Action::None, |row| {
                        activate_settings_row(app, &profile, row)
                    }),
                CanopyKey::Esc => {
                    app.profile_menu.stage = MenuStage::Browse;
                    Action::None
                }
                _ => Action::None,
            }
        }
        MenuStage::Members { profile } => match key {
            CanopyKey::Char('j') | CanopyKey::Down => {
                let last = app.profile_menu.members.len().saturating_sub(1);
                app.profile_menu.member_cursor = (app.profile_menu.member_cursor + 1).min(last);
                Action::None
            }
            CanopyKey::Char('k') | CanopyKey::Up => {
                app.profile_menu.member_cursor = app.profile_menu.member_cursor.saturating_sub(1);
                Action::None
            }
            CanopyKey::Char('d') => Action::MemberRemove,
            CanopyKey::ShiftUp => Action::MemberReorder(true),
            CanopyKey::ShiftDown => Action::MemberReorder(false),
            CanopyKey::Char('a') => {
                app.profile_menu.stage = MenuStage::MembersAdd { profile };
                app.profile_menu.add_cursor = 0;
                Action::None
            }
            CanopyKey::Esc => {
                app.profile_menu.stage = MenuStage::Settings { profile };
                Action::None
            }
            _ => Action::None,
        },
        MenuStage::MembersAdd { profile } => match key {
            CanopyKey::Char('j') | CanopyKey::Down => {
                let last = app.profile_menu.candidates().len().saturating_sub(1);
                app.profile_menu.add_cursor = (app.profile_menu.add_cursor + 1).min(last);
                Action::None
            }
            CanopyKey::Char('k') | CanopyKey::Up => {
                app.profile_menu.add_cursor = app.profile_menu.add_cursor.saturating_sub(1);
                Action::None
            }
            CanopyKey::Enter => Action::MemberAdd,
            CanopyKey::Esc => {
                app.profile_menu.stage = MenuStage::Members { profile };
                Action::None
            }
            _ => Action::None,
        },
        MenuStage::NamingCreate
        | MenuStage::NamingRename { .. }
        | MenuStage::NamingDuplicate { .. } => match key {
            CanopyKey::Char(c) => {
                app.command_line.push(c);
                app.profile_menu.name_error = false;
                Action::None
            }
            CanopyKey::Backspace => {
                app.command_line.pop();
                app.profile_menu.name_error = false;
                Action::None
            }
            CanopyKey::Esc => {
                // Naming steps back to where it was opened from: create to
                // Browse, rename/duplicate to their settings page.
                app.profile_menu.stage = match app.profile_menu.stage.clone() {
                    MenuStage::NamingRename { profile } => MenuStage::Settings { profile },
                    MenuStage::NamingDuplicate { source } => {
                        MenuStage::Settings { profile: source }
                    }
                    _ => MenuStage::Browse,
                };
                app.profile_menu.name_error = false;
                app.command_line.clear();
                Action::None
            }
            CanopyKey::Enter => submit_profile_name(app),
            _ => Action::None,
        },
        MenuStage::ConfirmDelete {
            profile,
            back_to_settings,
        } => match key {
            // Only Enter lands the delete — the explicit second act. A landed
            // delete always returns to Browse: the page's profile is gone.
            CanopyKey::Enter => {
                app.profile_menu.stage = MenuStage::Browse;
                Action::DeleteProfile(profile)
            }
            CanopyKey::Esc | CanopyKey::Char('n') => {
                app.profile_menu.stage = if back_to_settings {
                    MenuStage::Settings { profile }
                } else {
                    MenuStage::Browse
                };
                Action::None
            }
            _ => Action::None,
        },
    }
}

/// The colour-picker reducer (ADR-0039): stage one moves over the five slots,
/// stage two over the ten presets + the reset row. `Enter` selects (a preset
/// application returns the typed action — the state layer re-themes in the same
/// frame and appends the superseding event); `Esc` steps back
/// (presets → slots → the profile menu). Pure over `App.colour_picker`.
fn on_key_colour_picker(app: &mut App, key: CanopyKey) -> Action {
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
            // again supersedes again; Esc afterwards keeps it (ADR-0039 §3).
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

/// Validate and submit the naming buffer (create, rename, or duplicate). A
/// refused name — empty, over the 24-char budget, or a duplicate among live
/// profiles — marks the inline error and keeps the input open; a valid one
/// emits the typed action and returns to its origin: create and duplicate to
/// Browse (the reducer never learns the minted id — ADR-0044), rename to its
/// settings page.
fn submit_profile_name(app: &mut App) -> Action {
    use crate::profile_menu::MenuStage;
    let except = match &app.profile_menu.stage {
        MenuStage::NamingRename { profile } => Some(profile.as_str()),
        _ => None,
    };
    let Some(name) =
        crate::profile::normalize_profile_name(&app.command_line, &app.profiles, except)
    else {
        app.profile_menu.name_error = true;
        return Action::None;
    };
    let stage = std::mem::take(&mut app.profile_menu.stage);
    app.command_line.clear();
    app.profile_menu.name_error = false;
    match stage {
        MenuStage::NamingCreate => Action::CreateProfile(name),
        MenuStage::NamingRename { profile } => {
            app.profile_menu.stage = MenuStage::Settings {
                profile: profile.clone(),
            };
            Action::RenameProfile { profile, name }
        }
        MenuStage::NamingDuplicate { source } => Action::DuplicateProfile { source, name },
        // Unreachable: only the naming stages call this.
        other => {
            app.profile_menu.stage = other;
            Action::None
        }
    }
}

/// Route a vertical nav key to the focused panel: the projects list moves its
/// selection; the detail panel moves the feature-toggle cursor; the graphs panel
/// scrolls its viewport. `page` selects a larger jump. Pure — no I/O.
fn nav_vertical(app: &mut App, down: bool, page: bool) {
    use crate::Panel;
    match app.focus {
        Panel::Projects => {
            let steps = if page { 10 } else { 1 };
            for _ in 0..steps {
                if down {
                    app.select_next();
                } else {
                    app.select_prev();
                }
            }
        }
        Panel::Detail => app.move_toggle_cursor(down),
        Panel::Graphs => app.scroll_focused(down, page),
        Panel::Command => {}
    }
}

fn on_key_editing(app: &mut App, key: CanopyKey) -> Action {
    match key {
        CanopyKey::Char(c) => {
            app.command_line.push(c);
            app.command_error = None; // an edit clears the inline error
            app.suggest_highlight = 0; // the list changed under the cursor
            Action::None
        }
        CanopyKey::Backspace => {
            app.command_line.pop();
            app.command_error = None;
            app.suggest_highlight = 0;
            Action::None
        }
        // The suggest panel's keys (M5-11): arrows highlight; Tab INSERTS the
        // highlight and never executes — completion is always safe to press.
        CanopyKey::Down if app.mode == Mode::Command => {
            let len = crate::current_suggestions(app).len();
            if len > 0 {
                app.suggest_highlight = (app.suggest_highlight + 1).min(len - 1);
            }
            Action::None
        }
        CanopyKey::Up if app.mode == Mode::Command => {
            app.suggest_highlight = app.suggest_highlight.saturating_sub(1);
            Action::None
        }
        CanopyKey::Tab if app.mode == Mode::Command => {
            insert_suggestion(app);
            Action::None
        }
        CanopyKey::Esc | CanopyKey::Quit => {
            app.mode = Mode::Normal;
            app.command_line.clear();
            app.command_error = None;
            Action::None
        }
        CanopyKey::Enter => match app.mode {
            Mode::AddProject => {
                let buf = std::mem::take(&mut app.command_line);
                app.mode = Mode::Normal;
                let trimmed = buf.trim();
                if trimmed.is_empty() {
                    Action::None
                } else {
                    Action::AddProject(trimmed.to_string())
                }
            }
            // The reducer parses (span errors render inline and keep the
            // buffer); on success it clears the buffer + mode itself.
            Mode::Command => Action::RunLine(app.command_line.clone()),
            // on_key_editing is only reached in Command/AddProject; the others
            // are inert here (PanelEditor/ProfileMenu/ColourPicker route to
            // their own reducers).
            Mode::Normal | Mode::PanelEditor | Mode::ProfileMenu | Mode::ColourPicker => {
                Action::None
            }
        },
        // Navigation/focus keys are inert while editing the command/path buffer.
        _ => Action::None,
    }
}

/// Insert the highlighted suggestion into the buffer (Tab and the mouse
/// click share this — insert-only, never execution).
pub(crate) fn insert_suggestion(app: &mut App) {
    let suggestions = crate::current_suggestions(app);
    if let Some(sug) = suggestions.get(
        app.suggest_highlight
            .min(suggestions.len().saturating_sub(1)),
    ) {
        app.command_line.clone_from(&sug.insert);
        app.command_error = None;
        app.suggest_highlight = 0;
    }
}

/// Keys while an overlay is open — the modal owns them and its focus ring
/// (ADR-0027). Tab / Shift-Tab move focus across the modal's controls;
/// Enter / Space activate the focused control (a mutating Confirm needs Enter —
/// Space never confirms it, so a buffered keystroke can't); Esc always
/// dismisses a Results/Confirm; `j`/`k`/arrows and the wheel scroll a Results
/// body regardless of focus. Running swallows everything.
fn on_key_overlay(app: &mut App, key: CanopyKey) -> Action {
    use crate::dispatch::{Overlay, OverlayControl};
    // Focus-ring traversal is available in every modal that has controls.
    match key {
        CanopyKey::Tab => {
            app.cycle_overlay_focus(true);
            return Action::None;
        }
        CanopyKey::BackTab => {
            app.cycle_overlay_focus(false);
            return Action::None;
        }
        _ => {}
    }
    // Results keeps its j/k (and arrow) scroll whichever control is focused.
    if let Some(Overlay::Results { scroll, lines, .. }) = app.overlay.as_mut() {
        match key {
            CanopyKey::Char('j') | CanopyKey::Down => {
                *scroll = scroll.saturating_add(1).min(lines.len().saturating_sub(1));
                return Action::None;
            }
            CanopyKey::Char('k') | CanopyKey::Up => {
                *scroll = scroll.saturating_sub(1);
                return Action::None;
            }
            _ => {}
        }
    }
    // A normalized hard quit (Ctrl-C) exits the app even from a modal — an
    // open message must never trap the escape hatch.
    if matches!(key, CanopyKey::Quit) {
        app.quit = true;
        return Action::Quit;
    }
    // Esc closes/cancels a dismissable modal (ADR-0027) — results, confirm,
    // alert, notice, the wizard; only Running swallows it.
    if matches!(key, CanopyKey::Esc) {
        if !matches!(app.overlay, Some(Overlay::Running { .. })) {
            app.overlay = None;
            app.overlay_focus = 0;
        }
        return Action::None;
    }
    // Enter / Space activate the focused control. Space never activates the
    // mutating Confirm — only Enter does — so a stray Space can't run it.
    if matches!(key, CanopyKey::Enter | CanopyKey::Char(' ')) {
        let is_space = matches!(key, CanopyKey::Char(' '));
        return match app.focused_overlay_control() {
            Some(OverlayControl::Confirm) if !is_space => Action::ConfirmDispatch,
            Some(OverlayControl::Cancel | OverlayControl::Close) => {
                app.overlay = None;
                app.overlay_focus = 0;
                Action::None
            }
            // The wizard's *add a project* action (ADR-0029): dismiss the modal
            // and enter add-project mode (the `a` key's effect, from the wizard).
            Some(OverlayControl::Primary) => {
                app.start_add_project();
                Action::None
            }
            _ => Action::None,
        };
    }
    Action::None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EntryKind, Panel, Project, ProjectStatus};

    fn project(name: &str) -> Project {
        Project {
            name: name.into(),
            path: format!("/r/{name}"),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }
    }

    /// An app past the splash, ready for Normal-mode interaction (the first-run
    /// wizard, which an empty registry raises, is dismissed so keys reach the
    /// dashboard — overlay-key tests open their own overlay).
    fn ready(projects: Vec<Project>) -> App {
        let mut app = App::new(projects);
        app.splash.skip();
        app.overlay = None;
        app
    }

    fn feed(app: &mut App, keys: &[CanopyKey]) -> Action {
        let mut last = Action::None;
        for &k in keys {
            last = on_key(app, k);
        }
        last
    }

    #[test]
    fn first_key_only_dismisses_splash() {
        let mut app = App::new(vec![]); // fresh splash, not done
        assert!(!app.splash.done());
        let action = on_key(&mut app, CanopyKey::Char('a'));
        assert_eq!(action, Action::None);
        assert!(app.splash.done());
        assert_eq!(
            app.mode,
            Mode::Normal,
            "splash key is swallowed, not acted on"
        );
    }

    #[test]
    fn add_project_flow_yields_path() {
        let mut app = ready(vec![]);
        assert_eq!(on_key(&mut app, CanopyKey::Char('a')), Action::None);
        assert_eq!(app.mode, Mode::AddProject);
        let keys = [
            CanopyKey::Char('/'),
            CanopyKey::Char('t'),
            CanopyKey::Char('m'),
            CanopyKey::Char('p'),
        ];
        feed(&mut app, &keys);
        let action = on_key(&mut app, CanopyKey::Enter);
        assert_eq!(action, Action::AddProject("/tmp".to_string()));
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.command_line.is_empty());
    }

    #[test]
    fn empty_add_project_is_noop() {
        let mut app = ready(vec![]);
        on_key(&mut app, CanopyKey::Char('a'));
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::None);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn slash_quit_command_quits() {
        let mut app = ready(vec![]);
        on_key(&mut app, CanopyKey::Char('/'));
        feed(
            &mut app,
            &[
                CanopyKey::Char('q'),
                CanopyKey::Char('u'),
                CanopyKey::Char('i'),
                CanopyKey::Char('t'),
            ],
        );
        // Parsing moved to the reducer (M5-07): Enter submits the raw line;
        // the reducer's `/quit` handling produces Flow::Quit (covered in the
        // state tests).
        assert_eq!(
            on_key(&mut app, CanopyKey::Enter),
            Action::RunLine("quit".into())
        );
    }

    #[test]
    fn slash_other_command_runs() {
        let mut app = ready(vec![]);
        on_key(&mut app, CanopyKey::Char('/'));
        feed(
            &mut app,
            &[
                CanopyKey::Char('s'),
                CanopyKey::Char('t'),
                CanopyKey::Char('a'),
                CanopyKey::Char('t'),
                CanopyKey::Char('u'),
                CanopyKey::Char('s'),
            ],
        );
        match on_key(&mut app, CanopyKey::Enter) {
            Action::RunLine(buf) => assert_eq!(buf, "status"),
            other => panic!("expected RunLine, got {other:?}"),
        }
    }

    #[test]
    fn esc_cancels_editing() {
        let mut app = ready(vec![]);
        on_key(&mut app, CanopyKey::Char('a'));
        feed(&mut app, &[CanopyKey::Char('x'), CanopyKey::Char('y')]);
        assert_eq!(on_key(&mut app, CanopyKey::Esc), Action::None);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.command_line.is_empty());
    }

    #[test]
    fn selection_is_bounded() {
        let mut app = ready(vec![project("a"), project("b")]);
        assert_eq!(on_key(&mut app, CanopyKey::Char('k')), Action::None); // already top
        assert_eq!(app.selected, 0);
        on_key(&mut app, CanopyKey::Down);
        assert_eq!(app.selected, 1);
        on_key(&mut app, CanopyKey::Char('j')); // saturates
        assert_eq!(app.selected, 1);
        on_key(&mut app, CanopyKey::Up);
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn q_quits_in_normal_mode() {
        let mut app = ready(vec![]);
        assert_eq!(on_key(&mut app, CanopyKey::Char('q')), Action::Quit);
        assert!(app.quit);
    }

    #[test]
    fn remove_and_reorder_keys_act_on_focused_projects() {
        let mut app = ready(vec![project("a"), project("b")]);
        assert_eq!(app.focus, Panel::Projects);
        assert_eq!(
            on_key(&mut app, CanopyKey::Char('d')),
            Action::RemoveProject
        );
        assert_eq!(
            on_key(&mut app, CanopyKey::Char('x')),
            Action::RemoveProject
        );
        assert_eq!(
            on_key(&mut app, CanopyKey::ShiftUp),
            Action::ReorderProject(true)
        );
        assert_eq!(
            on_key(&mut app, CanopyKey::ShiftDown),
            Action::ReorderProject(false)
        );
    }

    #[test]
    fn remove_and_reorder_are_inert_when_projects_unfocused() {
        let mut app = ready(vec![project("a")]);
        app.set_focus(Panel::Detail);
        assert_eq!(on_key(&mut app, CanopyKey::Char('d')), Action::None);
        assert_eq!(on_key(&mut app, CanopyKey::ShiftUp), Action::None);
    }

    #[test]
    fn remove_is_inert_with_no_projects() {
        let mut app = ready(vec![]);
        assert_eq!(on_key(&mut app, CanopyKey::Char('d')), Action::None);
    }

    #[test]
    fn tab_navigation_order_through_on_key() {
        let mut app = ready(vec![project("a")]);
        assert_eq!(app.focus, Panel::Projects);
        for expected in [
            Panel::Detail,
            Panel::Graphs,
            Panel::Command,
            Panel::Projects,
        ] {
            assert_eq!(on_key(&mut app, CanopyKey::Tab), Action::None);
            assert_eq!(app.focus, expected);
        }
        // Shift-Tab reverses.
        assert_eq!(on_key(&mut app, CanopyKey::BackTab), Action::None);
        assert_eq!(app.focus, Panel::Command);
    }

    #[test]
    fn arrows_scroll_focused_graphs_panel() {
        let mut app = ready(vec![project("a"), project("b")]);
        app.set_focus(Panel::Graphs);
        on_key(&mut app, CanopyKey::Down);
        assert_eq!(
            app.scroll.graphs, 1,
            "Down scrolls the focused graphs panel"
        );
        assert_eq!(
            app.selected, 0,
            "selection is unchanged when Projects isn't focused"
        );
        on_key(&mut app, CanopyKey::Up);
        assert_eq!(app.scroll.graphs, 0);
    }

    #[test]
    fn arrows_move_toggle_cursor_when_detail_focused() {
        let mut app = ready(vec![project("a")]);
        app.set_focus(Panel::Detail);
        assert_eq!(app.toggle_cursor, 0);
        on_key(&mut app, CanopyKey::Down);
        assert_eq!(app.toggle_cursor, 1, "Down moves the toggle cursor");
        on_key(&mut app, CanopyKey::Up);
        assert_eq!(app.toggle_cursor, 0);
        assert_eq!(app.scroll.detail, 0, "detail no longer scrolls via arrows");
    }

    #[test]
    fn enter_on_the_rail_focuses_detail() {
        // Selection and inspection are one motion (M55-05): Enter on the rail
        // focuses the detail column; the selection is untouched.
        let mut app = ready(vec![project("a"), project("b")]);
        assert_eq!(app.focus, Panel::Projects);
        on_key(&mut app, CanopyKey::Down);
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::None);
        assert_eq!(app.focus, Panel::Detail);
        assert_eq!(app.selected, 1, "the selection stays put");
        // Existing behavior unchanged: Enter in the detail column toggles.
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::ToggleFeature);
    }

    #[test]
    fn detail_focus_keys_flip_and_retry_toggles() {
        let mut app = ready(vec![project("a")]);
        app.set_focus(Panel::Detail);
        assert_eq!(
            on_key(&mut app, CanopyKey::Char(' ')),
            Action::ToggleFeature
        );
        assert_eq!(
            on_key(&mut app, CanopyKey::Char('t')),
            Action::ToggleFeature
        );
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::ToggleFeature);
        assert_eq!(on_key(&mut app, CanopyKey::Char('r')), Action::RetryFeature);
        // Inert when the detail panel is not focused.
        app.set_focus(Panel::Projects);
        assert_eq!(on_key(&mut app, CanopyKey::Char('t')), Action::None);
    }

    #[test]
    fn focus_returns_to_previous_on_command_cancel() {
        let mut app = ready(vec![project("a")]);
        app.set_focus(Panel::Graphs);
        on_key(&mut app, CanopyKey::Char('/')); // focuses Command, enters Command mode
        assert_eq!(app.focus, Panel::Command);
        assert_eq!(app.mode, Mode::Command);
        assert_eq!(app.focus_prev, Panel::Graphs);
    }

    // ---- M56-08: message-modal keys (ADR-0029) -----------------------------

    #[test]
    fn wizard_primary_enter_begins_add_project() {
        let mut app = ready(vec![project("a")]);
        app.overlay = Some(crate::dispatch::Overlay::wizard()); // focus = Primary
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::None);
        assert_eq!(app.mode, Mode::AddProject, "Enter fired the wizard action");
        assert!(app.overlay.is_none(), "the wizard dismissed");
    }

    #[test]
    fn esc_dismisses_an_alert_and_a_notice() {
        for ov in [
            crate::dispatch::Overlay::alert(
                "e",
                vec!["x".into()],
                crate::dispatch::Severity::Error,
            ),
            crate::dispatch::Overlay::notice("ok", vec!["done".into()]),
        ] {
            let mut app = ready(vec![project("a")]);
            app.overlay = Some(ov);
            assert_eq!(on_key(&mut app, CanopyKey::Esc), Action::None);
            assert!(app.overlay.is_none(), "Esc closes every message modal");
        }
    }

    #[test]
    fn ctrl_c_quits_even_from_a_modal() {
        // A hard quit must never be trapped by an open message (the escape hatch).
        let mut app = ready(vec![project("a")]);
        app.overlay = Some(crate::dispatch::Overlay::wizard());
        assert_eq!(on_key(&mut app, CanopyKey::Quit), Action::Quit);
        assert!(app.quit, "Ctrl-C sets quit even with a modal open");
    }

    // ---- ADR-0037: the profile menu's state machine -------------------------

    /// An app with three live profiles (active first) past the splash.
    fn with_profiles() -> App {
        let mut app = ready(vec![project("a")]);
        app.profiles = ["default", "work", "lab"]
            .iter()
            .enumerate()
            .map(|(i, name)| crate::profile::ProfileEntry {
                profile_id: format!("pf{i}"),
                name: (*name).to_string(),
                created_at: i64::try_from(i).unwrap(),
            })
            .collect();
        app.active_profile = "pf0".into();
        app
    }

    #[test]
    fn p_opens_the_menu_on_the_active_row_and_esc_closes() {
        let mut app = with_profiles();
        app.active_profile = "pf1".into();
        assert_eq!(on_key(&mut app, CanopyKey::Char('p')), Action::None);
        assert_eq!(app.mode, Mode::ProfileMenu);
        assert_eq!(app.profile_menu.cursor, 1, "opens on the active row");
        assert_eq!(on_key(&mut app, CanopyKey::Esc), Action::None);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn p_is_inert_until_the_profile_plane_is_wired() {
        let mut app = ready(vec![project("a")]); // no profiles
        assert_eq!(on_key(&mut app, CanopyKey::Char('p')), Action::None);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn menu_cursor_moves_and_clamps() {
        let mut app = with_profiles();
        on_key(&mut app, CanopyKey::Char('p'));
        assert_eq!(app.profile_menu.cursor, 0);
        on_key(&mut app, CanopyKey::Char('k')); // clamps at the top
        assert_eq!(app.profile_menu.cursor, 0);
        on_key(&mut app, CanopyKey::Char('j'));
        on_key(&mut app, CanopyKey::Down);
        assert_eq!(app.profile_menu.cursor, 2);
        on_key(&mut app, CanopyKey::Char('j')); // clamps at the bottom
        assert_eq!(app.profile_menu.cursor, 2);
        on_key(&mut app, CanopyKey::Up);
        assert_eq!(app.profile_menu.cursor, 1);
    }

    #[test]
    fn enter_in_browse_switches() {
        let mut app = with_profiles();
        on_key(&mut app, CanopyKey::Char('p'));
        on_key(&mut app, CanopyKey::Char('j'));
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::SwitchProfile);
    }

    #[test]
    fn naming_create_validates_then_emits() {
        use crate::profile_menu::MenuStage;
        let mut app = with_profiles();
        on_key(&mut app, CanopyKey::Char('p'));
        on_key(&mut app, CanopyKey::Char('n'));
        assert_eq!(app.profile_menu.stage, MenuStage::NamingCreate);
        // A duplicate name is refused inline; the input stays open.
        for c in "work".chars() {
            on_key(&mut app, CanopyKey::Char(c));
        }
        assert_eq!(on_key(&mut app, CanopyKey::Enter), Action::None);
        assert!(app.profile_menu.name_error, "duplicate → inline error");
        assert_eq!(app.profile_menu.stage, MenuStage::NamingCreate);
        // Edit to a valid (trimmed) name and submit.
        for _ in 0..4 {
            on_key(&mut app, CanopyKey::Backspace);
        }
        for c in " client ".chars() {
            on_key(&mut app, CanopyKey::Char(c));
        }
        assert_eq!(
            on_key(&mut app, CanopyKey::Enter),
            Action::CreateProfile("client".into())
        );
        assert_eq!(app.profile_menu.stage, MenuStage::Browse);
        assert!(app.command_line.is_empty());
    }

    #[test]
    fn naming_rename_prefills_and_emits() {
        use crate::profile_menu::MenuStage;
        let mut app = with_profiles();
        on_key(&mut app, CanopyKey::Char('p'));
        on_key(&mut app, CanopyKey::Char('j')); // cursor → "work"
        on_key(&mut app, CanopyKey::Char('s')); // its settings page
        on_key(&mut app, CanopyKey::Enter); // the Name row
        assert_eq!(app.command_line, "work", "rename edits, not retypes");
        for c in "shop".chars() {
            on_key(&mut app, CanopyKey::Char(c));
        }
        assert_eq!(
            on_key(&mut app, CanopyKey::Enter),
            Action::RenameProfile {
                profile: "pf1".into(),
                name: "workshop".into()
            }
        );
        // A landed rename returns to the settings page it came from.
        assert_eq!(
            app.profile_menu.stage,
            MenuStage::Settings {
                profile: "pf1".into()
            }
        );
    }

    #[test]
    fn naming_esc_steps_back_to_browse() {
        use crate::profile_menu::MenuStage;
        let mut app = with_profiles();
        on_key(&mut app, CanopyKey::Char('p'));
        on_key(&mut app, CanopyKey::Char('n'));
        on_key(&mut app, CanopyKey::Char('x'));
        on_key(&mut app, CanopyKey::Esc);
        assert_eq!(app.profile_menu.stage, MenuStage::Browse);
        assert_eq!(app.mode, Mode::ProfileMenu, "one step back, not closed");
        assert!(app.command_line.is_empty());
    }

    #[test]
    fn delete_needs_the_inline_confirm_and_guards_refuse() {
        use crate::profile_menu::MenuStage;
        let mut app = with_profiles();
        on_key(&mut app, CanopyKey::Char('p'));
        // `d` on the ACTIVE profile is refused — the confirm can't open.
        assert_eq!(on_key(&mut app, CanopyKey::Char('d')), Action::None);
        assert_eq!(app.profile_menu.stage, MenuStage::Browse);
        // On a non-active row it opens the confirm; Esc cancels…
        on_key(&mut app, CanopyKey::Char('j'));
        on_key(&mut app, CanopyKey::Char('d'));
        assert_eq!(
            app.profile_menu.stage,
            MenuStage::ConfirmDelete {
                profile: "pf1".into(),
                back_to_settings: false
            }
        );
        on_key(&mut app, CanopyKey::Esc);
        assert_eq!(app.profile_menu.stage, MenuStage::Browse);
        // …and only Enter lands it (a stray key can't confirm).
        on_key(&mut app, CanopyKey::Char('d'));
        assert_eq!(on_key(&mut app, CanopyKey::Char('x')), Action::None);
        assert_eq!(
            on_key(&mut app, CanopyKey::Enter),
            Action::DeleteProfile("pf1".into())
        );
    }

    #[test]
    fn delete_refuses_the_last_profile() {
        let mut app = with_profiles();
        app.profiles.truncate(1);
        app.active_profile = String::new(); // not active, still the last
        on_key(&mut app, CanopyKey::Char('p'));
        assert_eq!(on_key(&mut app, CanopyKey::Char('d')), Action::None);
        assert_eq!(
            app.profile_menu.stage,
            crate::profile_menu::MenuStage::Browse
        );
    }

    #[test]
    fn ctrl_c_quits_even_from_the_menu() {
        let mut app = with_profiles();
        on_key(&mut app, CanopyKey::Char('p'));
        assert_eq!(on_key(&mut app, CanopyKey::Quit), Action::Quit);
        assert!(app.quit);
    }

    // ---- ADR-0039: the colour picker's two stages ----------------------------

    /// Open the picker the ADR-0044 way: the settings page's Colours row
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

    proptest::proptest! {
        /// Any key sequence is panic-free and never drives the selection out of
        /// bounds (the reducer never mutates the project list itself).
        #[test]
        fn arbitrary_keys_keep_selection_in_bounds(
            keys in proptest::collection::vec(key_strategy(), 0..60)
        ) {
            let mut app = ready(vec![project("a"), project("b")]);
            for k in keys {
                let _ = on_key(&mut app, k);
            }
            proptest::prop_assert!(app.selected < app.projects.len());
        }
    }

    fn key_strategy() -> impl proptest::strategy::Strategy<Value = CanopyKey> {
        use proptest::prelude::*;
        prop_oneof![
            any::<char>().prop_map(CanopyKey::Char),
            Just(CanopyKey::Enter),
            Just(CanopyKey::Esc),
            Just(CanopyKey::Backspace),
            Just(CanopyKey::Up),
            Just(CanopyKey::Down),
            Just(CanopyKey::Tab),
            Just(CanopyKey::BackTab),
            Just(CanopyKey::Left),
            Just(CanopyKey::Right),
            Just(CanopyKey::PageUp),
            Just(CanopyKey::PageDown),
            Just(CanopyKey::ShiftUp),
            Just(CanopyKey::ShiftDown),
            Just(CanopyKey::Quit),
        ]
    }
}
