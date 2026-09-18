//! The token-usage panel: the four-class proportional treemap, the `Σ` total
//! that is the "by worktree" axis's affordance, and the compact footer.

use crate::layout::Rect;
use crate::render::frame::{Color, Frame, Style};
use crate::render::{CellShaderKind, RenderBackend};
use crate::theme::{self, Section, Theme};

use super::format::fmt_km;
use super::worktree::WorktreeAxis;
use super::{body, panel_frame, Col3};

/// Token spend over time.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum TokensView {
    /// No token facts exist in scope — never a fake zero line.
    #[default]
    NoActivity,
    /// Live data.
    Data(TokensData),
}

/// The token-usage section's numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct TokensData {
    /// Tokens per bucket (oldest first), for the sparkline/bars.
    pub rate: Vec<f64>,
    /// Cumulative total across the visible history.
    pub total: f64,
    /// [`Self::total`] decomposed by worktree (the family view), computed in
    /// the same pass over the same rows.
    pub by_worktree: WorktreeAxis,
}

/// The input | read | output breakdown ("read" = cache reads — the settled
/// answer to the label question; cache writes are the secondary band).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum BreakdownView {
    /// No token facts exist in scope.
    #[default]
    NoActivity,
    /// Live data.
    Data(BreakdownData),
}

/// The four-class totals + the read fraction.
#[derive(Clone, Debug, PartialEq)]
pub struct BreakdownData {
    /// Uncached input tokens.
    pub input: f64,
    /// Cached-read input tokens.
    pub read: f64,
    /// Output tokens.
    pub output: f64,
    /// Cache-creation tokens (the secondary band).
    pub cache_write: f64,
    /// read / (input + read), `0/0 ≡ 0`.
    pub read_fraction: f64,
}

/// The token-usage title's fixed lead, up to and including the two pad cells
/// before the `Σ` total. Load-bearing: [`token_total_rect`] measures the
/// clickable span by subtracting it, so the drawn glyphs and the click cells
/// stay the same cells.
const TOKEN_TITLE_LEAD: &str = "token usage  ";

/// The token-usage panel's title — `token usage` alone with no facts in
/// scope, else with the `Σ` total appended.
fn token_title(col3: &Col3) -> String {
    match &col3.tokens {
        TokensView::Data(d) => format!("{TOKEN_TITLE_LEAD}Σ {}", fmt_km(d.total)),
        TokensView::NoActivity => "token usage".to_string(),
    }
}

/// The cells the token panel's `Σ …` total occupies on its title row — the
/// "by worktree" axis's click affordance (the family view).
///
/// The total **is** the affordance: the axis decomposes exactly that number,
/// so the number is what opens the decomposition. It costs no extra cells,
/// which is why the disclosure survives where a separate `[…]` chip could not
/// — column 3 is 34 cells wide in the goldens, leaving 29 for a title, and
/// `token usage  Σ 7.5k  [by worktree]` needs 34. The `w` key
/// (`keymap::GRAPHS`) is the same act from the keyboard, and it is available
/// whether or not this rect is.
///
/// **One solver** feeds the draw and the hit-test over
/// [`crate::chrome::panel`]'s own geometry (the title sits at `x + 3` on the
/// top border row, clipped to the span between the corners), so a click can
/// never land a cell away from the glyphs. `None` — and therefore no target
/// at all — with no total in scope, on a panel too small to carry a border,
/// or when the title would be clipped (a truncated affordance is not one).
///
/// `rect` is the **section's outer rect**, not its inner: the title lives on
/// the border row.
#[must_use]
pub fn token_total_rect(col3: &Col3, rect: Rect) -> Option<Rect> {
    if rect.w < 2 || rect.h < 2 {
        return None; // `chrome::panel` draws no frame, so there is no title
    }
    if !matches!(col3.tokens, TokensView::Data(_)) {
        return None; // no total, nothing to decompose, no target
    }
    let title = token_title(col3);
    let span_start = rect.x.checked_add(2)?;
    let span_end = rect.x.checked_add(rect.w)?.checked_sub(2)?;
    if span_end <= span_start {
        return None;
    }
    let avail = usize::from(span_end.checked_sub(span_start)?.checked_add(1)?).checked_sub(2)?;
    let len = title.chars().count();
    if len > avail {
        return None; // clipped by `chrome::panel` — never a partial chip
    }
    let lead = TOKEN_TITLE_LEAD.chars().count();
    let w = u16::try_from(len.checked_sub(lead)?).ok()?;
    let x = span_start
        .checked_add(1)?
        .checked_add(u16::try_from(lead).ok()?)?;
    Some(Rect {
        x,
        y: rect.y,
        w,
        h: 1,
    })
}

