//! The chrome bars (M55-04, ADR-0024 §1): the header and the hint bar.
//!
//! The **header** owns row 0 — the product mark, the binary version, the
//! data-plane health dot, and the right-aligned `? Help  / Palette  q Quit`
//! actions (the transient status toast rides the middle). The **hint bar**
//! sits above the command bar and renders the focused context's real keymap
//! ([`crate::keymap`]) — derived, never hand-written, so it can't advertise a
//! dead key. Both are pure `Frame` drawing over the [`ViewModel`]; the header
//! action geometry is shared with [`crate::hit`] so a click resolves exactly
//! where the glyphs render.

use crate::col2::sanitize_for_terminal;
use crate::keymap;
use crate::layout::Rect;
use crate::render::frame::Frame;
use crate::state::ViewModel;
use crate::theme::{Icon, Section, Theme, SURFACE_BG};

/// A clickable header action (the right-aligned key hints).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderAction {
    /// `?` — open the palette help overlay.
    Help,
    /// `/` — open the command palette.
    Palette,
    /// `q` — quit the dashboard.
    Quit,
}

/// The header's right-aligned actions: key glyph + label, in display order.
const HEADER_ITEMS: [(&str, &str, HeaderAction); 3] = [
    ("?", "Help", HeaderAction::Help),
    ("/", "Palette", HeaderAction::Palette),
    ("q", "Quit", HeaderAction::Quit),
];

/// The gap between header action items, in cells.
const ITEM_GAP: u16 = 2;

/// One positioned header action: its cell span `[start, end)` on row 0 and
/// the action a click there performs. Shared by the renderer and the
/// hit-test, so pixels and targets can never disagree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeaderSpan {
    /// First cell of the `key label` group.
    pub start: u16,
    /// One past the last cell.
    pub end: u16,
    /// What a click in the span does.
    pub action: HeaderAction,
}

/// Width of one `key label` group in cells.
fn item_width(key: &str, label: &str) -> u16 {
    let k = u16::try_from(key.chars().count()).unwrap_or(0);
    let l = u16::try_from(label.chars().count()).unwrap_or(0);
    k + 1 + l
}

/// The header actions' spans for a row of width `w`, right-aligned with a
/// one-cell margin. Empty when the row is too narrow to carry them all —
/// the cluster clips away whole rather than overlapping the left side.
#[must_use]
pub fn header_action_spans(w: u16) -> Vec<HeaderSpan> {
    let total: u16 = HEADER_ITEMS
        .iter()
        .map(|(k, l, _)| item_width(k, l))
        .sum::<u16>()
        + ITEM_GAP * (u16::try_from(HEADER_ITEMS.len()).unwrap_or(1) - 1);
    // The left cluster needs breathing room; below ~2× the cluster width the
    // hints clip away whole.
    if w < total.saturating_mul(2) {
        return Vec::new();
    }
    let mut x = w.saturating_sub(1).saturating_sub(total);
    let mut out = Vec::with_capacity(HEADER_ITEMS.len());
    for (key, label, action) in HEADER_ITEMS {
        let width = item_width(key, label);
        out.push(HeaderSpan {
            start: x,
            end: x + width,
            action,
        });
        x += width + ITEM_GAP;
    }
    out
}

/// The header action under column `col`, if any.
#[must_use]
pub fn header_action_at(w: u16, col: u16) -> Option<HeaderAction> {
    header_action_spans(w)
        .into_iter()
        .find(|s| col >= s.start && col < s.end)
        .map(|s| s.action)
}

