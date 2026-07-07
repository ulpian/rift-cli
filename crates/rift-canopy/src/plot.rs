//! Sub-cell graph plotting: one points→cells contract, two glyph tiers (M4-09).
//!
//! Everything a graph widget draws becomes a set of normalized [`PlotPoint`]s;
//! [`dot_of`] — the **single** points-to-cells contract — maps a point to a
//! terminal cell plus a sub-cell dot at a tier's resolution. Both tiers place a
//! point in the *same cell* (the cell math never consults the tier); the tier
//! only refines placement within it: braille refines a cell into 2×4 dots
//! ([`draw_braille`]), the block fallback into 1×2 half-blocks
//! ([`draw_blocks`]). The [`crate::render::RenderBackend::plot`] seam picks the
//! tier from capabilities, so every terminal shows the same data (user
//! story 19).
//!
//! The widget point-generators — [`sparkline_points`], [`bar_points`],
//! [`line_points`], [`gauge_points`] — are pure functions from series data to
//! points; the column-3 sections draw with them through the seam.

use crate::layout::Rect;
use crate::render::frame::{Color, Frame, Style};

/// How a plot's cells are coloured. **Cosmetic only** — a `Paint` is evaluated
/// per *already-lit* cell (after the point→cell contract has chosen which cells
/// light), so no variant can add or remove a lit cell: the one-contract invariant
/// holds for every `Paint`. `xfrac`/`yfrac` are the cell's normalized position in
/// the plot rect (`x`: 0 left → 1 right; `y`: 0 bottom → 1 top).
#[derive(Clone, Copy, Debug)]
pub enum Paint {
    /// One style for every cell. `Solid(Style::DEFAULT)` reproduces the
    /// pre-colour terminal-default look exactly.
    Solid(Style),
    /// A vertical gradient from `lo` (bottom) to `hi` (top) — bars that brighten
    /// as they rise.
    VGradient {
        /// The colour at the baseline (`yfrac = 0`).
        lo: Color,
        /// The colour at the top (`yfrac = 1`).
        hi: Color,
    },
    /// A horizontal ramp of `(position, colour)` stops sampled by `xfrac` — the
    /// thermometer look (green → amber → red as a meter extends rightward).
    HRamp(&'static [(f32, Color)]),
    /// One colour per x-slot: a cell's colour is `colours[floor(xfrac * n)]` — a
    /// categorical bar chart (e.g. input | read | output | cache-write).
    ByCategory(&'static [Color]),
}

impl Paint {
    /// The style for a lit cell at normalized position `(xfrac, yfrac)`. **Total**:
    /// always yields a concrete [`Style`]; it never signals "skip", so it can
    /// never change which cells are lit.
    #[must_use]
    pub fn style_at(&self, xfrac: f32, yfrac: f32) -> Style {
        match self {
            Self::Solid(s) => *s,
            Self::VGradient { lo, hi } => {
                Style::fg(crate::theme::lerp_color(*lo, *hi, clamp01(yfrac)))
            }
            Self::HRamp(stops) => Style::fg(crate::theme::ramp_color(stops, clamp01(xfrac))),
            Self::ByCategory(colours) => {
                let n = colours.len();
                if n == 0 {
                    return Style::DEFAULT;
                }
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let idx = ((clamp01(xfrac) * n as f32) as usize).min(n - 1);
                Style::fg(colours.get(idx).copied().unwrap_or(Color::Default))
            }
        }
    }
}

/// Clamp to `[0, 1]`, mapping non-finite inputs to `0.0`.
fn clamp01(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// A normalized plot point: `x` rightward and `y` upward from the bottom-left,
/// both in `[0, 1]`. Non-finite or out-of-range points are ignored at the
/// contract, so a corrupt sample can never write outside its rect.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlotPoint {
    /// Horizontal position, `0.0` = left edge, `1.0` = right edge.
    pub x: f32,
    /// Vertical position, `0.0` = bottom edge, `1.0` = top edge.
    pub y: f32,
}

/// Sub-cell dots per cell for the braille tier (2 columns × 4 rows).
const BRAILLE_SUB: (u32, u32) = (2, 4);
/// Sub-cell dots per cell for the block fallback tier (1 column × 2 half-rows).
const BLOCK_SUB: (u32, u32) = (1, 2);

/// The one points→cells contract: the (cell, dot) coordinate of `p` in a plot
/// of `w`×`h` cells at `sub` dots per cell, or `None` when the point is
/// non-finite, out of `[0, 1]`, or the plot is degenerate. The cell half of
/// the result — `(cell_x, cell_y)` — is **independent of `sub`**, which is
/// what makes the tiers correspond cell-for-cell by construction.
fn dot_of(p: PlotPoint, w: u16, h: u16, sub: (u32, u32)) -> Option<(u16, u16, u32, u32)> {
    if w == 0 || h == 0 || !p.x.is_finite() || !p.y.is_finite() {
        return None;
    }
    if !(0.0..=1.0).contains(&p.x) || !(0.0..=1.0).contains(&p.y) {
        return None;
    }
    let dots_x = u32::from(w) * sub.0;
    let dots_y = u32::from(h) * sub.1;
    // 1.0 clamps onto the last dot (loss-free for every interior value).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let dx = ((p.x * dots_x as f32) as u32).min(dots_x - 1);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let dy_up = ((p.y * dots_y as f32) as u32).min(dots_y - 1);
    let dy = dots_y - 1 - dy_up; // dot rows count downward from the top
    let cell_x = u16::try_from(dx / sub.0).ok()?;
    let cell_y = u16::try_from(dy / sub.1).ok()?;
    Some((cell_x, cell_y, dx % sub.0, dy % sub.1))
}

/// The braille dot bit for a sub-cell coordinate (Unicode braille numbering:
/// dots 1–3 + 7 down the left column, 4–6 + 8 down the right).
fn braille_bit(dot_x: u32, dot_y: u32) -> u32 {
    const LEFT: [u32; 4] = [0x01, 0x02, 0x04, 0x40];
    const RIGHT: [u32; 4] = [0x08, 0x10, 0x20, 0x80];
    let col = if dot_x == 0 { LEFT } else { RIGHT };
    *col.get(dot_y as usize).unwrap_or(&0)
}

/// Draw `points` into `area` as braille dot glyphs (the sub-cell tier). `paint`
/// colours each lit cell by its position; it never affects which cells light.
pub fn draw_braille(frame: &mut Frame, area: Rect, points: &[PlotPoint], paint: &Paint) {
    let mut masks: std::collections::BTreeMap<(u16, u16), u32> = std::collections::BTreeMap::new();
    for p in points {
        if let Some((cx, cy, dx, dy)) = dot_of(*p, area.w, area.h, BRAILLE_SUB) {
            *masks.entry((cx, cy)).or_insert(0) |= braille_bit(dx, dy);
        }
    }
    for ((cx, cy), mask) in masks {
        if let Some(ch) = char::from_u32(0x2800 + mask) {
            frame.set(
                area.x + cx,
                area.y + cy,
                ch,
                cell_style(paint, cx, cy, area),
            );
        }
    }
}

/// Draw `points` into `area` as half-block glyphs (the fallback floor tier):
/// top half `▀`, bottom half `▄`, both `█`. Lights exactly the same cells as
/// [`draw_braille`] for the same points; `paint` colours them the same way.
pub fn draw_blocks(frame: &mut Frame, area: Rect, points: &[PlotPoint], paint: &Paint) {
    let mut masks: std::collections::BTreeMap<(u16, u16), u8> = std::collections::BTreeMap::new();
    for p in points {
        if let Some((cx, cy, _dx, dy)) = dot_of(*p, area.w, area.h, BLOCK_SUB) {
            *masks.entry((cx, cy)).or_insert(0) |= if dy == 0 { 0b01 } else { 0b10 };
        }
    }
    for ((cx, cy), mask) in masks {
        let ch = match mask {
            0b01 => '▀',
            0b10 => '▄',
            _ => '█',
        };
        frame.set(
            area.x + cx,
            area.y + cy,
            ch,
            cell_style(paint, cx, cy, area),
        );
    }
}

/// Evaluate `paint` for the cell at local offset `(cx, cy)` within `area`. The
/// cell's normalized position is `x = (cx+0.5)/w` (left→right) and
/// `y = 1 - (cy+0.5)/h` (bottom→top, since cell rows count down from the top).
fn cell_style(paint: &Paint, cx: u16, cy: u16, area: Rect) -> Style {
    let w = f32::from(area.w.max(1));
    let h = f32::from(area.h.max(1));
    let xfrac = (f32::from(cx) + 0.5) / w;
    let yfrac = 1.0 - (f32::from(cy) + 0.5) / h;
    paint.style_at(xfrac, yfrac)
}

/// Normalize `v` against `max` (zero/degenerate max ⇒ 0.0, never a NaN).
fn norm(v: f64, max: f64) -> f32 {
    if max <= 0.0 || !max.is_finite() || !v.is_finite() || v <= 0.0 {
        return 0.0;
    }
    #[allow(clippy::cast_possible_truncation)]
    let y = (v / max).min(1.0) as f32;
    y
}

/// The centered x position of sample `i` of `n` (samples own equal slots).
#[allow(clippy::cast_precision_loss)]
fn slot_x(i: usize, n: usize) -> f32 {
    (i as f32 + 0.5) / n as f32
}

/// Sparkline: one dot per sample at its value height (autoscaled by `max`).
#[must_use]
pub fn sparkline_points(values: &[f64], max: f64) -> Vec<PlotPoint> {
    values
        .iter()
        .enumerate()
        .map(|(i, v)| PlotPoint {
            x: slot_x(i, values.len()),
            y: norm(*v, max),
        })
        .collect()
}

/// Bars: each sample becomes a filled column of `levels` stacked dots from the
/// baseline up to its value height. `levels` is the vertical point density —
/// pass at least the target dot resolution (rows × 4) for gap-free fills.
#[must_use]
pub fn bar_points(values: &[f64], max: f64, levels: u16) -> Vec<PlotPoint> {
    let levels = levels.max(1);
    let mut out = Vec::new();
    for (i, v) in values.iter().enumerate() {
        let x = slot_x(i, values.len());
        let top = norm(*v, max);
        for step in 0..=levels {
            let y = f32::from(step) / f32::from(levels);
            if y > top {
                break;
            }
            out.push(PlotPoint { x, y });
        }
    }
    out
}

/// Grouped bars: `groups` day-slots, each carrying `cats` category values, laid
/// out as adjacent bars within a slot with a one-sub-slot gap between slots
/// (M55-09). Returns one point vector **per category** so the caller draws each
/// in its own colour through the plot seam. Categories never overlap within a
/// slot by construction (each owns a distinct x sub-slot), so the
/// paint-never-moves-a-cell property extends to grouped bars. All bars share
/// `max` so heights are comparable across days and categories.
#[must_use]
#[allow(clippy::cast_precision_loss)]
pub fn grouped_bar_points(
    groups: &[Vec<f64>],
    cats: usize,
    max: f64,
    levels: u16,
) -> Vec<Vec<PlotPoint>> {
    let mut out: Vec<Vec<PlotPoint>> = vec![Vec::new(); cats];
    if groups.is_empty() || cats == 0 {
        return out;
    }
    let levels = levels.max(1);
    // Each group owns `cats` bar sub-slots plus a trailing gap sub-slot.
    let stride = cats + 1;
    let total_sub = groups.len() * stride;
    for (g, group) in groups.iter().enumerate() {
        for (c, out_cat) in out.iter_mut().enumerate() {
            let value = group.get(c).copied().unwrap_or(0.0);
            let sub = g * stride + c;
            let x = (sub as f32 + 0.5) / total_sub as f32;
            let top = norm(value, max);
            for step in 0..=levels {
                let y = f32::from(step) / f32::from(levels);
                if y > top {
                    break;
                }
                out_cat.push(PlotPoint { x, y });
            }
        }
    }
    out
}

/// Line: the series as a connected polyline, resampled at `xsamples` columns
/// with linear interpolation between samples. Empty/degenerate input yields no
/// points.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
pub fn line_points(values: &[f64], max: f64, xsamples: u16) -> Vec<PlotPoint> {
    if values.is_empty() || xsamples == 0 {
        return Vec::new();
    }
    if let [only] = values {
        return vec![PlotPoint {
            x: 0.5,
            y: norm(*only, max),
        }];
    }
    let n = values.len();
    let mut out = Vec::with_capacity(usize::from(xsamples));
    for s in 0..xsamples {
        let x = (f32::from(s) + 0.5) / f32::from(xsamples);
        // Position within the series, in sample units.
        let pos = f64::from(x) * (n - 1) as f64;
        let lo = (pos.floor() as usize).min(n - 1);
        let hi = (lo + 1).min(n - 1);
        let frac = pos - pos.floor();
        let v_lo = values.get(lo).copied().unwrap_or(0.0);
        let v_hi = values.get(hi).copied().unwrap_or(0.0);
        let v = v_lo + (v_hi - v_lo) * frac;
        out.push(PlotPoint { x, y: norm(v, max) });
    }
    out
}

/// Gauge: a horizontal fill covering `fraction` (clamped `[0, 1]`) of the plot
/// width, `ylevels` dots thick — the sub-cell horizontal meter.
#[must_use]
pub fn gauge_points(fraction: f32, xsamples: u16, ylevels: u16) -> Vec<PlotPoint> {
    let fraction = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (xsamples, ylevels) = (xsamples.max(1), ylevels.max(1));
    let mut out = Vec::new();
    for s in 0..xsamples {
        let x = (f32::from(s) + 0.5) / f32::from(xsamples);
        if x > fraction {
            break;
        }
        for step in 0..ylevels {
            let y = (f32::from(step) + 0.5) / f32::from(ylevels);
            out.push(PlotPoint { x, y });
        }
    }
    out
}

/// The autoscale ceiling for a series: its max, guarded so a zero-range or
/// empty series still yields a defined, non-zero scale (no divide-by-zero, no
/// panic — PRD "autoscale guarded against zero ranges").
#[must_use]
pub fn autoscale_max(values: &[f64]) -> f64 {
    let max = values
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .fold(0.0, f64::max);
    if max > 0.0 {
        max
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;
    use proptest::prelude::*;

    fn lit_cells(f: &Frame) -> Vec<(u16, u16)> {
        let mut out = Vec::new();
        for y in 0..f.height() {
            for x in 0..f.width() {
                if f.get(x, y).is_some_and(|c| c.ch != ' ') {
                    out.push((x, y));
                }
            }
        }
        out
    }

    fn area(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn braille_and_blocks_light_the_same_cells() {
        // The one-contract guarantee: same points → same cells at both tiers.
        let series = [1.0, 4.0, 2.0, 8.0, 5.0, 3.0, 7.0];
        let pts = line_points(&series, autoscale_max(&series), 24);
        let a = area(2, 1, 12, 4);

        let mut braille = Frame::new(Size::new(20, 8));
        draw_braille(&mut braille, a, &pts, &Paint::Solid(Style::DEFAULT));
        let mut blocks = Frame::new(Size::new(20, 8));
        draw_blocks(&mut blocks, a, &pts, &Paint::Solid(Style::DEFAULT));

        assert_eq!(lit_cells(&braille), lit_cells(&blocks));
        assert!(!lit_cells(&braille).is_empty());
    }

    #[test]
    fn braille_glyphs_are_braille_range() {
        let pts = sparkline_points(&[1.0, 2.0, 3.0], 3.0);
        let mut f = Frame::new(Size::new(10, 4));
        draw_braille(
            &mut f,
            area(0, 0, 10, 4),
            &pts,
            &Paint::Solid(Style::DEFAULT),
        );
        for (x, y) in lit_cells(&f) {
            let ch = f.get(x, y).unwrap().ch;
            let cp = ch as u32;
            assert!((0x2800..=0x28FF).contains(&cp), "non-braille glyph {ch:?}");
        }
    }

    #[test]
    fn empty_points_and_degenerate_rects_are_safe() {
        let mut f = Frame::new(Size::new(10, 4));
        draw_braille(
            &mut f,
            area(0, 0, 0, 0),
            &[PlotPoint { x: 0.5, y: 0.5 }],
            &Paint::Solid(Style::DEFAULT),
        );
        draw_blocks(
            &mut f,
            area(0, 0, 10, 0),
            &[PlotPoint { x: 0.5, y: 0.5 }],
            &Paint::Solid(Style::DEFAULT),
        );
        draw_braille(
            &mut f,
            area(0, 0, 10, 4),
            &[],
            &Paint::Solid(Style::DEFAULT),
        );
        assert!(lit_cells(&f).is_empty());
    }

    #[test]
    fn non_finite_points_are_ignored() {
        let hostile = [
            PlotPoint {
                x: f32::NAN,
                y: 0.5,
            },
            PlotPoint {
                x: 0.5,
                y: f32::INFINITY,
            },
            PlotPoint { x: -0.1, y: 0.5 },
            PlotPoint { x: 0.5, y: 1.7 },
        ];
        let mut f = Frame::new(Size::new(10, 4));
        draw_braille(
            &mut f,
            area(0, 0, 10, 4),
            &hostile,
            &Paint::Solid(Style::DEFAULT),
        );
        draw_blocks(
            &mut f,
            area(0, 0, 10, 4),
            &hostile,
            &Paint::Solid(Style::DEFAULT),
        );
        assert!(lit_cells(&f).is_empty());
    }

    #[test]
    fn autoscale_never_zero() {
        assert!((autoscale_max(&[]) - 1.0).abs() < f64::EPSILON);
        assert!((autoscale_max(&[0.0, 0.0]) - 1.0).abs() < f64::EPSILON);
        assert!((autoscale_max(&[f64::NAN]) - 1.0).abs() < f64::EPSILON);
        assert!((autoscale_max(&[3.0, 9.0]) - 9.0).abs() < f64::EPSILON);
    }

    #[test]
    fn single_point_series_has_a_defined_scale_and_plots() {
        let pts = line_points(&[5.0], autoscale_max(&[5.0]), 10);
        assert_eq!(pts.len(), 1);
        let mut f = Frame::new(Size::new(10, 4));
        draw_braille(
            &mut f,
            area(0, 0, 10, 4),
            &pts,
            &Paint::Solid(Style::DEFAULT),
        );
        assert_eq!(lit_cells(&f).len(), 1);
    }

    #[test]
    fn gauge_fill_scales_with_fraction() {
        let full = gauge_points(1.0, 24, 4);
        let half = gauge_points(0.5, 24, 4);
        let none = gauge_points(0.0, 24, 4);
        assert!(none.len() < half.len() && half.len() < full.len());
        // NaN degrades to empty, never a panic.
        assert!(gauge_points(f32::NAN, 24, 4).is_empty());
    }

    #[test]
    fn bars_fill_down_to_the_baseline() {
        let series = [8.0];
        let pts = bar_points(&series, 8.0, 16);
        let mut f = Frame::new(Size::new(4, 4));
        draw_blocks(
            &mut f,
            area(0, 0, 4, 4),
            &pts,
            &Paint::Solid(Style::DEFAULT),
        );
        // A full-height bar lights its whole column.
        let lit = lit_cells(&f);
        let col: Vec<u16> = lit.iter().map(|&(_, y)| y).collect();
        assert_eq!(col, vec![0, 1, 2, 3]);
    }

    #[test]
    fn solid_paint_is_a_passthrough() {
        let s = Style::fg(crate::render::frame::Color::Rgb(1, 2, 3));
        // Solid ignores position and returns exactly its style.
        assert_eq!(Paint::Solid(s).style_at(0.0, 0.0), s);
        assert_eq!(Paint::Solid(s).style_at(1.0, 1.0), s);
    }

    #[test]
    fn by_category_selects_by_x_slot() {
        use crate::render::frame::Color;
        const CATS: [Color; 4] = [
            Color::Rgb(1, 0, 0),
            Color::Rgb(2, 0, 0),
            Color::Rgb(3, 0, 0),
            Color::Rgb(4, 0, 0),
        ];
        let p = Paint::ByCategory(&CATS);
        // Each quarter of the width maps to its category; 1.0 clamps to the last.
        assert_eq!(p.style_at(0.10, 0.5).fg, CATS[0]);
        assert_eq!(p.style_at(0.35, 0.5).fg, CATS[1]);
        assert_eq!(p.style_at(0.60, 0.5).fg, CATS[2]);
        assert_eq!(p.style_at(0.90, 0.5).fg, CATS[3]);
        assert_eq!(p.style_at(1.00, 0.5).fg, CATS[3]);
    }

    #[test]
    fn vgradient_moves_from_lo_at_bottom_to_hi_at_top() {
        use crate::render::frame::Color;
        let p = Paint::VGradient {
            lo: Color::Rgb(0, 0, 0),
            hi: Color::Rgb(0, 0, 255),
        };
        assert_eq!(p.style_at(0.5, 0.0).fg, Color::Rgb(0, 0, 0));
        assert_eq!(p.style_at(0.5, 1.0).fg, Color::Rgb(0, 0, 255));
    }

    proptest! {
        /// Colour is cosmetic: for any points/rect, a coloured `Paint` lights the
        /// exact same cells as `Solid(DEFAULT)` — the one-contract invariant is
        /// independent of `Paint`.
        #[test]
        fn colour_never_moves_a_cell(
            points in proptest::collection::vec(
                (0.0f32..1.0, 0.0f32..1.0).prop_map(|(x, y)| PlotPoint { x, y }),
                0..80,
            ),
            w in 1u16..24, h in 1u16..10,
        ) {
            use crate::render::frame::Color;
            let a = area(0, 0, w, h);
            let paints = [
                Paint::Solid(Style::DEFAULT),
                Paint::VGradient { lo: Color::Rgb(0,0,0), hi: Color::Rgb(255,255,255) },
                Paint::HRamp(&crate::theme::GAUGE_RAMP),
                Paint::ByCategory(&crate::theme::BREAKDOWN),
            ];
            let solid_cells = {
                let mut f = Frame::new(Size::new(24, 10));
                draw_braille(&mut f, a, &points, &Paint::Solid(Style::DEFAULT));
                lit_cells(&f)
            };
            for p in &paints {
                let mut fb = Frame::new(Size::new(24, 10));
                draw_braille(&mut fb, a, &points, p);
                let mut fk = Frame::new(Size::new(24, 10));
                draw_blocks(&mut fk, a, &points, p);
                prop_assert_eq!(lit_cells(&fb), solid_cells.clone());
                prop_assert_eq!(lit_cells(&fk), solid_cells.clone());
            }
        }
    }

    proptest! {
        /// Plotted cells never exceed their rect, for arbitrary points and rects
        /// (PRD-M4 testing decision).
        #[test]
        fn plotted_cells_never_exceed_rect(
            points in proptest::collection::vec(
                (proptest::num::f32::ANY, proptest::num::f32::ANY)
                    .prop_map(|(x, y)| PlotPoint { x, y }),
                0..200,
            ),
            ax in 0u16..20, ay in 0u16..20, w in 0u16..30, h in 0u16..20,
        ) {
            let a = area(ax, ay, w, h);
            let mut f = Frame::new(Size::new(64, 48));
            draw_braille(&mut f, a, &points, &Paint::Solid(Style::DEFAULT));
            draw_blocks(&mut f, a, &points, &Paint::Solid(Style::DEFAULT));
            for (x, y) in lit_cells(&f) {
                prop_assert!(x >= a.x && x < a.x + a.w, "x {x} outside rect {a:?}");
                prop_assert!(y >= a.y && y < a.y + a.h, "y {y} outside rect {a:?}");
            }
        }

        /// The tiers correspond cell-for-cell for arbitrary in-range series.
        #[test]
        fn tiers_correspond_cell_for_cell(
            values in proptest::collection::vec(0.0f64..1000.0, 1..40),
            w in 1u16..24, h in 1u16..10,
        ) {
            let max = autoscale_max(&values);
            let pts = sparkline_points(&values, max);
            let a = area(0, 0, w, h);
            let mut fb = Frame::new(Size::new(24, 10));
            draw_braille(&mut fb, a, &pts, &Paint::Solid(Style::DEFAULT));
            let mut fk = Frame::new(Size::new(24, 10));
            draw_blocks(&mut fk, a, &pts, &Paint::Solid(Style::DEFAULT));
            prop_assert_eq!(lit_cells(&fb), lit_cells(&fk));
        }

        /// Widget generators are total: any finite series yields in-range points.
        #[test]
        fn generators_yield_normalized_points(
            values in proptest::collection::vec(-10.0f64..1e12, 0..50),
        ) {
            let max = autoscale_max(&values);
            for p in sparkline_points(&values, max)
                .into_iter()
                .chain(bar_points(&values, max, 8))
                .chain(line_points(&values, max, 32))
                .chain(gauge_points(0.7, 16, 2))
            {
                prop_assert!((0.0..=1.0).contains(&p.x));
                prop_assert!((0.0..=1.0).contains(&p.y));
            }
        }
    }
}
