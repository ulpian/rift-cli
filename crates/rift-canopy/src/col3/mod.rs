//! Column 3 — the live panels (the dashboard information architecture): the
//! earlier six sections regroup into **token usage** (the four-class
//! proportional treemap + Σ total + compact footer), **system** (context
//! pressure + recent activity + projection storage), and **recall** (savings %,
//! tokens avoided, and per-channel serve-call rates); the two workflow panels
//! join them.
//!
//! Each panel is an individually-bordered themed card, sized by the shared
//! [`crate::stack`] solver and **honest about absence**: a
//! data-less panel still occupies its slot with its placeholder — "no token
//! activity yet", the recall enable-prompt vs the visibly distinct
//! enabled-but-quiet state, the unknown-model indicator — never a fake zero.
//! Under height pressure a panel collapses to its one-line headline (fidelity
//! is lost, information is not). The donut rasterizes via the hi-res sub-cell
//! tier on SUBCELL-capable backends and degrades to the shipped horizontal
//! gauge bar elsewhere; its formula/bands stay owned by `rift-nic-gauge`
//! (the gauge's single owner).
//!
//! The types here are **filled by the data plane** (`rift-canopy-data`, which
//! imports this crate — never the reverse, the data-plane split) and rendered pure over
//! the [`crate::state::ViewModel`].
//!
//! One module per panel holds its view-model, its `draw_*` and its helpers;
//! `format` holds the number formats they share, `anim` the tween that eases
//! the call chart and snaps everything else, and `worktree` the by-worktree
//! axis two modals draw. This file keeps the section registry, the stack
//! solver, [`Col3`] and the frame that seats one panel per solved slot.

mod activity;
mod anim;
mod calls;
mod format;
mod recall;
mod recall_bars;
mod recall_period;
mod savings;
mod system;
mod tokens;
mod workflow;
mod worktree;

use crate::layout::Rect;
use crate::render::frame::{Frame, Style};
use crate::render::RenderBackend;
use crate::theme::{Section, Theme};

use recall::draw_recall_panel;
use system::draw_system_panel;
use tokens::draw_token_panel;
use workflow::{draw_dispatch_panel, draw_recap_panel};

pub use activity::{ActivityData, ActivityView, DayBar, OffStrip};
pub(crate) use anim::{interpolate, should_animate};
pub use calls::{CallsData, ChannelCallSeries};
pub use format::{fmt_count, fmt_km};
pub use recall::{recall_cost_rect, recall_share_rect, share_chip_label, RecallView};
pub use recall_bars::RecallUnit;
pub use recall_period::{
    recall_unit_rect, recall_window_buttons, RecallWindow, RecallWindowButton, WindowCut,
};
pub use savings::{
    savings_share_lines, usd_breakdown_lines, FamilyShareRow, SavingsData, ShareData, UsdRow,
};
pub use system::{ActiveSessionsView, NicReading, NicView, PanelMetrics, SystemScope};
pub use tokens::{token_total_rect, BreakdownData, BreakdownView, TokensData, TokensView};
pub use workflow::{DispatchData, DispatchView, FileTouch, RecapData, RecapSavings, RecapView};
pub use worktree::{worktree_axis_lines, WorktreeAxis, WorktreeSlice, UNKNOWN_WORKTREE};

/// The column-3 panels (the dashboard information architecture — the earlier
/// six sections regroup here, none of the data dropped, and the two workflow
/// panels join them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionId {
    /// Token usage: the four-class proportional treemap + compact readouts.
    TokenUsage,
    /// System: context pressure + recent activity + projection storage.
    System,
    /// Recall: savings % + tokens avoided and per-channel serve-call rates.
    Recall,
    /// Dispatch: what this repository has captured — checkpoints (and how their
    /// times were known), sessions, the AI/MX/HU split, and the files that
    /// carried it. The card half of `rift dispatch`.
    Dispatch,
    /// Recap: how agents are being used — sessions by agent (or by model),
    /// tokens, cost, and recall's ledger. The card half of `rift recap`.
    Recap,
}

