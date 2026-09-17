//! Engine-neutral sub-cell samples and the portable density-shader floor.
//!
//! Heat strips and treemaps describe a 2x4 sample field without naming an xRE
//! type. The standard TUI resolves that field with xRenderEngine; the default
//! trait implementation here gives ASCII/`NO_COLOR` builds the same occupied
//! cells and monotone density without pulling the engine dependency in.

use std::collections::BTreeMap;

use super::{Color, Frame, Style};
use crate::layout::Rect;

/// Draw the shared treemap boundary and return its drawable interior.
///
/// The boundary is geometry, not colour decoration: it remains visible in
/// ASCII and `NO_COLOR` tiers and is also the honest zero-total state.
pub(crate) fn treemap_outline(frame: &mut Frame, rect: Rect, style: Style, unicode: bool) -> Rect {
    if rect.w < 2 || rect.h < 2 {
        return rect;
    }
    let (tl, tr, bl, br, horizontal, vertical) = if unicode {
        ('┌', '┐', '└', '┘', '─', '│')
    } else {
        ('+', '+', '+', '+', '-', '|')
    };
    let right = rect.x + rect.w - 1;
    let bottom = rect.y + rect.h - 1;
    frame.set(rect.x, rect.y, tl, style);
    frame.set(right, rect.y, tr, style);
    frame.set(rect.x, bottom, bl, style);
    frame.set(right, bottom, br, style);
    for x in rect.x + 1..right {
        frame.set(x, rect.y, horizontal, style);
        frame.set(x, bottom, horizontal, style);
    }
    for y in rect.y + 1..bottom {
        frame.set(rect.x, y, vertical, style);
        frame.set(right, y, vertical, style);
    }
    Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        w: rect.w - 2,
        h: rect.h - 2,
    }
}

/// The cell shader a chart prefers for an engine-neutral sample field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellShaderKind {
    /// A 2x4 dot matrix; density remains legible without colour.
    Braille,
    /// A printable glyph ramp selected from average sample luminance.
    LuminanceRamp,
}

/// One normalized sub-cell sample.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ShadeSample {
    /// Horizontal position in `[0, 1]`, left to right.
    pub x: f32,
    /// Vertical position in `[0, 1]`, bottom to top.
    pub y: f32,
    /// Luminance in `[0, 1]`; non-finite values are ignored.
    pub luma: f32,
    /// Resolved category colour. `Default` keeps `NO_COLOR` colourless.
    pub color: Color,
}

const SX: u32 = 2;
const SY: u32 = 4;
const SAMPLE_COUNT: usize = (SX * SY) as usize;
const DENSITY: &[u8] = b" .:-=+*#%@";

#[derive(Clone, Copy)]
struct SampleCell {
    luma: [f32; SAMPLE_COUNT],
    color: [Color; SAMPLE_COUNT],
    filled: [bool; SAMPLE_COUNT],
}

impl Default for SampleCell {
    fn default() -> Self {
        Self {
            luma: [0.0; SAMPLE_COUNT],
            color: [Color::Default; SAMPLE_COUNT],
            filled: [false; SAMPLE_COUNT],
        }
    }
}

/// Draw the portable Braille/ramp floor used by the default trait method.
pub(crate) fn draw_portable(
    samples: &[ShadeSample],
    area: Rect,
    shader: CellShaderKind,
    frame: &mut Frame,
) {
    if area.w == 0 || area.h == 0 {
        return;
    }
    let mut cells: BTreeMap<(u16, u16), SampleCell> = BTreeMap::new();
    for sample in samples {
        let Some((cx, cy, dx, dy)) = sample_slot(*sample, area) else {
            continue;
        };
        let idx = usize::try_from(dy * SX + dx).unwrap_or(0);
        let cell = cells.entry((cx, cy)).or_default();
        let (Some(filled), Some(luma), Some(color)) = (
            cell.filled.get_mut(idx),
            cell.luma.get_mut(idx),
            cell.color.get_mut(idx),
        ) else {
            continue;
        };
        *filled = true;
        *luma = sample.luma.clamp(0.0, 1.0);
        *color = sample.color;
    }

    for ((cx, cy), cell) in cells {
        let resolved = match shader {
            CellShaderKind::Braille => resolve_braille(&cell),
            CellShaderKind::LuminanceRamp => resolve_ramp(&cell),
        };
        if let Some((glyph, style)) = resolved {
            frame.set(area.x + cx, area.y + cy, glyph, style);
        }
    }
}

