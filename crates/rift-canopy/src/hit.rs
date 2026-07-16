//! Mouse hit-testing: a click coordinate → the dashboard element under it.
//!
//! [`hit_test`] is a **pure** function of the render [`ViewModel`], recomputing
//! geometry from the *same* layout solvers the renderer uses ([`crate::layout`],
//! [`crate::col3::solve_stack`], [`crate::view::visible_window`],
//! [`crate::view::detail_line_kinds`]) — so a click always resolves to the exact
//! element that was drawn, with no stored/mutable geometry and no resize race.
//! It is engine-free (no `xre_*`), so it compiles and tests without the `xre`
//! feature, exactly like the keyboard reducer it feeds.

use crate::cards::{self, CardKind};
use crate::col3;
use crate::layout::{choose_mode, single_panel, two_col, LayoutMode, PanelRects, Rect};
use crate::state::ViewModel;
use crate::view::visible_window;
use crate::{Panel, ProjectStatus};

/// A scrollable region that can carry a scrollbar (ADR-0031). Identifies which
/// region a [`Target::Scrollbar`] belongs to so a future thumb-drag knows what to
/// move; the indicator ships first (drag is a later rung on the same geometry).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScrollRegion {
    /// The projects rail (column 1).
    Rail,
    /// The column-2 detail card stack.
    Detail,
    /// The column-3 graph panel stack.
    Graphs,
    /// An open modal's scrollable body.
    Modal,
}

/// What a click resolves to — mapped by the reducer onto the same state changes
/// the keyboard produces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// Focus a panel (a click in it that isn't a more specific target).
    PanelFocus(Panel),
    /// Select the project list row at this index (column 1).
    ProjectRow(usize),
    /// The rail's `+` button — begin registering a project (like the `a` key).
    RailAdd,
    /// The rail's `↑` button — move the selected project up (ReorderProject).
    RailReorderUp,
    /// The rail's `↓` button — move the selected project down.
    RailReorderDown,
    /// The rail's `✕` button — remove the selected project (RemoveProject).
    RailRemove,
    /// The feature-toggle at this index (column 2).
    ToggleRow(usize),
    /// The `[r]` retry chip on the Failed feature row at this index (column 2):
    /// reinstalls it, the click equivalent of the `r` key.
    RetryRow(usize),
    /// A column-3 graph section.
    Section(col3::SectionId),
    /// The bottom command bar.
    CommandBar,
    /// The dispatch overlay's Confirm button (mouse Enter — confirm only).
    OverlayConfirm,
    /// The dispatch overlay's Cancel button (mouse Esc).
    OverlayCancel,
    /// The modal's close X — dismiss (mouse/Tab equivalent of Esc; ADR-0027).
    OverlayClose,
    /// A message modal's primary action — the first-run wizard's *add a project*
    /// button (ADR-0029): begins registering a project, like the `a` key.
    WizardAdd,
    /// The dispatch overlay body (wheel scroll / inert click).
    OverlayBody,
    /// A region's scrollbar (ADR-0031): an overflow indicator today (a click is
    /// inert; the wheel scrolls the region under it). Drag/click-to-page is a
    /// later rung on this same track/thumb geometry.
    Scrollbar(ScrollRegion),
    /// A suggest-panel row: click INSERTS it (Tab's mouse equivalent — never
    /// execution); the wheel moves the highlight.
    SuggestRow(usize),
    /// The header's `?` action — open the palette help overlay (M55-04).
    HeaderHelp,
    /// The header's `/` action — open the command palette.
    HeaderPalette,
    /// The header's `e` action — toggle the panel editor (ADR-0034).
    HeaderEditor,
    /// The header's `q` action — quit the dashboard.
    HeaderQuit,
    /// The header's identity button (ADR-0037) — open/close the profile menu.
    HeaderProfile,
    /// A profile row in the open profile menu: click-to-activate (a click
    /// moves the cursor there and switches, exactly like `Enter`).
    ProfileRow(usize),
    /// A hub row's `[settings]` chip (ADR-0044 §4) — opens that profile's
    /// settings page, the click twin of `s`.
    ProfileSettings(usize),
    /// The hub's `[ + new profile ]` row — opens the naming input, the click
    /// twin of `n`.
    ProfileNew,
    /// The profile menu's close `[x]` (ADR-0046): closes the MODE — the
    /// pointer twin of Esc-from-Browse. Never `OverlayClose` (that clears an
    /// overlay, not a mode).
    ProfileMenuClose,
    /// A settings-page row (ADR-0044 §3): a click activates it, exactly like
    /// `Enter` on that row.
    SettingsRow(usize),
    /// A row in the open colour picker (ADR-0039): a click selects it through
    /// the shared modal hit-test, exactly like `Enter` on that row.
    PickerRow(usize),
    /// A unit's grip in panel-editor mode (ADR-0034): a press picks it up / drops
    /// it. Carries the unit's `(column, unit)` cursor so the drop knows the source.
    GripBar(crate::editor::Cursor),
    /// A column divider in panel-editor mode: a drag resizes it. Carries the index
    /// of the column to its left.
    ColumnDivider(usize),
}

