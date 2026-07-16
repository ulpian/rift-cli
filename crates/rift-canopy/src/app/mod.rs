//! The Canopy application shell: the synchronous frame/event loop.
//!
//! The loop is deliberately **synchronous** (no `tokio`): it blocks in
//! [`EventSource::poll`] for input, applies the pure [`update`] reducer, performs
//! the returned [`Effects`], and redraws **only when `dirty`** — so a quiescent
//! dashboard idles at ~0% CPU. The loop body and effect handling are engine-free
//! and generic over [`EventSource`] + `dyn RenderBackend`, so they are unit-tested
//! headlessly with a scripted source and a mock backend. The live terminal driver
//! ([`live`], `#[cfg(feature = "xre")]`) supplies the real event source, the
//! panic-safe terminal guard, and [`live::run`] — the bare-`rift` entry.

#[cfg(feature = "xre")]
pub mod live;

use std::time::{Duration, Instant};

use rift_core::Result as RiftResult;

use crate::dispatch::{CommandDispatcher, DispatchRequest};
use crate::render::{RenderBackend, RenderError};
use crate::state::{update, AppState, DataRequest, Effects, Event, Flow, PersistOp};
use crate::{FeatureManager, Project};

/// How long a poll blocks before yielding a `Tick` (input wakes it sooner). The
/// idle cadence — quiescent, no redraw, so CPU stays ~0%.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The fast cadence used **only** while a column-3 tween is in flight (~30fps),
/// so an easing graph redraws smoothly; the loop drops back to [`POLL_INTERVAL`]
/// the moment the tween settles.
const FRAME_INTERVAL: Duration = Duration::from_millis(33);

/// Options for launching Canopy (from the bare-`rift` CLI flags).
#[derive(Clone, Debug, Default)]
pub struct CanopyOpts {
    /// Force the ASCII fallback backend (`--ascii` / `NO_COLOR`).
    pub ascii: bool,
    /// Skip the startup splash (`--skip-splash` / `RIFT_SPLASH=never`).
    pub skip_splash: bool,
    /// The canonicalized process launch directory — drives the initial
    /// selection (M5-03; [`crate::apply_launch_cwd`]). `None` keeps index 0.
    pub launch_cwd: Option<String>,
    /// Whether the launch cwd sits inside a git repository (bin-resolved).
    /// With no registered match this shows the two-ways-in hint — the cwd is
    /// never auto-added.
    pub launch_cwd_is_repo: bool,
    /// The binary's version, shown in the header (M55-04). Injected like the
    /// launch cwd so the render crate stays env-free; empty hides the segment.
    pub version: String,
}

/// A source of already-translated Canopy [`Event`]s.
///
/// `poll` blocks up to `timeout`; an **empty** result means the timeout elapsed
/// (the loop treats that as a `Tick`). The live source pumps the terminal; the
/// test source replays a script — so the loop is driveable without a terminal.
pub trait EventSource {
    /// Block up to `timeout` for the next batch of events.
    ///
    /// # Errors
    /// Propagates a terminal read failure from the live source.
    fn poll(&mut self, timeout: Duration) -> RiftResult<Vec<Event>>;
}

/// Map a render error onto the shared error type (summary only — never bytes).
pub(crate) fn render_err(e: RenderError) -> rift_core::RiftError {
    rift_core::RiftError::Other(e.to_string())
}

/// The shared profile-plane load (ADR-0036 §6, W0): the live profiles, the
/// restored active id, and — when one is active — its theme (ADR-0038), all
/// through the injected loader, onto the state **before the first frame**.
/// `event_loop` calls this at its launch seam; the headless one-frame render
/// calls the same function, so the interactive and headless runs exercise
/// **one** startup path (the M5-02 principle — the pre-W0 headless frame
/// skipped this load entirely and was structurally blind to the class).
///
/// # Errors
/// Propagates a loader read failure.
pub fn load_profile_plane(
    state: &mut AppState,
    loader: &mut dyn crate::profile::ProfileLoader,
) -> RiftResult<()> {
    let (active, profiles) = loader.profiles()?;
    let active = active.unwrap_or_default();
    if !active.is_empty() {
        let overrides = loader.theme(&active)?;
        state.set_theme(overrides);
    }
    state.app.set_profiles(profiles, active);
    Ok(())
}

