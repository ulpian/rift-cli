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
//!
//! One module per card, each owning its drawing and the geometry its drawing
//! shares with the hit-test; this module keeps what every card shares — the
//! vocabulary, the stack solve, the draw context and the dispatch.

mod activity;
mod models;
mod repository;

#[cfg(test)]
mod fixtures;

use crate::col2::DetailView;
use crate::col3::ActivityView;
use crate::layout::Rect;
use crate::render::frame::{Frame, Style};
use crate::render::RenderBackend;
use crate::stack::{self, Priority, Spec};
use crate::theme::{Section, Theme};

use activity::draw_activity;
use models::draw_models;
use repository::draw_repository;
pub use repository::{
    repository_adopt_rect, repository_button_layout, repository_recall_rect,
    repository_settings_rect, RepositoryButton, RepositoryButtonLayout,
    REPOSITORY_RECALL_COLLAPSED, REPOSITORY_RECALL_VIEWER, REPOSITORY_SETTINGS,
    REPOSITORY_SETTINGS_COLLAPSED,
};

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
fn card_frame(
    theme: Theme,
    section: Section,
    title: &str,
    rect: Rect,
    focused: bool,
    frame: &mut Frame,
) -> Rect {
    crate::chrome::panel(
        frame,
        rect,
        title,
        theme.border(section, focused),
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

#[allow(clippy::cast_precision_loss)]
fn as_f64(v: u64) -> f64 {
    v as f64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::fixtures::{fixture_ctx, fixture_detail, fixture_rect};
    use crate::col3::ActivityData;
    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities};
    use crate::ProjectStatus;

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

    #[test]
    fn solver_gives_every_card_at_least_its_headline_and_never_disappears() {
        // Under pressure every card still occupies a slot (collapse, not drop),
        // for a tall and a short column.
        for h in [40u16, 12] {
            let slots = solve(&CardKind::ALL, fixture_rect(0, 0, 40, h));
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
        let d = fixture_detail(ProjectStatus::Idle);
        let act = ActivityView::NoProjection;
        let (_, text) = render(&fixture_ctx(&d, &act), fixture_rect(0, 0, 40, 60));
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
            let d = fixture_detail(status);
            let act = ActivityView::NoProjection;
            let (_, text) = render(&fixture_ctx(&d, &act), fixture_rect(0, 0, 40, 60));
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
    fn card_strings_are_sanitized() {
        // A hostile model name / label never reaches a cell as a control char.
        let mut d = fixture_detail(ProjectStatus::Idle);
        d.label = "evil\x1b[31mrepo".into();
        d.models[0].model = "bad\x1b[0mmodel".into();
        let act = ActivityView::NoProjection;
        let (f, _) = render(&fixture_ctx(&d, &act), fixture_rect(0, 0, 40, 60));
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
        let d = fixture_detail(ProjectStatus::Idle);
        let act = ActivityView::Days(ActivityData::default());
        for (w, h) in [(0, 0), (1, 1), (4, 3), (40, 2), (20, 60)] {
            let mut f = Frame::new(Size::new(w, h));
            let mut b = AsciiBackend::new(Capabilities::empty());
            draw(
                &fixture_ctx(&d, &act),
                Theme::rift_dark(),
                &CardKind::ALL,
                fixture_rect(0, 0, w, h),
                0,
                &mut b,
                &mut f,
            );
        }
    }
}