impl Target {
    /// Whether this target is a clickable *button* that acknowledges a press
    /// with the flash (ADR-0028). Header actions and the overlay
    /// confirm/cancel today; the rail/detail action buttons and the
    /// modal close join as they land. Selection/focus/body targets do not
    /// flash (the rail row already has its own selection pulse).
    #[must_use]
    pub fn is_button(&self) -> bool {
        matches!(
            self,
            Target::HeaderHelp
                | Target::HeaderPalette
                | Target::HeaderEditor
                | Target::HeaderQuit
                | Target::HeaderProfile
                | Target::OverlayConfirm
                | Target::OverlayCancel
                | Target::OverlayClose
                | Target::WizardAdd
                | Target::RailAdd
                | Target::RailReorderUp
                | Target::RailReorderDown
                | Target::RailRemove
                | Target::ToggleRow(_)
                | Target::RetryRow(_)
                | Target::ProfileSettings(_)
                | Target::ProfileNew
                | Target::ProfileMenuClose
        )
    }
}

/// The element at cell `(col, row)`, or `None` for empty space / a too-small
/// terminal. Row 0 is the header: its right-aligned actions resolve to their
/// targets (geometry shared with [`crate::bars`]); the rest of the bar is
/// inert.
#[must_use]
pub fn hit_test(vm: &ViewModel, col: u16, row: u16) -> Option<Target> {
    let size = vm.size;
    if col >= size.w || row >= size.h {
        return None; // off-grid
    }
    if choose_mode(size) == LayoutMode::TooSmall {
        return None;
    }
    if row == 0 {
        // The header actions stay clickable only while no overlay owns the
        // surface (mirrors the overlay-owns-clicks rule below).
        if vm.overlay.is_some() {
            return None;
        }
        // The cluster solver (ADR-0044) resolves every row-0 target — the
        // identity item included — from the SAME spans the renderer draws.
        return match crate::bars::header_action_at(size.w, vm.profile_name.as_deref(), col) {
            Some(crate::bars::HeaderAction::Help) => Some(Target::HeaderHelp),
            Some(crate::bars::HeaderAction::Profile) => Some(Target::HeaderProfile),
            Some(crate::bars::HeaderAction::Palette) => Some(Target::HeaderPalette),
            Some(crate::bars::HeaderAction::Editor) => Some(Target::HeaderEditor),
            Some(crate::bars::HeaderAction::Quit) => Some(Target::HeaderQuit),
            None => None,
        };
    }
    // The body sits one row below the title (mirrors `view::draw`).
    let body = Rect {
        x: 0,
        y: 1,
        w: size.w,
        h: size.h.saturating_sub(1),
    };
    // An open overlay owns its region (drawn last, so it is what the user sees).
    // Clicks outside fall through to nothing while the overlay is up — the panels
    // beneath are covered visually, so they must not be clickable.
    if let Some(overlay) = &vm.overlay {
        return hit_overlay(overlay, size, col, row);
    }
    // The suggest panel (drawn over the body, above the bar) owns its rows
    // while open — a click inserts, exactly like Tab (M5-11).
    if !vm.suggestions.is_empty() {
        let rect = crate::view::suggest_panel_rect(vm.size, vm.suggestions.len());
        if rect.h > 0 && contains(rect, col, row) {
            let inner = inner_of(rect);
            if let Some(dy) = local_row(inner, col, row) {
                let idx = usize::from(dy);
                if idx < vm.suggestions.len().min(usize::from(inner.h)) {
                    return Some(Target::SuggestRow(idx));
                }
            }
            return Some(Target::PanelFocus(Panel::Command));
        }
    }
    // The profile menu owns the surface while open (ADR-0037): its rows are
    // click-to-activate; everything else inside the frame is inert, and the
    // covered panels beneath are not clickable.
    if vm.mode == crate::input::Mode::ProfileMenu {
        return hit_profile_menu(vm, size, col, row);
    }
    // The colour picker owns the surface while open (ADR-0039): a row click
    // selects through the same modal geometry the renderer draws.
    if vm.mode == crate::input::Mode::ColourPicker {
        return hit_colour_picker(vm, size, col, row);
    }
    // Panel-editor mode owns the full grid's clicks: grips pick up units, dividers
    // resize (ADR-0034). Editing is Full3Col-only; the narrow tiers stay normal.
    if vm.mode == crate::input::Mode::PanelEditor && choose_mode(size) == LayoutMode::Full3Col {
        return hit_editor(vm, body, col, row);
    }
    match choose_mode(size) {
        // The full grid is data-driven (ADR-0033): hit-test the *same*
        // `solve_model` output the renderer draws, so pixels equal targets for
        // any arrangement. The degraded tiers keep the default role layout.
        LayoutMode::Full3Col => hit_model(
            vm,
            &crate::layout_model::solve_model(&vm.layout, body),
            col,
            row,
        ),
        LayoutMode::TwoCol => hit_grid(vm, two_col(body), col, row),
        LayoutMode::SinglePanel => hit_single(vm, body, col, row),
        LayoutMode::TooSmall => None,
    }
}

/// Hit-test a mixed column (post cross-column move, ADR-0034): resolve the click
/// to the panel of the unit under it — the same `solve_column` geometry the
/// renderer uses. Coarse focus; per-toggle precision in a mixed column is not a
/// first-cut requirement.
fn hit_mixed(units: &[crate::layout_model::Unit], rect: Rect, col: u16, row: u16) -> Target {
    use crate::layout_model::Unit;
    let (slots, _) = crate::layout_model::solve_column(units, rect, 0);
    for slot in &slots {
        if contains(slot.rect, col, row) {
            return match slot.unit {
                Unit::Rail => Target::PanelFocus(Panel::Projects),
                Unit::Card(_) => Target::PanelFocus(Panel::Detail),
                Unit::Section(id) => Target::Section(id),
            };
        }
    }
    Target::PanelFocus(Panel::Detail)
}

