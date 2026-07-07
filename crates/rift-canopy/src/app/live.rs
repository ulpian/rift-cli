//! The live terminal driver: the panic-safe guard, the real event source, and
//! [`run`] — the bare-`rift` entry. The only place the app shell touches the
//! engine's terminal layer; all loop logic lives in the engine-free parent.

use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use rift_core::Result as RiftResult;
use xre_term::{
    Event as XEvent, EventQueue, GuardOptions, KeyCode, KeyState, Modifiers,
    MouseButton as XMouseButton, MouseEvent as XMouseEvent, MouseKind as XMouseKind, TermError,
    TerminalGuard,
};

use super::{event_loop, CanopyOpts, EventSource};
use crate::dispatch::{CommandDispatcher, EditorLauncher};
use crate::render::frame::Size;
use crate::state::{AppState, DataRequest, Event, PersistOp};
use crate::{CanopyKey, FeatureManager, MouseButton, MouseInput, MouseKind, Project};

/// An RAII terminal guard. Wraps `xre_term::TerminalGuard`, which restores the
/// terminal on drop **and** on panic (its own process-wide hook) — so we never
/// hand-roll a panic hook. Mouse capture is opt-in per `mouse` (SGR reporting is
/// a terminal-wide mode; while on, native drag-to-select is suppressed —
/// Shift/Option-drag still copies in most terminals). Keyboard-enhancement /
/// pointer-shape stay off (so key semantics are unchanged).
pub struct Terminal {
    _guard: TerminalGuard,
}

impl Terminal {
    /// Enter raw mode + alternate screen, optionally capturing the mouse.
    /// Restored automatically on drop/panic.
    ///
    /// # Errors
    /// [`rift_core::RiftError`] if entering raw mode fails.
    pub fn enter(mouse: bool) -> RiftResult<Self> {
        let guard = TerminalGuard::enter_with(GuardOptions {
            mouse,
            keyboard_enhancement: false,
            pointer_shape: None,
        })
        .map_err(term_err)?;
        Ok(Self { _guard: guard })
    }
}

/// Whether to capture the mouse this launch: the persisted `CanopyPrefs.mouse`
/// flag, overridden off by the `RIFT_CANOPY_NO_MOUSE` env kill-switch (mirrors
/// the `RIFT_CANOPY_ASCII` pattern).
fn mouse_enabled(prefs_mouse: bool) -> bool {
    prefs_mouse && std::env::var_os("RIFT_CANOPY_NO_MOUSE").is_none()
}

/// The live event source: pump the terminal and translate to Canopy events.
struct LiveEvents {
    queue: EventQueue,
}

impl LiveEvents {
    fn new() -> Self {
        Self {
            queue: EventQueue::new(),
        }
    }
}

impl EventSource for LiveEvents {
    fn poll(&mut self, timeout: Duration) -> RiftResult<Vec<Event>> {
        self.queue.pump(timeout).map_err(term_err)?;
        let mut out = Vec::new();
        for ev in self.queue.drain() {
            match ev {
                XEvent::Resize(size) => out.push(Event::Resize(Size::new(
                    u16::try_from(size.x).unwrap_or(u16::MAX),
                    u16::try_from(size.y).unwrap_or(u16::MAX),
                ))),
                XEvent::Mouse(m) => {
                    if let Some(mouse) = to_canopy_mouse(&m) {
                        out.push(Event::Mouse(mouse));
                    }
                }
                other => {
                    if let Some(key) = to_canopy(&other) {
                        out.push(Event::Key(key));
                    }
                }
            }
        }
        Ok(out)
    }
}

/// The merged event source: terminal input **plus** the live data bus's updates.
/// Each poll pumps the terminal (blocking up to `timeout`), then drains any bus
/// updates that have arrived, so both flow into the one synchronous loop without
/// converting it to async. Bus latency is at most one poll interval.
struct MergedEvents {
    live: LiveEvents,
    data_rx: Receiver<Event>,
}

impl EventSource for MergedEvents {
    fn poll(&mut self, timeout: Duration) -> RiftResult<Vec<Event>> {
        let mut out = self.live.poll(timeout)?;
        while let Ok(ev) = self.data_rx.try_recv() {
            out.push(ev);
        }
        Ok(out)
    }
}

