//! The Recall panel: the savings headline with its two chips, the placeholder
//! states, and the frame that seats the period row, the bars and the call
//! chart below it.

use crate::layout::Rect;
use crate::render::frame::Frame;
use crate::render::RenderBackend;
use crate::theme::{Section, Theme};

use super::calls::draw_call_chart;
use super::format::{f64_of, fmt_km};
use super::recall_bars::{draw_recall_bars, recall_usd_available, RecallUnit};
use super::recall_period::{draw_recall_window_row, RecallWindow, RECALL_CHART_DY};
use super::savings::SavingsData;
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
    let inner = panel_frame(
        theme,
        Section::Recall,
        focused,
        recall_title(col3),
        rect,
        frame,
    );
    // The period row is drawn BEFORE the placeholder early-return, and that
    // ordering is load-bearing: a narrow window over a quiet ledger puts the
    // card *into* the placeholder state, and buttons that vanished there would
    // leave the only way back on the keyboard.
    draw_recall_window_row(col3, theme, collapsed, inner, frame);
    if draw_recall_placeholder(col3, theme, inner, frame) {
        return;
    }
    let (savings, call_total, sessions) = recall_headlines(col3);
    if collapsed || inner.w < 28 || inner.h < 7 {
        draw_recall_narrow(theme, inner, &savings, call_total, &sessions, frame);
        return;
    }
    body(
        frame,
        inner,
        0,
        &savings,
        theme.tile_value(theme.graph_accent()),
    );
    draw_recall_chips(col3, theme, collapsed, inner, frame);
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

