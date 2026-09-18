//! The Recall panel's read-vs-saved bar chart and the unit it is read in.
//!
//! Separate from the panel so the one rule the bars carry — both on one scale,
//! both in one unit, or tokens for both — sits beside the toggle that asks for
//! dollars and the check that refuses them.

use crate::layout::Rect;
use crate::render::frame::{Color, Frame};
use crate::render::{CellShaderKind, RenderBackend};
use crate::theme::{Section, Theme};

use super::format::{f64_of, fmt_km};
use super::recall::RecallView;
use super::recall_period::RECALL_CHART_DY;
use super::Col3;

/// The unit the Recall card's bar chart is read in — a viewer's choice, like
/// the period beside it.
///
/// Purely a *rendering* choice: both figures ride the same feed frame, so
/// flipping it costs no read and cannot show two windows at once. It is also
/// why the toggle emits no data request while the period buttons do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecallUnit {
    /// Token counts, the unit both figures are actually measured in.
    #[default]
    Tokens,
    /// The same figures valued in US dollars — an estimate over an estimate,
    /// and marked as one by the mandatory leading tilde the money authority
    /// renders.
    Usd,
}

impl RecallUnit {
    /// The chip's glyphs — what the bars are showing *now*, not what a press
    /// would switch to. A control labelled with its own effect and a control
    /// labelled with its own state are both defensible; showing the state is
    /// the one that still reads correctly when nobody is about to press it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RecallUnit::Tokens => "tok",
            RecallUnit::Usd => "usd",
        }
    }

    /// The other unit — what a press selects.
    #[must_use]
    pub fn flipped(self) -> RecallUnit {
        match self {
            RecallUnit::Tokens => RecallUnit::Usd,
            RecallUnit::Usd => RecallUnit::Tokens,
        }
    }
}

/// How many inner rows the read/saved bar chart takes: one per bar.
const RECALL_BAR_ROWS: u16 = 2;

/// The narrowest card that can carry a labelled bar and its value without the
/// bar itself shrinking to nothing: 5 label + 1 gap + 4 bar + 1 gap + the
/// widest figure the value column draws.
const RECALL_BAR_MIN_W: u16 = 24;

/// One bar of the read-vs-saved chart: its row label and the tokens behind it.
struct RecallBar {
    label: &'static str,
    /// The token count — the measured figure, and the bar's proportion when
    /// the card is read in tokens.
    value: u64,
    /// The same figure in micro-USD, when the money authority could value it.
    micros: Option<i64>,
    /// The pre-rendered dollar label (tilde mandatory), `Some` exactly when
    /// [`Self::micros`] is.
    usd_label: Option<String>,
    color: Color,
}

/// The three columns of a bar row: the label, the bar and the value.
struct BarColumns {
    label: u16,
    bar: u16,
    value: u16,
}

/// The two figures the bar chart compares, or `None` when neither side of the
/// window holds a token.
///
/// Absence draws nothing, the rule the two headline chips already follow: a
/// window with no reads and no avoided tokens has no proportion to show, and
/// two empty rows labelled `read` and `saved` would look like a measured pair
/// of zeroes rather than a question nobody answered. The rows the chart would
/// have taken go back to the call chart.
///
/// **Both figures come from the share**, so the bars stand with the
/// `[N% of reads]` chip rather than with the `saved N% (…)` headline beside
/// it. On a narrow period the two agree by construction — one cut reaches
/// both. At `all` on a repository carrying a boundary marker they can differ,
/// which is the *existing* difference between those two denominators, spelled
/// out in the cost chapter's "every rate names its denominator": the headline
/// asks what share of the context this plane was responsible for it did not
/// send, the share asks what share of the tokens the agents would have read it
/// avoided. The bars answer the second question, and it is the second question
/// the chip beside them answers too.
fn recall_bar_pair(col3: &Col3) -> Option<[RecallBar; 2]> {
    let RecallView::Data(sd) = &col3.savings else {
        return None;
    };
    let (read, saved) = (sd.share.read_tokens, sd.share.avoided_tokens);
    if read == 0 && saved == 0 {
        return None;
    }
    Some([
        RecallBar {
            label: "read",
            value: read,
            micros: sd.usd_read_micros,
            usd_label: sd.usd_read_label.clone(),
            // The token panel's accent: read tokens are spend, and that is
            // the colour spend already wears on this dashboard.
            color: Section::TokenUsage.accent(),
        },
        RecallBar {
            label: "saved",
            value: saved,
            micros: sd.usd_saved_micros,
            usd_label: sd.usd_label.clone(),
            color: Section::Recall.accent(),
        },
    ])
}

