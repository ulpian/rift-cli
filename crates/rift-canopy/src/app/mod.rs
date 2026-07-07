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
pub fn event_loop<F>(
    backend: &mut dyn RenderBackend,
    src: &mut dyn EventSource,
    state: &mut AppState,
    on_persist: &mut F,
    req_sink: &mut dyn FnMut(DataRequest),
    manager: &mut dyn FeatureManager,
) -> RiftResult<()>
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
{
    let mut last = Instant::now();
    loop {
        if state.dirty {
            let vm = state.snapshot();
            let mut frame = backend.begin_frame(state.size);
            crate::view::draw(&vm, backend, &mut frame);
            backend.present(&frame).map_err(render_err)?;
            state.dirty = false;
        }
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
                Flow::Continue(effects) => {
                    if !effects.is_empty() {
                        apply_effects(effects, state, on_persist, req_sink, manager);
                    }
                }
            }
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
    manager: &mut dyn FeatureManager,
) where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
{
    for req in effects.data_requests {
        req_sink(req);
    }
    for op in effects.persist {
        apply_persist(state, on_persist, &op, req_sink);
    }
    // Canopy never installs anything itself — it hands each planned action to the
    // injected manager; a failure surfaces as a status toast, never a crash.
    for action in effects.feature_actions {
        if let Err(reason) = manager.apply(&action) {
            state.app.status = Some(format!("{}: {reason}", action.feature.key()));
            state.dirty = true;
        }
    }
    for action in effects.commands {
        dispatch_command(state, &action.command, on_persist, req_sink);
    }
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
            state.app.status = Some(persist_status(op));
            // Track every current project on the bus (new adds included), and
            // prioritize the selection so its live status/detail refreshes.
            for p in &state.app.projects {
                req_sink(DataRequest::Refresh(p.path.clone()));
            }
            let active = state.app.selected_project().map(|p| p.path.clone());
            req_sink(DataRequest::SetActive(active));
        }
        Err(e) => state.app.status = Some(e.to_string()),
    }
    state.dirty = true;
}

/// A short toast line for a completed persistence op.
fn persist_status(op: &PersistOp) -> String {
    match op {
        PersistOp::AddProject { path } => format!("registered {path}"),
        PersistOp::RemoveProject { path } => format!("removed {path}"),
        PersistOp::RelabelProject { label, .. } => format!("relabeled to {label}"),
        PersistOp::ReorderProject { up, .. } => {
            format!("moved {}", if *up { "up" } else { "down" })
        }
        PersistOp::SetToggle {
            feature, desired, ..
        } => format!("{feature}: {}", if *desired { "on" } else { "off" }),
    }
}

/// Handle a slash command from the palette. Only dashboard-relevant verbs act;
/// the rest report that they are CLI-only for now (mirrors the prior behavior).
fn dispatch_command<F>(
    state: &mut AppState,
    cmd: &crate::Command,
    on_persist: &mut F,
    req_sink: &mut dyn FnMut(DataRequest),
) where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
{
    match cmd.name.as_str() {
        "setup" => match std::env::current_dir() {
            Ok(dir) => {
                let op = PersistOp::AddProject {
                    path: dir.to_string_lossy().into_owned(),
                };
                apply_persist(state, on_persist, &op, req_sink);
            }
            Err(e) => state.app.status = Some(format!("setup failed: {e}")),
        },
        "quit" => state.app.quit = true,
        other => {
            state.app.status = Some(format!("/{other} is only available from the CLI for now"));
        }
    }
    state.dirty = true;
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
        s
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
        )
        .unwrap();
        assert_eq!(seen, vec!["/tmp/x".to_string()]);
        assert!(state.app.status.unwrap().contains("registered /tmp/x"));
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
    fn failed_install_sets_status_not_panic() {
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
        )
        .unwrap();
        let status = state.app.status.expect("a failure toast");
        assert!(status.contains("install refused"), "got {status:?}");
    }
}
