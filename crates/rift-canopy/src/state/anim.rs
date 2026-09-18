//! The dashboard's motion: the idle cadence, the settle windows, the
//! column-3 tween and every dt-driven pulse [`AppState`] carries.
//!
//! Separate so the animator contract — fast-poll only while something is in
//! flight, never a timer or a thread — lives in one place, with the constants
//! that bound each window beside the methods that advance them.

use std::sync::Arc;
use std::time::Duration;

use crate::dispatch::Overlay;
use crate::hit::Target;
use crate::Mode;

use super::{AppState, WatchMode};

/// How many idle ticks between background re-stats of the selected project
/// when a native watcher backs the bus — the **status/aging cadence only**,
/// never the metric driver: panel metrics are anchored to events, so
/// time alone cannot move them, and fs wakes drive their recompute). At the
/// 100ms poll cadence this re-reads the foreground project roughly every 60s
/// — ample for the 30-minute `Running` → `Idle` aging (activity falling out of
/// the window surfaces even though no cursor change fires a bus poll).
/// Time-based only; never dirties.
pub(super) const REFRESH_EVERY_TICKS_WATCHED: u32 = 600;

/// The same cadence when the bus is poll-only: with
/// no watcher the heartbeat IS the wake, so it tightens to ~1.5s rather than
/// leaving the dashboard a minute stale. Adaptive, never silent.
pub(super) const REFRESH_EVERY_TICKS_POLL_ONLY: u32 = 15;

/// The idle re-stat cadence for the bus's reported wake mode.
pub(super) const fn refresh_every_ticks(mode: WatchMode) -> u32 {
    match mode {
        WatchMode::Native => REFRESH_EVERY_TICKS_WATCHED,
        WatchMode::PollOnly => REFRESH_EVERY_TICKS_POLL_ONLY,
    }
}

/// How long the one-shot startup sweep runs as the panels first fill after the
/// splash (the animator contract) — a single settle window, then it never re-fires.
const STARTUP_SWEEP: Duration = Duration::from_millis(450);
/// How long a selection/hover pulse takes to decay back to rest.
const PULSE_DECAY: Duration = Duration::from_millis(300);

/// How long the suggest panel's text fades in (the animator contract's shared contract).
const SUGGEST_FADE: Duration = Duration::from_millis(180);

/// How long a column-3 data change eases from its old shape to the new one. Kept
/// short so the graph "catches up" without feeling laggy; the loop only spins
/// fast (and only redraws) for this window, then returns to the ~0% CPU idle.
const COL3_SETTLE: Duration = Duration::from_millis(350);

/// How long a success/info [`Overlay::Notice`] lingers before its `dt`-timed
/// auto-dismiss under the animator contract — a bounded settle, not a timer or
/// thread, so the
/// idle property holds (the loop spins fast only for this window, then rests).
const NOTICE_SETTLE: Duration = Duration::from_millis(2200);

/// An in-flight column-3 tween: where the series eased *from*, and how far in.
/// The *target* is always `AppState::col3` (the truth), so a mid-tween snapshot
/// still reports the true headline numbers.
#[derive(Clone, Debug)]
pub(super) struct Col3Anim {
    /// The pre-change series to ease away from.
    from: Arc<crate::col3::Col3>,
    /// Elapsed time since the change (drives progress against [`COL3_SETTLE`]).
    elapsed: Duration,
}

impl AppState {
    /// Arm the one-shot startup sweep: the panels reveal as the
    /// dashboard first fills after the splash. Reduced motion shows the final
    /// frame instantly. Called once at launch; the sweep never re-fires.
    pub fn arm_startup_sweep(&mut self) {
        self.startup_sweep = if self.reduced_motion { 1.0 } else { 0.0 };
    }

    /// The startup-sweep reveal progress (0..=1) for the renderer.
    #[must_use]
    pub fn startup_sweep(&self) -> f32 {
        self.startup_sweep
    }

    /// (Re)arm the suggest panel's fade-in: entering command mode starts the
    /// ~180 ms ramp; reduced motion shows instantly.
    pub fn arm_suggest_fade(&mut self) {
        self.suggest_fade = if self.reduced_motion { 1.0 } else { 0.0 };
    }

