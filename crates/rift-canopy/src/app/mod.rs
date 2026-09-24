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

mod acts;
mod effects;
#[cfg(test)]
mod fixtures;
mod profile_plane;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use rift_core::Result as RiftResult;

use crate::dispatch::{CommandDispatcher, DispatchRequest};
use crate::explorer::DirListing;
use crate::profile::ProfileLoader;
use crate::render::{RenderBackend, RenderError};
use crate::state::{update, AppState, DataRequest, Effects, Event, Flow, PersistOp};
use crate::{Project, ProjectSettingsAction, ProjectSettingsActionKind, ProjectSettingsManager};

use acts::{run_dispatch, run_project_init, run_project_settings_action};
use effects::apply_effects;
use profile_plane::{load_layout, load_project_layout, reload_profile_plane};

pub use profile_plane::{load_profile_plane, prime_startup};

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
    /// Direct read-only Recall Viewer launch. `None` starts the dashboard.
    pub viewer_launch: Option<crate::recall_viewer::ViewerLaunch>,
    /// The canonicalized process launch directory — drives the initial
    /// selection (see [`crate::apply_launch_cwd`]). `None` keeps index 0.
    pub launch_cwd: Option<String>,
    /// Whether the launch cwd sits inside a git repository (bin-resolved).
    /// With no registered match this shows the two-ways-in hint — the cwd is
    /// never auto-added.
    pub launch_cwd_is_repo: bool,
    /// The binary's version, shown in the header. Injected like the
    /// launch cwd so the render crate stays env-free; empty hides the segment.
    pub version: String,
    /// The operator's home directory — the destination explorer's start
    /// (the destination explorer), resolved in the binary by the `HOME` / `USERPROFILE` env
    /// ladder (no `dirs` crate) so the render crate stays env-free. `None`
    /// falls back to the launch cwd, then `/`.
    pub home_dir: Option<String>,
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

/// The host seams one loop run holds, bundled so every phase of an iteration
/// takes one context rather than the loop's ten parameters. It lives only for
/// the length of [`event_loop`] and is never stored in state.
struct LoopHost<'a, F, R> {
    backend: &'a mut dyn RenderBackend,
    on_persist: &'a mut F,
    req_sink: &'a mut dyn FnMut(DataRequest),
    settings_manager: &'a mut dyn ProjectSettingsManager,
    dispatcher: &'a mut dyn CommandDispatcher,
    reload: &'a mut R,
    loader: &'a mut dyn ProfileLoader,
    list_dir: &'a mut dyn FnMut(&Path) -> DirListing,
}

/// The reads a batch asks for that must run AFTER its persist landed, each
/// through the one loader seam (or the lister seam), so they see the store the
/// persist just wrote.
struct PostPersistReads {
    reload_profiles: bool,
    load_layout: Option<String>,
    load_membership: Option<String>,
    load_picker_theme: Option<String>,
    list_dir: Option<PathBuf>,
}

/// The synchronous acts a batch asks for. They run last, and outside
/// [`apply_effects`], because each presents its running frame before its work
/// holds the loop.
struct LoopActs {
    settings_actions: Vec<ProjectSettingsAction>,
    dispatches: Vec<(DispatchRequest, String)>,
}

impl PostPersistReads {
    /// Take the post-persist reads off a batch. `reload_profiles` is copied,
    /// not cleared, so the batch still counts as non-empty for the persist
    /// step exactly as it did before the split.
    fn take(effects: &mut Effects) -> Self {
        Self {
            reload_profiles: effects.reload_profiles,
            load_layout: effects.load_layout.take(),
            load_membership: effects.load_membership.take(),
            load_picker_theme: effects.load_picker_theme.take(),
            list_dir: effects.list_dir.take(),
        }
    }
}

impl LoopActs {
    /// Take the synchronous acts off a batch before its persist lands.
    fn take(effects: &mut Effects) -> Self {
        Self {
            settings_actions: std::mem::take(&mut effects.project_settings_actions),
            dispatches: std::mem::take(&mut effects.dispatches),
        }
    }
}