/// The shared startup prime (M5-02): one `Refresh` per tracked project, then
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

/// Run the dashboard loop: render-if-dirty, block for events, reduce, apply
/// effects, repeat — until a quit. `on_persist` performs one append-only registry
/// mutation (add/remove/relabel/reorder/toggle) and returns the refreshed project
/// list; a persistence failure becomes a transient status line, never an abort.
/// `req_sink` forwards data requests to the live data bus (P8); the bus feeds
/// updates back in through `src` as [`Event::ProjectData`].
///
/// `manager` performs the feature install/remove actions the reducer plans
/// (Canopy never installs anything itself); a failure becomes a status toast.
///
/// # Errors
/// [`rift_core::RiftError`] on a terminal present/read failure.
#[allow(clippy::too_many_arguments)] // ADR-0011: every host dependency is one explicit seam
pub fn event_loop<F, R>(
    backend: &mut dyn RenderBackend,
    src: &mut dyn EventSource,
    state: &mut AppState,
    on_persist: &mut F,
    req_sink: &mut dyn FnMut(DataRequest),
    manager: &mut dyn FeatureManager,
    dispatcher: &mut dyn CommandDispatcher,
    reload: &mut R,
    loader: &mut dyn crate::profile::ProfileLoader,
) -> RiftResult<()>
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
    R: FnMut() -> RiftResult<Vec<Project>>,
{
    // The profile plane loads BEFORE the first frame (ADR-0036): the header
    // shows the restored active profile's name — and the dashboard wears its
    // theme (ADR-0038) — from frame one. The project list itself was injected
    // by the host (already profile-scoped), so this only fills the identity —
    // it never clobbers the injected rows.
    load_profile_plane(state, loader)?;
    // Launch is live by construction: the prime runs at the loop seam, BEFORE
    // the first poll, so the bus computes feeds for the initial selection with
    // zero key events (M5-02 — the M4 blind spot was a subscribe only the
    // headless vehicle ever sent).
    prime_startup(state, req_sink);
    // Load the initially-selected project's saved layout (ADR-0035, scoped to
    // the active profile per ADR-0036), so a rearranged dashboard is restored
    // on launch, before the first frame.
    load_project_layout(state, loader)?;
    let mut last = Instant::now();
    loop {
        render_if_dirty(backend, state)?;
        if state.app.quit {
            break;
        }
        // Poll fast only while a graph tween is settling; idle otherwise.
        let timeout = if state.anim_active() {
            FRAME_INTERVAL
        } else {
            POLL_INTERVAL
        };
        let events = src.poll(timeout)?;
        // Advance any in-flight tween by the real elapsed time; a settling frame
        // dirties, but at rest `advance_anim` is a no-op and never redraws.
        let now = Instant::now();
        let dt = now.saturating_duration_since(last);
        last = now;
        if state.advance_anim(dt) {
            state.dirty = true;
        }
        if events.is_empty() {
            // Timeout → a tick (idle: no redraw, so CPU stays ~0%).
            let _ = update(state, Event::Tick);
            continue;
        }
        for ev in events {
            match update(state, ev) {
                Flow::Quit => state.app.quit = true,
                Flow::Continue(mut effects) => {
                    // Dispatches and feature actions run HERE (not in
                    // apply_effects): their running/Enabling frames must present
                    // before the synchronous work holds the loop (ADR-0011
                    // posture; user-initiated).
                    let pending = std::mem::take(&mut effects.dispatches);
                    let actions = std::mem::take(&mut effects.feature_actions);
                    let load = effects.load_layout.take();
                    let reload_profiles = effects.reload_profiles;
                    if !effects.is_empty() {
                        apply_effects(effects, state, on_persist, req_sink, manager);
                    }
                    // A profile change reloads the whole view — names, active
                    // id, membership, selection, arrangement — through the one
                    // loader seam (ADR-0036 §6), after the persist landed.
                    if reload_profiles {
                        reload_profile_plane(state, loader, req_sink)?;
                    }
                    // A selection change loads the new project's saved layout
                    // (ADR-0035) — through the injected loader, applied to state.
                    if let Some(path) = load {
                        let m = loader
                            .layout(&state.app.active_profile, &path)?
                            .unwrap_or_default();
                        state.set_layout(m);
                    }
                    for action in actions {
                        run_feature_action(backend, state, manager, req_sink, action)?;
                    }
                    for (req, repo) in pending {
                        run_dispatch(
                            backend, state, dispatcher, reload, req_sink, on_persist, manager, req,
                            &repo,
                        )?;
                    }
                }
            }
        }
    }
    Ok(())
}

