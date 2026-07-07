//! The **surface theme** — "Rift dark" — and the capability-tiered colour
//! resolution (ADR-0023).
//!
//! This is the one built-in theme: a typed table of per-section title/border
//! styles, tile/badge/pill styles, selection/focus/hover styles, header and
//! hint chrome, the health-dot colours, and the canonical token-class → colour
//! map. It **extends** the colour foundation the graphs already ship (the
//! gamma-correct [`lerp_color`]/[`ramp_color`] helpers and the
//! [`crate::plot::Paint`] palettes stay here unchanged); the theme adds the
//! chrome vocabulary on top.
//!
//! Colour is authored once at full fidelity (24-bit RGB) and **resolved through
//! the capability lattice** by [`resolve_style`]: truecolor passes through, 256
//! quantizes into the xterm cube, and the mono/ASCII floors keep attributes but
//! carry **no colour by construction** — which is exactly what makes `NO_COLOR`
//! compliance a property rather than a feature (the forced-ASCII cap set is
//! empty, so its tier is [`Tier::Ascii`], which drops every colour). Draw code
//! targets the theme and never branches on capability; the tier is applied at
//! the backend boundary (and by the style-aware snapshot serializer).
//!
//! Vocabulary (ADR-0023): the colour system is the **surface theme** — here,
//! in docs, and in tests. "Palette" is only ever the command surface.

use crate::render::frame::{Color, Style};
use crate::render::{Capabilities, Mods};

// ── colour foundation (shared with the plot paints) ─────────────────────────

/// The Rift accent green — titles, selection, the savings line.
pub const ACCENT: Color = Color::Rgb(0x6c, 0xe0, 0x9a);

/// Token-usage bars: a vertical gradient — deep teal at the baseline brightening
/// to the bright green top as a bar rises.
pub const TOKEN_LO: Color = Color::Rgb(0x1f, 0x6f, 0x7a);
/// The bright top of the token-bar gradient.
pub const TOKEN_HI: Color = Color::Rgb(0x8c, 0xf0, 0xb4);

/// The four token classes, in `[input, read, output, cache_write]` order — the
/// **canonical class → colour map**. [`crate::plot::Paint::ByCategory`] draws
/// the categorical chart from it and the stat tiles border in the same colours,
/// so a class keeps one hue everywhere (user story 2). Deuteranopia-safe
/// (blue / green / violet / amber).
pub const BREAKDOWN: [Color; 4] = [
    Color::Rgb(0x5c, 0xa8, 0xff), // input       — uncached (blue)
    Color::Rgb(0x6c, 0xe0, 0x9a), // read        — cache read (green)
    Color::Rgb(0xb4, 0x8c, 0xff), // output      — (violet)
    Color::Rgb(0xf0, 0xb4, 0x5c), // cache-write — secondary (amber)
];

/// The nic-gauge / dumbometer thermometer ramp: green (empty) → amber → red
/// (full). Sampled by fill fraction, so a fuller meter reddens.
pub const GAUGE_RAMP: [(f32, Color); 3] = [
    (0.0, Color::Rgb(0x6c, 0xe0, 0x9a)),
    (0.6, Color::Rgb(0xf0, 0xc4, 0x5c)),
    (1.0, Color::Rgb(0xf0, 0x5c, 0x5c)),
];

/// Recall MCP-calls bars — a cool cyan, distinct from the green savings line.
pub const RECALL_CALLS: Color = Color::Rgb(0x5c, 0xc4, 0xe0);

/// The muted foreground text fades in FROM (ADR-0022) and descriptions rest at.
pub const MUTED: Color = Color::Rgb(0x5a, 0x64, 0x5a);

/// The settled body foreground (rows fade TO it).
pub const BODY_FG: Color = Color::Rgb(0xd8, 0xdd, 0xd8);

// ── chrome vocabulary (new in M5.5) ─────────────────────────────────────────

/// The idle (unfocused) border tone — a low-contrast slate that reads as a
/// frame without competing with the focused accent.
pub const BORDER_IDLE: Color = Color::Rgb(0x3a, 0x42, 0x40);

/// A filled-surface background (badges, tiles, the header bar) — one step above
/// the terminal's own background so a filled region reads as raised.
pub const SURFACE_BG: Color = Color::Rgb(0x1a, 0x20, 0x1e);

/// The selection pill's background — the accent, dimmed so reversed text reads.
pub const SELECT_BG: Color = Color::Rgb(0x24, 0x40, 0x33);

