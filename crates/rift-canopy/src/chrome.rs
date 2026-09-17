//! Chrome draw primitives — the themed frame vocabulary (the built-in surface theme).
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

/// The marker every list surface draws in the first cell of its cursor row.
///
/// A glyph rather than a colour, because the selection wash resolves away on
/// the mono and ASCII tiers and the ASCII floor flushes glyphs only — and a
/// row the operator cannot find is a list they cannot use. The projects rail
/// set the rule; [`cursor_row`] and [`row_prefix`] carry it to every modal
/// list (the destination explorer, the profile menu's stages, the colour
/// picker, Project Settings). The same glyph at every tier, so the four-tier
/// goldens differ only in colour and icons.
pub const CURSOR_MARKER: char = '>';

/// Draw a list surface's cursor row: `row` filled in `style` (the selection
/// pill) with [`CURSOR_MARKER`] in its first cell. Returns the x the row's
/// text starts at — two cells in, past the marker and its gap — which is what
/// [`row_prefix`] reserves on every row so the text column never moves.
pub fn cursor_row(frame: &mut Frame, row: Rect, style: Style) -> u16 {
    fill(frame, row, style);
    if row.w > 0 && row.h > 0 {
        frame.set(row.x, row.y, CURSOR_MARKER, style);
    }
    row.x.saturating_add(2)
}

/// The two-cell prefix a list row's text starts with: the marker and a gap on
/// the cursor row, two spaces elsewhere — so the text column is the same on
/// every row and the marker is the only glyph that moves with the cursor.
#[must_use]
pub const fn row_prefix(on_cursor: bool) -> &'static str {
    if on_cursor {
        "> "
    } else {
        "  "
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

/// The press-flash background for a button at `flash`: the button
/// accent at `flash == 1.0` lerping to the raised surface as it decays, or
/// `None` at rest so the button keeps its normal fill. Colour-only, so the
/// mono/ASCII tiers resolve it away — a keyboard/pointer press acknowledges
/// identically at every tier that can carry colour. Takes the runtime theme
/// (the runtime surface theme): both endpoints follow the profile's background/button slots.
#[must_use]
pub fn press_flash_bg(
    theme: crate::theme::Theme,
    flash: f32,
) -> Option<crate::render::frame::Color> {
    let f = flash.clamp(0.0, 1.0);
    (f > 0.0).then(|| crate::theme::lerp_color(theme.surface_bg(), theme.button_accent(), f))
}

/// A scrollbar's geometry: the **track** on a region's right edge and
/// the **thumb** within it, derived purely from `(offset, viewport, content_len)`
/// — shared between the renderer and the hit-test so the drawn cells and the
/// click region are identical, exactly like every other clickable geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Scrollbar {
    /// The full-height track on the region's right edge (1 cell wide).
    pub track: Rect,
    /// The thumb — its size and position derive from the offset/viewport/content.
    pub thumb: Rect,
}

/// The scrollbar for a scrollable `area` whose content is `content_len` rows, of
/// which `area.h` are visible starting at `offset` (the scrollbar rule). `None` when the
/// content fits the viewport — the bar's presence honestly means "there is more".
/// The thumb height is proportional (≥1 cell) and its position maps the offset
/// across the track's travel, so the drawn indicator reflects where the viewport
/// sits in the content.
#[must_use]
pub fn scrollbar(area: Rect, offset: usize, content_len: usize) -> Option<Scrollbar> {
    let viewport = usize::from(area.h);
    if area.w == 0 || viewport == 0 || content_len <= viewport {
        return None;
    }
    let track = Rect {
        x: area.x + area.w - 1,
        y: area.y,
        w: 1,
        h: area.h,
    };
    // Proportional thumb: (visible / total) of the track, at least one cell.
    let thumb_h = ((viewport * viewport) / content_len).clamp(1, viewport);
    // `content_len > viewport` here (we returned above otherwise), so the max
    // in-range offset is ≥ 1 — the division is always safe.
    let max_off = content_len - viewport;
    let travel = viewport - thumb_h; // rows the thumb can move
    let off = offset.min(max_off);
    let thumb_top = (off * travel) / max_off;
    let thumb = Rect {
        x: track.x,
        y: area.y + u16::try_from(thumb_top).unwrap_or(0),
        w: 1,
        h: u16::try_from(thumb_h).unwrap_or(1),
    };
    Some(Scrollbar { track, thumb })
}

