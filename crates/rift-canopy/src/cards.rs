//! Column 2 — the card stack (the dashboard information architecture).
//!
//! The earlier detail list gave way to individually-bordered **cards** — repository,
//! models, and activity — sized by the shared
//! [`crate::stack`] solver (weight, min-rows, priority, collapse-to-summary: a
//! card degrades to its headline, never disappears). All colour lives in the
//! `Frame` through the surface theme; the renderer is pure over the
//! [`DetailView`] + the activity feed, with the pure hit-test mapping through
//! the same card geometry (recomputed, never stored).
//!
//! Every projection-derived string (label, path, model name)
//! passes the shared sanitizer before it can reach a cell, so untrusted capture
//! data can never inject a terminal sequence (the security model).

use crate::col2::sanitize_for_terminal;
use crate::col2::DetailView;
use crate::col3::ActivityView;
use crate::layout::Rect;
use crate::render::frame::{Color, Frame, Style};
use crate::render::{CellShaderKind, RenderBackend};
use crate::stack::{self, Priority, Spec};
use crate::theme::{self, Icon, Section, Theme};
use crate::{EntryKind, ProjectStatus};

/// One card in the column-2 stack.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CardKind {
    /// Name, kind, path, and status.
    Repository,
    /// Repository-scoped model-call treemap with exact provenance-aware key.
    Models,
    /// Thirty-day heat strips for events/sessions/checkpoints.
    Activity,
}

impl CardKind {
    /// The cards in their fixed default display order (mirrors `CARDS`) — the
    /// default [`crate::layout_model::LayoutModel`]'s column-2 units.
    pub const ALL: [CardKind; 3] = [CardKind::Repository, CardKind::Models, CardKind::Activity];
}

