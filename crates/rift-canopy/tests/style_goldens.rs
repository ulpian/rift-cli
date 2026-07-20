//! Style-aware colour goldens (M55-03): the surface theme made visible to
//! snapshots. Each surface is rendered at all four capability tiers (truecolor →
//! 256 → mono → ASCII) through [`common::style_snapshot`], so a colour
//! regression fails a test instead of a dogfood eyeball (ADR-0023). This file
//! seeds the pattern over the W0 chrome primitives; later waves add one golden
//! per load-bearing surface (header, rail, cards, column-3 panels, tiles, the
//! donut) by the same call.
//!
//! **Golden budget** (the M55-03 decision, recorded in the map): full four-tier
//! goldens for chrome and load-bearing surfaces (few distinct styles each, so
//! the ×4 multiplication stays reviewable); the mono/ASCII tiers visibly carry
//! no colour, which is the `NO_COLOR` guarantee shown, not just tested.

mod common;

use common::four_tier_golden;
use rift_canopy::chrome;
use rift_canopy::render::frame::{Frame, Size};
use rift_canopy::render::{Capabilities, RenderBackend, RenderError};
use rift_canopy::theme::{Icon, Section, Theme, Tier};

/// A capability-bearing backend for the golden renders: it reports the tier's
/// capability set so `view::draw` chooses Unicode vs ASCII glyphs to match, and
/// draws the plot floor tier (deterministic). Present is a no-op (headless).
struct TierBackend {
    caps: Capabilities,
}

impl RenderBackend for TierBackend {
    fn name(&self) -> &'static str {
        "tier"
    }
    fn capabilities(&self) -> Capabilities {
        self.caps
    }
    fn present(&mut self, _frame: &Frame) -> Result<(), RenderError> {
        Ok(())
    }
}

/// The capability set whose `UNICODE` bit matches the tier's glyph choice, so
/// the frame's glyphs and the serializer's per-tier resolution agree.
fn caps_for(tier: Tier) -> Capabilities {
    if tier.unicode() {
        Capabilities::TRUECOLOR | Capabilities::UNICODE
    } else {
        Capabilities::empty()
    }
}

/// A compact frame exercising every chrome primitive — a badge and a button in
/// the header row, then a themed "capture" card with three icon-labelled stat
/// tiles — drawn for `tier` (icons drop to ASCII on the floor).
fn chrome_demo(tier: Tier) -> Frame {
    let theme = Theme::rift_dark();
    let unicode = tier.unicode();
    let mut f = Frame::new(Size::new(46, 9));

    // Header row: a count badge + an editor button.
    let mut x = chrome::badge(
        &mut f,
        0,
        0,
        "3 projects",
        theme.badge(Section::Projects.accent()),
    );
    x = x.saturating_add(2);
    chrome::button(
        &mut f,
        x,
        0,
        "Open",
        "e",
        theme.button(false),
        theme.key_hint(),
    );

    // The capture card.
    let card = rift_canopy::layout::Rect {
        x: 0,
        y: 1,
        w: 46,
        h: 8,
    };
    let inner = chrome::panel(
        &mut f,
        card,
        "capture",
        theme.border(Section::Capture, false),
        theme.title(Section::Capture),
    );

    // Three icon-labelled tiles, each in its capture-cyan border. The icon
    // leads the tile title (dropping to ASCII on the floor via `unicode`).
    let tiles = [
        (Icon::Events, "events", "128"),
        (Icon::Sessions, "sess", "6"),
        (Icon::Checkpoints, "checks", "12"),
    ];
    let accent = Section::Capture.accent();
    let tw = 14u16;
    for (i, (ic, label, value)) in tiles.into_iter().enumerate() {
        let Ok(i16) = u16::try_from(i) else { break };
        let tx = inner.x + i16 * tw;
        let trect = rift_canopy::layout::Rect {
            x: tx,
            y: inner.y,
            w: tw,
            h: 4,
        };
        let title = format!("{} {label}", ic.glyph(unicode));
        chrome::tile(
            &mut f,
            trect,
            &title,
            value,
            theme.tile_border(accent),
            theme.dim(),
            theme.tile_value(accent),
        );
    }
    f
}

#[test]
fn golden_chrome_primitives_four_tiers() {
    insta::assert_snapshot!("chrome_primitives", four_tier_golden(chrome_demo));
}

// ---- M55-04: the chrome bars (header · hint · command) ----------------------

