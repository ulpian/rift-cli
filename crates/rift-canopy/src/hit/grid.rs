//! The dashboard tiers' mouse planes: the data-driven full grid, the
//! two-column and single-panel tiers, the panel editor, and the rail and card
//! columns every tier shares — each resolved through the same `solve_model`,
//! `solve_column` and card solvers the renderer draws with.

use crate::cards::{self, CardKind, CardSlot};
use crate::col3;
use crate::layout::{single_panel, PanelRects, Rect};
use crate::layout_model::{solve_column, solve_model, ColumnRole, SolvedLayout, Unit};
use crate::state::ViewModel;
use crate::view::visible_window;
use crate::Panel;

use super::sections::section_or_focus;
use super::{contains, inner_of, local_row, on_scrollbar, third_of, ScrollRegion, Target};

/// Hit-test the data-driven full grid: the command row, then the physical
/// column under the click by its role — the same `solve_model`/role dispatch
/// `view::draw_full` uses, so a click resolves to exactly the element drawn.
pub(super) fn hit_model(vm: &ViewModel, sl: &SolvedLayout, col: u16, row: u16) -> Option<Target> {
    if contains(sl.command, col, row) {
        return Some(Target::CommandBar);
    }
    let scol = sl
        .columns
        .iter()
        .find(|scol| contains(scol.rect, col, row))?;
    match scol.role() {
        ColumnRole::Rail => Some(project_or_focus(vm, scol.rect, col, row)),
        ColumnRole::Cards => Some(toggle_or_focus(vm, &scol.card_kinds(), scol.rect, col, row)),
        ColumnRole::Sections => Some(section_or_focus(
            &scol.section_ids(),
            &vm.col3,
            scol.rect,
            vm.graphs_scroll,
            col,
            row,
        )),
        // A mixed column (post cross-column move): resolve the unit under
        // the click, retaining exact repository-button geometry.
        ColumnRole::Mixed => Some(hit_mixed(&scol.units, scol.rect, col, row)),
        ColumnRole::Empty => None,
    }
}

/// Hit-test the panel editor: a column divider (right border), then a
/// unit grip (its top border row), computed from the same `solve_model` /
/// `solve_column` the grip renderer uses — so a grip's drawn cells and its click
/// region are the same (pixels == targets). A click elsewhere is inert.
pub(super) fn hit_editor(vm: &ViewModel, body: Rect, col: u16, row: u16) -> Option<Target> {
    let sl = solve_model(&vm.layout, body);
    if contains(sl.command, col, row) {
        return Some(Target::CommandBar);
    }
    let n_columns = sl.columns.len();
    for (ci, scol) in sl.columns.iter().enumerate() {
        // The right divider (shared border) resizes; checked before the body so a
        // click on the seam grabs the divider, not the column.
        if ci + 1 < n_columns && on_right_border(scol.rect, col, row) {
            return Some(Target::ColumnDivider(ci));
        }
        if !contains(scol.rect, col, row) {
            continue;
        }
        let (slots, _) = solve_column(&scol.units, scol.rect, 0);
        // The grip is the unit's whole top border row (generous + usable).
        let grip = slots.iter().position(|slot| {
            row == slot.rect.y
                && col >= slot.rect.x
                && col < slot.rect.x.saturating_add(slot.rect.w)
        });
        return Some(match grip {
            Some(ui) => Target::GripBar(crate::editor::Cursor { col: ci, unit: ui }),
            None => Target::PanelFocus(Panel::Detail),
        });
    }
    None
}

/// Hit-test one of the multi-column grids (full or two-column).
pub(super) fn hit_grid(vm: &ViewModel, r: PanelRects, col: u16, row: u16) -> Option<Target> {
    if contains(r.command, col, row) {
        return Some(Target::CommandBar);
    }
    if contains(r.col1, col, row) {
        return Some(project_or_focus(vm, r.col1, col, row));
    }
    if contains(r.col2, col, row) {
        return Some(toggle_or_focus(vm, &CardKind::ALL, r.col2, col, row));
    }
    if contains(r.col3, col, row) {
        return Some(section_or_focus(
            &col3::SectionId::ALL,
            &vm.col3,
            r.col3,
            vm.graphs_scroll,
            col,
            row,
        ));
    }
    None
}

/// Hit-test the single-panel layout: a tab strip over one focused panel.
pub(super) fn hit_single(vm: &ViewModel, body: Rect, col: u16, row: u16) -> Option<Target> {
    let (tabs, panel, _hint, command) = single_panel(body);
    if contains(command, col, row) {
        return Some(Target::CommandBar);
    }
    if contains(tabs, col, row) {
        return Some(Target::PanelFocus(tab_at(col, tabs)));
    }
    if contains(panel, col, row) {
        return Some(match vm.focus {
            Panel::Detail => toggle_or_focus(vm, &CardKind::ALL, panel, col, row),
            Panel::Graphs => section_or_focus(
                &col3::SectionId::ALL,
                &vm.col3,
                panel,
                vm.graphs_scroll,
                col,
                row,
            ),
            Panel::Projects | Panel::Command => project_or_focus(vm, panel, col, row),
        });
    }
    None
}