/// Hit-test the panel editor (ADR-0034): a column divider (right border), then a
/// unit grip (its top border row), computed from the same `solve_model` /
/// `solve_column` the grip renderer uses — so a grip's drawn cells and its click
/// region are the same (pixels == targets). A click elsewhere is inert.
fn hit_editor(vm: &ViewModel, body: Rect, col: u16, row: u16) -> Option<Target> {
    let sl = crate::layout_model::solve_model(&vm.layout, body);
    if contains(sl.command, col, row) {
        return Some(Target::CommandBar);
    }
    for (ci, scol) in sl.columns.iter().enumerate() {
        // The right divider (shared border) resizes; checked before the body so a
        // click on the seam grabs the divider, not the column.
        if ci + 1 < sl.columns.len() && scol.rect.w > 0 {
            let x = scol.rect.x + scol.rect.w - 1;
            if col == x && row >= scol.rect.y && row < scol.rect.y.saturating_add(scol.rect.h) {
                return Some(Target::ColumnDivider(ci));
            }
        }
        if !contains(scol.rect, col, row) {
            continue;
        }
        let (slots, _) = crate::layout_model::solve_column(&scol.units, scol.rect, 0);
        for (ui, slot) in slots.iter().enumerate() {
            // The grip is the unit's whole top border row (generous + usable).
            if row == slot.rect.y
                && col >= slot.rect.x
                && col < slot.rect.x.saturating_add(slot.rect.w)
            {
                return Some(Target::GripBar(crate::editor::Cursor { col: ci, unit: ui }));
            }
        }
        return Some(Target::PanelFocus(Panel::Detail));
    }
    None
}

/// Hit-test the open profile menu (ADR-0037): a browse-stage click on a listed
/// row activates it (the mouse equivalent of moving the cursor and pressing
/// `Enter`); the rest of the frame is inert; outside the frame nothing is
/// clickable. The row geometry is [`crate::view::profile_menu_row_rect`] — the
/// same cells the renderer paints.
fn hit_profile_menu(
    vm: &ViewModel,
    size: crate::render::frame::Size,
    col: u16,
    row: u16,
) -> Option<Target> {
    let rect = crate::view::modal_rect(size);
    if rect.w < 8 || rect.h < 3 || !contains(rect, col, row) {
        return None;
    }
    // The close `[x]`, on every stage (ADR-0046 §2) — the mode's own close.
    if let Some(cr) = crate::view::modal_close_rect(rect) {
        if contains(cr, col, row) {
            return Some(Target::ProfileMenuClose);
        }
    }
    match &vm.profile_menu.stage {
        crate::profile_menu::MenuStage::Browse => {
            for i in 0..vm.profiles.len() {
                // The chip outranks the row it sits on (it is inside it).
                let chip = crate::view::profile_menu_settings_rect(rect, i);
                if chip.w > 0 && contains(chip, col, row) {
                    return Some(Target::ProfileSettings(i));
                }
                let rr = crate::view::profile_menu_row_rect(rect, i);
                if rr.h > 0 && contains(rr, col, row) {
                    return Some(Target::ProfileRow(i));
                }
            }
            let new_rr = crate::view::profile_menu_row_rect(rect, vm.profiles.len());
            if new_rr.h > 0 && contains(new_rr, col, row) {
                return Some(Target::ProfileNew);
            }
        }
        crate::profile_menu::MenuStage::Settings { .. } => {
            for i in 0..crate::profile_menu::SettingsRow::ALL.len() {
                let rr = crate::view::profile_menu_row_rect(rect, i);
                if rr.h > 0 && contains(rr, col, row) {
                    return Some(Target::SettingsRow(i));
                }
            }
        }
        _ => {}
    }
    Some(Target::OverlayBody)
}

/// Hit-test the open colour picker (ADR-0039): a click on a listed row (a slot,
/// a preset, or the reset row) selects it — the mouse twin of moving the cursor
/// and pressing `Enter`. The rest of the frame is inert; outside it, nothing.
fn hit_colour_picker(
    vm: &ViewModel,
    size: crate::render::frame::Size,
    col: u16,
    row: u16,
) -> Option<Target> {
    let rect = crate::view::modal_rect(size);
    if rect.w < 8 || rect.h < 3 || !contains(rect, col, row) {
        return None;
    }
    for i in 0..crate::view::colour_picker_rows(vm.colour_picker.stage) {
        let rr = crate::view::colour_picker_row_rect(rect, i);
        if rr.h > 0 && contains(rr, col, row) {
            return Some(Target::PickerRow(i));
        }
    }
    Some(Target::OverlayBody)
}