/// A live dashboard state with a fixed version and a degraded health dot, so
/// the header exercises its full vocabulary.
fn bars_vm(w: u16, h: u16) -> rift_canopy::ViewModel {
    use rift_canopy::{AppState, EntryKind, Project, ProjectStatus};
    let mut s = AppState::new(
        vec![Project {
            name: "rift-cli".into(),
            path: "/r/rift-cli".into(),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }],
        Size::new(w, h),
    );
    s.app.splash.skip();
    s.version = "0.1.0".into();
    s.health = rift_canopy::theme::Health::Warn; // amber: a worker error held
    s.snapshot()
}

/// Render just the three chrome bars for `tier` — header on row 0, the hint
/// bar and command bar in the bottom rows (the full-dashboard draw would drag
/// every panel into the golden; the bars are what M55-04 pins).
fn bars_frame(tier: Tier) -> Frame {
    use rift_canopy::layout::{chrome_split, Rect};
    let vm = bars_vm(80, 14);
    let mut f = Frame::new(Size::new(80, 14));
    let theme = Theme::rift_dark();
    let header = Rect {
        x: 0,
        y: 0,
        w: 80,
        h: 1,
    };
    rift_canopy::bars::draw_header(&vm, theme, tier.unicode(), &mut f, header);
    let body = Rect {
        x: 0,
        y: 1,
        w: 80,
        h: 13,
    };
    let (_, hint, command) = chrome_split(body);
    rift_canopy::bars::draw_hint_bar(&vm, theme, &mut f, hint);
    // The command bar renders through the full view (it needs the vm); draw it
    // here via the chrome panel exactly as `view::draw_command` styles it.
    let inner = rift_canopy::chrome::panel(
        &mut f,
        command,
        "command",
        theme.border(Section::Command, false),
        theme.title(Section::Command),
    );
    let px = f.put_str(inner.x, inner.y, "/", theme.key_hint());
    f.put_str(px, inner.y, "\u{2588}", theme.body());
    f
}

#[test]
fn golden_chrome_bars_four_tiers() {
    insta::assert_snapshot!("chrome_bars", four_tier_golden(bars_frame));
}

// ---- M55-04/05: the whole themed dashboard, per tier ------------------------

/// The full dashboard (header · projects rail · detail · col3 placeholders ·
/// hint · command) rendered through the real `view::draw` for `tier`. This is
/// the load-bearing surface the later waves' cards/panels regenerate; here it
/// pins the W1 header/hint/command chrome and the W2 rail (count badge, kind
/// icons, status dots, the selection pill) at every capability tier.
fn dashboard_frame(tier: Tier) -> Frame {
    use rift_canopy::{AppState, EntryKind, Project, ProjectStatus};
    let mut s = AppState::new(
        vec![
            Project {
                name: "rift-cli".into(),
                path: "/r/rift-cli".into(),
                status: ProjectStatus::Running {
                    since_ms: 0,
                    active_sessions: 1,
                },
                kind: EntryKind::Repo,
            },
            Project {
                name: "feature-wt".into(),
                path: "/r/rift-cli-wt".into(),
                status: ProjectStatus::NotInitialized,
                kind: EntryKind::Worktree,
            },
            Project {
                name: "rift-site".into(),
                path: "/r/rift-site".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            },
        ],
        Size::new(90, 18),
    );
    s.app.splash.skip();
    s.version = "0.1.0".into();
    s.health = rift_canopy::theme::Health::Ok;
    let vm = s.snapshot();
    let mut f = Frame::new(vm.size);
    let mut backend = TierBackend {
        caps: caps_for(tier),
    };
    rift_canopy::view::draw(&vm, &mut backend, &mut f);
    f
}

#[test]
fn golden_dashboard_four_tiers() {
    insta::assert_snapshot!("dashboard_themed", four_tier_golden(dashboard_frame));
}

// ---- ADR-0038: the runtime surface theme (M5.11) -----------------------------

/// Every slot overridden — the fully-themed fixture the ADR-0038 golden pins.
fn full_overrides() -> rift_canopy::ThemeOverrides {
    use rift_canopy::render::frame::Color;
    use rift_canopy::{ThemeOverrides, ThemeSlot};
    let mut o = ThemeOverrides::NONE;
    o.set(ThemeSlot::Background, Some(Color::Rgb(0x20, 0x14, 0x2c)));
    o.set(ThemeSlot::Border, Some(Color::Rgb(0xb4, 0x8c, 0xff)));
    o.set(ThemeSlot::Button, Some(Color::Rgb(0xf0, 0x8c, 0xc4)));
    o.set(ThemeSlot::Graph, Some(Color::Rgb(0x5c, 0xa8, 0xff)));
    o.set(ThemeSlot::Text, Some(Color::Rgb(0xe8, 0xec, 0xe8)));
    o
}