/// Everything the live loop needs from its host — **the** single injection
/// point (ADR-0011): a new event source or channel becomes a new field here,
/// wired in [`run`], and the loop body never changes.
pub struct LoopDeps<'m, F, R>
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
    R: FnMut() -> RiftResult<Vec<Project>>,
{
    /// Launch options (ascii / skip-splash) from the CLI flags.
    pub opts: CanopyOpts,
    /// The registered projects at launch.
    pub initial: Vec<Project>,
    /// Applies one append-only registry mutation and returns the refreshed list.
    pub on_persist: F,
    /// Live updates from the data bus (P8).
    pub data_rx: Receiver<Event>,
    /// The reducer's data requests, back to the bus.
    pub req_tx: Sender<DataRequest>,
    /// Performs the feature install/remove actions the reducer plans.
    pub manager: &'m mut dyn FeatureManager,
    /// The palette's delegated-verb door (ADR-0021): the binary's in-process
    /// dispatcher, injected exactly like `on_persist`.
    pub dispatcher: &'m mut dyn CommandDispatcher,
    /// The one sanctioned spawn (ADR-0025): opens a project in `$EDITOR`. The
    /// binary provides the argv-exec; the live driver wraps it with the
    /// terminal-guard suspend/resume ([`GuardedLauncher`]).
    pub launcher: &'m mut dyn EditorLauncher,
    /// Reloads the registry after a mutating dispatch (the persist-apply
    /// pattern: selection follows, the subscribe re-fires).
    pub reload: R,
}

/// Wraps the binary's argv-exec [`EditorLauncher`] with the terminal-guard
/// round-trip (ADR-0025): dropping the live [`Terminal`] guard restores raw
/// mode + the alternate screen (suspend); re-entering a fresh guard restores
/// them after the editor exits. Restore-on-panic holds across the round-trip —
/// during the launch the guard is already dropped (terminal cooked), and xre's
/// process-wide panic hook is the backstop otherwise.
struct GuardedLauncher<'a> {
    /// The binary's argv-only launcher.
    inner: &'a mut dyn EditorLauncher,
    /// The live guard, taken (dropped) for the duration of a launch.
    term: Option<Terminal>,
    /// Whether the re-entered guard captures the mouse (matches the initial one).
    mouse: bool,
}

impl EditorLauncher for GuardedLauncher<'_> {
    fn open(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.term = None; // suspend: drop the guard → terminal restored
        let outcome = self.inner.open(path);
        self.term = Terminal::enter(self.mouse).ok(); // resume: re-enter raw + alt
        outcome
    }
}

