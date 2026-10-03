//! The Recall panel's period row: the four trailing windows, what each cuts
//! against one clock reading, and the row of buttons that one solver places
//! for the draw and the hit-test alike.

use crate::layout::Rect;
use crate::render::frame::Frame;
use crate::theme::Theme;

use super::Col3;

/// The period the Recall panel's figures cover — a viewer's choice, not a
/// property of the data.
///
/// Every variant is a **cut**, and the cut is the only thing that leaves this
/// type: it is handed to `rift_recall_ledger::savings::compose` exactly the way
/// `rift savings --since` hands one in, so the dashboard runs the same
/// computation over a different window rather than a different computation.
/// Nothing here re-derives a share.
///
/// The three narrow windows are **trailing**, not calendar: a calendar "today"
/// needs a timezone this crate has no business holding, and four windows sitting
/// side by side that are not the same *kind* of measurement is worse than one of
/// them being spelled `24h` instead of `today`.
///
/// [`Self::AllTime`] is not "every row ever" — it is the window the family's
/// boundary marker names, which is what this card, `rift savings` and the status
/// table have always meant by "since the reset". It carries no cut of its own so
/// the marker still decides, and the default therefore renders exactly as it did
/// before the buttons existed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecallWindow {
    /// The trailing 24 hours.
    Today,
    /// The trailing 7 days.
    Last7Days,
    /// The trailing 30 days.
    Last30Days,
    /// The family's boundary marker, or the whole record when it has none.
    #[default]
    AllTime,
}

/// Milliseconds in a day — the trailing windows' unit.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

impl RecallWindow {
    /// The windows in button order, narrowest first.
    pub const ALL: [RecallWindow; 4] = [
        RecallWindow::Today,
        RecallWindow::Last7Days,
        RecallWindow::Last30Days,
        RecallWindow::AllTime,
    ];

    /// The button's glyphs — short, because four of them share one row.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RecallWindow::Today => "24h",
            RecallWindow::Last7Days => "7d",
            RecallWindow::Last30Days => "30d",
            RecallWindow::AllTime => "all",
        }
    }

    /// What this window resolves to against one reading of the clock.
    ///
    /// It answers with three cases rather than an `Option<i64>` because there
    /// are three, and the middle one used to be spelled as the first: a
    /// trailing window the clock cannot place was clamped at the epoch, which
    /// is "every row ever" — so a failed clock drew the whole record under a
    /// button that says `24h`. The cases are now distinguishable by the type,
    /// and the surface draws the honest empty window for the third.
    #[must_use]
    pub fn cut(self, now_ms: i64) -> WindowCut {
        let days = match self {
            RecallWindow::Today => 1,
            RecallWindow::Last7Days => 7,
            RecallWindow::Last30Days => 30,
            RecallWindow::AllTime => return WindowCut::Marker,
        };
        let start = now_ms.saturating_sub(DAY_MS.saturating_mul(days));
        if start < 0 {
            // Before the epoch: either the clock read `0` because the platform
            // refused it, or this machine believes it is January 1970. Neither
            // places a trailing window on the record's timeline.
            return WindowCut::NoClock;
        }
        WindowCut::Since(start)
    }
}

/// What a [`RecallWindow`] resolves to against one clock reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCut {
    /// No cut of its own: the family's boundary marker decides, which is what
    /// [`RecallWindow::AllTime`] has always meant.
    Marker,
    /// A trailing cut at this instant, to be composed with the marker.
    Since(i64),
    /// The clock could not place this trailing window. **Not "everything"** —
    /// absence, drawn as the empty window it is and labelled as such.
    NoClock,
}

/// The inner row the period buttons sit on, under the headline and the serve
/// count.
const RECALL_WINDOW_DY: u16 = 2;

/// The first inner row the per-channel call chart may use.
pub(super) const RECALL_CHART_DY: u16 = RECALL_WINDOW_DY + 1;

/// One period button on the Recall card: which window it selects and the exact
/// cells it occupies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecallWindowButton {
    /// The period a press selects.
    pub window: RecallWindow,
    /// Its exact clickable rectangle.
    pub rect: Rect,
}

