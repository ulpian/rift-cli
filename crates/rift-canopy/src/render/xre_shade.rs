//! xRenderEngine resolution of Canopy's engine-neutral 2x4 sample field.

use xre_core::math::UVec2;
use xre_core::{Cell as XCell, Color as XColor};
use xre_render::{resolve_cells, CellShader, Sample, SampleBuffer};

use super::{Color, Frame, Mods, ShadeSample, Style};
use crate::layout::Rect;

const SX: u32 = 2;
const SY: u32 = 4;

pub(crate) fn draw(
    samples: &[ShadeSample],
    area: Rect,
    shader: &dyn CellShader,
    buffer: &mut SampleBuffer,
    resolved: &mut Vec<Option<XCell>>,
    frame: &mut Frame,
) {
    if area.w == 0 || area.h == 0 {
        return;
    }
    buffer.resize(UVec2::new(u32::from(area.w), u32::from(area.h)), SX, SY);
    buffer.clear([0, 0, 0]);
    let width = buffer.width();
    let height = buffer.height();
    for sample in samples {
        if !sample.x.is_finite()
            || !sample.y.is_finite()
            || !sample.luma.is_finite()
            || !(0.0..=1.0).contains(&sample.x)
            || !(0.0..=1.0).contains(&sample.y)
        {
            continue;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let x = ((sample.x * width as f32) as u32).min(width - 1);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let y_up = ((sample.y * height as f32) as u32).min(height - 1);
        let y = height - 1 - y_up;
        buffer.put(
            x,
            y,
            Sample::new(sample.luma.clamp(0.0, 1.0), rgb(sample.color), 0.0),
        );
    }
    let count = usize::from(area.w) * usize::from(area.h);
    resolved.resize(count, None);
    resolve_cells(
        buffer,
        shader,
        u32::from(area.w),
        u32::from(area.h),
        resolved,
    );
    for cy in 0..area.h {
        for cx in 0..area.w {
            let idx = usize::from(cy) * usize::from(area.w) + usize::from(cx);
            if let Some(cell) = resolved.get(idx).and_then(|cell| *cell) {
                frame.set(area.x + cx, area.y + cy, cell.glyph, from_xre_cell(cell));
            }
        }
    }
}

fn rgb(color: Color) -> [u8; 3] {
    match color {
        Color::Rgb(r, g, b) => [r, g, b],
        Color::Indexed(i) => [i, i, i],
        Color::Default => [0xc8, 0xc8, 0xc8],
    }
}

fn from_xre_cell(cell: XCell) -> Style {
    let mut mods = Mods::NONE;
    if cell.attrs.contains(xre_core::Attrs::BOLD) {
        mods = mods.with(Mods::BOLD);
    }
    if cell.attrs.contains(xre_core::Attrs::DIM) {
        mods = mods.with(Mods::DIM);
    }
    if cell.attrs.contains(xre_core::Attrs::UNDERLINE) {
        mods = mods.with(Mods::UNDERLINE);
    }
    Style {
        fg: match cell.fg {
            XColor::Default => Color::Default,
            XColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
            XColor::Ansi16(i) | XColor::Ansi256(i) => Color::Indexed(i),
        },
        bg: match cell.bg {
            XColor::Default => Color::Default,
            XColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
            XColor::Ansi16(i) | XColor::Ansi256(i) => Color::Indexed(i),
        },
        mods,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{CellShaderKind, Size};
    use std::fmt::Write as _;
    use xre_render::{Braille, LuminanceRamp};

    fn render(shader: CellShaderKind) -> Frame {
        let mut buffer = SampleBuffer::new(UVec2::new(0, 0), SX, SY);
        let mut resolved = Vec::new();
        let samples = [ShadeSample {
            x: 0.5,
            y: 0.5,
            luma: 0.82,
            color: Color::Rgb(12, 34, 56),
        }];
        let mut frame = Frame::new(Size::new(1, 1));
        match shader {
            CellShaderKind::Braille => draw(
                &samples,
                frame.rect(),
                &Braille::default(),
                &mut buffer,
                &mut resolved,
                &mut frame,
            ),
            CellShaderKind::LuminanceRamp => draw(
                &samples,
                frame.rect(),
                &LuminanceRamp::default(),
                &mut buffer,
                &mut resolved,
                &mut frame,
            ),
        }
        frame
    }

    #[test]
    fn actual_xre_shaders_resolve_braille_and_ramp() {
        let braille = render(CellShaderKind::Braille);
        assert!(braille
            .get(0, 0)
            .is_some_and(|cell| (0x2800..=0x28ff).contains(&u32::from(cell.ch))));
        let ramp = render(CellShaderKind::LuminanceRamp);
        assert_ne!(ramp.get(0, 0).map(|cell| cell.ch), Some(' '));
    }

    /// One scratch buffer and resolve vector serve every draw below: the
    /// braille cell then the larger ramp block prove the scratch resizes on
    /// reuse, and the chart goldens reuse it again.
    #[test]
    fn xre_cell_shader_golden_and_reusable_scratch_resize() {
        let mut buffer = SampleBuffer::new(UVec2::new(0, 0), SX, SY);
        let mut resolved = Vec::new();

        let braille = braille_cell(&mut buffer, &mut resolved);
        assert_eq!((buffer.width(), buffer.height()), (2, 4));
        assert_eq!(resolved.len(), 1);

        let ramp = ramp_block(&mut buffer, &mut resolved);
        assert_eq!((buffer.width(), buffer.height()), (4, 8));
        assert_eq!(resolved.len(), 4);

        let mut golden = describe("braille", &braille);
        golden.push_str(&describe("ramp", &ramp));
        golden.push_str(&chart_goldens(&mut buffer, &mut resolved));

        let treemap_area = Rect {
            x: 0,
            y: 0,
            w: 12,
            h: 4,
        };
        let zero_leaves = crate::charts::treemap_layout(&[0.0; 4]);
        let zero_samples =
            crate::charts::treemap_samples(&zero_leaves, treemap_area, &crate::theme::BREAKDOWN);
        assert!(zero_samples.is_empty());
        insta::assert_snapshot!("xre_cell_shaders", golden);
    }

    /// One lit braille dot in a single cell.
    fn braille_cell(buffer: &mut SampleBuffer, resolved: &mut Vec<Option<XCell>>) -> Frame {
        let mut braille = Frame::new(Size::new(1, 1));
        draw(
            &[ShadeSample {
                x: 0.25,
                y: 0.875,
                luma: 1.0,
                color: Color::Rgb(12, 34, 56),
            }],
            braille.rect(),
            &Braille::default(),
            buffer,
            resolved,
            &mut braille,
        );
        braille
    }

    /// A 2x2-cell block, every sample lit, red on the left half and blue on
    /// the right, through the luminance ramp.
    fn ramp_block(buffer: &mut SampleBuffer, resolved: &mut Vec<Option<XCell>>) -> Frame {
        let mut ramp_samples = Vec::new();
        for sy in 0..8u32 {
            for sx in 0..4u32 {
                ramp_samples.push(ShadeSample {
                    x: (sx as f32 + 0.5) / 4.0,
                    y: (sy as f32 + 0.5) / 8.0,
                    luma: 0.82,
                    color: if sx < 2 {
                        Color::Rgb(200, 40, 20)
                    } else {
                        Color::Rgb(20, 80, 200)
                    },
                });
            }
        }
        let mut ramp = Frame::new(Size::new(2, 2));
        draw(
            &ramp_samples,
            ramp.rect(),
            &LuminanceRamp::default(),
            buffer,
            resolved,
            &mut ramp,
        );
        ramp
    }

    /// The activity heat strip, then the token and model treemaps, in the
    /// golden's order.
    fn chart_goldens(buffer: &mut SampleBuffer, resolved: &mut Vec<Option<XCell>>) -> String {
        let heat_area = Rect {
            x: 0,
            y: 0,
            w: 15,
            h: 2,
        };
        let heat_values: Vec<u64> = (0..30u64)
            .map(|day| if day == 9 { 100 } else { day * 2 })
            .collect();
        let mut golden = heat_golden(
            &heat_values,
            heat_area,
            crate::theme::Section::Activity.accent(),
            buffer,
            resolved,
        );
        let treemap_area = Rect {
            x: 0,
            y: 0,
            w: 12,
            h: 4,
        };
        golden.push_str(&treemap_golden(
            "token-treemap",
            &[8.0, 4.0, 2.0, 1.0],
            treemap_area,
            &crate::theme::BREAKDOWN,
            buffer,
            resolved,
        ));
        let model_area = Rect {
            x: 0,
            y: 0,
            w: 16,
            h: 5,
        };
        golden.push_str(&treemap_golden(
            "model-treemap",
            &[21.0, 13.0, 8.0, 5.0, 3.0, 2.0],
            model_area,
            &crate::theme::MODEL_USAGE,
            buffer,
            resolved,
        ));
        golden
    }

    fn heat_golden(
        values: &[u64],
        area: Rect,
        color: Color,
        buffer: &mut SampleBuffer,
        resolved: &mut Vec<Option<XCell>>,
    ) -> String {
        let heat = crate::charts::heat_strip(values, area, color);
        let mut frame = Frame::new(Size::new(area.w, area.h));
        draw(
            &heat.samples,
            area,
            &Braille::default(),
            buffer,
            resolved,
            &mut frame,
        );
        glyph_rows("activity-heat", &frame)
    }

    fn treemap_golden(
        label: &str,
        values: &[f64],
        area: Rect,
        palette: &[Color],
        buffer: &mut SampleBuffer,
        resolved: &mut Vec<Option<XCell>>,
    ) -> String {
        let leaves = crate::charts::treemap_layout(values);
        let samples = crate::charts::treemap_samples(&leaves, area, palette);
        let mut frame = Frame::new(Size::new(area.w, area.h));
        draw(
            &samples,
            area,
            &Braille::default(),
            buffer,
            resolved,
            &mut frame,
        );
        let mut out = glyph_rows(label, &frame);
        out.push_str(&color_rows(&format!("{label}-colors"), &frame, palette));
        out
    }

    fn describe(label: &str, frame: &Frame) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{label} {}x{}", frame.width(), frame.height());
        for y in 0..frame.height() {
            for x in 0..frame.width() {
                let cell = frame.get(x, y).unwrap_or_default();
                let _ = writeln!(
                    out,
                    "({x},{y}) U+{:04X} fg={:?} bg={:?}",
                    u32::from(cell.ch),
                    cell.style.fg,
                    cell.style.bg
                );
            }
        }
        out
    }

    fn glyph_rows(label: &str, frame: &Frame) -> String {
        let mut out = format!("{label} {}x{}\n", frame.width(), frame.height());
        for y in 0..frame.height() {
            for x in 0..frame.width() {
                out.push(frame.get(x, y).map_or(' ', |cell| cell.ch));
            }
            out.push('\n');
        }
        out
    }

    fn color_rows(label: &str, frame: &Frame, palette: &[Color]) -> String {
        let mut out = format!("{label}\n");
        for y in 0..frame.height() {
            for x in 0..frame.width() {
                let color = frame.get(x, y).map_or(Color::Default, |cell| cell.style.fg);
                let mark = palette
                    .iter()
                    .position(|candidate| *candidate == color)
                    .and_then(|index| char::from_digit(index as u32, 10))
                    .unwrap_or('.');
                out.push(mark);
            }
            out.push('\n');
        }
        out
    }
}