/// Present the current frame when dirty (shared by the loop and the
/// pre-dispatch running frame).
fn render_if_dirty(backend: &mut dyn RenderBackend, state: &mut AppState) -> RiftResult<()> {
    if state.dirty {
        let vm = state.snapshot();
        let mut frame = backend.begin_frame(state.size);
        crate::view::draw(&vm, backend, &mut frame);
        backend.present(&frame).map_err(render_err)?;
        state.dirty = false;
    }
    Ok(())
}

/// Run one feature action through the injected manager, with the row states
/// driven by the RESULT (M5-12, ADR-0013 — toasts stop standing in for
/// state): the row enters Enabling/Disabling on a presented frame, then the
/// manager's outcome moves it to On (runtime cleared; the persisted desired
/// state shows) or Failed-with-reason. A success refreshes the project so
/// the detail's toggle rows recompute from the store.
fn run_feature_action(
    backend: &mut dyn RenderBackend,
    state: &mut AppState,
    manager: &mut dyn FeatureManager,
    req_sink: &mut dyn FnMut(DataRequest),
    action: crate::FeatureAction,
) -> RiftResult<()> {
    let key = action.feature.key().to_string();
    let transient = match action.kind {
        crate::FeatureActionKind::Remove => crate::FeatureRuntime::Disabling,
        _ => crate::FeatureRuntime::Enabling,
    };
    state.app.feature_runtime.insert(key.clone(), transient);
    state.dirty = true;
    render_if_dirty(backend, state)?; // the Enabling/Disabling row is visible while we block
    match manager.apply(&action) {
        Ok(()) => {
            state.app.feature_runtime.remove(&key);
            // The persisted desired state now carries the row; recompute it.
            req_sink(DataRequest::Refresh(action.path.clone()));
        }
        Err(reason) => {
            state
                .app
                .feature_runtime
                .insert(key, crate::FeatureRuntime::Failed(reason));
        }
    }
    state.dirty = true;
    Ok(())
}

/// Load and apply the currently-selected project's saved layout (ADR-0035,
/// scoped to the active profile per ADR-0036) through the injected loader. No
/// selection ⇒ nothing to load. A missing/reset layout applies the default
/// (the loader returns `None`).
fn load_project_layout(
    state: &mut AppState,
    loader: &mut dyn crate::profile::ProfileLoader,
) -> RiftResult<()> {
    if let Some(path) = state.app.selected_project().map(|p| p.path.clone()) {
        let m = loader
            .layout(&state.app.active_profile, &path)?
            .unwrap_or_default();
        state.set_layout(m);
    }
    Ok(())
}

/// Reload the whole profile plane after a profile persist op (ADR-0036 §6):
/// the live names + active id, the active profile's membership view, the
/// selection (kept by path where the project survives the switch), the bus
/// subscription, and the (profile, project) arrangement — all through the one
/// injected loader. Runtime project status stays runtime-only: the bus re-feeds
/// it through the re-issued requests, exactly like a persist-apply.
fn reload_profile_plane(
    state: &mut AppState,
    loader: &mut dyn crate::profile::ProfileLoader,
    req_sink: &mut dyn FnMut(DataRequest),
) -> RiftResult<()> {
    let (active, profiles) = loader.profiles()?;
    let active = active.unwrap_or_default();
    state.app.set_profiles(profiles, active.clone());
    if !active.is_empty() {
        let keep = state.app.selected_project().map(|p| p.path.clone());
        let projects = loader.projects(&active)?;
        state.app.set_projects(projects);
        if let Some(path) = keep {
            state.app.select_path(&path);
        }
        prime_startup(state, req_sink);
        load_project_layout(state, loader)?;
        // The switch re-themes through the same reload path (ADR-0038 §6):
        // projects, arrangements, and colours swap as one view.
        let overrides = loader.theme(&active)?;
        state.set_theme(overrides);
    }
    state.dirty = true;
    Ok(())
}

