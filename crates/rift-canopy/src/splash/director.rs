//! The splash director: timing, skip, fidelity-tier selection, first-run gating,
//! and the Full3d frame-budget ladder.
//!
//! Stitches the [`ground`](super::ground) crack and [`wordmark`](super::wordmark)
//! into one timed, **skippable** sequence with a bounded (clamped) duration, picks
//! a [`Tier`] from [`Capabilities`] + reduced-motion, and persists `seen_splash`
//! as a superseding [`prefs`](super::prefs) write so the open plays first-run-only
//! by default. All timing is via an injected [`Clock`], so tests fast-forward
//! without real sleeps and the skip always lands on the deterministic final frame.
//!
//! ADR-0014's honesty half lives here too: a runtime probe measures each
//! Full3d frame against [`FRAME_BUDGET`] and the [`BudgetLadder`] degrades —
//! **resolution first, then the tier drops to Reduced2d** — so the delight
//! tier can never cost startup responsiveness (and never flaps back up, so
//! there is no stutter).

use std::cell::Cell;
use std::time::{Duration, Instant};

use crate::app::{render_err, EventSource};
use crate::render::frame::Size;
use crate::render::{Capabilities, RenderBackend};
use crate::state::Event;

use super::mesh3d::Resolution;
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
    /// The mesh raster (ADR-0014): crack-field ground + extruded wordmark,
    /// density-resolved. Live terminals only reach it on an `xre-3d` build
    /// (the capability lattice never advertises `MESH_3D` without it).
    Full3d,
    /// 2D Unicode crack + color-ramp wordmark.
    Reduced2d,
    /// Static figlet banner, no animation.
    StaticAscii,
    /// Skip entirely.
    Instant,
}

/// ADR-0014: the per-frame render budget the Full3d tier must hold. The
/// criterion benchmark (`benches/splash_frame.rs`) documents it; the
/// [`BudgetLadder`] enforces it live inside [`run_splash`].
pub const FRAME_BUDGET: Duration = Duration::from_millis(8);

/// One honesty rung taken by the [`BudgetLadder`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Downgrade {
    /// The Full3d raster density halved (braille → half-block sampling).
    ResolutionHalved,
    /// The tier dropped to the shipped [`Tier::Reduced2d`] floor.
    TierDropped,
}

/// The budget-or-degrade ladder (ADR-0014): fed each frame's measured cost,
/// it downgrades **resolution first, then the tier** — and never upgrades, so
/// a borderline machine settles instead of stuttering. Only [`Tier::Full3d`]
/// is subject to the budget; the lower tiers are the already-shipped floor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BudgetLadder {
    tier: Tier,
    res: Resolution,
    budget: Duration,
}

impl BudgetLadder {
    /// A ladder for the chosen tier at the ADR-0014 budget.
    #[must_use]
    pub fn new(tier: Tier) -> Self {
        Self::with_budget(tier, FRAME_BUDGET)
    }

    /// A ladder with an explicit budget (tests pin the threshold).
    #[must_use]
    pub fn with_budget(tier: Tier, budget: Duration) -> Self {
        Self {
            tier,
            res: Resolution::Full,
            budget,
        }
    }

    /// The tier frames should currently be drawn at.
    #[must_use]
    pub fn tier(&self) -> Tier {
        self.tier
    }

    /// The Full3d raster density frames should currently be drawn at.
    #[must_use]
    pub fn resolution(&self) -> Resolution {
        self.res
    }