/// The hover-target background — a fainter wash than [`SELECT_BG`].
pub const HOVER_BG: Color = Color::Rgb(0x22, 0x2c, 0x2a);

/// Health dot — the data plane is connected and polling cleanly.
pub const HEALTH_OK: Color = Color::Rgb(0x6c, 0xe0, 0x9a);
/// Health dot — the bus worker reported an error (degraded, recoverable).
pub const HEALTH_WARN: Color = Color::Rgb(0xf0, 0xc4, 0x5c);
/// Health dot — the canopy store is unreachable (the data plane is dead).
pub const HEALTH_DOWN: Color = Color::Rgb(0xf0, 0x5c, 0x5c);

/// The four status-dot colours for the projects rail, mirroring
/// [`crate::ProjectStatus`]: running (green), picking-up (amber), idle (slate),
/// uninitialized (dim), error (red).
pub const STATUS_RUNNING: Color = Color::Rgb(0x6c, 0xe0, 0x9a);
/// Picking-up / building.
pub const STATUS_BUSY: Color = Color::Rgb(0xf0, 0xc4, 0x5c);
/// Registered and quiet.
pub const STATUS_IDLE: Color = Color::Rgb(0x7a, 0x86, 0x82);
/// Registered but not initialized.
pub const STATUS_UNINIT: Color = Color::Rgb(0x4a, 0x52, 0x50);
/// A status read failed.
pub const STATUS_ERROR: Color = Color::Rgb(0xf0, 0x5c, 0x5c);

/// A dashboard surface with its own colour identity. Each card, panel, rail,
/// and chrome bar names one; the theme maps it to a title + border tone so a
/// developer finds capture, features, or recall by glance (user story 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Section {
    /// Column 1 — the projects rail.
    Projects,
    /// Column 2 — the repository card.
    Repository,
    /// Column 2 — the models card.
    Models,
    /// Column 2 — the settings card.
    Settings,
    /// Column 2 — the capture card.
    Capture,
    /// Column 2 — the features card.
    Features,
    /// Column 2 — the activity card.
    Activity,
    /// Column 3 — the token-usage panel.
    TokenUsage,
    /// Column 3 — the system panel (the dumbometer donut + domain metrics).
    System,
    /// Column 3 — the recall panel.
    Recall,
    /// The header bar.
    Header,
    /// The hint bar above the command bar.
    Hint,
    /// The command bar.
    Command,
}

impl Section {
    /// This section's identity colour — the hue its title (and focused border)
    /// carry. The set is chosen to keep neighbouring cards distinguishable on a
    /// dark terminal (user story 1).
    #[must_use]
    pub const fn accent(self) -> Color {
        match self {
            // Recall shares the accent green — its scoreboard is the payoff colour.
            Self::Projects | Self::Header | Self::Command | Self::Recall => ACCENT,
            Self::Repository => Color::Rgb(0x5c, 0xa8, 0xff), // blue
            Self::Models => Color::Rgb(0xb4, 0x8c, 0xff),     // violet
            Self::Settings => Color::Rgb(0x8a, 0x94, 0x90),   // slate
            Self::Capture => Color::Rgb(0x5c, 0xc4, 0xe0),    // cyan
            Self::Features => Color::Rgb(0xf0, 0xc4, 0x5c),   // amber
            Self::Activity => Color::Rgb(0x8c, 0xf0, 0xb4),   // bright green
            Self::TokenUsage => Color::Rgb(0x5c, 0xd0, 0xc0), // teal
            Self::System => Color::Rgb(0xf0, 0xb4, 0x5c),     // warm amber
            Self::Hint => MUTED,
        }
    }
}

// ── the theme table ─────────────────────────────────────────────────────────

/// The one built-in surface theme — "Rift dark". A zero-sized handle: the theme
/// is code, not config (ADR-0023), so it carries no state; its methods are the
/// single source of every chrome style. A future config surface swaps this type
/// for a loaded table without touching a call site.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Theme;

impl Theme {
    /// The built-in "Rift dark" theme.
    #[must_use]
    pub const fn rift_dark() -> Self {
        Self
    }

    /// A section's title style — its accent, bold.
    #[must_use]
    pub const fn title(self, section: Section) -> Style {
        Style::fg(section.accent()).with_mods(Mods::BOLD)
    }

