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

mod onboarding;
pub use onboarding::{run_installation, run_onboarding};

use super::{event_loop, CanopyOpts, EventSource};
use crate::dispatch::CommandDispatcher;
use crate::render::frame::Size;
use crate::state::{AppState, DataRequest, Event, PersistOp};
use crate::{CanopyKey, MouseButton, MouseInput, MouseKind, Project, ProjectSettingsManager};

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

/// Enter the terminal, retrying **without** the mouse when capture is
/// refused: a terminal that cannot report the mouse is still a terminal, and
/// losing the whole dashboard to a one-frame print over a refused mouse
/// enable is the wrong trade. Returns the guard and whether the retry was
/// taken. The first attempt has already switched raw mode and the alternate
/// screen on before the mouse enable fails; the retry re-issues both, which is
/// harmless (raw mode is idempotent and the engine keeps the original screen).
fn enter_with_fallback<T>(
    mouse: bool,
    mut enter: impl FnMut(bool) -> RiftResult<T>,
) -> RiftResult<(T, bool)> {
    if !mouse {
        return enter(false).map(|t| (t, false));
    }
    match enter(true) {
        Ok(t) => Ok((t, false)),
        Err(_) => enter(false).map(|t| (t, true)),
    }
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
/// point (the synchronous event loop): a new event source or channel becomes a new field here,
/// wired in [`run`], and the loop body never changes.
pub struct LoopDeps<'m, F, R, L>
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
    R: FnMut() -> RiftResult<Vec<Project>>,
    L: FnMut(&std::path::Path) -> crate::explorer::DirListing,
{
    /// Launch options (ascii / skip-splash) from the CLI flags.
    pub opts: CanopyOpts,
    /// The registered projects at launch (the active profile's view).
    pub initial: Vec<Project>,
    /// Applies one append-only registry mutation and returns the refreshed list.
    pub on_persist: F,
    /// Live updates from the data bus.
    pub data_rx: Receiver<Event>,
    /// The reducer's data requests, back to the bus.
    pub req_tx: Sender<DataRequest>,
    /// Performs repository Project Settings lifecycle operations.
    pub settings_manager: &'m mut dyn ProjectSettingsManager,
    /// The palette's delegated-verb door (the injected dispatcher): the binary's in-process
    /// dispatcher, injected exactly like `on_persist`.
    pub dispatcher: &'m mut dyn CommandDispatcher,
    /// Reloads the registry after a mutating dispatch (the persist-apply
    /// pattern: selection follows, the subscribe re-fires).
    pub reload: R,
    /// The profile loader seam (the profile plane, generalising the layout
    /// loader): the active profile, live names, membership, and (profile,
    /// project) arrangements; the binary wires it to the canopy store.
    pub profile_loader: &'m mut dyn crate::profile::ProfileLoader,
    /// The destination explorer's lister seam: one directory's
    /// immediate children, infallibly. The binary wires it to the data plane's
    /// real walk; tests inject scripted listings.
    pub list_dir: L,
}

/// Launch Canopy on the real terminal until the user quits, over the host
/// dependencies in [`LoopDeps`].
///
/// The [`Terminal`] guard restores the terminal on return *and* on panic.
///
/// # Errors
/// [`rift_core::RiftError`] on a terminal enter/read/present failure.
pub fn run<F, R, L>(deps: LoopDeps<'_, F, R, L>) -> RiftResult<()>
where
    F: FnMut(&PersistOp) -> RiftResult<Vec<Project>>,
    R: FnMut() -> RiftResult<Vec<Project>>,
    L: FnMut(&std::path::Path) -> crate::explorer::DirListing,
{
    let LoopDeps {
        opts,
        initial,
        mut on_persist,
        data_rx,
        req_tx,
        settings_manager,
        dispatcher,
        mut reload,
        profile_loader,
        mut list_dir,
    } = deps;
    let caps = crate::render::caps::detect(opts.ascii);
    let size = probe_size();

    let prefs = crate::splash::prefs::load();
    let mouse = mouse_enabled(prefs.mouse);
    // restores on drop AND on panic (xre guard); mouse capture is prefs+env-gated,
    // and a terminal that refuses capture gets the keyboard-only dashboard rather
    // than no dashboard. Held for the whole loop; dropped on return.
    let (_term, capture_refused) = enter_with_fallback(mouse, Terminal::enter)?;
    let mut backend = crate::render::select_backend(caps, opts.ascii);
    let mut state = startup_state(&opts, initial, size, &prefs, capture_refused);
    open_viewer_launch(&mut state, &opts, &req_tx);

    // Branding belongs to project onboarding; established projects open directly.
    let live = LiveEvents::new();
    state.app.splash.skip();

    let mut src = MergedEvents { live, data_rx };
    let mut req_sink = move |req: DataRequest| {
        let _ = req_tx.send(req); // a closed bus is non-fatal; the UI just goes static
    };
    event_loop(
        backend.as_mut(),
        &mut src,
        &mut state,
        &mut on_persist,
        &mut req_sink,
        settings_manager,
        dispatcher,
        &mut reload,
        profile_loader,
        &mut list_dir,
    )
}

/// The terminal's size in cells, 80x24 when the probe reports one that does
/// not fit.
fn probe_size() -> Size {
    let probe = xre_term::Capabilities::probe();
    Size::new(
        u16::try_from(probe.size.x).unwrap_or(80),
        u16::try_from(probe.size.y).unwrap_or(24),
    )
}

/// The dashboard state before the first frame: the refused-capture notice,
/// the motion preference, the version, the startup sweep and the launch
/// directory — everything the launch decides, and nothing the terminal does.
fn startup_state(
    opts: &CanopyOpts,
    initial: Vec<Project>,
    size: Size,
    prefs: &crate::splash::prefs::CanopyPrefs,
    capture_refused: bool,
) -> AppState {
    let mut state = AppState::new(initial, size);
    if capture_refused {
        state.open_notice(crate::dispatch::Message::info(
            "mouse",
            vec![
                "This terminal refused mouse capture; keyboard only.".to_owned(),
                "Every button has a key — the hint bar teaches them.".to_owned(),
            ],
        ));
    }
    state.reduced_motion =
        prefs.reduced_motion || opts.ascii || std::env::var_os("NO_COLOR").is_some();
    state.version.clone_from(&opts.version);
    // The one-shot startup sweep as the panels first fill after the splash;
    // reduced motion / ASCII show the final frame instantly.

    // Launch-cwd auto-select BEFORE the loop starts, so the startup
    // prime subscribes the project the developer launched from; `/add`
    // registers exactly this directory (the reducer never reads cwd).
    state.app.launch_cwd.clone_from(&opts.launch_cwd);
    // The destination explorer's start directory: home, resolved
    // by the binary's env ladder, falling back to the launch cwd then `/`.
    state.app.home_dir.clone_from(&opts.home_dir);
    crate::apply_launch_cwd(
        &mut state.app,
        opts.launch_cwd.as_deref(),
        opts.launch_cwd_is_repo,
    );
    // The CLI completes launch onboarding before creating this dashboard.
    state.app.set_focus(crate::Panel::Detail);
    state
}

/// A direct `rift viewer` launch enters through the same reducer action as
/// the Repository button and slash command, but before the first frame. Its
/// host injects the dedicated read-only bus; no dashboard prime request that
/// follows can make that bus run feeds, migrations, or ingest.
fn open_viewer_launch(state: &mut AppState, opts: &CanopyOpts, req_tx: &Sender<DataRequest>) {
    let Some(launch) = &opts.viewer_launch else {
        return;
    };
    state.app.select_path(&launch.repo);
    let effects = state.open_recall_viewer(launch.target.clone());
    for request in effects.data_requests {
        let _ = req_tx.send(request);
    }
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
        KeyCode::Backspace | KeyCode::Delete => Some(CanopyKey::Backspace),
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
        KeyCode::Home => Some(CanopyKey::Home),
        KeyCode::End => Some(CanopyKey::End),
        // Shift-Arrow reorders; plain arrows navigate/scroll. Reliable
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

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, mods: Modifiers) -> XEvent {
        XEvent::Key(xre_term::Key {
            code,
            mods,
            state: KeyState::Press,
        })
    }

    /// The terminal→Canopy key contract, one row per key the dashboard acts
    /// on. This is the only translation between the engine's key vocabulary
    /// and `CanopyKey`, so a silently dropped or re-pointed key here is a key
    /// that stops working with every keymap test still green.
    #[test]
    fn every_key_canopy_acts_on_arrives_as_itself() {
        let none = Modifiers::NONE;
        for (code, expected) in [
            (KeyCode::Char('q'), CanopyKey::Char('q')),
            (KeyCode::Char('/'), CanopyKey::Char('/')),
            (KeyCode::Char('?'), CanopyKey::Char('?')),
            (KeyCode::Enter, CanopyKey::Enter),
            (KeyCode::Esc, CanopyKey::Esc),
            (KeyCode::Backspace, CanopyKey::Backspace),
            (KeyCode::Tab, CanopyKey::Tab),
            (KeyCode::BackTab, CanopyKey::BackTab),
            (KeyCode::Left, CanopyKey::Left),
            (KeyCode::Right, CanopyKey::Right),
            (KeyCode::PageUp, CanopyKey::PageUp),
            (KeyCode::PageDown, CanopyKey::PageDown),
            (KeyCode::Home, CanopyKey::Home),
            (KeyCode::End, CanopyKey::End),
            (KeyCode::Up, CanopyKey::Up),
            (KeyCode::Down, CanopyKey::Down),
        ] {
            assert_eq!(
                to_canopy(&press(code, none)),
                Some(expected),
                "{code:?} lost its Canopy key"
            );
        }
    }

    /// Shift re-points exactly three keys — Tab back-tabs, Up/Down reorder —
    /// and leaves every other key alone. `BackTab` is the path that works
    /// without the keyboard-enhancement protocol, so it must not depend on
    /// the Shift flag being reported at all.
    #[test]
    fn shift_repoints_only_the_three_keys_that_reorder() {
        let shift = Modifiers::SHIFT;
        assert_eq!(
            to_canopy(&press(KeyCode::Tab, shift)),
            Some(CanopyKey::BackTab)
        );
        assert_eq!(
            to_canopy(&press(KeyCode::Up, shift)),
            Some(CanopyKey::ShiftUp)
        );
        assert_eq!(
            to_canopy(&press(KeyCode::Down, shift)),
            Some(CanopyKey::ShiftDown)
        );
        assert_eq!(
            to_canopy(&press(KeyCode::BackTab, Modifiers::NONE)),
            Some(CanopyKey::BackTab),
            "back-tab does not need a reported Shift"
        );
        for (code, expected) in [
            (KeyCode::Enter, CanopyKey::Enter),
            (KeyCode::Left, CanopyKey::Left),
            (KeyCode::Right, CanopyKey::Right),
            (KeyCode::Home, CanopyKey::Home),
            // `?` is a shifted glyph on most layouts; it must arrive as itself
            // whether or not the terminal reports the Shift that made it.
            (KeyCode::Char('?'), CanopyKey::Char('?')),
        ] {
            assert_eq!(to_canopy(&press(code, shift)), Some(expected), "{code:?}");
        }
    }

    /// Ctrl-C quits from any key it is held with, and is the only Ctrl
    /// binding this layer claims — every other Ctrl chord falls through to
    /// the plain key so the keymap, not the terminal layer, decides.
    #[test]
    fn ctrl_c_quits_and_no_other_ctrl_chord_is_claimed_here() {
        let ctrl = Modifiers::CTRL;
        assert_eq!(
            to_canopy(&press(KeyCode::Char('c'), ctrl)),
            Some(CanopyKey::Quit)
        );
        assert_eq!(
            to_canopy(&press(KeyCode::Char('c'), ctrl.with(Modifiers::SHIFT))),
            Some(CanopyKey::Quit),
            "an extra modifier does not disarm the quit"
        );
        assert_eq!(
            to_canopy(&press(KeyCode::Char('d'), ctrl)),
            Some(CanopyKey::Char('d')),
            "Ctrl-D is not this layer's to claim"
        );
        assert_eq!(
            to_canopy(&press(KeyCode::Char('c'), Modifiers::NONE)),
            Some(CanopyKey::Char('c')),
            "a bare c is a character, not a quit"
        );
    }

    /// A release is not an action: with the enhancement protocol off every
    /// event is a press, but if a terminal does report releases the dashboard
    /// must not act on each key twice — including Ctrl-C.
    #[test]
    fn a_released_key_is_never_an_action() {
        for (code, mods) in [
            (KeyCode::Char('c'), Modifiers::CTRL),
            (KeyCode::Enter, Modifiers::NONE),
            (KeyCode::Up, Modifiers::SHIFT),
        ] {
            let release = XEvent::Key(xre_term::Key {
                code,
                mods,
                state: KeyState::Release,
            });
            assert_eq!(to_canopy(&release), None, "{code:?} acted on release");
        }
        let repeat = XEvent::Key(xre_term::Key {
            code: KeyCode::Down,
            mods: Modifiers::NONE,
            state: KeyState::Repeat,
        });
        assert_eq!(
            to_canopy(&repeat),
            Some(CanopyKey::Down),
            "auto-repeat still scrolls"
        );
    }

    /// Keys with no Canopy meaning, and non-key events, are dropped rather
    /// than mapped onto something arbitrary.
    #[test]
    fn keys_and_events_with_no_canopy_meaning_are_dropped() {
        for code in [KeyCode::Insert, KeyCode::F(1), KeyCode::F(12)] {
            assert_eq!(to_canopy(&press(code, Modifiers::NONE)), None, "{code:?}");
        }
        assert_eq!(
            to_canopy(&press(KeyCode::Delete, Modifiers::NONE)),
            Some(CanopyKey::Backspace)
        );
        assert_eq!(to_canopy(&XEvent::FocusGained), None);
        assert_eq!(to_canopy(&XEvent::Paste("pasted".to_owned())), None);
    }

    /// Every mouse kind Canopy hit-tests on survives translation, with the
    /// button carried through, and the cell position is preserved.
    #[test]
    fn every_mouse_kind_canopy_hit_tests_on_survives_translation() {
        let cases = [
            (
                XMouseKind::Down(XMouseButton::Left),
                MouseKind::Down(MouseButton::Left),
            ),
            (
                XMouseKind::Down(XMouseButton::Right),
                MouseKind::Down(MouseButton::Right),
            ),
            (
                XMouseKind::Down(XMouseButton::Middle),
                MouseKind::Down(MouseButton::Middle),
            ),
            (
                XMouseKind::Up(XMouseButton::Left),
                MouseKind::Up(MouseButton::Left),
            ),
            (
                XMouseKind::Drag(XMouseButton::Left),
                MouseKind::Drag(MouseButton::Left),
            ),
            (XMouseKind::Moved, MouseKind::Moved),
            (XMouseKind::ScrollUp, MouseKind::ScrollUp),
            (XMouseKind::ScrollDown, MouseKind::ScrollDown),
        ];
        for (kind, expected) in cases {
            let ev = XMouseEvent {
                kind,
                col: 12,
                row: 34,
                mods: Modifiers::NONE,
            };
            let out = to_canopy_mouse(&ev).expect("a hit-testable mouse kind");
            assert_eq!(out.kind, expected, "{kind:?}");
            assert_eq!((out.col, out.row), (12, 34), "{kind:?} moved the cell");
        }
    }

    /// A terminal that reports a cell beyond `u16` saturates instead of
    /// wrapping — a wrap would land the click on the opposite side of the
    /// grid and fire whatever target sits there.
    #[test]
    fn an_out_of_range_cell_saturates_rather_than_wrapping() {
        let ev = XMouseEvent {
            kind: XMouseKind::Down(XMouseButton::Left),
            col: u32::from(u16::MAX) + 1,
            row: u32::MAX,
            mods: Modifiers::NONE,
        };
        let out = to_canopy_mouse(&ev).expect("still a click");
        assert_eq!((out.col, out.row), (u16::MAX, u16::MAX));
    }

    /// Mouse capture is a preference AND an env kill-switch: off by
    /// preference is off regardless, and the switch can only ever subtract.
    #[test]
    fn mouse_capture_is_the_preference_narrowed_by_the_kill_switch() {
        assert!(
            !mouse_enabled(false),
            "the preference alone can turn capture off"
        );
        assert_eq!(
            mouse_enabled(true),
            std::env::var_os("RIFT_CANOPY_NO_MOUSE").is_none(),
            "with the preference on, only the kill-switch decides"
        );
    }

    /// A refused capture retries without the mouse and says so; a refusal on
    /// both attempts is the real error; and with the mouse off it is never
    /// even asked for.
    #[test]
    fn a_refused_capture_retries_without_the_mouse() {
        let mut asked = Vec::new();
        let out = enter_with_fallback(true, |mouse| {
            asked.push(mouse);
            if mouse {
                Err(rift_core::RiftError::Other("no mouse here".into()))
            } else {
                Ok("guard")
            }
        })
        .unwrap();
        assert_eq!(out, ("guard", true));
        assert_eq!(asked, vec![true, false], "tried with, then without");

        let both = enter_with_fallback(true, |_| -> RiftResult<()> {
            Err(rift_core::RiftError::Other("refused".into()))
        });
        assert!(both.is_err(), "refused twice is a real failure");

        let mut asked = Vec::new();
        let out = enter_with_fallback(false, |mouse| {
            asked.push(mouse);
            Ok::<_, rift_core::RiftError>("guard")
        })
        .unwrap();
        assert_eq!(out, ("guard", false));
        assert_eq!(asked, vec![false], "mouse off never asks for it");
    }

    /// A terminal I/O failure keeps its `io::ErrorKind` rather than being
    /// flattened to a string — the caller distinguishes an interrupted read
    /// from a real failure.
    #[test]
    fn a_terminal_io_failure_stays_an_io_error() {
        let err = term_err(TermError::Io(std::io::Error::new(
            std::io::ErrorKind::BrokenPipe,
            "pipe",
        )));
        match err {
            rift_core::RiftError::Io(io) => {
                assert_eq!(io.kind(), std::io::ErrorKind::BrokenPipe);
            }
            other => panic!("terminal I/O flattened to {other:?}"),
        }
    }
}