/// The Recall card's period buttons — `[24h][7d][30d][all]` — solved once for
/// both the draw and the hit-test, so the cells that look pressable are the
/// cells that answer a press.
///
/// Chips **abut**, the rail toolbar's rule: a gap would clip the rightmost
/// button first, and losing "all" — the way back to the widest window — is the
/// worst one to lose.
///
/// Empty, and therefore no buttons anywhere, in the collapsed/narrow tier (the
/// card is already shedding text there) or when the row would not fit. The keys stay bound in both cases; nothing
/// here is the only door to a period.
#[must_use]
pub fn recall_window_buttons(collapsed: bool, inner: Rect) -> Vec<RecallWindowButton> {
    if collapsed || inner.w < 28 || inner.h < 7 {
        return Vec::new();
    }
    let mut x = inner.x;
    let right = u32::from(inner.x) + u32::from(inner.w);
    let mut out = Vec::with_capacity(RecallWindow::ALL.len());
    for window in RecallWindow::ALL {
        let Ok(w) = u16::try_from(window.label().chars().count().saturating_add(2)) else {
            break;
        };
        if u32::from(x) + u32::from(w) > right {
            break;
        }
        out.push(RecallWindowButton {
            window,
            rect: Rect {
                x,
                y: inner.y + RECALL_WINDOW_DY,
                w,
                h: 1,
            },
        });
        x = x.saturating_add(w);
    }
    // All four or none: a half-drawn row of periods reads as "these are the
    // periods", and a viewer would never learn the ones that did not fit.
    if out.len() == RecallWindow::ALL.len() {
        out
    } else {
        Vec::new()
    }
}

