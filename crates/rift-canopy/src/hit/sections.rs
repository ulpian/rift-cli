//! The column-3 panel stack's mouse plane: the overflow scrollbar, the
//! Recall card's period buttons and headline, the token panel's `Σ` total, and
//! the section under any other cell — resolved through `col3::solve_stack`
//! and the chip solvers the renderer draws with, in the blended space
//! (screen x, content-space y) a scrolled stack blits through.

use crate::col3::{self, SectionId, SectionSlot};
use crate::layout::Rect;
use crate::Panel;

use super::{contains, inner_of, local_row, on_scrollbar, ScrollRegion, Target};

/// A click in column 3 → the graph section under it, else focus the panel. When
/// the panels overflow, the viewport row maps through the scroll
/// `offset` into the fully-expanded stack — the same window the renderer blits —
/// so a click resolves to the panel actually on screen.
pub(super) fn section_or_focus(
    ids: &[SectionId],
    data: &col3::Col3,
    rect: Rect,
    offset: usize,
    col: u16,
    row: u16,
) -> Target {
    // The overflow scrollbar on the column's right edge wins over the panel body.
    if on_scrollbar(rect, offset, usize::from(col3::content_rows(ids)), col, row) {
        return Target::Scrollbar(ScrollRegion::Graphs);
    }
    let inner = inner_of(rect);
    let Some(dy) = local_row(inner, col, row) else {
        return Target::PanelFocus(Panel::Graphs);
    };
    let (solve_h, content_row) = content_space(ids, inner, offset, dy);
    let under = col3::solve_stack(ids, solve_h)
        .into_iter()
        .find(|slot| content_row >= slot.y && content_row < slot.y.saturating_add(slot.rows));
    let Some(slot) = under else {
        return Target::PanelFocus(Panel::Graphs);
    };
    let panel = Rect {
        x: inner.x,
        y: slot.y,
        w: inner.w,
        h: slot.rows,
    };
    chip_at(data, slot, panel, col, content_row).unwrap_or(Target::Section(slot.id))
}

/// The height the stack is solved at and the content-space row the viewport
/// row `dy` maps to: the viewport itself when the expanded stack fits, else
/// the full stack scrolled by `offset` (clamped to the overflow) — the same
/// window the renderer blits.
fn content_space(ids: &[SectionId], inner: Rect, offset: usize, dy: u16) -> (u16, u16) {
    let content_h = col3::content_rows(ids);
    if content_h <= inner.h {
        return (inner.h, dy);
    }
    let off = offset.min(usize::from(content_h.saturating_sub(inner.h)));
    (
        content_h,
        dy.saturating_add(u16::try_from(off).unwrap_or(0)),
    )
}

/// The chip under `(col, content_row)` on `panel`, if the section draws one
/// there. Every chip is recomputed from the SAME solver the renderer drew with,
/// in the blended space (screen x, content-space y), so the clickable cells are
/// exactly the drawn glyphs — no figure, collapsed, or narrow → the solver
/// says `None` → the plain section.
fn chip_at(
    data: &col3::Col3,
    slot: SectionSlot,
    panel: Rect,
    col: u16,
    content_row: u16,
) -> Option<Target> {
    match slot.id {
        SectionId::Recall => {
            recall_chip_at(data, slot.collapsed, inner_of(panel), col, content_row)
        }
        // The token panel's `Σ` total is the "by worktree" axis's affordance
        // (the family view) and sits on the panel's TITLE row, so the solver
        // takes the section's outer rect — not the inner one the Recall
        // headline lives in.
        SectionId::TokenUsage => {
            let total = col3::token_total_rect(data, panel)?;
            if contains(total, col, content_row) {
                return Some(Target::TokensByWorktree);
            }
            None
        }
        _ => None,
    }
}

