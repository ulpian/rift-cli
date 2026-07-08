//! Column 2 — the card stack (M55-08/09, ADR-0024 §3).
//!
//! The M4 detail list gives way to individually-bordered **cards** — repository,
//! models, settings, capture, features, activity — sized by the shared
//! [`crate::stack`] solver (weight, min-rows, priority, collapse-to-summary: a
//! card degrades to its headline, never disappears). All colour lives in the
//! `Frame` through the surface theme; the renderer is pure over the
//! [`DetailView`] + the activity feed, and the toggle cursor and the pure
//! hit-test map through the card geometry (recomputed, never stored).
//!
//! Every projection-derived string (label, path, model name, feature label)
//! passes the shared sanitizer before it can reach a cell, so untrusted capture
//! data can never inject a terminal sequence (`05-security`).

use std::collections::BTreeMap;

use crate::col2::sanitize_for_terminal;
use crate::col2::{DetailView, Feature, ToggleState};
use crate::col3::ActivityView;
use crate::layout::Rect;
use crate::plot::{self, Paint};
use crate::render::frame::{Color, Frame, Style};
use crate::render::RenderBackend;
use crate::stack::{self, Priority, Spec};
use crate::theme::{Icon, Section, Theme};
use crate::{EntryKind, FeatureRuntime, ProjectStatus};

/// One card in the column-2 stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardKind {
    /// Name, kind, path, status, and the *Open in $EDITOR* button.
    Repository,
    /// Calls-ranked models with count pills + an honest `+N more`.
    Models,
    /// The effective-policy summary (counts + booleans, never globs).
    Settings,
    /// Icon tiles for events / sessions / checkpoints.
    Capture,
    /// The feature toggles as checkbox lifecycle rows.
    Features,
    /// Grouped daily bars of events/sessions/checkpoints (trailing week).
    Activity,
}

/// The card registration table — the fixed display order and each card's stack
/// sizing (M55-08 decisions): the repository and activity cards hold out longest;
/// settings and models fold first.
const CARDS: [(CardKind, Spec); 6] = [
    (
        CardKind::Repository,
        Spec {
            weight: 2,
            min_rows: 5,
            priority: Priority::High,
        },
    ),
    (
        CardKind::Models,
        Spec {
            weight: 2,
            min_rows: 4,
            priority: Priority::Low,
        },
    ),
    (
        CardKind::Settings,
        Spec {
            weight: 1,
            min_rows: 4,
            priority: Priority::Low,
        },
    ),
    (
        CardKind::Capture,
        Spec {
            weight: 2,
            min_rows: 5,
            priority: Priority::Medium,
        },
    ),
    (
        CardKind::Features,
        Spec {
            weight: 2,
            min_rows: 5,
            priority: Priority::High,
        },
    ),
    (
        CardKind::Activity,
        Spec {
            weight: 3,
            min_rows: 6,
            priority: Priority::High,
        },
    ),
];

/// One solved card slot: which card, its rect within the column, and whether it
/// collapsed to its headline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardSlot {
    /// Which card.
    pub kind: CardKind,
    /// The card's rect (already offset into the column area).
    pub rect: Rect,
    /// Collapsed to a one-line headline.
    pub collapsed: bool,
}

/// Solve the card stack over `area`, returning each card's rect. Pure over the
/// area height — the same solve the renderer and the hit-test share, so a click
/// always resolves to the card that was drawn.
#[must_use]
pub fn solve(area: Rect) -> Vec<CardSlot> {
    let specs: Vec<Spec> = CARDS.iter().map(|(_, s)| *s).collect();
    stack::solve(&specs, area.h)
        .into_iter()
        .filter_map(|slot| {
            CARDS.get(slot.index).map(|(kind, _)| CardSlot {
                kind: *kind,
                rect: Rect {
                    x: area.x,
                    y: area.y + slot.y,
                    w: area.w,
                    h: slot.rows,
                },
                collapsed: slot.collapsed,
            })
        })
        .collect()
}

/// Everything a card render (and the hit-test) reads — bundled so the pure
/// renderer never touches `AppState`.
pub struct CardCtx<'a> {
    /// The selected project's detail.
    pub detail: &'a DetailView,
    /// The selected project's activity-card data.
    pub activity: &'a ActivityView,
    /// Feature-row runtime overlay (Enabling/Disabling/Failed).
    pub runtime: &'a BTreeMap<String, FeatureRuntime>,
    /// The column-2 toggle cursor (which feature row is highlighted).
    pub toggle_cursor: usize,
    /// Whether the detail column is focused (drives the cursor highlight).
    pub focused: bool,
    /// Whether Unicode glyphs are available (icon substitution).
    pub unicode: bool,
    /// The button whose press-flash is in flight (ADR-0028), if any.
    pub pressed: Option<crate::hit::Target>,
    /// The press-flash progress (1.0 → 0.0) keyed by [`Self::pressed`].
    pub press_pulse: f32,
}

