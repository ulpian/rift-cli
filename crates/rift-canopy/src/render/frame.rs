//! The backend-agnostic frame buffer the rest of Canopy draws into.
//!
//! Panels and the splash draw into a [`Frame`] of [`Cell`]s; a
//! [`super::RenderBackend`] flushes it (the ASCII backend prints lines, the xre
//! backend blits into an `xre_core::CellBuffer` and presents). Keeping Canopy's
//! own `Cell`/`Style`/`Color` independent of `xre-rs` is what lets every module
//! outside `render/` stay engine-free — the single seam rule. Coordinates are
//! cell `(x, y)` from the top-left; all writes are bounds-checked (a no-op on
//! out-of-range), never a panic.

use crate::layout::Rect;

/// A frame size in terminal cells.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Size {
    /// Width in columns.
    pub w: u16,
    /// Height in rows.
    pub h: u16,
}

impl Size {
    /// A new size.
    #[must_use]
    pub const fn new(w: u16, h: u16) -> Self {
        Self { w, h }
    }
}

/// A terminal color, authored at full fidelity and downgraded by the backend.
///
/// Mirrors the shape of `xre_core::Color` so the xre backend can convert without
/// loss, but is a Canopy-owned type so panels never name an engine type.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Color {
    /// The terminal's own default foreground/background.
    #[default]
    Default,
    /// A 24-bit RGB color.
    Rgb(u8, u8, u8),
    /// An index into the 256-color palette.
    Indexed(u8),
}

/// Text styling attributes, packed into a small bitset (no external dep).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods(u8);

impl Mods {
    /// No attributes.
    pub const NONE: Self = Self(0);
    /// Bold / increased intensity.
    pub const BOLD: Self = Self(1 << 0);
    /// Dim / decreased intensity.
    pub const DIM: Self = Self(1 << 1);
    /// Reverse video (swap fg/bg).
    pub const REVERSE: Self = Self(1 << 2);
    /// Underline.
    pub const UNDERLINE: Self = Self(1 << 3);

    /// `true` if every flag in `other` is set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Return `self` with the flags in `other` set.
    #[must_use]
    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// `true` if no attribute is set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The raw bit pattern.
    #[must_use]
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// A foreground/background color plus attributes applied to a glyph.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Style {
    /// Foreground color.
    pub fg: Color,
    /// Background color.
    pub bg: Color,
    /// Styling attributes.
    pub mods: Mods,
}

impl Style {
    /// The default style: default colors, no attributes.
    pub const DEFAULT: Self = Self {
        fg: Color::Default,
        bg: Color::Default,
        mods: Mods::NONE,
    };

    /// A style with the given foreground color.
    #[must_use]
    pub const fn fg(fg: Color) -> Self {
        Self {
            fg,
            bg: Color::Default,
            mods: Mods::NONE,
        }
    }

    /// Builder: set the background color.
    #[must_use]
    pub const fn with_bg(mut self, bg: Color) -> Self {
        self.bg = bg;
        self
    }

    /// Builder: set the attributes.
    #[must_use]
    pub const fn with_mods(mut self, mods: Mods) -> Self {
        self.mods = mods;
        self
    }
}

/// A single rendered cell: a glyph plus its style.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell {
    /// The character to print.
    pub ch: char,
    /// The style applied to the glyph.
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            style: Style::DEFAULT,
        }
    }
}

/// A row-major grid of [`Cell`]s — the buffer every panel draws into.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Frame {
    w: u16,
    h: u16,
    cells: Vec<Cell>,
}

impl Frame {
    /// A new frame of `size`, filled with spaces in the default style.
    #[must_use]
    pub fn new(size: Size) -> Self {
        let len = usize::from(size.w) * usize::from(size.h);
        Self {
            w: size.w,
            h: size.h,
            cells: vec![Cell::default(); len],
        }
    }

    /// The frame size.
    #[must_use]
    pub const fn size(&self) -> Size {
        Size {
            w: self.w,
            h: self.h,
        }
    }

    /// Width in columns.
    #[must_use]
    pub const fn width(&self) -> u16 {
        self.w
    }

    /// Height in rows.
    #[must_use]
    pub const fn height(&self) -> u16 {
        self.h
    }