/// The full dashboard with EVERY theme slot overridden, per tier — the second
/// identity ADR-0038 §5 pins (the first is the untouched goldens above, which
/// must pass unchanged: no overrides ⇒ byte-identity with Rift dark).
fn themed_dashboard_frame(tier: Tier) -> Frame {
    use rift_canopy::{AppState, EntryKind, Project, ProjectStatus};
    let mut s = AppState::new(
        vec![Project {
            name: "rift-cli".into(),
            path: "/r/rift-cli".into(),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }],
        Size::new(90, 18),
    );
    s.app.splash.skip();
    s.version = "0.1.0".into();
    s.app.set_profiles(
        vec![rift_canopy::profile::ProfileEntry {
            profile_id: "pf0".into(),
            name: "lab".into(),
            created_at: 0,
        }],
        "pf0".into(),
    );
    s.set_theme(full_overrides());
    let vm = s.snapshot();
    let mut f = Frame::new(vm.size);
    let mut backend = TierBackend {
        caps: caps_for(tier),
    };
    rift_canopy::view::draw(&vm, &mut backend, &mut f);
    f
}

#[test]
fn golden_fully_overridden_dashboard_four_tiers() {
    insta::assert_snapshot!(
        "dashboard_overridden",
        four_tier_golden(themed_dashboard_frame)
    );
}