impl CardCtx<'_> {
    /// The press-flash fraction (0.0 at rest) for a card button drawn as
    /// `target` (ADR-0028) — the shared "am I flashing?" for editor/toggle/retry.
    #[must_use]
    fn flash(&self, target: &crate::hit::Target) -> f32 {
        if self.pressed.as_ref() == Some(target) {
            self.press_pulse
        } else {
            0.0
        }
    }
}

/// The key hint for the repository card's *Open in $EDITOR* button.
pub const EDITOR_KEY: &str = "e";
/// The button label.
pub const EDITOR_LABEL: &str = "Open in $EDITOR";

/// Draw the card stack into `area`. An uninitialized project shows only the
/// repository card (with the init hint) filling the column — mirroring the M4
/// early-out, so the stack never invents data a fresh project doesn't have.
pub fn draw(
    ctx: &CardCtx<'_>,
    theme: Theme,
    area: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    if matches!(ctx.detail.status, ProjectStatus::NotInitialized) {
        draw_repository(ctx, theme, area, false, frame);
        return;
    }
    for slot in solve(area) {
        if slot.rect.h == 0 || slot.rect.w == 0 {
            continue;
        }
        match slot.kind {
            CardKind::Repository => draw_repository(ctx, theme, slot.rect, slot.collapsed, frame),
            CardKind::Models => draw_models(ctx, theme, slot.rect, slot.collapsed, frame),
            CardKind::Settings => draw_settings(ctx, theme, slot.rect, slot.collapsed, frame),
            CardKind::Capture => draw_capture(ctx, theme, slot.rect, slot.collapsed, frame),
            CardKind::Features => draw_features(ctx, theme, slot.rect, slot.collapsed, frame),
            CardKind::Activity => {
                draw_activity(ctx, theme, slot.rect, slot.collapsed, backend, frame);
            }
        }
    }
}

/// Draw a card border + title, returning the inner content rect.
fn card_frame(theme: Theme, section: Section, title: &str, rect: Rect, frame: &mut Frame) -> Rect {
    crate::chrome::panel(
        frame,
        rect,
        title,
        theme.border(section, false),
        theme.title(section),
    )
}

/// Print `text` on row `dy` of `inner`, clipped, in `style` (dropped if below).
fn line(frame: &mut Frame, inner: Rect, dy: u16, text: &str, style: Style) {
    if dy >= inner.h {
        return;
    }
    let clipped: String = text.chars().take(usize::from(inner.w)).collect();
    frame.put_str(inner.x, inner.y + dy, &clipped, style);
}

// --- the repository card -----------------------------------------------------

/// The repository card's *Open in $EDITOR* button rect within its inner area,
/// or `None` when the card is collapsed / too short to host it. Shared with the
/// hit-test (M55-10) so the click resolves exactly where the button draws.
#[must_use]
pub fn editor_button_rect(card_rect: Rect, collapsed: bool) -> Option<Rect> {
    if collapsed {
        return None;
    }
    let inner = inner_of(card_rect);
    if inner.h < 4 || inner.w < 6 {
        return None;
    }
    // The button sits on the last inner row.
    let label = format!("[ {EDITOR_LABEL} {EDITOR_KEY} ]");
    let bw = u16::try_from(label.chars().count())
        .unwrap_or(0)
        .min(inner.w);
    Some(Rect {
        x: inner.x,
        y: inner.y + inner.h - 1,
        w: bw,
        h: 1,
    })
}