/// The card registration table — the fixed display order and each card's stack
/// sizing: the repository and activity cards hold out longest;
/// models folds first.
const CARDS: [(CardKind, Spec); 3] = [
    (
        CardKind::Repository,
        Spec {
            weight: 2,
            min_rows: 6,
            priority: Priority::High,
        },
    ),
    (
        CardKind::Models,
        Spec {
            weight: 3,
            min_rows: 9,
            priority: Priority::Low,
        },
    ),
    (
        CardKind::Activity,
        Spec {
            weight: 3,
            min_rows: 9,
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

/// A card's stack sizing (weight, min-rows, priority) from the registration
/// table — the single source, so a data-driven column (the data-driven layout model) sizes each of
/// its cards exactly as the fixed stack did.
#[must_use]
pub fn card_spec(kind: CardKind) -> Spec {
    CARDS.iter().find(|(k, _)| *k == kind).map_or(
        Spec {
            weight: 1,
            min_rows: 3,
            priority: Priority::Medium,
        },
        |(_, s)| *s,
    )
}

/// Solve a card stack of `units` (in order) over `area`. Pure over the area
/// height — the same solve the renderer and the hit-test share, so a click
/// always resolves to the card that was drawn. `units` is the column's ordered
/// card list (`CardKind::ALL` for the default column).
#[must_use]
pub fn solve(units: &[CardKind], area: Rect) -> Vec<CardSlot> {
    solve_scrolled(units, area, 0).0
}

/// The number of cards in `units` — the content length the card-stack scrollbar
/// (the scrollbar rule) tracks (each is one row in the collapse-solve's lossy tier).
#[must_use]
pub fn content_len(units: &[CardKind]) -> usize {
    units.len()
}

/// Solve a card stack of `units` over `area` with a scroll `offset`,
/// returning each visible card's slot and the largest in-range offset
/// (`max_scroll`). When the stack fits, `offset` is clamped to 0 and the layout
/// is unchanged; when it overflows, the window starts `offset` cards down — the
/// shared solve the renderer and hit-test both use.
#[must_use]
pub fn solve_scrolled(units: &[CardKind], area: Rect, offset: usize) -> (Vec<CardSlot>, usize) {
    let specs: Vec<Spec> = units.iter().map(|k| card_spec(*k)).collect();
    let (slots, max_scroll) = stack::solve_windowed(&specs, area.h, offset);
    let cards = slots
        .into_iter()
        .filter_map(|slot| {
            units.get(slot.index).map(|kind| CardSlot {
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
        .collect();
    (cards, max_scroll)
}

/// Everything a card render (and the hit-test) reads — bundled so the pure
/// renderer never touches `AppState`.
pub struct CardCtx<'a> {
    /// The selected project's detail.
    pub detail: &'a DetailView,
    /// The selected project's activity-card data.
    pub activity: &'a ActivityView,
    /// Whether the detail column is focused (drives the cursor highlight).
    pub focused: bool,
    /// Whether Unicode glyphs are available (icon substitution).
    pub unicode: bool,
    /// The button whose press-flash is in flight, if any.
    pub pressed: Option<crate::hit::Target>,
    /// The press-flash progress (1.0 → 0.0) keyed by [`Self::pressed`].
    pub press_pulse: f32,
}

impl CardCtx<'_> {
    /// The press-flash fraction (0.0 at rest) for a card button drawn as
    /// `target` (the press-flash rule).
    #[must_use]
    fn flash(&self, target: &crate::hit::Target) -> f32 {
        if self.pressed.as_ref() == Some(target) {
            self.press_pulse
        } else {
            0.0
        }
    }
}

/// Draw the card stack into `area`. An uninitialized project shows only the
/// repository card (with the init hint) filling the column — mirroring the
/// earlier early-out, so the stack never invents data a fresh project doesn't have.
pub fn draw(
    ctx: &CardCtx<'_>,
    theme: Theme,
    units: &[CardKind],
    area: Rect,
    offset: usize,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    if ctx.detail.status.repository_card_only() {
        draw_repository(ctx, theme, area, false, frame);
        return;
    }
    for slot in solve_scrolled(units, area, offset).0 {
        if slot.rect.h == 0 || slot.rect.w == 0 {
            continue;
        }
        draw_one(
            ctx,
            theme,
            slot.kind,
            slot.rect,
            slot.collapsed,
            backend,
            frame,
        );
    }
}

/// Draw one card into `rect` — the per-unit dispatch a mixed column (a card
/// among panels, panel-editor mode) shares with the homogeneous stack.
pub fn draw_one(
    ctx: &CardCtx<'_>,
    theme: Theme,
    kind: CardKind,
    rect: Rect,
    collapsed: bool,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    if rect.h == 0 || rect.w == 0 {
        return;
    }
    match kind {
        CardKind::Repository => draw_repository(ctx, theme, rect, collapsed, frame),
        CardKind::Models => draw_models(ctx, theme, rect, collapsed, backend, frame),
        CardKind::Activity => draw_activity(ctx, theme, rect, collapsed, backend, frame),
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
        let buttons = repository_button_layout(rect, true);
        let summary = format!(
            "{} · {}",
            sanitize_for_terminal(&d.label),
            status_word(d.status)
        );
        // The collapsed card has a single row. Keep Recall anchored left and
        // Settings right, and put the repository headline in the honest gap
        // between them; drawing it from `inner.x` would be overwritten by the
        // left action and silently lose the selected-project state.
        let summary_x = buttons
            .recall
            .rect
            .x
            .saturating_add(buttons.recall.rect.w)
            .saturating_add(u16::from(buttons.recall.rect.w > 0));
        let summary_right = buttons
            .settings
            .rect
            .x
            .saturating_sub(u16::from(buttons.settings.rect.w > 0));
        let summary_w = usize::from(summary_right.saturating_sub(summary_x));
        let summary: String = summary.chars().take(summary_w).collect();
        draw_repository_button(
            ctx,
            theme,
            &buttons.recall,
            crate::hit::Target::RepositoryRecallViewer,
            frame,
        );
        draw_repository_button(
            ctx,
            theme,
            &buttons.settings,
            crate::hit::Target::RepositorySettings,
            frame,
        );
        frame.put_str(summary_x, inner.y, &summary, theme.body());
        return;
    }
    let buttons = repository_button_layout(rect, false);
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
    // Status with its coloured dot. At a width that needs the buttons stacked,
    // the two bottom rows belong to the two labelled actions; path + heading
    // remain and no action clips.
    if buttons.recall.rect.y == buttons.settings.rect.y {
        let dot = Icon::for_status(d.status).glyph(ctx.unicode);
        frame.set(inner.x, inner.y + 2, dot, theme.status_dot(d.status));
        // An unadopted member's status row IS the adopt act (the family view),
        // so it is drawn in the key-hint style: the cells that answer a click
        // are the cells that look clickable. `repository_adopt_rect` is the one
        // solver behind both, so the two cannot drift.
        let act = repository_adopt_rect(rect, false, d.status).is_some();
        line(
            frame,
            Rect {
                x: inner.x + 2,
                ..inner
            },
            2,
            &status_line(d),
            if act { theme.key_hint() } else { theme.body() },
        );
    }
    draw_repository_button(
        ctx,
        theme,
        &buttons.recall,
        crate::hit::Target::RepositoryRecallViewer,
        frame,
    );
    draw_repository_button(
        ctx,
        theme,
        &buttons.settings,
        crate::hit::Target::RepositorySettings,
        frame,
    );
}

/// The Repository card's **adopt act** (the family view): the status row,
/// clickable exactly when the card is drawing that row for an unadopted
/// member.
///
/// ONE solver feeds the draw and the hit-test, so the cells the operator sees
/// and the cells that answer a click are the same cells by construction — the
/// blind spot this closes, and the reason this is a function rather than a rule
/// spelled in two files. `None` means there is no act here: the card is
/// collapsed (its single row belongs to the two labelled actions), the
/// geometry pushed the buttons onto the status row, or the member is not
/// `NotAdopted` and has nothing to adopt.
#[must_use]
pub fn repository_adopt_rect(rect: Rect, collapsed: bool, status: ProjectStatus) -> Option<Rect> {
    if collapsed || status != ProjectStatus::NotAdopted {
        return None;
    }
    if rect.w < 2 || rect.h < 2 {
        return None;
    }
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        w: rect.w - 2,
        h: rect.h - 2,
    };
    // The status row is drawn only when the two actions share one row; when
    // they stack they OWN the two bottom rows and there is no status line to
    // click. Read off the same layout the renderer uses, never re-derived.
    let buttons = repository_button_layout(rect, false);
    if buttons.recall.rect.y != buttons.settings.rect.y || inner.h < 3 {
        return None;
    }
    Some(Rect {
        x: inner.x,
        y: inner.y + 2,
        w: inner.w,
        h: 1,
    })
}

/// Repository-card Project Settings button text in each collapse state.
pub const REPOSITORY_SETTINGS: &str = "[ Project Settings ]";
/// Compact button used by a collapsed repository card.
pub const REPOSITORY_SETTINGS_COLLAPSED: &str = "[settings]";
/// Repository-card entry point for the read-only Recall Viewer.
pub const REPOSITORY_RECALL_VIEWER: &str = "[ Recall Viewer ]";
/// Compact Recall Viewer chip.
pub const REPOSITORY_RECALL_COLLAPSED: &str = "[recall]";

/// One solved repository-card button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryButton {
    /// Exact clickable rectangle.
    pub rect: Rect,
    /// Exact rendered text.
    pub text: &'static str,
}

/// Both repository-card actions, solved together so they cannot overlap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryButtonLayout {
    /// Recall Viewer, left/first.
    pub recall: RepositoryButton,
    /// Project Settings, right/second.
    pub settings: RepositoryButton,
}

/// Solve both Repository actions. Full-width cards place them side by side;
/// narrow cards stack them on the two bottom inner rows. Collapsed cards use
/// compact labels and, at the absolute floor, one-letter chips rather than
/// overlapping.
#[must_use]
#[allow(clippy::too_many_lines)] // one geometry solver covering every responsive card tier
pub fn repository_button_layout(rect: Rect, collapsed: bool) -> RepositoryButtonLayout {
    let inner = if rect.w < 2 || rect.h < 2 {
        rect
    } else {
        Rect {
            x: rect.x + 1,
            y: rect.y + 1,
            w: rect.w - 2,
            h: rect.h - 2,
        }
    };
    let (mut recall_text, mut settings_text) = if collapsed {
        (REPOSITORY_RECALL_COLLAPSED, REPOSITORY_SETTINGS_COLLAPSED)
    } else {
        (REPOSITORY_RECALL_VIEWER, REPOSITORY_SETTINGS)
    };
    let mut recall_w = u16::try_from(recall_text.chars().count()).unwrap_or(0);
    let mut settings_w = u16::try_from(settings_text.chars().count()).unwrap_or(0);
    if recall_w > inner.w || settings_w > inner.w {
        recall_text = REPOSITORY_RECALL_COLLAPSED;
        settings_text = REPOSITORY_SETTINGS_COLLAPSED;
        recall_w = u16::try_from(recall_text.chars().count()).unwrap_or(0);
        settings_w = u16::try_from(settings_text.chars().count()).unwrap_or(0);
    }
    if recall_w > inner.w || settings_w > inner.w {
        recall_text = if inner.w >= 3 { "[R]" } else { "R" };
        settings_text = if inner.w >= 3 { "[S]" } else { "S" };
        recall_w = u16::try_from(recall_text.chars().count()).unwrap_or(0);
        settings_w = u16::try_from(settings_text.chars().count()).unwrap_or(0);
    }
    let empty = || Rect {
        x: inner.x,
        y: inner.y,
        w: 0,
        h: 0,
    };
    if inner.h == 0 || inner.w == 0 {
        return RepositoryButtonLayout {
            recall: RepositoryButton {
                rect: empty(),
                text: recall_text,
            },
            settings: RepositoryButton {
                rect: empty(),
                text: settings_text,
            },
        };
    }
    if recall_w.saturating_add(settings_w).saturating_add(1) <= inner.w {
        let y = if collapsed {
            inner.y
        } else {
            inner.y + inner.h - 1
        };
        return RepositoryButtonLayout {
            recall: RepositoryButton {
                rect: Rect {
                    x: inner.x,
                    y,
                    w: recall_w,
                    h: 1,
                },
                text: recall_text,
            },
            settings: RepositoryButton {
                rect: Rect {
                    x: inner.x + inner.w - settings_w,
                    y,
                    w: settings_w,
                    h: 1,
                },
                text: settings_text,
            },
        };
    }
    // A normal card has at least four inner rows. If a pathological custom
    // layout gives fewer, compact both actions onto the last row.
    if inner.h < 2 {
        let recall_width = if inner.w >= 2 { 1 } else { inner.w };
        let settings_width = u16::from(inner.w >= 2);
        return RepositoryButtonLayout {
            recall: RepositoryButton {
                rect: Rect {
                    x: inner.x,
                    y: inner.y,
                    w: recall_width,
                    h: 1,
                },
                text: "R",
            },
            settings: RepositoryButton {
                rect: Rect {
                    x: inner.x + inner.w.saturating_sub(settings_width),
                    y: inner.y,
                    w: settings_width,
                    h: 1,
                },
                text: "S",
            },
        };
    }
    RepositoryButtonLayout {
        recall: RepositoryButton {
            rect: Rect {
                x: inner.x,
                y: inner.y + inner.h - 2,
                w: recall_w.min(inner.w),
                h: 1,
            },
            text: recall_text,
        },
        settings: RepositoryButton {
            rect: Rect {
                x: inner.x,
                y: inner.y + inner.h - 1,
                w: settings_w.min(inner.w),
                h: 1,
            },
            text: settings_text,
        },
    }
}

/// Exact Project Settings chip rect, shared by draw and hit-test.
#[must_use]
pub fn repository_settings_rect(rect: Rect, collapsed: bool) -> Rect {
    repository_button_layout(rect, collapsed).settings.rect
}

/// Exact Recall Viewer chip rect, shared by draw and hit-test.
#[must_use]
pub fn repository_recall_rect(rect: Rect, collapsed: bool) -> Rect {
    repository_button_layout(rect, collapsed).recall.rect
}

fn draw_repository_button(
    ctx: &CardCtx<'_>,
    theme: Theme,
    button: &RepositoryButton,
    target: crate::hit::Target,
    frame: &mut Frame,
) {
    if button.rect.w == 0 {
        return;
    }
    let bg = crate::chrome::press_flash_bg(theme, ctx.flash(&target));
    let style = bg.map_or(theme.button(false), |bg| theme.button(false).with_bg(bg));
    let clipped: String = button
        .text
        .chars()
        .take(usize::from(button.rect.w))
        .collect();
    frame.put_str(button.rect.x, button.rect.y, &clipped, style);
}

// --- the models card ---------------------------------------------------------

fn draw_models(
    ctx: &CardCtx<'_>,
    theme: Theme,
    rect: Rect,
    collapsed: bool,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let models = &ctx.detail.models;
    let total = models
        .iter()
        .fold(0u64, |sum, model| sum.saturating_add(model.calls));
    let title = format!("models · Σ {total} calls");
    let inner = card_frame(theme, Section::Models, &title, rect, frame);
    if models.is_empty() && model_projection_placeholder(ctx.detail.status).is_some() {
        line(
            frame,
            inner,
            0,
            model_projection_placeholder(ctx.detail.status).unwrap_or("(none yet)"),
            theme.dim(),
        );
        return;
    }
    if collapsed {
        line(
            frame,
            inner,
            0,
            &format!("{} models · {total} calls", models.len()),
            theme.body(),
        );
        return;
    }

    if models.is_empty() {
        line(frame, inner, 0, "(none yet)", theme.dim());
        return;
    }

    let desired_key_rows = u16::try_from(models.len()).unwrap_or(u16::MAX);
    let key_rows = desired_key_rows
        .min(inner.h.saturating_sub(5))
        .max(1)
        .min(inner.h);
    let chart_rows = inner.h.saturating_sub(key_rows);
    if inner.w >= 24 && chart_rows >= 5 {
        let chart_area = Rect {
            x: inner.x,
            y: inner.y,
            w: inner.w,
            h: chart_rows,
        };
        let plot_rect = crate::render::treemap_outline(frame, chart_area, theme.dim(), ctx.unicode);
        let values: Vec<f64> = models.iter().map(|model| as_f64(model.calls)).collect();
        let colors: Vec<Color> = models
            .iter()
            .map(|model| model_usage_color(&model.model))
            .collect();
        let leaves = crate::charts::treemap_layout(&values);
        let samples = crate::charts::treemap_samples(&leaves, plot_rect, &colors);
        backend.shade_cells(&samples, plot_rect, CellShaderKind::Braille, frame);
        for leaf in leaves {
            let Some(model) = models.get(leaf.index) else {
                continue;
            };
            let label_rect = crate::charts::treemap_cell_rect(leaf, plot_rect);
            if label_rect.w == 0 || label_rect.h == 0 {
                continue;
            }
            let style = theme.tile_value(model_usage_color(&model.model));
            frame.set(
                label_rect.x,
                label_rect.y,
                model_leaf_symbol(leaf.index, ctx.unicode),
                style,
            );
        }
        draw_model_key(
            ctx,
            theme,
            Rect {
                y: inner.y + chart_rows,
                h: key_rows,
                ..inner
            },
            frame,
        );
    } else {
        // At intermediate geometry, exact ranked rows are more legible than a
        // treemap too small to distinguish its leaves.
        draw_model_key(ctx, theme, inner, frame);
    }
}

fn model_projection_placeholder(status: ProjectStatus) -> Option<&'static str> {
    match status {
        ProjectStatus::PickingUp => Some("(collecting model usage...)"),
        ProjectStatus::NotInitialized
        | ProjectStatus::NotProvisioned
        | ProjectStatus::SetupIncomplete
        | ProjectStatus::Error(crate::StatusError::ProjectionUnavailable) => {
            Some("(no projection yet)")
        }
        ProjectStatus::NotRepository => Some("(not a git repository)"),
        ProjectStatus::Error(crate::StatusError::RepoMissing) => Some("(repository unavailable)"),
        // `NotAdopted` says nothing about the projection: an unadopted member
        // reads its FAMILY's store, which is present and may be full. Claiming
        // "(no projection yet)" here would be the one placeholder on this card
        // that is not a fact.
        ProjectStatus::Running { .. }
        | ProjectStatus::Idle
        | ProjectStatus::NotAdopted
        | ProjectStatus::Error(crate::StatusError::CorruptSessionCache) => None,
    }
}

fn draw_model_key(ctx: &CardCtx<'_>, theme: Theme, rect: Rect, frame: &mut Frame) {
    if rect.h == 0 || rect.w == 0 {
        return;
    }
    let models = &ctx.detail.models;
    if models.is_empty() {
        line(frame, rect, 0, "(none yet)", theme.dim());
        return;
    }
    let visible = usize::from(rect.h);
    let show = models
        .len()
        .min(visible.saturating_sub(usize::from(models.len() > visible)));
    for (index, model) in models.iter().take(show).enumerate() {
        let Ok(row) = u16::try_from(index) else {
            break;
        };
        draw_model_key_row(ctx, theme, rect, row, index, model, frame);
    }
    let more = models.len().saturating_sub(show);
    if more > 0 {
        let tail_calls = models
            .iter()
            .skip(show)
            .fold(0u64, |sum, model| sum.saturating_add(model.calls));
        if let Ok(row) = u16::try_from(show) {
            line(
                frame,
                rect,
                row,
                &format!("+{more} more · {tail_calls} calls"),
                theme.dim(),
            );
        }
    }
}

fn draw_model_key_row(
    ctx: &CardCtx<'_>,
    theme: Theme,
    rect: Rect,
    row: u16,
    index: usize,
    model: &crate::col2::ModelUsage,
    frame: &mut Frame,
) {
    if row >= rect.h || rect.w == 0 {
        return;
    }
    let color = model_usage_color(&model.model);
    frame.set(
        rect.x,
        rect.y + row,
        model_leaf_symbol(index, ctx.unicode),
        theme.tile_value(color),
    );
    let provenance = match model.source {
        crate::col2::ModelProvenance::Runtime => "runtime",
        crate::col2::ModelProvenance::Configured => "configured",
    };
    let pill = format!("×{} · {provenance}", model.calls);
    let pill_width = pill.chars().count();
    let name = sanitize_for_terminal(&model.model);
    let name = if name.is_empty() { "(unknown)" } else { &name };
    let name_width = usize::from(rect.w).saturating_sub(pill_width + 3);
    let clipped: String = name.chars().take(name_width).collect();
    if rect.w >= 2 {
        frame.put_str(rect.x + 2, rect.y + row, &clipped, theme.body());
    }
    if pill_width < usize::from(rect.w) {
        let x = rect.x + rect.w - u16::try_from(pill_width).unwrap_or(rect.w);
        frame.put_str(x, rect.y + row, &pill, theme.tile_value(color));
    }
}

fn model_leaf_symbol(index: usize, unicode: bool) -> char {
    u8::try_from(index)
        .ok()
        .filter(|index| *index < 26)
        .map_or(if unicode { '•' } else { '*' }, |index| {
            char::from(b'A' + index)
        })
}

fn model_usage_color(model: &str) -> Color {
    // FNV-1a is a tiny stable checksum here, not a security identity. It keeps
    // a model's key/leaf hue stable across sort-order and usage changes.
    let hash = model
        .as_bytes()
        .iter()
        .fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        });
    let palette_len = u64::try_from(theme::MODEL_USAGE.len()).unwrap_or(1);
    let index = usize::try_from(hash % palette_len).unwrap_or(0);
    theme::MODEL_USAGE
        .get(index)
        .copied()
        .unwrap_or(Section::Models.accent())
}