/// Hit-test the data-driven full grid: the command row, then each physical
/// column by its role — the same `solve_model`/role dispatch `view::draw_full`
/// uses, so a click resolves to exactly the element drawn (ADR-0024 §6/0033).
fn hit_model(
    vm: &ViewModel,
    sl: &crate::layout_model::SolvedLayout,
    col: u16,
    row: u16,
) -> Option<Target> {
    if contains(sl.command, col, row) {
        return Some(Target::CommandBar);
    }
    for scol in &sl.columns {
        if !contains(scol.rect, col, row) {
            continue;
        }
        use crate::layout_model::ColumnRole;
        return match scol.role() {
            ColumnRole::Rail => Some(project_or_focus(vm, scol.rect, col, row)),
            ColumnRole::Cards => Some(toggle_or_focus(vm, &scol.card_kinds(), scol.rect, col, row)),
            ColumnRole::Sections => Some(section_or_focus(
                &scol.section_ids(),
                scol.rect,
                vm.graphs_scroll,
                col,
                row,
            )),
            // A mixed column (post cross-column move): resolve to the panel the
            // unit under the click belongs to (coarse focus; per-toggle precision
            // in a mixed column is not needed for the first cut).
            ColumnRole::Mixed => Some(hit_mixed(&scol.units, scol.rect, col, row)),
            ColumnRole::Empty => None,
        };
    }
    None
}

/// Resolve a click while an overlay owns the surface (ADR-0026/0027/0029/0031):
/// the close X, the confirm/cancel buttons, a message's primary action, the
/// modal-body scrollbar, else the (scrollable) body — the same cells
/// `view::draw_overlay` paints. A click outside the modal is inert (`None`).
fn hit_overlay(
    overlay: &crate::dispatch::Overlay,
    size: crate::render::frame::Size,
    col: u16,
    row: u16,
) -> Option<Target> {
    use crate::dispatch::{Overlay, OverlayControl};
    let rect = crate::view::modal_rect(size);
    if rect.w < 8 || rect.h < 3 || !contains(rect, col, row) {
        return None;
    }
    // The close X, top-right, for modals that carry a Close control.
    if overlay.controls().contains(&OverlayControl::Close) {
        if let Some(cr) = crate::view::modal_close_rect(rect) {
            if contains(cr, col, row) {
                return Some(Target::OverlayClose);
            }
        }
    }
    let inner = inner_of(rect);
    if let Overlay::Confirm { .. } = overlay {
        // The buttons row mirrors `view::draw_overlay`: inner dy=3.
        if row == inner.y.saturating_add(3) {
            let (confirm, cancel) = crate::view::CONFIRM_BUTTONS;
            let confirm_w = u16::try_from(confirm.chars().count()).unwrap_or(0);
            let cancel_w = u16::try_from(cancel.chars().count()).unwrap_or(0);
            if col >= inner.x && col < inner.x.saturating_add(confirm_w) {
                return Some(Target::OverlayConfirm);
            }
            let kx = inner.x.saturating_add(confirm_w).saturating_add(3);
            if col >= kx && col < kx.saturating_add(cancel_w) {
                return Some(Target::OverlayCancel);
            }
        }
    }
    // A message modal's primary action (the wizard's *add project*).
    if let Overlay::Alert(m) = overlay {
        if let Some(pr) = crate::view::modal_primary_rect(inner, m.action) {
            if contains(pr, col, row) {
                return Some(Target::WizardAdd);
            }
        }
    }
    // The modal-body scrollbar (a long Results listing overflows the box).
    if let Overlay::Results {
        lines,
        scroll,
        footer,
        ..
    } = overlay
    {
        let body = Rect {
            h: inner.h.saturating_sub(u16::from(footer.is_some())),
            ..inner
        };
        if on_scrollbar(body, *scroll, lines.len(), col, row) {
            return Some(Target::Scrollbar(ScrollRegion::Modal));
        }
    }
    Some(Target::OverlayBody)
}

/// Hit-test one of the multi-column grids (full or two-column).
fn hit_grid(vm: &ViewModel, r: PanelRects, col: u16, row: u16) -> Option<Target> {
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
            r.col3,
            vm.graphs_scroll,
            col,
            row,
        ));
    }
    None
}

/// Hit-test the single-panel layout: a tab strip over one focused panel.
fn hit_single(vm: &ViewModel, body: Rect, col: u16, row: u16) -> Option<Target> {
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
            Panel::Graphs => {
                section_or_focus(&col3::SectionId::ALL, panel, vm.graphs_scroll, col, row)
            }
            Panel::Projects | Panel::Command => project_or_focus(vm, panel, col, row),
        });
    }
    None
}

/// The tab under `col` in the `[Projects] Detail Graphs` strip — mapped by
/// horizontal thirds (exact glyph widths aren't needed for a coarse tab click).
fn tab_at(col: u16, tabs: Rect) -> Panel {
    let rel = col.saturating_sub(tabs.x);
    let third = (tabs.w / 3).max(1);
    if rel < third {
        Panel::Projects
    } else if rel < third.saturating_mul(2) {
        Panel::Detail
    } else {
        Panel::Graphs
    }
}

/// A click in column 1 → the project row under it, else focus the panel.
fn project_or_focus(vm: &ViewModel, rect: Rect, col: u16, row: u16) -> Target {
    let inner = inner_of(rect);
    let (list, buttons) = crate::view::rail_action_layout(inner, !vm.projects.is_empty());
    // The bottom action bar (M56-05) — add / reorder / remove — takes priority
    // over row selection; its chips are the same cells the renderer drew.
    for (r, target) in &buttons {
        if contains(*r, col, row) {
            return target.clone();
        }
    }
    if !vm.projects.is_empty() {
        let (start, end) = visible_window(vm.selected, vm.projects.len(), usize::from(list.h));
        // The rail overflow scrollbar on the panel's right border (ADR-0031).
        let bar_area = Rect {
            x: rect.x,
            y: list.y,
            w: rect.w,
            h: list.h,
        };
        if on_scrollbar(bar_area, start, vm.projects.len(), col, row) {
            return Target::Scrollbar(ScrollRegion::Rail);
        }
        if let Some(dy) = local_row(list, col, row) {
            let idx = start + usize::from(dy);
            if idx < end {
                return Target::ProjectRow(idx);
            }
        }
    }
    Target::PanelFocus(Panel::Projects)
}