fn draw_repository(
    ctx: &CardCtx<'_>,
    theme: Theme,
    rect: Rect,
    collapsed: bool,
    frame: &mut Frame,
) {
    let d = ctx.detail;
    let kind_word = match d.kind {
        EntryKind::Repo => "repo",
        EntryKind::Worktree => "worktree",
    };
    let inner = card_frame(theme, Section::Repository, "repository", rect, frame);
    if collapsed {
        line(
            frame,
            inner,
            0,
            &format!(
                "{} · {}",
                sanitize_for_terminal(&d.label),
                status_word(d.status)
            ),
            theme.body(),
        );
        return;
    }
    line(
        frame,
        inner,
        0,
        &format!("{} ({kind_word})", sanitize_for_terminal(&d.label)),
        theme.body(),
    );
    line(
        frame,
        inner,
        1,
        &format!("path: {}", sanitize_for_terminal(&d.path)),
        theme.dim(),
    );
    // Status with its coloured dot.
    let dot = Icon::for_status(d.status).glyph(ctx.unicode);
    frame.set(inner.x, inner.y + 2, dot, theme.status_dot(d.status));
    line(
        frame,
        Rect {
            x: inner.x + 2,
            ..inner
        },
        2,
        status_word(d.status),
        theme.body(),
    );
    if matches!(d.status, ProjectStatus::NotInitialized) {
        line(
            frame,
            inner,
            3,
            "not initialized — run `rift init`",
            theme.dim(),
        );
        return;
    }
    // The Open in $EDITOR button on the last inner row (action lands M55-10).
    if let Some(br) = editor_button_rect(rect, collapsed) {
        // The button flashes toward the accent on click/activation (ADR-0028).
        let base = theme.button(ctx.focused);
        let style =
            match crate::chrome::press_flash_bg(ctx.flash(&crate::hit::Target::EditorButton)) {
                Some(bg) => base.with_bg(bg),
                None => base,
            };
        crate::chrome::button(
            frame,
            br.x,
            br.y,
            EDITOR_LABEL,
            EDITOR_KEY,
            style,
            theme.key_hint(),
        );
    }
}

// --- the models card ---------------------------------------------------------

fn draw_models(ctx: &CardCtx<'_>, theme: Theme, rect: Rect, collapsed: bool, frame: &mut Frame) {
    let models = &ctx.detail.models;
    let inner = card_frame(theme, Section::Models, "models", rect, frame);
    if models.is_empty() {
        line(frame, inner, 0, "(none yet)", theme.dim());
        return;
    }
    if collapsed {
        line(
            frame,
            inner,
            0,
            &format!("{} models", models.len()),
            theme.body(),
        );
        return;
    }
    // Calls-ranked (already sorted most-used first by the feed); show as many as
    // fit, with an honest `+N more`.
    let visible = usize::from(inner.h);
    let show = models
        .len()
        .min(visible.saturating_sub(usize::from(models.len() > visible)));
    for (i, m) in models.iter().take(show).enumerate() {
        let Ok(dy) = u16::try_from(i) else { break };
        let name = sanitize_for_terminal(&m.model);
        // `model  ×N` — the count as a pill on the right.
        let pill = format!("×{}", m.calls);
        let name_w = usize::from(inner.w).saturating_sub(pill.chars().count() + 1);
        let clipped: String = name.chars().take(name_w).collect();
        line(frame, inner, dy, &clipped, theme.body());
        let px = inner.x + inner.w - u16::try_from(pill.chars().count()).unwrap_or(0);
        frame.put_str(
            px,
            inner.y + dy,
            &pill,
            theme.tile_value(Section::Models.accent()),
        );
    }
    let more = models.len().saturating_sub(show);
    if more > 0 {
        if let Ok(dy) = u16::try_from(show) {
            line(frame, inner, dy, &format!("+{more} more"), theme.dim());
        }
    }
}

// --- the settings card -------------------------------------------------------

fn draw_settings(ctx: &CardCtx<'_>, theme: Theme, rect: Rect, collapsed: bool, frame: &mut Frame) {
    let s = &ctx.detail.settings;
    let inner = card_frame(theme, Section::Settings, "settings", rect, frame);
    if collapsed {
        line(
            frame,
            inner,
            0,
            &format!("{} sensitive paths", s.sensitive_path_count),
            theme.body(),
        );
        return;
    }
    let dim = theme.dim();
    line(
        frame,
        inner,
        0,
        &format!("sensitive paths: {}", s.sensitive_path_count),
        theme.body(),
    );
    line(
        frame,
        inner,
        1,
        &format!("raw transcripts: {}", on_off(s.raw_transcripts)),
        dim,
    );
    line(
        frame,
        inner,
        2,
        &format!("tool calls: {}", cap_skip(s.captures_tool_calls)),
        dim,
    );
    line(
        frame,
        inner,
        3,
        &format!("token usage: {}", cap_skip(s.captures_token_usage)),
        dim,
    );
}

// --- the capture card (icon tiles) -------------------------------------------

