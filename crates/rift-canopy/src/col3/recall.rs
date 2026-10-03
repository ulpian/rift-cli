//! The Recall panel: the savings headline, the placeholder states, and the
//! frame that seats the period row, the bars and the call chart below it.

use crate::layout::Rect;
use crate::render::frame::{Frame, Style};
use crate::render::RenderBackend;
use crate::theme::{Section, Theme};

use super::calls::draw_call_chart;
use super::format::{f64_of, fmt_km};
use super::recall_bars::draw_recall_bars;
use super::recall_period::{draw_recall_window_row, RecallWindow, RECALL_CHART_DY};
use super::system::{active_sessions_label, ActiveSessionsView};
use super::{body, panel_frame, Col3};

/// A recall-derived section: the three honest states.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum RecallView<T> {
    /// Recall is not enabled for this project — prompt, never a zero line.
    #[default]
    Disabled,
    /// Enabled but no served events yet — visibly distinct from Disabled.
    Quiet,
    /// Live data.
    Data(T),
}

/// The headline's segments are joined by this, a middle dot between spaces:
/// three display cells, four bytes, which is why every width here is counted
/// in characters.
const HEADLINE_SEPARATOR: &str = " · ";

pub(super) fn draw_recall_panel(
    col3: &Col3,
    theme: Theme,
    focused: bool,
    unicode: bool,
    collapsed: bool,
    rect: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let inner = panel_frame(theme, Section::Recall, focused, "recall", rect, frame);
    // The period row is drawn BEFORE the placeholder early-return, and that
    // ordering is load-bearing: a narrow window over a quiet ledger puts the
    // card *into* the placeholder state, and buttons that vanished there would
    // leave the only way back on the keyboard.
    draw_recall_window_row(col3, theme, collapsed, inner, frame);
    if let Some(sentence) = recall_placeholder(col3) {
        body(frame, inner, 0, sentence, theme.dim());
        return;
    }
    let (_, call_total, sessions) = recall_headlines(col3);
    if collapsed || inner.w < 28 || inner.h < 7 {
        draw_recall_headline(col3, inner, theme.body(), frame);
        draw_recall_serves(theme, inner, call_total, &sessions, frame);
        return;
    }
    draw_recall_headline(col3, inner, theme.tile_value(theme.graph_accent()), frame);
    body(
        frame,
        inner,
        1,
        &format!("{call_total} serves · {sessions}"),
        theme.body(),
    );
    let bar_rows = draw_recall_bars(col3, theme, unicode, collapsed, inner, backend, frame);
    let chart_dy = RECALL_CHART_DY.saturating_add(bar_rows);
    draw_recall_calls(col3, theme, unicode, inner, chart_dy, backend, frame);
}

/// Draw the headline on the cells [`recall_headline_rect`] solves, so the
/// glyphs and the click target cannot drift apart. Flashless by design:
/// col3 carries no press-pulse plumbing, by a recorded decision.
fn draw_recall_headline(col3: &Col3, inner: Rect, style: Style, frame: &mut Frame) {
    let Some((text, door)) = recall_headline_cells(col3, inner) else {
        return;
    };
    frame.put_str(door.x, door.y, &text, style);
}

/// The collapsed and narrow tiers' serve count, with the session phrase on a
/// row of its own when the two will not share one.
fn draw_recall_serves(
    theme: Theme,
    inner: Rect,
    call_total: u64,
    sessions: &str,
    frame: &mut Frame,
) {
    if inner.w >= 30 {
        body(
            frame,
            inner,
            1,
            &format!("{call_total} serves · {sessions}"),
            theme.body(),
        );
        return;
    }
    body(
        frame,
        inner,
        1,
        &format!("{call_total} serves"),
        theme.body(),
    );
    body(frame, inner, 2, sessions, theme.body());
}

