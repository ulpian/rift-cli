//! The splash director: timing, skip, fidelity-tier selection, first-run gating.
//!
//! Stitches the [`ground`](super::ground) crack and [`wordmark`](super::wordmark)
//! into one timed, **skippable** sequence with a bounded (clamped) duration, picks
//! a [`Tier`] from [`Capabilities`] + reduced-motion, and persists `seen_splash`
//! as a superseding [`prefs`](super::prefs) write so the open plays first-run-only
//! by default. All timing is via an injected [`Clock`], so tests fast-forward
//! without real sleeps and the skip always lands on the deterministic final frame.

use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::app::{render_err, EventSource};
use crate::render::frame::Size;
use crate::render::{Capabilities, RenderBackend};
use crate::state::Event;

use super::{prefs, render2d};

/// An injected monotonic time source. `tick` advances it (live: sleep; test:
/// bump) so the director loop is deterministic and sleep-free under test.
pub trait Clock {
    /// Elapsed time since the clock started.
    fn now(&self) -> Duration;
    /// Advance by one frame (live: sleep `frame`; test: add `frame` to `now`).
    fn tick(&self, frame: Duration);
}

/// The live wall-clock (monotonic `Instant`). Used only on the real terminal —
/// never in tests, which use [`ManualClock`].
pub struct RealClock {
    start: Instant,
}

impl RealClock {
    /// Start a clock at "now".
    #[must_use]
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for RealClock {
    fn now(&self) -> Duration {
        Instant::now().saturating_duration_since(self.start)
    }
    fn tick(&self, frame: Duration) {
        std::thread::sleep(frame);
    }
}

/// A test clock advanced manually (no wall-clock dependence).
#[derive(Debug, Default)]
pub struct ManualClock {
    now: Cell<Duration>,
}

impl ManualClock {
    /// A manual clock at `t = 0`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            now: Cell::new(Duration::ZERO),
        }
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        self.now.get()
    }
    fn tick(&self, frame: Duration) {
        self.now.set(self.now.get().saturating_add(frame));
    }
}

/// When the splash plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SplashMode {
    /// Play only on the first run (default), then never again.
    FirstRunOnly,
    /// Play on every launch.
    Always,
    /// Never play.
    Never,
}

impl SplashMode {
    /// Parse the config label (`"always"`/`"never"`/anything → first-run).
    #[must_use]
    pub fn from_label(s: &str) -> Self {
        match s {
            "always" => Self::Always,
            "never" => Self::Never,
            _ => Self::FirstRunOnly,
        }
    }

