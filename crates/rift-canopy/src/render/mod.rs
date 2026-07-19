//! The render seam: one `RenderBackend` trait, two implementations.
//!
//! Every pixel Canopy draws goes through [`RenderBackend`]. There are exactly
//! two backends — [`AsciiBackend`] (pure std, always available) and the xre
//! backend (`#[cfg(feature = "xre")]`, full-fidelity terminal present) — chosen
//! at startup by [`select_backend`] from detected [`Capabilities`]. No module
//! outside `render/` references an `xre_*` type: panels draw into a Canopy
//! [`Frame`], and the backend flushes it. Because `xre-rs` is first-party and
//! co-evolving, the engine-specific code is isolated here and the ASCII floor is
//! guaranteed.

pub mod ascii_backend;
pub mod caps;
pub mod frame;
#[cfg(feature = "xre")]
pub mod xre_backend;
#[cfg(feature = "xre-3d")]
pub mod xre_hires;

pub use ascii_backend::AsciiBackend;
pub use frame::{Cell, Color, Frame, Mods, Size, Style};

use crate::layout::Rect;

/// What a backend can actually do on this terminal + build.
///
/// A Canopy-owned bitset derived from the terminal probe, compiled-in features,
/// and env (see [`caps::detect`]). Distinct from `xre_term::Capabilities` (which
/// is a *terminal* snapshot); this is the capability view the UI degrades over.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Capabilities(u32);

impl Capabilities {
    /// 24-bit foreground/background color.
    pub const TRUECOLOR: Self = Self(1 << 0);
    /// Full Unicode (box-drawing, braille, half-blocks).
    pub const UNICODE: Self = Self(1 << 1);
    /// 3D mesh rasterization available (xre `render`).
    pub const MESH_3D: Self = Self(1 << 2);
    /// Ramp / luminance shader available (xre `LuminanceRamp`).
    pub const RAMP_SHADER: Self = Self(1 << 3);
    /// Sub-cell plotting (braille / half-block).
    pub const SUBCELL: Self = Self(1 << 4);
    /// Parallel rasterization (xre `parallel`).
    pub const PARALLEL: Self = Self(1 << 5);
    /// Running the legacy-Windows console path.
    pub const LEGACY_WIN: Self = Self(1 << 6);

    /// The empty capability set.
    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    /// `true` if every flag in `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// `self` with the flags in `other` set.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `true` if no flag is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The raw bit pattern.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl std::ops::BitOr for Capabilities {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.with(rhs)
    }
}

/// A render failure. Carries only a human summary — never key/transcript bytes.
#[derive(Clone, Debug)]
pub enum RenderError {
    /// A terminal I/O failure (enter raw mode, flush, …).
    Terminal(String),
    /// A backend-internal failure.
    Backend(String),
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Terminal(m) => write!(f, "terminal error: {m}"),
            Self::Backend(m) => write!(f, "backend error: {m}"),
        }
    }
}

impl std::error::Error for RenderError {}

/// A 3D scene handed to [`RenderBackend::render_scene`].
///
/// Engine-free by construction: the xre backend (when built with 3D) rasterizes
/// it, while every other backend draws `fallback` — the static banner — so the
/// 3D entrypoint always yields *something* and never an error.
pub struct SceneRef<'a> {
    /// Lines drawn centered in `area` by backends without 3D.
    pub fallback: &'a [&'a str],
}

pub use crate::plot::{Paint, PlotPoint};

/// The swappable render backend — the one seam the rest of Stage 4 builds on.
pub trait RenderBackend {
    /// A human-readable backend name.
    fn name(&self) -> &str;

    /// What this backend can do on this terminal + build.
    fn capabilities(&self) -> Capabilities;

    /// Begin a frame sized to the current terminal.
    fn begin_frame(&mut self, size: Size) -> Frame {
        Frame::new(size)
    }

    /// Flush a finished frame to the terminal (diffed against the prior frame
    /// where the backend supports it).
    ///
    /// # Errors
    /// [`RenderError`] on an underlying terminal I/O failure.
    fn present(&mut self, frame: &Frame) -> Result<(), RenderError>;