impl SectionId {
    /// The panels the default [`crate::layout_model::LayoutModel`] seats in
    /// column 3, in display order — today, every registered one.
    ///
    /// It is a **separate list from [`SECTIONS`]** and stays one even while the
    /// two agree. Registration is what the stack solver sizes and what a
    /// persisted layout may name; seating is what a fresh install draws; and a
    /// panel can be the first without being the second, which is how the two
    /// workflow panels were built, fed and tested before they moved a single
    /// golden frame.
    ///
    /// **It is not a free list, and that is the point of keeping the two
    /// apart.** Adding an id here rewrites every committed frame that draws the
    /// dashboard, so the seated set is a product decision rather than a
    /// consequence of registering a panel — the workflow pair was seated by an
    /// explicit owner decision, on the argument that a panel a user cannot reach
    /// is a green test over an invisible surface (the panel editor moves units;
    /// it cannot add one).
    ///
    /// **What seating costs the panels already here is not nothing**, and the
    /// two halves of that are worth keeping apart because only one of them is
    /// pinned by a test.
    ///
    /// Pinned: no panel drops below its **registered minimum** before a `Low`
    /// one folds. Both newcomers are [`Priority::Low`] and the solver folds
    /// every `Low` before any `Medium`, so
    /// `the_workflow_panels_fold_before_the_chart_panels` holds TokenUsage,
    /// System and Recall at 9/8/13 rows at height 32.
    ///
    /// Not pinned, and measured instead: the **surplus** above those minimums
    /// is now divided five ways, and the column's expanded content height goes
    /// 27 → 42 — so a column between those heights that used to fit now
    /// SCROLLS ([`crate::col3::max_scroll`], the scrollbar rule's viewport). Read off the
    /// `col3_tall` golden at 30 rows: TokenUsage 11 → 9, System 9 → 8, Recall
    /// 10 → 10, and the recap panel below the fold at offset 0. Two tests in
    /// this file had to grow their frames (33 → 52, 30 → 45) for exactly that
    /// reason: their assertions sat on rows seating took away.
    ///
    /// The Recall panel's own minimum has since gone 10 → 13 for the period
    /// row and the read-vs-saved bars it now carries, taking the expanded
    /// content height 42 → 45 and moving one more panel below the fold on a
    /// short column.
    pub const ALL: [SectionId; 5] = [
        SectionId::TokenUsage,
        SectionId::System,
        SectionId::Recall,
        SectionId::Dispatch,
        SectionId::Recap,
    ];
}

/// How reluctantly a section collapses under height pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Collapses first.
    Low,
    /// Collapses after every Low section.
    Medium,
    /// Collapses last.
    High,
}

/// One section's registration: id, height weight, expanded minimum, priority.
#[derive(Clone, Copy, Debug)]
pub struct SectionSpec {
    /// Which section.
    pub id: SectionId,
    /// Relative share of surplus height (all sections at their minimums first).
    pub weight: u16,
    /// The smallest useful expanded height (title + content).
    pub min_rows: u16,
    /// Collapse order under pressure.
    pub priority: Priority,
}

impl SectionSpec {
    /// This section's sizing as a generic [`crate::stack::Spec`].
    fn to_stack(self) -> crate::stack::Spec {
        crate::stack::Spec {
            weight: self.weight,
            min_rows: self.min_rows,
            priority: match self.priority {
                Priority::Low => crate::stack::Priority::Low,
                Priority::Medium => crate::stack::Priority::Medium,
                Priority::High => crate::stack::Priority::High,
            },
        }
    }
}

/// The registration table — the **single** place the panel order, weights, and
/// collapse priorities are defined: token usage (the chart +
/// tiles) and system (the donut) hold out longest; recall folds first.
///
/// The two workflow panels are `Low`: they are *tables of counts*, so a
/// collapsed one-line headline loses far less than a collapsed chart does, and
/// a column that has to shed rows should shed them where the loss is smallest.
/// The solver collapses every `Low` before any `Medium`, so seating them cannot
/// take a row from the three panels that were here first until the column is
/// too short for all five headlines.
pub const SECTIONS: [SectionSpec; 5] = [
    SectionSpec {
        id: SectionId::TokenUsage,
        weight: 3,
        min_rows: 9,
        priority: Priority::High,
    },
    SectionSpec {
        id: SectionId::System,
        weight: 3,
        min_rows: 8,
        priority: Priority::High,
    },
    // Thirteen, not ten: the panel now carries three content blocks over its
    // two headline rows — the period row, the read-vs-saved bars, and the
    // per-channel call chart. At the old minimum the chart was left three
    // rows, which is a legend and an axis with nothing between them. Raising
    // the *registered* minimum is how a panel that grew says so, rather than
    // taking the rows silently out of a chart that has no way to complain.
    SectionSpec {
        id: SectionId::Recall,
        weight: 2,
        min_rows: 13,
        priority: Priority::Medium,
    },
    SectionSpec {
        id: SectionId::Dispatch,
        weight: 2,
        min_rows: 7,
        priority: Priority::Low,
    },
    SectionSpec {
        id: SectionId::Recap,
        weight: 2,
        min_rows: 8,
        priority: Priority::Low,
    },
];

