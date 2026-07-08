//! Chrome draw primitives — the themed frame vocabulary (M55-02, ADR-0023).
//!
//! Pure functions over a [`Frame`] + a rect (and, for glyph selection, the
//! Unicode capability): styled panel borders, background fills, the pill badge,
//! the stat tile (and its collapsed headline form), the button, and the icon
//! cell. They sit beside the low-level [`Frame::border`] but carry the theme's
//! richer look — **all colour lives in the `Frame`**, so both backends draw the
//! identical geometry and differ only in what they flush (the one-renderer
//! rule).
//!
//! The invariant every helper preserves: **styling never moves a lit cell.**
//! Glyph placement is a pure function of the geometry, never of the styles
//! passed — so the set of non-space cells a badge/tile/button lights is the same
//! whether it is drawn in the theme's colours or in [`Style::DEFAULT`] (the
//! paint-never-moves-a-cell property, extended from the plot seam). Callers pass
//! concrete styles (produced by [`crate::theme::Theme`]); the helpers are
//! colour-agnostic mechanism.

use crate::layout::Rect;
use crate::render::frame::{Frame, Style};
use crate::theme::Icon;

/// Draw a rounded panel border whose **title carries its own style**, distinct
/// from the border (a themed section: dim border, accent title). Returns the
/// inner content rect, matching [`Frame::border`] exactly so hit-testing and the
/// renderer never disagree. A degenerate rect (either side `< 2`) draws nothing
/// and returns the rect unchanged.
pub fn panel(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    border_style: Style,
    title_style: Style,
) -> Rect {
    if area.w < 2 || area.h < 2 {
        return area;
    }
    let (l, t) = (area.x, area.y);
    let r = area.x + area.w - 1;
    let b = area.y + area.h - 1;
    frame.set(l, t, '╭', border_style);
    frame.set(r, t, '╮', border_style);
    frame.set(l, b, '╰', border_style);
    frame.set(r, b, '╯', border_style);
    for x in (l + 1)..r {
        frame.set(x, t, '─', border_style);
        frame.set(x, b, '─', border_style);
    }
    for y in (t + 1)..b {
        frame.set(l, y, '│', border_style);
        frame.set(r, y, '│', border_style);
    }
    // The title sits in ` title ` between the corners, at l+2 onward — the same
    // placement `Frame::border` uses (a drop-in twin) but with the title in its
    // own style and the pads in the border's. Clipped so the trailing pad never
    // clobbers the last glyph or the corner (the edge `Frame::border` mishandles
    // when a title fills the width).
    let span_start = l + 2;
    let span_end = r.saturating_sub(1);
    if !title.is_empty() && span_end > span_start {
        let inner_w = usize::from(span_end - span_start + 1); // cells for ` title `
        let clipped: String = title.chars().take(inner_w.saturating_sub(2)).collect();
        let tlen = u16::try_from(clipped.chars().count()).unwrap_or(0);
        frame.set(span_start, t, ' ', border_style);
        frame.put_str(span_start + 1, t, &clipped, title_style);
        frame.set(span_start + 1 + tlen, t, ' ', border_style);
    }
    Rect {
        x: l + 1,
        y: t + 1,
        w: area.w - 2,
        h: area.h - 2,
    }
}

/// Fill `rect` with spaces in `style` — a background wash (e.g. a header bar or
/// a hovered row). Bounds-checked per cell; a background space is **not** a lit
/// cell (its glyph is `' '`), so a fill never violates paint-never-moves.
pub fn fill(frame: &mut Frame, rect: Rect, style: Style) {
    for dy in 0..rect.h {
        for dx in 0..rect.w {
            frame.set(rect.x + dx, rect.y + dy, ' ', style);
        }
    }
}

/// Draw a pill badge — ` text ` on a filled surface — at `(x, y)` in `style`.
/// Returns the x just past the badge. The one-cell pads carry the badge's
/// background so it reads as a rounded chip; the text glyphs are what light.
pub fn badge(frame: &mut Frame, x: u16, y: u16, text: &str, style: Style) -> u16 {
    let padded = format!(" {text} ");
    frame.put_str(x, y, &padded, style)
}

/// Draw an icon glyph at `(x, y)` in `style`, resolving the Unicode/ASCII
/// substitution from the capability (the theme's single glyph resolver).
pub fn icon(frame: &mut Frame, x: u16, y: u16, icon: Icon, style: Style, unicode: bool) {
    frame.set(x, y, icon.glyph(unicode), style);
}