    /// Render a 3D scene into a frame region. Backends without 3D draw the
    /// scene's static-banner fallback — **never** an error.
    fn render_scene(&mut self, scene: &SceneRef<'_>, area: Rect, frame: &mut Frame) {
        draw_centered(frame, area, scene.fallback);
    }

    /// Sub-cell plot for graphs/gauges. The default draws the half-block floor
    /// tier; the xre backend overrides with braille when `SUBCELL` is present.
    /// Both tiers place a point in the same cell (the one points→cells
    /// contract in [`crate::plot`]), so terminals differ only in sub-cell
    /// resolution, never in shape. `paint` colours the lit cells (cosmetic only —
    /// it can never change which cells light).
    fn plot(&mut self, points: &[PlotPoint], area: Rect, paint: &Paint, frame: &mut Frame) {
        crate::plot::draw_blocks(frame, area, points, paint);
    }

    /// Connected polyline for time-series graphs. The default rasterizes every
    /// clipped segment through the portable points contract; xRE overrides the
    /// high-fidelity tier with `SampleBuffer::line` + the Braille CellShader.
    fn polyline(&mut self, vertices: &[PlotPoint], area: Rect, style: Style, frame: &mut Frame) {
        let points = crate::plot::connected_line_points(
            vertices,
            u32::from(area.w).saturating_mul(2),
            u32::from(area.h).saturating_mul(4),
        );
        self.plot(&points, area, &Paint::Solid(style), frame);
    }

    /// Dev-only hot-reload tick (no-op unless built with hot-reload).
    fn hot_reload_tick(&mut self) {}
}

/// Choose a backend from capabilities. The ASCII floor is returned when forced,
/// when caps lack truecolor, or when the xre backend fails to initialize — so
/// startup degrades, never crashes.
#[must_use]
pub fn select_backend(caps: Capabilities, force_ascii: bool) -> Box<dyn RenderBackend> {
    #[cfg(feature = "xre")]
    {
        if !force_ascii && caps.contains(Capabilities::TRUECOLOR) {
            match xre_backend::XreBackend::try_new(caps) {
                Ok(b) => return Box::new(b),
                Err(_) => return Box::new(AsciiBackend::new(caps)),
            }
        }
    }
    let _ = force_ascii;
    Box::new(AsciiBackend::new(caps))
}

/// Draw `lines` centered within `area` (used for the 3D-scene fallback banner).
pub(crate) fn draw_centered(frame: &mut Frame, area: Rect, lines: &[&str]) {
    if area.h == 0 || lines.is_empty() {
        return;
    }
    let n = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let top = area.y + area.h.saturating_sub(n) / 2;
    for (i, line) in lines.iter().enumerate() {
        let y = top + u16::try_from(i).unwrap_or(0);
        let lw = u16::try_from(line.chars().count()).unwrap_or(0);
        let x = area.x + area.w.saturating_sub(lw) / 2;
        frame.put_str(x, y, line, Style::DEFAULT);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_set_ops() {
        let c = Capabilities::TRUECOLOR | Capabilities::UNICODE;
        assert!(c.contains(Capabilities::TRUECOLOR));
        assert!(c.contains(Capabilities::UNICODE));
        assert!(!c.contains(Capabilities::MESH_3D));
        assert!(!c.is_empty());
        assert!(Capabilities::empty().is_empty());
    }

    #[test]
    fn select_backend_returns_ascii_when_forced() {
        let b = select_backend(Capabilities::TRUECOLOR, true);
        assert_eq!(b.name(), "ascii-fallback");
    }

    #[test]
    fn select_backend_returns_ascii_when_no_truecolor() {
        let b = select_backend(Capabilities::empty(), false);
        assert_eq!(b.name(), "ascii-fallback");
    }

    #[test]
    fn render_scene_default_is_banner_not_error() {
        let mut b = select_backend(Capabilities::empty(), true);
        let mut f = b.begin_frame(Size::new(20, 5));
        let lines = ["RIFT"];
        b.render_scene(&SceneRef { fallback: &lines }, f.rect(), &mut f);
        let joined: String = f.cells().iter().map(|c| c.ch).collect();
        assert!(joined.contains("RIFT"));
    }

    #[test]
    fn render_error_display_has_no_bytes() {
        let e = RenderError::Terminal("enter raw mode failed".into());
        assert!(e.to_string().contains("terminal error"));
    }
}