/// Draw the header bar into `area`'s first row: mark, version, health dot,
/// the status toast, and the right-aligned actions. Colour rides the theme's
/// surface fill (the mono/ASCII tiers resolve it away; the glyphs stand).
pub fn draw_header(vm: &ViewModel, theme: Theme, unicode: bool, frame: &mut Frame, area: Rect) {
    if area.w == 0 || area.h == 0 {
        return;
    }
    let y = area.y;
    let bar = theme.header_bar();
    for x in area.x..area.x + area.w {
        frame.set(x, y, ' ', bar);
    }

    // Left cluster: ` RIFT ` mark · product · version · health dot.
    let mut x = frame.put_str(area.x, y, " RIFT ", theme.badge(Section::Header.accent()));
    x = frame.put_str(x, y, " Canopy", bar);
    if !vm.version.is_empty() {
        let v = sanitize_for_terminal(&vm.version);
        x = frame.put_str(x, y, &format!(" v{v}"), theme.dim().with_bg(SURFACE_BG));
    }
    x = frame.put_str(x, y, "  ", bar);
    frame.set(
        x,
        y,
        Icon::HealthDot.glyph(unicode),
        theme.health(vm.health).with_bg(SURFACE_BG),
    );
    x = x.saturating_add(1);

    // Right cluster: the actions (drawn first so the toast clips against it).
    let spans = header_action_spans(area.w);
    let toast_end = spans.first().map_or(area.x + area.w, |s| {
        area.x + s.start.saturating_sub(ITEM_GAP)
    });
    for span in &spans {
        let Some((key, label, _)) = HEADER_ITEMS.iter().find(|(_, _, a)| *a == span.action) else {
            continue;
        };
        let kx = frame.put_str(
            area.x + span.start,
            y,
            key,
            theme.key_hint().with_bg(SURFACE_BG),
        );
        frame.put_str(kx, y, &format!(" {label}"), theme.dim().with_bg(SURFACE_BG));
    }

    // The transient status toast rides the middle, clipped to the free span.
    if let Some(status) = &vm.status {
        let sx = x.saturating_add(2);
        if toast_end > sx {
            let room = usize::from(toast_end - sx);
            let text: String = sanitize_for_terminal(status).chars().take(room).collect();
            frame.put_str(sx, y, &text, theme.dim().with_bg(SURFACE_BG));
        }
    }
}