    /// The current suggest fade progress (0..=1).
    #[must_use]
    pub fn suggest_fade(&self) -> f32 {
        self.suggest_fade
    }

    /// Replace the column-3 sections' view-model (the graph feeds' apply path).
    ///
    /// An identical replay is a no-op (delivery robustness). Otherwise the new
    /// value becomes the target and — unless `reduced_motion` or the change is a
    /// pure placeholder↔data transition — a tween is armed from the prior value,
    /// so the graphs ease to the new shape over `COL3_SETTLE`. Headlines and
    /// variants are never tweened (see `crate::col3::interpolate`).
    pub fn set_col3(&mut self, col3: crate::col3::Col3) {
        if *self.col3 == col3 {
            return;
        }
        let animate = !self.reduced_motion && crate::col3::should_animate(&self.col3, &col3);
        if animate {
            // Ease from what is on screen *now* — the interpolated frame if a tween
            // is already running — so a re-tween continues smoothly instead of
            // snapping back to the previous target.
            let from = self.displayed_col3();
            self.col3 = Arc::new(col3);
            self.col3_anim = Some(Col3Anim {
                from,
                elapsed: Duration::ZERO,
            });
        } else {
            // A non-animating change (a nic snap, or a placeholder↔data variant
            // flip): adopt the new target but LEAVE any in-flight tween running —
            // `interpolate` reads non-tweened sections from the target, so the fresh
            // values show at once while other sections keep easing (no
            // cross-section cancellation).
            self.col3 = Arc::new(col3);
        }
        self.dirty = true;
    }

    /// Advance any in-flight column-3 tween by `dt`, clearing it once it settles.
    /// Returns `true` while a tween is (or was just) active, so the loop redraws
    /// the interpolated frame; returns `false` at rest (no wasted redraw). Takes
    /// an explicit `dt` so it is deterministic in tests.
    pub fn advance_anim(&mut self, dt: Duration) -> bool {
        let mut active = false;
        if let Some(anim) = self.col3_anim.as_mut() {
            anim.elapsed = anim.elapsed.saturating_add(dt);
            if anim.elapsed >= COL3_SETTLE {
                self.col3_anim = None; // the final (settled) frame renders the target
            }
            active = true;
        }
        // The suggest panel's fade rides the same dt-advance: the
        // loop's settle-window fast poll carries the frames; at 1.0 it is a
        // no-op and never redraws (the idle property).
        if self.app.mode == Mode::Command && self.suggest_fade < 1.0 {
            let step = dt.as_secs_f32() / SUGGEST_FADE.as_secs_f32();
            self.suggest_fade = (self.suggest_fade + step).min(1.0);
            active = true;
        }
        // The one-shot startup sweep: advances to 1.0 then rests
        // forever (it never re-arms, so it can't re-fire).
        if self.startup_sweep < 1.0 {
            let step = dt.as_secs_f32() / STARTUP_SWEEP.as_secs_f32();
            self.startup_sweep = (self.startup_sweep + step).min(1.0);
            active = true;
        }
        // The selection pulse decays to rest.
        if self.select_pulse > 0.0 {
            let step = dt.as_secs_f32() / PULSE_DECAY.as_secs_f32();
            self.select_pulse = (self.select_pulse - step).max(0.0);
            active = true;
        }
        // The button press-flash decays to rest — same shape as the
        // selection pulse, keyed by `pressed`.
        if self.press_pulse > 0.0 {
            let step = dt.as_secs_f32() / PULSE_DECAY.as_secs_f32();
            self.press_pulse = (self.press_pulse - step).max(0.0);
            if self.press_pulse <= 0.0 {
                self.pressed = None;
            }
            active = true;
        }
        // A Notice auto-dismisses by `dt`: its settle decays and, at 0,
        // the Notice closes itself — a bounded settle, never a timer/thread. A
        // Notice dismissed early (X/Esc) drops out here (the overlay is no longer
        // a Notice), so the settle can't keep the loop awake past its close.
        if matches!(self.app.overlay, Some(Overlay::Notice(_))) {
            let step = dt.as_secs_f32() / NOTICE_SETTLE.as_secs_f32();
            self.notice_settle = (self.notice_settle - step).max(0.0);
            if self.notice_settle <= 0.0 {
                self.app.overlay = None;
                self.app.overlay_focus = 0;
                self.app.maybe_raise_wizard(); // an empty registry keeps its wizard
            }
            active = true;
        } else {
            self.notice_settle = 0.0;
        }
        active
    }