fn draw_capture(ctx: &CardCtx<'_>, theme: Theme, rect: Rect, collapsed: bool, frame: &mut Frame) {
    let c = &ctx.detail.capture;
    let inner = card_frame(theme, Section::Capture, "capture", rect, frame);
    if collapsed {
        line(
            frame,
            inner,
            0,
            &format!("{} ev · {} se · {} ck", c.events, c.sessions, c.checkpoints),
            theme.body(),
        );
        return;
    }
    let accent = Section::Capture.accent();
    let tiles = [
        (Icon::Events, "events", c.events),
        (Icon::Sessions, "sessions", c.sessions),
        (Icon::Checkpoints, "checks", c.checkpoints),
    ];
    // Three tiles side by side when there's room; else stacked headlines.
    let tw = inner.w / 3;
    if inner.h >= 3 && tw >= 5 {
        for (i, (ic, label, val)) in tiles.into_iter().enumerate() {
            let Ok(i16) = u16::try_from(i) else { break };
            let title = format!("{} {label}", ic.glyph(ctx.unicode));
            crate::chrome::tile(
                frame,
                Rect {
                    x: inner.x + i16 * tw,
                    y: inner.y,
                    w: tw,
                    h: inner.h.min(3),
                },
                &title,
                &crate::col3::fmt_km(as_f64(val)),
                theme.tile_border(accent),
                theme.dim(),
                theme.tile_value(accent),
            );
        }
    } else {
        for (i, (ic, label, val)) in tiles.into_iter().enumerate() {
            let Ok(dy) = u16::try_from(i) else { break };
            crate::chrome::tile_headline(
                frame,
                inner.x,
                inner.y + dy,
                inner.w,
                &format!("{} {label}", ic.glyph(ctx.unicode)),
                &val.to_string(),
                theme.dim(),
                theme.tile_value(accent),
            );
        }
    }
}

// --- the features card (checkbox lifecycle rows) -----------------------------

/// The number of feature toggle rows the features card renders for `ctx`.
fn feature_row_count(ctx: &CardCtx<'_>) -> usize {
    ctx.detail.toggles.rows.len()
}

/// The toggle-row index at `(col, row)` inside the features card, or `None`.
/// Shared with the hit-test so a click flips the exact row drawn.
#[must_use]
pub fn feature_toggle_at(
    ctx: &CardCtx<'_>,
    card_rect: Rect,
    collapsed: bool,
    col: u16,
    row: u16,
) -> Option<usize> {
    if collapsed {
        return None;
    }
    let inner = inner_of(card_rect);
    if col < inner.x || col >= inner.x + inner.w || row < inner.y {
        return None;
    }
    let dy = usize::from(row - inner.y);
    if dy < feature_row_count(ctx) && dy < usize::from(inner.h) {
        Some(dy)
    } else {
        None
    }
}

fn draw_features(ctx: &CardCtx<'_>, theme: Theme, rect: Rect, collapsed: bool, frame: &mut Frame) {
    let d = ctx.detail;
    let inner = card_frame(theme, Section::Features, "features", rect, frame);
    let on = d.toggles.rows.iter().filter(|r| r.state.is_on()).count();
    if collapsed || d.toggles.rows.is_empty() {
        let text = if d.toggles.rows.is_empty() {
            "(no features)".to_string()
        } else {
            format!("{on}/{} on", d.toggles.rows.len())
        };
        line(frame, inner, 0, &text, theme.body());
        return;
    }
    for (i, row) in d.toggles.rows.iter().enumerate() {
        let Ok(dy) = u16::try_from(i) else { break };
        if dy >= inner.h {
            break;
        }
        let cursored = ctx.focused && i == ctx.toggle_cursor;
        let marker = if cursored { '>' } else { ' ' };
        let indent = if row.indent { "  " } else { "" };
        let effective = effective_state(row, ctx.runtime);
        let mut text = format!(
            "{marker} {indent}{} {}",
            effective.glyph(),
            row.feature.label()
        );
        if staged_feature(row.feature) && matches!(effective, ToggleState::On) {
            text.push_str(" · M7");
        }
        if let ToggleState::Failed(reason) = &effective {
            text.push_str(" — ");
            text.push_str(&sanitize_for_terminal(reason));
        }
        // The row flashes toward the accent when its toggle is clicked
        // (ADR-0028); otherwise the cursored row keeps the selection wash.
        let flash = ctx.flash(&crate::hit::Target::ToggleRow(i));
        let (style, fill_row) = if let Some(bg) = crate::chrome::press_flash_bg(flash) {
            (theme.selection().with_bg(bg), true)
        } else if cursored {
            (theme.selection(), true)
        } else {
            (theme.body(), false)
        };
        if fill_row {
            crate::chrome::fill(
                frame,
                Rect {
                    x: inner.x,
                    y: inner.y + dy,
                    w: inner.w,
                    h: 1,
                },
                style,
            );
        }
        line(frame, inner, dy, &text, style);
    }
}

