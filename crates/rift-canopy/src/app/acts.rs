//! The synchronous acts the loop performs behind a running frame: a Project
//! Settings action, the add flow's project init, and a delegated dispatch.

use rift_core::Result as RiftResult;

use crate::dispatch::DispatchRequest;
use crate::render::RenderBackend;
use crate::state::{update, AppState, DataRequest, Event, Flow, PersistOp};
use crate::{Mode, Project, ProjectSettingsManager};

use super::effects::apply_effects;
use super::{prime_startup, render_if_dirty, LoopHost};

pub(super) fn run_project_settings_action(
    backend: &mut dyn RenderBackend,
    state: &mut AppState,
    manager: &mut dyn ProjectSettingsManager,
    req_sink: &mut dyn FnMut(DataRequest),
    action: crate::ProjectSettingsAction,
) -> RiftResult<()> {
    state.app.project_settings.running = true;
    state.app.project_settings.result = Some("working…".to_owned());
    state.dirty = true;
    render_if_dirty(backend, state)?;
    match manager.apply(&action) {
        Ok(view) => {
            let path = view.path.clone();
            state.app.project_settings.refresh(view);
            state.app.project_settings.result = Some("saved and verified".to_owned());
            state.app.project_settings.failed_action = None;
            req_sink(DataRequest::Refresh(path));
        }
        Err(reason) => {
            state.app.project_settings.running = false;
            state.app.project_settings.result = Some(reason);
            state.app.project_settings.failed_action = Some(action);
        }
    }
    state.dirty = true;
    Ok(())
}

/// Set a project up as `rift init` would, in the progress dialog: the
/// Running frame opens before any work, every step the manager reports
/// re-presents it while the synchronous work holds the loop, and the finished
/// log stays up as a message — green when every step landed, red naming the
/// failure — until the developer closes it. The add flow's act, run here
/// beside the dispatches rather than inside the settings modal's own running
/// state because no modal page is open: the dialog IS the surface.
pub(super) fn run_project_init(
    backend: &mut dyn RenderBackend,
    state: &mut AppState,
    manager: &mut dyn ProjectSettingsManager,
    req_sink: &mut dyn FnMut(DataRequest),
    action: crate::ProjectSettingsAction,
) -> RiftResult<()> {
    let path = action.path.clone();
    let name = state
        .app
        .projects
        .iter()
        .find(|p| p.path == path)
        .map_or_else(
            || path.rsplit('/').next().unwrap_or(&path).to_owned(),
            |p| p.name.clone(),
        );
    state
        .app
        .open_overlay(crate::dispatch::Overlay::running(format!(
            "setup of {name}"
        )));
    state.dirty = true;
    render_if_dirty(backend, state)?; // the empty frame is visible before the first step
    let mut log = crate::SetupLog::default();
    let result = manager.initialize(&action, &mut |progress| {
        log.record(progress);
        if let Some(crate::dispatch::Overlay::Running { lines, .. }) = state.app.overlay.as_mut() {
            *lines = log.lines();
        }
        state.dirty = true;
        // A frame that fails to present is not a setup failure: the work goes
        // on, and the outcome still lands as the message below.
        let _ = render_if_dirty(backend, state);
    });
    let mut body = log.lines();
    match result {
        Ok(view) => {
            if state.app.mode == Mode::ProjectSettings {
                state.app.project_settings.refresh(view);
            }
            state.open_alert(crate::dispatch::Message::success(
                format!("{name} is set up"),
                body,
            ));
        }
        Err(reason) => {
            if !body.is_empty() {
                body.push(String::new());
            }
            body.push(reason);
            state.open_alert(crate::dispatch::Message::error(
                format!("setup of {name} failed"),
                body,
            ));
        }
    }
    req_sink(DataRequest::Refresh(path));
    state.dirty = true;
    Ok(())
}

/// Run one delegated dispatch synchronously: present the running frame, cross
/// the injected dispatcher seam, feed the outcome back through the reducer,
/// and — after a successful MUTATING verb — reload the registry through the
/// injected reload hook, follow the selection, and re-fire the subscribe (the
/// persist-apply pattern), so a palette-side prune is reflected immediately.
pub(super) fn run_dispatch<F, R>(
    host: &mut LoopHost<'_, F, R>,
    state: &mut AppState,
    req: DispatchRequest,
    repo: &str,
) -> RiftResult<()>
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
    R: FnMut() -> RiftResult<Vec<Project>>,
{
    state.dirty = true;
    render_if_dirty(host.backend, state)?; // the running line is visible while we block
    let outcome = host.dispatcher.dispatch(&req, std::path::Path::new(repo));
    let reload_needed = req.is_mutating() && outcome.code == 0;
    if let Flow::Continue(effects) = update(
        state,
        Event::DispatchDone {
            request: req,
            outcome,
        },
    ) {
        if !effects.is_empty() {
            apply_effects(effects, state, host.on_persist, host.req_sink);
        }
    }
    if reload_needed {
        reload_after_mutation(host, state);
    }
    Ok(())
}