    /// Whether any dt-tween is in flight (drives the loop's fast-poll window):
    /// a col-3 tween, the suggest fade, the startup sweep, or a selection pulse.
    #[must_use]
    pub fn anim_active(&self) -> bool {
        self.col3_anim.is_some()
            || (self.app.mode == Mode::Command && self.suggest_fade < 1.0)
            || self.startup_sweep < 1.0
            || self.select_pulse > 0.0
            || self.press_pulse > 0.0
            || (matches!(self.app.overlay, Some(Overlay::Notice(_))) && self.notice_settle > 0.0)
    }

    /// Arm a selection/hover pulse: a brief colour lerp acknowledges
    /// the cursor. Reduced motion skips it (rests at 0).
    pub(super) fn arm_select_pulse(&mut self) {
        if !self.reduced_motion {
            self.select_pulse = 1.0;
        }
    }

    /// Arm a button press-flash, keyed by `target`: a brief colour
    /// lerp acknowledges the press. Bound to the activation event, so a repeat
    /// press re-arms; reduced motion skips it (rests at 0).
    pub(super) fn arm_press_pulse(&mut self, target: Target) {
        if !self.reduced_motion {
            self.press_pulse = 1.0;
            self.pressed = Some(target);
        }
    }

    /// The displayed column-3 value: the interpolated (eased) frame while a tween
    /// is in flight, else the shared target `Arc` (unchanged — so a quiescent
    /// snapshot stays pointer-equal, the ~0% CPU invariant).
    pub(super) fn displayed_col3(&self) -> Arc<crate::col3::Col3> {
        match &self.col3_anim {
            Some(anim) => {
                let progress = ease_out(elapsed_fraction(anim.elapsed));
                Arc::new(crate::col3::interpolate(&anim.from, &self.col3, progress))
            }
            None => Arc::clone(&self.col3),
        }
    }
}

/// The raw linear progress of a tween in `[0, 1]` for `elapsed` against the
/// settle window.
fn elapsed_fraction(elapsed: Duration) -> f32 {
    let settle = COL3_SETTLE.as_secs_f32();
    if settle <= 0.0 {
        return 1.0;
    }
    (elapsed.as_secs_f32() / settle).clamp(0.0, 1.0)
}