/// Draw the hint bar: the focused context's keymap as `key verb` pairs. A
/// zero-height rect (collapsed chrome) or an empty table (a running dispatch)
/// draws nothing.
pub fn draw_hint_bar(vm: &ViewModel, theme: Theme, frame: &mut Frame, rect: Rect) {
    if rect.h == 0 || rect.w == 0 {
        return;
    }
    let ctx = keymap::context(vm.mode, vm.focus, vm.overlay.as_ref());
    let table = keymap::hints(ctx);
    let mut x = rect.x.saturating_add(1);
    let end = rect.x + rect.w;
    for (i, h) in table.iter().enumerate() {
        if i > 0 {
            x = frame.put_str(x, rect.y, "  ·  ", theme.dim());
        }
        if x >= end {
            break;
        }
        x = frame.put_str(x, rect.y, h.keys, theme.key_hint());
        x = frame.put_str(x, rect.y, &format!(" {}", h.verb), theme.dim());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::{Color, Size};
    use crate::state::AppState;
    use crate::theme::Health;
    use crate::{EntryKind, Project, ProjectStatus};

    fn vm(w: u16, h: u16) -> ViewModel {
        let mut s = AppState::new(
            vec![Project {
                name: "p".into(),
                path: "/r/p".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(w, h),
        );
        s.app.splash.skip();
        s.version = "0.1.0".into();
        s.snapshot()
    }

    fn row_text(f: &Frame, y: u16) -> String {
        (0..f.width())
            .map(|x| f.get(x, y).map_or(' ', |c| c.ch))
            .collect()
    }

    #[test]
    fn header_draws_mark_version_health_and_actions() {
        let v = vm(80, 24);
        let mut f = Frame::new(Size::new(80, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let row = row_text(&f, 0);
        assert!(row.contains("RIFT"), "product mark: {row}");
        assert!(row.contains("Canopy"));
        assert!(row.contains("v0.1.0"), "version renders: {row}");
        assert!(row.contains("? Help"), "actions render: {row}");
        assert!(row.contains("/ Palette"));
        assert!(row.contains("q Quit"));
        assert!(row.contains(Icon::HealthDot.unicode()));
    }

    #[test]
    fn header_actions_render_exactly_on_their_spans() {
        let v = vm(80, 24);
        let mut f = Frame::new(Size::new(80, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        for span in header_action_spans(80) {
            let (key, _, _) = HEADER_ITEMS
                .iter()
                .find(|(_, _, a)| *a == span.action)
                .unwrap();
            let cell = f.get(span.start, 0).unwrap();
            assert_eq!(
                cell.ch,
                key.chars().next().unwrap(),
                "the span start is the key glyph for {:?}",
                span.action
            );
        }
    }

    #[test]
    fn header_action_at_resolves_and_misses() {
        let spans = header_action_spans(80);
        assert_eq!(spans.len(), 3);
        for span in &spans {
            assert_eq!(header_action_at(80, span.start), Some(span.action));
            assert_eq!(header_action_at(80, span.end - 1), Some(span.action));
            // One past a span is a gap (or the margin) — never a target.
            assert_eq!(header_action_at(80, span.end), None);
        }
        assert_eq!(header_action_at(80, 0), None, "the mark is not a target");
        // Too narrow: the cluster clips away whole; nothing is clickable.
        assert!(header_action_spans(30).is_empty());
        assert_eq!(header_action_at(30, 15), None);
    }

    #[test]
    fn health_dot_carries_the_health_colour() {
        let mut v = vm(80, 24);
        let mut f = Frame::new(Size::new(80, 24));
        v.health = Health::Down;
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let dot_x = (0..80u16)
            .find(|&x| {
                f.get(x, 0)
                    .is_some_and(|c| c.ch == Icon::HealthDot.unicode())
            })
            .expect("the dot renders");
        assert_eq!(
            f.get(dot_x, 0).unwrap().style.fg,
            Health::Down.color(),
            "a dead data plane looks dead"
        );
    }

    #[test]
    fn header_status_toast_is_sanitized_and_clipped() {
        let mut v = vm(80, 24);
        v.status = Some("evil\x1b[31mtoast".into());
        let mut f = Frame::new(Size::new(80, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let row = row_text(&f, 0);
        assert!(row.contains("evil[31mtoast"), "escape stripped: {row}");
        for x in 0..80u16 {
            assert!(!f.get(x, 0).unwrap().ch.is_control());
        }
    }

    #[test]
    fn hint_bar_shows_the_rail_keymap() {
        let v = vm(100, 30); // focus starts on Projects
        let mut f = Frame::new(Size::new(100, 30));
        let rect = Rect {
            x: 0,
            y: 26,
            w: 100,
            h: 1,
        };
        draw_hint_bar(&v, Theme::rift_dark(), &mut f, rect);
        let row = row_text(&f, 26);
        assert!(row.contains("j/k navigate"), "rail hints render: {row}");
        assert!(row.contains("a add"));
        assert!(row.contains("q quit"));
    }

    #[test]
    fn hint_bar_key_glyphs_carry_the_accent() {
        let v = vm(100, 30);
        let mut f = Frame::new(Size::new(100, 30));
        let rect = Rect {
            x: 0,
            y: 26,
            w: 100,
            h: 1,
        };
        draw_hint_bar(&v, Theme::rift_dark(), &mut f, rect);
        // Cell (1, 26) is the first hint's key glyph `j`.
        let cell = f.get(1, 26).unwrap();
        assert_eq!(cell.ch, 'j');
        assert_eq!(cell.style.fg, crate::theme::ACCENT);
        assert!(cell.style.mods.contains(crate::render::Mods::BOLD));
    }

    #[test]
    fn zero_height_hint_rect_draws_nothing() {
        let v = vm(100, 30);
        let mut f = Frame::new(Size::new(100, 30));
        draw_hint_bar(
            &v,
            Theme::rift_dark(),
            &mut f,
            Rect {
                x: 0,
                y: 26,
                w: 100,
                h: 0,
            },
        );
        assert_eq!(row_text(&f, 26).trim(), "");
    }

    #[test]
    fn tiny_header_never_panics() {
        let v = vm(3, 2);
        for w in 0..12u16 {
            let mut f = Frame::new(Size::new(w, 2));
            let area = f.rect();
            draw_header(&v, Theme::rift_dark(), false, &mut f, area);
        }
        // ASCII floor: the dot substitutes, no tofu.
        let mut f = Frame::new(Size::new(80, 2));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), false, &mut f, area);
        let row = row_text(&f, 0);
        assert!(row.contains(Icon::HealthDot.ascii()));
        assert!(!row.contains(Icon::HealthDot.unicode()));
    }

    #[test]
    fn header_bar_carries_no_colour_on_the_ascii_tier() {
        use crate::theme::{resolve_style, Tier};
        let v = vm(80, 24);
        let mut f = Frame::new(Size::new(80, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), false, &mut f, area);
        for x in 0..80u16 {
            let s = resolve_style(f.get(x, 0).unwrap().style, Tier::Ascii);
            assert_eq!(s.fg, Color::Default);
            assert_eq!(s.bg, Color::Default);
        }
    }
}
