//! Profile-menu mode: the hub-and-spoke menu over the live profiles, its
//! settings page, the members editor, naming and the inline delete confirm.

use super::{Action, CanopyKey, Mode};
use crate::profile_menu::{MenuStage, SettingsRow};
use crate::App;

/// Enter profile-menu mode: reset the menu's state machine and open
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

/// Open the settings page for a profile (the profile hub) — the one entry both
/// the `s` key and the row chip use.
pub(crate) fn enter_profile_settings(app: &mut App, profile: String) {
    app.profile_menu.stage = MenuStage::Settings { profile };
    app.profile_menu.settings_cursor = 0;
    app.profile_menu.name_error = false;
    app.command_line.clear();
}

/// Activate one settings row for `profile` (the profile hub) — the one dispatch
/// both `Enter` and a row click use.
pub(crate) fn activate_settings_row(app: &mut App, profile: &str, row: SettingsRow) -> Action {
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
            // The picker binds to THIS page's profile and
            // returns here on Esc — the menu stage stays intact underneath.
            app.mode = Mode::ColourPicker;
            app.colour_picker.reset();
            app.colour_picker.target_profile = profile.to_string();
            Action::OpenColours(profile.to_string())
        }
        SettingsRow::Projects => {
            // The members editor: the stage opens now; the lists
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
            // The removal guards refuse here first (the profile plane): never the
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

/// The profile-menu reducer (hub-and-spoke, per the profile hub): Browse
/// moves over the live profiles (`Enter` switches, `s` opens the cursor row's
/// settings, `n` creates, `d` deletes behind the inline confirm); the settings
/// page moves over [`crate::profile_menu::SettingsRow::ALL`] and `Enter`
/// activates; naming reuses the editing-mode buffer (`App::command_line`, the
/// AddProject precedent) and validates against the profile name rules
/// before any op is emitted. `Esc` steps one stage back toward Browse, then
/// closes. Rename and colours live in settings — Browse no longer carries
/// `r`/`c`.
///
/// Each stage's key table is a handler of its own, so the stage a key lands
/// in is read here and what the key does is read in one place.
pub(super) fn on_key_profile_menu(app: &mut App, key: CanopyKey) -> Action {
    // Ctrl-C still quits the whole app, even mid-menu.
    if matches!(key, CanopyKey::Quit) {
        app.quit = true;
        return Action::Quit;
    }
    match app.profile_menu.stage.clone() {
        MenuStage::Browse => on_key_menu_browse(app, key),
        MenuStage::Settings { profile } => on_key_menu_settings(app, key, &profile),
        MenuStage::Members { profile } => on_key_menu_members(app, key, profile),
        MenuStage::MembersAdd { profile } => on_key_menu_members_add(app, key, profile),
        MenuStage::NamingCreate
        | MenuStage::NamingRename { .. }
        | MenuStage::NamingDuplicate { .. } => on_key_menu_naming(app, key),
        MenuStage::ConfirmDelete {
            profile,
            back_to_settings,
        } => on_key_menu_confirm_delete(app, key, profile, back_to_settings),
    }
}

/// Browse: the cursor over the live profiles, and the acts on its row.
fn on_key_menu_browse(app: &mut App, key: CanopyKey) -> Action {
    let last = app.profiles.len().saturating_sub(1);
    match key {
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
            // The in-hub shortcut keeps the palette grammar; the guards
            // refuse here first (the profile plane).
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
    }
}

/// A profile's settings page: the cursor over its rows, `Enter` activates.
fn on_key_menu_settings(app: &mut App, key: CanopyKey, profile: &str) -> Action {
    let rows_last = SettingsRow::ALL.len() - 1;
    match key {
        CanopyKey::Char('j') | CanopyKey::Down => {
            app.profile_menu.settings_cursor =
                (app.profile_menu.settings_cursor + 1).min(rows_last);
            Action::None
        }
        CanopyKey::Char('k') | CanopyKey::Up => {
            app.profile_menu.settings_cursor = app.profile_menu.settings_cursor.saturating_sub(1);
            Action::None
        }
        CanopyKey::Enter => SettingsRow::ALL
            .get(app.profile_menu.settings_cursor.min(rows_last))
            .copied()
            .map_or(Action::None, |row| activate_settings_row(app, profile, row)),
        CanopyKey::Esc => {
            app.profile_menu.stage = MenuStage::Browse;
            Action::None
        }
        _ => Action::None,
    }
}

/// The members editor: the cursor over the edited profile's members, and the
/// remove / reorder / add requests the loader seam answers.
fn on_key_menu_members(app: &mut App, key: CanopyKey, profile: String) -> Action {
    match key {
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
    }
}

/// The members editor's add stage: the cursor over the candidates.
fn on_key_menu_members_add(app: &mut App, key: CanopyKey, profile: String) -> Action {
    match key {
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
    }
}

/// The naming stages (create, rename, duplicate) share one buffer and one
/// submit; only where `Esc` returns to differs.
fn on_key_menu_naming(app: &mut App, key: CanopyKey) -> Action {
    match key {
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
                MenuStage::NamingDuplicate { source } => MenuStage::Settings { profile: source },
                _ => MenuStage::Browse,
            };
            app.profile_menu.name_error = false;
            app.command_line.clear();
            Action::None
        }
        CanopyKey::Enter => submit_profile_name(app),
        _ => Action::None,
    }
}

/// The inline delete confirm: only `Enter` lands it; `Esc` or `n` step back to
/// the stage that opened it.
fn on_key_menu_confirm_delete(
    app: &mut App,
    key: CanopyKey,
    profile: String,
    back_to_settings: bool,
) -> Action {
    match key {
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
    }
}

/// Validate and submit the naming buffer (create, rename, or duplicate). A
/// refused name — empty, over the 24-char budget, or a duplicate among live
/// profiles — marks the inline error and keeps the input open; a valid one
/// emits the typed action and returns to its origin: create and duplicate to
/// Browse (the reducer never learns the minted id — the profile hub), rename to its
/// settings page.
fn submit_profile_name(app: &mut App) -> Action {
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

#[cfg(test)]
mod tests {
    use super::super::fixtures::{project, ready, with_profiles};
    use super::super::on_key;
    use super::*;

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
}