/// ADR-0038 §3's floor proof: on the mono and ASCII tiers a profile palette
/// changes NOTHING — the lattice drops colour by construction, so the themed
/// and untouched serializations are identical. (`NO_COLOR` forces the ASCII
/// tier, so this is also the `NO_COLOR` guarantee.) The only glyph difference
/// between the fixtures is the identity button the themed one carries, so both
/// frames here use the same profiled state and differ ONLY in overrides.
#[test]
fn a_palette_changes_nothing_on_the_floor_tiers() {
    use rift_canopy::{AppState, EntryKind, Project, ProjectStatus};
    let frame_for = |overrides: Option<rift_canopy::ThemeOverrides>, tier: Tier| {
        let mut s = AppState::new(
            vec![Project {
                name: "rift-cli".into(),
                path: "/r/rift-cli".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(90, 18),
        );
        s.app.splash.skip();
        s.version = "0.1.0".into();
        s.app.set_profiles(
            vec![rift_canopy::profile::ProfileEntry {
                profile_id: "pf0".into(),
                name: "lab".into(),
                created_at: 0,
            }],
            "pf0".into(),
        );
        if let Some(o) = overrides {
            s.set_theme(o);
        }
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        let mut backend = TierBackend {
            caps: caps_for(tier),
        };
        rift_canopy::view::draw(&vm, &mut backend, &mut f);
        common::style_snapshot(&f, tier)
    };
    for tier in [Tier::Mono, Tier::Ascii] {
        assert_eq!(
            frame_for(None, tier),
            frame_for(Some(full_overrides()), tier),
            "{tier:?}: a palette must not manifest on a colourless tier"
        );
    }
    // And the sanity inverse: on truecolor the override IS visible.
    assert_ne!(
        frame_for(None, Tier::Truecolor),
        frame_for(Some(full_overrides()), Tier::Truecolor),
        "truecolor renders the palette"
    );
}

// ---- M5.14: context pressure (radial on SUBCELL, bar degrade) ---------------

/// Render the system panel for a 62% reading, on a backend whose capabilities
/// `caps` decide radial-vs-bar. Used to pin the donut's two tiers.
fn system_panel(caps: Capabilities, w: u16, h: u16) -> Frame {
    use rift_canopy::{Col3, NicReading, NicView};
    let col3 = Col3 {
        nic: NicView::Reading(NicReading {
            pct: 62,
            band: "Moderate",
            model: "claude-fable-5".into(),
            active_sessions: 2,
        }),
        active_sessions: rift_canopy::ActiveSessionsView::Count(2),
        metrics: rift_canopy::PanelMetrics {
            events_5m: Some(42),
            tokens_5m: Some(12_400),
            mcp_calls_5m: Some(7),
            projection_bytes: Some(18_600_000),
            context_used: Some(124_000),
            context_window: Some(200_000),
            context_scope: rift_canopy::SystemScope::CurrentSessions,
            activity_scope: rift_canopy::SystemScope::CurrentSessions,
        },
        ..Col3::default()
    };
    let mut f = Frame::new(Size::new(w, h));
    let mut backend = TierBackend { caps };
    let unicode = caps.contains(Capabilities::UNICODE);
    rift_canopy::col3::draw(
        &col3,
        &[rift_canopy::col3::SectionId::System],
        false,
        unicode,
        rift_canopy::layout::Rect { x: 0, y: 0, w, h },
        0,
        &mut backend,
        &mut f,
    );
    f
}

#[test]
fn golden_donut_radial_on_subcell_and_bar_degrade() {
    // SUBCELL-capable → the radial arc; no SUBCELL → the horizontal gauge bar.
    let sub = Capabilities::TRUECOLOR | Capabilities::UNICODE | Capabilities::SUBCELL;
    insta::assert_snapshot!(
        "donut_radial",
        common::style_snapshot(&system_panel(sub, 30, 14), Tier::Truecolor)
    );
    let plain = Capabilities::TRUECOLOR | Capabilities::UNICODE;
    insta::assert_snapshot!(
        "donut_bar_degrade",
        common::style_snapshot(&system_panel(plain, 30, 14), Tier::Truecolor)
    );
}

#[test]
fn donut_unknown_model_shows_no_percentage() {
    use rift_canopy::{Col3, NicView};
    let sub = Capabilities::TRUECOLOR | Capabilities::UNICODE | Capabilities::SUBCELL;
    let col3 = Col3 {
        nic: NicView::UnknownModel {
            model: "mystery-9000".into(),
            active_sessions: 1,
        },
        active_sessions: rift_canopy::ActiveSessionsView::Count(1),
        metrics: rift_canopy::PanelMetrics {
            context_used: Some(125),
            context_scope: rift_canopy::SystemScope::CurrentSessions,
            ..rift_canopy::PanelMetrics::default()
        },
        ..Col3::default()
    };
    let mut f = Frame::new(Size::new(30, 14));
    let mut b = TierBackend { caps: sub };
    rift_canopy::col3::draw(
        &col3,
        &[rift_canopy::col3::SectionId::System],
        false,
        true,
        rift_canopy::layout::Rect {
            x: 0,
            y: 0,
            w: 30,
            h: 14,
        },
        0,
        &mut b,
        &mut f,
    );
    let text: String = (0..f.height())
        .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
        .map(|(x, y)| f.get(x, y).map_or(' ', |c| c.ch))
        .collect();
    assert!(text.contains("unknown"), "the explicit indicator renders");
    assert!(
        !text.contains('%'),
        "an unknown model never invents a percentage"
    );
}

/// The serializer's own contract, asserted directly (not via a golden): the
/// truecolor tier carries RGB colour; the mono/ASCII floors carry attributes
/// but **no colour** (every legend entry's fg and bg is the terminal default —
/// the `NO_COLOR` guarantee); and the ASCII floor substitutes the icons.
#[test]
fn tier_floor_carries_attributes_but_no_colour() {
    let tc = common::style_snapshot(&chrome_demo(Tier::Truecolor), Tier::Truecolor);
    assert!(tc.contains("fg=#"), "truecolor carries RGB colour");

    for tier in [Tier::Mono, Tier::Ascii] {
        let snap = common::style_snapshot(&chrome_demo(tier), tier);
        assert!(!snap.contains("fg=#"), "{tier:?} leaks an RGB colour");
        assert!(
            !snap.contains("fg=@"),
            "{tier:?} leaks a 256-indexed colour"
        );
        assert!(
            !snap.contains("bg=#") && !snap.contains("bg=@"),
            "{tier:?} leaks a bg colour"
        );
        // Attributes survive the floor — the title stays bold.
        assert!(snap.contains("+bold"), "{tier:?} keeps bold attributes");
    }

    // The ASCII tier substitutes the capture icons for printable ASCII.
    let ascii = common::style_snapshot(&chrome_demo(Tier::Ascii), Tier::Ascii);
    assert!(
        ascii.contains(Icon::Events.ascii()),
        "ASCII floor substitutes the events icon"
    );
    assert!(
        !ascii.contains(Icon::Events.unicode()),
        "no Unicode icon survives on the floor"
    );
}