/// A section's stack sizing as a generic [`crate::stack::Spec`] — the single
/// source, so a data-driven column (the data-driven layout model) sizes each panel exactly as the
/// fixed stack did.
#[must_use]
pub fn section_spec(id: SectionId) -> crate::stack::Spec {
    SECTIONS.iter().find(|s| s.id == id).map_or(
        crate::stack::Spec {
            weight: 1,
            min_rows: 3,
            priority: crate::stack::Priority::Medium,
        },
        |s| s.to_stack(),
    )
}

/// One solved slot: where a section renders and whether it collapsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionSlot {
    /// Which section.
    pub id: SectionId,
    /// Row offset within the column body.
    pub y: u16,
    /// Rows granted (1 when collapsed).
    pub rows: u16,
    /// Collapsed to a one-line summary.
    pub collapsed: bool,
}

/// Distribute `height` rows across the six sections via the shared
/// [`crate::stack`] solver (the same contract the column-2 card stack uses):
/// everything expanded when the minimums fit; under pressure Low then
/// Medium then High collapse to one-line headlines; below six rows the first
/// `height` render collapsed and the tail drops. Maps the generic index slots
/// back onto the fixed [`SectionId`] order.
#[must_use]
pub fn solve_stack(ids: &[SectionId], height: u16) -> Vec<SectionSlot> {
    let specs: Vec<crate::stack::Spec> = ids.iter().map(|id| section_spec(*id)).collect();
    crate::stack::solve(&specs, height)
        .into_iter()
        .filter_map(|slot| {
            ids.get(slot.index).map(|id| SectionSlot {
                id: *id,
                y: slot.y,
                rows: slot.rows,
                collapsed: slot.collapsed,
            })
        })
        .collect()
}

/// The panel stack's **fully-expanded** height for `ids` (every panel at its
/// `min_rows`, none collapsed) — the content length the graphs viewport scrolls
/// over (the scrollbar rule).
#[must_use]
pub fn content_rows(ids: &[SectionId]) -> u16 {
    ids.iter()
        .map(|id| section_spec(*id).min_rows)
        .fold(0, u16::saturating_add)
}

/// The largest in-range graphs scroll offset for a column of `height` rows: 0
/// when the expanded panels fit, else the overflow. Content-aware clamping reads
/// this so the offset can never scroll past the end.
#[must_use]
pub fn max_scroll(ids: &[SectionId], height: u16) -> usize {
    usize::from(content_rows(ids).saturating_sub(height))
}

/// The whole column-3 view-model the data plane fills (Arc-shared through the
/// snapshot; `Default` is every section's honest placeholder).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Col3 {
    /// How the data bus is woken: `PollOnly` adds a
    /// dim "watch: poll-only" row to the System panel (and a head suffix when
    /// collapsed); `Native` draws nothing — the default, so every golden that
    /// predates the field is unchanged.
    pub watch: crate::state::WatchMode,
    /// Token usage.
    pub tokens: TokensView,
    /// Input | read | output.
    pub breakdown: BreakdownView,
    /// The nic-gauge.
    pub nic: NicView,
    /// Recall savings.
    pub savings: RecallView<SavingsData>,
    /// Recall serve calls.
    pub calls: RecallView<CallsData>,
    /// The unit the Recall panel's bar chart is read in. A viewer's choice,
    /// set by the reducer; no feed writes it, and flipping it needs no read
    /// because both figures ride the same frame.
    pub recall_unit: RecallUnit,
    /// The period the Recall panel's figures were computed over.
    ///
    /// A **viewer's choice**, so the reducer sets it and no feed ever does:
    /// the panel must be able to say which window it is drawing before the
    /// bus has answered for it, or the buttons would lag a frame behind the
    /// press. The default is [`RecallWindow::AllTime`], which is what the card
    /// showed before there were buttons.
    pub recall_window: RecallWindow,
    /// Top-level capture sessions currently live.
    pub active_sessions: ActiveSessionsView,
    /// The system panel's four domain metrics. Headlines — taken from
    /// the target immediately, never tweened.
    pub metrics: PanelMetrics,
    /// The dispatch panel's window facts.
    pub dispatch: DispatchView,
    /// The recap panel's window facts.
    pub recap: RecapView,
}