/// Draw the period row from [`recall_window_buttons`]. The selected period is
/// drawn in the key-hint style the rest of the dashboard uses for a live
/// affordance; the others are dim, so which window the figures cover is
/// readable without counting brackets.
pub(super) fn draw_recall_window_row(
    col3: &Col3,
    theme: Theme,
    collapsed: bool,
    inner: Rect,
    frame: &mut Frame,
) {
    for button in recall_window_buttons(collapsed, inner) {
        let selected = button.window == col3.recall_window;
        let style = if selected {
            theme.key_hint()
        } else {
            theme.dim()
        };
        frame.put_str(
            button.rect.x,
            button.rect.y,
            &format!("[{}]", button.window.label()),
            style,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities};
    use crate::theme::Section;

    use super::super::fixtures::usd_col3;
    use super::super::{draw_one, panel_frame, SectionId};

    /// The windows are trailing cuts, ordered, and the default is the one
    /// that changes nothing.
    ///
    /// The default matters most: `AllTime` yields **no cut at all**, so the
    /// family's boundary marker still decides the period and a dashboard
    /// nobody has clicked draws exactly what it drew before these buttons
    /// existed.
    #[test]
    fn the_recall_windows_are_ordered_trailing_cuts_over_an_uncut_default() {
        let now = 1_784_505_600_000i64;
        assert_eq!(RecallWindow::default(), RecallWindow::AllTime);
        assert_eq!(RecallWindow::AllTime.cut(now), WindowCut::Marker);

        let cuts: Vec<i64> = [
            RecallWindow::Today,
            RecallWindow::Last7Days,
            RecallWindow::Last30Days,
        ]
        .into_iter()
        .map(|w| match w.cut(now) {
            WindowCut::Since(ms) => ms,
            other => panic!("a narrow window names its own cut: {other:?}"),
        })
        .collect();
        assert_eq!(
            cuts,
            vec![now - DAY_MS, now - 7 * DAY_MS, now - 30 * DAY_MS]
        );
        assert!(
            cuts.windows(2).all(|pair| pair[0] > pair[1]),
            "a wider window must start earlier: {cuts:?}"
        );

        // Button order is narrowest-first and every label is distinct — four
        // chips on one row that shared a glyph would be four doors a viewer
        // cannot tell apart.
        let labels: Vec<&str> = RecallWindow::ALL.iter().map(|w| w.label()).collect();
        assert_eq!(labels, vec!["24h", "7d", "30d", "all"]);
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len());
    }

    /// A clock a trailing window cannot be placed against is **not**
    /// "everything".
    ///
    /// The bus answers `0` when the platform refuses the wall clock, and the
    /// old arithmetic clamped `0 − 24h` to the epoch — the whole record, drawn
    /// under a button that says `24h`, with every figure on the card claiming
    /// to be a day old. The window now says it cannot be placed, and the card
    /// draws the empty window and names the reason.
    #[test]
    fn a_window_the_clock_cannot_place_is_absence_and_never_the_whole_record() {
        for window in [
            RecallWindow::Today,
            RecallWindow::Last7Days,
            RecallWindow::Last30Days,
        ] {
            assert_eq!(window.cut(0), WindowCut::NoClock, "{window:?}");
            assert_eq!(window.cut(1_000), WindowCut::NoClock, "{window:?}");
            assert_eq!(window.cut(i64::MIN), WindowCut::NoClock, "{window:?}");
            // A real clock still places it.
            assert!(matches!(
                window.cut(1_784_505_600_000),
                WindowCut::Since(ms) if ms > 0
            ));
        }
        // `all` never needs a clock: the marker decides it.
        assert_eq!(RecallWindow::AllTime.cut(0), WindowCut::Marker);
    }

    /// The period buttons' glyphs land exactly on the cells the shared solver
    /// reports, and the selected one is the one drawn as live.
    ///
    /// Pixels == target: `hit_test` recomputes these rects from this same
    /// function, so a drift between them would put a viewer's press on the
    /// wrong period — the failure the one-solver rule exists to make
    /// impossible.
    #[test]
    fn window_button_glyphs_land_on_the_solver_rects() {
        let col3 = Col3 {
            recall_window: RecallWindow::Last7Days,
            ..usd_col3(None)
        };
        let rect = Rect {
            x: 0,
            y: 0,
            w: 40,
            h: 10,
        };
        let mut f = Frame::new(Size::new(40, 10));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_one(
            &col3,
            SectionId::Recall,
            false,
            false,
            false,
            rect,
            &mut b,
            &mut f,
        );
        let mut scratch = Frame::new(Size::new(40, 10));
        let inner = panel_frame(
            Theme::rift_dark(),
            Section::Recall,
            false,
            "recall",
            rect,
            &mut scratch,
        );
        let buttons = recall_window_buttons(false, inner);
        assert_eq!(buttons.len(), 4, "all four periods or none");
        for button in &buttons {
            let drawn: String = (button.rect.x..button.rect.x + button.rect.w)
                .map(|x| f.get(x, button.rect.y).map_or(' ', |c| c.ch))
                .collect();
            assert_eq!(drawn, format!("[{}]", button.window.label()));
        }
        // Abutting, in narrowest-first order, all on one row.
        assert!(buttons
            .windows(2)
            .all(|p| p[0].rect.x + p[0].rect.w == p[1].rect.x));
        assert!(buttons.iter().all(|b| b.rect.y == buttons[0].rect.y));
        // Nothing else shares the row: past `[all]` the period row is blank
        // to the panel's edge.
        let last = buttons.last().expect("four buttons");
        let tail: String = (last.rect.x + last.rect.w..inner.x + inner.w)
            .map(|x| f.get(x, last.rect.y).map_or(' ', |c| c.ch))
            .collect();
        assert!(
            tail.trim().is_empty(),
            "the period row carries only periods: {tail:?}"
        );
        assert_eq!(
            buttons.iter().map(|b| b.window).collect::<Vec<_>>(),
            RecallWindow::ALL.to_vec()
        );
        // The selected period is the only one drawn in the live style — the
        // card has to say which window its figures cover.
        let live: Vec<RecallWindow> = buttons
            .iter()
            .filter(|b| {
                f.get(b.rect.x + 1, b.rect.y)
                    .is_some_and(|c| c.style == Theme::rift_dark().key_hint())
            })
            .map(|b| b.window)
            .collect();
        assert_eq!(live, vec![RecallWindow::Last7Days]);
    }

    /// The degraded tiers draw no period row, and they draw **none of it**:
    /// a half-row of buttons reads as "these are the periods", and a viewer
    /// would never learn about the ones that did not fit.
    #[test]
    fn the_period_row_is_all_four_buttons_or_nothing() {
        let inner = |w: u16, h: u16| Rect { x: 1, y: 1, w, h };
        assert!(
            recall_window_buttons(true, inner(38, 8)).is_empty(),
            "collapsed"
        );
        assert!(
            recall_window_buttons(false, inner(27, 8)).is_empty(),
            "narrow"
        );
        assert!(
            recall_window_buttons(false, inner(38, 6)).is_empty(),
            "short"
        );
        for w in 28..80u16 {
            let solved = recall_window_buttons(false, inner(w, 8));
            assert!(
                solved.is_empty() || solved.len() == RecallWindow::ALL.len(),
                "a partial period row at w={w}: {solved:?}"
            );
            // Whatever is drawn stays inside the panel.
            for button in &solved {
                assert!(u32::from(button.rect.x) + u32::from(button.rect.w) <= u32::from(1 + w));
            }
        }
    }
}