/// Run the dashboard loop: render-if-dirty, block for events, reduce, apply
/// effects, repeat — until a quit. `on_persist` performs one append-only registry
/// mutation (add/remove/relabel/reorder) and returns the refreshed project
/// list; a persistence failure becomes a transient status line, never an abort.
/// `req_sink` forwards data requests to the live data bus; the bus feeds
/// updates back in through `src` as [`Event::ProjectData`].
///
/// `list_dir` is the destination explorer's lister seam: the
/// reducer only requests a directory; this closure performs the walk and its
/// (infallible) reply lands on the state in the same iteration.
///
/// # Errors
/// [`rift_core::RiftError`] on a terminal present/read failure.
#[allow(clippy::too_many_arguments)] // one explicit host seam, fixed by its callers
pub fn event_loop<F, R>(
    backend: &mut dyn RenderBackend,
    src: &mut dyn EventSource,
    state: &mut AppState,
    on_persist: &mut F,
    req_sink: &mut dyn FnMut(DataRequest),
    settings_manager: &mut dyn ProjectSettingsManager,
    dispatcher: &mut dyn CommandDispatcher,
    reload: &mut R,
    loader: &mut dyn ProfileLoader,
    list_dir: &mut dyn FnMut(&Path) -> DirListing,
) -> RiftResult<()>
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
    R: FnMut() -> RiftResult<Vec<Project>>,
{
    let mut host = LoopHost {
        backend,
        on_persist,
        req_sink,
        settings_manager,
        dispatcher,
        reload,
        loader,
        list_dir,
    };
    start(&mut host, state)?;
    let mut last = Instant::now();
    loop {
        render_if_dirty(host.backend, state)?;
        if state.app.quit {
            return Ok(());
        }
        let events = src.poll(poll_timeout(state))?;
        advance_tween(state, &mut last);
        if events.is_empty() {
            // Timeout → a tick (idle: no redraw, so CPU stays ~0%).
            let _ = update(state, Event::Tick);
            continue;
        }
        for ev in events {
            match update(state, ev) {
                Flow::Quit => state.app.quit = true,
                Flow::Continue(effects) => perform_effects(&mut host, state, effects)?,
            }
        }
    }
}

/// Map a render error onto the shared error type (summary only — never bytes).
pub(crate) fn render_err(e: RenderError) -> rift_core::RiftError {
    rift_core::RiftError::Other(e.to_string())
}

/// The launch seam, run once before the first frame and the first poll.
fn start<F, R>(host: &mut LoopHost<'_, F, R>, state: &mut AppState) -> RiftResult<()> {
    // The profile plane loads BEFORE the first frame: the header
    // shows the restored active profile's name — and the dashboard wears its
    // theme — from frame one. The project list itself was injected
    // by the host (already profile-scoped), so this only fills the identity —
    // it never clobbers the injected rows.
    load_profile_plane(state, host.loader)?;
    // Launch is live by construction: the prime runs at the loop seam, BEFORE
    // the first poll, so the bus computes feeds for the initial selection with
    // zero key events (the blind spot this closes was a subscribe only the
    // headless vehicle ever sent).
    prime_startup(state, host.req_sink);
    // Load the initially-selected project's saved layout (scoped to the active
    // profile), so a rearranged dashboard is restored
    // on launch, before the first frame.
    load_project_layout(state, host.loader)
}

/// Poll fast only while a graph tween is settling; idle otherwise.
fn poll_timeout(state: &AppState) -> Duration {
    if state.anim_active() {
        FRAME_INTERVAL
    } else {
        POLL_INTERVAL
    }
}

/// Advance any in-flight tween by the real time elapsed since the last poll; a
/// settling frame dirties, but at rest `advance_anim` is a no-op and never
/// redraws.
fn advance_tween(state: &mut AppState, last: &mut Instant) {
    let now = Instant::now();
    let dt = now.saturating_duration_since(*last);
    *last = now;
    if state.advance_anim(dt) {
        state.dirty = true;
    }
}

/// Perform one reduced event's effects in a fixed order: the deferred
/// selected-sync joins the persist batch, the batch lands, the reads that must
/// see it run, and the synchronous acts go last.
fn perform_effects<F, R>(
    host: &mut LoopHost<'_, F, R>,
    state: &mut AppState,
    mut effects: Effects,
) -> RiftResult<()>
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
    R: FnMut() -> RiftResult<Vec<Project>>,
{
    // Dispatches and Project Settings actions run HERE (not in
    // apply_effects): their running frames must present before
    // synchronous work holds the loop (the synchronous event loop posture;
    // user-initiated).
    let acts = LoopActs::take(&mut effects);
    let reads = PostPersistReads::take(&mut effects);
    prepend_select_sync(state, &mut effects.persist);
    // Whether an actual switch is in this batch — the sticky-
    // session discriminator.
    let select_landed = selects_profile(&effects.persist);
    if !effects.is_empty() {
        apply_effects(effects, state, host.on_persist, host.req_sink);
    }
    read_after_persist(host, state, reads, select_landed)?;
    run_acts(host, state, acts)
}

