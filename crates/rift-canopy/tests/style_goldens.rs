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
