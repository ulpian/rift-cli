//! The hi-res coloured plot tier (`#[cfg(feature = "xre-3d")]`).
//!
//! An additive enhancement over the braille floor: instead of one glyph colour
//! per cell, the plot points are rasterized into an `xre_render::SampleBuffer`
//! (2×4 samples per cell) with **per-sample** colour from the [`Paint`], then
//! resolved through the `HalfBlock` density shader — `▀` with the top-half colour
//! in the foreground and the bottom-half colour in the background, doubling the
//! vertical *colour* resolution (the truecolor showpiece). The braille/blocks
//! floor stays the cross-terminal shape reference; this tier only lights on
//! `xre-3d` builds whose caps advertise `MESH_3D`, so it is a pure superset.
//!
//! Like the floor, this writes into the Canopy [`Frame`]; `present` is unchanged.

use xre_core::math::UVec2;
use xre_core::{Cell as XCell, Color as XColor};
use xre_render::{Braille, CellShader, HalfBlock, Sample, SampleBuffer};

use super::frame::{Color, Frame, Mods, Style};
use crate::layout::Rect;
use crate::plot::{Paint, PlotPoint};

/// Sub-cell samples per cell: 2 wide × 4 tall (matches the braille grid; the
/// `HalfBlock` shader splits the 4 rows into a top/bottom colour band).
const SX: u32 = 2;
const SY: u32 = 4;

/// Draw `points` into `area` of `frame` at hi-res: rasterize into a sample buffer
/// coloured per-sample by `paint`, then resolve with `HalfBlock`. Lights the same
/// region the floor would, only with richer colour; empty cells are left
/// untouched (so panel text/placeholders below show through).
pub(crate) fn draw(points: &[PlotPoint], area: Rect, paint: &Paint, frame: &mut Frame) {
    if area.w == 0 || area.h == 0 {
        return;
    }
    let mut buf = SampleBuffer::new(UVec2::new(u32::from(area.w), u32::from(area.h)), SX, SY);
    let width = buf.width();
    let height = buf.height();
    if width == 0 || height == 0 {
        return;
    }
    for p in points {
        if !p.x.is_finite() || !p.y.is_finite() {
            continue;
        }
        if !(0.0..=1.0).contains(&p.x) || !(0.0..=1.0).contains(&p.y) {
            continue;
        }
        let rgb = fg_rgb(paint.style_at(p.x, p.y));
        // Sample coordinates: x rightward, y downward (plot y is bottom-up).
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let sx = ((p.x * (width - 1) as f32).round() as u32).min(width - 1);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let sy = (((1.0 - p.y) * (height - 1) as f32).round() as u32).min(height - 1);
        buf.put(sx, sy, Sample::new(1.0, rgb, 0.0));
    }
    let shader = HalfBlock;
    for cy in 0..area.h {
        for cx in 0..area.w {
            if let Some(cell) = shader.shade(&buf, u32::from(cx), u32::from(cy)) {
                frame.set(area.x + cx, area.y + cy, cell.glyph, from_xre_cell(&cell));
            }
        }
    }
}

/// Draw an actual connected polyline through xRenderEngine's wireframe path and
/// resolve its 2×4 samples with the Braille CellShader. Filled plots continue to
/// use [`draw`] and `HalfBlock`.
pub(crate) fn draw_polyline(vertices: &[PlotPoint], area: Rect, style: Style, frame: &mut Frame) {
    if area.w == 0 || area.h == 0 || vertices.is_empty() {
        return;
    }
    let mut buf = SampleBuffer::new(UVec2::new(u32::from(area.w), u32::from(area.h)), SX, SY);
    let width = buf.width();
    let height = buf.height();
    if width == 0 || height == 0 {
        return;
    }
    let sample = |p: PlotPoint| -> Option<(f32, f32, f32)> {
        if !p.x.is_finite()
            || !p.y.is_finite()
            || !(0.0..=1.0).contains(&p.x)
            || !(0.0..=1.0).contains(&p.y)
        {
            return None;
        }
        Some((
            p.x * (width - 1) as f32,
            (1.0 - p.y) * (height - 1) as f32,
            0.0,
        ))
    };
    let rgb = fg_rgb(style);
    if let [only] = vertices {
        if let Some((x, y, z)) = sample(*only) {
            buf.point(x, y, Sample::new(1.0, rgb, z));
        }
    } else {
        for pair in vertices.windows(2) {
            let Some((&a, &b)) = pair.first().zip(pair.get(1)) else {
                continue;
            };
            let Some((a, b)) = crate::plot::clip_segment(a, b) else {
                continue;
            };
            if let (Some(a), Some(b)) = (sample(a), sample(b)) {
                buf.line(a, b, rgb, 1.0);
            }
        }
    }
    let shader = Braille::default();
    for cy in 0..area.h {
        for cx in 0..area.w {
            if let Some(cell) = shader.shade(&buf, u32::from(cx), u32::from(cy)) {
                frame.set(area.x + cx, area.y + cy, cell.glyph, from_xre_cell(&cell));
            }
        }
    }
}