    /// The whole frame as a [`Rect`] rooted at the origin.
    #[must_use]
    pub const fn rect(&self) -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: self.w,
            h: self.h,
        }
    }

    /// All cells in row-major order (for headless render / snapshot tests).
    #[must_use]
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Flat index of `(x, y)`, or `None` when out of bounds.
    fn index(&self, x: u16, y: u16) -> Option<usize> {
        if x < self.w && y < self.h {
            Some(usize::from(y) * usize::from(self.w) + usize::from(x))
        } else {
            None
        }
    }

    /// The cell at `(x, y)`, or `None` when out of bounds.
    #[must_use]
    #[allow(clippy::indexing_slicing)] // index(x, y) yields an in-bounds cell index
    pub fn get(&self, x: u16, y: u16) -> Option<Cell> {
        self.index(x, y).map(|i| self.cells[i])
    }

    /// Write a styled glyph at `(x, y)`. Out-of-bounds writes are ignored.
    ///
    /// A [`Color::Default`] **background** is transparent at compose time: the
    /// cell keeps the background already beneath it. The root draw fills the
    /// surface with the theme's background first (ADR-0038: the background
    /// slot applies to every element's backdrop), so foreground-only styles
    /// ride the themed surface instead of punching terminal-default holes in
    /// it. A surface that must be opaque (a modal blanking the dashboard
    /// behind it) sets its background explicitly.
    #[allow(clippy::indexing_slicing)] // index(x, y) yields an in-bounds cell index
    pub fn set(&mut self, x: u16, y: u16, ch: char, style: Style) {
        if let Some(i) = self.index(x, y) {
            let mut style = style;
            if matches!(style.bg, Color::Default) {
                style.bg = self.cells[i].style.bg;
            }
            self.cells[i] = Cell { ch, style };
        }
    }

    /// Fill the whole frame with one styled glyph.
    pub fn fill(&mut self, ch: char, style: Style) {
        self.cells.fill(Cell { ch, style });
    }

    /// Copy a horizontal band of `src` — rows `[src_row, src_row + area.h)` ×
    /// columns `[0, area.w)` — into `self` at `area` (ADR-0031's scroll window).
    /// The band is the visible slice of a taller off-screen render; source rows
    /// past the end leave the destination as the caller left it. Cells carry
    /// their style, so a scrolled panel keeps its colours.
    pub fn blit_window(&mut self, src: &Frame, area: Rect, src_row: usize) {
        for dy in 0..area.h {
            let Ok(sy) = u16::try_from(src_row.saturating_add(usize::from(dy))) else {
                break;
            };
            for dx in 0..area.w {
                if let Some(cell) = src.get(dx, sy) {
                    self.set(area.x + dx, area.y + dy, cell.ch, cell.style);
                }
            }
        }
    }

    /// Print `s` left-to-right starting at `(x, y)`, clipped at the right edge
    /// and dropped entirely when `y` is out of range. Returns the next free `x`.
    /// Counts in `char`s (one column each) — good enough for the ASCII/box-draw
    /// glyphs Canopy uses; wide-glyph handling is the backend's concern.
    pub fn put_str(&mut self, x: u16, y: u16, s: &str, style: Style) -> u16 {
        if y >= self.h {
            return x;
        }
        let mut cx = x;
        for ch in s.chars() {
            if cx >= self.w {
                break;
            }
            self.set(cx, y, ch, style);
            cx = cx.saturating_add(1);
        }
        cx
    }

    /// Draw a single-line rounded border around `area` (clipped to the frame),
    /// returning the inner content rect. A common panel primitive used by the
    /// dashboard view; pure `Frame` drawing so both backends render it.
    pub fn border(&mut self, area: Rect, title: &str, style: Style) -> Rect {
        if area.w < 2 || area.h < 2 {
            return area;
        }
        let (l, t) = (area.x, area.y);
        let r = area.x + area.w - 1;
        let b = area.y + area.h - 1;
        self.set(l, t, '╭', style);
        self.set(r, t, '╮', style);
        self.set(l, b, '╰', style);
        self.set(r, b, '╯', style);
        for x in (l + 1)..r {
            self.set(x, t, '─', style);
            self.set(x, b, '─', style);
        }
        for y in (t + 1)..b {
            self.set(l, y, '│', style);
            self.set(r, y, '│', style);
        }
        if !title.is_empty() && area.w > 4 {
            let max = usize::from(area.w - 4);
            let clipped: String = title.chars().take(max).collect();
            self.put_str(l + 2, t, &format!(" {clipped} "), style);
        }
        Rect {
            x: l + 1,
            y: t + 1,
            w: area.w - 2,
            h: area.h - 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_is_bounds_checked() {
        let mut f = Frame::new(Size::new(4, 3));
        f.set(10, 10, 'X', Style::DEFAULT); // out of range → no-op, no panic
        assert_eq!(f.get(10, 10), None);
        f.set(1, 1, 'A', Style::fg(Color::Rgb(1, 2, 3)));
        assert_eq!(f.get(1, 1).unwrap().ch, 'A');
    }

    #[test]
    fn put_str_clips_at_right_edge() {
        let mut f = Frame::new(Size::new(5, 1));
        let next = f.put_str(2, 0, "hello", Style::DEFAULT);
        assert_eq!(next, 5);
        assert_eq!(f.get(2, 0).unwrap().ch, 'h');
        assert_eq!(f.get(4, 0).unwrap().ch, 'l');
    }

    #[test]
    fn put_str_off_screen_row_is_noop() {
        let mut f = Frame::new(Size::new(5, 1));
        let next = f.put_str(0, 9, "hi", Style::DEFAULT);
        assert_eq!(next, 0);
    }

    #[test]
    fn border_returns_inner_rect_and_draws_corners() {
        let mut f = Frame::new(Size::new(10, 5));
        let inner = f.border(f.rect(), "T", Style::DEFAULT);
        assert_eq!(
            inner,
            Rect {
                x: 1,
                y: 1,
                w: 8,
                h: 3
            }
        );
        assert_eq!(f.get(0, 0).unwrap().ch, '╭');
        assert_eq!(f.get(9, 4).unwrap().ch, '╯');
    }

    #[test]
    fn mods_pack_and_query() {
        let m = Mods::BOLD.with(Mods::UNDERLINE);
        assert!(m.contains(Mods::BOLD));
        assert!(m.contains(Mods::UNDERLINE));
        assert!(!m.contains(Mods::DIM));
        assert!(!m.is_empty());
        assert!(Mods::NONE.is_empty());
    }
}