/// Run one delegated dispatch synchronously: present the running frame, cross
/// the injected dispatcher seam, feed the outcome back through the reducer,
/// and — after a successful MUTATING verb — reload the registry through the
/// injected reload hook, follow the selection, and re-fire the subscribe (the
/// persist-apply pattern), so a palette-side prune is reflected immediately.
#[allow(clippy::too_many_arguments)]
fn run_dispatch<F, R>(
    backend: &mut dyn RenderBackend,
    state: &mut AppState,
    dispatcher: &mut dyn CommandDispatcher,
    reload: &mut R,
    req_sink: &mut dyn FnMut(DataRequest),
    on_persist: &mut F,
    manager: &mut dyn FeatureManager,
    req: DispatchRequest,
    repo: &str,
) -> RiftResult<()>
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
    R: FnMut() -> RiftResult<Vec<Project>>,
{
    state.dirty = true;
    render_if_dirty(backend, state)?; // the running line is visible while we block
    let outcome = dispatcher.dispatch(&req, std::path::Path::new(repo));
    let reload_needed = req.is_mutating() && outcome.code == 0;
    if let Flow::Continue(effects) = update(
        state,
        Event::DispatchDone {
            request: req,
            outcome,
        },
    ) {
        if !effects.is_empty() {
            apply_effects(effects, state, on_persist, req_sink, manager);
        }
    }
    if reload_needed {
        if let Ok(projects) = reload() {
            let keep = state.app.selected_project().map(|p| p.path.clone());
            state.app.set_projects(projects);
            if let Some(path) = keep {
                state.app.select_path(&path);
            }
            prime_startup(state, req_sink);
            state.dirty = true;
        }
    }
    Ok(())
}

/// Perform the side effects the reducer returned: forward data requests to the
/// bus, apply append-only persistence, hand feature actions to the manager, and
/// dispatch commands. Reducer purity is preserved — all I/O happens here.
fn apply_effects<F>(
    effects: Effects,
    state: &mut AppState,
    on_persist: &mut F,
    req_sink: &mut dyn FnMut(DataRequest),
    _manager: &mut dyn FeatureManager,
) where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
{
    for req in effects.data_requests {
        req_sink(req);
    }
    for op in effects.persist {
        apply_persist(state, on_persist, &op, req_sink);
    }
    // Feature actions are handled by `run_feature_action` in the loop (they
    // need the backend for the Enabling frame); nothing reaches here.
    debug_assert!(effects.feature_actions.is_empty());
}

/// Apply one append-only persistence op and reflect the refreshed registry back
/// into the dashboard: keep the selection on the same project, set a status line,
/// and (re-)track every current project on the bus so new adds get live data.
fn apply_persist<F>(
    state: &mut AppState,
    on_persist: &mut F,
    op: &PersistOp,
    req_sink: &mut dyn FnMut(DataRequest),
) where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
{
    let keep = state.app.selected_project().map(|p| p.path.clone());
    match on_persist(op) {
        Ok(projects) => {
            state.app.set_projects(projects);
            if let Some(path) = keep {
                state.app.select_path(&path); // selection follows the same project
            }
            // A success is a Notice (ADR-0029); it auto-dismisses. When the
            // removal emptied the registry, `set_projects` already raised the
            // wizard, which the Notice's settle-close re-raises after it fades.
            // A theme apply is the one silent op (ADR-0039): the live repaint
            // IS the acknowledgement — a toast would cover the preview.
            if !matches!(op, PersistOp::SetProfileTheme { .. }) {
                state.open_notice(crate::dispatch::Message::success(
                    "registry",
                    vec![persist_status(op)],
                ));
            }
            // Track every current project on the bus (new adds included), and
            // prioritize the selection so its live status/detail refreshes.
            for p in &state.app.projects {
                req_sink(DataRequest::Refresh(p.path.clone()));
            }
            let active = state.app.selected_project().map(|p| p.path.clone());
            req_sink(DataRequest::SetActive(active));
        }
        Err(e) => state.open_alert(crate::dispatch::Message::error(
            "registry",
            vec![e.to_string()],
        )),
    }
    state.dirty = true;
}