    /// A section's border style: the accent when focused, the idle slate
    /// otherwise (mirrors the M4 focused-border convention, now themed).
    #[must_use]
    pub const fn border(self, section: Section, focused: bool) -> Style {
        if focused {
            Style::fg(section.accent())
        } else {
            Style::fg(BORDER_IDLE)
        }
    }

    /// Ordinary body text on a card.
    #[must_use]
    pub const fn body(self) -> Style {
        Style::fg(BODY_FG)
    }

    /// De-emphasized text (secondary values, `+N more`, placeholders).
    #[must_use]
    pub const fn dim(self) -> Style {
        Style::fg(MUTED)
    }

    /// A count/status pill badge (e.g. the rail's project count) — the accent on
    /// the surface fill.
    #[must_use]
    pub const fn badge(self, accent: Color) -> Style {
        Style::fg(accent).with_bg(SURFACE_BG).with_mods(Mods::BOLD)
    }

    /// The selection pill: body text reversed onto the selection wash.
    #[must_use]
    pub const fn selection(self) -> Style {
        Style::fg(ACCENT).with_bg(SELECT_BG).with_mods(Mods::BOLD)
    }

    /// A hover target's wash (pointer acknowledgement).
    #[must_use]
    pub const fn hover(self) -> Style {
        Style::fg(BODY_FG).with_bg(HOVER_BG)
    }

    /// A stat tile's border, coloured by the class/section it reports.
    #[must_use]
    pub const fn tile_border(self, accent: Color) -> Style {
        Style::fg(accent)
    }

    /// A stat tile's value text.
    #[must_use]
    pub const fn tile_value(self, accent: Color) -> Style {
        Style::fg(accent).with_mods(Mods::BOLD)
    }

    /// A button (e.g. *Open in $EDITOR*): its label style, focused or not.
    #[must_use]
    pub const fn button(self, focused: bool) -> Style {
        if focused {
            Style::fg(ACCENT).with_bg(SELECT_BG).with_mods(Mods::BOLD)
        } else {
            Style::fg(BODY_FG).with_bg(SURFACE_BG)
        }
    }

    /// A key-hint glyph (the letter in `? Help`) — accented and bold.
    #[must_use]
    pub const fn key_hint(self) -> Style {
        Style::fg(ACCENT).with_mods(Mods::BOLD)
    }

    /// The header bar's base fill: body text on the raised surface.
    #[must_use]
    pub const fn header_bar(self) -> Style {
        Style::fg(BODY_FG).with_bg(SURFACE_BG)
    }

    /// A project status dot's style (the rail's trailing dot, M55-05).
    #[must_use]
    pub const fn status_dot(self, status: crate::ProjectStatus) -> Style {
        Style::fg(status_color(status))
    }

    /// The style for a canonical token class (chart + tile share it).
    #[must_use]
    pub fn token_class(self, class: TokenClass) -> Style {
        Style::fg(class.color())
    }

    /// The health-dot style for a data-plane [`Health`] state.
    #[must_use]
    pub const fn health(self, health: Health) -> Style {
        Style::fg(health.color()).with_mods(Mods::BOLD)
    }
}

/// The status-dot colour for a project status (single-sourced with the rail's
/// icon choice, [`Icon::for_status`]).
#[must_use]
pub const fn status_color(status: crate::ProjectStatus) -> Color {
    match status {
        crate::ProjectStatus::Running { .. } => STATUS_RUNNING,
        crate::ProjectStatus::PickingUp => STATUS_BUSY,
        crate::ProjectStatus::Idle => STATUS_IDLE,
        crate::ProjectStatus::NotInitialized => STATUS_UNINIT,
        crate::ProjectStatus::Error(_) => STATUS_ERROR,
    }
}

/// The data plane's health, as the header dot reports it (ADR-0024 §1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Health {
    /// Bus worker alive and the canopy store reachable.
    Ok,
    /// The worker reported an error (degraded, recoverable on a healthy poll).
    Warn,
    /// The store is unreachable — the data plane is dead.
    Down,
}

impl Health {
    /// The dot colour for this health.
    #[must_use]
    pub const fn color(self) -> Color {
        match self {
            Self::Ok => HEALTH_OK,
            Self::Warn => HEALTH_WARN,
            Self::Down => HEALTH_DOWN,
        }
    }
}

