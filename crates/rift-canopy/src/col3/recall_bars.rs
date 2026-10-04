//! The Recall panel's read-vs-saved bar chart.
//!
//! Separate from the panel so the one rule the bars carry — both on one
//! scale, both in tokens — sits apart from the headline that reads the same
//! figures as a line.

use crate::layout::Rect;
use crate::render::frame::{Color, Frame};
use crate::render::{CellShaderKind, RenderBackend};
use crate::theme::{Section, Theme};

use super::format::{f64_of, fmt_km};
use super::recall::RecallView;
use super::recall_period::RECALL_CHART_DY;
use super::Col3;

/// How many inner rows the read/saved bar chart takes: one per bar.
const RECALL_BAR_ROWS: u16 = 2;

/// The narrowest card that can carry a labelled bar and its value without the
/// bar itself shrinking to nothing: 5 label + 1 gap + 4 bar + 1 gap + the
/// widest figure the value column draws.
const RECALL_BAR_MIN_W: u16 = 24;

/// One bar of the read-vs-saved chart: its row label and the tokens behind it.
struct RecallBar {
    label: &'static str,
    /// The token count — the measured figure and the bar's proportion.
    value: u64,
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
/// Absence draws nothing: a window with no reads and no avoided tokens has
/// no proportion to show, and two empty rows labelled `read` and `saved` would
/// look like a measured pair of zeroes rather than a question nobody answered.
/// The rows the chart would have taken go back to the call chart.
///
/// **Both figures come from the share**, the same one the headline's tokens
/// and percentage are read from, so the line and the bars under it cannot
/// describe two windows or two denominators.
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
            // The token panel's accent: read tokens are spend, and that is
            // the colour spend already wears on this dashboard.
            color: Section::TokenUsage.accent(),
        },
        RecallBar {
            label: "saved",
            value: saved,
            color: Section::Recall.accent(),
        },
    ])
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
    let labels: Vec<String> = bars.iter().map(|bar| fmt_km(f64_of(bar.value))).collect();
    let Some(columns) = bar_columns(inner, &labels) else {
        return 0;
    };
    let scale = bars
        .iter()
        .map(|bar| f64_of(bar.value))
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
            f64_of(bar.value) / scale
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