// --- the activity card (three one-dimensional heat strips) -------------------

/// The three activity categories' colours (events, sessions, checkpoints),
/// sourced from the theme so the row labels and heat density match.
fn activity_cats() -> [(Icon, &'static str, Color); 3] {
    [
        (Icon::Events, "events", Section::Capture.accent()),
        (Icon::Sessions, "sess", Section::Repository.accent()),
        (Icon::Checkpoints, "checks", Section::Activity.accent()),
    ]
}

/// The activity card's title, carrying the off-strip remainder when there is
/// one.
///
/// **Byte-identical to the bare title when nothing was left off**, so a card on
/// a fully-placed month reads exactly as it always did and every existing
/// golden stays green — the change is visible only where something is actually
/// missing from the strips.
///
/// The title is the carrier rather than an extra row because the strips already
/// spend every row they have (three heat rows plus the time scale), and a fact
/// that only appears when the card is tall enough is a fact the reader cannot
/// rely on. It is drawn before the collapsed check for the same reason: a
/// collapsed card must not be the one that hides it.
fn activity_title(off: crate::col3::OffStrip) -> String {
    if !off.any() {
        return "activity · 30d".to_owned();
    }
    let parts: Vec<String> = [
        (off.events, "ev"),
        (off.sessions, "se"),
        (off.checkpoints, "ck"),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, tag)| format!("{n} {tag}"))
    .collect();
    format!("activity · 30d · {} off-strip", parts.join(" "))
}

