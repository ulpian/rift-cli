//! The profile menu's interaction state (ADR-0037).
//!
//! The menu is a **mode**, not an `Overlay`: its state — a cursor over the live
//! profiles, a naming input, a pending delete — lives here, in the mode's own
//! state machine (`browse | naming(create | rename) | confirm-delete`), and its
//! reducer branch runs over synthetic keys like every other mode. The closed
//! `Overlay`/`MessageAction` enums are untouched ("a message is data, not a
//! capability" — ADR-0029). The naming buffer itself reuses `App::command_line`
//! (the AddProject precedent), so the editing input path stays single.

/// Which stage the profile menu is in (ADR-0037 §2). `Esc` steps back one
/// stage: naming/confirm → browse → closed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MenuStage {
    /// Moving the cursor over the live profiles (`j`/`k`; `Enter` switches).
    #[default]
    Browse,
    /// Typing a name for a new profile (`n`) — create-with-copy on submit.
    NamingCreate,
    /// Typing a new name for the profile the cursor was on (`r`).
    NamingRename {
        /// The stable id of the profile being renamed.
        profile: String,
    },
    /// The inline delete confirm (`d`) — an explicit second act inside the
    /// menu, never a `MessageAction` (ADR-0037 §3).
    ConfirmDelete {
        /// The stable id of the profile pending removal.
        profile: String,
    },
}

/// The profile menu's transient state — only meaningful while
/// `mode == Mode::ProfileMenu`; resting (`Default`) otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileMenuState {
    /// The highlighted row (an index into `App::profiles`).
    pub cursor: usize,
    /// The current stage of the menu's state machine.
    pub stage: MenuStage,
    /// The last refused name, rendered inline until the next edit (the name
    /// rules are trimmed, non-empty, ≤ 24 chars, unique — ADR-0037 §4).
    pub name_error: bool,
}

impl ProfileMenuState {
    /// Reset to the resting state (leaving the menu or entering it fresh).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