/// Ease-out cubic: fast start, gentle finish — the motion reads as "settling"
/// rather than linear. Endpoints are exact (`0 → 0`, `1 → 1`).
fn ease_out(t: f32) -> f32 {
    let inv = 1.0 - t.clamp(0.0, 1.0);
    1.0 - inv * inv * inv
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::Message;
    use crate::input::CanopyKey;
    use crate::render::frame::Size;
    use crate::state::click::apply_click;
    use crate::state::fixtures::*;
    use crate::state::{update, Event, FeedPayload, FeedUpdate};

    #[test]
    fn startup_sweep_fires_once_then_settles_and_never_re_fires() {
        let mut s = ready(vec![project("a")]);
        assert!(
            (s.startup_sweep - 1.0).abs() < f32::EPSILON,
            "rests done by default"
        );
        s.arm_startup_sweep();
        assert_eq!(s.startup_sweep, 0.0, "armed to the start");
        assert!(s.anim_active(), "a sweeping frame fast-polls");

        // A chosen dt advances the reveal deterministically.
        assert!(s.advance_anim(STARTUP_SWEEP / 2));
        assert!(
            (s.startup_sweep - 0.5).abs() < 0.02,
            "half-way after half the window"
        );
        assert!(s.advance_anim(STARTUP_SWEEP)); // overshoots → clamps to 1.0
        assert_eq!(s.startup_sweep, 1.0, "settles at the final frame");

        // At rest it is idle and a further advance never re-arms it.
        assert!(!s.anim_active());
        assert!(!s.advance_anim(STARTUP_SWEEP));
        assert_eq!(s.startup_sweep, 1.0, "the one-shot sweep never re-fires");
    }

    #[test]
    fn reduced_motion_shows_the_final_frame_instantly() {
        let mut s = ready(vec![project("a")]);
        s.reduced_motion = true;
        s.arm_startup_sweep();
        assert_eq!(
            s.startup_sweep, 1.0,
            "reduced motion snaps to the settled frame"
        );
        assert!(!s.anim_active());
        // The pulse is likewise skipped.
        s.arm_select_pulse();
        assert_eq!(s.select_pulse, 0.0);
    }

    #[test]
    fn selection_change_arms_a_decaying_pulse() {
        let mut s = ready(vec![project("a"), project("b")]);
        update(&mut s, Event::Key(CanopyKey::Down)); // select b
        assert_eq!(s.select_pulse, 1.0, "a cursor move arms the pulse");
        assert!(s.anim_active());
        assert!(s.advance_anim(PULSE_DECAY / 2));
        assert!(s.select_pulse < 1.0 && s.select_pulse > 0.0, "it decays");
        assert!(s.advance_anim(PULSE_DECAY)); // overshoots → clamps to 0
        assert_eq!(s.select_pulse, 0.0, "decays to rest");
        assert!(!s.anim_active());
    }

    #[test]
    fn a_button_press_flashes_and_re_presses_re_arm() {
        let mut s = ready(vec![project("a")]);
        // A button press arms the flash keyed by its target.
        let _ = apply_click(&mut s, Target::HeaderHelp);
        assert_eq!(s.press_pulse, 1.0, "a button press arms the flash");
        assert_eq!(s.pressed, Some(Target::HeaderHelp));
        assert!(s.anim_active());
        assert_eq!(s.select_pulse, 0.0, "the selection pulse is untouched");
        // It decays…
        assert!(s.advance_anim(PULSE_DECAY / 2));
        assert!(s.press_pulse < 1.0 && s.press_pulse > 0.0, "it decays");
        // …and a fresh press re-arms it to full (bound to the event, not a change).
        let _ = apply_click(&mut s, Target::HeaderHelp);
        assert_eq!(s.press_pulse, 1.0, "re-pressing the same button re-flashes");
        // Overshoot → rest, and `pressed` clears.
        assert!(s.advance_anim(PULSE_DECAY));
        assert_eq!(s.press_pulse, 0.0, "decays to rest");
        assert_eq!(s.pressed, None);
        assert!(!s.anim_active());
    }

    #[test]
    fn reduced_motion_snaps_the_press_flash() {
        let mut s = ready(vec![project("a")]);
        s.reduced_motion = true;
        let _ = apply_click(&mut s, Target::HeaderHelp);
        assert_eq!(s.press_pulse, 0.0, "reduced motion never flashes");
        assert_eq!(s.pressed, None);
    }

    #[test]
    fn a_row_select_uses_the_selection_pulse_not_the_press_flash() {
        let mut s = ready(vec![project("a"), project("b")]);
        let _ = apply_click(&mut s, Target::ProjectRow(1));
        assert_eq!(s.press_pulse, 0.0, "a row select is not a button press");
        assert!(s.pressed.is_none());
        assert_eq!(s.select_pulse, 1.0, "it arms the selection pulse instead");
    }

    #[test]
    fn sweep_reveal_snapshots_the_full_frame_at_rest() {
        // The settled snapshot (sweep = 1.0) is the full dashboard — every
        // headless/golden render sees the final frame, never a mid-wipe one.
        use crate::render::frame::Frame;
        let mut s = ready(vec![project("a")]);
        let vm = s.snapshot();
        assert!((vm.sweep - 1.0).abs() < f32::EPSILON);
        // Mid-sweep, the lower rows blank (a wipe reveal).
        s.arm_startup_sweep();
        s.advance_anim(STARTUP_SWEEP / 4); // ~25% revealed
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        crate::view::draw(&vm, &mut b, &mut f);
        // A row near the bottom of the body is blank during the wipe.
        let bottom = vm.size.h.saturating_sub(2);
        let row: String = (0..vm.size.w)
            .map(|x| f.get(x, bottom).map_or(' ', |c| c.ch))
            .collect();
        assert_eq!(
            row.trim(),
            "",
            "rows below the reveal line are blank mid-sweep"
        );
    }

    #[test]
    fn metrics_feed_patches_col3_metrics_without_a_tween() {
        use crate::col3::PanelMetrics;
        let mut s = ready(vec![project("a")]);
        let metrics = PanelMetrics {
            events_5m: Some(3),
            tokens_5m: Some(600),
            serve_calls_5m: Some(2),
            projection_bytes: Some(4096),
            activity_scope: crate::col3::SystemScope::CurrentSessions,
            ..PanelMetrics::default()
        };
        update(
            &mut s,
            Event::Feed(FeedUpdate {
                project: "/r/a".into(),
                payload: FeedPayload::System {
                    context: crate::col3::NicView::NoData,
                    metrics: metrics.clone(),
                    active_sessions: crate::col3::ActiveSessionsView::NoSessions,
                },
            }),
        );
        assert_eq!(s.snapshot().col3.metrics, metrics);
        assert!(
            s.col3_anim.is_none(),
            "a metrics-only change never arms a tween"
        );
    }

    #[test]
    fn suggest_fade_is_deterministic_by_dt_and_idles_after_settle() {
        // the animator contract: progress is a pure function of accumulated dt; after the
        // settle the animator is inert (the fast-poll-only-while-animating
        // idle property extends to the panel).
        let mut a = ready(vec![project("a")]);
        let mut b = ready(vec![project("a")]);
        for s in [&mut a, &mut b] {
            update(s, Event::Key(CanopyKey::Char('/')));
            assert!(s.anim_active(), "entering command mode arms the fade");
        }
        // Two identical dt sequences produce identical progress.
        for s in [&mut a, &mut b] {
            assert!(s.advance_anim(Duration::from_millis(60)));
            assert!(s.advance_anim(Duration::from_millis(60)));
        }
        assert!((a.suggest_fade() - b.suggest_fade()).abs() < f32::EPSILON);
        assert!(a.suggest_fade() < 1.0);
        // Settle: past 180ms the fade is done and the loop idles again.
        assert!(a.advance_anim(Duration::from_millis(120)));
        assert!((a.suggest_fade() - 1.0).abs() < f32::EPSILON);
        assert!(!a.anim_active(), "idle after settle");
        assert!(
            !a.advance_anim(Duration::from_millis(16)),
            "no wasted redraw at rest"
        );
    }

    #[test]
    fn reduced_motion_snaps_the_suggest_fade() {
        let mut s = ready(vec![project("a")]);
        s.reduced_motion = true;
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        assert!(
            (s.suggest_fade() - 1.0).abs() < f32::EPSILON,
            "instant final frame"
        );
        assert!(!s.anim_active());
    }

    #[test]
    fn token_data_to_data_feed_snaps_atomically() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, tokens_feed(vec![1.0, 2.0], 3.0));
        assert!(!s.anim_active(), "the first population snaps");
        update(&mut s, tokens_feed(vec![5.0, 6.0], 11.0));
        assert!(!s.anim_active(), "token composition never arms a tween");
        assert_eq!(tokens_of(&s.snapshot()), (vec![5.0, 6.0], 11.0));
    }

    #[test]
    fn reduced_motion_snaps_without_a_tween() {
        let mut s = ready(vec![project("a")]);
        s.reduced_motion = true;
        update(&mut s, tokens_feed(vec![1.0], 1.0));
        update(&mut s, tokens_feed(vec![9.0], 9.0));
        assert!(!s.anim_active(), "reduced_motion never tweens");
        assert_eq!(
            tokens_of(&s.snapshot()).0,
            vec![9.0],
            "shows the target at once"
        );
    }

    #[test]
    fn a_non_animating_feed_keeps_an_in_flight_tween() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, recall_feed(vec![1.0, 2.0])); // placeholder→Data: snaps
        update(&mut s, recall_feed(vec![5.0, 6.0])); // Data→Data: arms a tween
        assert!(s.anim_active());
        // A nic feed snaps (nic is never tweened); it must NOT cancel the tokens
        // recall tween, and its own value must show immediately.
        let nic = Event::Feed(FeedUpdate {
            project: "/r/a".into(),
            payload: FeedPayload::System {
                context: crate::col3::NicView::Reading(crate::col3::NicReading {
                    pct: 50,
                    band: "Moderate",
                    model: "m".into(),
                    active_sessions: 1,
                }),
                metrics: crate::col3::PanelMetrics::default(),
                active_sessions: crate::col3::ActiveSessionsView::Count(1),
            },
        });
        update(&mut s, nic);
        assert!(
            s.anim_active(),
            "an unrelated (snapping) nic update must not cancel the recall tween"
        );
        match &s.snapshot().col3.nic {
            crate::col3::NicView::Reading(r) => assert_eq!(r.pct, 50, "nic shows at once"),
            other => panic!("expected a reading, got {other:?}"),
        }
    }

    #[test]
    fn advance_anim_is_a_noop_at_rest() {
        let mut s = ready(vec![project("a")]);
        assert!(
            !s.advance_anim(Duration::from_millis(16)),
            "with no tween in flight, advancing is a no-op (no redraw)"
        );
    }

    #[test]
    fn identical_feed_replay_neither_dirties_nor_tweens() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, tokens_feed(vec![3.0], 3.0)); // placeholder→Data, snaps
        s.dirty = false;
        update(&mut s, tokens_feed(vec![3.0], 3.0)); // identical replay
        assert!(!s.dirty, "an identical feed replay must not redraw");
        assert!(!s.anim_active(), "and must not arm a tween");
    }

    #[test]
    fn open_notice_arms_a_bounded_auto_dismiss() {
        let mut s = ready(vec![project("a")]);
        s.reduced_motion = false;
        s.open_notice(Message::success("ok", vec!["done".into()]));
        assert!(matches!(s.app.overlay, Some(Overlay::Notice(_))));
        assert!(s.anim_active(), "a pending Notice keeps the loop awake");
        // A partial dt keeps it open; a dt past the settle window closes it.
        assert!(s.advance_anim(Duration::from_millis(200)));
        assert!(
            matches!(s.app.overlay, Some(Overlay::Notice(_))),
            "still up"
        );
        let _ = s.advance_anim(Duration::from_secs(3));
        assert!(s.app.overlay.is_none(), "the Notice auto-dismissed by dt");
        assert!(!s.anim_active(), "and the loop returns to idle");
    }

    #[test]
    fn a_notice_dismissed_early_does_not_keep_the_loop_awake() {
        let mut s = ready(vec![project("a")]);
        s.open_notice(Message::success("ok", vec!["done".into()]));
        s.app.overlay = None; // the user pressed Esc / X before the settle
        assert!(!s.anim_active(), "an early dismiss ends the settle");
        assert!(!s.advance_anim(Duration::from_millis(16)));
    }

    #[test]
    fn open_alert_does_not_arm_the_settle() {
        let mut s = ready(vec![project("a")]);
        s.open_alert(Message::error("e", vec!["boom".into()]));
        assert!(matches!(s.app.overlay, Some(Overlay::Alert(_))));
        assert!(!s.anim_active(), "an Alert blocks; it never auto-dismisses");
    }

    #[test]
    fn a_success_notice_over_an_emptying_registry_re_raises_the_wizard() {
        // The removal-to-empty case: a Notice shows, then its settle-close hands
        // the surface back to the wizard (the empty registry keeps its prompt).
        let mut s = AppState::new(vec![], Size::new(120, 40));
        s.open_notice(Message::success("registry", vec!["removed /a".into()]));
        assert!(matches!(s.app.overlay, Some(Overlay::Notice(_))));
        let _ = s.advance_anim(Duration::from_secs(3));
        assert!(
            s.app.showing_wizard(),
            "the wizard returns after the notice"
        );
    }
}
