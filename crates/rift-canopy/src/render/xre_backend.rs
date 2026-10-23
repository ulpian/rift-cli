//! The xre-backed render backend — full-fidelity terminal present.
//!
//! The live backend and its sibling `xre_shade` are the only modules that name
//! xRE render types. This backend blits a Canopy [`Frame`] into a reused
//! `xre_core::CellBuffer`, resolves chart samples through reusable xRE shader
//! scratch, and flushes through the diffing [`xre_term::Presenter`]. The 3D
//! scene tier remains isolated behind `xre-3d`.

use std::io::Stdout;

use xre_core::math::UVec2;
use xre_core::{Attrs, Cell as XCell, CellBuffer, Color as XColor};
use xre_render::{Braille, LuminanceRamp, SampleBuffer};
use xre_term::Presenter;

use super::frame::{Cell, Color, Frame, Mods};
use super::{Capabilities, CellShaderKind, RenderBackend, RenderError, ShadeSample};

/// The live xRenderEngine backend.
pub struct XreBackend {
    presenter: Presenter<Stdout>,
    scratch: CellBuffer,
    shade_buffer: SampleBuffer,
    shade_resolved: Vec<Option<XCell>>,
    braille: Braille,
    luminance_ramp: LuminanceRamp,
    caps: Capabilities,
}

impl XreBackend {
    /// Probe the terminal and build the presenter. Returns [`RenderError`] if the
    /// engine cannot initialize on this terminal (so [`super::select_backend`]
    /// degrades to ASCII instead of crashing).
    ///
    /// # Errors
    /// [`RenderError::Terminal`] on a presenter/terminal init failure.
    pub fn try_new(caps: Capabilities) -> Result<Self, RenderError> {
        let probe = super::caps::terminal_probe();
        let mut presenter = Presenter::stdout(&probe);
        // The presenter resolves every colour to a depth at present time, and
        // its default is the engine's environment probe — 16 colours wherever
        // `COLORTERM` is unset, which is every Windows host. Canopy's own
        // capability set is the authority once this backend is selected.
        presenter.set_color_depth(present_depth(caps, probe.color));
        let scratch = CellBuffer::new(probe.size);
        Ok(Self {
            presenter,
            scratch,
            shade_buffer: SampleBuffer::new(UVec2::new(0, 0), 2, 4),
            shade_resolved: Vec::new(),
            braille: Braille::default(),
            luminance_ramp: LuminanceRamp::default(),
            caps,
        })
    }
}

impl RenderBackend for XreBackend {
    fn name(&self) -> &'static str {
        "xre"
    }

    fn capabilities(&self) -> Capabilities {
        self.caps
    }

    fn present(&mut self, frame: &Frame) -> Result<(), RenderError> {
        blit_into(frame, &mut self.scratch);
        self.presenter
            .present(&self.scratch)
            .map_err(|e| RenderError::Terminal(e.to_string()))
    }

    fn plot(
        &mut self,
        points: &[crate::plot::PlotPoint],
        area: crate::layout::Rect,
        paint: &crate::plot::Paint,
        frame: &mut Frame,
    ) {
        // Hi-res truecolor tier on `xre-3d` builds whose caps advertise a raster
        // engine — a strict superset of the braille floor (which stays the
        // cross-terminal shape reference).
        #[cfg(feature = "xre-3d")]
        {
            if self.caps.contains(Capabilities::MESH_3D) {
                crate::render::xre_hires::draw(points, area, paint, frame);
                return;
            }
        }
        // Braille where the terminal has sub-cell fidelity; the same-cell block
        // floor otherwise (the one points→cells contract keeps them aligned).
        if self.caps.contains(Capabilities::SUBCELL) {
            crate::plot::draw_braille(frame, area, points, paint);
        } else {
            crate::plot::draw_blocks(frame, area, points, paint);
        }
    }

    fn polyline(
        &mut self,
        vertices: &[crate::plot::PlotPoint],
        area: crate::layout::Rect,
        style: crate::render::frame::Style,
        frame: &mut Frame,
    ) {
        #[cfg(feature = "xre-3d")]
        {
            if self.caps.contains(Capabilities::MESH_3D) {
                crate::render::xre_hires::draw_polyline(vertices, area, style, frame);
                return;
            }
        }
        let points = crate::plot::connected_line_points(
            vertices,
            u32::from(area.w).saturating_mul(2),
            u32::from(area.h).saturating_mul(4),
        );
        self.plot(&points, area, &crate::plot::Paint::Solid(style), frame);
    }

    fn shade_cells(
        &mut self,
        samples: &[ShadeSample],
        area: crate::layout::Rect,
        shader: CellShaderKind,
        frame: &mut Frame,
    ) {
        let shader: &dyn xre_render::CellShader = match shader {
            CellShaderKind::Braille if self.caps.contains(Capabilities::SUBCELL) => &self.braille,
            CellShaderKind::Braille | CellShaderKind::LuminanceRamp => &self.luminance_ramp,
        };
        crate::render::xre_shade::draw(
            samples,
            area,
            shader,
            &mut self.shade_buffer,
            &mut self.shade_resolved,
            frame,
        );
    }
}