/// The per-channel call chart from `chart_dy` down, or the sentence that says
/// why there is none: no attributed serves at all, or none in this window.
fn draw_recall_calls(
    col3: &Col3,
    theme: Theme,
    unicode: bool,
    inner: Rect,
    chart_dy: u16,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let RecallView::Data(calls) = &col3.calls else {
        body(
            frame,
            inner,
            chart_dy.saturating_add(1),
            "(no attributed serves yet)",
            theme.dim(),
        );
        return;
    };
    if calls.series.is_empty() {
        body(
            frame,
            inner,
            chart_dy.saturating_add(1),
            "(no calls in window)",
            theme.dim(),
        );
        return;
    }
    let chart = Rect {
        x: inner.x,
        y: inner.y + chart_dy,
        w: inner.w,
        h: inner.h.saturating_sub(chart_dy),
    };
    draw_call_chart(calls, theme, unicode, chart, backend, frame);
}

/// The sentence a card with nothing to measure draws in place of its
/// headline, or `None` when there is a headline to draw.
fn recall_placeholder(col3: &Col3) -> Option<&'static str> {
    if matches!(col3.savings, RecallView::Disabled) && matches!(col3.calls, RecallView::Disabled) {
        return Some("recall off — open Project Settings");
    }
    if !(matches!(col3.savings, RecallView::Quiet) && matches!(col3.calls, RecallView::Quiet)) {
        return None;
    }
    // Which emptiness, said out loud. Over the whole record an empty ledger
    // means the plane has never served here and `doctor` is the next step;
    // over a narrow period it means this period is empty, and sending an
    // operator to a diagnostic for that would be the card reporting a fault
    // where there is none. The narrow sentence also covers the window a
    // broken clock cannot place: nothing is in a window that could not be
    // placed, which is exactly what it says.
    match col3.recall_window {
        RecallWindow::AllTime => Some("enabled, no serves yet — see `rift doctor`"),
        RecallWindow::Today | RecallWindow::Last7Days | RecallWindow::Last30Days => {
            Some("no serves in this period — try [all]")
        }
    }
}

/// The Recall headline's exact cells: its first body row, from the inner
/// rect's left edge for as many cells as the line draws once the frame has
/// clipped it.
///
/// **One solver** feeds the draw and the hit-test in every tier — wide,
/// narrow and collapsed print the same line, clipped by the frame like any
/// body line — so the cells that read as the headline are the cells that
/// open the savings breakdown, the click twin of `S`. A door that only
/// existed beside a dollar figure would vanish on every repository with no
/// seeded price; the line itself is always there when there is a line.
///
/// `None` — and therefore no door — when the card draws a placeholder
/// sentence instead of a headline, or when the inner rect has no cell.
#[must_use]
pub fn recall_headline_rect(col3: &Col3, inner: Rect) -> Option<Rect> {
    recall_headline_cells(col3, inner).map(|(_, door)| door)
}

/// The fitted headline text and the cells it occupies — the measurement
/// both [`recall_headline_rect`] and the draw read.
fn recall_headline_cells(col3: &Col3, inner: Rect) -> Option<(String, Rect)> {
    if inner.w == 0 || inner.h == 0 || recall_placeholder(col3).is_some() {
        return None;
    }
    let fitted = fit_headline(&recall_headline_segments(col3), usize::from(inner.w));
    let w = u16::try_from(fitted.chars().count()).ok()?;
    let door = Rect {
        x: inner.x,
        y: inner.y,
        w,
        h: 1,
    };
    Some((fitted, door))
}

/// The widest whole-segment prefix of the headline that fits `width` cells,
/// shedding from the right — the dollars first, then the share — so no tier
/// draws half a figure. Only when even `saved N tokens` does not fit does the
/// line fall back to the rule every other body line on the card follows
/// ([`body`]): cut at the frame, one cell per character.
fn fit_headline(segments: &[String], width: usize) -> String {
    for n in (1..=segments.len()).rev() {
        let Some(kept) = segments.get(..n) else {
            continue;
        };
        let line = kept.join(HEADLINE_SEPARATOR);
        if line.chars().count() <= width {
            return line;
        }
    }
    let first = segments.first().map_or("", String::as_str);
    first.chars().take(width).collect()
}

