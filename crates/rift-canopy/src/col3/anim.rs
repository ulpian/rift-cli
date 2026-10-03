//! The column's tween: what eases on a data change and what snaps.
//!
//! Only the *plottable series* interpolate (bar/line heights); every scalar
//! headline and enum variant is taken from the target immediately, so a mid-tween
//! read still shows the true total/band/count and a fresh headless render (which
//! never animates) is always the settled target. A section whose variant changed
//! (placeholder↔Data) snaps rather than tweens.
//!
//! The **nic-gauge does not tween**: its `pct` is *both* the gauge fill and the
//! title headline (`"{pct}% {band}"`), so easing it would desync the number from
//! its band (e.g. show "80% Clear" mid-transition). The meter snaps to each
//! reading — consistent, and standard for a small horizontal gauge.

use super::calls::{CallsData, ChannelCallSeries};
use super::recall::RecallView;
use super::Col3;

/// Linear interpolation of two `f64`s at `t ∈ [0, 1]`.
fn lerp_f64(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Interpolate two equal-length series element-wise; `None` when the lengths
/// differ (the caller snaps to the target rather than guess an alignment).
fn lerp_series(a: &[f64], b: &[f64], t: f64) -> Option<Vec<f64>> {
    if a.len() != b.len() {
        return None;
    }
    Some(a.iter().zip(b).map(|(x, y)| lerp_f64(*x, *y, t)).collect())
}

/// Interpolate a `RecallView<T>` only when both sides carry data; otherwise the
/// target wins (the placeholder↔data transition snaps).
fn interp_recall<T: Clone>(
    from: &RecallView<T>,
    to: &RecallView<T>,
    blend: impl Fn(&T, &T) -> T,
) -> RecallView<T> {
    match (from, to) {
        (RecallView::Data(a), RecallView::Data(b)) => RecallView::Data(blend(a, b)),
        _ => to.clone(),
    }
}

/// Would tweening `from → to` show any motion? True iff at least one section is a
/// same-variant `Data` change with an interpolable (equal-length) series or a
/// changed value — so a pure placeholder↔data update snaps instead.
#[must_use]
pub(crate) fn should_animate(from: &Col3, to: &Col3) -> bool {
    // Token composition, Nic and the recall savings deliberately snap: every
    // proportional area, headline and compact readout must describe the same
    // atomic feed frame. Only the per-channel call chart is a series over time
    // and therefore the only thing on this card with motion to show.
    matches!((&from.calls, &to.calls),
            (RecallView::Data(a), RecallView::Data(b)) if calls_can_animate(a, b)
                && a.series.iter().zip(&b.series).any(|(x, y)| x.rate != y.rate))
}

/// The displayed `Col3` at tween progress `t ∈ [0, 1]`: `from`'s series eased
/// toward `to`'s, with all headlines/variants taken from `to`.
#[must_use]
pub(crate) fn interpolate(from: &Col3, to: &Col3, t: f32) -> Col3 {
    let t = f64::from(t.clamp(0.0, 1.0));
    Col3 {
        // Not tweened: the wake mode is a fact, read from the target.
        watch: to.watch,
        tokens: to.tokens.clone(),
        breakdown: to.breakdown.clone(),
        // The nic-gauge snaps: its pct is a headline (title + fill share it).
        nic: to.nic.clone(),
        // The savings card snaps whole. Every figure on it is a headline over
        // one window — the share, the money and its rows, the axis of counts —
        // and a frame between two measurements is a number no window held.
        savings: to.savings.clone(),
        calls: interp_recall(&from.calls, &to.calls, |a, b| {
            if !calls_can_animate(a, b) {
                return b.clone();
            }
            CallsData {
                total: b.total,
                window_end_ms: b.window_end_ms,
                series: a
                    .series
                    .iter()
                    .zip(&b.series)
                    .map(|(x, y)| ChannelCallSeries {
                        channel: y.channel.clone(),
                        last_call_at: y.last_call_at,
                        rate: lerp_series(&x.rate, &y.rate, t).unwrap_or_else(|| y.rate.clone()),
                    })
                    .collect(),
            }
        }),
        active_sessions: to.active_sessions.clone(),
        // Not tweened: the chosen window is the viewer's, read from the
        // target so the pressed control lights in the frame of the press.
        recall_window: to.recall_window,
        // Domain metrics are headlines — taken from the target immediately.
        metrics: to.metrics.clone(),
        // The workflow panels carry no plottable series at all: every value in
        // them is a count, a split or a formatted figure, and easing a count
        // would put a number on screen that no window ever yielded. They take
        // the target, like every other headline here.
        dispatch: to.dispatch.clone(),
        recap: to.recap.clone(),
    }
}

/// Calls may ease only when every sample still denotes the same time bucket
/// and serve channel. A shifted window or reordered channel set snaps.
fn calls_can_animate(a: &CallsData, b: &CallsData) -> bool {
    a.window_end_ms == b.window_end_ms
        && a.series.len() == b.series.len()
        && a.series
            .iter()
            .zip(&b.series)
            .all(|(x, y)| x.channel == y.channel && x.rate.len() == y.rate.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::fixtures::{share_fixture, usd_col3};
    use super::super::savings::{SavingsData, ShareData};
    use super::super::system::{NicReading, NicView};
    use super::super::tokens::{BreakdownData, BreakdownView, TokensData, TokensView};
    use super::super::worktree::WorktreeAxis;

    /// the priced savings estimate: money never animates — the estimate and its rows are
    /// headlines, copied from the target frame exactly like `pct`.
    #[test]
    fn money_is_a_headline_never_tweened() {
        let mut from = usd_col3(Some("~$1.00"));
        if let RecallView::Data(sd) = &mut from.savings {
            sd.usd_saved_micros = Some(1_000_000);
        }
        let to = usd_col3(Some("~$6.25"));
        let mid = interpolate(&from, &to, 0.5);
        let RecallView::Data(sd) = mid.savings else {
            panic!("expected Data");
        };
        assert_eq!(
            sd.usd_saved_micros,
            Some(6_250_000),
            "target's figure, not a blend"
        );
        assert_eq!(sd.usd_label.as_deref(), Some("~$6.25"));
    }

    fn savings(avoided: u64) -> RecallView<SavingsData> {
        RecallView::Data(SavingsData {
            share: Box::new(ShareData {
                overall: Some(3.0),
                avoided_tokens: avoided,
                read_tokens: 1_000,
                ..ShareData::default()
            }),
            ..SavingsData::default()
        })
    }

    /// The savings card **snaps**: every figure on it is a headline over one
    /// window, and a frame between two measurements is a number no window
    /// held. Only the per-channel call chart, a series over time, animates.
    #[test]
    fn the_savings_card_is_taken_from_the_target_frame_whole() {
        let from = Col3 {
            savings: savings(100),
            ..Col3::default()
        };
        let to = Col3 {
            savings: savings(700),
            ..Col3::default()
        };
        for t in [0.0, 0.5, 1.0] {
            assert_eq!(
                interpolate(&from, &to, t).savings,
                to.savings,
                "the card is the target's at t={t}"
            );
        }
        assert!(
            !should_animate(&from, &to),
            "a changed savings figure is not motion"
        );
    }

    #[test]
    fn placeholder_to_data_snaps_not_tweens() {
        let from = Col3::default(); // NoActivity everywhere
        let to = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![5.0],
                total: 5.0,
                by_worktree: WorktreeAxis::default(),
            }),
            ..Col3::default()
        };
        assert!(
            !should_animate(&from, &to),
            "a placeholder→data update must snap"
        );
        // And interpolate at t=0 already shows the target (no blank tween frame).
        assert_eq!(interpolate(&from, &to, 0.0).tokens, to.tokens);
    }

    /// The call chart is the card's one series over time, so it is the one
    /// thing on it with motion to show.
    #[test]
    fn recall_call_series_change_animates() {
        let card = |value: f64| Col3 {
            savings: savings(700),
            calls: RecallView::Data(CallsData {
                total: 2,
                window_end_ms: 1_800_000,
                series: vec![ChannelCallSeries {
                    channel: "hook_inject".into(),
                    last_call_at: 1_799_000,
                    rate: vec![value; 30],
                }],
            }),
            ..Col3::default()
        };
        let (from, to) = (card(1.0), card(4.0));
        assert!(should_animate(&from, &to));
        match interpolate(&from, &to, 0.5).calls {
            RecallView::Data(d) => assert_eq!(d.series[0].rate[0], 2.5, "half-way"),
            _ => panic!("expected Data"),
        }
    }

    #[test]
    fn token_composition_snaps_atomically() {
        let make = |input, read| Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![input, read],
                total: input + read,
                by_worktree: WorktreeAxis::default(),
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input,
                read,
                output: 0.0,
                cache_write: 0.0,
                read_fraction: read / (input + read),
            }),
            ..Col3::default()
        };
        let from = make(10.0, 10.0);
        let to = make(20.0, 80.0);
        assert!(!should_animate(&from, &to));
        let mid = interpolate(&from, &to, 0.5);
        assert_eq!(mid.tokens, to.tokens);
        assert_eq!(mid.breakdown, to.breakdown);
    }

    #[test]
    fn recall_lines_snap_when_the_window_or_channel_identity_changes() {
        let calls = |window_end_ms, channel: &str, value| CallsData {
            total: 1,
            window_end_ms,
            series: vec![ChannelCallSeries {
                channel: channel.into(),
                last_call_at: window_end_ms - 1,
                rate: vec![value; 30],
            }],
        };
        let base = calls(1_800_000, "hook_inject", 1.0);
        assert!(calls_can_animate(
            &base,
            &calls(1_800_000, "hook_inject", 2.0)
        ));
        assert!(!calls_can_animate(
            &base,
            &calls(1_860_000, "hook_inject", 2.0)
        ));
        assert!(!calls_can_animate(
            &base,
            &calls(1_800_000, "skill_cli", 2.0)
        ));
    }

    #[test]
    fn nic_reading_change_snaps_and_never_tweens() {
        // The nic pct is a headline (title + gauge share it), so a reading change
        // must snap — never show an eased pct beside the target band.
        let reading = |pct, band| Col3 {
            nic: NicView::Reading(NicReading {
                pct,
                band,
                model: "m".into(),
                active_sessions: 1,
            }),
            ..Col3::default()
        };
        let from = reading(80, "High");
        let to = reading(20, "Clear");
        assert!(
            !should_animate(&from, &to),
            "a nic reading change must not arm a tween"
        );
        // interpolate at any progress shows the TARGET reading verbatim — pct and
        // band always agree, never "80% Clear".
        for t in [0.0, 0.5, 1.0] {
            assert_eq!(interpolate(&from, &to, t).nic, to.nic);
        }
    }

    /// The share is a headline, not an animation: interpolating two frames
    /// takes it from the target whole, so no frame of a tween is ever read as
    /// a measurement.
    #[test]
    fn the_share_is_never_tweened() {
        let from = Col3 {
            savings: RecallView::Data(share_fixture(Some(1.0))),
            ..Col3::default()
        };
        let to = Col3 {
            savings: RecallView::Data(share_fixture(Some(9.0))),
            ..Col3::default()
        };
        let mid = interpolate(&from, &to, 0.5);
        let RecallView::Data(sd) = mid.savings else {
            panic!("data");
        };
        assert_eq!(sd.share.overall, Some(9.0));
    }
}