/// Copy a Canopy [`Frame`] into a reused `xre_core::CellBuffer`, resizing it to
/// match (so the diffing presenter keeps its zero-per-frame-allocation hot path).
fn blit_into(frame: &Frame, scratch: &mut CellBuffer) {
    let want = UVec2::new(u32::from(frame.width()), u32::from(frame.height()));
    if scratch.size() != want {
        scratch.resize(want);
    }
    let w = u32::from(frame.width().max(1));
    for (i, cell) in frame.cells().iter().enumerate() {
        let i = u32::try_from(i).unwrap_or(0);
        scratch.set(i % w, i / w, to_xre_cell(*cell));
    }
}

/// Convert a Canopy [`Cell`] to an `xre_core::Cell` (the present boundary).
fn to_xre_cell(c: Cell) -> XCell {
    let (mut fg, mut bg) = (to_xre_color(c.style.fg), to_xre_color(c.style.bg));
    if c.style.mods.contains(Mods::REVERSE) {
        std::mem::swap(&mut fg, &mut bg);
    }
    XCell {
        glyph: c.ch,
        fg,
        bg,
        attrs: to_xre_attrs(c.style.mods),
    }
}

/// The depth the presenter resolves colours to: truecolor whenever Canopy's
/// capability set carries the `TRUECOLOR` bit (the Windows console probe, or
/// a POSIX environment that advertised it), else whatever the engine probed —
/// so a host the environment does describe keeps the engine's answer.
pub(crate) fn present_depth(
    caps: Capabilities,
    probed: xre_term::ColorDepth,
) -> xre_term::ColorDepth {
    if caps.contains(Capabilities::TRUECOLOR) {
        xre_term::ColorDepth::TrueColor
    } else {
        probed
    }
}

/// Map Canopy [`Color`] onto `xre_core::Color`.
fn to_xre_color(c: Color) -> XColor {
    match c {
        Color::Default => XColor::Default,
        Color::Rgb(r, g, b) => XColor::Rgb(r, g, b),
        Color::Indexed(i) => XColor::Ansi256(i),
    }
}