/// A pinned launch defers its `selected` write to the
/// session's FIRST persist batch: prepending
/// the synchronizing SelectProfile here aligns the store's
/// fold-active with the shown profile before any op that
/// implicitly targets "the active profile" lands.
fn prepend_select_sync(state: &mut AppState, persist: &mut Vec<PersistOp>) {
    if !state.select_sync_pending || persist.is_empty() {
        return;
    }
    state.select_sync_pending = false;
    if !selects_profile(persist) && !state.app.active_profile.is_empty() {
        persist.insert(
            0,
            PersistOp::SelectProfile {
                profile: state.app.active_profile.clone(),
            },
        );
    }
}

/// Whether a persist batch carries a profile switch of its own.
fn selects_profile(persist: &[PersistOp]) -> bool {
    persist
        .iter()
        .any(|op| matches!(op, PersistOp::SelectProfile { .. }))
}

/// Run the reads a batch asked for, after its persist landed.
fn read_after_persist<F, R>(
    host: &mut LoopHost<'_, F, R>,
    state: &mut AppState,
    reads: PostPersistReads,
    select_landed: bool,
) -> RiftResult<()> {
    // A profile change reloads the whole view — names, active
    // id, membership, selection, arrangement — through the one
    // loader seam (the profile plane), after the persist landed.
    if reads.reload_profiles {
        reload_profile_plane(state, host.loader, host.req_sink, select_landed)?;
    }
    // A selection change loads the new project's saved layout
    // — through the injected loader, applied to state.
    if let Some(path) = reads.load_layout {
        load_layout(state, host.loader, &path)?;
    }
    // The members editor re-reads its rows through the seam
    // (the profile hub) — on entry and after every membership
    // op, AFTER the persist landed (the load_layout pattern).
    if let Some(profile) = reads.load_membership {
        let members = host.loader.projects(&profile)?;
        let catalogue = host.loader.catalogue()?;
        state.app.set_membership_lists(members, catalogue);
        state.dirty = true;
    }
    // A non-active profile's picker swatches load through the
    // seam — the active profile seeds purely.
    if let Some(profile) = reads.load_picker_theme {
        let overrides = host.loader.theme(&profile)?;
        state.app.colour_picker.target_overrides = overrides;
        state.dirty = true;
    }
    // The destination explorer's listing: the
    // walk crosses the injected seam and lands here, in the
    // SAME iteration — before the next `render_if_dirty` — so
    // traversal never shows a loading frame. The reply is
    // infallible: a failure arrives folded into the listing
    // and `apply` keeps the operator where they were.
    if let Some(path) = reads.list_dir {
        state.app.explorer.apply((host.list_dir)(&path));
        state.app.clamp_scrolls(state.size);
        state.dirty = true;
    }
    Ok(())
}