/// A stat tile: a small bordered box with a `label` title and a `value`
/// centered in the body, bordered in the class colour. Used for the capture
/// scoreboard and the token-usage tiles. Degrades to [`tile_headline`] when the
/// rect is too small for a box. `border`/`label_style`/`value_style` are passed
/// explicitly so the geometry stays colour-agnostic.
pub fn tile(
    frame: &mut Frame,
    rect: Rect,
    label: &str,
    value: &str,
    border: Style,
    label_style: Style,
    value_style: Style,
) {
    if rect.h < 3 || rect.w < 4 {
        tile_headline(
            frame,
            rect.x,
            rect.y,
            rect.w,
            label,
            value,
            label_style,
            value_style,
        );
        return;
    }
    let inner = panel(frame, rect, label, border, label_style);
    if inner.h == 0 || inner.w == 0 {
        return;
    }
    // Center the value on the tile's middle row.
    let vw = u16::try_from(value.chars().count()).unwrap_or(0);
    let vx = inner.x + inner.w.saturating_sub(vw) / 2;
    let vy = inner.y + inner.h / 2;
    frame.put_str(vx, vy, value, value_style);
}

/// The collapsed one-line tile form: `label value` at `(x, y)`, clipped to `w`.
/// The headline the tile degrades to under height pressure (never disappears —
/// the collapse-to-summary convention). Returns the x just past what was drawn.
#[allow(clippy::too_many_arguments)] // label + value each need their own style
pub fn tile_headline(
    frame: &mut Frame,
    x: u16,
    y: u16,
    w: u16,
    label: &str,
    value: &str,
    label_style: Style,
    value_style: Style,
) -> u16 {
    let lx = frame.put_str(x, y, &clip(label, w), label_style);
    if lx >= x.saturating_add(w) {
        return lx;
    }
    let space = frame.put_str(lx, y, " ", value_style);
    let remaining = x.saturating_add(w).saturating_sub(space);
    frame.put_str(space, y, &clip(value, remaining), value_style)
}

/// Draw a button — `[ label key ]` — at `(x, y)`. The whole chip takes `style`;
/// the trailing key hint takes `key_style` (accent) so the bound key stands out.
/// Returns the x just past the button. An empty `key` renders `[ label ]`.
pub fn button(
    frame: &mut Frame,
    x: u16,
    y: u16,
    label: &str,
    key: &str,
    style: Style,
    key_style: Style,
) -> u16 {
    let head = format!("[ {label} ");
    let mut cx = frame.put_str(x, y, &head, style);
    if !key.is_empty() {
        cx = frame.put_str(cx, y, key, key_style);
        cx = frame.put_str(cx, y, " ", style);
    }
    frame.put_str(cx, y, "]", style)
}

/// The press-flash background for a button at `flash` (ADR-0028): the accent at
/// `flash == 1.0` lerping to the raised surface as it decays, or `None` at rest
/// so the button keeps its normal fill. Colour-only, so the mono/ASCII tiers
/// resolve it away — a keyboard/pointer press acknowledges identically at every
/// tier that can carry colour.
#[must_use]
pub fn press_flash_bg(flash: f32) -> Option<crate::render::frame::Color> {
    let f = flash.clamp(0.0, 1.0);
    (f > 0.0).then(|| crate::theme::lerp_color(crate::theme::SURFACE_BG, crate::theme::ACCENT, f))
}