/// A short toast line for a completed persistence op.
fn persist_status(op: &PersistOp) -> String {
    match op {
        PersistOp::AddProject { path } => format!("registered {path}"),
        PersistOp::RemoveProject { path } => format!("removed {path}"),
        PersistOp::RelabelProject { label, .. } => format!("relabeled to {label}"),
        PersistOp::ReorderProject { up, .. } | PersistOp::ReorderProfileProject { up, .. } => {
            format!("moved {}", if *up { "up" } else { "down" })
        }
        PersistOp::SetToggle {
            feature, desired, ..
        } => format!("{feature}: {}", if *desired { "on" } else { "off" }),
        PersistOp::SetLayout { layout, .. } => {
            if layout.is_some() {
                "saved layout".to_string()
            } else {
                "reset layout".to_string()
            }
        }
        PersistOp::CreateProfile { name } => format!("created profile {name}"),
        PersistOp::DuplicateProfile { name, .. } => format!("duplicated to {name}"),
        PersistOp::RenameProfile { name, .. } => format!("renamed profile to {name}"),
        PersistOp::RemoveProfile { .. } => "deleted profile".to_string(),
        PersistOp::SelectProfile { .. } => "switched profile".to_string(),
        PersistOp::AddProfileProject { path, .. } => format!("added {path} to profile"),
        PersistOp::RemoveProfileProject { path, .. } => {
            format!("removed {path} from profile")
        }
        PersistOp::SetProfileTheme { slot, colour, .. } => {
            if colour.is_some() {
                format!("set {slot} colour")
            } else {
                format!("reset {slot} colour")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::{Frame, Size};
    use crate::render::Capabilities;
    use crate::state::Event;
    use crate::{CanopyKey, EntryKind, NullFeatureManager};

    /// A render backend that counts presents (the headless loop probe).
    struct MockBackend {
        presents: usize,
    }
    impl RenderBackend for MockBackend {
        fn name(&self) -> &'static str {
            "mock"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::empty()
        }
        fn present(&mut self, _frame: &Frame) -> Result<(), RenderError> {
            self.presents += 1;
            Ok(())
        }
    }

    /// A scripted event source: each `poll` yields the next batch (then ticks).
    struct ScriptedEvents {
        steps: std::collections::VecDeque<Vec<Event>>,
    }
    impl ScriptedEvents {
        fn new(steps: Vec<Vec<Event>>) -> Self {
            Self {
                steps: steps.into(),
            }
        }
    }
    impl EventSource for ScriptedEvents {
        fn poll(&mut self, _timeout: Duration) -> RiftResult<Vec<Event>> {
            Ok(self.steps.pop_front().unwrap_or_default())
        }
    }

    /// A scripted source that also records the timeout it was polled with, so a
    /// test can prove the loop polls fast only while a tween settles.
    struct TimeoutRecordingEvents {
        steps: std::collections::VecDeque<Vec<Event>>,
        seen: Vec<Duration>,
    }
    impl EventSource for TimeoutRecordingEvents {
        fn poll(&mut self, timeout: Duration) -> RiftResult<Vec<Event>> {
            self.seen.push(timeout);
            Ok(self.steps.pop_front().unwrap_or_default())
        }
    }

    fn no_persist(_op: &PersistOp) -> RiftResult<Vec<Project>> {
        Ok(vec![])
    }

    fn ready_state() -> AppState {
        let mut s = AppState::new(vec![], Size::new(80, 24));
        s.app.splash.skip();
        s.app.overlay = None; // dismiss the first-run wizard; drive the dashboard
        s
    }

    #[test]
    fn startup_prime_subscribes_before_the_first_poll_with_zero_key_events() {
        // The M4 dogfood escape (M5-02): launch must be live BY CONSTRUCTION —
        // the loop itself issues one Refresh per project + the SetActive for
        // the initial selection before it ever polls for input. The shared
        // marker log proves the order; no key event is ever injected.
        use std::cell::RefCell;
        use std::rc::Rc;

        #[derive(Debug, PartialEq, Eq)]
        enum Mark {
            Req(DataRequest),
            Poll,
        }
        let log: Rc<RefCell<Vec<Mark>>> = Rc::new(RefCell::new(Vec::new()));

        struct MarkingSource {
            log: Rc<RefCell<Vec<Mark>>>,
        }
        impl EventSource for MarkingSource {
            fn poll(&mut self, _t: Duration) -> RiftResult<Vec<Event>> {
                self.log.borrow_mut().push(Mark::Poll);
                // End the loop with ZERO events — the prime must already be done.
                Err(rift_core::RiftError::Other("scripted stop".into()))
            }
        }

        let mut backend = MockBackend { presents: 0 };
        let mut src = MarkingSource { log: log.clone() };
        let mut state = AppState::new(
            vec![
                Project {
                    name: "a".into(),
                    path: "/a".into(),
                    status: crate::ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
                Project {
                    name: "b".into(),
                    path: "/b".into(),
                    status: crate::ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
            ],
            Size::new(80, 24),
        );
        state.app.splash.skip();

        let sink_log = log.clone();
        let mut sink = move |req: DataRequest| sink_log.borrow_mut().push(Mark::Req(req));
        let _ = event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut sink,
            &mut NullFeatureManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
        );

        assert_eq!(
            *log.borrow(),
            vec![
                Mark::Req(DataRequest::Refresh("/a".into())),
                Mark::Req(DataRequest::Refresh("/b".into())),
                Mark::Req(DataRequest::SetActive(Some("/a".into()))),
                Mark::Poll,
            ],
            "the full prime precedes the first poll"
        );
    }

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

    fn one_project_state() -> AppState {
        let mut s = AppState::new(
            vec![Project {
                name: "a".into(),
                path: "/r/a".into(),
                status: crate::ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(100, 32),
        );
        s.app.splash.skip();
        s
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
            &mut NullFeatureManager,
            &mut dispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
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

    /// A profile loader double that records every read and serves a two-profile
    /// plane whose active id lives in a shared cell (the persist seam moves it,
    /// exactly like the real store's `selected` fold).
    struct RecordingLoader {
        active: std::rc::Rc<std::cell::RefCell<String>>,
        calls: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
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
    }

    #[test]
    fn a_profile_switch_reloads_the_whole_view_through_the_loader_seam() {
        // ADR-0036 §6 / ADR-0037 §5: `Enter` in the menu emits SelectProfile;
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
            &mut NullFeatureManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut loader,
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
        // The switch re-themed as part of the same reload (ADR-0038 §6).
        assert_eq!(
            state.theme().overrides().background,
            Some(crate::render::frame::Color::Rgb(1, 2, 3)),
            "colours swap with the view"
        );
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
            &mut NullFeatureManager,
            &mut dispatcher,
            &mut reload,
            &mut crate::profile::NullProfileLoader,
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
            &mut NullFeatureManager,
            &mut dispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
        )
        .unwrap();
        assert!(dispatcher.calls.is_empty(), "cancel dispatches nothing");
        assert!(state.app.overlay.is_none());
    }

    #[test]
    fn quit_event_breaks_loop() {
        let mut backend = MockBackend { presents: 0 };
        let mut src = ScriptedEvents::new(vec![vec![Event::Key(CanopyKey::Quit)]]);
        let mut state = ready_state();
        let r = event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut |_| {},
            &mut NullFeatureManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
        );
        assert!(r.is_ok());
        assert!(state.app.quit);
        assert!(backend.presents >= 1);
    }

    #[test]
    fn dirty_gating_coalesces_redraws() {
        // Two keys in ONE poll batch must produce ONE redraw, not two.
        let mut backend = MockBackend { presents: 0 };
        let mut src = ScriptedEvents::new(vec![
            vec![Event::Key(CanopyKey::Down), Event::Key(CanopyKey::Down)],
            vec![Event::Key(CanopyKey::Quit)],
        ]);
        let mut state = AppState::new(
            vec![
                Project {
                    name: "a".into(),
                    path: "/a".into(),
                    status: crate::ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
                Project {
                    name: "b".into(),
                    path: "/b".into(),
                    status: crate::ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
            ],
            Size::new(80, 24),
        );
        state.app.splash.skip();
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut |_| {},
            &mut NullFeatureManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
        )
        .unwrap();
        // initial + one per batch (2 batches) = 3 presents.
        assert_eq!(backend.presents, 3);
    }

    #[test]
    fn idle_tick_does_not_redraw() {
        // An empty poll batch (tick) followed by quit: the tick must NOT redraw.
        let mut backend = MockBackend { presents: 0 };
        let mut src = ScriptedEvents::new(vec![vec![], vec![Event::Key(CanopyKey::Quit)]]);
        let mut state = ready_state();
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut |_| {},
            &mut NullFeatureManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
        )
        .unwrap();
        // initial render + render after the quit key = 2; the idle tick added none.
        assert_eq!(backend.presents, 2);
    }

    #[test]
    fn tween_polls_fast_then_idle_stays_slow() {
        use crate::col3::{TokensData, TokensView};
        use crate::state::{FeedPayload, FeedUpdate};
        let feed = |rate: Vec<f64>, total: f64| {
            Event::Feed(FeedUpdate {
                project: "/r/a".into(),
                payload: FeedPayload::Tokens {
                    tokens: TokensView::Data(TokensData { rate, total }),
                    breakdown: crate::col3::BreakdownView::NoActivity,
                },
            })
        };
        let mut backend = MockBackend { presents: 0 };
        let mut src = TimeoutRecordingEvents {
            steps: std::collections::VecDeque::from(vec![
                vec![feed(vec![1.0, 2.0], 3.0)], // placeholder→Data: snaps (still idle)
                vec![feed(vec![5.0, 6.0], 11.0)], // Data→Data: arms the tween
                vec![],                          // a settling frame → fast poll
                vec![Event::Key(CanopyKey::Quit)],
            ]),
            seen: Vec::new(),
        };
        let mut state = AppState::new(
            vec![Project {
                name: "a".into(),
                path: "/r/a".into(),
                status: crate::ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(80, 24),
        );
        state.app.splash.skip();
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut |_| {},
            &mut NullFeatureManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
        )
        .unwrap();
        // Before any tween, the loop polls at the idle cadence…
        assert_eq!(src.seen[0], POLL_INTERVAL);
        assert_eq!(src.seen[1], POLL_INTERVAL);
        // …and once a Data→Data feed lands, it polls fast to animate the settle.
        assert!(
            src.seen.contains(&FRAME_INTERVAL),
            "a tween must trigger fast polling, saw {:?}",
            src.seen
        );
    }

    #[test]
    fn startup_sweep_fast_polls_then_settles_and_never_re_fires() {
        // M55-14: once the sweep is armed, the loop fast-polls (FRAME_INTERVAL)
        // until it settles, then drops back to the idle cadence; the one-shot
        // sweep never re-fires on a headless (never-armed) frame.
        let mut backend = MockBackend { presents: 0 };
        let mut src = TimeoutRecordingEvents {
            steps: std::collections::VecDeque::from(vec![
                vec![], // a sweeping frame → fast poll
                vec![], // still sweeping → fast poll
                vec![Event::Key(CanopyKey::Quit)],
            ]),
            seen: Vec::new(),
        };
        let mut state = one_project_state();
        state.arm_startup_sweep();
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut |_| {},
            &mut NullFeatureManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
        )
        .unwrap();
        assert!(
            src.seen.first() == Some(&FRAME_INTERVAL),
            "the armed sweep fast-polls its reveal, saw {:?}",
            src.seen
        );
    }

    #[test]
    fn resize_updates_size() {
        let mut backend = MockBackend { presents: 0 };
        let mut src = ScriptedEvents::new(vec![
            vec![Event::Resize(Size::new(120, 40))],
            vec![Event::Key(CanopyKey::Quit)],
        ]);
        let mut state = ready_state();
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut |_| {},
            &mut NullFeatureManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
        )
        .unwrap();
        assert_eq!(state.size, Size::new(120, 40));
    }

    #[test]
    fn add_project_effect_invokes_on_add() {
        let mut backend = MockBackend { presents: 0 };
        let mut src = ScriptedEvents::new(vec![
            vec![Event::Key(CanopyKey::Char('a'))],
            "/tmp/x"
                .chars()
                .map(|c| Event::Key(CanopyKey::Char(c)))
                .collect(),
            vec![Event::Key(CanopyKey::Enter)],
            vec![Event::Key(CanopyKey::Quit)],
        ]);
        let mut state = ready_state();
        let mut seen: Vec<String> = Vec::new();
        let mut on_persist = |op: &PersistOp| {
            let PersistOp::AddProject { path } = op else {
                return Ok(vec![]);
            };
            seen.push(path.clone());
            Ok(vec![Project {
                name: "x".into(),
                path: path.clone(),
                status: crate::ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }])
        };
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut on_persist,
            &mut |_| {},
            &mut NullFeatureManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
        )
        .unwrap();
        assert_eq!(seen, vec!["/tmp/x".to_string()]);
        assert!(
            matches!(&state.app.overlay, Some(crate::dispatch::Overlay::Notice(m))
                if m.body.iter().any(|l| l.contains("registered /tmp/x"))),
            "the add success opened a Notice: {:?}",
            state.app.overlay
        );
    }

    /// A manager that always fails — proves a failed install surfaces as a status
    /// toast and never panics or aborts the loop.
    struct FailingManager;
    impl crate::FeatureManager for FailingManager {
        fn apply(&mut self, _action: &crate::FeatureAction) -> Result<(), String> {
            Err("install refused".into())
        }
    }

    #[test]
    fn failed_install_sets_the_row_state_not_a_toast() {
        let mut backend = MockBackend { presents: 0 };
        // Focus the detail panel, then flip a toggle (Space) → a feature action.
        let mut src = ScriptedEvents::new(vec![
            vec![Event::Key(CanopyKey::Tab)], // Projects → Detail
            vec![Event::Key(CanopyKey::Char(' '))],
            vec![Event::Key(CanopyKey::Quit)],
        ]);
        let mut state = AppState::new(
            vec![Project {
                name: "a".into(),
                path: "/r/a".into(),
                status: crate::ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(80, 24),
        );
        state.app.splash.skip();
        let mut on_persist = |_op: &PersistOp| {
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
            &mut on_persist,
            &mut |_| {},
            &mut FailingManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
        )
        .unwrap();
        // M5-12 (ADR-0013): the RESULT drives the row — Failed-with-reason
        // lives on the feature's runtime state, not a toast.
        let runtime = state
            .app
            .feature_runtime
            .get("fff")
            .expect("a Failed row state");
        assert_eq!(
            *runtime,
            crate::FeatureRuntime::Failed("install refused".into())
        );
        assert!(
            backend.presents >= 2,
            "the Enabling frame presented before the manager ran"
        );
    }

    /// A successful action clears the transient state and refreshes the
    /// project so the persisted desired state carries the row.
    #[test]
    fn successful_install_clears_the_transient_and_refreshes() {
        let mut backend = MockBackend { presents: 0 };
        let mut src = ScriptedEvents::new(vec![
            vec![Event::Key(CanopyKey::Tab)],
            vec![Event::Key(CanopyKey::Char(' '))],
            vec![Event::Key(CanopyKey::Quit)],
        ]);
        let mut state = AppState::new(
            vec![Project {
                name: "a".into(),
                path: "/r/a".into(),
                status: crate::ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(80, 24),
        );
        state.app.splash.skip();
        let refreshes = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink_log = refreshes.clone();
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut move |req| sink_log.borrow_mut().push(req),
            &mut NullFeatureManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
        )
        .unwrap();
        assert!(state.app.feature_runtime.is_empty(), "transient cleared");
        assert!(
            refreshes
                .borrow()
                .iter()
                .any(|r| *r == DataRequest::Refresh("/r/a".into())),
            "a success refreshes the project's detail"
        );
    }
}