/// Draw a [`Scrollbar`]: the track glyph down the right edge in the
/// (themable) border tone, the thumb cells over it in the accent. Both glyph and
/// colour degrade on ASCII (`│`/`█` → `|`/`#`) so a mono terminal still shows an
/// honest indicator. Chrome — the bar sits on the region's own last column, never
/// over a data cell (callers reserve that column).
pub fn draw_scrollbar(
    frame: &mut Frame,
    bar: &Scrollbar,
    theme: crate::theme::Theme,
    unicode: bool,
) {
    let (track_g, thumb_g) = if unicode { ('│', '█') } else { ('|', '#') };
    let track_style = Style::fg(theme.border_idle());
    let thumb_style = Style::fg(crate::theme::ACCENT);
    for dy in 0..bar.track.h {
        frame.set(bar.track.x, bar.track.y + dy, track_g, track_style);
    }
    for dy in 0..bar.thumb.h {
        frame.set(bar.thumb.x, bar.thumb.y + dy, thumb_g, thumb_style);
    }
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
            (
                "cursor_row",
                Box::new(|f: &mut Frame, s: Style| {
                    cursor_row(f, rect(0, 2, 8, 1), s);
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

    /// The cursor cue: the whole row wears the pill, exactly one cell lights
    /// (the marker), the text column sits past it, and the plain prefix keeps
    /// every other row's text in the same column.
    #[test]
    fn cursor_row_fills_the_row_and_lights_only_the_marker() {
        let mut f = Frame::new(Size::new(8, 2));
        let style = Theme::rift_dark().selection();
        let text_x = cursor_row(&mut f, rect(1, 1, 6, 1), style);
        assert_eq!(text_x, 3, "text starts past the marker and its gap");
        assert_eq!(
            lit(&f),
            vec![(1, 1, CURSOR_MARKER)],
            "the marker is the one lit cell"
        );
        for x in 1..7 {
            assert_eq!(f.get(x, 1).unwrap().style, style, "cell {x} wears the pill");
        }
        assert_eq!(
            f.get(0, 1).unwrap().style,
            Style::DEFAULT,
            "outside the row is untouched"
        );
        assert_eq!(row_prefix(true), "> ");
        assert_eq!(row_prefix(false), "  ");
        assert_eq!(
            row_prefix(true).chars().count(),
            row_prefix(false).chars().count(),
            "both prefixes reserve the same cells"
        );
        assert!(row_prefix(true).starts_with(CURSOR_MARKER));
        // An empty rect is a no-op that still answers with a text column.
        let mut g = Frame::new(Size::new(4, 1));
        assert_eq!(cursor_row(&mut g, rect(2, 0, 0, 0), style), 4);
        assert!(lit(&g).is_empty());
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

    // ---- the scrollbar primitive ------------------------

    #[test]
    fn no_scrollbar_when_content_fits() {
        // Content ≤ viewport ⇒ no bar (its presence honestly means "there's more").
        let area = rect(0, 0, 10, 8);
        assert!(scrollbar(area, 0, 8).is_none(), "exactly fits ⇒ no bar");
        assert!(scrollbar(area, 0, 3).is_none(), "under-fills ⇒ no bar");
        assert!(scrollbar(area, 0, 9).is_some(), "one row over ⇒ a bar");
    }

    #[test]
    fn thumb_tracks_top_middle_bottom() {
        // 20 rows of content in an 8-row viewport: the thumb sits at the top at
        // offset 0, and at the bottom of the track at the max offset (12).
        let area = rect(0, 0, 10, 8);
        let top = scrollbar(area, 0, 20).unwrap();
        assert_eq!(top.thumb.y, area.y, "offset 0 ⇒ thumb at the top");
        let bottom = scrollbar(area, 12, 20).unwrap();
        assert_eq!(
            bottom.thumb.y + bottom.thumb.h,
            area.y + area.h,
            "max offset ⇒ thumb flush to the track bottom"
        );
        // The thumb never leaves the track, at any offset.
        for off in 0..=12 {
            let bar = scrollbar(area, off, 20).unwrap();
            assert!(bar.thumb.y >= bar.track.y);
            assert!(bar.thumb.y + bar.thumb.h <= bar.track.y + bar.track.h);
            assert!(bar.thumb.h >= 1, "the thumb is always at least one cell");
        }
    }

    #[test]
    fn drawn_cells_equal_the_geometry() {
        // The rendered track/thumb cells are exactly the geometry's cells
        // (pixels == targets — the same discipline as every clickable geometry).
        let area = rect(0, 0, 12, 8);
        let bar = scrollbar(area, 4, 24).unwrap();
        let mut f = Frame::new(Size::new(12, 8));
        draw_scrollbar(&mut f, &bar, crate::theme::Theme::rift_dark(), true);
        let track_x = area.x + area.w - 1;
        for y in 0..8 {
            let lit = f.get(track_x, y).is_some_and(|c| c.ch != ' ');
            let in_track = y >= bar.track.y && y < bar.track.y + bar.track.h;
            assert_eq!(lit, in_track, "track cell mismatch at row {y}");
            let is_thumb = y >= bar.thumb.y && y < bar.thumb.y + bar.thumb.h;
            let glyph = f.get(track_x, y).map(|c| c.ch);
            if is_thumb {
                assert_eq!(glyph, Some('█'), "thumb glyph at row {y}");
            }
        }
        // No cell OUTSIDE the track column is touched.
        for y in 0..8 {
            for x in 0..track_x {
                assert!(
                    f.get(x, y).is_some_and(|c| c.ch == ' '),
                    "the bar wrote outside its column at ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn scrollbar_degrades_on_ascii() {
        let area = rect(0, 0, 6, 6);
        let bar = scrollbar(area, 0, 12).unwrap();
        let mut f = Frame::new(Size::new(6, 6));
        draw_scrollbar(&mut f, &bar, crate::theme::Theme::rift_dark(), false);
        let x = area.x + area.w - 1;
        // The ASCII floor keeps an honest indicator: `|` track, `#` thumb.
        let glyphs: Vec<char> = (0..6).filter_map(|y| f.get(x, y).map(|c| c.ch)).collect();
        assert!(glyphs.contains(&'#'), "ascii thumb: {glyphs:?}");
        assert!(glyphs.contains(&'|'), "ascii track: {glyphs:?}");
    }
}