/// Whether the dollar view can be honoured: **both** bars valued, so the pair
/// is still comparable.
///
/// One priced bar beside one unpriced one is worse than two token bars — it
/// would draw a proportion nobody computed. So the fallback is all-or-nothing,
/// and the card says why in its title rather than inventing a rate.
pub(super) fn recall_usd_available(col3: &Col3) -> bool {
    recall_bar_pair(col3).is_some_and(|bars| bars.iter().all(|bar| bar.micros.is_some()))
}

/// The unit the bars are actually drawn in: the viewer's choice, unless the
/// dollar view has no honest basis.
pub(super) fn recall_effective_unit(col3: &Col3) -> RecallUnit {
    if col3.recall_unit == RecallUnit::Usd && !recall_usd_available(col3) {
        RecallUnit::Tokens
    } else {
        col3.recall_unit
    }
}

/// Draw the read-vs-saved bar chart under the period row, returning the rows
/// it used (`0` when it drew nothing, so the call chart reclaims them).
///
/// **Both bars share one scale**, the larger of the two, because the whole
/// point of putting them on one card is that their lengths are comparable:
/// two independently-normalised bars would draw a repository that avoided 1 %
/// of its reads exactly like one that avoided 99 %.
pub(super) fn draw_recall_bars(
    col3: &Col3,
    theme: Theme,
    unicode: bool,
    collapsed: bool,
    inner: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) -> u16 {
    if collapsed || inner.w < RECALL_BAR_MIN_W {
        return 0;
    }
    if inner.h < RECALL_CHART_DY.saturating_add(RECALL_BAR_ROWS) {
        return 0;
    }
    let Some(bars) = recall_bar_pair(col3) else {
        return 0;
    };
    let unit = recall_effective_unit(col3);
    let labels: Vec<String> = bars.iter().map(|bar| bar_label(unit, bar)).collect();
    let Some(columns) = bar_columns(inner, &labels) else {
        return 0;
    };
    let scale = bars
        .iter()
        .map(|bar| bar_amount(unit, bar))
        .fold(0.0f64, f64::max);
    let shader = if unicode {
        CellShaderKind::Braille
    } else {
        CellShaderKind::LuminanceRamp
    };
    for (index, (bar, value)) in bars.iter().zip(&labels).enumerate() {
        let Ok(offset) = u16::try_from(index) else {
            break;
        };
        let y = inner.y + RECALL_CHART_DY + offset;
        frame.put_str(inner.x, y, bar.label, theme.dim());
        let plot = Rect {
            x: inner.x + columns.label,
            y,
            w: columns.bar,
            h: 1,
        };
        let fraction = if scale > 0.0 {
            bar_amount(unit, bar) / scale
        } else {
            0.0
        };
        let samples = crate::charts::hbar_samples(fraction, plot, bar.color);
        backend.shade_cells(&samples, plot, shader, frame);
        let value_w = u16::try_from(value.chars().count())
            .unwrap_or(columns.value)
            .min(columns.value);
        let value_x = inner.x + inner.w.saturating_sub(value_w);
        frame.put_str(value_x, y, value, theme.tile_value(bar.color));
    }
    RECALL_BAR_ROWS
}

/// The figure printed beside a bar: the pre-rendered dollar label in the
/// dollar view, the token count otherwise.
fn bar_label(unit: RecallUnit, bar: &RecallBar) -> String {
    match (unit, bar.usd_label.as_deref()) {
        (RecallUnit::Usd, Some(label)) => label.to_owned(),
        _ => fmt_km(f64_of(bar.value)),
    }
}

/// What a bar's length is proportioned on.
///
/// In dollars the bars are proportioned on the money, not the tokens: a
/// partially-priced mix can value the same token counts differently, and a
/// bar whose length disagreed with the figure printed beside it would be the
/// one thing this chart must never do.
fn bar_amount(unit: RecallUnit, bar: &RecallBar) -> f64 {
    match (unit, bar.micros) {
        (RecallUnit::Usd, Some(micros)) => f64_of(u64::try_from(micros.max(0)).unwrap_or(0)),
        _ => f64_of(bar.value),
    }
}