/// Truncate `s` to at most `w` display cells (char-counted).
fn clip(s: &str, w: u16) -> String {
    s.chars().take(usize::from(w)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::{Color, Size};
    use crate::theme::{Section, Theme};

    /// The lit cells of a frame — non-space glyphs, mirroring the plot property.
    fn lit(frame: &Frame) -> Vec<(u16, u16, char)> {
        let mut out = Vec::new();
        for y in 0..frame.height() {
            for x in 0..frame.width() {
                if let Some(c) = frame.get(x, y) {
                    if c.ch != ' ' {
                        out.push((x, y, c.ch));
                    }
                }
            }
        }
        out
    }

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn panel_matches_frame_border_geometry() {
        // The themed panel lights the same border cells as the plain primitive.
        let mut a = Frame::new(Size::new(12, 5));
        let ra = a.rect();
        let inner_a = panel(&mut a, ra, "T", Style::DEFAULT, Style::DEFAULT);
        let mut b = Frame::new(Size::new(12, 5));
        let inner_b = b.border(b.rect(), "T", Style::DEFAULT);
        assert_eq!(inner_a, inner_b, "inner rect matches Frame::border");
        assert_eq!(lit(&a), lit(&b), "same glyphs lit");
    }

    #[test]
    fn panel_title_carries_its_own_style() {
        let theme = Theme::rift_dark();
        let mut f = Frame::new(Size::new(20, 4));
        let border = theme.border(Section::Capture, false);
        let title = theme.title(Section::Capture);
        let rf = f.rect();
        panel(&mut f, rf, "capture", border, title);
        // The title glyph 'c' sits at l+3=(3,0) in the title style; the corner
        // takes the border style (mirroring `Frame::border`'s ` title ` layout).
        assert_eq!(f.get(3, 0).unwrap().ch, 'c');
        assert_eq!(f.get(3, 0).unwrap().style.fg, Section::Capture.accent());
        assert_eq!(f.get(0, 0).unwrap().ch, '╭');
        assert_eq!(f.get(0, 0).unwrap().style.fg, crate::theme::BORDER_IDLE);
    }

    #[test]
    fn styling_never_moves_a_lit_cell() {
        // A badge / tile / button lights the exact same cells in the theme's
        // colours as in the default style — geometry is colour-agnostic.
        type DrawFn = Box<dyn Fn(&mut Frame, Style)>;
        let theme = Theme::rift_dark();
        let cases: Vec<(&str, DrawFn)> = vec![
            (
                "badge",
                Box::new(|f: &mut Frame, s: Style| {
                    badge(f, 1, 1, "12", s);
                }),
            ),
            (
                "button",
                Box::new(|f: &mut Frame, s: Style| {
                    button(f, 1, 1, "Open", "e", s, s);
                }),
            ),
            (
                "tile",
                Box::new(|f: &mut Frame, s: Style| {
                    tile(f, rect(0, 0, 10, 3), "EVENTS", "42", s, s, s);
                }),
            ),
            (
                "tile_headline",
                Box::new(|f: &mut Frame, s: Style| {
                    tile_headline(f, 0, 0, 14, "READ", "48%", s, s);
                }),
            ),
        ];
        let themed = theme.selection();
        for (name, draw) in cases {
            let mut styled = Frame::new(Size::new(16, 4));
            draw(&mut styled, themed);
            let mut plain = Frame::new(Size::new(16, 4));
            draw(&mut plain, Style::DEFAULT);
            assert_eq!(
                lit(&styled),
                lit(&plain),
                "{name}: styling moved a lit cell"
            );
            assert!(!lit(&styled).is_empty(), "{name}: drew nothing");
        }
    }

    #[test]
    fn badge_wraps_text_in_pads() {
        let mut f = Frame::new(Size::new(10, 1));
        let end = badge(&mut f, 0, 0, "9", Style::fg(Color::Rgb(1, 2, 3)));
        assert_eq!(end, 3, "` 9 ` occupies three cells");
        assert_eq!(f.get(1, 0).unwrap().ch, '9');
        assert_eq!(f.get(1, 0).unwrap().style.fg, Color::Rgb(1, 2, 3));
    }

    #[test]
    fn tile_collapses_when_small() {
        // Too short for a box → the headline form, still lighting cells.
        let mut f = Frame::new(Size::new(14, 1));
        tile(
            &mut f,
            rect(0, 0, 14, 1),
            "READ",
            "48%",
            Style::DEFAULT,
            Style::DEFAULT,
            Style::DEFAULT,
        );
        let text: String = (0..f.width()).map(|x| f.get(x, 0).unwrap().ch).collect();
        assert!(text.contains("READ"));
        assert!(text.contains("48%"));
    }

    #[test]
    fn button_reads_as_a_chip_with_key_hint() {
        let mut f = Frame::new(Size::new(20, 1));
        let end = button(
            &mut f,
            0,
            0,
            "Open",
            "e",
            Style::DEFAULT,
            Style::fg(Color::Rgb(9, 9, 9)),
        );
        let text: String = (0..end).map(|x| f.get(x, 0).unwrap().ch).collect();
        assert_eq!(text, "[ Open e ]");
        // The key hint 'e' sits after `[ Open ` (col 7) and carries the key
        // style — distinct from the 'e' inside "Open", which is body-styled.
        assert_eq!(f.get(7, 0).unwrap().ch, 'e');
        assert_eq!(f.get(7, 0).unwrap().style.fg, Color::Rgb(9, 9, 9));
        assert_eq!(
            f.get(4, 0).unwrap().style.fg,
            Color::Default,
            "the 'e' in Open is not the key"
        );
    }

    #[test]
    fn icon_resolves_ascii_on_the_floor() {
        let mut f = Frame::new(Size::new(4, 1));
        icon(&mut f, 0, 0, Icon::Repo, Style::DEFAULT, true);
        icon(&mut f, 1, 0, Icon::Repo, Style::DEFAULT, false);
        assert_eq!(f.get(0, 0).unwrap().ch, Icon::Repo.unicode());
        assert_eq!(f.get(1, 0).unwrap().ch, Icon::Repo.ascii());
        assert!(f.get(1, 0).unwrap().ch.is_ascii());
    }

    #[test]
    fn helpers_are_bounds_safe_at_any_rect() {
        // Degenerate rects never panic and never write out of range.
        let mut f = Frame::new(Size::new(6, 3));
        for r in [rect(0, 0, 0, 0), rect(5, 2, 10, 10), rect(0, 0, 1, 1)] {
            panel(&mut f, r, "x", Style::DEFAULT, Style::DEFAULT);
            fill(&mut f, r, Style::DEFAULT);
            tile(
                &mut f,
                r,
                "L",
                "V",
                Style::DEFAULT,
                Style::DEFAULT,
                Style::DEFAULT,
            );
        }
        badge(&mut f, 20, 20, "off-screen", Style::DEFAULT);
        button(&mut f, 20, 0, "x", "y", Style::DEFAULT, Style::DEFAULT);
    }
}