/// Render the column-3 panels into `area` through the plot seam. Each panel
/// is an individually-bordered themed card, sized by the
/// shared stack solver; `focused` accents the borders when the graphs column
/// holds focus; `unicode` drives glyph substitution. Pure over the view-model
/// and safe at any size.
pub fn draw(
    col3: &Col3,
    ids: &[SectionId],
    focused: bool,
    unicode: bool,
    area: Rect,
    offset: usize,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let content_h = content_rows(ids);
    if content_h <= area.h || area.w == 0 || area.h == 0 {
        // The expanded panels fit: solve at the column height (surplus spreads),
        // draw straight in. `scroll.graphs` clamps to 0 — nothing to scroll.
        draw_stack(col3, ids, focused, unicode, area, backend, frame);
        return;
    }
    // Overflow (the scrollbar rule): render the fully-expanded stack into a scratch frame,
    // then blit the [offset, offset+viewport) window — so "j/k scroll" moves the
    // panels instead of collapsing them, with the scrollbar as its sign.
    let mut scratch = Frame::new(crate::render::frame::Size::new(area.w, content_h));
    draw_stack(
        col3,
        ids,
        focused,
        unicode,
        scratch.rect(),
        backend,
        &mut scratch,
    );
    let off = offset.min(usize::from(content_h.saturating_sub(area.h)));
    frame.blit_window(&scratch, area, off);
}

/// Draw the `ids` panels straight into `area` at the solver's sizing (the
/// non-scrolled path; also the scratch-frame body for the scrolled path).
fn draw_stack(
    col3: &Col3,
    ids: &[SectionId],
    focused: bool,
    unicode: bool,
    area: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    for slot in solve_stack(ids, area.h) {
        let rect = Rect {
            x: area.x,
            y: area.y + slot.y,
            w: area.w,
            h: slot.rows,
        };
        draw_one(
            col3,
            slot.id,
            focused,
            unicode,
            slot.collapsed,
            rect,
            backend,
            frame,
        );
    }
}

/// Draw one panel into `rect` — the per-unit dispatch a mixed column (a panel
/// among cards, panel-editor mode) shares with the homogeneous panel stack.
pub fn draw_one(
    col3: &Col3,
    id: SectionId,
    focused: bool,
    unicode: bool,
    collapsed: bool,
    rect: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    if rect.h == 0 || rect.w == 0 {
        return;
    }
    let theme = Theme::rift_dark();
    match id {
        SectionId::TokenUsage => {
            draw_token_panel(
                col3, theme, focused, unicode, collapsed, rect, backend, frame,
            );
        }
        SectionId::System => {
            draw_system_panel(
                col3, theme, focused, unicode, collapsed, rect, backend, frame,
            );
        }
        SectionId::Recall => draw_recall_panel(
            col3, theme, focused, unicode, collapsed, rect, backend, frame,
        ),
        SectionId::Dispatch => draw_dispatch_panel(col3, theme, focused, collapsed, rect, frame),
        SectionId::Recap => draw_recap_panel(col3, theme, focused, collapsed, rect, frame),
    }
}