/// Run a batch's synchronous acts: every Project Settings action first (the
/// add flow's `Init` through the progress dialog), then every dispatch.
fn run_acts<F, R>(
    host: &mut LoopHost<'_, F, R>,
    state: &mut AppState,
    acts: LoopActs,
) -> RiftResult<()>
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
    R: FnMut() -> RiftResult<Vec<Project>>,
{
    for action in acts.settings_actions {
        if matches!(action.kind, ProjectSettingsActionKind::Init) {
            run_project_init(
                host.backend,
                state,
                host.settings_manager,
                host.req_sink,
                action,
            )?;
        } else {
            run_project_settings_action(
                host.backend,
                state,
                host.settings_manager,
                host.req_sink,
                action,
            )?;
        }
    }
    for (req, repo) in acts.dispatches {
        run_dispatch(host, state, req, &repo)?;
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

#[cfg(test)]
mod tests {
    use super::fixtures::{
        no_lister, no_persist, one_project_state, ready_state, MockBackend, ScriptedEvents,
    };
    use super::*;
    use crate::render::frame::{Frame, Size};
    use crate::render::Capabilities;
    use crate::state::Event;
    use crate::{CanopyKey, EntryKind};

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

    #[test]
    fn startup_prime_subscribes_before_the_first_poll_with_zero_key_events() {
        // The escape this closes: launch must be live BY CONSTRUCTION —
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
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut no_lister,
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
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut no_lister,
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
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut no_lister,
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
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut no_lister,
        )
        .unwrap();
        // initial render + render after the quit key = 2; the idle tick added none.
        assert_eq!(backend.presents, 2);
    }

    #[test]
    fn tween_polls_fast_then_idle_stays_slow() {
        use crate::col3::{RecallView, SavingsData};
        use crate::state::{FeedPayload, FeedUpdate};
        // The call chart is what animates on this card: the savings figures
        // are headlines over one window and snap whole.
        let feed = |rate: Vec<f64>| {
            Event::Feed(FeedUpdate {
                project: "/r/a".into(),
                payload: FeedPayload::Recall {
                    savings: RecallView::Data(SavingsData {
                        target_pct: Some(90),
                        ..SavingsData::default()
                    }),
                    calls: RecallView::Data(crate::col3::CallsData {
                        total: 3,
                        window_end_ms: 1_800_000,
                        series: vec![crate::col3::ChannelCallSeries {
                            channel: "hook_inject".into(),
                            last_call_at: 1_799_000,
                            rate,
                        }],
                    }),
                },
            })
        };
        let mut backend = MockBackend { presents: 0 };
        let mut src = TimeoutRecordingEvents {
            steps: std::collections::VecDeque::from(vec![
                vec![feed(vec![1.0, 2.0])], // placeholder→Data: snaps (still idle)
                vec![feed(vec![5.0, 6.0])], // Data→Data: arms the tween
                vec![],                     // a settling frame → fast poll
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
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut no_lister,
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
        // Once the sweep is armed, the loop fast-polls (FRAME_INTERVAL)
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
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut no_lister,
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
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut no_lister,
        )
        .unwrap();
        assert_eq!(state.size, Size::new(120, 40));
    }

    /// A backend that records the drawn text of every presented frame, so a
    /// loop test can prove WHAT the operator saw, not just what state held.
    struct RecordingBackend {
        frames: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
    }
    impl RenderBackend for RecordingBackend {
        fn name(&self) -> &'static str {
            "recording"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::empty()
        }
        fn present(&mut self, frame: &Frame) -> Result<(), RenderError> {
            let mut text = String::new();
            for y in 0..frame.height() {
                for x in 0..frame.width() {
                    text.push(frame.get(x, y).map_or(' ', |c| c.ch));
                }
                text.push('\n');
            }
            self.frames.borrow_mut().push(text);
            Ok(())
        }
    }

    #[test]
    fn the_lister_seam_is_invoked_once_and_lands_before_the_next_frame() {
        // the destination explorer: the reducer requests a directory, the LOOP walks it, and
        // the reply lands in the SAME iteration — so traversal never presents a
        // loading state. Proven end to end: the very next frame shows the rows.
        let frames = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut backend = RecordingBackend {
            frames: frames.clone(),
        };
        let mut src = ScriptedEvents::new(vec![
            vec![Event::Key(CanopyKey::Char('a'))],
            vec![Event::Key(CanopyKey::Quit)],
        ]);
        let mut state = ready_state();
        state.size = Size::new(120, 40);
        state.app.home_dir = Some("/home/dev".into());
        let mut asked: Vec<std::path::PathBuf> = Vec::new();
        let mut lister = |path: &std::path::Path| {
            asked.push(path.to_path_buf());
            crate::explorer::DirListing {
                path: path.to_path_buf(),
                entries: vec![crate::explorer::ExplorerEntry {
                    name: "alpha".into(),
                    is_dir: true,
                    tag: None,
                }],
                omitted: 0,
                error: None,
            }
        };
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut |_| {},
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut lister,
        )
        .unwrap();

        assert_eq!(
            asked,
            vec![std::path::PathBuf::from("/home/dev")],
            "the lister is invoked exactly once, with the requested directory"
        );
        assert_eq!(
            state.app.explorer.cwd,
            std::path::PathBuf::from("/home/dev"),
            "the listing landed on the state"
        );
        assert!(frames
            .borrow()
            .iter()
            .any(|f| f.contains("alpha/") && f.contains("add project")));
    }

    #[test]
    fn a_failed_listing_lands_as_an_inline_error_through_the_loop() {
        // The stay-put contract, proven through the loop rather than by unit:
        // the walk fails, the operator keeps the seeded directory and gets a
        // reason — the explorer never ejects them.
        let mut backend = MockBackend { presents: 0 };
        let mut src = ScriptedEvents::new(vec![
            vec![Event::Key(CanopyKey::Char('a'))],
            vec![Event::Key(CanopyKey::Quit)],
        ]);
        let mut state = ready_state();
        state.app.home_dir = Some("/home/dev".into());
        let mut lister = |path: &std::path::Path| crate::explorer::DirListing {
            path: path.to_path_buf(),
            entries: vec![],
            omitted: 0,
            error: Some("permission denied".into()),
        };
        event_loop(
            &mut backend,
            &mut src,
            &mut state,
            &mut no_persist,
            &mut |_| {},
            &mut crate::NullProjectSettingsManager,
            &mut crate::dispatch::NullDispatcher,
            &mut || Ok(vec![]),
            &mut crate::profile::NullProfileLoader,
            &mut lister,
        )
        .unwrap();
        assert_eq!(
            state.app.explorer.error.as_deref(),
            Some("permission denied")
        );
        assert_eq!(
            state.app.explorer.cwd,
            std::path::PathBuf::from("/home/dev"),
            "the seeded directory survived the failure"
        );
        assert!(state.app.explorer.entries.is_empty());
    }
}
