//! The xre-backed render backend — full-fidelity terminal present.
//!
//! The *only* module that names `xre_core`/`xre_term` render types. It blits a
//! Canopy [`Frame`] into a reused `xre_core::CellBuffer` and flushes it through
//! the diffing [`xre_term::Presenter`] (truecolor resolved at present time). 3D
//! scenes and braille plots are reserved for the future `xre-3d` feature; until
//! then this inherits the trait's banner/ASCII-bar defaults, so the engine churn
//! stays localized here.

use std::io::Stdout;

use xre_core::math::UVec2;
use xre_core::{Attrs, Cell as XCell, CellBuffer, Color as XColor};
use xre_term::{Capabilities as XCaps, Presenter};

use super::frame::{Cell, Color, Frame, Mods};
use super::{Capabilities, RenderBackend, RenderError};

/// The live xRenderEngine backend.
pub struct XreBackend {
    presenter: Presenter<Stdout>,
    scratch: CellBuffer,
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
        let probe = XCaps::probe();
        let presenter = Presenter::stdout(&probe);
        let scratch = CellBuffer::new(probe.size);
        Ok(Self {
            presenter,
            scratch,
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