/// The Recall card's controls: the period buttons, then the headline — the
/// card's one door onto the breakdown modal, present whenever the line is
/// drawn, so the modal stays reachable by mouse on a repository whose models
/// carry no seeded price.
fn recall_chip_at(
    data: &col3::Col3,
    collapsed: bool,
    inner: Rect,
    col: u16,
    content_row: u16,
) -> Option<Target> {
    let period = col3::recall_window_buttons(collapsed, inner)
        .into_iter()
        .find(|button| contains(button.rect, col, content_row));
    if let Some(button) = period {
        return Some(Target::RecallWindow(button.window));
    }
    let headline = col3::recall_headline_rect(data, inner)?;
    if contains(headline, col, content_row) {
        return Some(Target::RecallHeadline);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hit::fixtures::project;
    use crate::hit::hit_test;
    use crate::layout::solve;
    use crate::render::frame::Size;
    use crate::state::AppState;

    /// A column `w` wide and tall enough that the expanded stack fits (the
    /// un-scrolled path), with the slot and panel rect of `id` recomputed the
    /// same way the resolver does, in the blended space it works in.
    struct FittedColumn {
        rect: Rect,
        inner: Rect,
        panel: Rect,
        slot: SectionSlot,
    }

    fn fitted_column(w: u16, id: SectionId) -> FittedColumn {
        let content = col3::content_rows(&SectionId::ALL);
        let rect = Rect {
            x: 0,
            y: 0,
            w,
            h: content + 2,
        };
        let inner = inner_of(rect);
        let slot = col3::solve_stack(&SectionId::ALL, inner.h)
            .into_iter()
            .find(|slot| slot.id == id)
            .expect("the section is in the seated stack");
        let panel = Rect {
            x: inner.x,
            y: slot.y,
            w: inner.w,
            h: slot.rows,
        };
        FittedColumn {
            rect,
            inner,
            panel,
            slot,
        }
    }

    /// A Recall card carrying a priced window, the line every segment draws.
    fn priced_card() -> col3::Col3 {
        use crate::col3::{RecallView, SavingsData, ShareData};
        col3::Col3 {
            savings: RecallView::Data(SavingsData {
                usd_saved_micros: Some(105_900_000),
                usd_label: Some("~$105.90".to_owned()),
                share: Box::new(ShareData {
                    overall: Some(42.0),
                    avoided_tokens: 6_000,
                    read_tokens: 8_285,
                    ..ShareData::default()
                }),
                ..SavingsData::default()
            }),
            ..col3::Col3::default()
        }
    }

    /// The headline is the card's one door onto the breakdown: every cell of
    /// the drawn line resolves to it, and the cell past its end is the plain
    /// section — priced or not, since the door is the line and not the money.
    #[test]
    fn clicking_the_headline_opens_the_breakdown_modal() {
        use crate::col3::RecallView;
        let mut unpriced = priced_card();
        if let RecallView::Data(sd) = &mut unpriced.savings {
            sd.usd_saved_micros = None;
            sd.usd_label = None;
        }
        let ids = col3::SectionId::ALL;
        let FittedColumn {
            rect,
            inner,
            panel,
            slot,
        } = fitted_column(60, SectionId::Recall);
        assert!(!slot.collapsed, "fixture must exercise the wide path");
        for data in [priced_card(), unpriced] {
            let door =
                col3::recall_headline_rect(&data, inner_of(panel)).expect("a drawn headline");
            // Blended space → screen: y is content-space (no scroll ⇒ + inner.y).
            let screen_row = inner.y + door.y;
            for x in door.x..door.x + door.w {
                assert_eq!(
                    section_or_focus(&ids, &data, rect, 0, x, screen_row),
                    Target::RecallHeadline,
                    "cell {x} of the headline"
                );
            }
            assert_eq!(
                section_or_focus(&ids, &data, rect, 0, door.x + door.w, screen_row),
                Target::Section(col3::SectionId::Recall),
                "the cell past the line is the plain section"
            );
        }
        assert!(Target::RecallHeadline.is_button(), "button semantics");
    }

    /// The narrow and collapsed tiers draw the same line with whole segments
    /// shed to fit, and the door follows the drawn cells — so a collapsed
    /// card keeps its click twin of `S` instead of shedding it.
    #[test]
    fn the_headline_door_follows_the_fitted_line_in_the_narrow_and_collapsed_tiers() {
        let data = priced_card();
        // `saved 6.0k tokens` is 17 cells; with the share it is 25.
        for (inner, collapsed, drawn_w) in [
            (
                Rect {
                    x: 1,
                    y: 5,
                    w: 24,
                    h: 11,
                },
                false,
                17,
            ),
            (
                Rect {
                    x: 1,
                    y: 5,
                    w: 30,
                    h: 1,
                },
                true,
                25,
            ),
        ] {
            let door = col3::recall_headline_rect(&data, inner).expect("a drawn headline");
            assert_eq!(
                door.w, drawn_w,
                "whole segments only, collapsed={collapsed}"
            );
            assert_eq!(
                recall_chip_at(&data, collapsed, inner, door.x + door.w, inner.y),
                None,
                "the cell past the fitted line opens nothing"
            );
            for x in door.x..door.x + door.w {
                assert_eq!(
                    recall_chip_at(&data, collapsed, inner, x, inner.y),
                    Some(Target::RecallHeadline),
                    "cell {x}, collapsed={collapsed}"
                );
            }
        }
    }

    /// A placeholder card draws a sentence rather than a headline, so its
    /// first row opens nothing; the `S` key, hinted with the column, is the
    /// door there and always answers.
    #[test]
    fn a_placeholder_card_has_no_headline_door() {
        use crate::col3::RecallView;
        let ids = col3::SectionId::ALL;
        let FittedColumn {
            rect,
            inner,
            panel,
            slot: _,
        } = fitted_column(60, SectionId::Recall);
        let card = inner_of(panel);
        for (savings, calls) in [
            (RecallView::Quiet, RecallView::Quiet),
            (RecallView::Disabled, RecallView::Disabled),
        ] {
            let data = col3::Col3 {
                savings,
                calls,
                ..col3::Col3::default()
            };
            assert_eq!(col3::recall_headline_rect(&data, card), None);
            for x in card.x..card.x + card.w {
                assert_eq!(
                    section_or_focus(&ids, &data, rect, 0, x, inner.y + card.y),
                    Target::Section(col3::SectionId::Recall),
                    "cell {x} of the placeholder row"
                );
            }
        }
    }

    /// Each period button resolves to **its own** window on exactly its own
    /// cells, and the row is otherwise the plain section.
    ///
    /// Four adjacent chips is where an off-by-one is invisible on screen and
    /// obvious in use — a viewer asks for 7 days and gets 30 — so every
    /// button is walked cell by cell rather than sampled.
    #[test]
    fn each_period_button_resolves_to_its_own_window() {
        let data = col3::Col3 {
            recall_window: col3::RecallWindow::AllTime,
            ..col3::Col3::default()
        };
        let ids = col3::SectionId::ALL;
        let FittedColumn {
            rect,
            inner,
            panel,
            slot,
        } = fitted_column(40, SectionId::Recall);
        let buttons = col3::recall_window_buttons(slot.collapsed, inner_of(panel));
        assert_eq!(buttons.len(), 4, "the fixture must draw the period row");
        for button in &buttons {
            // Blended space → screen: y is content-space (no scroll ⇒ + inner.y).
            let screen_row = inner.y + button.rect.y;
            for x in button.rect.x..button.rect.x + button.rect.w {
                assert_eq!(
                    section_or_focus(&ids, &data, rect, 0, x, screen_row),
                    Target::RecallWindow(button.window),
                    "cell {x} of the {} button",
                    button.window.label()
                );
            }
            assert!(Target::RecallWindow(button.window).is_button());
        }
        // Past the last button the row is the plain section again, and the
        // headline row above it never answers a period.
        let last = buttons.last().expect("four buttons");
        let after = last.rect.x + last.rect.w;
        for x in after..inner.x + inner.w {
            assert_eq!(
                section_or_focus(&ids, &data, rect, 0, x, inner.y + last.rect.y),
                Target::Section(col3::SectionId::Recall),
                "cell {x} past the periods belongs to no control"
            );
        }
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, buttons[0].rect.x, inner.y + slot.y),
            Target::Section(col3::SectionId::Recall),
            "the panel's title row is not the period row",
        );
    }

    /// the family view: the token panel's `Σ` total resolves to the axis on
    /// exactly its own cells — same solver as the draw — and the cells beside
    /// it stay the plain section. It sits on the panel's TITLE row, which is
    /// the section's first row, so this also proves the resolver reaches a
    /// section's border and not only its body.
    #[test]
    fn the_token_total_wins_over_its_section_exactly_on_its_cells() {
        use crate::col3::{BreakdownData, BreakdownView, TokensData, TokensView, WorktreeAxis};
        let data = col3::Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 2.0],
                total: 7_500.0,
                by_worktree: WorktreeAxis::default(),
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 1.0,
                read: 1.0,
                output: 1.0,
                cache_write: 1.0,
                read_fraction: 0.5,
            }),
            ..col3::Col3::default()
        };
        let ids = col3::SectionId::ALL;
        let FittedColumn {
            rect,
            inner,
            panel,
            slot: _,
        } = fitted_column(40, SectionId::TokenUsage);
        let chip = col3::token_total_rect(&data, panel).expect("a total is an affordance");
        let screen_row = inner.y + chip.y;
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x, screen_row),
            Target::TokensByWorktree,
            "the total's own cells hit the axis"
        );
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x + chip.w - 1, screen_row),
            Target::TokensByWorktree,
            "…all the way to its last cell"
        );
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x - 1, screen_row),
            Target::Section(col3::SectionId::TokenUsage),
            "one cell left (the pad before it) is the plain section"
        );
        assert!(Target::TokensByWorktree.is_button(), "button semantics");
        // No facts in scope → the same cells are plain section clicks.
        let empty = col3::Col3::default();
        assert_eq!(
            section_or_focus(&ids, &empty, rect, 0, chip.x, screen_row),
            Target::Section(col3::SectionId::TokenUsage),
            "no total, no affordance, no target"
        );
    }

    #[test]
    fn the_graphs_scrollbar_resolves_where_it_is_drawn() {
        // A short Full3Col column overflows its expanded panels, so a scrollbar
        // renders — every track cell resolves to the graphs region (the scrollbar rule).
        let size = Size::new(100, 16);
        let mut s = AppState::new(vec![project("a")], size);
        s.app.splash.skip();
        s.size = size;
        let v = s.snapshot();
        let body = Rect {
            x: 0,
            y: 1,
            w: 100,
            h: 15,
        };
        let col3 = solve(body).col3;
        let bar = crate::chrome::scrollbar(
            col3,
            v.graphs_scroll,
            usize::from(col3::content_rows(&col3::SectionId::ALL)),
        )
        .expect("the short column overflows, so a bar exists");
        // Every track cell resolves to the graphs scrollbar (pixels == targets).
        for dy in 0..bar.track.h {
            assert_eq!(
                hit_test(&v, bar.track.x, bar.track.y + dy),
                Some(Target::Scrollbar(ScrollRegion::Graphs)),
            );
        }
    }

    #[test]
    fn a_fitting_column_shows_no_scrollbar() {
        // A tall column fits the expanded panels — no bar; the right edge is the
        // panel, not a scrollbar target. 55 rows, not 40: the seated stack's
        // expanded minimum is 42 (9 + 8 + 10 + 7 + 8) and the body is one row
        // short of the frame, so 40 no longer FITS and this test would be
        // asserting the overflow case while claiming the fitting one.
        let size = Size::new(100, 55);
        let mut s = AppState::new(vec![project("a")], size);
        s.app.splash.skip();
        s.size = size;
        let v = s.snapshot();
        let body = Rect {
            x: 0,
            y: 1,
            w: 100,
            h: 54,
        };
        let col3 = solve(body).col3;
        assert!(
            col3.h >= col3::content_rows(&col3::SectionId::ALL),
            "the premise: this column really does fit its expanded panels"
        );
        assert!(
            crate::chrome::scrollbar(
                col3,
                0,
                usize::from(col3::content_rows(&col3::SectionId::ALL))
            )
            .is_none(),
            "the tall column fits — no overflow bar"
        );
        let edge = col3.x + col3.w - 1;
        assert_ne!(
            hit_test(&v, edge, col3.y + 2),
            Some(Target::Scrollbar(ScrollRegion::Graphs)),
            "no scrollbar target where there is no overflow"
        );
    }
}
