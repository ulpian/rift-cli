//! The profile plane on the loop's side of the seam: the launch load, the
//! startup prime, and the whole-view reload after a profile op.

use rift_core::Result as RiftResult;

use crate::state::{AppState, DataRequest};

/// The shared profile-plane load: the live profiles, the
/// restored active id, and — when one is active — its theme, all
/// through the injected loader, onto the state **before the first frame**.
/// `event_loop` calls this at its launch seam; the headless one-frame render
/// calls the same function, so the interactive and headless runs exercise
/// **one** startup path (an earlier headless frame skipped this load fully and
/// was structurally blind to the whole class of defects).
///
/// # Errors
/// Propagates a loader read failure.
pub fn load_profile_plane(
    state: &mut AppState,
    loader: &mut dyn crate::profile::ProfileLoader,
) -> RiftResult<()> {
    let (active, profiles) = loader.profiles()?;
    // The startup pin: a LIVE pin overrides the last-selected
    // fold at launch; a dead pin loses to the chain. A pure read — launch
    // writes nothing, so relaunching is idempotent by construction.
    let pinned = loader.startup()?;
    let fold_active = active.clone().unwrap_or_default();
    let resolved = pinned
        .clone()
        .filter(|p| profiles.iter().any(|e| e.profile_id == *p))
        .or(active)
        .unwrap_or_default();
    if !resolved.is_empty() {
        let overrides = loader.theme(&resolved)?;
        state.set_theme(overrides);
    }
    // When the pin diverged from the fold, the session's first WRITE will
    // prepend the synchronizing `SelectProfile`.
    state.select_sync_pending = !resolved.is_empty() && resolved != fold_active;
    state.app.set_profiles(profiles, resolved);
    state.app.startup_profile = pinned;
    Ok(())
}

/// The shared startup prime: one `Refresh` per tracked project, then
/// the subscribe for the initial selection — the same requests a selection
/// change or persist-apply would issue, sent once at startup so the first
/// frame's data is already being computed. `event_loop` calls this before its
/// first poll; the headless frame calls it in place of a hand-rolled send, so
/// the interactive and headless runs exercise **one** startup path.
pub fn prime_startup(state: &AppState, req_sink: &mut dyn FnMut(DataRequest)) {
    for p in &state.app.projects {
        req_sink(DataRequest::Refresh(p.path.clone()));
    }
    let active = state.app.selected_project().map(|p| p.path.clone());
    req_sink(DataRequest::SetActive(active));
}

/// Load and apply the currently-selected project's saved layout (scoped to the
/// active profile) through the injected loader. No
/// selection ⇒ nothing to load. A missing/reset layout applies the default
/// (the loader returns `None`).
pub(super) fn load_project_layout(
    state: &mut AppState,
    loader: &mut dyn crate::profile::ProfileLoader,
) -> RiftResult<()> {
    if let Some(path) = state.app.selected_project().map(|p| p.path.clone()) {
        load_layout(state, loader, &path)?;
    }
    Ok(())
}

/// Load and apply one project's saved layout, scoped to the active profile —
/// the one read behind both the launch restore and a selection change. A
/// missing/reset layout applies the default (the loader returns `None`).
pub(super) fn load_layout(
    state: &mut AppState,
    loader: &mut dyn crate::profile::ProfileLoader,
    path: &str,
) -> RiftResult<()> {
    let m = loader
        .layout(&state.app.active_profile, path)?
        .unwrap_or_default();
    state.set_layout(m);
    Ok(())
}

