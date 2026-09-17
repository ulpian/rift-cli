//! The chrome bars (the dashboard information architecture): the header and the hint bar.
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
    /// `p` — the identity button (the profile hub): the active profile's name,
    /// and the door to the profile menu.
    Profile,
    /// `/` — open the command palette.
    Palette,
    /// `e` — toggle the panel editor.
    Editor,
    /// `q` — quit the dashboard.
    Quit,
}

/// The header's **static** right-aligned actions: key glyph + label, in
/// display order. The identity item is dynamic (it carries the active
/// profile's name) and is inserted immediately before Palette by
/// [`header_cluster`] (the profile hub).
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

/// The identity item's name budget in cells (the profile menu and header identity, carried into the
/// cluster by the profile hub): beyond it the name truncates with an ellipsis.
const PROFILE_NAME_BUDGET: usize = 16;

/// The narrowest name the identity item renders (one cell + an ellipsis)
/// before the ladder drops the item whole (the profile hub).
const PROFILE_MIN_NAME_CELLS: usize = 2;

/// A header item's label: static chrome text, or the identity item's budgeted
/// profile name (drawn behind the profile glyph, which substitutes on the
/// ASCII tier — both glyph forms are one cell, so geometry is tier-free).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HeaderLabel {
    /// A fixed action label (`Help`, `Palette`, …).
    Static(&'static str),
    /// The identity item: the budgeted profile name behind `⦿ `.
    Profile(String),
}

impl HeaderLabel {
    /// The label's width in cells (the glyph + its gap cost 2 for the identity).
    fn cells(&self) -> u16 {
        match self {
            Self::Static(s) => u16::try_from(s.chars().count()).unwrap_or(u16::MAX),
            Self::Profile(name) => {
                2u16.saturating_add(u16::try_from(name.chars().count()).unwrap_or(u16::MAX))
            }
        }
    }
}

/// One resolved header-cluster item: its span plus exactly what the renderer
/// puts there — **one** solver feeds the draw and the row-0 hit-test, so a
/// resize can never desync pixels from targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderItem {
    /// Where the item sits on row 0.
    pub span: HeaderSpan,
    /// The key glyph (key-hint styled) the chrome teaches.
    pub key: &'static str,
    /// The label beside it.
    pub label: HeaderLabel,
}

