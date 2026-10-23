//! Folding an input [`Action`] into a [`Flow`]: the profile plane's
//! superseding ops, the picker's live re-theme, the members editor, and the
//! remove/reorder ops that scope to the active profile.
//!
//! Separate so a key, a click and a palette verb that name the same act fold
//! through the same function, and so every persist op the reducer can emit
//! is constructed beside the action that earns it.

use crate::input::Action;
use crate::profile_menu::MenuStage;
use crate::{App, Mode};

use super::{AppState, Effects, Flow, PersistOp};

/// Fold the existing [`Action`] (from [`on_key`]) into a [`Flow`]/[`Effects`].
/// `RemoveProject` and `ReorderProject` are handled in [`update`] because they
/// need the selected project, so they never reach here.
pub(super) fn fold_action(action: Action) -> Flow {
    match action {
        Action::Quit => Flow::Quit,
        Action::None
        | Action::RemoveProject
        | Action::ReorderProject(_)
        | Action::OpenProjectSettings
        | Action::OpenRecallViewer
        | Action::OpenHelp
        | Action::OpenWorktreeAxis
        | Action::OpenSavingsBreakdown
        | Action::SetRecallWindow(_)
        | Action::AdoptWorktree
        | Action::ApplyProjectSettings(_)
        | Action::RunLine(_)
        | Action::ConfirmDispatch
        | Action::SwitchProfile
        | Action::CreateProfile(_)
        | Action::RenameProfile { .. }
        | Action::DeleteProfile(_)
        | Action::DuplicateProfile { .. }
        | Action::ToggleStartupPin(_)
        | Action::LoadMembers(_)
        | Action::MemberRemove
        | Action::MemberReorder(_)
        | Action::MemberAdd
        | Action::OpenColours(_)
        | Action::SetThemeSlot { .. } => Flow::Continue(Effects::default()),
        Action::AddProject(path) => Flow::Continue(Effects {
            persist: vec![PersistOp::AddProject { path }],
            ..Effects::default()
        }),
        // The lister seam (the destination explorer): the reducer requests, the loop walks.
        Action::ExplorerList(path) => Flow::Continue(Effects {
            list_dir: Some(path),
            ..Effects::default()
        }),
    }
}

/// Fold the profile-plane actions — each one superseding
/// persist op plus its reload (or, for a picker selection, the same-frame
/// re-theme) — deferring every other action to [`fold_action`].
pub(super) fn profile_or_fold_action(s: &mut AppState, action: Action) -> Flow {
    match action {
        Action::SwitchProfile => switch_profile_flow(s),
        Action::CreateProfile(name) => profile_persist_flow(PersistOp::CreateProfile { name }),
        Action::RenameProfile { profile, name } => {
            profile_persist_flow(PersistOp::RenameProfile { profile, name })
        }
        Action::DeleteProfile(profile) => {
            profile_persist_flow(PersistOp::RemoveProfile { profile })
        }
        Action::DuplicateProfile { source, name } => {
            profile_persist_flow(PersistOp::DuplicateProfile { source, name })
        }
        // The settings toggle resolves pin-vs-unpin against the CURRENT pin;
        // the reload keeps the rendered state honest.
        Action::ToggleStartupPin(profile) => {
            let pinned = s.app.startup_profile.as_deref() == Some(profile.as_str());
            let profile = if pinned { None } else { Some(profile) };
            profile_persist_flow(PersistOp::SetStartupProfile { profile })
        }
        // A picker selection applies LIVE in the same frame: the
        // snapshot re-themes here; the superseding event persists.
        Action::SetThemeSlot { slot, colour } => apply_theme_slot(s, slot, colour),
        Action::OpenColours(profile) => open_colours_flow(s, profile),
        // The members editor: entry loads the lists; every op is a
        // superseding membership insert plus a re-read of the editor's rows.
        Action::LoadMembers(profile) => Flow::Continue(Effects {
            load_membership: Some(profile),
            ..Effects::default()
        }),
        Action::MemberRemove => member_op(s, |profile, path| PersistOp::RemoveProfileProject {
            profile,
            path,
        }),
        Action::MemberReorder(up) => member_op(s, move |profile, path| {
            PersistOp::ReorderProfileProject { profile, path, up }
        }),
        Action::MemberAdd => member_add_flow(s),
        other => fold_action(other),
    }
}