pub(super) fn draw_token_panel(
    col3: &Col3,
    theme: Theme,
    focused: bool,
    unicode: bool,
    collapsed: bool,
    rect: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    // Σ total in the title (from the token totals); the treemap + footer from
    // the four-class breakdown. They arrive together, so breakdown gates data.
    let title = token_title(col3);
    let inner = panel_frame(theme, Section::TokenUsage, focused, &title, rect, frame);
    draw_total_chip(col3, theme, &title, rect, frame);
    let BreakdownView::Data(d) = &col3.breakdown else {
        body(frame, inner, 0, "(no token activity yet)", theme.dim());
        return;
    };
    let read_pct = (d.read_fraction * 100.0).round() as u8;
    if collapsed {
        let line = format!(
            "in {} rd {} out {} · read {read_pct}%",
            fmt_km(d.input),
            fmt_km(d.read),
            fmt_km(d.output)
        );
        body(frame, inner, 0, &line, theme.body());
        return;
    }
    let footer_rows = 2u16;
    if inner.h >= footer_rows + 4 && inner.w >= 16 {
        let chart_area = Rect {
            h: inner.h - footer_rows,
            ..inner
        };
        draw_treemap(d, theme, unicode, chart_area, backend, frame);
    }
    let footer = Rect {
        y: inner.y + inner.h.saturating_sub(footer_rows),
        h: footer_rows.min(inner.h),
        ..inner
    };
    draw_token_footer(theme, d, read_pct, footer, frame);
}

/// The `Σ` total re-drawn over its own title cells, from the SAME solver the
/// hit-test reads, in the key-hint style that marks an act everywhere else in
/// the dashboard — it is the "by worktree" axis's affordance (the family
/// view). Flashless by design, like the USD chip: col3 carries no press-pulse
/// plumbing, by a recorded decision.
fn draw_total_chip(col3: &Col3, theme: Theme, title: &str, rect: Rect, frame: &mut Frame) {
    let Some(chip) = token_total_rect(col3, rect) else {
        return;
    };
    let sigma: String = title
        .chars()
        .skip(TOKEN_TITLE_LEAD.chars().count())
        .collect();
    frame.put_str(chip.x, chip.y, &sigma, theme.key_hint());
}

/// The four-class treemap: the outline, the shaded leaves and one class
/// symbol per leaf that has a cell to carry it.
fn draw_treemap(
    d: &BreakdownData,
    theme: Theme,
    unicode: bool,
    chart_area: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let vals = [d.input, d.read, d.output, d.cache_write];
    let plot_rect = crate::render::treemap_outline(frame, chart_area, theme.dim(), unicode);
    let leaves = crate::charts::treemap_layout(&vals);
    let samples = crate::charts::treemap_samples(&leaves, plot_rect, &theme::BREAKDOWN);
    backend.shade_cells(&samples, plot_rect, CellShaderKind::Braille, frame);
    let symbols = ['I', 'R', 'O', 'C'];
    for leaf in leaves {
        let label_rect = crate::charts::treemap_cell_rect(leaf, plot_rect);
        if label_rect.w == 0 || label_rect.h == 0 {
            continue;
        }
        let colour = theme::BREAKDOWN
            .get(leaf.index)
            .copied()
            .unwrap_or(theme.graph_accent());
        let symbol = symbols.get(leaf.index).copied().unwrap_or('T');
        frame.set(label_rect.x, label_rect.y, symbol, theme.tile_value(colour));
    }
}