/// The card's headline, its serve count and its active-session phrase.
///
/// The headline is `saved N tokens`, then the shared savings share, then the
/// dollar estimate, joined by middle dots — each segment present only when
/// its figure is. Tokens lead because they are the measurement; the share is
/// out of the one `savings::compose` call the command line reads from, and
/// the dollars are the money authority's pre-rendered label, tilde and all.
///
/// One decimal on the share, because that is how `rift savings` prints it: a
/// card that rounded to whole percent would disagree with the terminal beside
/// it on a repository sitting at 2.5 %. An unanswerable share draws no
/// percentage, never a zero — nothing read and nothing avoided is a question
/// with no answer — and a card with no savings frame at all is `saved —`.
fn recall_headlines(col3: &Col3) -> (String, u64, String) {
    let savings = recall_headline_segments(col3).join(HEADLINE_SEPARATOR);
    let call_total = match &col3.calls {
        RecallView::Data(calls) => calls.total,
        RecallView::Quiet | RecallView::Disabled => 0,
    };
    let sessions = match &col3.active_sessions {
        ActiveSessionsView::Count(count) => active_sessions_label(*count),
        ActiveSessionsView::NoSessions => active_sessions_label(0),
    };
    (savings, call_total, sessions)
}

/// The headline's segments in reading order, each present only when its
/// figure is: the tokens avoided, the share, the dollar estimate.
fn recall_headline_segments(col3: &Col3) -> Vec<String> {
    let RecallView::Data(sd) = &col3.savings else {
        return vec!["saved —".to_owned()];
    };
    let tokens = format!("saved {} tokens", fmt_km(f64_of(sd.share.avoided_tokens)));
    let share = sd.share.overall.map(|pct| format!("{pct:.1}%"));
    std::iter::once(tokens)
        .chain(share)
        .chain(sd.usd_label.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities};

    use super::super::calls::{CallsData, ChannelCallSeries};
    use super::super::fixtures::{draw_to, share_fixture, text_of, usd_col3};
    use super::super::savings::{SavingsData, ShareData};
    use super::super::{draw_one, SectionId};

    /// The priced fixture's line, every segment present.
    const PRICED: &str = "saved 6.0k tokens · 42.0% · ~$105.90";

    /// Draw the Recall card into a `w`×`h` frame and return it with the inner
    /// rect the panel chrome granted, recomputed through the same solver.
    fn drawn_card(col3: &Col3, w: u16, h: u16, collapsed: bool) -> (Frame, Rect) {
        let rect = Rect { x: 0, y: 0, w, h };
        let mut f = Frame::new(Size::new(w, h));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_one(
            col3,
            SectionId::Recall,
            false,
            false,
            collapsed,
            rect,
            &mut b,
            &mut f,
        );
        let mut scratch = Frame::new(Size::new(w, h));
        let inner = panel_frame(
            Theme::rift_dark(),
            Section::Recall,
            false,
            "recall",
            rect,
            &mut scratch,
        );
        (f, inner)
    }

    /// The glyphs a frame holds on `rect`'s first row, cell by cell.
    fn cells(f: &Frame, rect: Rect) -> String {
        (rect.x..rect.x + rect.w)
            .map(|x| f.get(x, rect.y).map_or(' ', |c| c.ch))
            .collect()
    }

    #[test]
    fn recall_panel_states_render_honestly() {
        // Disabled / Quiet / Data (savings % + calls + threads).
        let disabled = text_of(&draw_to(&Col3::default(), 40, 24));
        assert!(disabled.contains("recall off"));

        let data = Col3 {
            savings: RecallView::Data(SavingsData {
                share: Box::new(ShareData {
                    overall: Some(42.0),
                    avoided_tokens: 6_000,
                    read_tokens: 8_285,
                    ..ShareData::default()
                }),
                ..SavingsData::default()
            }),
            calls: RecallView::Data(CallsData {
                total: 17,
                window_end_ms: 1_800_000,
                series: vec![ChannelCallSeries {
                    channel: "hook_inject".into(),
                    last_call_at: 1_799_000,
                    rate: vec![1.0; 30],
                }],
            }),
            active_sessions: ActiveSessionsView::Count(3),
            ..Col3::default()
        };
        let text = text_of(&draw_to(&data, 40, 24));
        assert!(
            text.contains("saved 6.0k tokens · 42.0%"),
            "the tokens avoided and the shared share: \n{text}"
        );
        assert!(text.contains("17 serves"));
        assert!(text.contains("hook"), "channel legend name: \n{text}");
        assert!(text.contains("3 active sessions"));
    }

    /// A priced window says all three things on one line, tokens first,
    /// because the tokens are the measurement and the other two are read off
    /// it.
    #[test]
    fn a_priced_window_draws_tokens_share_and_dollars_on_one_line() {
        let col3 = usd_col3(Some("~$105.90"));
        assert_eq!(recall_headlines(&col3).0, PRICED);
    }

    /// No seeded price means no dollar segment — never `~$0.00`, which would
    /// be a figure nobody computed.
    #[test]
    fn an_unpriced_window_draws_two_segments() {
        let col3 = usd_col3(None);
        assert_eq!(recall_headlines(&col3).0, "saved 6.0k tokens · 42.0%");
        let text = text_of(&draw_to(&col3, 64, 24));
        let headline = text
            .lines()
            .find(|line| line.contains("saved 6.0k tokens"))
            .expect("the recall headline is drawn");
        assert!(
            !headline.contains('$'),
            "no money without a figure: {headline}"
        );
        assert!(
            !headline.contains('['),
            "the headline carries no chip: {headline}"
        );
    }

    /// An unanswerable share draws no percentage — never `0 %`, because the
    /// two mean opposite things and only one of them is a measurement — but
    /// the tokens it avoided are still a count, and the line is still the
    /// door that explains the absence.
    #[test]
    fn an_unanswerable_share_draws_no_percentage_but_keeps_its_door() {
        let col3 = Col3 {
            savings: RecallView::Data(share_fixture(None)),
            ..Col3::default()
        };
        assert_eq!(recall_headlines(&col3).0, "saved 10k tokens");
        let (f, inner) = drawn_card(&col3, 60, 13, false);
        let door = recall_headline_rect(&col3, inner).expect("a drawn headline is a door");
        assert_eq!(cells(&f, door), "saved 10k tokens");

        let answered = Col3 {
            savings: RecallView::Data(share_fixture(Some(3.04))),
            ..Col3::default()
        };
        assert_eq!(
            recall_headlines(&answered).0,
            "saved 10k tokens · 3.0%",
            "the shared share, one decimal like the command line"
        );
    }

    /// The headline's door is exactly the cells its glyphs occupy, in the
    /// wide, narrow and collapsed tiers alike: one solver feeds the draw and
    /// the hit-test, so a click lands where the line is and nowhere else.
    ///
    /// Measured in cells, not bytes: the middle dot is two bytes and one
    /// cell, and a door sized by byte length would run past the line.
    #[test]
    fn the_headline_draws_exactly_where_the_shared_solver_says() {
        let col3 = usd_col3(Some("~$105.90"));
        for (w, h, collapsed) in [(64, 13, false), (26, 13, false), (30, 1, true)] {
            let (f, inner) = drawn_card(&col3, w, h, collapsed);
            let door = recall_headline_rect(&col3, inner).expect("a drawn headline is a door");
            let fitted = fit_headline(&recall_headline_segments(&col3), usize::from(inner.w));
            assert_eq!(cells(&f, door), fitted, "w={w} collapsed={collapsed}");
            assert_eq!((door.x, door.y, door.h), (inner.x, inner.y, 1));
            let end = door.x + door.w;
            if end < inner.x + inner.w {
                assert_eq!(f.get(end, door.y).map(|c| c.ch), Some(' '), "w={w}");
            }
        }
        let (_, wide) = drawn_card(&col3, 64, 13, false);
        let door = recall_headline_rect(&col3, wide).expect("the wide door");
        assert_eq!(usize::from(door.w), PRICED.chars().count());
        assert!(usize::from(door.w) < PRICED.len(), "cells, never bytes");
    }

    /// A headline that does not fit sheds whole segments from the right —
    /// the dollars, then the share — so no tier ever draws half a figure.
    #[test]
    fn a_narrow_headline_sheds_whole_segments_and_never_cuts_a_figure() {
        let col3 = usd_col3(Some("~$105.90"));
        let prefixes = ["saved 6.0k tokens", "saved 6.0k tokens · 42.0%", PRICED];
        for (w, h, collapsed) in [
            (64, 13, false),
            (40, 13, false),
            (30, 13, false),
            (26, 13, false),
            (30, 1, true),
            (20, 1, true),
        ] {
            let (f, inner) = drawn_card(&col3, w, h, collapsed);
            let door = recall_headline_rect(&col3, inner).expect("a drawn headline is a door");
            let drawn = cells(&f, door);
            assert!(
                prefixes.contains(&drawn.as_str()),
                "w={w} collapsed={collapsed} drew a cut figure: {drawn:?}"
            );
            let longest = prefixes
                .iter()
                .rev()
                .find(|p| p.chars().count() <= usize::from(inner.w))
                .expect("the tokens segment fits every tier here");
            assert_eq!(&drawn, longest, "the widest whole prefix that fits, w={w}");
        }
        // Too narrow even for the tokens: the card's body-line clip applies.
        let (f, inner) = drawn_card(&col3, 12, 1, true);
        let door = recall_headline_rect(&col3, inner).expect("a clipped headline is a door");
        assert_eq!(cells(&f, door), "saved 6.0k t");
    }

    /// A placeholder card draws a sentence, not a headline, so it has no
    /// headline door; `S` remains the way in, and it always answers.
    #[test]
    fn a_placeholder_card_draws_no_headline_and_offers_no_door() {
        for (savings, calls) in [
            (RecallView::Quiet, RecallView::Quiet),
            (RecallView::Disabled, RecallView::Disabled),
        ] {
            let col3 = Col3 {
                savings,
                calls,
                ..Col3::default()
            };
            for (w, h, collapsed) in [(64, 13, false), (26, 13, false), (30, 1, true)] {
                let (f, inner) = drawn_card(&col3, w, h, collapsed);
                assert_eq!(recall_headline_rect(&col3, inner), None, "w={w}");
                assert!(!cells(&f, inner).contains("saved"), "w={w}");
            }
        }
    }

    /// A narrow window over a quiet ledger puts the card into its placeholder
    /// state — and that is exactly when the way back must still be on screen.
    ///
    /// Drawn before the placeholder's early return for that reason. Without
    /// it a viewer who asked for the last 24 hours of a repository that served
    /// last week would be looking at "enabled, no serves yet" with no visible
    /// way to widen the window again.
    #[test]
    fn the_period_row_survives_the_placeholder_states() {
        for (savings, calls) in [
            (RecallView::Quiet, RecallView::Quiet),
            (RecallView::Disabled, RecallView::Disabled),
        ] {
            let col3 = Col3 {
                savings,
                calls,
                recall_window: RecallWindow::Today,
                ..Col3::default()
            };
            let (f, _) = drawn_card(&col3, 40, 10, false);
            let text = text_of(&f);
            for window in RecallWindow::ALL {
                assert!(
                    text.contains(&format!("[{}]", window.label())),
                    "the {} button vanished behind the placeholder:\n{text}",
                    window.label()
                );
            }
        }
    }
}