    /// The config label for this mode.
    #[must_use]
    pub fn as_label(self) -> &'static str {
        match self {
            Self::FirstRunOnly => "first-run",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

/// The chosen fidelity tier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier {
    /// Full 3D voxel crack + mesh wordmark (only when built with `xre-3d`).
    Full3d,
    /// 2D Unicode crack + color-ramp wordmark.
    Reduced2d,
    /// Static figlet banner, no animation.
    StaticAscii,
    /// Skip entirely.
    Instant,
}

/// The splash configuration.
#[derive(Clone, Copy, Debug)]
pub struct SplashConfig {
    /// When the splash plays.
    pub mode: SplashMode,
    /// Reduced-motion → static tier, no animation.
    pub reduced_motion: bool,
    /// A hard cap on the whole sequence (clamped at run time).
    pub total: Duration,
    /// The crack seed (reproducible demos/tests).
    pub seed: u64,
}

impl Default for SplashConfig {
    fn default() -> Self {
        Self {
            mode: SplashMode::FirstRunOnly,
            reduced_motion: false,
            total: Duration::from_millis(2200),
            seed: 0x5249_4654_5249_4654, // "RIFTRIFT"
        }
    }
}

/// The hard ceiling on splash duration (a hostile config can't DoS startup).
const MAX_TOTAL: Duration = Duration::from_secs(5);
/// The per-frame step.
const FRAME_DT: Duration = Duration::from_millis(33);

/// Clamp the configured total to a sane `[0, MAX_TOTAL]` range.
#[must_use]
pub fn clamp_total(total: Duration) -> Duration {
    total.min(MAX_TOTAL)
}

/// Whether the splash should play, given the mode and the persisted `seen` flag.
#[must_use]
pub fn should_play(mode: SplashMode, seen: bool) -> bool {
    match mode {
        SplashMode::Never => false,
        SplashMode::Always => true,
        SplashMode::FirstRunOnly => !seen,
    }
}

/// Pick the fidelity tier from capabilities + reduced-motion. Reduced-motion and
/// legacy-Windows never select a 3D/animated tier.
#[must_use]
pub fn choose_tier(caps: Capabilities, reduced_motion: bool) -> Tier {
    if reduced_motion {
        return Tier::StaticAscii;
    }
    let three_d = Capabilities::MESH_3D | Capabilities::RAMP_SHADER | Capabilities::TRUECOLOR;
    let two_d = Capabilities::TRUECOLOR | Capabilities::UNICODE;
    let legacy = caps.contains(Capabilities::LEGACY_WIN);
    if caps.contains(three_d) && !legacy {
        Tier::Full3d
    } else if caps.contains(two_d) && !legacy {
        Tier::Reduced2d
    } else if caps.contains(Capabilities::TRUECOLOR) {
        Tier::StaticAscii
    } else {
        Tier::Instant
    }
}

/// Run the splash to completion or a skip. Any key press ends it immediately and
/// the **deterministic final frame** is drawn (skip parity); the total never
/// exceeds the clamped cap. Pure-ish: all I/O is the backend present and the key
/// poll; all timing flows through `clock`.
///
/// # Errors
/// [`rift_core::RiftError`] on a backend present failure.
pub fn run_splash(
    backend: &mut dyn RenderBackend,
    src: &mut dyn EventSource,
    clock: &dyn Clock,
    cfg: &SplashConfig,
    caps: Capabilities,
    size: Size,
) -> rift_core::Result<()> {
    let tier = choose_tier(caps, cfg.reduced_motion);
    if tier == Tier::Instant {
        return Ok(());
    }
    let total = clamp_total(cfg.total);
    let start = clock.now();
    loop {
        // Skip on ANY key press (non-blocking poll).
        let events = src.poll(Duration::ZERO)?;
        if events.iter().any(|e| matches!(e, Event::Key(_))) {
            break;
        }
        let elapsed = clock.now().saturating_sub(start);
        let t = if total.is_zero() {
            1.0
        } else {
            (elapsed.as_secs_f32() / total.as_secs_f32()).clamp(0.0, 1.0)
        };
        let mut frame = backend.begin_frame(size);
        render2d::draw_tier(tier, t, cfg.seed, backend, &mut frame, caps);
        backend.present(&frame).map_err(render_err)?;
        if t >= 1.0 {
            break;
        }
        clock.tick(FRAME_DT);
    }
    // The deterministic end pose, so a skip never leaves a half-drawn glitch.
    let mut frame = backend.begin_frame(size);
    render2d::draw_tier(tier, 1.0, cfg.seed, backend, &mut frame, caps);
    backend.present(&frame).map_err(render_err)?;
    Ok(())
}

/// Mark the splash seen (only for `FirstRunOnly`) via the env-resolved prefs.
fn mark_seen_with(env: impl Fn(&str) -> Option<String>, mode: SplashMode) {
    if mode == SplashMode::FirstRunOnly {
        let mut p = prefs::load_from(&env);
        p.seen_splash = true;
        let _ = prefs::save_from(&env, &p); // best-effort; a UI pref is non-fatal
    }
}

/// The live entry: load prefs, gate on first-run, play, then mark seen. Splash
/// errors are non-fatal — the dashboard always follows.
pub fn play_live(
    backend: &mut dyn RenderBackend,
    src: &mut dyn EventSource,
    caps: Capabilities,
    size: Size,
) {
    let env = |k: &str| std::env::var(k).ok();
    let p = prefs::load_from(env);
    let mode = SplashMode::from_label(&p.splash_mode);
    if !should_play(mode, p.seen_splash) {
        return;
    }
    let cfg = SplashConfig {
        mode,
        reduced_motion: p.reduced_motion,
        ..SplashConfig::default()
    };
    let clock = RealClock::new();
    let _ = run_splash(backend, src, &clock, &cfg, caps, size);
    mark_seen_with(env, mode);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Frame;
    use crate::render::RenderError;
    use crate::CanopyKey;

    /// A backend that records the last presented frame's text.
    struct Recorder {
        last: Option<String>,
        presents: usize,
    }
    impl RenderBackend for Recorder {
        fn name(&self) -> &'static str {
            "recorder"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::empty()
        }
        fn present(&mut self, frame: &Frame) -> Result<(), RenderError> {
            let mut s = String::new();
            for y in 0..frame.height() {
                for x in 0..frame.width() {
                    s.push(frame.get(x, y).map_or(' ', |c| c.ch));
                }
                s.push('\n');
            }
            self.last = Some(s);
            self.presents += 1;
            Ok(())
        }
    }

    /// An event source that yields `script` once, then empties.
    struct Script {
        steps: std::collections::VecDeque<Vec<Event>>,
    }
    impl Script {
        fn new(steps: Vec<Vec<Event>>) -> Self {
            Self {
                steps: steps.into(),
            }
        }
    }
    impl EventSource for Script {
        fn poll(&mut self, _t: Duration) -> rift_core::Result<Vec<Event>> {
            Ok(self.steps.pop_front().unwrap_or_default())
        }
    }

    fn truecolor_unicode() -> Capabilities {
        Capabilities::TRUECOLOR | Capabilities::UNICODE
    }