// --- the activity card (grouped daily bars) — M55-09 -------------------------

/// The three activity categories' colours (events, sessions, checkpoints),
/// sourced from the theme so the legend and bars match.
fn activity_cats() -> [(Icon, &'static str, Color); 3] {
    [
        (Icon::Events, "events", Section::Capture.accent()),
        (Icon::Sessions, "sess", Section::Repository.accent()),
        (Icon::Checkpoints, "checks", Section::Activity.accent()),
    ]
}

fn draw_activity(
    ctx: &CardCtx<'_>,
    theme: Theme,
    rect: Rect,
    collapsed: bool,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let inner = card_frame(theme, Section::Activity, "activity · 7d", rect, frame);
    let days = match ctx.activity {
        ActivityView::NoProjection => {
            line(frame, inner, 0, "(no projection yet)", theme.dim());
            return;
        }
        ActivityView::Days(d) => &d.days,
    };
    if collapsed {
        let today = days.last().cloned().unwrap_or_default();
        line(
            frame,
            inner,
            0,
            &format!(
                "today {} ev · {} se · {} ck",
                today.events, today.sessions, today.checkpoints
            ),
            theme.body(),
        );
        return;
    }
    // The legend on the first inner row.
    let mut lx = inner.x;
    for (ic, label, colour) in activity_cats() {
        frame.set(lx, inner.y, ic.glyph(ctx.unicode), Style::fg(colour));
        lx = frame.put_str(lx + 1, inner.y, &format!(" {label}  "), theme.dim());
        if lx >= inner.x + inner.w {
            break;
        }
    }
    // The grouped bars fill the middle; the day labels the last row.
    if inner.h < 3 {
        return;
    }
    let plot_rect = Rect {
        x: inner.x,
        y: inner.y + 1,
        w: inner.w,
        h: inner.h - 2,
    };
    let groups: Vec<Vec<f64>> = days
        .iter()
        .map(|b| vec![as_f64(b.events), as_f64(b.sessions), as_f64(b.checkpoints)])
        .collect();
    let max = groups
        .iter()
        .flat_map(|g| g.iter().copied())
        .fold(0.0f64, f64::max)
        .max(1.0);
    let levels = plot_rect.h.saturating_mul(4).max(4);
    let per_cat = plot::grouped_bar_points(&groups, 3, max, levels);
    let cats = activity_cats();
    for (c, pts) in per_cat.iter().enumerate() {
        let colour = cats.get(c).map_or(Color::Default, |(_, _, col)| *col);
        backend.plot(pts, plot_rect, &Paint::Solid(Style::fg(colour)), frame);
    }
    // Day labels (first letter of each weekday) on the last inner row.
    let label_row = inner.y + inner.h - 1;
    let stride = inner.w / u16::try_from(days.len().max(1)).unwrap_or(1);
    for (i, b) in days.iter().enumerate() {
        let Ok(i16) = u16::try_from(i) else { break };
        let x = inner.x + i16 * stride;
        if x < inner.x + inner.w {
            frame.set(
                x,
                label_row,
                b.weekday.chars().next().unwrap_or('?'),
                theme.dim(),
            );
        }
    }
}

// --- shared helpers ----------------------------------------------------------

/// The inner content rect of a bordered card (mirrors `chrome::panel`).
fn inner_of(r: Rect) -> Rect {
    if r.w < 2 || r.h < 2 {
        return r;
    }
    Rect {
        x: r.x + 1,
        y: r.y + 1,
        w: r.w - 2,
        h: r.h - 2,
    }
}

/// The effective toggle state: the runtime overlay (last action RESULT) wins
/// over the persisted desired state (M5-12 logic, carried into the card).
fn effective_state(
    row: &crate::col2::ToggleRow,
    runtime: &BTreeMap<String, FeatureRuntime>,
) -> ToggleState {
    match runtime.get(row.feature.key()) {
        Some(FeatureRuntime::Enabling) => ToggleState::Enabling,
        Some(FeatureRuntime::Disabling) => ToggleState::Disabling,
        Some(FeatureRuntime::Failed(reason)) => ToggleState::Failed(reason.clone()),
        None => row.state.clone(),
    }
}

/// Whether a feature's engine is staged for M7 (fff/codegraph + variants).
fn staged_feature(f: Feature) -> bool {
    matches!(
        f,
        Feature::Fff
            | Feature::Codegraph
            | Feature::CodegraphTs
            | Feature::CodegraphPy
            | Feature::CodegraphRs
    )
}

/// A short human label for a project's live status.
fn status_word(status: ProjectStatus) -> &'static str {
    match status {
        ProjectStatus::Running { .. } => "running",
        ProjectStatus::PickingUp => "picking up",
        ProjectStatus::Idle => "idle",
        ProjectStatus::NotInitialized => "not initialized",
        ProjectStatus::Error(_) => "error",
    }
}