/// A click in column 2 → a feature-toggle row, the editor button, else focus
/// the panel. Recomputes the card stack from the same solver the renderer uses,
/// so a click resolves to the exact card/row/button drawn (M55-08/10).
fn toggle_or_focus(vm: &ViewModel, units: &[CardKind], rect: Rect, col: u16, row: u16) -> Target {
    // The card-stack overflow scrollbar (ADR-0031) on the column's right edge.
    if on_scrollbar(rect, vm.detail_scroll, cards::content_len(units), col, row) {
        return Target::Scrollbar(ScrollRegion::Detail);
    }
    let Some(detail) = vm.detail.as_deref() else {
        return Target::PanelFocus(Panel::Detail);
    };
    // An uninitialized project fills column 2 with just the repository card (no
    // toggles, no button) — mirrors `cards::draw`'s early-out.
    if matches!(detail.status, ProjectStatus::NotInitialized) {
        return Target::PanelFocus(Panel::Detail);
    }
    let ctx = cards::CardCtx {
        detail,
        activity: &vm.activity,
        runtime: &vm.feature_runtime,
        toggle_cursor: vm.toggle_cursor,
        focused: vm.focus == Panel::Detail,
        unicode: false, // geometry only; glyph choice never moves a cell
        pressed: None,  // geometry only; the flash never moves a cell
        press_pulse: 0.0,
    };
    for slot in cards::solve_scrolled(units, rect, vm.detail_scroll).0 {
        if !contains(slot.rect, col, row) {
            continue;
        }
        // Only the Features card has clickable sub-regions (retry chip + toggle
        // rows); every other card focuses the detail panel on a plain click.
        if slot.kind == CardKind::Features {
            // The retry chip is a sub-region of a Failed row, so it wins
            // over the row toggle (checked first).
            if let Some(i) = cards::feature_retry_at(&ctx, slot.rect, slot.collapsed, col, row) {
                return Target::RetryRow(i);
            }
            if let Some(i) = cards::feature_toggle_at(&ctx, slot.rect, slot.collapsed, col, row) {
                return Target::ToggleRow(i);
            }
        }
        break; // clicked inside this card but not a specific target → focus
    }
    Target::PanelFocus(Panel::Detail)
}

/// Whether `(col, row)` lands on the overflow scrollbar for a region of `area`
/// with `(offset, content_len)` — the same `chrome::scrollbar` geometry the
/// renderer draws, so the bar's cells and its hit-region are identical (ADR-0031).
fn on_scrollbar(area: Rect, offset: usize, content_len: usize, col: u16, row: u16) -> bool {
    crate::chrome::scrollbar(area, offset, content_len)
        .is_some_and(|bar| contains(bar.track, col, row))
}

/// A click in column 3 → the graph section under it, else focus the panel. When
/// the panels overflow (ADR-0031), the viewport row maps through the scroll
/// `offset` into the fully-expanded stack — the same window the renderer blits —
/// so a click resolves to the panel actually on screen.
fn section_or_focus(
    ids: &[col3::SectionId],
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
    if let Some(dy) = local_row(inner, col, row) {
        let content_h = col3::content_rows(ids);
        let (solve_h, content_row) = if content_h <= inner.h {
            (inner.h, dy)
        } else {
            let off = offset.min(usize::from(content_h.saturating_sub(inner.h)));
            (
                content_h,
                dy.saturating_add(u16::try_from(off).unwrap_or(0)),
            )
        };
        for slot in col3::solve_stack(ids, solve_h) {
            if content_row >= slot.y && content_row < slot.y.saturating_add(slot.rows) {
                return Target::Section(slot.id);
            }
        }
    }
    Target::PanelFocus(Panel::Graphs)
}

/// The content rect inside a bordered panel (mirrors [`crate::Frame::border`]:
/// a one-cell ring, so content is inset by 1). Panels too small to border keep
/// their whole area.
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

/// The 0-based row offset of `(col, row)` within `inner`, or `None` when the
/// point is outside it.
fn local_row(inner: Rect, col: u16, row: u16) -> Option<u16> {
    if !contains(inner, col, row) {
        return None;
    }
    Some(row - inner.y)
}