fn draw_activity(
    ctx: &CardCtx<'_>,
    theme: Theme,
    rect: Rect,
    collapsed: bool,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let data = match ctx.activity {
        ActivityView::NoProjection => None,
        ActivityView::Days(d) => Some(d),
    };
    let title = activity_title(data.map_or_else(Default::default, |d| d.off_strip));
    let inner = card_frame(theme, Section::Activity, &title, rect, frame);
    let Some(days) = data.map(|d| &d.days) else {
        line(frame, inner, 0, "(no projection yet)", theme.dim());
        return;
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
    if inner.h < 4 {
        return;
    }
    draw_activity_strips(ctx, theme, inner, days, backend, frame);
}

fn draw_activity_strips(
    ctx: &CardCtx<'_>,
    theme: Theme,
    inner: Rect,
    days: &[crate::col3::DayBar],
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let cats = activity_cats();
    let series: [Vec<u64>; 3] = [
        days.iter().map(|day| day.events).collect(),
        days.iter().map(|day| day.sessions).collect(),
        days.iter().map(|day| day.checkpoints).collect(),
    ];
    let max_labels: Vec<String> = series
        .iter()
        .map(|values| crate::col3::fmt_km(as_f64(values.iter().copied().max().unwrap_or(0))))
        .collect();
    let label_w = if inner.w >= 20 { 3 } else { 1 };
    let max_w = if inner.w >= 20 {
        u16::try_from(max_labels.iter().map(String::len).max().unwrap_or(1))
            .unwrap_or(1)
            .min(6)
    } else {
        0
    };
    let plot_w = inner.w.saturating_sub(label_w).saturating_sub(max_w);
    if plot_w == 0 {
        return;
    }
    let chart_rows = inner.h - 1;
    let strip_h = if chart_rows >= 6 { 2 } else { 1 };
    if chart_rows < strip_h * 3 {
        return;
    }
    let shader = if ctx.unicode {
        CellShaderKind::Braille
    } else {
        CellShaderKind::LuminanceRamp
    };
    for index in 0..3usize {
        let y = inner.y + u16::try_from(index).unwrap_or(0) * strip_h;
        let plot_rect = Rect {
            x: inner.x + label_w,
            y,
            w: plot_w,
            h: strip_h,
        };
        let Some((_, name, color)) = cats.get(index).copied() else {
            continue;
        };
        let short = if label_w >= 3 {
            match index {
                0 => "ev ",
                1 => "se ",
                _ => "ck ",
            }
        } else {
            &name[..1]
        };
        frame.put_str(inner.x, y + strip_h / 2, short, Style::fg(color));
        let strip = crate::charts::heat_strip(
            series.get(index).map_or(&[], Vec::as_slice),
            plot_rect,
            color,
        );
        backend.shade_cells(&strip.samples, plot_rect, shader, frame);
        if max_w > 0 {
            let label = crate::col3::fmt_km(as_f64(strip.max));
            let width = u16::try_from(label.len()).unwrap_or(max_w).min(max_w);
            frame.put_str(
                inner.x + inner.w.saturating_sub(width),
                y + strip_h / 2,
                &label,
                theme.dim(),
            );
        }
    }
    draw_activity_time_scale(
        frame,
        Rect {
            x: inner.x + label_w,
            y: inner.y + inner.h - 1,
            w: plot_w,
            h: 1,
        },
        ctx.unicode,
        theme.dim(),
    );
}

fn draw_activity_time_scale(frame: &mut Frame, rect: Rect, unicode: bool, style: Style) {
    if rect.w < 5 || rect.h == 0 {
        return;
    }
    let oldest = if unicode { "−29d" } else { "-29d" };
    frame.put_str(rect.x, rect.y, oldest, style);
    if rect.w >= 18 {
        let middle = if unicode { "−15d" } else { "-15d" };
        let middle_w = u16::try_from(middle.chars().count()).unwrap_or(4);
        frame.put_str(
            rect.x + rect.w.saturating_sub(middle_w) / 2,
            rect.y,
            middle,
            style,
        );
    }
    let today = "today";
    let today_w = u16::try_from(today.len()).unwrap_or(5);
    if rect.w >= today_w.saturating_add(6) {
        frame.put_str(rect.x + rect.w - today_w, rect.y, today, style);
    }
}

// --- shared helpers ----------------------------------------------------------

/// The status row's text: the status word, and — for a family row whose
/// `Running` count is the sum over its members — which members it came from
/// (the family view), else the act the status asks for.
///
/// One row rather than two: the card's third line is the only one guaranteed
/// to be drawn at the widths where the two actions sit side by side, and a
/// count a reader cannot attribute is the thing the serve accounting seam forbids. Members are
/// already sanitized by the reader that names them (they are administrative
/// directory names), and the row is clipped by `line`.
fn status_line(d: &DetailView) -> String {
    let word = status_word(d.status);
    if !d.running_members.is_empty() {
        return format!("{word} · {}", d.running_members.join(", "));
    }
    // The hint already names the state it is a hint about, so it replaces the
    // word rather than following it: "error · checkout gone…" would say the
    // same thing twice in the row that has the least space for it.
    d.status
        .hint(d.kind)
        .map_or_else(|| word.to_owned(), str::to_owned)
}

/// A short human label for a project's live status.
fn status_word(status: ProjectStatus) -> &'static str {
    match status {
        ProjectStatus::Running { .. } => "running",
        ProjectStatus::PickingUp => "picking up",
        ProjectStatus::Idle => "idle",
        ProjectStatus::NotInitialized => "not initialized",
        ProjectStatus::NotProvisioned => "not set up here",
        ProjectStatus::NotRepository => "not a repository",
        ProjectStatus::SetupIncomplete => "setup incomplete",
        ProjectStatus::NotAdopted => "not adopted",
        ProjectStatus::Error(_) => "error",
    }
}