/// One of the four canonical token classes. The single enum both the chart and
/// the stat tiles switch on, so the class → colour map is sourced once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenClass {
    /// Uncached input tokens.
    Input,
    /// Cache-read input tokens.
    Read,
    /// Output tokens.
    Output,
    /// Cache-creation tokens (the secondary band).
    CacheWrite,
}

impl TokenClass {
    /// The four classes in canonical `[input, read, output, cache_write]` order
    /// — the same order as [`BREAKDOWN`].
    pub const ALL: [Self; 4] = [Self::Input, Self::Read, Self::Output, Self::CacheWrite];

    /// This class's index into [`BREAKDOWN`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Input => 0,
            Self::Read => 1,
            Self::Output => 2,
            Self::CacheWrite => 3,
        }
    }

    /// This class's canonical colour (the single source shared with the chart).
    #[must_use]
    pub fn color(self) -> Color {
        BREAKDOWN
            .get(self.index())
            .copied()
            .unwrap_or(Color::Default)
    }

    /// A short human label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::Read => "read",
            Self::Output => "output",
            Self::CacheWrite => "cache",
        }
    }
}

// ── icons: a curated Unicode-safe set with an ASCII substitution floor ───────

/// A themed glyph. Every icon resolves to a Unicode glyph on capable terminals
/// and to a printable-ASCII substitution on the floor tier — no Nerd-Font
/// dependency, and no tofu on a default terminal (ADR-0023).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    /// A git repository row.
    Repo,
    /// A linked worktree row.
    Worktree,
    /// Status: an agent session is active.
    StatusRunning,
    /// Status: picking up / building.
    StatusBusy,
    /// Status: registered and idle.
    StatusIdle,
    /// Status: registered but not initialized.
    StatusUninit,
    /// Status: a read error.
    StatusError,
    /// Capture tile — events.
    Events,
    /// Capture tile — sessions.
    Sessions,
    /// Capture tile — checkpoints.
    Checkpoints,
    /// A section/settings marker.
    Setting,
    /// A feature checkbox — on.
    CheckOn,
    /// A feature checkbox — off.
    CheckOff,
    /// A feature checkbox — in flight (enabling/disabling).
    CheckPending,
    /// A feature checkbox — failed.
    CheckFailed,
    /// A dot-label bullet (metric rows).
    Bullet,
    /// The data-plane health dot.
    HealthDot,
}

impl Icon {
    /// The status icon for a project status (colour comes from
    /// [`status_color`] — one fact, two renderings).
    #[must_use]
    pub const fn for_status(status: crate::ProjectStatus) -> Self {
        match status {
            crate::ProjectStatus::Running { .. } => Self::StatusRunning,
            crate::ProjectStatus::PickingUp => Self::StatusBusy,
            crate::ProjectStatus::Idle => Self::StatusIdle,
            crate::ProjectStatus::NotInitialized => Self::StatusUninit,
            crate::ProjectStatus::Error(_) => Self::StatusError,
        }
    }

    /// The kind icon for a rail row (repo vs linked worktree).
    #[must_use]
    pub const fn for_kind(kind: crate::EntryKind) -> Self {
        match kind {
            crate::EntryKind::Repo => Self::Repo,
            crate::EntryKind::Worktree => Self::Worktree,
        }
    }

    /// The Unicode glyph for this icon (the capable tier).
    // Conceptually-distinct icons may legitimately share a fallback glyph (a
    // dot, an error bang) — the enum documents intent; the glyph is cosmetic.
    #[allow(clippy::match_same_arms)]
    #[must_use]
    pub const fn unicode(self) -> char {
        match self {
            Self::Repo => '▣',
            Self::Worktree => '⌥',
            Self::StatusRunning => '●',
            Self::StatusBusy => '◐',
            Self::StatusIdle => '○',
            Self::StatusUninit => '·',
            Self::StatusError => '!',
            Self::Events => '⊹',
            Self::Sessions => '◇',
            Self::Checkpoints => '⎇',
            Self::Setting => '·',
            Self::CheckOn => '☑',
            Self::CheckOff => '☐',
            Self::CheckPending => '◌',
            Self::CheckFailed => '☒',
            Self::Bullet => '·',
            Self::HealthDot => '●',
        }
    }