/// Whether `(x, y)` lies within `r` (right/bottom exclusive).
fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::solve;
    use crate::render::frame::Size;
    use crate::state::AppState;
    use crate::{EntryKind, Project, ProjectStatus};

    fn project(name: &str) -> Project {
        Project {
            name: name.into(),
            path: format!("/r/{name}"),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }
    }

    /// A ready dashboard view-model at a full-grid size.
    fn vm(w: u16, h: u16) -> ViewModel {
        let mut s = AppState::new(
            vec![project("a"), project("b"), project("c")],
            Size::new(w, h),
        );
        s.app.splash.skip();
        s.snapshot()
    }

    /// A ready view-model in panel-editor mode at a full-grid size.
    fn editor_vm(w: u16, h: u16) -> ViewModel {
        let mut s = AppState::new(
            vec![project("a"), project("b"), project("c")],
            Size::new(w, h),
        );
        s.app.splash.skip();
        crate::input::enter_panel_editor(&mut s.app);
        s.snapshot()
    }

    #[test]
    fn panel_editor_grips_and_dividers_resolve_where_drawn() {
        // In edit mode, every unit's top border row grips and the shared column
        // borders resolve to dividers — computed from the same solve_model the
        // grip renderer uses (pixels == targets, ADR-0034).
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
    fn title_row_and_offgrid_are_none() {
        let v = vm(120, 40);
        assert_eq!(hit_test(&v, 5, 0), None, "the header's left side is inert");
        assert_eq!(hit_test(&v, 999, 5), None, "off-grid is None");
        assert_eq!(hit_test(&v, 5, 999), None);
    }

    #[test]
    fn header_actions_resolve_on_row_zero() {
        let v = vm(120, 40);
        let spans = crate::bars::header_action_spans(120, None);
        assert_eq!(spans.len(), 4);
        let expect = [
            Target::HeaderHelp,
            Target::HeaderPalette,
            Target::HeaderEditor,
            Target::HeaderQuit,
        ];
        for (span, want) in spans.iter().zip(expect) {
            assert_eq!(hit_test(&v, span.start, 0), Some(want.clone()));
        }
        // Between the spans (the gap) row 0 stays inert.
        let gap = spans[0].end;
        assert_eq!(hit_test(&v, gap, 0), None);
    }

    #[test]
    fn header_actions_are_inert_while_an_overlay_is_open() {
        let mut s = AppState::new(vec![project("a")], Size::new(120, 40));
        s.app.splash.skip();
        s.app.overlay = Some(crate::dispatch::Overlay::Running { title: "x".into() });
        let v = s.snapshot();
        let spans = crate::bars::header_action_spans(120, None);
        assert_eq!(
            hit_test(&v, spans[0].start, 0),
            None,
            "the overlay owns the surface"
        );
    }

    #[test]
    fn too_small_yields_no_targets() {
        let v = vm(10, 4);
        assert_eq!(hit_test(&v, 2, 2), None);
    }

    /// A ready state with the profile plane wired (active = `pf0`).
    fn profiled_state(w: u16, h: u16) -> AppState {
        let mut s = AppState::new(vec![project("a")], Size::new(w, h));
        s.app.splash.skip();
        s.version = "0.1.0".into();
        s.app.set_profiles(
            ["default", "work"]
                .iter()
                .enumerate()
                .map(|(i, name)| crate::profile::ProfileEntry {
                    profile_id: format!("pf{i}"),
                    name: (*name).to_string(),
                    created_at: i64::try_from(i).unwrap(),
                })
                .collect(),
            "pf0".into(),
        );
        s
    }

    #[test]
    fn identity_button_resolves_on_row_zero_and_goes_inert_under_an_overlay() {
        let mut s = profiled_state(120, 40);
        let v = s.snapshot();
        let span = crate::bars::header_cluster(120, v.profile_name.as_deref())
            .into_iter()
            .find(|i| i.span.action == crate::bars::HeaderAction::Profile)
            .map(|i| i.span)
            .unwrap();
        assert_eq!(hit_test(&v, span.start, 0), Some(Target::HeaderProfile));
        assert_eq!(hit_test(&v, span.end - 1, 0), Some(Target::HeaderProfile));
        assert_eq!(hit_test(&v, span.end, 0), None, "one past the span misses");
        // Inert while an overlay owns the surface (ADR-0037 §1).
        s.app.overlay = Some(crate::dispatch::Overlay::Running { title: "x".into() });
        let v = s.snapshot();
        assert_eq!(hit_test(&v, span.start, 0), None);
    }

    #[test]
    fn settings_chips_and_new_button_hit_their_drawn_cells() {
        // ADR-0046: every hub act has a click twin — the chips, the
        // `[ + new profile ]` row, and the close X on EVERY stage.
        let mut s = profiled_state(120, 40);
        crate::input::enter_profile_menu(&mut s.app);
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        for i in 0..2usize {
            let chip = crate::view::profile_menu_settings_rect(modal, i);
            assert!(chip.w > 0, "the chip renders at this width");
            assert_eq!(
                hit_test(&v, chip.x, chip.y),
                Some(Target::ProfileSettings(i))
            );
            // The rest of the row still switches.
            let rr = crate::view::profile_menu_row_rect(modal, i);
            assert_eq!(hit_test(&v, rr.x, rr.y), Some(Target::ProfileRow(i)));
        }
        let new_rr = crate::view::profile_menu_row_rect(modal, 2);
        assert_eq!(hit_test(&v, new_rr.x, new_rr.y), Some(Target::ProfileNew));
        let cr = crate::view::modal_close_rect(modal).unwrap();
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::ProfileMenuClose));
        // The settings stage: rows resolve, and the X still closes.
        s.app.profile_menu.stage = crate::profile_menu::MenuStage::Settings {
            profile: "pf0".into(),
        };
        let v = s.snapshot();
        for i in 0..crate::profile_menu::SettingsRow::ALL.len() {
            let rr = crate::view::profile_menu_row_rect(modal, i);
            assert_eq!(hit_test(&v, rr.x, rr.y), Some(Target::SettingsRow(i)));
        }
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::ProfileMenuClose));
    }

    #[test]
    fn profile_menu_rows_hit_their_drawn_cells_and_the_body_is_inert() {
        let mut s = profiled_state(120, 40);
        crate::input::enter_profile_menu(&mut s.app);
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        for i in 0..2usize {
            let rr = crate::view::profile_menu_row_rect(modal, i);
            assert!(rr.h > 0);
            assert_eq!(hit_test(&v, rr.x, rr.y), Some(Target::ProfileRow(i)));
            assert_eq!(
                hit_test(&v, rr.x + rr.w - 1, rr.y),
                Some(Target::ProfileRow(i))
            );
        }
        // Inside the frame but off the rows: inert body. Outside: nothing.
        assert_eq!(
            hit_test(&v, modal.x + 1, modal.y + modal.h - 2),
            Some(Target::OverlayBody)
        );
        assert_eq!(hit_test(&v, modal.x.saturating_sub(2), modal.y + 2), None);
    }

    #[test]
    fn colour_picker_rows_hit_their_drawn_cells() {
        let mut s = profiled_state(120, 40);
        crate::input::enter_profile_menu(&mut s.app);
        s.app.mode = crate::input::Mode::ColourPicker;
        s.app.colour_picker.reset();
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        // Slots stage: five rows.
        for i in 0..crate::theme::ThemeSlot::ALL.len() {
            let rr = crate::view::colour_picker_row_rect(modal, i);
            assert!(rr.h > 0);
            assert_eq!(hit_test(&v, rr.x, rr.y), Some(Target::PickerRow(i)));
        }
        // Presets stage: eleven rows (ten presets + reset).
        s.app.colour_picker.stage = crate::colour_picker::PickerStage::Presets;
        let v = s.snapshot();
        for i in 0..=crate::colour_picker::RESET_ROW {
            let rr = crate::view::colour_picker_row_rect(modal, i);
            if rr.h == 0 {
                continue; // a short frame clips trailing rows whole
            }
            assert_eq!(
                hit_test(&v, rr.x + rr.w - 1, rr.y),
                Some(Target::PickerRow(i))
            );
        }
        // Off the rows: an inert body. Outside the frame: nothing.
        assert_eq!(
            hit_test(&v, modal.x + 1, modal.y + modal.h - 2),
            Some(Target::OverlayBody)
        );
        assert_eq!(hit_test(&v, modal.x.saturating_sub(2), modal.y + 2), None);
    }

    #[test]
    fn profile_menu_rows_go_inert_outside_browse() {
        let mut s = profiled_state(120, 40);
        crate::input::enter_profile_menu(&mut s.app);
        s.app.profile_menu.stage = crate::profile_menu::MenuStage::NamingCreate;
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        let rr = crate::view::profile_menu_row_rect(modal, 0);
        assert_eq!(
            hit_test(&v, rr.x, rr.y),
            Some(Target::OverlayBody),
            "naming owns the input; rows are not switch targets"
        );
    }

    #[test]
    fn clicking_a_project_row_resolves_to_its_index() {
        let v = vm(120, 40);
        // col1 inner content begins at (1, 2): border at x=0,y=1; inset by 1.
        assert_eq!(hit_test(&v, 3, 2), Some(Target::ProjectRow(0)));
        assert_eq!(hit_test(&v, 3, 3), Some(Target::ProjectRow(1)));
        assert_eq!(hit_test(&v, 3, 4), Some(Target::ProjectRow(2)));
    }

    #[test]
    fn clicking_empty_col1_focuses_projects() {
        let v = vm(120, 40);
        // A row well below the three project rows is still inside col1 → focus.
        assert_eq!(
            hit_test(&v, 3, 20),
            Some(Target::PanelFocus(Panel::Projects))
        );
    }

    #[test]
    fn clicking_the_graphs_column_hits_a_section_or_focus() {
        let v = vm(120, 40);
        // col3 starts at x = col1.w + col2.w; sample a column well inside it.
        let hit = hit_test(&v, 100, 6);
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
    fn detail_column_focuses_or_hits_a_toggle() {
        let v = vm(120, 40);
        // col2 begins after col1; a mid-column click resolves to Detail focus or
        // a toggle row (the thin fallback detail has no toggles → focus).
        let hit = hit_test(&v, 40, 10);
        assert!(
            matches!(
                hit,
                Some(Target::ToggleRow(_) | Target::PanelFocus(Panel::Detail))
            ),
            "got {hit:?}"
        );
    }

    #[test]
    fn overlay_owns_its_region_and_buttons_resolve() {
        let mut s = AppState::new(vec![project("a")], Size::new(100, 32));
        s.app.splash.skip();
        s.app.overlay = Some(crate::dispatch::Overlay::Confirm {
            request: crate::dispatch::DispatchRequest::Rebuild,
            repo: "/r/a".into(),
        });
        let v = s.snapshot();
        // The modal is a centered box (ADR-0026); its geometry comes from the
        // one shared `modal_rect(size)` the renderer also uses.
        let size = Size::new(100, 32);
        let rect = crate::view::modal_rect(size);
        let inner = inner_of(rect);
        let button_row = inner.y + 3;
        // The Confirm button's first cell.
        assert_eq!(
            hit_test(&v, inner.x, button_row),
            Some(Target::OverlayConfirm)
        );
        // The Cancel button (after the confirm label + the 3-cell gap).
        let confirm_w = u16::try_from(crate::view::CONFIRM_BUTTONS.0.chars().count()).unwrap();
        assert_eq!(
            hit_test(&v, inner.x + confirm_w + 3, button_row),
            Some(Target::OverlayCancel)
        );
        // Any other overlay cell is the body.
        assert_eq!(
            hit_test(&v, inner.x + 1, inner.y),
            Some(Target::OverlayBody)
        );
        // Outside the modal nothing is clickable while it is up (the scrim
        // click is inert): a corner well outside the centered box.
        assert_eq!(hit_test(&v, 1, size.h - 1), None);
    }

    #[test]
    fn results_overlay_has_no_buttons() {
        let mut s = AppState::new(vec![project("a")], Size::new(100, 32));
        s.app.splash.skip();
        s.app.overlay = Some(crate::dispatch::Overlay::Results {
            title: "rift status".into(),
            lines: vec!["ok".into()],
            scroll: 0,
            footer: None,
            truncated: false,
        });
        let v = s.snapshot();
        let inner = inner_of(crate::view::modal_rect(Size::new(100, 32)));
        assert_eq!(
            hit_test(&v, inner.x, inner.y + 3),
            Some(Target::OverlayBody),
            "no confirm target on a results overlay"
        );
    }

    #[test]
    fn the_close_x_is_clickable_and_pixels_equal_targets() {
        let mut s = AppState::new(vec![project("a")], Size::new(100, 32));
        s.app.splash.skip();
        s.app.open_overlay(crate::dispatch::Overlay::Confirm {
            request: crate::dispatch::DispatchRequest::Rebuild,
            repo: "/r/a".into(),
        });
        let v = s.snapshot();
        let rect = crate::view::modal_rect(Size::new(100, 32));
        let cr = crate::view::modal_close_rect(rect).expect("wide modal has a close X");
        // Every cell the X is drawn into resolves to OverlayClose.
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::OverlayClose));
        assert_eq!(
            hit_test(&v, cr.x + cr.w - 1, cr.y),
            Some(Target::OverlayClose)
        );
        // Just left of the X is the modal body, not the close.
        assert_eq!(hit_test(&v, cr.x - 1, cr.y), Some(Target::OverlayBody));
    }

    #[test]
    fn the_wizard_primary_button_resolves_where_it_is_drawn() {
        // The empty registry raises the wizard modal; its *add project* chip is
        // clickable and its drawn cells equal the hit region (ADR-0029).
        let mut s = AppState::new(vec![], Size::new(100, 32));
        s.app.splash.skip();
        assert!(s.app.showing_wizard());
        let v = s.snapshot();
        let inner = inner_of(crate::view::modal_rect(Size::new(100, 32)));
        let pr = crate::view::modal_primary_rect(inner, crate::dispatch::MessageAction::AddProject)
            .expect("the wizard has a primary button");
        // Both ends of the chip resolve to WizardAdd.
        assert_eq!(hit_test(&v, pr.x, pr.y), Some(Target::WizardAdd));
        assert_eq!(hit_test(&v, pr.x + pr.w - 1, pr.y), Some(Target::WizardAdd));
        // The close X is still reachable on the wizard (dismiss-only fallback).
        let cr =
            crate::view::modal_close_rect(crate::view::modal_rect(Size::new(100, 32))).unwrap();
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::OverlayClose));
    }

    #[test]
    fn a_running_modal_has_no_close_x() {
        let mut s = AppState::new(vec![project("a")], Size::new(100, 32));
        s.app.splash.skip();
        s.app
            .open_overlay(crate::dispatch::Overlay::Running { title: "x".into() });
        let v = s.snapshot();
        let rect = crate::view::modal_rect(Size::new(100, 32));
        let cr = crate::view::modal_close_rect(rect).unwrap();
        // Running carries no Close control, so that cell is inert body, not close.
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::OverlayBody));
    }

    #[test]
    fn rail_action_bar_buttons_are_clickable() {
        let mut s = AppState::new(vec![project("a"), project("b")], Size::new(120, 40));
        s.app.splash.skip();
        let v = s.snapshot();
        // Recompute the rail inner exactly as hit_test does (Full3Col).
        let body = Rect {
            x: 0,
            y: 1,
            w: 120,
            h: 39,
        };
        let col1 = solve(body).col1;
        let inner = inner_of(col1);
        let (_list, buttons) = crate::view::rail_action_layout(inner, true);
        assert!(!buttons.is_empty(), "the rail has an action bar");
        // Every chip cell resolves to its target (pixels == targets).
        for (r, target) in &buttons {
            assert_eq!(hit_test(&v, r.x, r.y), Some(target.clone()));
        }
        assert_eq!(buttons[0].1, Target::RailAdd, "the first chip is Add");
    }

    #[test]
    fn the_graphs_scrollbar_resolves_where_it_is_drawn() {
        // A short Full3Col column overflows its expanded panels, so a scrollbar
        // renders — every track cell resolves to the graphs region (ADR-0031).
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
        // panel, not a scrollbar target.
        let size = Size::new(100, 40);
        let mut s = AppState::new(vec![project("a")], size);
        s.app.splash.skip();
        s.size = size;
        let v = s.snapshot();
        let body = Rect {
            x: 0,
            y: 1,
            w: 100,
            h: 39,
        };
        let col3 = solve(body).col3;
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

    proptest::proptest! {
        /// Hit-testing is total and panic-free for any coordinate and size.
        #[test]
        fn hit_test_never_panics(w in 0u16..300, h in 0u16..120, col in 0u16..300, row in 0u16..120) {
            let mut s = AppState::new(vec![project("a")], Size::new(w, h));
            s.app.splash.skip();
            let v = s.snapshot();
            let _ = hit_test(&v, col, row);
        }
    }
}