/// Map Canopy [`Mods`] onto `xre_core::Attrs` (REVERSE has no engine attr — it is
/// applied as an fg/bg swap in [`to_xre_cell`]).
fn to_xre_attrs(m: Mods) -> Attrs {
    let mut a = Attrs::NONE;
    if m.contains(Mods::BOLD) {
        a = a.with(Attrs::BOLD);
    }
    if m.contains(Mods::DIM) {
        a = a.with(Attrs::DIM);
    }
    if m.contains(Mods::UNDERLINE) {
        a = a.with(Attrs::UNDERLINE);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Style;
    use xre_term::Capabilities as XCaps;

    #[test]
    fn color_conversion_round_trips_through_xre() {
        assert_eq!(to_xre_color(Color::Default), XColor::Default);
        assert_eq!(to_xre_color(Color::Rgb(1, 2, 3)), XColor::Rgb(1, 2, 3));
        assert_eq!(to_xre_color(Color::Indexed(9)), XColor::Ansi256(9));
    }

    #[test]
    fn reverse_swaps_fg_bg() {
        let style = Style::fg(Color::Rgb(1, 1, 1))
            .with_bg(Color::Rgb(2, 2, 2))
            .with_mods(Mods::REVERSE);
        let xc = to_xre_cell(Cell { ch: 'x', style });
        assert_eq!(xc.fg, XColor::Rgb(2, 2, 2));
        assert_eq!(xc.bg, XColor::Rgb(1, 1, 1));
    }

    /// Canopy's `TRUECOLOR` bit, not the engine's environment probe, decides
    /// the presenter's depth — without this the Windows probe fix would select
    /// the engine backend and still flush 16-colour output.
    #[test]
    fn present_depth_follows_the_canopy_truecolor_bit() {
        use xre_term::ColorDepth;
        assert_eq!(
            present_depth(Capabilities::TRUECOLOR, ColorDepth::Ansi16),
            ColorDepth::TrueColor
        );
        assert_eq!(
            present_depth(Capabilities::empty(), ColorDepth::Ansi256),
            ColorDepth::Ansi256,
            "without the bit the engine's answer stands"
        );
        assert_eq!(
            present_depth(Capabilities::UNICODE, ColorDepth::Mono),
            ColorDepth::Mono
        );
    }

    #[test]
    fn palette_presenter_quantizes_rgb_resets_defaults_and_leaves_idle_silent() {
        let caps = XCaps {
            color: xre_term::ColorDepth::Ansi256,
            ..XCaps::FALLBACK
        };
        let mut output = Vec::new();
        let mut presenter = Presenter::new(&mut output, &caps);
        let mut frame = Frame::new(crate::render::Size::new(4, 1));
        frame.put_str(
            0,
            0,
            "R",
            Style::fg(Color::Rgb(160, 160, 160)).with_bg(Color::Rgb(240, 196, 92)),
        );
        let mut scratch = CellBuffer::new(UVec2::new(4, 1));
        blit_into(&frame, &mut scratch);
        presenter.present(&scratch).unwrap();
        frame = Frame::new(frame.size());
        blit_into(&frame, &mut scratch);
        presenter.present(&scratch).unwrap();
        drop(presenter);
        let text = String::from_utf8(output.clone()).unwrap();
        assert!(text.contains("38;5;"));
        assert!(text.contains("48;5;"));
        assert!(text.contains("39;49m"));
        assert!(!text.contains("38;2;") && !text.contains("48;2;"));
        // Repeated frames add no bytes: shimmer diffs stay confined to the mark.
        let mut presenter = Presenter::new(Vec::new(), &caps);
        presenter.present(&scratch).unwrap();
        let initial = presenter.into_inner();
        let mut presenter = Presenter::new(Vec::new(), &caps);
        presenter.present(&scratch).unwrap();
        presenter.present(&scratch).unwrap();
        assert_eq!(presenter.into_inner(), initial);
    }

    #[test]
    fn attrs_map_through() {
        let a = to_xre_attrs(Mods::BOLD.with(Mods::UNDERLINE));
        assert!(a.contains(Attrs::BOLD));
        assert!(a.contains(Attrs::UNDERLINE));
        assert!(!a.contains(Attrs::DIM));
    }

    #[test]
    fn blit_copies_glyphs_into_cellbuffer() {
        let mut f = Frame::new(crate::render::frame::Size::new(4, 2));
        f.put_str(0, 0, "ok", Style::DEFAULT);
        let mut scratch = CellBuffer::new(UVec2::new(4, 2));
        blit_into(&f, &mut scratch);
        assert_eq!(scratch.get(0, 0).unwrap().glyph, 'o');
        assert_eq!(scratch.get(1, 0).unwrap().glyph, 'k');
        assert_eq!(scratch.get(3, 1).unwrap().glyph, ' ');
    }

    #[test]
    fn blit_resizes_scratch_to_frame() {
        let f = Frame::new(crate::render::frame::Size::new(5, 3));
        let mut scratch = CellBuffer::new(UVec2::new(1, 1));
        blit_into(&f, &mut scratch);
        assert_eq!(scratch.size(), UVec2::new(5, 3));
    }

    #[test]
    fn present_into_byte_sink_emits_output() {
        // Drive a Vec<u8>-backed presenter directly (xre's headless pattern) to
        // prove the blit+present path produces bytes without a real terminal.
        let mut scratch = CellBuffer::new(UVec2::new(4, 2));
        let mut f = Frame::new(crate::render::frame::Size::new(4, 2));
        f.put_str(0, 0, "ok", Style::DEFAULT);
        blit_into(&f, &mut scratch);
        let mut presenter = Presenter::new(Vec::<u8>::new(), &XCaps::FALLBACK);
        assert!(presenter.present(&scratch).is_ok());
        assert!(
            !presenter.into_inner().is_empty(),
            "presenter emitted output"
        );
    }
}