fn on_off(b: bool) -> &'static str {
    if b {
        "on"
    } else {
        "off"
    }
}

fn cap_skip(b: bool) -> &'static str {
    if b {
        "captured"
    } else {
        "skipped"
    }
}

#[allow(clippy::cast_precision_loss)]
fn as_f64(v: u64) -> f64 {
    v as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::col2::{CaptureSummary, ModelUsage, SettingsSummary, ToggleRow, ToggleView};
    use crate::col3::{ActivityData, DayBar};
    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities};

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    fn detail(status: ProjectStatus) -> DetailView {
        DetailView {
            path: "/r/proj".into(),
            label: "proj".into(),
            kind: EntryKind::Repo,
            status,
            models: vec![
                ModelUsage {
                    model: "claude-opus".into(),
                    calls: 12,
                },
                ModelUsage {
                    model: "claude-haiku".into(),
                    calls: 3,
                },
            ],
            settings: SettingsSummary::default(),
            capture: CaptureSummary {
                events: 128,
                sessions: 6,
                checkpoints: 12,
                last_checkpoint_at_ms: Some(1),
            },
            toggles: ToggleView {
                rows: Feature::ALL
                    .iter()
                    .map(|f| ToggleRow {
                        feature: *f,
                        state: ToggleState::Off,
                        indent: false,
                    })
                    .collect(),
            },
        }
    }

    fn render(ctx: &CardCtx<'_>, area: Rect) -> (Frame, String) {
        let mut f = Frame::new(Size::new(area.x + area.w, area.y + area.h));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw(ctx, Theme::rift_dark(), area, &mut b, &mut f);
        let text: String = (0..f.height())
            .map(|y| {
                (0..f.width())
                    .map(|x| f.get(x, y).map_or(' ', |c| c.ch))
                    .collect::<String>()
                    + "\n"
            })
            .collect();
        (f, text)
    }

    fn ctx<'a>(
        d: &'a DetailView,
        act: &'a ActivityView,
        rt: &'a BTreeMap<String, FeatureRuntime>,
    ) -> CardCtx<'a> {
        CardCtx {
            detail: d,
            activity: act,
            runtime: rt,
            toggle_cursor: 0,
            focused: true,
            unicode: false,
            pressed: None,
            press_pulse: 0.0,
        }
    }

    #[test]
    fn solver_gives_every_card_at_least_its_headline_and_never_disappears() {
        // Under pressure every card still occupies a slot (collapse, not drop),
        // for a tall and a short column.
        for h in [40u16, 12] {
            let slots = solve(rect(0, 0, 40, h));
            assert_eq!(slots.len(), 6, "all six cards present at h={h}");
            let kinds: Vec<CardKind> = slots.iter().map(|s| s.kind).collect();
            assert_eq!(kinds[0], CardKind::Repository);
            assert_eq!(kinds[5], CardKind::Activity);
            // Contiguous, in-bounds.
            let mut y = 0;
            for s in &slots {
                assert_eq!(s.rect.y, y);
                y += s.rect.h;
            }
            assert!(y <= h);
        }
    }

    #[test]
    fn tall_stack_expands_and_renders_every_card_title() {
        let d = detail(ProjectStatus::Idle);
        let act = ActivityView::NoProjection;
        let rt = BTreeMap::new();
        let (_, text) = render(&ctx(&d, &act, &rt), rect(0, 0, 40, 60));
        for title in [
            "repository",
            "models",
            "settings",
            "capture",
            "features",
            "activity",
        ] {
            assert!(
                text.contains(title),
                "the {title} card renders its title: \n{text}"
            );
        }
        // The models card ranks and pills; the capture card tiles the counts.
        assert!(text.contains("claude-opus"));
        assert!(text.contains("×12"));
        assert!(text.contains("128"), "the capture events count tiles");
        // The repository card carries the Open in $EDITOR button.
        assert!(text.contains("Open in $EDITOR"));
    }

    #[test]
    fn uninitialized_shows_only_repository_with_the_init_hint() {
        let d = detail(ProjectStatus::NotInitialized);
        let act = ActivityView::NoProjection;
        let rt = BTreeMap::new();
        let (_, text) = render(&ctx(&d, &act, &rt), rect(0, 0, 40, 60));
        assert!(text.contains("repository"));
        assert!(text.contains("rift init"));
        // The data cards are suppressed for an uninitialized project.
        assert!(!text.contains("models"));
        assert!(!text.contains("capture"));
    }

    #[test]
    fn models_more_is_honest() {
        let mut d = detail(ProjectStatus::Idle);
        d.models = (0..10u64)
            .map(|i| ModelUsage {
                model: format!("m{i}"),
                calls: 10 - i,
            })
            .collect();
        let act = ActivityView::NoProjection;
        let rt = BTreeMap::new();
        // A short models card can't show all ten → an honest `+N more`.
        let slots = solve(rect(0, 0, 40, 40));
        let models_slot = slots.iter().find(|s| s.kind == CardKind::Models).unwrap();
        let mut f = Frame::new(Size::new(40, 40));
        draw_models(
            &ctx(&d, &act, &rt),
            Theme::rift_dark(),
            models_slot.rect,
            false,
            &mut f,
        );
        let text: String = (0..f.height())
            .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
            .map(|(x, y)| f.get(x, y).map_or(' ', |c| c.ch))
            .collect();
        assert!(text.contains("more"), "the tail is counted: {text}");
    }

    #[test]
    fn feature_toggle_hit_maps_to_the_drawn_row() {
        let d = detail(ProjectStatus::Idle);
        let act = ActivityView::NoProjection;
        let rt = BTreeMap::new();
        let c = ctx(&d, &act, &rt);
        let slots = solve(rect(0, 0, 40, 60));
        let feat = slots.iter().find(|s| s.kind == CardKind::Features).unwrap();
        // The features card inner starts at rect.y+1 (border); row i = toggle i.
        let inner_y = feat.rect.y + 1;
        assert_eq!(
            feature_toggle_at(&c, feat.rect, false, feat.rect.x + 2, inner_y),
            Some(0)
        );
        assert_eq!(
            feature_toggle_at(&c, feat.rect, false, feat.rect.x + 2, inner_y + 1),
            Some(1)
        );
        // Collapsed → no toggle hit.
        assert_eq!(
            feature_toggle_at(&c, feat.rect, true, feat.rect.x + 2, inner_y),
            None
        );
    }

    #[test]
    fn editor_button_rect_matches_the_drawn_button() {
        let d = detail(ProjectStatus::Idle);
        let act = ActivityView::NoProjection;
        let rt = BTreeMap::new();
        let slots = solve(rect(0, 0, 40, 60));
        let repo = slots
            .iter()
            .find(|s| s.kind == CardKind::Repository)
            .unwrap();
        let br = editor_button_rect(repo.rect, false).expect("expanded repo card has a button");
        // Render and confirm the button glyph '[' sits at the button rect origin.
        let mut f = Frame::new(Size::new(40, 60));
        draw_repository(
            &ctx(&d, &act, &rt),
            Theme::rift_dark(),
            repo.rect,
            false,
            &mut f,
        );
        assert_eq!(f.get(br.x, br.y).unwrap().ch, '[');
        // Collapsed cards have no button.
        assert_eq!(editor_button_rect(repo.rect, true), None);
    }

    #[test]
    fn a_pressed_editor_button_flashes_toward_the_accent() {
        let d = detail(ProjectStatus::Idle);
        let act = ActivityView::NoProjection;
        let rt = BTreeMap::new();
        let slots = solve(rect(0, 0, 40, 60));
        let repo = slots
            .iter()
            .find(|s| s.kind == CardKind::Repository)
            .unwrap();
        let br = editor_button_rect(repo.rect, false).unwrap();
        let ctx = CardCtx {
            detail: &d,
            activity: &act,
            runtime: &rt,
            toggle_cursor: 0,
            focused: true,
            unicode: false,
            pressed: Some(crate::hit::Target::EditorButton),
            press_pulse: 1.0,
        };
        let mut f = Frame::new(Size::new(40, 60));
        draw_repository(&ctx, Theme::rift_dark(), repo.rect, false, &mut f);
        let flash_bg = crate::chrome::press_flash_bg(1.0).unwrap();
        assert_eq!(
            f.get(br.x, br.y).unwrap().style.bg,
            flash_bg,
            "the pressed editor button flashes"
        );
    }

    #[test]
    fn a_pressed_toggle_row_flashes() {
        let d = detail(ProjectStatus::Idle);
        let act = ActivityView::NoProjection;
        let rt = BTreeMap::new();
        let slots = solve(rect(0, 0, 40, 60));
        let feat = slots
            .iter()
            .find(|s| s.kind == CardKind::Features)
            .expect("features card present");
        let flash_bg = crate::chrome::press_flash_bg(1.0).unwrap();
        let has_flash = |pressed: Option<crate::hit::Target>| {
            let ctx = CardCtx {
                detail: &d,
                activity: &act,
                runtime: &rt,
                toggle_cursor: 0,
                focused: true,
                unicode: false,
                pressed,
                press_pulse: 1.0,
            };
            let mut f = Frame::new(Size::new(40, 60));
            draw_features(&ctx, Theme::rift_dark(), feat.rect, false, &mut f);
            (feat.rect.y..feat.rect.y + feat.rect.h).any(|y| {
                (feat.rect.x..feat.rect.x + feat.rect.w)
                    .any(|x| f.get(x, y).is_some_and(|c| c.style.bg == flash_bg))
            })
        };
        assert!(
            has_flash(Some(crate::hit::Target::ToggleRow(0))),
            "a pressed toggle row flashes"
        );
        assert!(
            !has_flash(None),
            "no flash bg at rest (only the cursored selection wash)"
        );
    }

    #[test]
    fn activity_card_renders_bars_legend_and_placeholder() {
        let d = detail(ProjectStatus::Idle);
        let rt = BTreeMap::new();
        // Missing projection → the honest placeholder.
        let placeholder = ActivityView::NoProjection;
        let mut f = Frame::new(Size::new(44, 12));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_activity(
            &ctx(&d, &placeholder, &rt),
            Theme::rift_dark(),
            rect(0, 0, 44, 12),
            false,
            &mut b,
            &mut f,
        );
        let text: String = (0..f.height())
            .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
            .map(|(x, y)| f.get(x, y).map_or(' ', |c| c.ch))
            .collect();
        assert!(text.contains("no projection"));

        // A dense week draws the grouped bars + the legend.
        let days = ActivityView::Days(ActivityData {
            days: (0..7u64)
                .map(|i| DayBar {
                    weekday: "Mon",
                    events: i * 3,
                    sessions: i,
                    checkpoints: i / 2,
                })
                .collect(),
        });
        let mut f2 = Frame::new(Size::new(44, 12));
        let mut b2 = AsciiBackend::new(Capabilities::empty());
        draw_activity(
            &ctx(&d, &days, &rt),
            Theme::rift_dark(),
            rect(0, 0, 44, 12),
            false,
            &mut b2,
            &mut f2,
        );
        let mut bar_cells = 0;
        for y in 0..f2.height() {
            for x in 0..f2.width() {
                if let Some(c) = f2.get(x, y) {
                    if matches!(c.ch, '▀' | '▄' | '█') {
                        bar_cells += 1;
                    }
                }
            }
        }
        assert!(bar_cells > 0, "the grouped daily bars draw");
    }

    #[test]
    fn card_strings_are_sanitized() {
        // A hostile model name / label never reaches a cell as a control char.
        let mut d = detail(ProjectStatus::Idle);
        d.label = "evil\x1b[31mrepo".into();
        d.models[0].model = "bad\x1b[0mmodel".into();
        let act = ActivityView::NoProjection;
        let rt = BTreeMap::new();
        let (f, _) = render(&ctx(&d, &act, &rt), rect(0, 0, 40, 60));
        for y in 0..f.height() {
            for x in 0..f.width() {
                if let Some(c) = f.get(x, y) {
                    assert!(!c.ch.is_control(), "control char at ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn draw_is_bounds_safe_at_any_size() {
        let d = detail(ProjectStatus::Idle);
        let act = ActivityView::Days(ActivityData::default());
        let rt = BTreeMap::new();
        for (w, h) in [(0, 0), (1, 1), (4, 3), (40, 2), (20, 60)] {
            let mut f = Frame::new(Size::new(w, h));
            let mut b = AsciiBackend::new(Capabilities::empty());
            draw(
                &ctx(&d, &act, &rt),
                Theme::rift_dark(),
                rect(0, 0, w, h),
                &mut b,
                &mut f,
            );
        }
    }
}