fn draw_token_footer(theme: Theme, d: &BreakdownData, read_pct: u8, rect: Rect, frame: &mut Frame) {
    if rect.h == 0 {
        return;
    }
    let cats = &theme::BREAKDOWN;
    let color = |index: usize| cats.get(index).copied().unwrap_or(theme.graph_accent());
    let values = [
        fmt_km(d.input),
        fmt_km(d.read),
        fmt_km(d.output),
        fmt_km(d.cache_write),
    ];
    let full_labels = ["input", "read", "output", "cache-write"];
    let compact_labels = ["in", "rd", "out", "ca"];
    let use_full_labels = footer_pair_width(full_labels[0], &values[0], full_labels[1], &values[1])
        <= usize::from(rect.w)
        && footer_pair_width(full_labels[2], &values[2], full_labels[3], &values[3])
            <= usize::from(rect.w);
    let labels = if use_full_labels {
        full_labels
    } else {
        compact_labels
    };
    let end = rect.x.saturating_add(rect.w);
    let pct = format!("read {read_pct}%");
    let row_one_width = footer_pair_width(labels[0], &values[0], labels[1], &values[1]);
    let row_two_width = footer_pair_width(labels[2], &values[2], labels[3], &values[3]);
    let pct_width = pct.len();
    let pct_row = if row_one_width + 1 + pct_width <= usize::from(rect.w) {
        Some(0u16)
    } else if rect.h >= 2 && row_two_width + 1 + pct_width <= usize::from(rect.w) {
        Some(1u16)
    } else {
        None
    };
    let row_end = |row: u16| {
        if pct_row == Some(row) {
            end.saturating_sub(u16::try_from(pct_width + 1).unwrap_or(rect.w))
        } else {
            end
        }
    };
    draw_footer_pair(
        frame,
        rect.x,
        row_end(0),
        rect.y,
        [
            (labels[0], values[0].as_str(), color(0)),
            (labels[1], values[1].as_str(), color(1)),
        ],
        theme,
    );
    if rect.h >= 2 {
        draw_footer_pair(
            frame,
            rect.x,
            row_end(1),
            rect.y + 1,
            [
                (labels[2], values[2].as_str(), color(2)),
                (labels[3], values[3].as_str(), color(3)),
            ],
            theme,
        );
    }
    if let Some(row) = pct_row {
        let x = end.saturating_sub(u16::try_from(pct_width).unwrap_or(rect.w));
        put_footer_str(
            frame,
            x,
            end,
            rect.y + row,
            &pct,
            theme.tile_value(color(1)),
        );
    }
}

fn footer_pair_width(label_a: &str, value_a: &str, label_b: &str, value_b: &str) -> usize {
    label_a.len() + value_a.len() + label_b.len() + value_b.len() + 3
}

fn draw_footer_pair(
    frame: &mut Frame,
    x: u16,
    end: u16,
    y: u16,
    items: [(&str, &str, Color); 2],
    theme: Theme,
) {
    let [(first_label, first_value, first_color), (second_label, second_value, second_color)] =
        items;
    let x = footer_item(
        frame,
        x,
        end,
        y,
        first_label,
        first_value,
        first_color,
        theme,
    );
    let x = put_footer_str(frame, x, end, y, " ", theme.dim());
    footer_item(
        frame,
        x,
        end,
        y,
        second_label,
        second_value,
        second_color,
        theme,
    );
}

fn footer_item(
    frame: &mut Frame,
    x: u16,
    end: u16,
    y: u16,
    label: &str,
    value: &str,
    color: Color,
    theme: Theme,
) -> u16 {
    let x = put_footer_str(frame, x, end, y, label, Style::fg(color));
    let x = put_footer_str(frame, x, end, y, " ", theme.dim());
    put_footer_str(frame, x, end, y, value, theme.tile_value(color))
}

