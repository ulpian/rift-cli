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
use crate::theme::{Icon, Section, Theme};

/// A clickable header action (the right-aligned key hints).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderAction {
    /// `?` — open the palette help overlay.
    Help,
    /// `/` — open the command palette.
    Palette,
    /// `e` — toggle the panel editor (ADR-0034).
    Editor,
    /// `q` — quit the dashboard.
    Quit,
}

/// The header's right-aligned actions: key glyph + label, in display order. The
/// panel-editor toggle sits between the palette and quit (ADR-0034).
const HEADER_ITEMS: [(&str, &str, HeaderAction); 4] = [
    ("?", "Help", HeaderAction::Help),
    ("/", "Palette", HeaderAction::Palette),
    ("e", "Panel Editor", HeaderAction::Editor),
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
    // The left cluster (mark · product · version · health dot ≈ half the action
    // cluster) needs breathing room; below ~1.5× the cluster width the hints clip
    // away whole rather than overlapping the mark.
    if w < total.saturating_add(total / 2) {
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

/// The identity button's name budget in cells (ADR-0037 §1): beyond it the
/// name truncates with an ellipsis.
const PROFILE_NAME_BUDGET: usize = 16;

/// The gap between the health dot and the identity button, in cells.
const PROFILE_GAP: u16 = 2;

/// The active profile's name, terminal-sanitised and truncated at the fixed
/// budget (ADR-0037 §§1/4) — the one text both the renderer and the span
/// math use, so the drawn cells always equal the hit target.
fn profile_label(name: &str) -> String {
    let clean = sanitize_for_terminal(name);
    if clean.chars().count() <= PROFILE_NAME_BUDGET {
        return clean;
    }
    let mut out: String = clean.chars().take(PROFILE_NAME_BUDGET - 1).collect();
    out.push('…');
    out
}

/// One past the left cluster's last cell (the health dot) — the identity
/// button's anchor, deterministic from the version string alone. Kept in
/// lockstep with the strings [`draw_header`] puts.
fn left_cluster_end(version: &str) -> u16 {
    // ` RIFT ` (6) + ` Canopy` (7)
    let mut x = 6u16.saturating_add(7);
    if !version.is_empty() {
        let v = sanitize_for_terminal(version);
        let count = u16::try_from(v.chars().count()).unwrap_or(u16::MAX);
        x = x.saturating_add(2).saturating_add(count); // ` v` + version
    }
    x.saturating_add(2).saturating_add(1) // `  ` + the health dot
}

/// The header identity button's cell span `[start, end)` on row 0 (ADR-0037):
/// `⦿ name` right after the version and the health dot. **One** function feeds
/// both the renderer and the row-0 hit-test, so a resize can never desync what
/// is drawn from what is clickable. `None` hides the button — no profile wired
/// (empty name), or a row too narrow to hold it clear of the right cluster: it
/// clips away whole rather than garbling (the right cluster's rule).
#[must_use]
pub fn profile_span(version: &str, profile_name: Option<&str>, w: u16) -> Option<(u16, u16)> {
    let name = profile_name?;
    if name.is_empty() {
        return None;
    }
    let label = profile_label(name);
    let start = left_cluster_end(version).saturating_add(PROFILE_GAP);
    let label_cells = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
    let end = start.saturating_add(2).saturating_add(label_cells); // `⦿ ` + name
                                                                   // The button must clear the action cluster (when present) by a gap, and
                                                                   // the row's right margin regardless — else it clips away whole.
    let limit = header_action_spans(w)
        .first()
        .map_or_else(|| w.saturating_sub(1), |s| s.start.saturating_sub(ITEM_GAP));
    if end > limit {
        return None;
    }
    Some((start, end))
}

/// Whether row-0 column `col` is on the identity button.
#[must_use]
pub fn profile_hit(version: &str, profile_name: Option<&str>, w: u16, col: u16) -> bool {
    profile_span(version, profile_name, w).is_some_and(|(start, end)| col >= start && col < end)
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
        x = frame.put_str(
            x,
            y,
            &format!(" v{v}"),
            theme.dim().with_bg(theme.surface_bg()),
        );
    }
    x = frame.put_str(x, y, "  ", bar);
    frame.set(
        x,
        y,
        Icon::HealthDot.glyph(unicode),
        theme.health(vm.health).with_bg(theme.surface_bg()),
    );

    // The identity button (ADR-0037): `⦿ name` after the health dot, drawn on
    // the SAME span the hit-test resolves. Press-flash on click; a steady
    // active wash while the profile menu is open (the panel-editor pattern).
    if let Some((start, end)) = profile_span(&vm.version, vm.profile_name.as_deref(), area.w) {
        let target = crate::hit::Target::HeaderProfile;
        let active = vm.mode == crate::input::Mode::ProfileMenu;
        let bg = crate::chrome::press_flash_bg(theme, vm.button_flash(&target))
            .or_else(|| active.then_some(crate::theme::SELECT_BG))
            .unwrap_or(theme.surface_bg());
        let label = vm
            .profile_name
            .as_deref()
            .map(profile_label)
            .unwrap_or_default();
        let gx = area.x + start;
        frame.set(
            gx,
            y,
            Icon::Profile.glyph(unicode),
            theme.key_hint().with_bg(bg),
        );
        frame.put_str(gx + 1, y, &format!(" {label}"), theme.dim().with_bg(bg));
        let _ = end;
    }

    // Right cluster: the header actions.
    let spans = header_action_spans(area.w);
    for span in &spans {
        let Some((key, label, _)) = HEADER_ITEMS.iter().find(|(_, _, a)| *a == span.action) else {
            continue;
        };
        // A press flashes the action's chip toward the accent (ADR-0028); at
        // rest it keeps the header's raised-surface fill.
        let target = match span.action {
            HeaderAction::Help => crate::hit::Target::HeaderHelp,
            HeaderAction::Palette => crate::hit::Target::HeaderPalette,
            HeaderAction::Editor => crate::hit::Target::HeaderEditor,
            HeaderAction::Quit => crate::hit::Target::HeaderQuit,
        };
        // The panel-editor toggle reads active while the mode is on — a steady
        // accent wash, distinct from the transient press flash.
        let active =
            span.action == HeaderAction::Editor && vm.mode == crate::input::Mode::PanelEditor;
        let bg = crate::chrome::press_flash_bg(theme, vm.button_flash(&target))
            .or_else(|| active.then_some(crate::theme::SELECT_BG))
            .unwrap_or(theme.surface_bg());
        let kx = frame.put_str(area.x + span.start, y, key, theme.key_hint().with_bg(bg));
        frame.put_str(kx, y, &format!(" {label}"), theme.dim().with_bg(bg));
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
    fn a_pressed_header_action_flashes_toward_the_accent() {
        let mut v = vm(80, 24);
        v.pressed = Some(crate::hit::Target::HeaderPalette);
        v.press_pulse = 1.0;
        let mut f = Frame::new(Size::new(80, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let spans = header_action_spans(80);
        let palette = spans
            .iter()
            .find(|s| s.action == HeaderAction::Palette)
            .unwrap();
        let help = spans
            .iter()
            .find(|s| s.action == HeaderAction::Help)
            .unwrap();
        let flash_bg = crate::chrome::press_flash_bg(Theme::rift_dark(), 1.0).unwrap();
        assert_eq!(
            f.get(palette.start, 0).unwrap().style.bg,
            flash_bg,
            "the pressed chip flashes toward the accent"
        );
        assert_eq!(
            f.get(help.start, 0).unwrap().style.bg,
            crate::theme::SURFACE_BG,
            "an unpressed chip keeps the header's surface fill"
        );
    }

    #[test]
    fn header_action_at_resolves_and_misses() {
        let spans = header_action_spans(80);
        assert_eq!(spans.len(), 4);
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

    /// A snapshot with the profile plane wired: one active profile named `name`.
    fn vm_with_profile(name: &str, w: u16, h: u16) -> ViewModel {
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
        s.app.set_profiles(
            vec![crate::profile::ProfileEntry {
                profile_id: "pf1".into(),
                name: name.into(),
                created_at: 1,
            }],
            "pf1".into(),
        );
        s.snapshot()
    }

    #[test]
    fn identity_button_draws_exactly_on_its_span() {
        // ADR-0037 §1: one span function feeds the renderer and the hit-test —
        // the glyph sits on the span's first cell, the name follows, and every
        // cell in [start, end) hits while start-1 / end miss.
        let v = vm_with_profile("work", 80, 24);
        let mut f = Frame::new(Size::new(80, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let (start, end) = profile_span(&v.version, v.profile_name.as_deref(), 80)
            .expect("the identity button shows on a wide header");
        assert_eq!(f.get(start, 0).unwrap().ch, Icon::Profile.unicode());
        let text: String = (start..end).map(|x| f.get(x, 0).unwrap().ch).collect();
        assert!(
            text.contains("work"),
            "the name renders on the span: {text}"
        );
        for col in start..end {
            assert!(profile_hit(&v.version, v.profile_name.as_deref(), 80, col));
        }
        assert!(!profile_hit(
            &v.version,
            v.profile_name.as_deref(),
            80,
            start - 1
        ));
        assert!(!profile_hit(&v.version, v.profile_name.as_deref(), 80, end));
    }

    #[test]
    fn identity_name_truncates_with_an_ellipsis_at_the_budget() {
        let long = "a-very-long-profile-name-indeed";
        let v = vm_with_profile(long, 120, 24);
        let (start, end) =
            profile_span(&v.version, v.profile_name.as_deref(), 120).expect("shows when wide");
        // `⦿ ` + 15 chars + `…` = 18 cells.
        assert_eq!(end - start, 18, "the span holds the budgeted label");
        let mut f = Frame::new(Size::new(120, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let text: String = (start..end).map(|x| f.get(x, 0).unwrap().ch).collect();
        assert!(text.ends_with('…'), "truncation is an ellipsis: {text}");
    }

    #[test]
    fn identity_clips_away_whole_on_a_narrow_header() {
        // Mirrors the right cluster's rule: below the threshold the whole span
        // vanishes rather than garbling — and nothing on the row hits.
        let v = vm_with_profile("work", 30, 24);
        assert_eq!(
            profile_span(&v.version, v.profile_name.as_deref(), 30),
            None
        );
        let mut f = Frame::new(Size::new(30, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let row: String = (0..30u16)
            .map(|x| f.get(x, 0).map_or(' ', |c| c.ch))
            .collect();
        assert!(
            !row.contains(Icon::Profile.unicode()),
            "no partial button: {row}"
        );
        for col in 0..30u16 {
            assert!(!profile_hit(&v.version, v.profile_name.as_deref(), 30, col));
        }
    }

    #[test]
    fn identity_hides_until_the_profile_plane_is_wired() {
        let v = vm(80, 24); // no profiles set
        assert_eq!(v.profile_name, None);
        assert_eq!(
            profile_span(&v.version, v.profile_name.as_deref(), 80),
            None
        );
    }

    #[test]
    fn identity_reads_active_while_the_menu_is_open_and_flashes_on_press() {
        use crate::input::Mode;
        let mut v = vm_with_profile("work", 80, 24);
        v.mode = Mode::ProfileMenu;
        let (start, _) = profile_span(&v.version, v.profile_name.as_deref(), 80).unwrap();
        let mut f = Frame::new(Size::new(80, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        assert_eq!(
            f.get(start, 0).unwrap().style.bg,
            crate::theme::SELECT_BG,
            "a steady active wash while the menu is open"
        );
        // A press flashes toward the accent (and wins over the wash).
        v.pressed = Some(crate::hit::Target::HeaderProfile);
        v.press_pulse = 1.0;
        let mut f = Frame::new(Size::new(80, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        assert_eq!(
            f.get(start, 0).unwrap().style.bg,
            crate::chrome::press_flash_bg(Theme::rift_dark(), 1.0).unwrap()
        );
    }

    #[test]
    fn hostile_profile_names_are_sanitised_before_render() {
        // A control-byte name cannot smuggle bytes into the header (story 22).
        let v = vm_with_profile("wo\x1b[31mrk\x07", 120, 24);
        let (start, end) = profile_span(&v.version, v.profile_name.as_deref(), 120).unwrap();
        let mut f = Frame::new(Size::new(120, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let text: String = (start..end).map(|x| f.get(x, 0).unwrap().ch).collect();
        assert!(!text.contains('\x1b') && !text.contains('\x07'), "{text:?}");
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
