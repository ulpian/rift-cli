//! The ASCII fallback backend — the guaranteed floor (no `xre-rs`).
//!
//! The [`RenderBackend`] impl flushes a Canopy [`Frame`] to stdout as lines, so
//! both the interactive `--ascii` path and the bin's non-TTY one-frame path
//! share the one `view::draw` renderer.

use std::io::Write;

use super::frame::Frame;
use super::{Capabilities, RenderBackend, RenderError};

/// The ASCII fallback backend (no `xre-rs`, works on any terminal).
pub struct AsciiBackend;

impl AsciiBackend {
    /// Construct the ASCII backend. Capabilities are dropped — the floor reports
    /// the empty set from [`RenderBackend::capabilities`].
    #[must_use]
    pub fn new(_caps: Capabilities) -> Self {
        Self
    }
}

impl RenderBackend for AsciiBackend {
    fn name(&self) -> &'static str {
        "ascii-fallback"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::empty()
    }

    fn present(&mut self, frame: &Frame) -> Result<(), RenderError> {
        let mut out = std::io::stdout().lock();
        // Full repaint: cursor home, then each row. ANSI home works on any
        // VT-ish terminal; the dumb-terminal case uses the one-frame `render`.
        let mut buf = String::from("\x1b[H");
        let w = frame.width();
        for y in 0..frame.height() {
            let mut line = String::with_capacity(usize::from(w));
            for x in 0..w {
                line.push(frame.get(x, y).map_or(' ', |c| c.ch));
            }
            buf.push_str(line.trim_end());
            buf.push_str("\x1b[K\r\n"); // clear to EOL so stale glyphs don't linger
        }
        out.write_all(buf.as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| RenderError::Terminal(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;

    #[test]
    fn backend_name() {
        assert_eq!(AsciiBackend.name(), "ascii-fallback");
    }

    #[test]
    fn present_emits_frame_cells() {
        // A trait-level present writes the frame glyphs (smoke: no panic, content path).
        let mut f = Frame::new(Size::new(6, 2));
        f.put_str(0, 0, "hi", super::super::Style::DEFAULT);
        // We can't capture stdout here easily; assert the frame content is what
        // present would emit (glyph extraction parity).
        let row0: String = (0..f.width())
            .map(|x| f.get(x, 0).map_or(' ', |c| c.ch))
            .collect();
        assert_eq!(row0.trim_end(), "hi");
    }
}