/// A profile persist op plus the whole-view reload it implies:
/// the loop re-reads names, active id, membership, selection, and arrangement
/// through the loader seam after the op lands.
pub(super) fn profile_persist_flow(op: PersistOp) -> Flow {
    Flow::Continue(Effects {
        persist: vec![op],
        reload_profiles: true,
        ..Effects::default()
    })
}

/// The menu's switch (`Enter` on a row, the profile menu and header identity): select the cursored
/// profile and close the menu — the whole view follows through the reload.
/// Switching to the already-active profile just closes the menu.
pub(super) fn switch_profile_flow(s: &mut AppState) -> Flow {
    let Some(target) = s
        .app
        .profiles
        .get(s.app.profile_menu.cursor)
        .map(|p| p.profile_id.clone())
    else {
        return Flow::Continue(Effects::default());
    };
    s.app.mode = Mode::Normal;
    s.app.profile_menu.reset();
    if target == s.app.active_profile {
        return Flow::Continue(Effects::default());
    }
    profile_persist_flow(PersistOp::SelectProfile { profile: target })
}

/// Apply one theme-slot selection (the colour picker's presets, applied to the
/// profile whose page opened it):
/// the event persists to the picker's TARGET profile always; the running
/// dashboard re-themes in the same frame **only when the target is the active
/// profile** — editing a non-active profile's colours never disturbs the
/// shown view. The picker's own swatches update purely either way, so its
/// slot stage tells the truth without a reload. No profile wired ⇒ the
/// preview alone (nothing durable to scope it to).
pub(super) fn apply_theme_slot(
    s: &mut AppState,
    slot: crate::theme::ThemeSlot,
    colour: Option<crate::render::frame::Color>,
) -> Flow {
    let target = if s.app.colour_picker.target_profile.is_empty() {
        s.app.active_profile.clone()
    } else {
        s.app.colour_picker.target_profile.clone()
    };
    s.app.colour_picker.target_overrides.set(slot, colour);
    if target == s.app.active_profile {
        let mut overrides = s.theme().overrides();
        overrides.set(slot, colour);
        s.set_theme(overrides);
    }
    s.dirty = true;
    if target.is_empty() {
        return Flow::Continue(Effects::default());
    }
    Flow::Continue(Effects {
        persist: vec![PersistOp::SetProfileTheme {
            profile: target,
            slot: slot.key().to_string(),
            colour: colour.and_then(crate::theme::colour_to_hex),
        }],
        ..Effects::default()
    })
}

/// The remove op for a column-1 project: profile-scoped (this view only —
/// the profile plane) when a profile is wired; the catalogue removal
/// otherwise (headless/tests keep pre-profile behavior).
pub(super) fn remove_project_op(app: &App, path: String) -> PersistOp {
    if app.active_profile.is_empty() {
        PersistOp::RemoveProject { path }
    } else {
        PersistOp::RemoveProfileProject {
            profile: app.active_profile.clone(),
            path,
        }
    }
}

/// The reorder op for a column-1 project: profile-scoped when a profile is
/// wired (the view reorders; the catalogue keeps its order), global otherwise.
pub(super) fn reorder_project_op(app: &App, path: String, up: bool) -> PersistOp {
    if app.active_profile.is_empty() {
        PersistOp::ReorderProject { path, up }
    } else {
        PersistOp::ReorderProfileProject {
            profile: app.active_profile.clone(),
            path,
            up,
        }
    }
}

/// One members-editor op on the row under the cursor: resolve (profile, path)
/// from the stage + the loaded member list, persist the superseding event, and
/// re-read the editor's rows through the seam.
fn member_op(s: &mut AppState, op: impl FnOnce(String, String) -> PersistOp) -> Flow {
    let MenuStage::Members { profile } = s.app.profile_menu.stage.clone() else {
        return Flow::Continue(Effects::default());
    };
    let Some(path) = s
        .app
        .profile_menu
        .members
        .get(s.app.profile_menu.member_cursor)
        .map(|p| p.path.clone())
    else {
        return Flow::Continue(Effects::default());
    };
    s.dirty = true;
    Flow::Continue(Effects {
        persist: vec![op(profile.clone(), path)],
        load_membership: Some(profile),
        ..Effects::default()
    })
}

