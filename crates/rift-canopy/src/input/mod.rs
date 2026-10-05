//! The pure interaction reducer: keys → state transitions + side-effect actions.
//!
//! All of Canopy's interactivity lives here as a terminal-free function over
//! [`App`], so the dashboard's behavior (selection, the add-project prompt, the
//! slash palette, quitting) is unit- and property-testable without a real
//! terminal. The xre backend (`crate::xre`) only translates its event stream
//! into [`CanopyKey`] and renders the resulting state — it holds no logic of its
//! own.
//!
//! The vocabulary the backend translates into and the dispatcher that routes a
//! key by [`Mode`] live here; each mode's handler is a module of its own, so a
//! mode's key table reads in one place.

mod colour_picker;
mod editing;
mod explorer;
mod normal;
mod overlay;
mod panel_editor;
mod profile_menu;
mod project_settings;

#[cfg(test)]
mod fixtures;

use crate::App;

use colour_picker::on_key_colour_picker;
use editing::on_key_editing;
use explorer::on_key_explorer;
use normal::on_key_normal;
use overlay::on_key_overlay;
use panel_editor::on_key_editor;
use profile_menu::on_key_profile_menu;
use project_settings::on_key_project_settings;

pub use explorer::enter_explorer;
pub use panel_editor::enter_panel_editor;
pub use profile_menu::enter_profile_menu;

pub(crate) use editing::insert_suggestion;
pub(crate) use explorer::{
    explorer_descend, explorer_select, explorer_toggle_hidden, explorer_type_path, explorer_up,
};
pub(crate) use profile_menu::{activate_settings_row, enter_profile_settings};

/// Which editing/interaction mode the dashboard is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Navigating: arrow/`j`/`k` select, `a` adds a project, `/` opens the palette.
    Normal,
    /// Typing a slash command into the bottom palette.
    Command,
    /// Typing a path to register a new project.
    AddProject,
    /// Rearranging the dashboard layout: grips on every panel, the
    /// cursor moves/reorders/resizes, `Esc` or the toggle exits.
    PanelEditor,
    /// Managing profiles (the profile menu and header identity): a centered menu over the shared modal
    /// geometry — browse/switch/create/rename/delete, `Esc` steps back.
    ProfileMenu,
    /// Picking a profile's theme colours (the colour picker and its presets): two stages — the five
    /// slots, then the ten presets + reset — with the dashboard as the live
    /// preview; `Esc` steps back (presets → slots → the profile menu).
    ColourPicker,
    /// Browsing for a project's destination: a scrolling mode-modal
    /// over the shared modal geometry — walk the directories, `s` registers the
    /// folder currently open, `i` falls back to typing a path, `Esc` closes.
    Explorer,
    /// Repository-owned Project Settings: policy, capture, Recall, security,
    /// and all native adapters in one scrolling modal.
    ProjectSettings,
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
    /// Jump to the first item/line.
    Home,
    /// Jump to the last item/line.
    End,
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
    /// Open Project Settings for the selected repository.
    OpenProjectSettings,
    /// Open the selected repository in the read-only Recall Viewer.
    OpenRecallViewer,
    /// Open the palette help overlay — the keyboard twin of the header's
    /// `? Help`, folded by the reducer through the same opener the click uses.
    OpenHelp,
    /// Open the graphs column's "by worktree" axis (the family view) as the
    /// existing results overlay. An action rather than a state mutation
    /// because the axis is column-3 data, which the reducer cannot see.
    OpenWorktreeAxis,
    /// Open the Recall card's savings breakdown — the keyboard twin of its
    /// headline, and an action for the same reason `OpenWorktreeAxis` is.
    OpenSavingsBreakdown,
    /// Recompute the Recall card over this period. An action rather than a
    /// state mutation for the same reason: the chosen window lives on the
    /// column-3 model, which this reducer does not hold.
    SetRecallWindow(crate::col3::RecallWindow),
    /// Apply one Project Settings lifecycle operation.
    ApplyProjectSettings(crate::ProjectSettingsAction),
    /// Submit the command buffer (the reducer parses; errors render inline).
    RunLine(String),
    /// The confirm overlay's Enter/Confirm — dispatch the pending request.
    ConfirmDispatch,
    /// Switch to the profile under the menu cursor (the whole view
    /// follows — membership, selection, arrangement).
    SwitchProfile,
    /// Create a profile with the validated name (copying the active profile's
    /// project list at creation — the profile plane).
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
    /// its effective state.
    DuplicateProfile {
        /// The stable id of the profile being cloned.
        source: String,
        /// The validated new display name.
        name: String,
    },
    /// Flip the startup pin for a profile: pin it when it is
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
    /// The picker opened for a profile: the state layer seeds
    /// the target's overrides — purely from the live theme when the target is
    /// active, else through the loader seam.
    OpenColours(String),
    /// Wire the selected linked worktree for the family's enabled agents
    /// (the family view). Raised by `A` and by a click on the Repository
    /// card's status row; the state layer confirms before dispatching.
    AdoptWorktree,
    /// List a directory's immediate children through the host's lister seam
    /// — raised on entering the explorer and on every traversal.
    /// The reducer never reads the filesystem; the loop performs the walk and
    /// lands the reply on `App.explorer` before the next frame.
    ExplorerList(std::path::PathBuf),
    /// Apply one theme-slot selection for the active profile (the colour picker and its presets):
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
    // An open overlay owns the keys (the injected dispatcher): Results scrolls/dismisses,
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
        Mode::Explorer => on_key_explorer(app, key),
        Mode::ProjectSettings => on_key_project_settings(app, key),
    }
}

/// Whether the selected row is a member with nothing wired in it — the one
/// premise the adopt act is offered on (the family view's `NotAdopted`).
#[must_use]
pub fn selection_is_unadopted(app: &App) -> bool {
    app.selected_project()
        .is_some_and(|p| p.status == crate::ProjectStatus::NotAdopted)
}

#[cfg(test)]
mod tests {
    use super::fixtures::{project, ready};
    use super::*;

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