    /// Record one frame's measured cost. Over budget on the Full3d tier the
    /// ladder takes exactly one rung — resolution, then tier — and reports
    /// it; under budget (or already on the floor) it is a no-op.
    pub fn record(&mut self, frame_cost: Duration) -> Option<Downgrade> {
        if self.tier != Tier::Full3d || frame_cost <= self.budget {
            return None;
        }
        if self.res == Resolution::Full {
            self.res = Resolution::Half;
            Some(Downgrade::ResolutionHalved)
        } else {
            self.tier = Tier::Reduced2d;
            Some(Downgrade::TierDropped)
        }
    }
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
/// poll; all timing flows through `clock` — including the ADR-0014 frame probe,
/// which measures each frame through the injected clock and feeds the
/// [`BudgetLadder`] (resolution first, then the tier drops to Reduced2d).
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
    let mut ladder = BudgetLadder::new(tier);
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
        // The frame probe: measure the draw + present through the injected
        // clock and let the ladder degrade before the next frame — the
        // downgrade applies forward, so no frame is ever drawn twice.
        let before = clock.now();
        let mut frame = backend.begin_frame(size);
        render2d::draw_tier(
            ladder.tier(),
            t,
            cfg.seed,
            backend,
            &mut frame,
            caps,
            ladder.resolution(),
        );
        backend.present(&frame).map_err(render_err)?;
        let cost = clock.now().saturating_sub(before);
        let _ = ladder.record(cost);
        if t >= 1.0 {
            break;
        }
        clock.tick(FRAME_DT);
    }
    // The deterministic end pose (at the ladder's settled tier/resolution),
    // so a skip never leaves a half-drawn glitch.
    let mut frame = backend.begin_frame(size);
    render2d::draw_tier(
        ladder.tier(),
        1.0,
        cfg.seed,
        backend,
        &mut frame,
        caps,
        ladder.resolution(),
    );
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

    // --- the Full3d tier + the ADR-0014 budget ladder -----------------------

    /// A clock whose every `now()` read advances by `step` — the injected
    /// "slow machine": each probed frame appears to cost `step`.
    struct SlowClock {
        now: Cell<Duration>,
        step: Duration,
    }
    impl SlowClock {
        fn new(step: Duration) -> Self {
            Self {
                now: Cell::new(Duration::ZERO),
                step,
            }
        }
    }
    impl Clock for SlowClock {
        fn now(&self) -> Duration {
            let v = self.now.get();
            self.now.set(v.saturating_add(self.step));
            v
        }
        fn tick(&self, frame: Duration) {
            self.now.set(self.now.get().saturating_add(frame));
        }
    }

    /// A backend recording EVERY presented frame's text (the Recorder above
    /// keeps only the last).
    struct Tape {
        frames: Vec<String>,
    }
    impl Tape {
        fn new() -> Self {
            Self { frames: Vec::new() }
        }
    }
    impl RenderBackend for Tape {
        fn name(&self) -> &'static str {
            "tape"
        }
        fn capabilities(&self) -> Capabilities {
            Capabilities::empty()
        }
        fn present(&mut self, frame: &Frame) -> Result<(), RenderError> {
            self.frames.push(frame_text(frame));
            Ok(())
        }
    }

    fn frame_text(frame: &Frame) -> String {
        let mut s = String::new();
        for y in 0..frame.height() {
            for x in 0..frame.width() {
                s.push(frame.get(x, y).map_or(' ', |c| c.ch));
            }
            s.push('\n');
        }
        s
    }

    fn full3d_caps() -> Capabilities {
        Capabilities::MESH_3D
            | Capabilities::RAMP_SHADER
            | Capabilities::TRUECOLOR
            | Capabilities::UNICODE
            | Capabilities::SUBCELL
    }

    fn has_braille(s: &str) -> bool {
        s.chars().any(|c| (0x2800..=0x28FF).contains(&(c as u32)))
    }

    fn has_split_halfblock(s: &str) -> bool {
        // ▀/▄ are unique to the half-block density resolve; Reduced2d's crack
        // ramp uses ░▒▓█ only.
        s.chars().any(|c| matches!(c, '▀' | '▄'))
    }

    /// The deterministic final frame of a tier at a resolution (reference).
    fn reference_final(tier: Tier, res: Resolution, caps: Capabilities, size: Size) -> String {
        let mut b = Tape::new();
        let mut f = Frame::new(size);
        render2d::draw_tier(
            tier,
            1.0,
            SplashConfig::default().seed,
            &mut b,
            &mut f,
            caps,
            res,
        );
        frame_text(&f)
    }

    #[test]
    fn ladder_downgrades_resolution_first_then_tier() {
        let mut l = BudgetLadder::new(Tier::Full3d);
        assert_eq!(l.tier(), Tier::Full3d);
        assert_eq!(l.resolution(), Resolution::Full);
        let over = FRAME_BUDGET + Duration::from_millis(1);
        assert_eq!(l.record(over), Some(Downgrade::ResolutionHalved));
        assert_eq!((l.tier(), l.resolution()), (Tier::Full3d, Resolution::Half));
        assert_eq!(l.record(over), Some(Downgrade::TierDropped));
        assert_eq!(l.tier(), Tier::Reduced2d);
        // On the floor: further slow frames change nothing (no oscillation).
        assert_eq!(l.record(over), None);
        assert_eq!(l.tier(), Tier::Reduced2d);
    }

    #[test]
    fn ladder_holds_at_budget_and_below() {
        let mut l = BudgetLadder::new(Tier::Full3d);
        assert_eq!(l.record(FRAME_BUDGET), None, "exactly on budget is fine");
        assert_eq!(l.record(Duration::from_millis(1)), None);
        assert_eq!((l.tier(), l.resolution()), (Tier::Full3d, Resolution::Full));
    }

    #[test]
    fn ladder_never_touches_the_lower_tiers() {
        for tier in [Tier::Reduced2d, Tier::StaticAscii, Tier::Instant] {
            let mut l = BudgetLadder::new(tier);
            assert_eq!(l.record(Duration::from_secs(1)), None);
            assert_eq!(l.tier(), tier);
        }
    }

    proptest::proptest! {
        /// "Never a stutter": for ANY cost sequence the ladder moves at most
        /// two rungs, monotonically (never back up), so at most two frames
        /// ever miss the budget before the settled state.
        #[test]
        fn ladder_is_monotone_and_bounded(costs in proptest::collection::vec(0u64..40, 0..64)) {
            let mut l = BudgetLadder::new(Tier::Full3d);
            let mut downgrades = 0;
            let mut prev = (l.tier(), l.resolution());
            for ms in costs {
                if l.record(Duration::from_millis(ms)).is_some() {
                    downgrades += 1;
                }
                let now = (l.tier(), l.resolution());
                // Never upgrades: Full→Half and Full3d→Reduced2d are one-way.
                proptest::prop_assert!(!(prev.0 == Tier::Reduced2d && now.0 == Tier::Full3d));
                proptest::prop_assert!(
                    !(prev.1 == Resolution::Half && now.1 == Resolution::Full && now.0 == Tier::Full3d)
                );
                prev = now;
            }
            proptest::prop_assert!(downgrades <= 2);
        }
    }

    #[test]
    fn slow_clock_downgrades_resolution_then_tier_live() {
        // Injected slow machine: every probed frame costs 20ms (> 8ms). The
        // wired loop must present braille (Full3d/Full) once, half-blocks
        // (Full3d/Half) once, then Reduced2d for the rest — resolution first,
        // tier second, each frame presented exactly once (no stutter).
        let mut tape = Tape::new();
        let clock = SlowClock::new(Duration::from_millis(20));
        let cfg = SplashConfig::default();
        let size = Size::new(60, 20);
        run_splash(
            &mut tape,
            &mut Script::new(vec![]),
            &clock,
            &cfg,
            full3d_caps(),
            size,
        )
        .unwrap();

        assert!(tape.frames.len() >= 3, "probe + two rungs need ≥ 3 frames");
        let first = tape.frames.first().unwrap();
        assert!(has_braille(first), "frame 1 is the full-res braille raster");
        let second = tape.frames.get(1).unwrap();
        assert!(
            !has_braille(second) && has_split_halfblock(second),
            "frame 2 is the half-resolution raster (resolution downgraded first)"
        );
        for (i, later) in tape.frames.iter().enumerate().skip(2) {
            assert!(
                !has_braille(later) && !has_split_halfblock(later),
                "frame {i} must be the Reduced2d floor (tier dropped second)"
            );
        }
        // And the settled final frame is exactly the shipped 2D tier's.
        assert_eq!(
            tape.frames.last().unwrap(),
            &reference_final(Tier::Reduced2d, Resolution::Half, full3d_caps(), size),
        );
    }

    #[test]
    fn fast_clock_keeps_full3d_at_full_resolution() {
        // The ManualClock never advances inside the probe → cost 0 → the
        // ladder holds and the final frame is the full-res Full3d pose.
        let mut tape = Tape::new();
        let clock = ManualClock::new();
        let cfg = SplashConfig::default();
        let size = Size::new(60, 20);
        run_splash(
            &mut tape,
            &mut Script::new(vec![]),
            &clock,
            &cfg,
            full3d_caps(),
            size,
        )
        .unwrap();
        assert_eq!(
            tape.frames.last().unwrap(),
            &reference_final(Tier::Full3d, Resolution::Full, full3d_caps(), size),
        );
    }

    #[test]
    fn full3d_skip_lands_on_the_same_final_frame() {
        // Skip parity on the Full3d tier (M5-14): natural completion and an
        // instant keypress land on the identical deterministic final frame.
        let cfg = SplashConfig::default();
        let size = Size::new(60, 20);

        let mut nat = Tape::new();
        run_splash(
            &mut nat,
            &mut Script::new(vec![]),
            &ManualClock::new(),
            &cfg,
            full3d_caps(),
            size,
        )
        .unwrap();

        let mut skip = Tape::new();
        let mut src = Script::new(vec![vec![Event::Key(CanopyKey::Char('x'))]]);
        run_splash(
            &mut skip,
            &mut src,
            &ManualClock::new(),
            &cfg,
            full3d_caps(),
            size,
        )
        .unwrap();

        assert_eq!(skip.frames.len(), 1, "skip presents only the final frame");
        assert_eq!(
            nat.frames.last(),
            skip.frames.last(),
            "skip must land on the same deterministic Full3d final frame"
        );
        assert!(has_braille(skip.frames.last().unwrap()));
    }
}