fn put_footer_str(frame: &mut Frame, x: u16, end: u16, y: u16, text: &str, style: Style) -> u16 {
    if x >= end {
        return x;
    }
    let clipped: String = text.chars().take(usize::from(end - x)).collect();
    frame.put_str(x, y, &clipped, style).min(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities};

    use super::super::fixtures::{draw_to, text_of, tokens_col3};
    use super::super::{draw_one, SectionId};

    #[derive(Default)]
    struct ShaderProbe {
        shader: Option<CellShaderKind>,
    }

    impl RenderBackend for ShaderProbe {
        fn name(&self) -> &'static str {
            "shader-probe"
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities::UNICODE | Capabilities::SUBCELL
        }

        fn present(&mut self, _frame: &Frame) -> Result<(), crate::render::RenderError> {
            Ok(())
        }

        fn shade_cells(
            &mut self,
            _samples: &[crate::render::ShadeSample],
            _area: Rect,
            shader: CellShaderKind,
            _frame: &mut Frame,
        ) {
            self.shader = Some(shader);
        }
    }

    #[test]
    fn token_panel_draws_proportional_treemap_and_compact_footer() {
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 5.0],
                total: 22.0,
                by_worktree: WorktreeAxis::default(),
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 1234.0,
                read: 800.0,
                output: 300.0,
                cache_write: 50.0,
                read_fraction: 0.8,
            }),
            ..Col3::default()
        };
        let f = draw_to(&col3, 40, 24);
        // The Σ total renders in the panel title.
        let text = text_of(&f);
        assert!(text.contains("Σ"), "the Σ total is in the title: \n{text}");
        assert!(
            text.contains("input 1.2k"),
            "input footer is compact: \n{text}"
        );
        assert!(
            text.contains("read 800"),
            "read footer is compact: \n{text}"
        );
        assert!(
            text.contains("output 300"),
            "output footer is compact: \n{text}"
        );
        assert!(
            text.contains("cache-write 50"),
            "cache footer is compact: \n{text}"
        );
        assert!(
            text.contains("read 80%"),
            "read fraction is exact: \n{text}"
        );
        // The ASCII floor remains a coloured density ramp when Braille is not
        // supported by the backend.
        let mut coloured = 0;
        for y in 0..f.height() {
            for x in 0..f.width() {
                if let Some(c) = f.get(x, y) {
                    if ".:-=+*#%@".contains(c.ch) && matches!(c.style.fg, Color::Rgb(_, _, _)) {
                        coloured += 1;
                    }
                }
            }
        }
        assert!(coloured > 0, "the treemap drew no coloured ramp cells");
    }

    #[test]
    fn token_panel_uses_braille_symbols_and_full_footer_names_at_live_panel_width() {
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0],
                total: 37_500_000.0,
                by_worktree: WorktreeAxis::default(),
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 114.0,
                read: 33_664_392.0,
                output: 58_306.0,
                cache_write: 3_767_170.0,
                read_fraction: 1.0,
            }),
            ..Col3::default()
        };
        let mut frame = Frame::new(Size::new(46, 9));
        let mut backend = ShaderProbe::default();
        draw_token_panel(
            &col3,
            Theme::rift_dark(),
            false,
            true,
            false,
            frame.rect(),
            &mut backend,
            &mut frame,
        );
        let text = text_of(&frame);
        assert_eq!(backend.shader, Some(CellShaderKind::Braille));
        assert!(text.contains("input 114"), "input class is named:\n{text}");
        assert!(text.contains("read 33.7M"), "read class is named:\n{text}");
        assert!(
            text.contains("output 58k"),
            "output class is named:\n{text}"
        );
        assert!(
            text.contains("cache-write 3.8M"),
            "cache-write class is named:\n{text}"
        );
        assert!(text.contains("read 100%"), "read fraction remains:\n{text}");
        let chart_text: String = (1..6)
            .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
            .map(|(x, y)| frame.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(chart_text.contains('R') && chart_text.contains('C'));
        assert!(
            !chart_text.contains("read") && !chart_text.contains("cache"),
            "the plot keeps only class symbols; full names belong in the footer: {chart_text}"
        );
    }

    #[test]
    fn read_fraction_zero_over_zero_renders_as_zero_not_error() {
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0],
                total: 5.0,
                by_worktree: WorktreeAxis::default(),
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 0.0,
                read: 0.0,
                output: 5.0,
                cache_write: 0.0,
                read_fraction: 0.0,
            }),
            ..Col3::default()
        };
        let frame = draw_to(&col3, 44, 24);
        let text = text_of(&frame);
        assert!(
            text.contains("read 0%"),
            "the footer shows a defined 0, not a dash"
        );
    }

    #[test]
    fn zero_total_keeps_empty_boundary_and_all_compact_footer_values() {
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![0.0],
                total: 0.0,
                by_worktree: WorktreeAxis::default(),
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 0.0,
                read: 0.0,
                output: 0.0,
                cache_write: 0.0,
                read_fraction: 0.0,
            }),
            ..Col3::default()
        };
        let frame = draw_to(&col3, 44, 24);
        let text = text_of(&frame);
        assert!(
            text.contains('+'),
            "the empty outer boundary remains:\n{text}"
        );
        assert!(text.contains("input 0 read 0"), "first footer row:\n{text}");
        assert!(
            text.contains("output 0 cache-write 0") && text.contains("read 0%"),
            "second footer row:\n{text}"
        );
        for y in 2..5 {
            for x in 2..42 {
                assert_eq!(
                    frame.get(x, y).map(|cell| cell.ch),
                    Some(' '),
                    "zero weights do not receive shaded interior cells at ({x},{y})"
                );
            }
        }
    }

    /// the family view: the `Σ` total IS the axis's affordance — one solver
    /// feeds the draw and the hit-test, so the clickable cells are exactly
    /// the drawn glyphs, and they are drawn in the key-hint style that marks
    /// an act (not the section-title style the rest of the title carries).
    #[test]
    fn the_sigma_total_is_the_drawn_axis_affordance() {
        let col3 = tokens_col3(7_500.0);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 34,
            h: 9,
        };
        let mut f = Frame::new(Size::new(34, 9));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_one(
            &col3,
            SectionId::TokenUsage,
            false,
            false,
            false,
            rect,
            &mut b,
            &mut f,
        );
        let chip = token_total_rect(&col3, rect).expect("a total is an affordance");
        let drawn: String = (chip.x..chip.x + chip.w)
            .map(|x| f.get(x, chip.y).map_or(' ', |c| c.ch))
            .collect();
        assert_eq!(drawn, "Σ 7.5k", "the solver names the total's own cells");
        assert_eq!(chip.y, rect.y, "the title row, not the body");
        // Marked as an act, and distinguishable from the title beside it.
        let theme = Theme::rift_dark();
        assert_eq!(
            f.get(chip.x, chip.y).map(|c| c.style),
            Some(theme.key_hint())
        );
        assert_ne!(
            theme.key_hint(),
            theme.title(Section::TokenUsage),
            "an affordance that looked exactly like its title would teach nothing"
        );
        // The word before it is still the title, in the title's own style.
        assert_eq!(
            f.get(chip.x - 2, chip.y).map(|c| c.style),
            Some(theme.title(Section::TokenUsage))
        );
    }

    /// No total, no border, or a clipped title → no target at all. A
    /// truncated affordance is not one, and a click must never land on a
    /// panel that never offered it (the `recall_cost_rect` rule, the priced savings estimate).
    #[test]
    fn no_total_and_no_room_offer_no_axis_target() {
        let wide = Rect {
            x: 0,
            y: 0,
            w: 34,
            h: 9,
        };
        assert_eq!(
            token_total_rect(&Col3::default(), wide),
            None,
            "no facts in scope ⇒ no total ⇒ nothing to decompose"
        );
        let col3 = tokens_col3(7_500.0);
        assert_eq!(
            token_total_rect(
                &col3,
                Rect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 9
                }
            ),
            None,
            "too narrow to carry a frame ⇒ no title row exists"
        );
        assert_eq!(
            token_total_rect(
                &col3,
                Rect {
                    x: 0,
                    y: 0,
                    w: 34,
                    h: 1
                }
            ),
            None,
            "too short to carry a frame"
        );
        assert_eq!(
            token_total_rect(
                &col3,
                Rect {
                    x: 0,
                    y: 0,
                    w: 20,
                    h: 9
                }
            ),
            None,
            "`token usage  Σ 7.5k` does not fit whole ⇒ no partial chip"
        );
        // …and the panel still draws its (clipped) title at that width: the
        // affordance sheds, the panel does not.
        let mut f = Frame::new(Size::new(20, 9));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_one(
            &col3,
            SectionId::TokenUsage,
            false,
            false,
            false,
            Rect {
                x: 0,
                y: 0,
                w: 20,
                h: 9,
            },
            &mut b,
            &mut f,
        );
        let row: String = (0..20).map(|x| f.get(x, 0).map_or(' ', |c| c.ch)).collect();
        assert!(row.contains("token usage"), "{row}");
    }
}