#[allow(clippy::cast_precision_loss)]
fn as_f64(v: u64) -> f64 {
    v as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::col2::{CaptureSummary, ModelProvenance, ModelUsage};
    use crate::col3::{ActivityData, DayBar};
    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities, RenderError, ShadeSample};

    #[derive(Default)]
    struct ShaderProbe {
        shader: Option<CellShaderKind>,
        samples: usize,
    }

    impl RenderBackend for ShaderProbe {
        fn name(&self) -> &'static str {
            "shader-probe"
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities::UNICODE | Capabilities::SUBCELL
        }

        fn present(&mut self, _frame: &Frame) -> Result<(), RenderError> {
            Ok(())
        }

        fn shade_cells(
            &mut self,
            samples: &[ShadeSample],
            _area: Rect,
            shader: CellShaderKind,
            _frame: &mut Frame,
        ) {
            self.shader = Some(shader);
            self.samples = samples.len();
        }
    }

    fn rect(x: u16, y: u16, w: u16, h: u16) -> Rect {
        Rect { x, y, w, h }
    }

    fn detail(status: ProjectStatus) -> DetailView {
        DetailView {
            running_members: Vec::new(),
            path: "/r/proj".into(),
            label: "proj".into(),
            kind: EntryKind::Repo,
            status,
            models: vec![
                ModelUsage {
                    model: "claude-opus".into(),
                    calls: 12,
                    source: ModelProvenance::Runtime,
                },
                ModelUsage {
                    model: "claude-haiku".into(),
                    calls: 3,
                    source: ModelProvenance::Runtime,
                },
            ],
            capture: CaptureSummary {
                events: 128,
                sessions: 6,
                checkpoints: 12,
                last_checkpoint_at_ms: Some(1),
            },
            project_settings: Box::new(crate::ProjectSettingsView::thin(
                "/r/proj".into(),
                "proj".into(),
            )),
        }
    }

    fn render(ctx: &CardCtx<'_>, area: Rect) -> (Frame, String) {
        let mut f = Frame::new(Size::new(area.x + area.w, area.y + area.h));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw(
            ctx,
            Theme::rift_dark(),
            &CardKind::ALL,
            area,
            0,
            &mut b,
            &mut f,
        );
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

    fn ctx<'a>(d: &'a DetailView, act: &'a ActivityView) -> CardCtx<'a> {
        CardCtx {
            detail: d,
            activity: act,
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
            let slots = solve(&CardKind::ALL, rect(0, 0, 40, h));
            assert_eq!(slots.len(), 3, "all three cards present at h={h}");
            let kinds: Vec<CardKind> = slots.iter().map(|s| s.kind).collect();
            assert_eq!(kinds[0], CardKind::Repository);
            assert_eq!(kinds[2], CardKind::Activity);
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
        let (_, text) = render(&ctx(&d, &act), rect(0, 0, 40, 60));
        for title in ["repository", "models", "activity"] {
            assert!(
                text.contains(title),
                "the {title} card renders its title: \n{text}"
            );
        }
        // The models card ranks and pills.
        assert!(text.contains("claude-opus"));
        assert!(text.contains("×12"));
        assert!(
            !text.contains("capture"),
            "the retired capture card draws nothing: \n{text}"
        );
    }

    #[test]
    fn uninitialized_shows_only_repository_with_project_settings() {
        // Every status with nothing to project reads the same way: the
        // repository card alone, with its Settings door — the surface the
        // hit-test mirrors through the same predicate.
        // The status row carries the status word, or the act hint where the
        // status has one (`NotProvisioned`, `NotRepository`).
        for (status, word) in [
            (ProjectStatus::NotInitialized, "not initialized"),
            (ProjectStatus::NotProvisioned, "not set up here"),
            (ProjectStatus::NotRepository, "not a git repository"),
        ] {
            assert!(status.repository_card_only());
            let d = detail(status);
            let act = ActivityView::NoProjection;
            let (_, text) = render(&ctx(&d, &act), rect(0, 0, 40, 60));
            assert!(text.contains("repository"), "{word}: {text}");
            assert!(text.contains("Project Settings"), "{word}: {text}");
            assert!(text.contains(word), "{word}: {text}");
            // The data cards are suppressed for a project with no projection.
            assert!(!text.contains("models"), "{word}: {text}");
            assert!(!text.contains("capture"), "{word}: {text}");
        }
        assert!(!ProjectStatus::Idle.repository_card_only());
        assert!(!ProjectStatus::SetupIncomplete.repository_card_only());
    }

    #[test]
    fn repository_recall_and_settings_actions_never_overlap() {
        for width in 3..60 {
            for collapsed in [false, true] {
                let layout = repository_button_layout(rect(0, 0, width, 7), collapsed);
                let a = layout.recall.rect;
                let b = layout.settings.rect;
                let overlap = a.w > 0
                    && b.w > 0
                    && a.x < b.x.saturating_add(b.w)
                    && b.x < a.x.saturating_add(a.w)
                    && a.y < b.y.saturating_add(b.h)
                    && b.y < a.y.saturating_add(a.h);
                assert!(!overlap, "width {width}, collapsed {collapsed}");
                for button in [&layout.recall, &layout.settings] {
                    if button.rect.w > 0 {
                        assert!(
                            u16::try_from(button.text.chars().count()).unwrap_or(u16::MAX)
                                <= button.rect.w,
                            "width {width}, collapsed {collapsed}: {:?}",
                            button.text
                        );
                    }
                }
            }
        }
        let wide = repository_button_layout(rect(0, 0, 48, 7), false);
        assert_eq!(wide.recall.text, "[ Recall Viewer ]");
        assert_eq!(wide.settings.text, "[ Project Settings ]");
        assert_eq!(wide.recall.rect.y, wide.settings.rect.y);
    }

    #[test]
    fn models_more_is_honest() {
        let mut d = detail(ProjectStatus::Idle);
        d.models = (0..10u64)
            .map(|i| ModelUsage {
                model: format!("m{i}"),
                calls: 10 - i,
                source: ModelProvenance::Runtime,
            })
            .collect();
        let act = ActivityView::NoProjection;
        // A short models card can't show all ten → an honest `+N more`.
        let models_rect = rect(0, 0, 40, 9);
        let mut f = Frame::new(Size::new(40, 9));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_models(
            &ctx(&d, &act),
            Theme::rift_dark(),
            models_rect,
            false,
            &mut b,
            &mut f,
        );
        let text: String = (0..f.height())
            .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
            .map(|(x, y)| f.get(x, y).map_or(' ', |c| c.ch))
            .collect();
        assert!(text.contains("more"), "the tail is counted: {text}");
        assert!(
            text.contains("45 calls"),
            "the hidden tail's calls remain exact: {text}"
        );
    }

    #[test]
    fn model_treemap_requests_braille_and_keeps_exact_key() {
        let mut d = detail(ProjectStatus::Idle);
        d.models.push(ModelUsage {
            model: "configured-only".into(),
            calls: 0,
            source: ModelProvenance::Configured,
        });
        let act = ActivityView::NoProjection;
        let mut frame = Frame::new(Size::new(48, 13));
        let mut backend = ShaderProbe::default();
        let mut model_ctx = ctx(&d, &act);
        model_ctx.unicode = true;
        draw_models(
            &model_ctx,
            Theme::rift_dark(),
            rect(0, 0, 48, 13),
            false,
            &mut backend,
            &mut frame,
        );
        assert_eq!(backend.shader, Some(CellShaderKind::Braille));
        assert!(
            backend.samples > 0,
            "positive calls produce treemap samples"
        );
        let text: String = (0..frame.height())
            .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
            .map(|(x, y)| frame.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(
            text.contains("Σ 15 calls"),
            "the title states scope: {text}"
        );
        assert!(text.contains("×12 · runtime"), "exact runtime key: {text}");
        assert!(
            text.contains("×0 · configured"),
            "zero-call observations remain in the exact key: {text}"
        );
        let chart_text: String = (1..9)
            .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
            .map(|(x, y)| frame.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(chart_text.contains('A') && chart_text.contains('B'));
        assert!(
            !chart_text.contains("claude") && !chart_text.contains('×'),
            "the plot keeps only compact symbols; exact text belongs in the key: {chart_text}"
        );
    }

    #[test]
    fn model_treemap_zero_total_has_empty_boundary_and_zero_key() {
        let mut d = detail(ProjectStatus::Idle);
        d.models = vec![ModelUsage {
            model: "configured-only".into(),
            calls: 0,
            source: ModelProvenance::Configured,
        }];
        let act = ActivityView::NoProjection;
        let mut frame = Frame::new(Size::new(40, 10));
        let mut backend = ShaderProbe::default();
        let model_ctx = ctx(&d, &act);
        draw_models(
            &model_ctx,
            Theme::rift_dark(),
            rect(0, 0, 40, 10),
            false,
            &mut backend,
            &mut frame,
        );
        assert_eq!(backend.shader, Some(CellShaderKind::Braille));
        assert_eq!(backend.samples, 0, "zero calls receive no area inflation");
        let text: String = (0..frame.height())
            .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
            .map(|(x, y)| frame.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(text.contains("+------------------------------------+"));
        assert!(text.contains("×0 · configured"));
    }

    #[test]
    fn model_card_degrades_to_ranked_rows_then_collapsed_total() {
        let d = detail(ProjectStatus::Idle);
        let act = ActivityView::NoProjection;
        let model_ctx = ctx(&d, &act);
        let mut compact = Frame::new(Size::new(22, 7));
        let mut compact_probe = ShaderProbe::default();
        draw_models(
            &model_ctx,
            Theme::rift_dark(),
            rect(0, 0, 22, 7),
            false,
            &mut compact_probe,
            &mut compact,
        );
        let compact_text: String = (0..compact.height())
            .flat_map(|y| (0..compact.width()).map(move |x| (x, y)))
            .map(|(x, y)| compact.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(compact_text.contains("×12"));
        assert_eq!(
            compact_probe.shader, None,
            "compact geometry uses exact rows"
        );

        let mut collapsed = Frame::new(Size::new(32, 3));
        let mut ascii = AsciiBackend::new(Capabilities::empty());
        draw_models(
            &model_ctx,
            Theme::rift_dark(),
            rect(0, 0, 32, 3),
            true,
            &mut ascii,
            &mut collapsed,
        );
        let collapsed_text: String = (0..collapsed.height())
            .flat_map(|y| (0..collapsed.width()).map(move |x| (x, y)))
            .map(|(x, y)| collapsed.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(collapsed_text.contains("2 models · 15 calls"));
    }

    #[test]
    fn empty_model_state_distinguishes_projection_availability() {
        let act = ActivityView::NoProjection;
        for (status, expected) in [
            (ProjectStatus::Idle, "(none yet)"),
            (ProjectStatus::PickingUp, "(collecting model usage...)"),
            (
                ProjectStatus::Error(crate::StatusError::ProjectionUnavailable),
                "(no projection yet)",
            ),
        ] {
            let mut d = detail(status);
            d.models.clear();
            let mut frame = Frame::new(Size::new(40, 9));
            let mut ascii = AsciiBackend::new(Capabilities::empty());
            draw_models(
                &ctx(&d, &act),
                Theme::rift_dark(),
                rect(0, 0, 40, 9),
                false,
                &mut ascii,
                &mut frame,
            );
            let text: String = (0..frame.height())
                .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
                .map(|(x, y)| frame.get(x, y).map_or(' ', |cell| cell.ch))
                .collect();
            assert!(text.contains(expected), "status {status:?}: {text}");
        }
    }

    /// Strengthened after review: the old "reordered" arm
    /// found `claude-opus` in an array and mapped the same single name — it
    /// never exercised a second order. The mapping is a pure FNV over the
    /// name, so order-independence is inherent; this now DEMONSTRATES it by
    /// mapping the same model set in both directions and comparing per-model,
    /// and pins every assignment inside the palette.
    #[test]
    fn model_palette_mapping_is_stable_and_order_independent() {
        let models = ["claude-opus", "claude-haiku", "gpt-5.6-sol", "gemini"];
        let forward: Vec<_> = models.iter().map(|m| model_usage_color(m)).collect();
        let backward: Vec<_> = models.iter().rev().map(|m| model_usage_color(m)).collect();
        for (i, m) in models.iter().enumerate() {
            assert_eq!(
                forward[i],
                backward[models.len() - 1 - i],
                "{m} maps differently depending on visit order"
            );
            assert_eq!(forward[i], model_usage_color(m), "{m} is unstable");
            assert!(
                theme::MODEL_USAGE.contains(&forward[i]),
                "{m} left the palette"
            );
        }
    }

    #[test]
    fn model_leaf_symbols_are_compact_and_have_an_overflow_fallback() {
        assert_eq!(model_leaf_symbol(0, true), 'A');
        assert_eq!(model_leaf_symbol(25, true), 'Z');
        assert_eq!(model_leaf_symbol(26, true), '•');
        assert_eq!(model_leaf_symbol(26, false), '*');
    }

    #[test]
    fn activity_card_renders_heat_strips_scales_and_placeholder() {
        let d = detail(ProjectStatus::Idle);
        // Missing projection → the honest placeholder.
        let placeholder = ActivityView::NoProjection;
        let mut f = Frame::new(Size::new(44, 12));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_activity(
            &ctx(&d, &placeholder),
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

        // A dense month draws three independently-scaled heat strips.
        let days = ActivityView::Days(ActivityData {
            days: (0..30u64)
                .map(|i| DayBar {
                    weekday: "Mon",
                    events: i * 3,
                    sessions: i,
                    checkpoints: i / 2,
                })
                .collect(),
            ..ActivityData::default()
        });
        let mut f2 = Frame::new(Size::new(44, 12));
        let mut b2 = AsciiBackend::new(Capabilities::empty());
        draw_activity(
            &ctx(&d, &days),
            Theme::rift_dark(),
            rect(0, 0, 44, 12),
            false,
            &mut b2,
            &mut f2,
        );
        let mut density_cells = 0;
        for y in 0..f2.height() {
            for x in 0..f2.width() {
                if let Some(c) = f2.get(x, y) {
                    if ".:-=+*#%@".contains(c.ch) {
                        density_cells += 1;
                    }
                }
            }
        }
        assert!(density_cells > 0, "the heat strips draw density cells");
        let text2: String = (0..f2.height())
            .flat_map(|y| (0..f2.width()).map(move |x| (x, y)))
            .map(|(x, y)| f2.get(x, y).map_or(' ', |c| c.ch))
            .collect();
        assert!(
            text2.contains("87"),
            "the events strip shows its own maximum: {text2}"
        );
        assert!(text2.contains("ev "), "events row is named: {text2}");
        assert!(text2.contains("se "), "sessions row is named: {text2}");
        assert!(text2.contains("ck "), "checkpoints row is named: {text2}");
        assert!(
            text2.contains("-29d"),
            "oldest relative tick renders: {text2}"
        );
        assert!(
            text2.contains("-15d"),
            "midpoint relative tick renders: {text2}"
        );
        assert!(text2.contains("today"), "today tick renders: {text2}");
    }

    /// The title is the only place the off-strip remainder can appear, so it
    /// is pinned on both sides: unchanged when nothing is missing (every
    /// existing golden depends on that), and naming what is missing when
    /// something is.
    #[test]
    fn the_activity_title_is_unchanged_until_something_is_off_the_strip() {
        use crate::col3::OffStrip;
        assert_eq!(activity_title(OffStrip::default()), "activity · 30d");
        assert_eq!(
            activity_title(OffStrip {
                events: 0,
                sessions: 0,
                checkpoints: 103,
            }),
            "activity · 30d · 103 ck off-strip",
            "the measured case: a hundred checkpoints no strip can place"
        );
        assert_eq!(
            activity_title(OffStrip {
                events: 2,
                sessions: 1,
                checkpoints: 4,
            }),
            "activity · 30d · 2 ev 1 se 4 ck off-strip",
            "categories appear in strip order and zeros are never listed"
        );
    }

    /// A quiet month and a month whose rows are all unplaceable render the same
    /// zero strips — the title is what tells them apart, and a *collapsed* card
    /// must not be the one that hides it.
    #[test]
    fn an_unplaceable_month_says_so_even_when_the_card_is_collapsed() {
        let d = detail(ProjectStatus::Idle);
        let blind = ActivityView::Days(ActivityData {
            days: (0..30u64)
                .map(|_| DayBar {
                    weekday: "Tue",
                    events: 0,
                    sessions: 0,
                    checkpoints: 0,
                })
                .collect(),
            off_strip: crate::col3::OffStrip {
                checkpoints: 103,
                ..crate::col3::OffStrip::default()
            },
        });
        for collapsed in [false, true] {
            let mut f = Frame::new(Size::new(44, 12));
            let mut b = AsciiBackend::new(Capabilities::empty());
            draw_activity(
                &ctx(&d, &blind),
                Theme::rift_dark(),
                rect(0, 0, 44, 12),
                collapsed,
                &mut b,
                &mut f,
            );
            let text: String = (0..f.height())
                .flat_map(|y| (0..f.width()).map(move |x| (x, y)))
                .map(|(x, y)| f.get(x, y).map_or(' ', |c| c.ch))
                .collect();
            assert!(
                text.contains("103 ck off-strip"),
                "collapsed={collapsed}: the remainder must reach the card: {text}"
            );
        }
    }

    #[test]
    fn a_quiet_month_still_draws_zero_scales_and_time_range() {
        // Thirty zero-count days are a real quiet month, distinct from the
        // missing-projection placeholder.
        let d = detail(ProjectStatus::Idle);
        let quiet = ActivityView::Days(ActivityData {
            days: (0..30u64)
                .map(|_| DayBar {
                    weekday: "Tue",
                    events: 0,
                    sessions: 0,
                    checkpoints: 0,
                })
                .collect(),
            ..ActivityData::default()
        });
        let mut f = Frame::new(Size::new(44, 12));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_activity(
            &ctx(&d, &quiet),
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
        assert!(text.contains('0'), "each quiet strip shows a zero maximum");
        assert!(
            text.contains("today"),
            "the fixed time range still frames the quiet month"
        );
    }

    #[test]
    fn activity_compacts_to_one_row_strips_then_the_today_summary() {
        let d = detail(ProjectStatus::Idle);
        let days = ActivityView::Days(ActivityData {
            days: (0..30u64)
                .map(|i| DayBar {
                    weekday: "Wed",
                    events: i,
                    sessions: i / 2,
                    checkpoints: i / 3,
                })
                .collect(),
            ..ActivityData::default()
        });
        let mut compact = Frame::new(Size::new(44, 6));
        let mut backend = AsciiBackend::new(Capabilities::empty());
        draw_activity(
            &ctx(&d, &days),
            Theme::rift_dark(),
            compact.rect(),
            false,
            &mut backend,
            &mut compact,
        );
        let compact_text: String = (0..compact.height())
            .flat_map(|y| (0..compact.width()).map(move |x| (x, y)))
            .map(|(x, y)| compact.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(compact_text.contains("ev "));
        assert!(compact_text.contains("se "));
        assert!(compact_text.contains("ck "));
        assert!(compact_text.contains("today"));

        let mut collapsed = Frame::new(Size::new(44, 3));
        draw_activity(
            &ctx(&d, &days),
            Theme::rift_dark(),
            collapsed.rect(),
            true,
            &mut backend,
            &mut collapsed,
        );
        let collapsed_text: String = (0..collapsed.height())
            .flat_map(|y| (0..collapsed.width()).map(move |x| (x, y)))
            .map(|(x, y)| collapsed.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(collapsed_text.contains("today 29 ev · 14 se · 9 ck"));
    }

    #[test]
    fn card_strings_are_sanitized() {
        // A hostile model name / label never reaches a cell as a control char.
        let mut d = detail(ProjectStatus::Idle);
        d.label = "evil\x1b[31mrepo".into();
        d.models[0].model = "bad\x1b[0mmodel".into();
        let act = ActivityView::NoProjection;
        let (f, _) = render(&ctx(&d, &act), rect(0, 0, 40, 60));
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
        for (w, h) in [(0, 0), (1, 1), (4, 3), (40, 2), (20, 60)] {
            let mut f = Frame::new(Size::new(w, h));
            let mut b = AsciiBackend::new(Capabilities::empty());
            draw(
                &ctx(&d, &act),
                Theme::rift_dark(),
                &CardKind::ALL,
                rect(0, 0, w, h),
                0,
                &mut b,
                &mut f,
            );
        }
    }
}
