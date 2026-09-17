//! axis.rs — the shared cartesian axis frame.
//!
//! Canopy's bar/line charts rasterize normalized points into a bare rectangle —
//! shape without magnitude. This helper adds a light quantitative frame *around*
//! the plot (the raster is untouched, the mesh splash tier): a left **Y-tick gutter**
//! (min / mid / max, humanized), a bottom **X-label row** (categorical labels or
//! positioned time ticks), and a **legend key** (series colour → name). It reserves the gutter
//! columns + the label row (+ a legend row when a legend is given) and returns
//! the shrunk plot [`Rect`] the bars fill — the same "shrink the rect to leave a
//! label row" the activity card already made, generalized.
//!
//! Density is directional (the chart-axis rule): three Y ticks, **no gridlines** behind the
//! bars. Axis cells are **chrome** (labels/legend), drawn outside the plot rect;
//! bars are **data**, drawn inside it — so the axis never lights a data cell and
//! the paint-never-moves-a-lit-cell property holds across the frame. The empty
//! case still draws the axis and a `0` baseline (placeholder-not-fake-zero).
//! Applied to Canopy's cartesian charts only; the radial gauges are excluded.

use crate::layout::Rect;
use crate::render::frame::{Color, Frame, Style};
use crate::theme::Theme;

/// The smallest Y-tick gutter (columns) — a `0` baseline still needs a column.
pub const MIN_GUTTER: u16 = 3;
/// The widest Y-tick gutter — a humanized label (`1.6M`) is ~4 cols; capped so a
/// wide tick can never eat the bars (the fog note's ceiling).
pub const MAX_GUTTER: u16 = 6;

/// One explicitly positioned X-axis tick for time-series charts.
#[derive(Clone, Copy, Debug)]
pub struct XTick<'a> {
    /// Normalized horizontal position, clamped to `0..=1`.
    pub position: f32,
    /// Short terminal label.
    pub label: &'a str,
}

/// The two X-axis layouts used by Canopy's cartesian charts.
pub enum XAxis<'a> {
    /// One evenly strided label per categorical bar group.
    Categories(&'a [String]),
    /// Explicit normalized positions for time-series ticks.
    Positions(&'a [XTick<'a>]),
}

/// A cartesian chart's axis frame. The Y ticks are derived from `max`
/// via `humanize` (min = 0, mid = max/2, max); `x_axis` selects categorical or
/// positioned labels; `legend` (colour → name) draws a key row above the chart.
pub struct AxisSpec<'a> {
    /// The data ceiling (the bars' autoscaled max) — the top Y tick.
    pub max: f64,
    /// Humanizes a Y value into a short label (e.g. `col3::fmt_km`).
    pub humanize: fn(f64) -> String,
    /// Category labels or explicit positional ticks.
    pub x_axis: XAxis<'a>,
    /// The legend key: (colour, name) per series; empty draws no legend row.
    pub legend: &'a [(Color, &'a str)],
}

impl AxisSpec<'_> {
    /// The three Y ticks, top → bottom: `[max, mid, 0]` (humanized). The bottom is
    /// always a real `0` baseline (placeholder-not-fake-zero).
    #[must_use]
    pub fn y_ticks(&self) -> [String; 3] {
        let h = self.humanize;
        [h(self.max), h(self.max / 2.0), h(0.0)]
    }

    /// The reserved gutter width: the widest tick + 1 pad, clamped to the band.
    #[must_use]
    pub fn gutter(&self) -> u16 {
        let widest = self
            .y_ticks()
            .iter()
            .map(|t| t.chars().count())
            .max()
            .unwrap_or(1);
        (u16::try_from(widest)
            .unwrap_or(MIN_GUTTER)
            .saturating_add(1))
        .clamp(MIN_GUTTER, MAX_GUTTER)
    }

    /// Whether a legend row is reserved (a non-empty legend).
    #[must_use]
    pub fn legend_rows(&self) -> u16 {
        u16::from(!self.legend.is_empty())
    }
}

/// The plot rect the bars fill after the axis frame is reserved within `r`: a
/// legend row off the top (when present), the X-label row off the bottom, and
/// the Y-tick gutter off the left. Pure geometry — shared with [`draw`] and
/// pinned by a property test so the reserved budget can't drift.
#[must_use]
pub fn plot_rect(r: Rect, spec: &AxisSpec<'_>) -> Rect {
    let g = spec.gutter();
    let legend = spec.legend_rows();
    Rect {
        x: r.x.saturating_add(g),
        y: r.y.saturating_add(legend),
        w: r.w.saturating_sub(g),
        h: r.h.saturating_sub(legend).saturating_sub(1), // minus the X-label row
    }
}