/// A panel border + title (with the Σ / headline appended), returning the inner
/// content rect.
pub(super) fn panel_frame(
    theme: Theme,
    section: Section,
    focused: bool,
    title: &str,
    rect: Rect,
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

/// One body line inside a panel's inner rect (dropped when below it).
pub(super) fn body(frame: &mut Frame, inner: Rect, dy: u16, text: &str, style: Style) {
    if dy >= inner.h {
        return;
    }
    let clipped: String = text.chars().take(usize::from(inner.w)).collect();
    frame.put_str(inner.x, inner.y + dy, &clipped, style);
}

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;

    use super::fixtures::{draw_at, text_of};

    #[test]
    fn a_short_column_scrolls_the_expanded_panels() {
        // The scroll offset moves content: below the expanded content
        // height, a different offset windows a different slice; a tall column
        // fits, so the offset is inert (the old code's no-op, now the fit path).
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 5.0],
                total: 22.0,
                by_worktree: WorktreeAxis::default(),
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 200.0,
                read: 800.0,
                output: 300.0,
                cache_write: 50.0,
                read_fraction: 0.8,
            }),
            ..Col3::default()
        };
        assert!(
            content_rows(&SectionId::ALL) > 12,
            "the fixture stack overflows a 12-row column"
        );
        let top = text_of(&draw_at(&col3, 34, 12, 0));
        let scrolled = text_of(&draw_at(&col3, 34, 12, 5));
        assert_ne!(top, scrolled, "the offset scrolls the panels");
        // A column taller than the expanded content ignores the offset.
        let fit_h = content_rows(&SectionId::ALL);
        let fit_a = text_of(&draw_at(&col3, 34, fit_h, 0));
        let fit_b = text_of(&draw_at(&col3, 34, fit_h, 5));
        assert_eq!(fit_a, fit_b, "a fitting column has nothing to scroll");
        assert_eq!(max_scroll(&SectionId::ALL, fit_h), 0);
        assert!(max_scroll(&SectionId::ALL, 12) > 0);
    }

    #[test]
    fn order_is_the_panels_by_registration() {
        let ids: Vec<SectionId> = SECTIONS.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![
                SectionId::TokenUsage,
                SectionId::System,
                SectionId::Recall,
                SectionId::Dispatch,
                SectionId::Recap,
            ]
        );
        // Registration is the wider list; `ALL` is what the default layout
        // seats. They are allowed to differ — but every seated panel must be a
        // registered one, or the stack solver sizes it from a fallback.
        for id in SectionId::ALL {
            assert!(
                SECTIONS.iter().any(|s| s.id == id),
                "{id:?} is unregistered"
            );
        }
    }

    /// **Under pressure the two workflow panels fold before anything that was
    /// here first**, so no panel that predates them is pushed below its
    /// registered minimum.
    ///
    /// That is the whole of what this test pins, and it is deliberately less
    /// than "seating costs the chart panels nothing". It looks at height 29,
    /// where the minimums fit exactly and there is no surplus to divide; the
    /// cost of seating lands in the surplus at every taller height, and it is
    /// real (see [`SectionId::ALL`] for the measured figures). A test that
    /// examines the one height with nothing to lose cannot speak for the
    /// heights that have something.
    #[test]
    fn the_workflow_panels_fold_before_the_chart_panels() {
        let ids: Vec<SectionId> = SECTIONS.iter().map(|s| s.id).collect();
        // 9 + 8 + 13 + 1 + 1 = 32: every High/Medium panel expanded, both Low
        // panels at their one-line headline.
        let slots = solve_stack(&ids, 32);
        assert_eq!(slots.len(), 5, "every panel keeps a slot");
        let by = |id: SectionId| *slots.iter().find(|s| s.id == id).expect("slot");
        assert!(!by(SectionId::TokenUsage).collapsed);
        assert!(!by(SectionId::System).collapsed);
        assert!(!by(SectionId::Recall).collapsed);
        assert!(by(SectionId::Dispatch).collapsed);
        assert!(by(SectionId::Recap).collapsed);
        assert_eq!(by(SectionId::TokenUsage).rows, 9, "no row was taken");
        assert_eq!(by(SectionId::System).rows, 8);
        assert_eq!(by(SectionId::Recall).rows, 13);
    }

    #[test]
    fn tall_column_expands_every_panel() {
        // 9 + 8 + 13 + 7 + 8 = 45, the seated stack's expanded minimum.
        let slots = solve_stack(&SectionId::ALL, 45);
        assert_eq!(slots.len(), 5);
        assert!(slots.iter().all(|s| !s.collapsed));
        let mut y = 0;
        for s in &slots {
            assert_eq!(s.y, y);
            y += s.rows;
        }
        assert!(y <= 45);
    }

    #[test]
    fn short_column_collapses_recall_first_then_none_disappears() {
        // Recall is Medium and folds before the two High panels — but AFTER the
        // two Low ones, so the height that isolates its fold is the High-only
        // tier: 9 + 8 + 1 + 1 + 1 = 20 fits, 29 (Medium still expanded) does
        // not. 22 sits inside that band and is what makes this test about
        // Recall rather than about everything folding at once.
        let slots = solve_stack(&SectionId::ALL, 22);
        assert_eq!(slots.len(), 5, "every panel keeps a slot");
        let by = |id: SectionId| *slots.iter().find(|s| s.id == id).unwrap();
        assert!(
            by(SectionId::Recall).collapsed,
            "recall folds before the High panels"
        );
        assert!(!by(SectionId::TokenUsage).collapsed);
        assert!(!by(SectionId::System).collapsed);
        assert!(
            slots.iter().all(|s| s.rows >= 1),
            "a collapsed panel keeps its headline, never disappears"
        );
    }

    #[test]
    fn zero_height_is_empty() {
        assert!(solve_stack(&SectionId::ALL, 0).is_empty());
    }
}