/// Where a bar row's three columns fall in `inner` for these value labels, or
/// `None` when the bar itself would have no cell.
fn bar_columns(inner: Rect, labels: &[String]) -> Option<BarColumns> {
    let value_w = u16::try_from(labels.iter().map(|v| v.chars().count()).max().unwrap_or(1))
        .unwrap_or(1)
        .min(12);
    // Six: the widest label (`saved`) plus the space that keeps it off the
    // bar's first cell.
    let label_w = 6u16;
    let bar_w = inner
        .w
        .saturating_sub(label_w)
        .saturating_sub(value_w)
        .saturating_sub(1);
    if bar_w == 0 {
        return None;
    }
    Some(BarColumns {
        label: label_w,
        bar: bar_w,
        value: value_w,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities};

    use super::super::fixtures::{barred_col3, text_of};
    use super::super::recall::recall_title;
    use super::super::{draw_one, SectionId};

    /// The bars go through the **render seam**, so the ASCII floor draws them
    /// too — and they share one scale, so their lengths are comparable.
    ///
    /// Two independently-normalised bars would draw a repository that avoided
    /// 1 % of its reads exactly like one that avoided 99 %, which is the one
    /// thing a reader takes from putting the pair on one card.
    #[test]
    fn the_read_and_saved_bars_share_one_scale_through_the_render_seam() {
        let col3 = barred_col3(10_000, 1_000);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 44,
            h: 13,
        };
        for unicode in [false, true] {
            let mut f = Frame::new(Size::new(44, 13));
            let mut b = AsciiBackend::new(if unicode {
                Capabilities::UNICODE
            } else {
                Capabilities::empty()
            });
            draw_one(
                &col3,
                SectionId::Recall,
                false,
                unicode,
                false,
                rect,
                &mut b,
                &mut f,
            );
            let text = text_of(&f);
            assert!(text.contains("read"), "the read bar is labelled:\n{text}");
            assert!(text.contains("saved"), "the saved bar is labelled:\n{text}");
            assert!(text.contains("10k"), "the read figure:\n{text}");
            assert!(text.contains("1.0k"), "the saved figure:\n{text}");

            // Both bars drew, and the shorter one is genuinely shorter: the
            // saved row lights fewer cells than the read row on the same scale.
            let lit = |row: u16| -> usize {
                (0..f.width())
                    .filter(|x| {
                        f.get(*x, row)
                            .is_some_and(|c| !c.ch.is_whitespace() && c.ch != '│' && c.ch != '|')
                    })
                    .count()
            };
            // Inner rows: border(1) + headline, serves, periods → bars at 4, 5.
            let (read_row, saved_row) = (lit(4), lit(5));
            assert!(
                read_row > 0 && saved_row > 0,
                "both bars drew (unicode={unicode})"
            );
            assert!(
                read_row > saved_row,
                "one scale: 10k must draw longer than 1.0k (unicode={unicode}), \
                 got {read_row} vs {saved_row}\n{text}"
            );
        }
    }

    /// The unit toggle flips what the bars read in, and the chip says which
    /// unit is on screen now rather than which one a press would reach.
    #[test]
    fn the_unit_toggle_switches_the_bar_figures_and_names_the_live_unit() {
        let mut col3 = barred_col3(10_000, 1_000);
        if let RecallView::Data(sd) = &mut col3.savings {
            sd.usd_read_micros = Some(50_000_000);
            sd.usd_read_label = Some("~$50.00".to_owned());
            sd.usd_saved_micros = Some(5_000_000);
            sd.usd_label = Some("~$5.00".to_owned());
        }
        let render = |col3: &Col3| -> String {
            let mut f = Frame::new(Size::new(44, 13));
            let mut b = AsciiBackend::new(Capabilities::empty());
            draw_one(
                col3,
                SectionId::Recall,
                false,
                false,
                false,
                f.rect(),
                &mut b,
                &mut f,
            );
            text_of(&f)
        };

        let tokens = render(&col3);
        assert!(
            tokens.contains("[tok]"),
            "the live unit is named:\n{tokens}"
        );
        assert!(
            tokens.contains("10k") && tokens.contains("1.0k"),
            "{tokens}"
        );
        assert!(!tokens.contains("~$50.00"), "{tokens}");

        col3.recall_unit = RecallUnit::Usd;
        let dollars = render(&col3);
        assert!(
            dollars.contains("[usd]"),
            "the live unit is named:\n{dollars}"
        );
        assert!(
            dollars.contains("~$50.00") && dollars.contains("~$5.00"),
            "both bars are valued, tilde mandatory:\n{dollars}"
        );
        // The title is untouched while the request can be honoured.
        assert!(dollars.contains("recall "), "{dollars}");
        assert!(!dollars.contains("no seeded price"), "{dollars}");
    }

    /// Asked for dollars with no seeded price, the card shows **tokens** and
    /// says why in its title — it never invents a rate, and it never draws one
    /// priced bar beside one unpriced one.
    ///
    /// All-or-nothing because a mixed pair would draw a proportion nobody
    /// computed, which is worse than the token view the viewer did not ask
    /// for but can at least read.
    #[test]
    fn dollars_without_a_seeded_price_fall_back_to_tokens_and_say_so() {
        let mut col3 = barred_col3(10_000, 1_000);
        col3.recall_unit = RecallUnit::Usd;
        // Only the saved side priced: the pair is not comparable in dollars.
        if let RecallView::Data(sd) = &mut col3.savings {
            sd.usd_saved_micros = Some(5_000_000);
            sd.usd_label = Some("~$5.00".to_owned());
        }
        assert!(!recall_usd_available(&col3), "half a basis is not a basis");
        assert_eq!(recall_effective_unit(&col3), RecallUnit::Tokens);
        assert_eq!(recall_title(&col3), "recall · no seeded price");

        let mut f = Frame::new(Size::new(44, 13));
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
        assert!(
            text.contains("no seeded price"),
            "the reason is on the card:\n{text}"
        );
        assert!(
            text.contains("[tok]"),
            "the chip names what is drawn:\n{text}"
        );
        // The BAR rows only. The headline's own `[~$5.00]` savings chip is a
        // different affordance with its own basis and is right to keep
        // drawing; what must not appear is a dollar figure on a bar whose
        // partner has none.
        // Rows 4 and 5: border, headline, serves, periods, then the two bars.
        // Selected by position rather than by text, because "saved" also
        // opens the headline and a filter on it would sweep three rows.
        let bars: Vec<&str> = text.lines().skip(4).take(2).collect();
        assert_eq!(bars.len(), 2, "both bars drew:\n{text}");
        assert!(
            bars[0].contains("read") && bars[1].contains("saved"),
            "{text}"
        );
        for row in &bars {
            assert!(
                !row.contains('$'),
                "a priced bar beside an unpriced one: {row}"
            );
        }
        assert!(
            bars[0].contains("10k"),
            "the read bar in tokens: {}",
            bars[0]
        );
        assert!(
            bars[1].contains("1.0k"),
            "the saved bar in tokens: {}",
            bars[1]
        );
    }

    /// A window with nothing on either side draws no bars at all, and gives
    /// the rows back to the call chart.
    ///
    /// Absence and a measured zero are different facts. Two empty rows
    /// labelled `read` and `saved` would look like a pair of zeroes somebody
    /// measured, which is exactly the claim the card has no basis for.
    #[test]
    fn an_unmeasured_window_draws_no_bars() {
        let col3 = barred_col3(0, 0);
        let mut f = Frame::new(Size::new(44, 13));
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
        assert!(!text.contains("saved  "), "no bar rows:\n{text}");
        assert!(
            text.contains("[24h]"),
            "the period row is unaffected:\n{text}"
        );
    }

    /// A positive value always lights at least one sample column.
    ///
    /// A bar that rounded a small-but-real saving down to nothing would draw
    /// absence, and the card would be claiming a measurement it did not make.
    #[test]
    fn a_tiny_but_real_bar_is_short_never_empty() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 1,
        };
        let colour = Section::Recall.accent();
        assert!(crate::charts::hbar_samples(0.0, area, colour).is_empty());
        assert!(crate::charts::hbar_samples(-1.0, area, colour).is_empty());
        assert!(crate::charts::hbar_samples(f64::NAN, area, colour).is_empty());
        assert!(
            !crate::charts::hbar_samples(0.000_01, area, colour).is_empty(),
            "a real value never rounds to an empty bar"
        );
        // Monotonic in the fraction, and full at 1.0 (and beyond, clamped).
        let width = |f: f64| crate::charts::hbar_samples(f, area, colour).len();
        assert!(width(0.25) < width(0.5) && width(0.5) < width(1.0));
        assert_eq!(width(1.0), width(2.0), "the fill clamps at the area");
    }
}