fn sample_slot(sample: ShadeSample, area: Rect) -> Option<(u16, u16, u32, u32)> {
    if !sample.x.is_finite()
        || !sample.y.is_finite()
        || !sample.luma.is_finite()
        || !(0.0..=1.0).contains(&sample.x)
        || !(0.0..=1.0).contains(&sample.y)
    {
        return None;
    }
    let width = u32::from(area.w) * SX;
    let height = u32::from(area.h) * SY;
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let sx = ((sample.x * width as f32) as u32).min(width - 1);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let sy_up = ((sample.y * height as f32) as u32).min(height - 1);
    let sy = height - 1 - sy_up;
    Some((
        u16::try_from(sx / SX).ok()?,
        u16::try_from(sy / SY).ok()?,
        sx % SX,
        sy % SY,
    ))
}

fn resolve_braille(cell: &SampleCell) -> Option<(char, Style)> {
    const BITS: [u8; SAMPLE_COUNT] = [0x01, 0x08, 0x02, 0x10, 0x04, 0x20, 0x40, 0x80];
    let mut mask = 0u8;
    let mut colors = Vec::new();
    for (((filled, luma), color), bit) in cell
        .filled
        .iter()
        .zip(&cell.luma)
        .zip(&cell.color)
        .zip(BITS)
    {
        if *filled && *luma >= 0.5 {
            mask |= bit;
            colors.push(*color);
        }
    }
    if mask == 0 {
        return None;
    }
    let glyph = char::from_u32(0x2800 + u32::from(mask)).unwrap_or('⣿');
    Some((glyph, Style::fg(mean_color(&colors))))
}

fn resolve_ramp(cell: &SampleCell) -> Option<(char, Style)> {
    let mut sum = 0.0f32;
    let mut colors = Vec::new();
    for ((filled, luma), color) in cell.filled.iter().zip(&cell.luma).zip(&cell.color) {
        if *filled {
            sum += *luma;
            colors.push(*color);
        }
    }
    if colors.is_empty() {
        return None;
    }
    let coverage = (sum / SAMPLE_COUNT as f32).clamp(0.0, 1.0);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let idx = (coverage * (DENSITY.len() - 1) as f32).round() as usize;
    let glyph = char::from(*DENSITY.get(idx).unwrap_or(&b' '));
    Some((glyph, Style::fg(mean_color(&colors))))
}

fn mean_color(colors: &[Color]) -> Color {
    let mut red = 0u32;
    let mut green = 0u32;
    let mut blue = 0u32;
    let mut count = 0u32;
    for color in colors {
        if let Color::Rgb(r, g, b) = color {
            red += u32::from(*r);
            green += u32::from(*g);
            blue += u32::from(*b);
            count += 1;
        }
    }
    let averaged = [red, green, blue].map(|channel| {
        channel
            .checked_div(count)
            .and_then(|value| u8::try_from(value).ok())
    });
    match averaged {
        [Some(r), Some(g), Some(b)] => Color::Rgb(r, g, b),
        _ => Color::Default,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Size;

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        }
    }

    #[test]
    fn braille_density_is_monotone_and_colour_is_preserved() {
        let red = Color::Rgb(200, 10, 10);
        let mut frame = Frame::new(Size::new(1, 1));
        draw_portable(
            &[ShadeSample {
                x: 0.1,
                y: 0.9,
                luma: 1.0,
                color: red,
            }],
            area(),
            CellShaderKind::Braille,
            &mut frame,
        );
        let one = frame.get(0, 0).unwrap_or_default();
        assert!((0x2800..=0x28ff).contains(&u32::from(one.ch)));
        assert_eq!(one.style.fg, red);

        let full: Vec<_> = (0..8)
            .map(|i| ShadeSample {
                x: if i % 2 == 0 { 0.1 } else { 0.9 },
                y: (3.5 - (i / 2) as f32) / 4.0,
                luma: 1.0,
                color: red,
            })
            .collect();
        let mut full_frame = Frame::new(Size::new(1, 1));
        draw_portable(&full, area(), CellShaderKind::Braille, &mut full_frame);
        assert_eq!(full_frame.get(0, 0).map(|c| c.ch), Some('⣿'));
    }

    #[test]
    fn ramp_and_invalid_samples_are_total() {
        let samples = [
            ShadeSample {
                x: 0.5,
                y: 0.5,
                luma: 0.82,
                color: Color::Default,
            },
            ShadeSample {
                x: f32::NAN,
                y: 2.0,
                luma: 1.0,
                color: Color::Default,
            },
        ];
        let mut frame = Frame::new(Size::new(1, 1));
        draw_portable(&samples, area(), CellShaderKind::LuminanceRamp, &mut frame);
        assert_ne!(frame.get(0, 0).map(|c| c.ch), Some(' '));
        assert_eq!(frame.get(0, 0).map(|c| c.style.fg), Some(Color::Default));
    }
}