/// Reload the registry after a successful mutating dispatch, keeping the
/// selection on the same project and re-firing the subscribe. A failed
/// reload leaves the dashboard as it was: the dispatch itself already landed.
fn reload_after_mutation<F, R>(host: &mut LoopHost<'_, F, R>, state: &mut AppState)
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
    R: FnMut() -> RiftResult<Vec<Project>>,
{
    // A mutating dispatch in a pinned, not-yet-synchronized session first
    // aligns the fold with the shown profile — else the
    // reload hook would hand back the wrong profile's rows.
    if state.select_sync_pending && !state.app.active_profile.is_empty() {
        state.select_sync_pending = false;
        let _ = (host.on_persist)(&PersistOp::SelectProfile {
            profile: state.app.active_profile.clone(),
        });
    }
    let Ok(projects) = (host.reload)() else {
        return;
    };
    let keep = state.app.selected_project().map(|p| p.path.clone());
    state.app.set_projects(projects);
    if let Some(path) = keep {
        state.app.select_path(&path);
    }
    prime_startup(state, host.req_sink);
    state.dirty = true;
}

#[cfg(test)]
mod tests {
    use super::super::event_loop;
    use super::super::fixtures::{
        no_lister, no_persist, one_project_state, MockBackend, ScriptedEvents,
    };
    use crate::state::Event;
    use crate::{CanopyKey, EntryKind, Project};

    /// A dispatcher that records requests and answers with a scripted code.
    struct RecordingDispatcher {
        calls: Vec<(crate::dispatch::DispatchRequest, String)>,
        code: i32,
    }
    impl crate::dispatch::CommandDispatcher for RecordingDispatcher {
        fn dispatch(
            &mut self,
            req: &crate::dispatch::DispatchRequest,
            repo: &std::path::Path,
        ) -> crate::dispatch::DispatchOutcome {
            self.calls
                .push((req.clone(), repo.to_string_lossy().into_owned()));
            crate::dispatch::DispatchOutcome {
                code: self.code,
                human: "scripted".into(),
                json: None,
            }
        }
    }

    fn keys(line: &str) -> Vec<Event> {
        let mut evs = vec![Event::Key(CanopyKey::Char('/'))];
        evs.extend(line.chars().map(|c| Event::Key(CanopyKey::Char(c))));
        evs.push(Event::Key(CanopyKey::Enter));
        evs
    }

    #[test]
    fn read_only_verb_crosses_the_dispatcher_and_lands_in_the_overlay() {
        let mut backend = MockBackend { presents: 0 };
        let mut script = keys("status");
        // The Results overlay swallows the first Esc (dismiss); Quit then exits.
        script.push(Event::Key(CanopyKey::Esc));
        script.push(Event::Key(CanopyKey::Quit));
        let mut src = ScriptedEvents::new(vec![script]);
        let mut state = one_project_state();
        let mut dispatcher = RecordingDispatcher {
            calls: vec![],
            code: 0,
        };
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut |_| {},
            &mut crate::NullProjectSettingsManager,
            &mut dispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut no_lister,
        )
        .unwrap();
        assert_eq!(dispatcher.calls.len(), 1, "one dispatch crossed the seam");
        assert_eq!(
            dispatcher
                .calls
                .first()
                .map(|(r, p)| (r.clone(), p.clone())),
            Some((crate::dispatch::DispatchRequest::Status, "/r/a".into())),
            "the request carries the SELECTED project's path"
        );
        // The outcome landed in the Results overlay (pinned by the reducer
        // test dispatch_done_renders_the_results_overlay); the script's Esc
        // then dismissed it — Esc never quits the dashboard from an overlay.
        assert!(state.app.overlay.is_none(), "Esc dismissed the overlay");
        assert!(backend.presents >= 2, "a running frame presented mid-turn");
    }

    #[test]
    fn mutating_verb_dispatches_only_after_confirm_and_reloads() {
        let mut backend = MockBackend { presents: 0 };
        // /rebuild → confirm overlay → Enter confirms → dispatch → reload.
        let mut script = keys("rebuild");
        script.push(Event::Key(CanopyKey::Enter)); // the confirm
        script.push(Event::Key(CanopyKey::Esc)); // dismiss the results overlay
        script.push(Event::Key(CanopyKey::Quit));
        let mut src = ScriptedEvents::new(vec![script]);
        let mut state = one_project_state();
        let mut dispatcher = RecordingDispatcher {
            calls: vec![],
            code: 0,
        };
        let mut reloads = 0u32;
        let mut reload = || {
            reloads += 1;
            Ok(vec![Project {
                name: "a".into(),
                path: "/r/a".into(),
                status: crate::ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }])
        };
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut |_| {},
            &mut crate::NullProjectSettingsManager,
            &mut dispatcher,
            &mut reload,
            &mut crate::profile::NullProfileLoader,
            &mut no_lister,
        )
        .unwrap();
        assert_eq!(dispatcher.calls.len(), 1);
        assert_eq!(reloads, 1, "a successful mutation reloads the registry");
    }

    #[test]
    fn cancelled_confirm_dispatches_nothing() {
        let mut backend = MockBackend { presents: 0 };
        let mut script = keys("gc");
        script.push(Event::Key(CanopyKey::Esc)); // cancel the confirm
        script.push(Event::Key(CanopyKey::Quit));
        let mut src = ScriptedEvents::new(vec![script]);
        let mut state = one_project_state();
        let mut dispatcher = RecordingDispatcher {
            calls: vec![],
            code: 0,
        };
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut |_| {},
            &mut crate::NullProjectSettingsManager,
            &mut dispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut no_lister,
        )
        .unwrap();
        assert!(dispatcher.calls.is_empty(), "cancel dispatches nothing");
        assert!(state.app.overlay.is_none());
    }
}