/// `Enter` on an add-stage candidate: include it in the edited profile's
/// view — a superseding membership insert — and re-read the editor's rows
/// through the seam. Inert outside the add stage or past the candidates.
fn member_add_flow(s: &mut AppState) -> Flow {
    let MenuStage::MembersAdd { profile } = s.app.profile_menu.stage.clone() else {
        return Flow::Continue(Effects::default());
    };
    let Some(path) = s
        .app
        .profile_menu
        .candidates()
        .get(s.app.profile_menu.add_cursor)
        .map(|p| p.path.clone())
    else {
        return Flow::Continue(Effects::default());
    };
    s.dirty = true;
    Flow::Continue(Effects {
        persist: vec![PersistOp::AddProfileProject {
            profile: profile.clone(),
            path,
        }],
        load_membership: Some(profile),
        ..Effects::default()
    })
}

/// The picker opened for a profile: seed its swatches — purely when the
/// target is active; through the seam otherwise.
fn open_colours_flow(s: &mut AppState, profile: String) -> Flow {
    s.dirty = true;
    if profile == s.app.active_profile {
        s.app.colour_picker.target_overrides = s.theme().overrides();
        return Flow::Continue(Effects::default());
    }
    Flow::Continue(Effects {
        load_picker_theme: Some(profile),
        ..Effects::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hit::Target;
    use crate::input::CanopyKey;
    use crate::state::fixtures::*;
    use crate::state::verbs::run_line;
    use crate::state::{update, Event};

    #[test]
    fn add_project_action_becomes_persist_effect() {
        let mut s = ready(vec![]);
        // `a` browses (the destination explorer); `i` falls back to the typed flow, then a
        // path and Enter — the same funnel the explorer's select reuses.
        update(&mut s, Event::Key(CanopyKey::Char('a')));
        update(&mut s, Event::Key(CanopyKey::Char('i')));
        assert_eq!(s.app.mode, Mode::AddProject);
        for c in "/tmp/x".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let flow = update(&mut s, Event::Key(CanopyKey::Enter));
        match flow {
            Flow::Continue(e) => {
                assert_eq!(
                    e.persist,
                    vec![PersistOp::AddProject {
                        path: "/tmp/x".into()
                    }]
                );
            }
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn remove_key_emits_remove_persist_for_selected() {
        let mut s = ready(vec![project("a"), project("b")]);
        open_drawer(&mut s);
        update(&mut s, Event::Key(CanopyKey::Down)); // select /r/b
        let flow = update(&mut s, Event::Key(CanopyKey::Char('d')));
        match flow {
            Flow::Continue(e) => assert_eq!(
                e.persist,
                vec![PersistOp::RemoveProject {
                    path: "/r/b".into()
                }]
            ),
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn reorder_key_emits_reorder_persist_for_selected() {
        let mut s = ready(vec![project("a"), project("b")]);
        open_drawer(&mut s);
        let flow = update(&mut s, Event::Key(CanopyKey::ShiftDown));
        match flow {
            Flow::Continue(e) => assert_eq!(
                e.persist,
                vec![PersistOp::ReorderProject {
                    path: "/r/a".into(),
                    up: false
                }]
            ),
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn remove_with_no_projects_is_inert() {
        let mut s = ready(vec![]);
        let flow = update(&mut s, Event::Key(CanopyKey::Char('d')));
        match flow {
            Flow::Continue(e) => assert!(e.persist.is_empty()),
            Flow::Quit => panic!("expected Continue"),
        }
    }

    #[test]
    fn wired_remove_and_reorder_are_profile_scoped() {
        // Story 8: a column-1 remove/reorder touches only the active profile's
        // view — the catalogue op is the unwired fallback, not the default.
        let mut s = ready_with_profiles(vec![project("a"), project("b")]);
        open_drawer(&mut s);
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('d'))));
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfileProject {
                profile: "pf0".into(),
                path: "/r/a".into()
            }]
        );
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::ShiftDown)));
        assert_eq!(
            e.persist,
            vec![PersistOp::ReorderProfileProject {
                profile: "pf0".into(),
                path: "/r/a".into(),
                up: false
            }]
        );
    }

    #[test]
    fn menu_switch_emits_select_plus_reload_and_closes() {
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::SelectProfile {
                profile: "pf1".into()
            }]
        );
        assert!(e.reload_profiles, "the whole view reloads on a switch");
        assert_eq!(s.app.mode, Mode::Normal, "the menu closed");
    }

    #[test]
    fn switching_to_the_active_profile_just_closes_the_menu() {
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert!(e.persist.is_empty());
        assert!(!e.reload_profiles);
        assert_eq!(s.app.mode, Mode::Normal);
    }

    #[test]
    fn menu_create_rename_delete_emit_their_ops_with_reload() {
        // Create.
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('n')));
        for c in "client".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::CreateProfile {
                name: "client".into()
            }]
        );
        assert!(e.reload_profiles);

        // Rename — through the settings page now (the profile hub): the Name row
        // opens the prefilled input; append a char.
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        let _ = update(&mut s, Event::Key(CanopyKey::Enter)); // Name row opens
        update(&mut s, Event::Key(CanopyKey::Char('2')));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::RenameProfile {
                profile: "pf1".into(),
                name: "work2".into()
            }]
        );
        assert!(e.reload_profiles);

        // Delete, through the inline confirm.
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('d')));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfile {
                profile: "pf1".into()
            }]
        );
        assert!(e.reload_profiles);
    }

    #[test]
    fn settings_opens_from_key_chip_and_enter_dispatches_rows() {
        use crate::profile_menu::MenuStage;
        // `s` opens the cursor row's settings page…
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::Settings {
                profile: "pf1".into()
            }
        );
        // …Esc steps back to the hub…
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert_eq!(s.app.profile_menu.stage, MenuStage::Browse);
        assert_eq!(s.app.mode, Mode::ProfileMenu);
        // …and the row chip is the click twin (modal-surface parity).
        let click = click_on(Target::ProfileSettings(2), &mut s);
        let e = effects_of(update(&mut s, Event::Mouse(click)));
        assert!(e.persist.is_empty());
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::Settings {
                profile: "pf2".into()
            },
            "the chip opens ITS row's settings page"
        );
    }

    #[test]
    fn duplicate_from_settings_names_then_emits_full_clone_op() {
        use crate::profile_menu::{MenuStage, SettingsRow};
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j'))); // a NON-active row
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        // Cursor to the Duplicate row, activate, type the clone's name.
        let dup = SettingsRow::ALL
            .iter()
            .position(|r| matches!(r, SettingsRow::Duplicate))
            .unwrap();
        for _ in 0..dup {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        update(&mut s, Event::Key(CanopyKey::Enter));
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::NamingDuplicate {
                source: "pf1".into()
            }
        );
        for c in "work-copy".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::DuplicateProfile {
                source: "pf1".into(),
                name: "work-copy".into()
            }],
            "the clone op carries the SETTINGS page's profile as its source"
        );
        assert!(e.reload_profiles);
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::Browse,
            "a landed duplicate returns to the hub (the reducer never learns the minted id)"
        );
    }

    #[test]
    fn startup_toggle_pins_and_unpins() {
        use crate::profile_menu::SettingsRow;
        let pin_row = SettingsRow::ALL
            .iter()
            .position(|r| matches!(r, SettingsRow::StartupPin))
            .unwrap();
        // Unpinned → the toggle pins the page's profile.
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        for _ in 0..pin_row {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::SetStartupProfile {
                profile: Some("pf1".into())
            }]
        );
        assert!(e.reload_profiles);
        // Already the pin → the same act unpins (NULL).
        s.app.startup_profile = Some("pf1".into());
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::SetStartupProfile { profile: None }]
        );
    }

    #[test]
    fn delete_from_settings_confirms_then_returns_to_browse() {
        use crate::profile_menu::{MenuStage, SettingsRow};
        let del_row = SettingsRow::ALL
            .iter()
            .position(|r| matches!(r, SettingsRow::Delete))
            .unwrap();
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        for _ in 0..del_row {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        update(&mut s, Event::Key(CanopyKey::Enter));
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::ConfirmDelete {
                profile: "pf1".into(),
                back_to_settings: true
            }
        );
        // Esc cancels BACK to the settings page it came from…
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::Settings {
                profile: "pf1".into()
            }
        );
        // …while a landed delete returns to the hub (the page's profile is gone).
        update(&mut s, Event::Key(CanopyKey::Enter));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfile {
                profile: "pf1".into()
            }]
        );
        assert_eq!(s.app.profile_menu.stage, MenuStage::Browse);
    }

    /// The members editor for the NON-active `pf1`, entered through its
    /// settings page with the loop's list delivery simulated; the entry's
    /// effects ride along so a test can assert the seam request.
    fn members_editor_for_pf1() -> (AppState, Effects) {
        use crate::profile_menu::SettingsRow;
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j'))); // a NON-active row
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        let projects_row = SettingsRow::ALL
            .iter()
            .position(|r| matches!(r, SettingsRow::Projects))
            .unwrap();
        for _ in 0..projects_row {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        let entered = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        // The loop lands the lists (simulated here).
        s.app.set_membership_lists(
            vec![project("m1"), project("m2")],
            vec![project("m1"), project("m2"), project("free")],
        );
        (s, entered)
    }

    #[test]
    fn the_members_editor_enters_through_the_seam_then_removes_and_reorders() {
        use crate::profile_menu::MenuStage;
        let (mut s, entered) = members_editor_for_pf1();
        // Entering requests the lists through the seam (the reducer never
        // reads a store).
        assert_eq!(entered.load_membership.as_deref(), Some("pf1"));
        assert!(entered.persist.is_empty());
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::Members {
                profile: "pf1".into()
            }
        );
        // Every op then carries the EDITED profile's id and re-requests the read.
        update(&mut s, Event::Key(CanopyKey::Char('j'))); // cursor → m2
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('d'))));
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfileProject {
                profile: "pf1".into(),
                path: "/r/m2".into()
            }]
        );
        assert_eq!(e.load_membership.as_deref(), Some("pf1"));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::ShiftUp)));
        assert_eq!(
            e.persist,
            vec![PersistOp::ReorderProfileProject {
                profile: "pf1".into(),
                path: "/r/m2".into(),
                up: true
            }]
        );
    }

    #[test]
    fn the_add_stage_lists_non_members_and_enter_includes_one() {
        use crate::profile_menu::MenuStage;
        let (mut s, _entered) = members_editor_for_pf1();
        // The add stage lists only NON-members; Enter includes the candidate.
        update(&mut s, Event::Key(CanopyKey::Char('a')));
        assert_eq!(
            s.app.profile_menu.stage,
            MenuStage::MembersAdd {
                profile: "pf1".into()
            }
        );
        assert_eq!(
            s.app
                .profile_menu
                .candidates()
                .iter()
                .map(|p| p.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/r/free"],
            "candidates are the catalogue minus the members"
        );
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.persist,
            vec![PersistOp::AddProfileProject {
                profile: "pf1".into(),
                path: "/r/free".into()
            }]
        );
        assert_eq!(e.load_membership.as_deref(), Some("pf1"));
        // Esc steps back: add → members → settings.
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert!(matches!(
            s.app.profile_menu.stage,
            MenuStage::Members { .. }
        ));
        update(&mut s, Event::Key(CanopyKey::Esc));
        assert!(matches!(
            s.app.profile_menu.stage,
            MenuStage::Settings { .. }
        ));
    }

    #[test]
    fn picker_for_a_non_active_profile_persists_without_retheming() {
        use crate::profile_menu::SettingsRow;
        use crate::theme::{ThemeSlot, PRESETS};
        let mut s = ready_with_profiles(vec![project("a")]);
        let before = s.theme();
        // Open the NON-active pf1's settings → Colours.
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        let colours_row = SettingsRow::ALL
            .iter()
            .position(|r| matches!(r, SettingsRow::Colours))
            .unwrap();
        for _ in 0..colours_row {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            e.load_picker_theme.as_deref(),
            Some("pf1"),
            "a non-active target's swatches load through the seam"
        );
        assert_eq!(s.app.colour_picker.target_profile, "pf1");
        // Apply a preset: the event carries pf1; the LIVE theme is untouched;
        // the picker's own swatches update purely.
        update(&mut s, Event::Key(CanopyKey::Enter)); // background's presets
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter))); // red
        assert_eq!(
            e.persist,
            vec![PersistOp::SetProfileTheme {
                profile: "pf1".into(),
                slot: "background".into(),
                colour: crate::theme::colour_to_hex(PRESETS[0].1),
            }]
        );
        assert_eq!(
            s.theme(),
            before,
            "editing a non-active profile never re-themes the shown view"
        );
        assert_eq!(
            s.app
                .colour_picker
                .target_overrides
                .get(ThemeSlot::Background),
            Some(PRESETS[0].1),
            "the picker's swatches stay honest without a reload"
        );
    }

    #[test]
    fn an_explorer_list_action_folds_to_the_lister_seam() {
        // the destination explorer: the reducer never reads the filesystem — it requests.
        let path = std::path::PathBuf::from("/home/dev/alpha");
        let e = effects_of(fold_action(Action::ExplorerList(path.clone())));
        assert_eq!(e.list_dir, Some(path));
        assert!(!e.is_empty(), "a listing request is real work for the loop");
        assert!(e.persist.is_empty() && e.data_requests.is_empty());
    }

    #[test]
    fn keyboard_movement_follows_the_cursor_without_stealing_a_free_offset() {
        // the destination explorer: the first mode-modal follow-cursor clamp.
        let mut s = browsing();
        s.app.explorer.entries = (0..200).map(|i| dir(&format!("d{i:03}"))).collect();
        let list_h =
            usize::from(crate::view::explorer_list_area(crate::view::modal_rect(s.size)).h);
        for _ in 0..list_h + 5 {
            update(&mut s, Event::Key(CanopyKey::Char('j')));
        }
        assert_eq!(s.app.explorer.cursor, list_h + 5);
        assert_eq!(
            s.app.explorer.scroll,
            list_h + 5 + 1 - list_h,
            "the viewport followed the cursor down"
        );
        // A wheel-set offset that still shows the cursor survives a redraw.
        let mut s = browsing();
        s.app.explorer.entries = (0..200).map(|i| dir(&format!("d{i:03}"))).collect();
        s.app.explorer.cursor = 30;
        s.app.explorer.scroll = 28;
        s.app.clamp_scrolls(s.size);
        assert_eq!(s.app.explorer.scroll, 28, "an in-view cursor moves nothing");
    }

    #[test]
    fn selecting_a_destination_registers_it_and_add_is_untouched() {
        // The funnel (the destination explorer): select yields exactly the typed flow's op.
        let mut s = browsing();
        update(&mut s, Event::Key(CanopyKey::Enter)); // descend to alpha…
        s.app.explorer.apply(crate::explorer::DirListing {
            path: std::path::PathBuf::from("/home/dev/alpha"),
            entries: vec![dir("src")],
            omitted: 0,
            error: None,
        });
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('s'))));
        assert_eq!(
            e.persist,
            vec![PersistOp::AddProject {
                path: "/home/dev/alpha".into()
            }],
            "the folder that is OPEN, one level down"
        );

        // `/add` still registers the launch cwd and never raises the explorer.
        let mut s = ready(vec![project("a")]);
        s.app.launch_cwd = Some("/r/here".into());
        s.app.home_dir = Some("/home/dev".into());
        let e = effects_of(run_line(&mut s, "add"));
        assert_eq!(
            e.persist,
            vec![PersistOp::AddProject {
                path: "/r/here".into()
            }]
        );
        assert!(e.list_dir.is_none(), "/add browses nothing");
        assert_eq!(s.app.mode, Mode::Normal);
    }

    #[test]
    fn a_picker_selection_rethemes_the_same_frame_and_persists_one_event() {
        use crate::theme::{ThemeSlot, PRESETS};
        let mut s = ready_with_profiles(vec![project("a")]);
        // p → menu, s → settings, j+Enter → the Colours row opens the picker
        // (the profile hub), Enter → background's presets, Enter → red.
        for k in ['p', 's', 'j'] {
            update(&mut s, Event::Key(CanopyKey::Char(k)));
        }
        update(&mut s, Event::Key(CanopyKey::Enter));
        update(&mut s, Event::Key(CanopyKey::Enter));
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        // Applied live: the snapshot already wears the override.
        assert_eq!(
            s.snapshot().theme.overrides().get(ThemeSlot::Background),
            Some(PRESETS[0].1),
            "the dashboard is the preview — re-themed in the same frame"
        );
        // Exactly one superseding event persists, with the resolved hex.
        assert_eq!(
            e.persist,
            vec![PersistOp::SetProfileTheme {
                profile: "pf0".into(),
                slot: "background".into(),
                colour: crate::theme::colour_to_hex(PRESETS[0].1),
            }]
        );
        assert!(!e.reload_profiles, "live apply needs no view reload");

        // The reset row supersedes back to default (a NULL colour).
        for _ in 0..20 {
            update(&mut s, Event::Key(CanopyKey::Down));
        }
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Enter)));
        assert_eq!(
            s.snapshot().theme.overrides().get(ThemeSlot::Background),
            None,
            "reset returns the slot to Rift dark"
        );
        assert_eq!(
            e.persist,
            vec![PersistOp::SetProfileTheme {
                profile: "pf0".into(),
                slot: "background".into(),
                colour: None,
            }]
        );
    }
}