    /// The printable-ASCII substitution for the floor tier (no glyph exceeds one
    /// cell; every icon has one, so the ASCII floor never renders tofu).
    #[allow(clippy::match_same_arms)] // shared fallbacks; see `unicode`
    #[must_use]
    pub const fn ascii(self) -> char {
        match self {
            Self::Repo => '#',
            Self::Worktree => '>',
            Self::StatusRunning => '*',
            Self::StatusBusy => '~',
            Self::StatusIdle => 'o',
            Self::StatusUninit => '-',
            Self::StatusError => '!',
            Self::Events => '.',
            Self::Sessions => '=',
            Self::Checkpoints => '^',
            Self::Setting => ':',
            Self::CheckOn => 'x',
            Self::CheckOff => ' ',
            Self::CheckPending => '~',
            Self::CheckFailed => '!',
            Self::Bullet => '-',
            Self::HealthDot => '*',
        }
    }

    /// The glyph for this icon at the given Unicode capability — the single
    /// resolver draw code calls (never branching on caps itself).
    #[must_use]
    pub const fn glyph(self, unicode: bool) -> char {
        if unicode {
            self.unicode()
        } else {
            self.ascii()
        }
    }
}

// ── the capability lattice ──────────────────────────────────────────────────

/// A colour-fidelity tier — the linear downgrade chain the theme resolves over
/// (truecolor → 256 → mono → ASCII). Derived from [`Capabilities`] at the
/// backend boundary; the style-aware goldens force one directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// 24-bit RGB passes through untouched.
    Truecolor,
    /// RGB quantizes into the xterm 256-colour cube.
    Ansi256,
    /// No colour — attributes only (bold/dim/reverse/underline survive).
    Mono,
    /// The floor — no colour and ASCII-substituted glyphs.
    Ascii,
}

impl Tier {
    /// Derive the tier from a capability set. The forced-ASCII / `NO_COLOR` cap
    /// set is empty and lands on [`Tier::Ascii`] — which is exactly why the
    /// floor carries no colour (see [`resolve_style`]).
    #[must_use]
    pub const fn from_caps(caps: Capabilities) -> Self {
        if caps.is_empty() {
            Self::Ascii
        } else if caps.contains(Capabilities::TRUECOLOR) {
            Self::Truecolor
        } else if caps.contains(Capabilities::UNICODE) {
            Self::Ansi256
        } else {
            Self::Mono
        }
    }

    /// Whether this tier draws Unicode glyphs. The mono tier arises from cap
    /// sets without the `UNICODE` probe (e.g. a legacy console), so it
    /// substitutes like the ASCII floor — matching the renderer, which keys
    /// glyph choice on `Capabilities::UNICODE` exactly.
    #[must_use]
    pub const fn unicode(self) -> bool {
        matches!(self, Self::Truecolor | Self::Ansi256)
    }

    /// Whether this tier carries any colour at all. Mono and ASCII do not — the
    /// `NO_COLOR` guarantee is structural.
    #[must_use]
    pub const fn has_colour(self) -> bool {
        matches!(self, Self::Truecolor | Self::Ansi256)
    }
}

/// Resolve a themed [`Style`] for a colour tier — the one downgrade every cell
/// passes through. **Total**: every style resolves at every tier. Truecolor is
/// the identity; 256 quantizes each RGB channel into the xterm cube; mono and
/// ASCII drop all colour to the terminal default and keep only the attributes.
#[must_use]
pub fn resolve_style(style: Style, tier: Tier) -> Style {
    let map = |c: Color| resolve_color(c, tier);
    Style {
        fg: map(style.fg),
        bg: map(style.bg),
        mods: style.mods,
    }
}

/// Resolve one colour for a tier.
#[must_use]
fn resolve_color(c: Color, tier: Tier) -> Color {
    match tier {
        Tier::Truecolor => c,
        Tier::Ansi256 => match c {
            Color::Rgb(r, g, b) => Color::Indexed(rgb_to_ansi256(r, g, b)),
            other => other,
        },
        // No colour by construction — the `NO_COLOR` property.
        Tier::Mono | Tier::Ascii => Color::Default,
    }
}

