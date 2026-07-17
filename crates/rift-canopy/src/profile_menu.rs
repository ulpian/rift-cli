//! The profile menu's interaction state (ADR-0037, hub-and-spoke per
//! ADR-0044).
//!
//! The menu is a **mode**, not an `Overlay`: its state — a cursor over the live
//! profiles, a settings page, a naming input, a pending delete — lives here, in
//! the mode's own state machine, and its reducer branch runs over synthetic
//! keys like every other mode. The closed `Overlay`/`MessageAction` enums are
//! untouched ("a message is data, not a capability" — ADR-0029). The naming
//! buffer itself reuses `App::command_line` (the AddProject precedent), so the
//! editing input path stays single. Every stage is a single list or a single
//! input — never a multi-field form.

/// One activatable row on the settings page (ADR-0044 §3) — the single row
/// source the reducer, the renderer, and the hit-test all share (the
/// `ThemeSlot::ALL` pattern), so the rows can never drift apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SettingsRow {
    /// Rename the profile (opens the naming input, prefilled).
    Name,
    /// The profile's five theme slots (opens the colour picker).
    Colours,
    /// The profile's project membership (opens the members editor).
    Projects,
    /// Clone the profile whole — membership, arrangements, `Some` theme
    /// slots (ADR-0045 §1; opens the duplicate naming input).
    Duplicate,
    /// Pin/unpin the profile as the launch view (ADR-0045 §2).
    StartupPin,
    /// Delete the profile (guarded; opens the inline confirm).
    Delete,
}

impl SettingsRow {
    /// Every row, in display order.
    pub const ALL: [Self; 6] = [
        Self::Name,
        Self::Colours,
        Self::Projects,
        Self::Duplicate,
        Self::StartupPin,
        Self::Delete,
    ];

    /// The row's rendered label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Colours => "colours",
            Self::Projects => "projects",
            Self::Duplicate => "duplicate",
            Self::StartupPin => "startup pin",
            Self::Delete => "delete profile",
        }
    }
}

/// Which stage the profile menu is in (ADR-0037 §2, grown hub-and-spoke by
/// ADR-0044 §3). `Esc` steps back one stage toward Browse, then closes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum MenuStage {
    /// Moving the cursor over the live profiles (`j`/`k`; `Enter` switches).
    #[default]
    Browse,
    /// The per-profile settings page — a cursor over [`SettingsRow::ALL`].
    Settings {
        /// The profile the page manages (not necessarily the active one).
        profile: String,
    },
    /// Typing a name for a new profile (`n`) — create-with-copy on submit.
    NamingCreate,
    /// Typing a new name for a profile (Settings → Name; prefilled).
    NamingRename {
        /// The stable id of the profile being renamed.
        profile: String,
    },
    /// Typing the clone's name (Settings → Duplicate; ADR-0045 §1).
    NamingDuplicate {
        /// The stable id of the profile being cloned.
        source: String,
    },
    /// The members editor (ADR-0044 §3, M512-06): a cursor over the profile's
    /// ordered project view, loaded through the loader seam.
    Members {
        /// The profile whose membership is being edited.
        profile: String,
    },
    /// The add stage: a cursor over the catalogue projects NOT yet in this
    /// profile's view; `Enter` includes one.
    MembersAdd {
        /// The profile gaining a member.
        profile: String,
    },
    /// The inline delete confirm — an explicit second act inside the menu,
    /// never a `MessageAction` (ADR-0037 §3).
    ConfirmDelete {
        /// The stable id of the profile pending removal.
        profile: String,
        /// Where `Esc`/cancel returns: the settings page (`true`) or Browse.
        /// A LANDED delete always returns to Browse — the page's profile is
        /// gone (ADR-0044).
        back_to_settings: bool,
    },
}

/// The profile menu's transient state — only meaningful while
/// `mode == Mode::ProfileMenu`; resting (`Default`) otherwise.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfileMenuState {
    /// The highlighted row (an index into `App::profiles`).
    pub cursor: usize,
    /// The highlighted settings row (an index into [`SettingsRow::ALL`]).
    pub settings_cursor: usize,
    /// The current stage of the menu's state machine.
    pub stage: MenuStage,
    /// The last refused name, rendered inline until the next edit (the name
    /// rules are trimmed, non-empty, ≤ 24 chars, unique — ADR-0037 §4).
    pub name_error: bool,
    /// The edited profile's ordered project view (the members editor's rows),
    /// loaded through the loader seam on entry and after every membership op.
    pub members: Vec<crate::Project>,
    /// The global catalogue — the add stage's candidate source. Candidates
    /// are computed pure ([`ProfileMenuState::candidates`]), never stored.
    pub catalogue: Vec<crate::Project>,
    /// The highlighted member row.
    pub member_cursor: usize,
    /// The highlighted add-stage candidate row.
    pub add_cursor: usize,
}

impl ProfileMenuState {
    /// Reset to the resting state (leaving the menu or entering it fresh).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// The add stage's rows: catalogue projects NOT in the member view —
    /// computed from the two loaded lists, so the rows can never drift from
    /// what a membership op would actually do.
    #[must_use]
    pub fn candidates(&self) -> Vec<&crate::Project> {
        self.catalogue
            .iter()
            .filter(|c| !self.members.iter().any(|m| m.path == c.path))
            .collect()
    }
}