/// A colour swatch glyph for a legend key (a filled block; ASCII floor `#`).
fn swatch(unicode: bool) -> char {
    if unicode {
        '\u{25A0}' // ■
    } else {
        '#'
    }
}

/// Draw the axis chrome (legend row, Y-tick gutter, X-label row) around the plot
/// and return the plot [`Rect`] the caller rasterizes the bars into. Every cell
/// this writes is chrome, outside the returned plot rect. A degenerate `r` (too
/// small for a gutter + a plot column) draws nothing and returns `r` unchanged.
pub fn draw(frame: &mut Frame, theme: Theme, r: Rect, spec: &AxisSpec<'_>, unicode: bool) -> Rect {
    let plot = plot_rect(r, spec);
    if plot.w == 0 || plot.h == 0 || r.w <= spec.gutter() {
        return r;
    }
    // The legend key on the top row (colour swatch + name), left to right.
    if spec.legend_rows() == 1 {
        let mut lx = r.x;
        let sw = swatch(unicode);
        for (colour, name) in spec.legend {
            if lx >= r.x.saturating_add(r.w) {
                break;
            }
            frame.set(lx, r.y, sw, Style::fg(*colour));
            lx = frame.put_str(
                lx.saturating_add(1),
                r.y,
                &format!(" {name}  "),
                theme.dim(),
            );
        }
    }
    // The Y-tick gutter: max at the top plot row, mid at the middle, 0 at the
    // bottom — right-aligned in the gutter columns (chrome, left of the bars).
    let ticks = spec.y_ticks();
    let gutter = spec.gutter();
    let rows = [
        (plot.y, &ticks[0]),
        (plot.y + plot.h / 2, &ticks[1]),
        (plot.y + plot.h.saturating_sub(1), &ticks[2]),
    ];
    for (ty, label) in rows {
        let w = u16::try_from(label.chars().count())
            .unwrap_or(0)
            .min(gutter);
        let tx = r.x + gutter.saturating_sub(w).saturating_sub(1);
        frame.put_str(tx, ty, label, theme.dim());
    }
    // The X-label row on the bottom: either one label per categorical group or
    // explicitly positioned time ticks.
    let label_row = r.y + r.h.saturating_sub(1);
    match &spec.x_axis {
        XAxis::Categories(labels) if !labels.is_empty() => {
            let n = u16::try_from(labels.len()).unwrap_or(1).max(1);
            let stride = (plot.w / n).max(1);
            for (i, label) in labels.iter().enumerate() {
                let Ok(iu) = u16::try_from(i) else { break };
                let x = plot.x + iu * stride;
                if x >= plot.x + plot.w {
                    break;
                }
                let clipped: String = label.chars().take(usize::from(stride)).collect();
                frame.put_str(x, label_row, &clipped, theme.dim());
            }
        }
        XAxis::Positions(ticks) => {
            let right = plot.x.saturating_add(plot.w);
            for tick in *ticks {
                let position = if tick.position.is_finite() {
                    tick.position.clamp(0.0, 1.0)
                } else {
                    0.0
                };
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let center =
                    plot.x + (position * f32::from(plot.w.saturating_sub(1))).round() as u16;
                let label_w = u16::try_from(tick.label.chars().count())
                    .unwrap_or(plot.w)
                    .min(plot.w);
                let x = center
                    .saturating_sub(label_w / 2)
                    .clamp(plot.x, right.saturating_sub(label_w));
                let clipped: String = tick.label.chars().take(usize::from(label_w)).collect();
                frame.put_str(x, label_row, &clipped, theme.dim());
            }
        }
        XAxis::Categories(_) => {}
    }
    plot
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::col3::fmt_km;
    use crate::render::frame::Size;
    use crate::theme::BREAKDOWN;

    fn spec_tokens<'a>(max: f64, x: &'a [String], legend: &'a [(Color, &'a str)]) -> AxisSpec<'a> {
        AxisSpec {
            max,
            humanize: fmt_km,
            x_axis: XAxis::Categories(x),
            legend,
        }
    }

    #[test]
    fn gutter_and_plot_reserve_a_fixed_budget() {
        // A property: the plot rect never exceeds the card, and the axis reserves
        // exactly the gutter columns + the label row (+ a legend row).
        let r = Rect {
            x: 2,
            y: 1,
            w: 30,
            h: 10,
        };
        let x = vec!["in".to_string(), "rd".into(), "out".into(), "ca".into()];
        let legend = [(BREAKDOWN[0], "input"), (BREAKDOWN[1], "read")];
        let spec = spec_tokens(1_600_000.0, &x, &legend);
        let plot = plot_rect(r, &spec);
        assert_eq!(plot.x, r.x + spec.gutter(), "gutter reserved on the left");
        assert_eq!(plot.y, r.y + 1, "legend row reserved on top");
        assert_eq!(plot.h, r.h - 1 - 1, "legend + label rows reserved");
        assert!(plot.x + plot.w <= r.x + r.w, "plot stays inside the card");
        assert!(plot.y + plot.h <= r.y + r.h);
        // The gutter clamps: a 1.6M label is wide but capped at MAX_GUTTER.
        assert!(spec.gutter() <= MAX_GUTTER && spec.gutter() >= MIN_GUTTER);
    }

    #[test]
    fn y_ticks_are_humanized_and_monotonic() {
        let x: Vec<String> = vec![];
        let spec = spec_tokens(1_000_000.0, &x, &[]);
        let t = spec.y_ticks();
        assert_eq!(t[0], "1.0M");
        assert_eq!(t[2], "0", "the baseline is a real zero");
        // No legend ⇒ no legend row reserved.
        assert_eq!(spec.legend_rows(), 0);
    }

    #[test]
    fn the_empty_case_draws_the_axis_and_a_zero_baseline() {
        // max = the autoscale floor (1), zero bars → the axis + `0` still render.
        let mut f = Frame::new(Size::new(24, 8));
        let x = vec!["M".to_string(), "T".into(), "W".into()];
        let legend = [(BREAKDOWN[0], "ev")];
        let spec = spec_tokens(1.0, &x, &legend);
        let r = Rect {
            x: 0,
            y: 0,
            w: 24,
            h: 8,
        };
        let plot = draw(&mut f, Theme::rift_dark(), r, &spec, true);
        let text: String = (0..f.height())
            .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
            .map(|(x, y)| f.get(x, y).map_or(' ', |c| c.ch))
            .collect();
        assert!(text.contains('0'), "the zero baseline tick renders");
        assert!(text.contains("ev"), "the legend renders");
        assert!(text.contains('M'), "the X labels render");
        assert!(plot.w > 0 && plot.h > 0, "a usable plot remains");
    }

    #[test]
    fn axis_labels_never_land_inside_the_plot_rect() {
        // Paint-never-moves extended: every lit axis cell is OUTSIDE the plot rect
        // (labels are chrome; bars own the plot).
        let mut f = Frame::new(Size::new(30, 10));
        let x = vec!["in".to_string(), "rd".into(), "out".into(), "ca".into()];
        let legend = [(BREAKDOWN[0], "input"), (BREAKDOWN[1], "read")];
        let spec = spec_tokens(9000.0, &x, &legend);
        let r = Rect {
            x: 0,
            y: 0,
            w: 30,
            h: 10,
        };
        let plot = draw(&mut f, Theme::rift_dark(), r, &spec, true);
        for y in 0..f.height() {
            for x in 0..f.width() {
                if f.get(x, y).is_some_and(|c| c.ch != ' ') {
                    let inside =
                        x >= plot.x && x < plot.x + plot.w && y >= plot.y && y < plot.y + plot.h;
                    assert!(!inside, "an axis glyph landed in the plot at ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn positioned_time_ticks_anchor_left_middle_and_right() {
        let mut frame = Frame::new(Size::new(30, 8));
        let ticks = [
            XTick {
                position: 0.0,
                label: "-30m",
            },
            XTick {
                position: 0.5,
                label: "-15m",
            },
            XTick {
                position: 1.0,
                label: "last",
            },
        ];
        let spec = AxisSpec {
            max: 4.0,
            humanize: fmt_km,
            x_axis: XAxis::Positions(&ticks),
            legend: &[],
        };
        let rect = Rect {
            x: 0,
            y: 0,
            w: 30,
            h: 8,
        };
        let plot = draw(&mut frame, Theme::rift_dark(), rect, &spec, true);
        let row: String = (0..frame.width())
            .map(|x| frame.get(x, 7).map_or(' ', |c| c.ch))
            .collect();
        assert!(row.contains("-30m"));
        assert!(row.contains("-15m"));
        assert!(row.contains("last"));
        assert!(plot.h < rect.h, "tick chrome reserves a row");
    }
}