/// Map a 24-bit RGB triple onto the nearest xterm-256 index: the 6×6×6 colour
/// cube (16..=231) or the 24-step greyscale ramp (232..=255), whichever is
/// closer. Deterministic and total.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    /// The six cube levels xterm uses on each axis.
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

    // Nearest cube level per channel: `(cube index 0..6, quantized level)`.
    let nearest = |v: u8| -> (u16, u8) {
        let mut best_i = 0u16;
        let mut best_lv = 0u8;
        let mut best_d = u16::MAX;
        for (i, &lv) in LEVELS.iter().enumerate() {
            let d = u16::from(v.abs_diff(lv));
            if d < best_d {
                best_d = d;
                best_i = i as u16;
                best_lv = lv;
            }
        }
        (best_i, best_lv)
    };
    let (ri, rq) = nearest(r);
    let (gi, gq) = nearest(g);
    let (bi, bq) = nearest(b);
    let cube_idx = 16 + 36 * ri + 6 * gi + bi;
    let cube_err = dist2(r, g, b, rq, gq, bq);

    // Nearest greyscale step (232..=255 are grey 8,18,…,238).
    let luma = (u16::from(r) + u16::from(g) + u16::from(b)) / 3;
    let step = (luma.saturating_sub(8) / 10).min(23) as u8;
    let grey = 8 + step * 10;
    let grey_err = dist2(r, g, b, grey, grey, grey);

    if grey_err < cube_err {
        232 + step
    } else {
        cube_idx as u8
    }
}

/// Squared RGB distance (for the cube-vs-greyscale choice).
fn dist2(r: u8, g: u8, b: u8, r2: u8, g2: u8, b2: u8) -> u32 {
    let d = |a: u8, c: u8| -> u32 {
        let x = u32::from(a.abs_diff(c));
        x * x
    };
    d(r, r2) + d(g, g2) + d(b, b2)
}

// ── gamma-correct interpolation (the graph gradients) ───────────────────────

/// The gamma used to linearize sRGB channels before interpolation.
const GAMMA: f32 = 2.2;

/// Linearize one 0–255 sRGB channel to ~linear light.
fn to_linear(c: u8) -> f32 {
    (f32::from(c) / 255.0).powf(GAMMA)
}

/// Convert a linear-light value back to a 0–255 sRGB channel.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_srgb(l: f32) -> u8 {
    (l.clamp(0.0, 1.0).powf(1.0 / GAMMA) * 255.0).round() as u8
}

/// Interpolate one channel in linear light (`t` clamped to `[0, 1]`).
fn lerp_channel(a: u8, b: u8, t: f32) -> u8 {
    let la = to_linear(a);
    let lb = to_linear(b);
    to_srgb(la + (lb - la) * t)
}

/// The RGB triple of a colour, or `None` for `Default`/`Indexed` (which carry no
/// numeric channels to blend).
fn rgb_of(c: Color) -> Option<(u8, u8, u8)> {
    match c {
        Color::Rgb(r, g, b) => Some((r, g, b)),
        Color::Default | Color::Indexed(_) => None,
    }
}

/// Interpolate `a`→`b` at `t∈[0,1]` in linear light. Non-RGB endpoints don't
/// blend: the nearer endpoint is returned (a total, panic-free fallback).
#[must_use]
pub fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    match (rgb_of(a), rgb_of(b)) {
        (Some((ar, ag, ab)), Some((br, bg, bb))) => Color::Rgb(
            lerp_channel(ar, br, t),
            lerp_channel(ag, bg, t),
            lerp_channel(ab, bb, t),
        ),
        _ if t < 0.5 => a,
        _ => b,
    }
}