/// The rail's overflow scrollbar (on `bar_area`'s right edge) or the project
/// row under the click in `list` — the same visible window the renderer
/// blits, so a scrolled rail resolves to the row on screen. `None` for an
/// empty rail or a cell past the last row; the Recall Viewer's projects pane
/// shares this with the dashboard rail.
pub(super) fn rail_row_at(
    vm: &ViewModel,
    list: Rect,
    bar_area: Rect,
    col: u16,
    row: u16,
) -> Option<Target> {
    if vm.projects.is_empty() {
        return None;
    }
    let (start, end) = visible_window(vm.selected, vm.projects.len(), usize::from(list.h));
    if on_scrollbar(bar_area, start, vm.projects.len(), col, row) {
        return Some(Target::Scrollbar(ScrollRegion::Rail));
    }
    let dy = local_row(list, col, row)?;
    let index = start + usize::from(dy);
    if index < end {
        return Some(Target::ProjectRow(index));
    }
    None
}

/// Hit-test a mixed column (post cross-column move, panel-editor mode): resolve the click
/// to the panel of the unit under it — the same `solve_column` geometry the
/// renderer uses. Repository settings retains exact button geometry even after
/// cross-column moves.
fn hit_mixed(units: &[Unit], rect: Rect, col: u16, row: u16) -> Target {
    let (slots, _) = solve_column(units, rect, 0);
    let Some(slot) = slots.iter().find(|slot| contains(slot.rect, col, row)) else {
        return Target::PanelFocus(Panel::Detail);
    };
    match slot.unit {
        Unit::Rail => Target::PanelFocus(Panel::Projects),
        Unit::Card(_) => Target::PanelFocus(Panel::Detail),
        Unit::Section(id) => Target::Section(id),
    }
}

/// Whether `(col, row)` is on the right border column of `rect` — the seam
/// two columns share; a column the solver gave no width has none.
fn on_right_border(rect: Rect, col: u16, row: u16) -> bool {
    rect.w > 0 && col == rect.x + rect.w - 1 && row >= rect.y && row < rect.y.saturating_add(rect.h)
}

/// The tab under `col` in the `[Projects] Detail Graphs` strip — mapped by
/// horizontal thirds (exact glyph widths aren't needed for a coarse tab click).
fn tab_at(col: u16, tabs: Rect) -> Panel {
    match third_of(col, tabs) {
        0 => Panel::Projects,
        1 => Panel::Detail,
        _ => Panel::Graphs,
    }
}

/// A click in column 1 → the rail's action bar, the project row under it,
/// else focus the panel.
pub(super) fn project_or_focus(vm: &ViewModel, rect: Rect, col: u16, row: u16) -> Target {
    let inner = inner_of(rect);
    let (list, buttons) = crate::view::rail_action_layout(inner, !vm.projects.is_empty());
    // The bottom action bar — add / reorder / remove — takes priority
    // over row selection; its chips are the same cells the renderer drew.
    if let Some((_, target)) = buttons.iter().find(|(r, _)| contains(*r, col, row)) {
        return target.clone();
    }
    // The rail overflow scrollbar sits on the panel's right border.
    let bar_area = Rect {
        x: rect.x,
        y: list.y,
        w: rect.w,
        h: list.h,
    };
    rail_row_at(vm, list, bar_area, col, row).unwrap_or(Target::PanelFocus(Panel::Projects))
}

/// A click in column 2 → the repository card's buttons or panel focus.
/// Recomputes the card stack from the same solver the renderer uses, so a click
/// resolves to the exact card/button drawn.
pub(super) fn toggle_or_focus(
    vm: &ViewModel,
    units: &[CardKind],
    rect: Rect,
    col: u16,
    row: u16,
) -> Target {
    // The card-stack overflow scrollbar on the column's right edge.
    if on_scrollbar(rect, vm.detail_scroll, cards::content_len(units), col, row) {
        return Target::Scrollbar(ScrollRegion::Detail);
    }
    let Some(detail) = vm.detail.as_deref() else {
        return Target::PanelFocus(Panel::Detail);
    };
    // A project with nothing to project fills column 2 with just the
    // repository card — mirror `cards::draw` and keep its setup entry point
    // clickable.
    if detail.status.repository_card_only() {
        let adopt = cards::repository_adopt_rect(rect, false, detail.status);
        return if adopt.is_some_and(|rect| contains(rect, col, row)) {
            Target::RepositoryAdopt
        } else {
            Target::PanelFocus(Panel::Detail)
        };
    }
    let under = cards::solve_scrolled(units, rect, vm.detail_scroll)
        .0
        .into_iter()
        .find(|slot| contains(slot.rect, col, row));
    match under {
        Some(slot) if slot.kind == CardKind::Repository => {
            repository_card_at(&slot, detail.status, col, row)
        }
        // Inside another card, or between cards: focus.
        _ => Target::PanelFocus(Panel::Detail),
    }
}