/// Launch Canopy on the real terminal until the user quits, over the host
/// dependencies in [`LoopDeps`].
///
/// The [`Terminal`] guard restores the terminal on return *and* on panic.
///
/// # Errors
/// [`rift_core::RiftError`] on a terminal enter/read/present failure.
pub fn run<F, R>(deps: LoopDeps<'_, F, R>) -> RiftResult<()>
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
    R: FnMut() -> RiftResult<Vec<Project>>,
{
    let LoopDeps {
        opts,
        initial,
        mut on_persist,
        data_rx,
        req_tx,
        manager,
        dispatcher,
        launcher,
        mut reload,
    } = deps;
    let caps = crate::render::caps::detect(opts.ascii);
    let probe = xre_term::Capabilities::probe();
    let size = Size::new(
        u16::try_from(probe.size.x).unwrap_or(80),
        u16::try_from(probe.size.y).unwrap_or(24),
    );

    let prefs = crate::splash::prefs::load();
    let mouse = mouse_enabled(prefs.mouse);
    // restores on drop AND on panic (xre guard); mouse capture is prefs+env-gated.
    let term = Terminal::enter(mouse)?;
    let mut backend = crate::render::select_backend(caps, opts.ascii);
    let mut state = AppState::new(initial, size);
    state.reduced_motion = prefs.reduced_motion;
    state.version.clone_from(&opts.version);

    // Launch-cwd auto-select (M5-03) BEFORE the loop starts, so the startup
    // prime subscribes the project the developer launched from; `/add`
    // registers exactly this directory (the reducer never reads cwd).
    state.app.launch_cwd.clone_from(&opts.launch_cwd);
    crate::apply_launch_cwd(
        &mut state.app,
        opts.launch_cwd.as_deref(),
        opts.launch_cwd_is_repo,
    );

    // Play the splash on the raw terminal source, so bus updates that arrive
    // meanwhile stay queued (they are drained on the first main-loop poll).
    let mut live = LiveEvents::new();
    if !opts.skip_splash {
        crate::splash::play_live(backend.as_mut(), &mut live, caps, size);
    }
    state.app.splash.skip();

    let mut src = MergedEvents { live, data_rx };
    let mut req_sink = move |req: DataRequest| {
        let _ = req_tx.send(req); // a closed bus is non-fatal; the UI just goes static
    };
    // The live guard moves into the guarded launcher so a $EDITOR launch can
    // suspend (drop) and resume (re-enter) it around the argv-exec (ADR-0025).
    let mut guarded = GuardedLauncher {
        inner: launcher,
        term: Some(term),
        mouse,
    };
    event_loop(
        backend.as_mut(),
        &mut src,
        &mut state,
        &mut on_persist,
        &mut req_sink,
        manager,
        dispatcher,
        &mut guarded,
        &mut reload,
    )
}

/// Translate an xre input event into a terminal-agnostic [`CanopyKey`], or `None`
/// for events the dashboard ignores (mouse, focus, key releases, …).
fn to_canopy(ev: &XEvent) -> Option<CanopyKey> {
    let XEvent::Key(key) = ev else {
        return None;
    };
    if key.state == KeyState::Release {
        return None;
    }
    if key.mods.contains(Modifiers::CTRL) {
        if let KeyCode::Char('c') = key.code {
            return Some(CanopyKey::Quit);
        }
    }
    let shift = key.mods.contains(Modifiers::SHIFT);
    match key.code {
        KeyCode::Char(c) => Some(CanopyKey::Char(c)),
        KeyCode::Enter => Some(CanopyKey::Enter),
        KeyCode::Esc => Some(CanopyKey::Esc),
        KeyCode::Backspace => Some(CanopyKey::Backspace),
        KeyCode::Tab => Some(if shift {
            CanopyKey::BackTab
        } else {
            CanopyKey::Tab
        }),
        KeyCode::BackTab => Some(CanopyKey::BackTab),
        KeyCode::Left => Some(CanopyKey::Left),
        KeyCode::Right => Some(CanopyKey::Right),
        KeyCode::PageUp => Some(CanopyKey::PageUp),
        KeyCode::PageDown => Some(CanopyKey::PageDown),
        // Shift-Arrow reorders (P4 S1); plain arrows navigate/scroll. Reliable
        // Shift reporting needs the keyboard-enhancement protocol, which is off —
        // BackTab still gives a Shift-Tab path regardless.
        KeyCode::Up => Some(if shift {
            CanopyKey::ShiftUp
        } else {
            CanopyKey::Up
        }),
        KeyCode::Down => Some(if shift {
            CanopyKey::ShiftDown
        } else {
            CanopyKey::Down
        }),
        _ => None,
    }
}

/// Translate an xre mouse event into the terminal-agnostic [`MouseInput`] the
/// reducer consumes. Cell coordinates are clamped into `u16` (terminals never
/// approach the ceiling); unknown (`#[non_exhaustive]`) kinds are ignored.
fn to_canopy_mouse(m: &XMouseEvent) -> Option<MouseInput> {
    let kind = match m.kind {
        XMouseKind::Down(b) => MouseKind::Down(mouse_button(b)),
        XMouseKind::Up(b) => MouseKind::Up(mouse_button(b)),
        XMouseKind::Drag(b) => MouseKind::Drag(mouse_button(b)),
        XMouseKind::Moved => MouseKind::Moved,
        XMouseKind::ScrollUp => MouseKind::ScrollUp,
        XMouseKind::ScrollDown => MouseKind::ScrollDown,
        _ => return None,
    };
    Some(MouseInput {
        kind,
        col: u16::try_from(m.col).unwrap_or(u16::MAX),
        row: u16::try_from(m.row).unwrap_or(u16::MAX),
    })
}

/// Map an xre mouse button onto the Canopy-native one.
fn mouse_button(b: XMouseButton) -> MouseButton {
    match b {
        XMouseButton::Left => MouseButton::Left,
        XMouseButton::Right => MouseButton::Right,
        XMouseButton::Middle => MouseButton::Middle,
    }
}

/// Map an xre terminal error onto the shared error type without leaking anything
/// beyond a reason string.
fn term_err(e: TermError) -> rift_core::RiftError {
    match e {
        TermError::Io(io) => rift_core::RiftError::Io(io),
        other => rift_core::RiftError::Other(other.to_string()),
    }
}