/// Sample a `(position, colour)` ramp at `x∈[0,1]`. Stops must be sorted by
/// position; `x` before the first / after the last clamps to that stop. Empty
/// ramp ⇒ the terminal default.
#[must_use]
pub fn ramp_color(stops: &[(f32, Color)], x: f32) -> Color {
    let x = if x.is_finite() {
        x.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let Some((first_pos, first_col)) = stops.first().copied() else {
        return Color::Default;
    };
    if x <= first_pos {
        return first_col;
    }
    for pair in stops.windows(2) {
        let [(lo_pos, lo_col), (hi_pos, hi_col)] = pair else {
            continue;
        };
        if x <= *hi_pos {
            let span = hi_pos - lo_pos;
            let t = if span > f32::EPSILON {
                (x - lo_pos) / span
            } else {
                0.0
            };
            return lerp_color(*lo_col, *hi_col, t);
        }
    }
    stops.last().map_or(Color::Default, |&(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn rgb(c: Color) -> (u8, u8, u8) {
        match c {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("expected rgb, got {other:?}"),
        }
    }

    // ── colour foundation (carried from the old palette module) ──────────────

    #[test]
    fn lerp_endpoints_are_exact() {
        let a = Color::Rgb(0, 0, 0);
        let b = Color::Rgb(255, 255, 255);
        assert_eq!(rgb(lerp_color(a, b, 0.0)), (0, 0, 0));
        assert_eq!(rgb(lerp_color(a, b, 1.0)), (255, 255, 255));
    }

    #[test]
    fn lerp_midpoint_is_between() {
        let a = Color::Rgb(0, 0, 0);
        let b = Color::Rgb(200, 0, 0);
        let (r, _, _) = rgb(lerp_color(a, b, 0.5));
        assert!(r > 0 && r < 200, "midpoint channel {r} strictly between");
    }

    #[test]
    fn lerp_clamps_and_survives_non_finite() {
        let a = Color::Rgb(10, 10, 10);
        let b = Color::Rgb(20, 20, 20);
        assert_eq!(rgb(lerp_color(a, b, -1.0)), (10, 10, 10));
        assert_eq!(rgb(lerp_color(a, b, 2.0)), (20, 20, 20));
        assert_eq!(rgb(lerp_color(a, b, f32::NAN)), (10, 10, 10));
    }

    #[test]
    fn non_rgb_endpoint_falls_back_to_nearer() {
        assert_eq!(
            lerp_color(Color::Default, Color::Rgb(1, 2, 3), 0.2),
            Color::Default
        );
        assert_eq!(
            lerp_color(Color::Default, Color::Rgb(1, 2, 3), 0.9),
            Color::Rgb(1, 2, 3)
        );
    }

    #[test]
    fn ramp_endpoints_and_interior() {
        assert_eq!(ramp_color(&GAUGE_RAMP, 0.0), GAUGE_RAMP[0].1);
        assert_eq!(ramp_color(&GAUGE_RAMP, 1.0), GAUGE_RAMP[2].1);
        assert_eq!(ramp_color(&GAUGE_RAMP, -5.0), GAUGE_RAMP[0].1);
        assert_eq!(ramp_color(&GAUGE_RAMP, 5.0), GAUGE_RAMP[2].1);
        let mid = rgb(ramp_color(&GAUGE_RAMP, 0.3));
        assert!(mid.0 > 0x6c, "ramp reddens past the green stop: {mid:?}");
    }

    #[test]
    fn empty_ramp_is_default() {
        assert_eq!(ramp_color(&[], 0.5), Color::Default);
    }

    // ── the class map is single-sourced ──────────────────────────────────────

    #[test]
    fn token_class_map_matches_breakdown() {
        for class in TokenClass::ALL {
            assert_eq!(class.color(), BREAKDOWN[class.index()]);
            assert_eq!(Theme.token_class(class).fg, class.color());
        }
        // Order is the canonical input, read, output, cache-write.
        let colours: Vec<Color> = TokenClass::ALL.iter().map(|c| c.color()).collect();
        assert_eq!(colours, BREAKDOWN.to_vec());
    }

    // ── icons round-trip: every glyph has an ASCII substitution ──────────────

    #[test]
    fn every_icon_has_ascii_and_unicode() {
        const ALL: [Icon; 17] = [
            Icon::Repo,
            Icon::Worktree,
            Icon::StatusRunning,
            Icon::StatusBusy,
            Icon::StatusIdle,
            Icon::StatusUninit,
            Icon::StatusError,
            Icon::Events,
            Icon::Sessions,
            Icon::Checkpoints,
            Icon::Setting,
            Icon::CheckOn,
            Icon::CheckOff,
            Icon::CheckPending,
            Icon::CheckFailed,
            Icon::Bullet,
            Icon::HealthDot,
        ];
        for icon in ALL {
            // The ASCII floor is printable and never tofu / control.
            let a = icon.ascii();
            assert!(
                a.is_ascii() && !a.is_control(),
                "{icon:?} ascii not printable"
            );
            assert_eq!(icon.glyph(false), a);
            assert_eq!(icon.glyph(true), icon.unicode());
        }
    }

    // ── the capability lattice ───────────────────────────────────────────────

    #[test]
    fn tier_from_caps_covers_the_lattice() {
        assert_eq!(Tier::from_caps(Capabilities::empty()), Tier::Ascii);
        assert_eq!(
            Tier::from_caps(Capabilities::TRUECOLOR | Capabilities::UNICODE),
            Tier::Truecolor
        );
        assert_eq!(
            Tier::from_caps(Capabilities::UNICODE),
            Tier::Ansi256,
            "colour-capable but not truecolor"
        );
        assert_eq!(
            Tier::from_caps(Capabilities::LEGACY_WIN),
            Tier::Mono,
            "a non-empty cap set with neither truecolor nor unicode is mono"
        );
    }

    #[test]
    fn glyph_substitution_follows_the_unicode_probe() {
        // Mono cap-sets lack the UNICODE probe, so they substitute like the
        // floor; the colour-capable tiers imply the probe succeeded.
        assert!(!Tier::Ascii.unicode());
        assert!(!Tier::Mono.unicode());
        assert!(Tier::Ansi256.unicode());
        assert!(Tier::Truecolor.unicode());
    }

    #[test]
    fn ansi256_quantizes_known_anchors() {
        // Pure black / white / the cube corners map to their canonical indices.
        assert_eq!(rgb_to_ansi256(0, 0, 0), 16);
        assert_eq!(rgb_to_ansi256(255, 255, 255), 231);
        assert_eq!(rgb_to_ansi256(255, 0, 0), 196);
        assert_eq!(rgb_to_ansi256(0, 255, 0), 46);
        assert_eq!(rgb_to_ansi256(0, 0, 255), 21);
    }

    #[test]
    fn ansi256_greyscale_beats_the_cube_for_mid_grey() {
        // A mid grey should land on the 232..=255 ramp, not a cube corner.
        let idx = rgb_to_ansi256(0x7a, 0x7a, 0x7a);
        assert!(
            (232..=255).contains(&idx),
            "mid grey → greyscale ramp, got {idx}"
        );
    }

    proptest! {
        /// Theme resolution is **total** over the capability lattice: every
        /// themed style resolves at every tier without panic, and the mono/ASCII
        /// floors carry no colour by construction (the `NO_COLOR` property).
        #[test]
        fn resolution_is_total_and_floors_are_colourless(
            fr in 0u8..=255, fg in 0u8..=255, fb in 0u8..=255,
            br in 0u8..=255, bg in 0u8..=255, bb in 0u8..=255,
            bold in any::<bool>(),
        ) {
            let mods = if bold { Mods::BOLD } else { Mods::NONE };
            let style = Style {
                fg: Color::Rgb(fr, fg, fb),
                bg: Color::Rgb(br, bg, bb),
                mods,
            };
            for tier in [Tier::Truecolor, Tier::Ansi256, Tier::Mono, Tier::Ascii] {
                let r = resolve_style(style, tier);
                // Attributes always survive.
                prop_assert_eq!(r.mods, mods);
                match tier {
                    Tier::Truecolor => {
                        prop_assert_eq!(r.fg, style.fg);
                        prop_assert_eq!(r.bg, style.bg);
                    }
                    Tier::Ansi256 => {
                        prop_assert!(matches!(r.fg, Color::Indexed(_)));
                        prop_assert!(matches!(r.bg, Color::Indexed(_)));
                    }
                    Tier::Mono | Tier::Ascii => {
                        // No colour anywhere — the structural guarantee.
                        prop_assert_eq!(r.fg, Color::Default);
                        prop_assert_eq!(r.bg, Color::Default);
                    }
                }
            }
        }

        /// `NO_COLOR` (the empty cap set → `Tier::Ascii`) yields no colour for any
        /// themed section style.
        #[test]
        fn no_color_yields_no_colour_anywhere(seed in 0usize..13) {
            const SECTIONS: [Section; 13] = [
                Section::Projects, Section::Repository, Section::Models,
                Section::Settings, Section::Capture, Section::Features,
                Section::Activity, Section::TokenUsage, Section::System,
                Section::Recall, Section::Header, Section::Hint, Section::Command,
            ];
            let tier = Tier::from_caps(Capabilities::empty()); // forced ASCII
            let section = SECTIONS[seed % SECTIONS.len()];
            for style in [
                Theme.title(section),
                Theme.border(section, true),
                Theme.border(section, false),
                Theme.selection(),
                Theme.hover(),
                Theme.badge(section.accent()),
            ] {
                let r = resolve_style(style, tier);
                prop_assert_eq!(r.fg, Color::Default);
                prop_assert_eq!(r.bg, Color::Default);
            }
        }

        /// The cube index is always in the 256 range for any RGB input.
        #[test]
        fn ansi256_is_always_in_range(r in 0u8..=255, g in 0u8..=255, b in 0u8..=255) {
            let idx = rgb_to_ansi256(r, g, b);
            prop_assert!(idx >= 16, "0..15 are the system colours, never emitted");
        }
    }
}