/// The repository card's acts: its two labelled buttons, then the status row
/// when it is the adopt act. The adopt row is solved by the same function the
/// renderer draws from and consulted AFTER the buttons, so a geometry that
/// overlapped them could never steal a click from one.
fn repository_card_at(slot: &CardSlot, status: crate::ProjectStatus, col: u16, row: u16) -> Target {
    let adopt = cards::repository_adopt_rect(slot.rect, slot.collapsed, status);
    if adopt.is_some_and(|r| contains(r, col, row)) {
        return Target::RepositoryAdopt;
    }
    Target::PanelFocus(Panel::Detail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hit::fixtures::{editor_vm, project, state_with_detail, vm};
    use crate::hit::hit_test;
    use crate::render::frame::Size;
    use crate::state::AppState;

    #[test]
    fn repository_card_has_no_navigation_click_targets() {
        for (width, height) in [(120, 40), (60, 20), (40, 16)] {
            let mut s = state_with_detail(width, height);
            let view = s.snapshot();
            let rect = crate::dashboard::home_rects(view.size).0;
            for y in rect.y..rect.y + rect.h {
                for x in rect.x..rect.x + rect.w {
                    assert!(!matches!(
                        hit_test(&view, x, y),
                        Some(Target::DashboardTab(_))
                    ));
                }
            }
        }
    }

    #[test]
    fn panel_editor_grips_and_dividers_resolve_where_drawn() {
        // In edit mode, every unit's top border row grips and the shared column
        // borders resolve to dividers — computed from the same solve_model the
        // grip renderer uses (pixels == targets, panel-editor mode).
        let v = editor_vm(120, 40);
        let body = Rect {
            x: 0,
            y: 1,
            w: 120,
            h: 39,
        };
        let sl = crate::layout_model::solve_model(&v.layout, body);
        // The first column's first unit (the rail): its top border row grips.
        let c0 = &sl.columns[0];
        let (slots, _) = crate::layout_model::solve_column(&c0.units, c0.rect, 0);
        let top = slots[0].rect;
        assert_eq!(
            hit_test(&v, top.x, top.y),
            Some(Target::GripBar(crate::editor::Cursor { col: 0, unit: 0 })),
        );
        // The divider on column 0's right border resolves to ColumnDivider(0).
        let dx = c0.rect.x + c0.rect.w - 1;
        assert_eq!(
            hit_test(&v, dx, c0.rect.y + 3),
            Some(Target::ColumnDivider(0)),
        );
    }

    #[test]
    fn clicking_a_project_row_resolves_to_its_index() {
        let mut v = vm(120, 40);
        v.projects_open = true;
        for index in 0..v.projects.len() {
            assert!(
                (4..v.size.h - 2).any(|y| hit_test(&v, 3, y) == Some(Target::ProjectRow(index)))
            );
        }
    }

    #[test]
    fn clicking_empty_col1_focuses_projects() {
        let mut v = vm(120, 40);
        v.projects_open = true;
        // A row well below the three project rows is still inside col1 → focus.
        assert_eq!(hit_test(&v, 3, 20), None);
    }

    #[test]
    fn clicking_the_graphs_column_hits_a_section_or_focus() {
        let v = vm(120, 40);
        // col3 starts at x = col1.w + col2.w; sample a column well inside it.
        let recall = crate::dashboard::home_recall_content(v.size);
        let hit = hit_test(&v, recall.x + 4, recall.y);
        assert!(
            matches!(
                hit,
                Some(Target::Section(_) | Target::PanelFocus(Panel::Graphs))
            ),
            "graphs column resolves to a section or its focus, got {hit:?}"
        );
    }

    #[test]
    fn clicking_the_command_bar() {
        let v = vm(120, 40);
        // The command row is the last three rows.
        assert_eq!(hit_test(&v, 10, 39), Some(Target::CommandBar));
    }

    #[test]
    fn detail_column_focuses() {
        let v = vm(120, 40);
        // col2 begins after col1; a mid-column click resolves to Detail focus.
        let hit = hit_test(&v, 40, 10);
        assert_eq!(hit, Some(Target::PanelFocus(Panel::Detail)));
    }

    #[test]
    fn picker_footer_add_open_and_back_are_clickable() {
        let mut s = AppState::new(vec![project("a"), project("b")], Size::new(120, 40));
        s.app.splash.skip();
        s.app.projects_open = true;
        let v = s.snapshot();
        let targets = (0..v.size.w)
            .filter_map(|x| hit_test(&v, x, v.size.h - 1))
            .collect::<Vec<_>>();
        assert!(targets.contains(&Target::RailAdd));
        assert!(targets.contains(&Target::ProjectRow(v.project_cursor)));
        assert!(targets.contains(&Target::ProjectDrawer));
    }
}