    #[test]
    fn choose_tier_table() {
        let full = Capabilities::MESH_3D
            | Capabilities::RAMP_SHADER
            | Capabilities::TRUECOLOR
            | Capabilities::UNICODE;
        assert_eq!(choose_tier(full, false), Tier::Full3d);
        assert_eq!(choose_tier(full, true), Tier::StaticAscii); // reduced-motion wins
        assert_eq!(choose_tier(truecolor_unicode(), false), Tier::Reduced2d);
        assert_eq!(
            choose_tier(truecolor_unicode() | Capabilities::LEGACY_WIN, false),
            Tier::StaticAscii,
            "legacy windows never gets an animated tier"
        );
        assert_eq!(
            choose_tier(Capabilities::TRUECOLOR, false),
            Tier::StaticAscii
        );
        assert_eq!(choose_tier(Capabilities::empty(), false), Tier::Instant);
    }

    #[test]
    fn legacy_windows_never_3d() {
        let caps = Capabilities::MESH_3D
            | Capabilities::RAMP_SHADER
            | Capabilities::TRUECOLOR
            | Capabilities::LEGACY_WIN;
        // Full3d requires the 3D bits but legacy must not get the animated path;
        // it still has truecolor, so it lands on StaticAscii — never Full3d/Reduced2d.
        let tier = choose_tier(caps, false);
        assert_ne!(tier, Tier::Full3d);
        assert_ne!(tier, Tier::Reduced2d);
    }

    #[test]
    fn should_play_logic() {
        assert!(should_play(SplashMode::Always, true));
        assert!(!should_play(SplashMode::Never, false));
        assert!(should_play(SplashMode::FirstRunOnly, false));
        assert!(!should_play(SplashMode::FirstRunOnly, true));
    }

    #[test]
    fn total_is_clamped() {
        assert_eq!(clamp_total(MAX_TOTAL * 100), MAX_TOTAL); // a 10-minute config is bounded
        assert_eq!(clamp_total(Duration::from_secs(2)), Duration::from_secs(2));
    }

    #[test]
    fn run_splash_terminates_and_draws_final_frame() {
        let mut b = Recorder {
            last: None,
            presents: 0,
        };
        let mut src = Script::new(vec![]); // no input → plays to the cap
        let clock = ManualClock::new();
        let cfg = SplashConfig::default();
        run_splash(
            &mut b,
            &mut src,
            &clock,
            &cfg,
            truecolor_unicode(),
            Size::new(60, 20),
        )
        .unwrap();
        assert!(b.presents >= 1);
        // The manual clock never advanced past the (clamped) cap.
        assert!(clock.now() <= clamp_total(cfg.total) + FRAME_DT);
        assert!(b.last.unwrap().contains('_')); // final frame shows the wordmark
    }

    #[test]
    fn skip_on_any_keypress_lands_on_final_frame() {
        let cfg = SplashConfig::default();
        let caps = truecolor_unicode();
        let size = Size::new(60, 20);

        // Natural completion.
        let mut nat = Recorder {
            last: None,
            presents: 0,
        };
        let clock_n = ManualClock::new();
        run_splash(
            &mut nat,
            &mut Script::new(vec![]),
            &clock_n,
            &cfg,
            caps,
            size,
        )
        .unwrap();

        // Skip on the very first poll.
        let mut skip = Recorder {
            last: None,
            presents: 0,
        };
        let clock_s = ManualClock::new();
        let mut src = Script::new(vec![vec![Event::Key(CanopyKey::Char('x'))]]);
        run_splash(&mut skip, &mut src, &clock_s, &cfg, caps, size).unwrap();

        assert_eq!(
            nat.last, skip.last,
            "skip must land on the same deterministic final frame"
        );
    }

    #[test]
    fn instant_tier_presents_nothing() {
        let mut b = Recorder {
            last: None,
            presents: 0,
        };
        let clock = ManualClock::new();
        run_splash(
            &mut b,
            &mut Script::new(vec![]),
            &clock,
            &SplashConfig::default(),
            Capabilities::empty(), // → Instant
            Size::new(40, 10),
        )
        .unwrap();
        assert_eq!(b.presents, 0);
    }

    #[test]
    fn first_run_marks_seen_then_skips() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_string_lossy().into_owned();
        let env = move |k: &str| (k == "RIFT_CONFIG_DIR").then(|| dir.clone());

        // Fresh: first-run plays.
        assert!(should_play(
            SplashMode::FirstRunOnly,
            prefs::load_from(&env).seen_splash
        ));
        mark_seen_with(&env, SplashMode::FirstRunOnly);
        // Now seen → does not play again.
        assert!(prefs::load_from(&env).seen_splash);
        assert!(!should_play(
            SplashMode::FirstRunOnly,
            prefs::load_from(&env).seen_splash
        ));
    }

    #[test]
    fn mode_label_round_trips() {
        for m in [
            SplashMode::FirstRunOnly,
            SplashMode::Always,
            SplashMode::Never,
        ] {
            assert_eq!(SplashMode::from_label(m.as_label()), m);
        }
        assert_eq!(SplashMode::from_label("garbage"), SplashMode::FirstRunOnly);
    }
}