/// The RGB triple of a style's foreground, with a light-grey fallback for the
/// (unused-by-graphs) default/indexed cases so a sample is always coloured.
fn fg_rgb(style: Style) -> [u8; 3] {
    match style.fg {
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Default | Color::Indexed(_) => [0xc8, 0xc8, 0xc8],
    }
}

/// Convert a shaded `xre_core::Cell`'s colours/attrs into a Canopy [`Style`].
fn from_xre_cell(c: &XCell) -> Style {
    Style {
        fg: from_xre_color(c.fg),
        bg: from_xre_color(c.bg),
        mods: from_xre_attrs(c.attrs),
    }
}

/// Map an `xre_core::Color` back onto Canopy's [`Color`].
fn from_xre_color(c: XColor) -> Color {
    match c {
        XColor::Default => Color::Default,
        XColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
        XColor::Ansi16(i) | XColor::Ansi256(i) => Color::Indexed(i),
    }
}

/// Map `xre_core::Attrs` back onto Canopy [`Mods`] (italic has no Canopy mod and
/// is dropped; `HalfBlock` sets no attributes anyway).
fn from_xre_attrs(a: xre_core::Attrs) -> Mods {
    let mut m = Mods::NONE;
    if a.contains(xre_core::Attrs::BOLD) {
        m = m.with(Mods::BOLD);
    }
    if a.contains(xre_core::Attrs::DIM) {
        m = m.with(Mods::DIM);
    }
    if a.contains(xre_core::Attrs::UNDERLINE) {
        m = m.with(Mods::UNDERLINE);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;

    #[test]
    fn gauge_fill_draws_coloured_halfblocks() {
        // A full-width gauge fill must light half-block glyphs coloured by the
        // ramp (RGB), leaving no cell terminal-default.
        let pts = crate::plot::gauge_points(1.0, 16, 4);
        let mut f = Frame::new(Size::new(10, 3));
        let area = Rect {
            x: 0,
            y: 0,
            w: 8,
            h: 2,
        };
        draw(&pts, area, &Paint::HRamp(&crate::theme::GAUGE_RAMP), &mut f);
        let mut lit = 0;
        for y in 0..f.height() {
            for x in 0..f.width() {
                if let Some(c) = f.get(x, y) {
                    if matches!(c.ch, '▀' | '▄') {
                        assert!(matches!(c.style.fg, Color::Rgb(_, _, _)), "fg is RGB");
                        lit += 1;
                    }
                }
            }
        }
        assert!(lit > 0, "the hi-res gauge drew no half-block cells");
    }

    #[test]
    fn empty_points_and_degenerate_area_are_safe() {
        let mut f = Frame::new(Size::new(6, 3));
        draw(
            &[],
            Rect {
                x: 0,
                y: 0,
                w: 6,
                h: 3,
            },
            &Paint::Solid(Style::DEFAULT),
            &mut f,
        );
        draw(
            &[PlotPoint { x: 0.5, y: 0.5 }],
            Rect {
                x: 0,
                y: 0,
                w: 0,
                h: 0,
            },
            &Paint::Solid(Style::DEFAULT),
            &mut f,
        );
        // Nothing lit; no panic.
        for y in 0..f.height() {
            for x in 0..f.width() {
                assert_eq!(f.get(x, y).map(|c| c.ch), Some(' '));
            }
        }
    }

    #[test]
    fn polyline_resolves_through_braille_shader() {
        let mut frame = Frame::new(Size::new(12, 5));
        draw_polyline(
            &[
                PlotPoint { x: 0.0, y: 0.0 },
                PlotPoint { x: 0.5, y: 1.0 },
                PlotPoint { x: 1.0, y: 0.0 },
            ],
            Rect {
                x: 1,
                y: 1,
                w: 10,
                h: 3,
            },
            Style::fg(Color::Rgb(0x5c, 0xc4, 0xe0)),
            &mut frame,
        );
        let lit: Vec<_> = frame.cells().iter().filter(|cell| cell.ch != ' ').collect();
        assert!(!lit.is_empty());
        assert!(lit
            .iter()
            .all(|cell| (0x2800..=0x28ff).contains(&(cell.ch as u32))));
    }
}