/// Reload the whole profile plane after a profile persist op:
/// the live names + active id, the active profile's membership view, the
/// selection (kept by path where the project survives the switch), the bus
/// subscription, and the (profile, project) arrangement — all through the one
/// injected loader. Runtime project status stays runtime-only: the bus re-feeds
/// it through the re-issued requests, exactly like a persist-apply.
pub(super) fn reload_profile_plane(
    state: &mut AppState,
    loader: &mut dyn crate::profile::ProfileLoader,
    req_sink: &mut dyn FnMut(DataRequest),
    select_landed: bool,
) -> RiftResult<()> {
    let (fold_active, profiles) = loader.profiles()?;
    // Sticky sessions: a pinned launch never writes `selected`,
    // so the fold's active and the session's shown profile may differ. Adopt
    // the fold only when a switch landed in this batch or the shown profile
    // died; otherwise keep showing what we show — its name list, membership,
    // arrangement, and theme still reload below.
    let shown = state.app.active_profile.clone();
    let shown_live = !shown.is_empty() && profiles.iter().any(|p| p.profile_id == shown);
    let active = if select_landed || !shown_live {
        fold_active.unwrap_or_default()
    } else {
        shown
    };
    state.app.set_profiles(profiles, active.clone());
    state.app.startup_profile = loader.startup()?;
    if !active.is_empty() {
        let keep = state.app.selected_project().map(|p| p.path.clone());
        let projects = loader.projects(&active)?;
        state.app.set_projects(projects);
        if let Some(path) = keep {
            state.app.select_path(&path);
        }
        prime_startup(state, req_sink);
        load_project_layout(state, loader)?;
        // The switch re-themes through the same reload path:
        // projects, arrangements, and colours swap as one view.
        let overrides = loader.theme(&active)?;
        state.set_theme(overrides);
    }
    state.dirty = true;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::event_loop;
    use super::super::fixtures::{no_lister, one_project_state, MockBackend, ScriptedEvents};
    use crate::state::{Event, PersistOp};
    use crate::{CanopyKey, EntryKind, Project};
    use rift_core::Result as RiftResult;

    /// A profile loader double that records every read and serves a two-profile
    /// plane whose active id lives in a shared cell (the persist seam moves it,
    /// exactly like the real store's `selected` fold). `pinned` plays the
    /// startup-pin fold.
    struct RecordingLoader {
        active: std::rc::Rc<std::cell::RefCell<String>>,
        calls: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
        pinned: Option<String>,
    }
    impl crate::profile::ProfileLoader for RecordingLoader {
        fn profiles(&mut self) -> RiftResult<(Option<String>, Vec<crate::profile::ProfileEntry>)> {
            self.calls.borrow_mut().push("profiles".into());
            let entries = [("pf0", "default"), ("pf1", "lab")]
                .iter()
                .enumerate()
                .map(|(i, (id, name))| crate::profile::ProfileEntry {
                    profile_id: (*id).to_string(),
                    name: (*name).to_string(),
                    created_at: i64::try_from(i).unwrap_or(0),
                })
                .collect();
            Ok((Some(self.active.borrow().clone()), entries))
        }
        fn projects(&mut self, profile: &str) -> RiftResult<Vec<Project>> {
            self.calls.borrow_mut().push(format!("projects:{profile}"));
            let path = if profile == "pf1" { "/x" } else { "/a" };
            Ok(vec![Project {
                name: path.trim_start_matches('/').into(),
                path: path.into(),
                status: crate::ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }])
        }
        fn layout(
            &mut self,
            profile: &str,
            path: &str,
        ) -> RiftResult<Option<crate::layout_model::LayoutModel>> {
            self.calls
                .borrow_mut()
                .push(format!("layout:{profile}:{path}"));
            if profile == "pf1" && path == "/x" {
                let mut m = crate::layout_model::LayoutModel::default();
                m.columns.swap(0, 1);
                return Ok(Some(m));
            }
            Ok(None)
        }
        fn theme(&mut self, profile: &str) -> RiftResult<crate::theme::ThemeOverrides> {
            self.calls.borrow_mut().push(format!("theme:{profile}"));
            let mut o = crate::theme::ThemeOverrides::NONE;
            if profile == "pf1" {
                o.set(
                    crate::theme::ThemeSlot::Background,
                    Some(crate::render::frame::Color::Rgb(1, 2, 3)),
                );
            }
            Ok(o)
        }
        fn startup(&mut self) -> RiftResult<Option<String>> {
            self.calls.borrow_mut().push("startup".into());
            Ok(self.pinned.clone())
        }
        fn catalogue(&mut self) -> RiftResult<Vec<Project>> {
            self.calls.borrow_mut().push("catalogue".into());
            Ok(vec![
                Project {
                    name: "a".into(),
                    path: "/a".into(),
                    status: crate::ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
                Project {
                    name: "x".into(),
                    path: "/x".into(),
                    status: crate::ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
            ])
        }
    }

    #[test]
    fn a_profile_switch_reloads_the_whole_view_through_the_loader_seam() {
        // `Enter` in the profile menu emits SelectProfile;
        // the loop then reloads names, membership, selection, and the
        // (profile, project) arrangement through the ONE injected loader.
        let mut backend = MockBackend { presents: 0 };
        let mut src = ScriptedEvents::new(vec![vec![
            Event::Key(CanopyKey::Char('p')),
            Event::Key(CanopyKey::Char('j')),
            Event::Key(CanopyKey::Enter),
            Event::Key(CanopyKey::Quit),
        ]]);
        let mut state = one_project_state();
        let active = std::rc::Rc::new(std::cell::RefCell::new("pf0".to_string()));
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut loader = RecordingLoader {
            active: std::rc::Rc::clone(&active),
            calls: std::rc::Rc::clone(&calls),
            pinned: None,
        };
        let persisted = std::cell::RefCell::new(Vec::new());
        let persist_active = std::rc::Rc::clone(&active);
        let mut on_persist = |op: &PersistOp| {
            if let PersistOp::SelectProfile { profile } = op {
                *persist_active.borrow_mut() = profile.clone();
            }
            persisted.borrow_mut().push(op.clone());
            Ok(vec![])
        };
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut on_persist,
            &mut |_| {},
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut loader,
            &mut no_lister,
        )
        .unwrap();

        assert_eq!(
            *persisted.borrow(),
            vec![PersistOp::SelectProfile {
                profile: "pf1".into()
            }],
            "the switch is one superseding persist op"
        );
        assert_eq!(state.app.active_profile, "pf1", "the identity followed");
        assert_eq!(
            state
                .app
                .projects
                .iter()
                .map(|p| p.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/x"],
            "membership swapped to the new profile's view"
        );
        let mut swapped = crate::layout_model::LayoutModel::default();
        swapped.columns.swap(0, 1);
        assert_eq!(
            state.app.layout, swapped,
            "the (profile, project) arrangement restored through the seam"
        );
        assert!(
            calls.borrow().iter().any(|c| c == "layout:pf1:/x"),
            "the layout read was profile-scoped: {:?}",
            calls.borrow()
        );
        // The switch re-themed as part of the same reload.
        assert_eq!(
            state.theme().overrides().background,
            Some(crate::render::frame::Color::Rgb(1, 2, 3)),
            "colours swap with the view"
        );
    }

    #[test]
    fn entering_members_loads_through_the_loader_and_lands_the_lists() {
        // The reducer only REQUESTS the lists; the loop reads the
        // edited profile's view + the catalogue through the one seam.
        let mut backend = MockBackend { presents: 0 };
        let mut script = vec![
            Event::Key(CanopyKey::Char('p')),
            Event::Key(CanopyKey::Char('j')), // cursor → pf1
            Event::Key(CanopyKey::Char('s')),
        ];
        // Settings cursor → the Projects row, then Enter opens the editor.
        let projects_row = crate::profile_menu::SettingsRow::ALL
            .iter()
            .position(|r| matches!(r, crate::profile_menu::SettingsRow::Projects))
            .unwrap();
        for _ in 0..projects_row {
            script.push(Event::Key(CanopyKey::Char('j')));
        }
        script.push(Event::Key(CanopyKey::Enter));
        script.push(Event::Key(CanopyKey::Quit));
        let mut src = ScriptedEvents::new(vec![script]);
        let mut state = one_project_state();
        let active = std::rc::Rc::new(std::cell::RefCell::new("pf0".to_string()));
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut loader = RecordingLoader {
            active: std::rc::Rc::clone(&active),
            calls: std::rc::Rc::clone(&calls),
            pinned: None,
        };
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut |_| Ok(vec![]),
            &mut |_| {},
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut loader,
            &mut no_lister,
        )
        .unwrap();
        assert!(
            calls.borrow().iter().any(|c| c == "projects:pf1"),
            "the member view read was profile-scoped: {:?}",
            calls.borrow()
        );
        assert!(
            calls.borrow().iter().any(|c| c == "catalogue"),
            "the add stage's candidate source loaded: {:?}",
            calls.borrow()
        );
        assert_eq!(
            state
                .app
                .profile_menu
                .members
                .iter()
                .map(|p| p.path.as_str())
                .collect::<Vec<_>>(),
            vec!["/x"],
            "the edited profile's view landed on the editor"
        );
        assert_eq!(state.app.profile_menu.catalogue.len(), 2);
    }

    #[test]
    fn a_pinned_launch_shows_the_pinned_profile_and_writes_nothing() {
        // The pin overrides the last-selected fold at launch —
        // identity AND theme — and launch stays a pure read: zero persists.
        let mut backend = MockBackend { presents: 0 };
        let mut src = ScriptedEvents::new(vec![vec![Event::Key(CanopyKey::Quit)]]);
        let mut state = one_project_state();
        let active = std::rc::Rc::new(std::cell::RefCell::new("pf0".to_string()));
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut loader = RecordingLoader {
            active: std::rc::Rc::clone(&active),
            calls: std::rc::Rc::clone(&calls),
            pinned: Some("pf1".to_string()),
        };
        let persisted = std::cell::RefCell::new(Vec::new());
        let mut on_persist = |op: &PersistOp| {
            persisted.borrow_mut().push(op.clone());
            Ok(vec![])
        };
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut on_persist,
            &mut |_| {},
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut loader,
            &mut no_lister,
        )
        .unwrap();
        assert_eq!(
            state.app.active_profile, "pf1",
            "the live pin wins the launch resolution"
        );
        assert_eq!(
            state.theme().overrides().background,
            Some(crate::render::frame::Color::Rgb(1, 2, 3)),
            "the dashboard wears the pinned profile's theme from frame one"
        );
        assert!(
            calls.borrow().iter().any(|c| c == "startup"),
            "the pin loaded through the seam: {:?}",
            calls.borrow()
        );
        assert!(
            persisted.borrow().is_empty(),
            "launch writes nothing: {:?}",
            persisted.borrow()
        );
        assert!(
            state.select_sync_pending,
            "the deferred selected-sync stays armed until the first write"
        );
    }

    #[test]
    fn a_pinned_sessions_first_write_syncs_then_the_view_stays() {
        // A pinned session's first persist batch prepends the
        // synchronizing SelectProfile, so the fold's active aligns with the
        // shown profile and a non-select op can never snap the view back.
        let mut backend = MockBackend { presents: 0 };
        let mut script = vec![Event::Key(CanopyKey::Char('p'))];
        script.push(Event::Key(CanopyKey::Char('n')));
        for ch in "qa".chars() {
            script.push(Event::Key(CanopyKey::Char(ch)));
        }
        script.push(Event::Key(CanopyKey::Enter));
        script.push(Event::Key(CanopyKey::Esc));
        script.push(Event::Key(CanopyKey::Quit));
        let mut src = ScriptedEvents::new(vec![script]);
        let mut state = one_project_state();
        let active = std::rc::Rc::new(std::cell::RefCell::new("pf0".to_string()));
        let calls = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut loader = RecordingLoader {
            active: std::rc::Rc::clone(&active),
            calls: std::rc::Rc::clone(&calls),
            pinned: Some("pf1".to_string()),
        };
        let persisted = std::cell::RefCell::new(Vec::new());
        let persist_active = std::rc::Rc::clone(&active);
        let mut on_persist = |op: &PersistOp| {
            if let PersistOp::SelectProfile { profile } = op {
                *persist_active.borrow_mut() = profile.clone();
            }
            persisted.borrow_mut().push(op.clone());
            Ok(vec![])
        };
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut on_persist,
            &mut |_| {},
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut loader,
            &mut no_lister,
        )
        .unwrap();
        assert_eq!(
            *persisted.borrow(),
            vec![
                PersistOp::SelectProfile {
                    profile: "pf1".into()
                },
                PersistOp::CreateProfile { name: "qa".into() },
            ],
            "the first write carries the deferred selected-sync FIRST"
        );
        assert_eq!(
            state.app.active_profile, "pf1",
            "the shown profile never snapped back to the pre-pin fold"
        );
        assert!(
            !state.select_sync_pending,
            "the sync is one-shot — later batches carry no prepend"
        );
    }
}