/// The collapsed and narrow tiers: the headline and the serve count, with the
/// session phrase on a row of its own when the two will not share one.
fn draw_recall_narrow(
    theme: Theme,
    inner: Rect,
    savings: &str,
    call_total: u64,
    sessions: &str,
    frame: &mut Frame,
) {
    body(frame, inner, 0, savings, theme.body());
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

/// The two chips on the headline row: the USD estimate and the savings-share
/// target, each drawn from the same solver the hit-test reads, so pixels ==
/// target. An absent label draws no chip at all (honest absence, never a
/// fabricated figure). Flashless by design: col3 carries no press-pulse
/// plumbing, by a recorded decision.
fn draw_recall_chips(col3: &Col3, theme: Theme, collapsed: bool, inner: Rect, frame: &mut Frame) {
    let RecallView::Data(sd) = &col3.savings else {
        return;
    };
    let cost = recall_cost_rect(col3, collapsed, inner);
    if let (Some(chip), Some(label)) = (cost, sd.usd_label.as_deref()) {
        frame.put_str(chip.x, chip.y, &format!("[{label}]"), theme.key_hint());
    }
    let share = recall_share_rect(col3, collapsed, inner);
    if let (Some(chip), Some(label)) = (share, share_chip_label(sd)) {
        frame.put_str(chip.x, chip.y, &label, theme.key_hint());
    }
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

/// The Recall panel's title, carrying the reason the dollar view was refused
/// when it was.
///
/// **Byte-identical to the bare title whenever the card is showing what was
/// asked for**, the activity card's off-strip precedent, so every golden that
/// does not exercise the refusal is unchanged. It goes in the title because
/// the card's rows are all spoken for and a reason that only appears when the
/// panel is tall enough is a reason the reader cannot rely on.
pub(super) fn recall_title(col3: &Col3) -> &'static str {
    if col3.recall_unit == RecallUnit::Usd && !recall_usd_available(col3) {
        "recall · no seeded price"
    } else {
        "recall"
    }
}

fn draw_recall_placeholder(col3: &Col3, theme: Theme, inner: Rect, frame: &mut Frame) -> bool {
    if matches!(col3.savings, RecallView::Disabled) && matches!(col3.calls, RecallView::Disabled) {
        body(
            frame,
            inner,
            0,
            "recall off — open Project Settings",
            theme.dim(),
        );
        return true;
    }
    if matches!(col3.savings, RecallView::Quiet) && matches!(col3.calls, RecallView::Quiet) {
        // Which emptiness, said out loud. Over the whole record an empty
        // ledger means the plane has never served here and `doctor` is the
        // next step; over a narrow period it means this period is empty, and
        // sending an operator to a diagnostic for that would be the card
        // reporting a fault where there is none. The narrow sentence also
        // covers the window a broken clock cannot place: nothing is in a
        // window that could not be placed, which is exactly what it says.
        let sentence = if col3.recall_window == RecallWindow::AllTime {
            "enabled, no serves yet — see `rift doctor`"
        } else {
            "no serves in this period — try [all]"
        };
        body(frame, inner, 0, sentence, theme.dim());
        return true;
    }
    false
}

/// The clickable USD-savings chip on the Recall headline row:
/// `[~$105.90]`, two cells after the `saved …` text. **One solver** feeds the
/// draw and the hit-test, so the glyphs and the click target are always the
/// same cells (`hit_test` recomputes geometry; a second solver would put
/// clicks on the wrong cells).
///
/// `None` — and therefore no chip anywhere — when there is no honest figure
/// (`usd_label` absent, the priced savings estimate rung 4), in the collapsed/narrow path
/// (the panel is already shedding text), or when the whole chip cannot fit
/// on the headline row.
#[must_use]
pub fn recall_cost_rect(col3: &Col3, collapsed: bool, inner: Rect) -> Option<Rect> {
    if collapsed || inner.w < 28 || inner.h < 7 {
        return None;
    }
    let RecallView::Data(sd) = &col3.savings else {
        return None;
    };
    let label = sd.usd_label.as_deref()?;
    let (savings, _, _) = recall_headlines(col3);
    let headline = u16::try_from(savings.chars().count()).ok()?;
    let w = u16::try_from(label.chars().count().checked_add(2)?).ok()?;
    let x = inner.x.checked_add(headline)?.checked_add(2)?;
    (u32::from(x) + u32::from(w) <= u32::from(inner.x) + u32::from(inner.w)).then_some(Rect {
        x,
        y: inner.y,
        w,
        h: 1,
    })
}

/// The target chip on the Recall headline row: `[of 90% target]`, two cells
/// after the USD chip when there is one and after the headline text when there
/// is not.
///
/// It says what the headline beside it is aiming at — the product's stated
/// savings target, from the ledger's one constant — so a reader can size a
/// small share against the goal instead of against nothing. It is a target and
/// never a measurement, which is why it carries no tilde and no `n`.
///
/// **One solver** feeds the draw and the hit-test, exactly as the USD chip
/// does, so the glyphs and the click target are always the same cells. It
/// opens the same breakdown modal the USD chip opens — the same act through a
/// second door, so the card gains no act and no new modal surface. That
/// matters for more than tidiness: the USD chip disappears whenever no seeded
/// price applies, and without this door the per-family share rows would be
/// unreachable by mouse on exactly the repositories that have no price
/// catalogue. Both doors have a key twin (`S`, hinted while the graphs column
/// has focus), so neither direction of the parity rule rests on the other
/// chip being drawn.
///
/// `None` — and therefore no chip — when the card carries no target, in the
/// collapsed/narrow path, or when the whole chip cannot fit on the row.
#[must_use]
pub fn recall_share_rect(col3: &Col3, collapsed: bool, inner: Rect) -> Option<Rect> {
    if collapsed || inner.w < 28 || inner.h < 7 {
        return None;
    }
    let RecallView::Data(sd) = &col3.savings else {
        return None;
    };
    let label = share_chip_label(sd)?;
    let (headline, _, _) = recall_headlines(col3);
    let mut x = u32::from(inner.x) + headline.chars().count() as u32 + 2;
    if let Some(cost) = recall_cost_rect(col3, collapsed, inner) {
        x = u32::from(cost.x) + u32::from(cost.w) + 1;
    }
    let w = u32::try_from(label.chars().count()).ok()?;
    let x = u16::try_from(x).ok()?;
    let w = u16::try_from(w).ok()?;
    (u32::from(x) + u32::from(w) <= u32::from(inner.x) + u32::from(inner.w)).then_some(Rect {
        x,
        y: inner.y,
        w,
        h: 1,
    })
}

/// The target chip's glyphs, or `None` when this card carries no target.
///
/// "of N% target" rather than a bare percentage, because the row already holds
/// one percentage and two of them unlabelled is how a reader ends up quoting
/// the wrong figure.
#[must_use]
pub fn share_chip_label(sd: &SavingsData) -> Option<String> {
    let target = sd.target_pct?;
    Some(format!("[of {target}% target]"))
}

/// The card's headline, its serve count and its active-session phrase.
///
/// The headline is **the shared savings share** and the token count behind it,
/// both out of the one `savings::compose` call the command line reads from. It
/// used to be a second percentage — avoided over avoided plus returned — which
/// is a different question with the same shape, drawn two cells from a chip
/// that answered the other one. Its denominator is named where there is room
/// to name it: the two bars under it are literally `read` and `saved`, and the
/// modal spells the fraction out.
///
/// One decimal, because that is how `rift savings` prints it: a card that
/// rounded to whole percent would disagree with the terminal beside it on a
/// repository sitting at 2.5 %. An unanswerable share is `saved —`, never a
/// zero — nothing read and nothing avoided is a question with no answer.
///
/// It is kept **short** on purpose. Everything else on this row — the dollar
/// estimate, the target — is positioned after it and shed when it will not
/// fit, so a longer headline is paid for by whatever stands to its right at
/// the narrow widths this column is usually drawn at.
fn recall_headlines(col3: &Col3) -> (String, u64, String) {
    let savings = match &col3.savings {
        RecallView::Data(sd) => match sd.share.overall {
            Some(pct) => format!(
                "saved {pct:.1}% ({})",
                fmt_km(f64_of(sd.share.avoided_tokens))
            ),
            None => "saved —".to_string(),
        },
        RecallView::Quiet | RecallView::Disabled => "saved —".to_string(),
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities};

    use super::super::calls::{CallsData, ChannelCallSeries};
    use super::super::fixtures::{barred_col3, draw_to, share_fixture, text_of, usd_col3};
    use super::super::savings::ShareData;
    use super::super::{draw_one, SectionId};

    #[test]
    fn recall_panel_states_render_honestly() {
        // Disabled / Quiet / Data (savings % + calls + threads).
        let disabled = text_of(&draw_to(&Col3::default(), 40, 24));
        assert!(disabled.contains("recall off"));

        let data = Col3 {
            savings: RecallView::Data(SavingsData {
                target_pct: Some(90),
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
            text.contains("saved 42.0% (6.0k)"),
            "the shared share and the tokens behind it: \n{text}"
        );
        assert!(text.contains("17 serves"));
        assert!(text.contains("hook"), "channel legend name: \n{text}");
        assert!(text.contains("3 active sessions"));
    }

    /// the priced savings estimate: the chip's glyphs land exactly on the cells the shared
    /// solver reports — pixels == target, one geometry for draw and hit-test.
    #[test]
    fn usd_chip_draws_exactly_where_the_shared_solver_says() {
        let col3 = usd_col3(Some("~$6.25"));
        let rect = Rect {
            x: 0,
            y: 0,
            w: 64,
            h: 10,
        };
        let mut f = Frame::new(Size::new(64, 10));
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
        // Recompute the inner rect through the same chrome solver the panel
        // used (geometry is theme-independent).
        let mut scratch = Frame::new(Size::new(64, 10));
        let inner = panel_frame(
            Theme::rift_dark(),
            Section::Recall,
            false,
            "recall",
            rect,
            &mut scratch,
        );
        let chip = recall_cost_rect(&col3, false, inner).expect("an honest figure draws a chip");
        // The drawn text at the chip's cells is exactly `[~$6.25]`.
        let drawn: String = (chip.x..chip.x + chip.w)
            .map(|x| f.get(x, chip.y).map_or(' ', |c| c.ch))
            .collect();
        assert_eq!(drawn, "[~$6.25]");
        assert!(drawn.contains('~'), "the estimate marker is mandatory");
        // And nowhere else: the row before the chip is the headline text.
        let row: String = (inner.x..inner.x + inner.w)
            .map(|x| f.get(x, chip.y).map_or(' ', |c| c.ch))
            .collect();
        // The headline row carries exactly two chips: the money and the
        // target beside it, in that order and never overlapping.
        assert_eq!(row.matches('[').count(), 2, "money and target: {row}");
        let target = recall_share_rect(&col3, false, inner).expect("the target chip");
        assert!(
            target.x >= chip.x + chip.w,
            "the chips abut without overlapping: {row}"
        );
    }

    /// Honest absence and degraded layouts draw no chip at all (never a
    /// fabricated figure, never a truncated one).
    #[test]
    fn usd_chip_absent_without_an_honest_figure_or_room() {
        // No figure → no chip on the headline row, which is the only row
        // either chip is ever drawn on. Scoped to that row rather than to the
        // whole panel: the period buttons own their own row of `[…]` chips,
        // and a panel-wide search for a bracket could no longer tell a
        // fabricated dollar figure from the row that lets a viewer change the
        // window.
        let none = usd_col3(None);
        let text = text_of(&draw_to(&none, 64, 24));
        let headline = text
            .lines()
            .find(|line| line.contains("saved 42.0% (6.0k)"))
            .expect("the recall headline is drawn");
        assert!(
            !headline.contains("~$"),
            "no money without a figure: {headline}"
        );
        assert_eq!(
            headline.matches('[').count(),
            1,
            "the target chip stands alone, and it is the modal's only door \
             on a repository with no seeded price: {headline}"
        );

        let some = usd_col3(Some("~$6.25"));
        let inner = Rect {
            x: 1,
            y: 1,
            w: 38,
            h: 8,
        };
        // Collapsed path: the panel is shedding text; the chip goes first.
        assert_eq!(recall_cost_rect(&some, true, inner), None);
        // Narrow / short inners: below the wide-path floor, no chip.
        assert_eq!(
            recall_cost_rect(
                &some,
                false,
                Rect {
                    x: 1,
                    y: 1,
                    w: 27,
                    h: 8
                }
            ),
            None
        );
        assert_eq!(
            recall_cost_rect(
                &some,
                false,
                Rect {
                    x: 1,
                    y: 1,
                    w: 38,
                    h: 6
                }
            ),
            None
        );
        // A row too tight for the whole chip refuses rather than truncates:
        // headline "saved 42% (6.0k)" = 16 cells + 2 gap + 8 chip = 26 < 28
        // fits at w=28... shrink until it cannot: chip needs x+w ≤ inner end.
        let tight = Rect {
            x: 1,
            y: 1,
            w: 28,
            h: 8,
        };
        // 16 + 2 = 18 offset, +8 = 26 ≤ 28 → fits; w=25 would fail the wide
        // floor first, so prove the fit guard with a longer label instead.
        let long = usd_col3(Some("~$1,234,567.89"));
        assert_eq!(
            recall_cost_rect(&long, false, tight),
            None,
            "a chip that cannot fit whole is not drawn at all"
        );
    }

    /// The title is byte-identical to the bare one whenever the card shows
    /// what was asked for — the activity card's off-strip rule, and what keeps
    /// every golden that does not exercise the refusal unchanged.
    #[test]
    fn the_recall_title_is_bare_until_the_dollar_view_is_refused() {
        let priced = {
            let mut col3 = barred_col3(10_000, 1_000);
            if let RecallView::Data(sd) = &mut col3.savings {
                sd.usd_read_micros = Some(1);
                sd.usd_read_label = Some("~<$0.01".to_owned());
                sd.usd_saved_micros = Some(1);
                sd.usd_label = Some("~<$0.01".to_owned());
            }
            col3
        };
        assert_eq!(recall_title(&Col3::default()), "recall");
        assert_eq!(recall_title(&barred_col3(10, 1)), "recall", "tokens asked");
        assert_eq!(recall_title(&priced), "recall", "dollars available");
        let mut asked = priced.clone();
        asked.recall_unit = RecallUnit::Usd;
        assert_eq!(recall_title(&asked), "recall", "dollars asked and given");
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
            let mut f = Frame::new(Size::new(40, 10));
            let mut b = AsciiBackend::new(Capabilities::empty());
            draw_one(
                &col3,
                SectionId::Recall,
                false,
                false,
                false,
                f.rect(),
                &mut b,
                &mut f,
            );
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

    /// An unanswerable share draws no percentage on the card — `saved —`,
    /// never `0 %`, because the two mean opposite things and only one of them
    /// is a measurement — while the **target** chip beside it stays, since a
    /// target is a goal the product states rather than a figure it measured.
    /// It is also the modal's door, and a door that vanished with the
    /// measurement would take the explanation of the absence with it.
    #[test]
    fn an_unanswerable_share_draws_no_percentage_but_keeps_its_door() {
        let col3 = Col3 {
            savings: RecallView::Data(share_fixture(None)),
            ..Col3::default()
        };
        let inner = Rect {
            x: 0,
            y: 0,
            w: 60,
            h: 10,
        };
        assert_eq!(recall_headlines(&col3).0, "saved —");
        assert!(recall_share_rect(&col3, false, inner).is_some());

        let answered = Col3 {
            savings: RecallView::Data(share_fixture(Some(3.04))),
            ..Col3::default()
        };
        assert_eq!(
            recall_headlines(&answered).0,
            "saved 3.0% (10k)",
            "the shared share, one decimal like the command line"
        );
        assert_eq!(
            share_chip_label(&share_fixture(Some(3.04))).as_deref(),
            Some("[of 90% target]")
        );
        // A card with no target draws no chip: absence, not a zero target.
        let targetless = SavingsData {
            target_pct: None,
            ..share_fixture(Some(3.04))
        };
        assert_eq!(share_chip_label(&targetless), None);
        let chip = recall_share_rect(&answered, false, inner).expect("a target");
        assert_eq!(chip.w, 15, "the rect is exactly the glyphs it draws");
        // Collapsed and narrow shed it, like every other chip on this row.
        assert_eq!(recall_share_rect(&answered, true, inner), None);
        assert_eq!(
            recall_share_rect(
                &answered,
                false,
                Rect {
                    x: 0,
                    y: 0,
                    w: 27,
                    h: 10
                }
            ),
            None
        );
    }
}