/// The active profile's name, terminal-sanitised and truncated with an
/// ellipsis at `budget` cells — the one truncation rule every ladder step uses.
fn profile_label(name: &str, budget: usize) -> String {
    let clean = sanitize_for_terminal(name);
    if clean.chars().count() <= budget {
        return clean;
    }
    let mut out: String = clean.chars().take(budget.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The cluster's candidate items: the four static actions plus, when a
/// profile is wired, the identity item immediately before Palette with its
/// name at `budget` cells (the profile hub).
fn cluster_candidates(
    profile: Option<(&str, usize)>,
) -> Vec<(&'static str, HeaderLabel, HeaderAction)> {
    let mut items: Vec<(&'static str, HeaderLabel, HeaderAction)> = HEADER_ITEMS
        .iter()
        .map(|(k, l, a)| (*k, HeaderLabel::Static(l), *a))
        .collect();
    if let Some((name, budget)) = profile {
        items.insert(
            1,
            (
                "p",
                HeaderLabel::Profile(profile_label(name, budget)),
                HeaderAction::Profile,
            ),
        );
    }
    items
}

/// Right-align `items` on a row of width `w`, or `None` when they miss the
/// cluster's fit threshold (the left cluster — mark · product · version ·
/// health dot ≈ half the action cluster — needs breathing room; below ~1.5×
/// the cluster width the row would garble).
fn lay_out(
    items: Vec<(&'static str, HeaderLabel, HeaderAction)>,
    w: u16,
) -> Option<Vec<HeaderItem>> {
    let gaps = ITEM_GAP * (u16::try_from(items.len()).unwrap_or(1).saturating_sub(1));
    let total: u16 = items
        .iter()
        .map(|(k, l, _)| u16::try_from(k.chars().count()).unwrap_or(0) + 1 + l.cells())
        .sum::<u16>()
        + gaps;
    if w < total.saturating_add(total / 2) {
        return None;
    }
    let mut x = w.saturating_sub(1).saturating_sub(total);
    let mut out = Vec::with_capacity(items.len());
    for (key, label, action) in items {
        let width = u16::try_from(key.chars().count()).unwrap_or(0) + 1 + label.cells();
        out.push(HeaderItem {
            span: HeaderSpan {
                start: x,
                end: x + width,
                action,
            },
            key,
            label,
        });
        x += width + ITEM_GAP;
    }
    Some(out)
}

/// The header cluster at width `w` (the profile hub) — the degradation ladder:
/// the identity item's name renders at the full budget, shrinks stepwise with
/// an ellipsis to its two-cell floor, then the item drops whole (the four
/// actions stay exactly as today), and finally the whole cluster clips. Once
/// the identity is visible at a width it is visible at every greater width —
/// the monotonicity the earlier hidden band lacked.
#[must_use]
pub fn header_cluster(w: u16, profile_name: Option<&str>) -> Vec<HeaderItem> {
    if let Some(name) = profile_name.filter(|n| !n.is_empty()) {
        for budget in (PROFILE_MIN_NAME_CELLS..=PROFILE_NAME_BUDGET).rev() {
            if let Some(items) = lay_out(cluster_candidates(Some((name, budget))), w) {
                return items;
            }
        }
    }
    lay_out(cluster_candidates(None), w).unwrap_or_default()
}

/// The header actions' spans for a row of width `w` — the cluster solver's
/// spans view (the hit-test and the span tests consume this).
#[must_use]
pub fn header_action_spans(w: u16, profile_name: Option<&str>) -> Vec<HeaderSpan> {
    header_cluster(w, profile_name)
        .into_iter()
        .map(|i| i.span)
        .collect()
}

/// The header action under column `col`, if any.
#[must_use]
pub fn header_action_at(w: u16, profile_name: Option<&str>, col: u16) -> Option<HeaderAction> {
    header_cluster(w, profile_name)
        .into_iter()
        .find(|i| col >= i.span.start && col < i.span.end)
        .map(|i| i.span.action)
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

    // Right cluster (the profile hub): the actions plus the identity item, drawn on
    // the exact spans the row-0 hit-test resolves — one solver for pixels and
    // targets, including the identity name's width-dependent shrink.
    for item in header_cluster(area.w, vm.profile_name.as_deref()) {
        // A press flashes the item's chip toward the accent; at
        // rest it keeps the header's raised-surface fill.
        let target = match item.span.action {
            HeaderAction::Help => crate::hit::Target::HeaderHelp,
            HeaderAction::Profile => crate::hit::Target::HeaderProfile,
            HeaderAction::Palette => crate::hit::Target::HeaderPalette,
            HeaderAction::Editor => crate::hit::Target::HeaderEditor,
            HeaderAction::Quit => crate::hit::Target::HeaderQuit,
        };
        // The mode-owning toggles read active while their mode is on — a
        // steady accent wash, distinct from the transient press flash.
        let active = (item.span.action == HeaderAction::Editor
            && vm.mode == crate::input::Mode::PanelEditor)
            || (item.span.action == HeaderAction::Profile
                && vm.mode == crate::input::Mode::ProfileMenu);
        let bg = crate::chrome::press_flash_bg(theme, vm.button_flash(&target))
            .or_else(|| active.then_some(crate::theme::SELECT_BG))
            .unwrap_or(theme.surface_bg());
        let kx = frame.put_str(
            area.x + item.span.start,
            y,
            item.key,
            theme.key_hint().with_bg(bg),
        );
        match &item.label {
            HeaderLabel::Static(label) => {
                frame.put_str(kx, y, &format!(" {label}"), theme.dim().with_bg(bg));
            }
            HeaderLabel::Profile(name) => {
                let gx = frame.put_str(kx, y, " ", theme.dim().with_bg(bg));
                frame.set(
                    gx,
                    y,
                    Icon::Profile.glyph(unicode),
                    theme.key_hint().with_bg(bg),
                );
                frame.put_str(gx + 1, y, &format!(" {name}"), theme.dim().with_bg(bg));
            }
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
    let ctx = keymap::context(
        vm.mode,
        vm.focus,
        vm.overlay.as_ref(),
        &vm.profile_menu.stage,
        vm.project_settings.running,
        // the family view: the adopt stage is derived from the SELECTED row's
        // status, which is the same premise `input::selection_is_unadopted`
        // binds `A` on — so the bar teaches the key exactly where the reducer
        // answers it.
        vm.projects
            .get(vm.selected)
            .is_some_and(|r| r.status == crate::ProjectStatus::NotAdopted),
    );
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
        for span in header_action_spans(80, None) {
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
        let spans = header_action_spans(80, None);
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
        let spans = header_action_spans(80, None);
        assert_eq!(spans.len(), 4, "unwired: the four static actions");
        for span in &spans {
            assert_eq!(header_action_at(80, None, span.start), Some(span.action));
            assert_eq!(header_action_at(80, None, span.end - 1), Some(span.action));
            // One past a span is a gap (or the margin) — never a target.
            assert_eq!(header_action_at(80, None, span.end), None);
        }
        assert_eq!(
            header_action_at(80, None, 0),
            None,
            "the mark is not a target"
        );
        // Too narrow: the cluster clips away whole; nothing is clickable.
        assert!(header_action_spans(30, None).is_empty());
        assert_eq!(header_action_at(30, None, 15), None);
        // Wired: five items, the identity immediately before Palette
        // (the profile hub).
        let spans = header_action_spans(90, Some("work"));
        assert_eq!(spans.len(), 5);
        assert_eq!(
            spans.iter().map(|s| s.action).collect::<Vec<_>>(),
            vec![
                HeaderAction::Help,
                HeaderAction::Profile,
                HeaderAction::Palette,
                HeaderAction::Editor,
                HeaderAction::Quit
            ]
        );
    }

    #[test]
    fn an_unwired_cluster_keeps_the_four_item_geometry() {
        // With no profile the cluster is byte-identical to its pre-hub
        // four-item form: same positions, same clip threshold — so the
        // unprofiled goldens never re-cut.
        let spans = header_action_spans(80, None);
        assert_eq!(spans.first().map(|s| s.start), Some(38), "right-aligned");
        assert_eq!(spans.last().map(|s| s.end), Some(79), "one-cell margin");
        assert!(header_action_spans(60, None).is_empty());
        assert_eq!(header_action_spans(61, None).len(), 4);
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

    /// The identity item's span at width `w`, if the ladder kept it.
    fn identity_span(w: u16, name: &str) -> Option<(u16, u16)> {
        header_cluster(w, Some(name))
            .into_iter()
            .find(|i| i.span.action == HeaderAction::Profile)
            .map(|i| (i.span.start, i.span.end))
    }

    #[test]
    fn identity_button_draws_exactly_on_its_span() {
        // the profile hub: one cluster solver feeds the renderer and the hit-test —
        // the key glyph opens the span, the profile glyph and name follow, and
        // every cell in [start, end) resolves Profile while the gaps miss.
        let v = vm_with_profile("work", 80, 24);
        let mut f = Frame::new(Size::new(80, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let (start, end) = identity_span(80, "work").expect("shows on a wide header");
        assert_eq!(f.get(start, 0).unwrap().ch, 'p', "the chrome teaches `p`");
        assert_eq!(f.get(start + 2, 0).unwrap().ch, Icon::Profile.unicode());
        let text: String = (start..end).map(|x| f.get(x, 0).unwrap().ch).collect();
        assert!(
            text.contains("work"),
            "the name renders on the span: {text}"
        );
        for col in start..end {
            assert_eq!(
                header_action_at(80, Some("work"), col),
                Some(HeaderAction::Profile)
            );
        }
        assert_eq!(header_action_at(80, Some("work"), start - 1), None);
        assert_eq!(header_action_at(80, Some("work"), end), None);
    }

    #[test]
    fn identity_name_truncates_with_an_ellipsis_at_the_budget() {
        let long = "a-very-long-profile-name-indeed";
        let v = vm_with_profile(long, 120, 24);
        let (start, end) = identity_span(120, long).expect("shows when wide");
        // `p` + ` ` + `⦿ ` + 15 chars + `…` = 20 cells.
        assert_eq!(end - start, 20, "the span holds the budgeted label");
        let mut f = Frame::new(Size::new(120, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let text: String = (start..end).map(|x| f.get(x, 0).unwrap().ch).collect();
        assert!(text.ends_with('…'), "truncation is an ellipsis: {text}");
    }

    #[test]
    fn identity_clips_away_whole_on_a_narrow_header() {
        // The ladder (the profile hub) spends the name first: at moderate
        // narrowness the label shrinks to its ellipsis floor…
        let v = vm_with_profile("work", 75, 24);
        let (start, end) =
            identity_span(75, "work").expect("above the floor the item shrinks instead of hiding");
        let mut f = Frame::new(Size::new(75, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let text: String = (start..end).map(|x| f.get(x, 0).unwrap().ch).collect();
        assert!(
            text.ends_with('…'),
            "the floor label is an ellipsis: {text}"
        );
        // …narrower, the item drops WHOLE while the four actions stand…
        let v = vm_with_profile("work", 70, 24);
        assert_eq!(identity_span(70, "work"), None);
        let spans = header_action_spans(70, Some("work"));
        assert_eq!(spans.len(), 4, "the actions survive the identity drop");
        let mut f = Frame::new(Size::new(70, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let row: String = (0..70u16)
            .map(|x| f.get(x, 0).map_or(' ', |c| c.ch))
            .collect();
        assert!(
            !row.contains(Icon::Profile.unicode()),
            "no partial button: {row}"
        );
        for col in 0..70u16 {
            assert_ne!(
                header_action_at(70, Some("work"), col),
                Some(HeaderAction::Profile)
            );
        }
        // …and at the narrowest the whole cluster clips; nothing hits.
        assert!(header_action_spans(50, Some("work")).is_empty());
    }

    #[test]
    fn identity_name_shrinks_before_the_button_hides() {
        // The hidden-band class, found in real use: at widths where the full name
        // misses the fit, the label shrinks instead of vanishing.
        let v = vm_with_profile("collaboration", 80, 24);
        let (start, end) =
            identity_span(80, "collaboration").expect("the shrunk item shows in the cluster");
        let mut f = Frame::new(Size::new(80, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let text: String = (start..end).map(|x| f.get(x, 0).unwrap().ch).collect();
        assert!(text.ends_with('…'), "shrunk with an ellipsis: {text}");
        // The shrunk label and the hit target come from one solver.
        for col in start..end {
            assert_eq!(
                header_action_at(80, Some("collaboration"), col),
                Some(HeaderAction::Profile)
            );
        }
        // Wider, the same name renders whole again.
        let v = vm_with_profile("collaboration", 95, 24);
        let (start, end) = identity_span(95, "collaboration").unwrap();
        let mut f = Frame::new(Size::new(95, 24));
        let area = f.rect();
        draw_header(&v, Theme::rift_dark(), true, &mut f, area);
        let text: String = (start..end).map(|x| f.get(x, 0).unwrap().ch).collect();
        assert!(text.contains("collaboration"), "{text}");
    }

    #[test]
    fn identity_visibility_is_monotonic_across_widths() {
        // the profile hub's keystone: once the identity is visible at a width it
        // is visible at every greater width — the property whose absence was
        // the silent hidden band (62–77 cells, before this rule).
        for name in ["work", "collaboration", "a-very-long-profile-name-indeed"] {
            let mut seen = false;
            for w in 10..=200u16 {
                let visible = identity_span(w, name).is_some();
                assert!(
                    visible || !seen,
                    "{name}: visible at a narrower width but hidden at {w}"
                );
                seen = seen || visible;
            }
            assert!(seen, "{name}: the item shows somewhere in the sweep");
        }
    }

    #[test]
    fn identity_hides_until_the_profile_plane_is_wired() {
        let v = vm(80, 24); // no profiles set
        assert_eq!(v.profile_name, None);
        assert!(header_cluster(80, None)
            .iter()
            .all(|i| i.span.action != HeaderAction::Profile));
    }

    #[test]
    fn identity_reads_active_while_the_menu_is_open_and_flashes_on_press() {
        use crate::input::Mode;
        let mut v = vm_with_profile("work", 80, 24);
        v.mode = Mode::ProfileMenu;
        let (start, _) = identity_span(80, "work").unwrap();
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
        let hostile = "wo\x1b[31mrk\x07";
        let v = vm_with_profile(hostile, 120, 24);
        let (start, end) = identity_span(120, hostile).unwrap();
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
