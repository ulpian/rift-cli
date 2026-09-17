//! Mouse hit-testing: a click coordinate → the dashboard element under it.
//!
//! [`hit_test`] is a **pure** function of the render [`ViewModel`], recomputing
//! geometry from the *same* layout solvers the renderer uses ([`crate::layout`],
//! [`crate::col3::solve_stack`], `crate::view::visible_window`,
//! `crate::view::detail_line_kinds`) — so a click always resolves to the exact
//! element that was drawn, with no stored/mutable geometry and no resize race.
//! It is engine-free (no `xre_*`), so it compiles and tests without the `xre`
//! feature, exactly like the keyboard reducer it feeds.

use crate::cards::{self, CardKind};
use crate::col3;
use crate::layout::{choose_mode, single_panel, two_col, LayoutMode, PanelRects, Rect};
use crate::state::ViewModel;
use crate::view::visible_window;
use crate::Panel;

/// A scrollable region that can carry a scrollbar. Identifies which
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
    /// Recall Viewer checkpoint catalogue.
    ViewerCheckpoints,
    /// Recall Viewer selected-checkpoint timeline.
    ViewerTimeline,
}

fn hit_recall_viewer(vm: &ViewModel, col: u16, row: u16) -> Option<Target> {
    use crate::recall_viewer::{geometry, timeline_line_count, ViewerPane, CHECKPOINT_ROW_HEIGHT};
    let geometry = geometry(vm.size, vm.viewer.pane);
    if contains(geometry.command, col, row) {
        return Some(Target::CommandBar);
    }
    if geometry.tabs.h > 0 && contains(geometry.tabs, col, row) {
        let rel = col.saturating_sub(geometry.tabs.x);
        let third = (geometry.tabs.w / 3).max(1);
        let pane = if rel < third {
            ViewerPane::Projects
        } else if rel < third.saturating_mul(2) {
            ViewerPane::Checkpoints
        } else {
            ViewerPane::Timeline
        };
        return Some(Target::ViewerTab(pane));
    }
    if geometry.projects.w > 0 && contains(geometry.projects, col, row) {
        let inner = inner_of(geometry.projects);
        if !vm.projects.is_empty() {
            let (start, end) = visible_window(vm.selected, vm.projects.len(), usize::from(inner.h));
            if on_scrollbar(geometry.projects, start, vm.projects.len(), col, row) {
                return Some(Target::Scrollbar(ScrollRegion::Rail));
            }
            if let Some(dy) = local_row(inner, col, row) {
                let index = start + usize::from(dy);
                if index < end {
                    return Some(Target::ProjectRow(index));
                }
            }
        }
        return Some(Target::PanelFocus(Panel::Projects));
    }
    if geometry.checkpoints.w > 0 && contains(geometry.checkpoints, col, row) {
        let inner = inner_of(geometry.checkpoints);
        if row == inner.y {
            return Some(Target::ViewerFilter);
        }
        let header_rows = if vm.viewer.filter_progress.is_some() || vm.viewer.new_count > 0 {
            2
        } else {
            1
        };
        if vm.viewer.new_count > 0 && row == inner.y.saturating_add(1) {
            return Some(Target::ViewerLatest);
        }
        let list = Rect {
            x: inner.x,
            y: inner.y.saturating_add(header_rows),
            w: inner.w,
            h: inner.h.saturating_sub(header_rows),
        };
        if on_scrollbar(
            geometry.checkpoints,
            vm.viewer.checkpoint_scroll,
            vm.viewer.visible.len(),
            col,
            row,
        ) {
            return Some(Target::Scrollbar(ScrollRegion::ViewerCheckpoints));
        }
        if let Some(dy) = local_row(list, col, row) {
            let row_offset = usize::from(dy / CHECKPOINT_ROW_HEIGHT);
            let index = vm.viewer.checkpoint_scroll.saturating_add(row_offset);
            if index < vm.viewer.visible.len() {
                return Some(Target::ViewerCheckpointRow(index));
            }
        }
        return Some(Target::PanelFocus(Panel::Detail));
    }
    if geometry.timeline.w > 0 && contains(geometry.timeline, col, row) {
        let controls = crate::recall_viewer::timeline_controls(geometry.timeline);
        if contains(controls.full_session, col, row) {
            return Some(Target::ViewerFullSession);
        }
        if contains(controls.dashboard, col, row) {
            return Some(Target::ViewerDashboard);
        }
        let line_count = timeline_line_count(&vm.viewer);
        if on_scrollbar(
            controls.content,
            vm.viewer.timeline_scroll,
            line_count,
            col,
            row,
        ) {
            return Some(Target::Scrollbar(ScrollRegion::ViewerTimeline));
        }
        if let Some(dy) = local_row(controls.content, col, row) {
            let line = vm.viewer.timeline_scroll.saturating_add(usize::from(dy));
            if line < line_count {
                return Some(Target::ViewerTimelineLine(line));
            }
        }
        return Some(Target::PanelFocus(Panel::Graphs));
    }
    None
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
    /// The Repository card's Project Settings button.
    RepositorySettings,
    /// The Repository card's read-only Recall Viewer button.
    RepositoryRecallViewer,
    /// The Repository card's **adopt** act: the status row of an unadopted
    /// member (the family view). Present only while that row is drawn, so the
    /// click never offers an act the card is not showing.
    RepositoryAdopt,
    /// Recall Viewer Filter chip.
    ViewerFilter,
    /// Recall Viewer `Latest` action after a live refresh.
    ViewerLatest,
    /// Recall Viewer full compact-session toggle.
    ViewerFullSession,
    /// Recall Viewer return-to-dashboard chip.
    ViewerDashboard,
    /// Recall Viewer checkpoint catalogue row (visible index).
    ViewerCheckpointRow(usize),
    /// Recall Viewer flattened timeline line.
    ViewerTimelineLine(usize),
    /// Recall Viewer single-column tab.
    ViewerTab(crate::recall_viewer::ViewerPane),
    /// One right-aligned Project Settings checkbox/action chip.
    ProjectSettingsAction(usize),
    /// The Project Settings mode-modal close X.
    ProjectSettingsClose,
    /// Cancel the raw-transcript acknowledgement without closing the modal.
    ProjectSettingsCancel,
    /// A column-3 graph section.
    Section(col3::SectionId),
    /// The `[~$…]` USD-savings chip on the Recall headline: opens
    /// the read-only breakdown modal explaining how the estimate was
    /// composed. Only hit when an honest figure exists — no figure, no chip,
    /// no target.
    RecallCost,
    /// One `[24h]`/`[7d]`/`[30d]`/`[all]` period button on the Recall card:
    /// recompute the card's figures over that window. The pointer twin of the
    /// `1`–`4` keys, which are bound only while the graphs column has focus —
    /// so every button has a key and every key a button.
    RecallWindow(col3::RecallWindow),
    /// The `[tok]`/`[usd]` chip at the right of the Recall card's period row:
    /// flip the unit the read-vs-saved bars are read in. The pointer twin of
    /// `$`, bound only while the graphs column has focus.
    RecallUnitToggle,
    /// The `Σ …` total on the token-usage panel's title row (the family view):
    /// opens the read-only "by worktree" axis of exactly that number, in
    /// the existing `Overlay::Results`. The pointer twin of `w` in the graphs
    /// column. Only hit when a total exists and its title is not clipped —
    /// no total, no target.
    TokensByWorktree,
    /// The bottom command bar.
    CommandBar,
    /// The dispatch overlay's Confirm button (mouse Enter — confirm only).
    OverlayConfirm,
    /// The dispatch overlay's Cancel button (mouse Esc).
    OverlayCancel,
    /// The modal's close X — dismiss (mouse/Tab equivalent of Esc).
    OverlayClose,
    /// A message modal's primary action — the first-run wizard's *add a project*
    /// button (the message-routing rule): begins registering a project, like the `a` key.
    WizardAdd,
    /// The dispatch overlay body (wheel scroll / inert click).
    OverlayBody,
    /// A region's scrollbar: an overflow indicator today (a click is
    /// inert; the wheel scrolls the region under it). Drag/click-to-page is a
    /// later rung on this same track/thumb geometry.
    Scrollbar(ScrollRegion),
    /// A suggest-panel row: click INSERTS it (Tab's mouse equivalent — never
    /// execution); the wheel moves the highlight.
    SuggestRow(usize),
    /// The header's `?` action — open the palette help overlay.
    HeaderHelp,
    /// The header's `/` action — open the command palette.
    HeaderPalette,
    /// The header's `e` action — toggle the panel editor.
    HeaderEditor,
    /// The header's `q` action — quit the dashboard.
    HeaderQuit,
    /// The header's identity button — open/close the profile menu.
    HeaderProfile,
    /// A profile row in the open profile menu: click-to-activate (a click
    /// moves the cursor there and switches, exactly like `Enter`).
    ProfileRow(usize),
    /// A hub row's `[settings]` chip — opens that profile's
    /// settings page, the click twin of `s`.
    ProfileSettings(usize),
    /// The hub's `[ + new profile ]` row — opens the naming input, the click
    /// twin of `n`.
    ProfileNew,
    /// The profile menu's close `[x]`: closes the MODE — the
    /// pointer twin of Esc-from-Browse. Never `OverlayClose` (that clears an
    /// overlay, not a mode).
    ProfileMenuClose,
    /// A settings-page row (the profile hub): a click activates it, exactly like
    /// `Enter` on that row.
    SettingsRow(usize),
    /// A members-editor row: a click moves the cursor there (selection; the
    /// action chips act on the cursor row).
    MemberRow(usize),
    /// An add-stage candidate row: a click includes it, exactly like `Enter`.
    MemberAddRow(usize),
    /// The members editor's `[↑]` chip — reorder the cursor row up (⇧k).
    MemberUp,
    /// The members editor's `[↓]` chip — reorder the cursor row down (⇧j).
    MemberDown,
    /// The members editor's `[remove]` chip — remove the cursor row (`d`).
    MemberRemove,
    /// The members editor's `[ add ]` chip — open the add stage (`a`).
    MemberAddOpen,
    /// A row in the open colour picker: a click selects it through
    /// the shared modal hit-test, exactly like `Enter` on that row.
    PickerRow(usize),
    /// The colour picker's close `[x]`: closes the picker back to
    /// the profile menu — the pointer twin of Esc-to-the-end.
    PickerClose,
    /// A row in the open destination explorer, by **visible** index:
    /// a click moves the cursor there and, on a directory row, descends —
    /// exactly like cursoring + `Enter`. A file row only moves the cursor.
    ExplorerRow(usize),
    /// The explorer's `⬑ ..` back row — steps one level up, the click twin of
    /// Backspace. Inert at the filesystem root.
    ExplorerUp,
    /// The explorer's `[ select destination ]` chip — registers the folder
    /// currently open, the click twin of `s`.
    ExplorerSelect,
    /// The explorer's `[ input as path ]` chip — falls back to today's typed
    /// flow, the click twin of `i`.
    ExplorerTypePath,
    /// The explorer's `[.dotfiles]` chip — shows/hides dot-entries, the click
    /// twin of `.`. Right-aligned in the back row so it survives every width
    /// the surface draws at (modal-surface parity: a hinted act is always clickable).
    ExplorerDotfiles,
    /// The explorer's close `[x]`: closes the MODE — the pointer
    /// twin of Esc. Never `OverlayClose` (that clears an overlay, not a mode).
    ExplorerClose,
    /// A unit's grip in panel-editor mode: a press picks it up / drops
    /// it. Carries the unit's `(column, unit)` cursor so the drop knows the source.
    GripBar(crate::editor::Cursor),
    /// A column divider in panel-editor mode: a drag resizes it. Carries the index
    /// of the column to its left.
    ColumnDivider(usize),
}

impl Target {
    /// Whether this target is a clickable *button* that acknowledges a press
    /// with the flash. Header actions and the overlay
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
                | Target::RepositorySettings
                | Target::RepositoryRecallViewer
                | Target::RepositoryAdopt
                | Target::ViewerFilter
                | Target::ViewerLatest
                | Target::ViewerFullSession
                | Target::ViewerDashboard
                | Target::ProjectSettingsAction(_)
                | Target::ProjectSettingsClose
                | Target::ProjectSettingsCancel
                | Target::RecallCost
                | Target::RecallWindow(_)
                | Target::RecallUnitToggle
                | Target::TokensByWorktree
                | Target::ProfileSettings(_)
                | Target::ProfileNew
                | Target::ProfileMenuClose
                | Target::MemberUp
                | Target::MemberDown
                | Target::MemberRemove
                | Target::MemberAddOpen
                | Target::PickerClose
                | Target::ExplorerUp
                | Target::ExplorerSelect
                | Target::ExplorerTypePath
                | Target::ExplorerDotfiles
                | Target::ExplorerClose
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
        // The cluster solver (the profile hub) resolves every row-0 target — the
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
    // while open — a click inserts, exactly like Tab.
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
    if vm.mode == crate::input::Mode::ProjectSettings {
        return hit_project_settings(vm, size, col, row);
    }
    // The profile menu owns the surface while open: its rows are
    // click-to-activate; everything else inside the frame is inert, and the
    // covered panels beneath are not clickable.
    if vm.mode == crate::input::Mode::ProfileMenu {
        return hit_profile_menu(vm, size, col, row);
    }
    // The colour picker owns the surface while open: a row click
    // selects through the same modal geometry the renderer draws.
    if vm.mode == crate::input::Mode::ColourPicker {
        return hit_colour_picker(vm, size, col, row);
    }
    // The destination explorer owns the surface while open: rows
    // descend, the chips act, and the scrim outside the frame is inert.
    if vm.mode == crate::input::Mode::Explorer {
        return hit_explorer(vm, size, col, row);
    }
    // Panel-editor mode owns the full grid's clicks: grips pick up units, dividers
    // resize (panel-editor mode). Editing is Full3Col-only; the narrow tiers stay normal.
    if vm.mode == crate::input::Mode::PanelEditor && choose_mode(size) == LayoutMode::Full3Col {
        return hit_editor(vm, body, col, row);
    }
    if vm.surface == crate::recall_viewer::Surface::RecallViewer {
        return hit_recall_viewer(vm, col, row);
    }
    match choose_mode(size) {
        // The full grid is data-driven (the data-driven layout model): hit-test the *same*
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

fn hit_project_settings(
    vm: &ViewModel,
    size: crate::render::frame::Size,
    col: u16,
    row: u16,
) -> Option<Target> {
    let modal = crate::view::modal_rect(size);
    if !contains(modal, col, row) {
        return None;
    }
    if crate::view::modal_close_rect(modal).is_some_and(|rect| contains(rect, col, row)) {
        return Some(Target::ProjectSettingsClose);
    }
    if crate::view::project_settings_cancel_rect(modal, &vm.project_settings)
        .is_some_and(|rect| contains(rect, col, row))
    {
        return Some(Target::ProjectSettingsCancel);
    }
    for (index, rect) in crate::view::project_settings_action_rects(modal, &vm.project_settings) {
        if contains(rect, col, row) {
            return Some(Target::ProjectSettingsAction(index));
        }
    }
    Some(Target::OverlayBody)
}

/// Hit-test a mixed column (post cross-column move, panel-editor mode): resolve the click
/// to the panel of the unit under it — the same `solve_column` geometry the
/// renderer uses. Repository settings retains exact button geometry even after
/// cross-column moves.
fn hit_mixed(units: &[crate::layout_model::Unit], rect: Rect, col: u16, row: u16) -> Target {
    use crate::layout_model::Unit;
    let (slots, _) = crate::layout_model::solve_column(units, rect, 0);
    for slot in &slots {
        if contains(slot.rect, col, row) {
            return match slot.unit {
                Unit::Rail => Target::PanelFocus(Panel::Projects),
                Unit::Card(CardKind::Repository) => {
                    let buttons = cards::repository_button_layout(slot.rect, slot.collapsed);
                    if buttons.recall.rect.w > 0 && contains(buttons.recall.rect, col, row) {
                        Target::RepositoryRecallViewer
                    } else if buttons.settings.rect.w > 0
                        && contains(buttons.settings.rect, col, row)
                    {
                        Target::RepositorySettings
                    } else {
                        Target::PanelFocus(Panel::Detail)
                    }
                }
                Unit::Card(_) => Target::PanelFocus(Panel::Detail),
                Unit::Section(id) => Target::Section(id),
            };
        }
    }
    Target::PanelFocus(Panel::Detail)
}

/// Hit-test the panel editor: a column divider (right border), then a
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

/// Hit-test the open profile menu: a browse-stage click on a listed
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
    // The close `[x]`, on every stage — the mode's own close.
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
        crate::profile_menu::MenuStage::Members { .. } => {
            // The action chips first (they sit on their own trailing row).
            for (chip, _, text) in
                crate::view::members_action_chips(rect, vm.profile_menu.members.len())
            {
                if contains(chip, col, row) {
                    return Some(crate::view::members_chip_target(text));
                }
            }
            for i in 0..vm.profile_menu.members.len() {
                let rr = crate::view::profile_menu_row_rect(rect, i);
                if rr.h > 0 && contains(rr, col, row) {
                    return Some(Target::MemberRow(i));
                }
            }
        }
        crate::profile_menu::MenuStage::MembersAdd { .. } => {
            for i in 0..vm.profile_menu.candidates().len() {
                let rr = crate::view::profile_menu_row_rect(rect, i);
                if rr.h > 0 && contains(rr, col, row) {
                    return Some(Target::MemberAddRow(i));
                }
            }
        }
        _ => {}
    }
    Some(Target::OverlayBody)
}

/// Hit-test the open colour picker: a click on a listed row (a slot,
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
    // The close `[x]` — the picker's own close.
    if let Some(cr) = crate::view::modal_close_rect(rect) {
        if contains(cr, col, row) {
            return Some(Target::PickerClose);
        }
    }
    for i in 0..crate::view::colour_picker_rows(vm.colour_picker.stage) {
        let rr = crate::view::colour_picker_row_rect(rect, i);
        if rr.h > 0 && contains(rr, col, row) {
            return Some(Target::PickerRow(i));
        }
    }
    Some(Target::OverlayBody)
}

/// Hit-test the open destination explorer: the close X, the action
/// chips, the back row, the list's overflow scrollbar, then an entry row — each
/// resolved through the EXACT solvers `view::draw_explorer` drew with, so a
/// click lands on the cell the operator sees. The scroll offset maps the
/// viewport row back to its visible index, so a scrolled list stays honest. The
/// rest of the frame is an inert body; outside it, nothing.
fn hit_explorer(
    vm: &ViewModel,
    size: crate::render::frame::Size,
    col: u16,
    row: u16,
) -> Option<Target> {
    // The same floor `draw_explorer` refuses to paint below — a frame too short
    // to draw the surface must not resolve clicks on affordances that are not
    // on screen (notably the close X, whose rect exists from 8 columns).
    let rect = crate::view::modal_rect(size);
    if rect.w < 8 || rect.h < 5 || !contains(rect, col, row) {
        return None;
    }
    // The close `[x]` — the mode's own close, never the overlay's.
    if let Some(cr) = crate::view::modal_close_rect(rect) {
        if contains(cr, col, row) {
            return Some(Target::ExplorerClose);
        }
    }
    for (chip, target, _) in crate::view::explorer_button_rects(rect) {
        if contains(chip, col, row) {
            return Some(target);
        }
    }
    // The dot-toggle chip sits INSIDE the back row, so it outranks it.
    let dots = crate::view::explorer_dotfiles_rect(rect);
    if dots.w > 0 && contains(dots, col, row) {
        return Some(Target::ExplorerDotfiles);
    }
    let up = crate::view::explorer_up_rect(rect);
    if up.h > 0 && contains(up, col, row) {
        return Some(Target::ExplorerUp);
    }
    // The list's overflow scrollbar sits on the viewport's right edge and wins
    // over the row beneath it (the column-3 / rail precedent).
    let list = crate::view::explorer_list_area(rect);
    if on_scrollbar(
        list,
        vm.explorer.scroll,
        vm.explorer.visible_len(),
        col,
        row,
    ) {
        return Some(Target::Scrollbar(ScrollRegion::Modal));
    }
    if let Some(dy) = local_row(list, col, row) {
        let idx = vm.explorer.scroll.saturating_add(usize::from(dy));
        if idx < vm.explorer.visible_len() {
            return Some(Target::ExplorerRow(idx));
        }
    }
    Some(Target::OverlayBody)
}

/// Hit-test the data-driven full grid: the command row, then each physical
/// column by its role — the same `solve_model`/role dispatch `view::draw_full`
/// uses, so a click resolves to exactly the element drawn.
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
        };
    }
    None
}

/// Resolve a click while an overlay owns the surface (the modal geometry,
/// focus-and-close, message-routing and scrollbar rules):
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
    // The bottom action bar — add / reorder / remove — takes priority
    // over row selection; its chips are the same cells the renderer drew.
    for (r, target) in &buttons {
        if contains(*r, col, row) {
            return target.clone();
        }
    }
    if !vm.projects.is_empty() {
        let (start, end) = visible_window(vm.selected, vm.projects.len(), usize::from(list.h));
        // The rail overflow scrollbar on the panel's right border.
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

/// A click in column 2 → the repository settings button or panel focus.
/// Recomputes the card stack from the same solver the renderer uses, so a click
/// resolves to the exact card/button drawn.
fn toggle_or_focus(vm: &ViewModel, units: &[CardKind], rect: Rect, col: u16, row: u16) -> Target {
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
        let buttons = cards::repository_button_layout(rect, false);
        if buttons.recall.rect.w > 0 && contains(buttons.recall.rect, col, row) {
            return Target::RepositoryRecallViewer;
        }
        if buttons.settings.rect.w > 0 && contains(buttons.settings.rect, col, row) {
            return Target::RepositorySettings;
        }
        return Target::PanelFocus(Panel::Detail);
    }
    for slot in cards::solve_scrolled(units, rect, vm.detail_scroll).0 {
        if !contains(slot.rect, col, row) {
            continue;
        }
        if slot.kind == CardKind::Repository {
            let buttons = cards::repository_button_layout(slot.rect, slot.collapsed);
            if buttons.recall.rect.w > 0 && contains(buttons.recall.rect, col, row) {
                return Target::RepositoryRecallViewer;
            }
            if buttons.settings.rect.w > 0 && contains(buttons.settings.rect, col, row) {
                return Target::RepositorySettings;
            }
            // The status row, when it is the adopt act. Solved by the same
            // function the renderer draws from, and consulted AFTER the two
            // labelled buttons so a geometry that overlapped them could never
            // steal a click from one.
            if cards::repository_adopt_rect(slot.rect, slot.collapsed, detail.status)
                .is_some_and(|r| contains(r, col, row))
            {
                return Target::RepositoryAdopt;
            }
        }
        break; // clicked inside this card but not a specific target → focus
    }
    Target::PanelFocus(Panel::Detail)
}

/// Whether `(col, row)` lands on the overflow scrollbar for a region of `area`
/// with `(offset, content_len)` — the same `chrome::scrollbar` geometry the
/// renderer draws, so the bar's cells and its hit-region are identical (the scrollbar rule).
fn on_scrollbar(area: Rect, offset: usize, content_len: usize, col: u16, row: u16) -> bool {
    crate::chrome::scrollbar(area, offset, content_len)
        .is_some_and(|bar| contains(bar.track, col, row))
}

/// A click in column 3 → the graph section under it, else focus the panel. When
/// the panels overflow, the viewport row maps through the scroll
/// `offset` into the fully-expanded stack — the same window the renderer blits —
/// so a click resolves to the panel actually on screen.
fn section_or_focus(
    ids: &[col3::SectionId],
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
                // The USD-savings chip on the Recall headline
                // wins over the section body. Recomputed from the SAME solver
                // the renderer drew with — in the blended space this loop
                // already works in (screen x, content-space y) — so the
                // clickable cells are exactly the drawn glyphs. No figure,
                // collapsed, or narrow → the solver says None → plain section.
                let panel = Rect {
                    x: inner.x,
                    y: slot.y,
                    w: inner.w,
                    h: slot.rows,
                };
                if slot.id == col3::SectionId::Recall {
                    // The period buttons are their own row, so they cannot
                    // collide with the headline chips below; solved from the
                    // same function the renderer drew with, in this loop's
                    // blended space (screen x, content-space y).
                    for button in col3::recall_window_buttons(slot.collapsed, inner_of(panel)) {
                        if contains(button.rect, col, content_row) {
                            return Target::RecallWindow(button.window);
                        }
                    }
                    if let Some(chip) =
                        col3::recall_unit_rect(data, slot.collapsed, inner_of(panel))
                    {
                        if contains(chip, col, content_row) {
                            return Target::RecallUnitToggle;
                        }
                    }
                    if let Some(chip) =
                        col3::recall_cost_rect(data, slot.collapsed, inner_of(panel))
                    {
                        if contains(chip, col, content_row) {
                            return Target::RecallCost;
                        }
                    }
                    // The savings-share chip is a SECOND door onto the same
                    // breakdown modal, so it maps to the same target: no new
                    // act, no new hint, and the modal stays reachable on a
                    // repository whose models carry no seeded price.
                    if let Some(chip) =
                        col3::recall_share_rect(data, slot.collapsed, inner_of(panel))
                    {
                        if contains(chip, col, content_row) {
                            return Target::RecallCost;
                        }
                    }
                }
                // The token panel's `Σ` total is the "by worktree" axis's
                // affordance (the family view) and sits on the panel's TITLE
                // row, so the solver takes the section's outer rect — not the
                // inner one the Recall chip's headline lives in.
                if slot.id == col3::SectionId::TokenUsage {
                    if let Some(chip) = col3::token_total_rect(data, panel) {
                        if contains(chip, col, content_row) {
                            return Target::TokensByWorktree;
                        }
                    }
                }
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

    fn state_with_detail(w: u16, h: u16) -> AppState {
        let mut s = AppState::new(vec![project("a")], Size::new(w, h));
        s.app.splash.skip();
        let mut detail = crate::DetailView::thin("/r/a".into(), "a".into(), ProjectStatus::Idle);
        detail.project_settings = Box::new(crate::ProjectSettingsView {
            path: "/r/a".into(),
            project_name: "a".into(),
            initialization: crate::ProjectInitialization::Initialized,
            settings: crate::ProjectSettingId::ALL
                .iter()
                .map(|id| crate::ProjectSettingRow {
                    id: *id,
                    enabled: false,
                    actionable: true,
                    detail: None,
                })
                .collect(),
            policy: crate::ProjectPolicySummary::default(),
            integrations: Vec::new(),
        });
        crate::state::update(
            &mut s,
            crate::Event::ProjectData(crate::ProjectData {
                project: "/r/a".into(),
                status: ProjectStatus::Idle,
                detail: Some(detail),
            }),
        );
        s
    }

    #[test]
    fn repository_settings_button_hits_in_full_moved_mixed_two_and_single_layouts() {
        let body = |size: Size| Rect {
            x: 0,
            y: 1,
            w: size.w,
            h: size.h.saturating_sub(1),
        };

        // Default full-grid, expanded repository card.
        let mut s = state_with_detail(120, 40);
        let v = s.snapshot();
        let solved = crate::layout_model::solve_model(&v.layout, body(v.size));
        let card_col = &solved.columns[1];
        let repo = cards::solve(&card_col.card_kinds(), card_col.rect)
            .into_iter()
            .find(|slot| slot.kind == CardKind::Repository)
            .unwrap();
        assert!(!repo.collapsed);
        let chip = cards::repository_settings_rect(repo.rect, repo.collapsed);
        assert_eq!(
            hit_test(&v, chip.x, chip.y),
            Some(Target::RepositorySettings)
        );

        // Moved within the homogeneous card column.
        let mut s = state_with_detail(120, 40);
        s.app.layout.columns[1].units.rotate_left(1);
        let v = s.snapshot();
        let solved = crate::layout_model::solve_model(&v.layout, body(v.size));
        let card_col = &solved.columns[1];
        let repo = cards::solve(&card_col.card_kinds(), card_col.rect)
            .into_iter()
            .find(|slot| slot.kind == CardKind::Repository)
            .unwrap();
        let chip = cards::repository_settings_rect(repo.rect, repo.collapsed);
        assert_eq!(
            hit_test(&v, chip.x, chip.y),
            Some(Target::RepositorySettings)
        );

        // Mixed with graph sections after a cross-column move.
        let mut s = state_with_detail(120, 40);
        let repository = s.app.layout.columns[1].units.remove(0);
        s.app.layout.columns[2].units.insert(1, repository);
        let v = s.snapshot();
        let solved = crate::layout_model::solve_model(&v.layout, body(v.size));
        let mixed = &solved.columns[2];
        let (slots, _) = crate::layout_model::solve_column(&mixed.units, mixed.rect, 0);
        let repo = slots
            .into_iter()
            .find(|slot| slot.unit == crate::layout_model::Unit::Card(CardKind::Repository))
            .unwrap();
        let chip = cards::repository_settings_rect(repo.rect, repo.collapsed);
        assert_eq!(
            hit_test(&v, chip.x, chip.y),
            Some(Target::RepositorySettings)
        );

        // Two-column degraded tier.
        let mut s = state_with_detail(60, 20);
        let v = s.snapshot();
        let panels = crate::layout::two_col(body(v.size));
        let repo = cards::solve(&CardKind::ALL, panels.col2)
            .into_iter()
            .find(|slot| slot.kind == CardKind::Repository)
            .unwrap();
        let chip = cards::repository_settings_rect(repo.rect, repo.collapsed);
        assert_eq!(
            hit_test(&v, chip.x, chip.y),
            Some(Target::RepositorySettings)
        );

        // Single-panel pressure collapses the repository to `[settings]`.
        let mut s = state_with_detail(40, 16);
        s.app.set_focus(Panel::Detail);
        let v = s.snapshot();
        let (_, panel, _, _) = crate::layout::single_panel(body(v.size));
        let repo = cards::solve(&CardKind::ALL, panel)
            .into_iter()
            .find(|slot| slot.kind == CardKind::Repository)
            .unwrap();
        assert!(repo.collapsed);
        let chip = cards::repository_settings_rect(repo.rect, true);
        assert_eq!(chip.w, 10);
        assert_eq!(
            hit_test(&v, chip.x, chip.y),
            Some(Target::RepositorySettings)
        );
    }

    #[test]
    fn project_settings_mouse_plane_is_limited_to_action_chips() {
        let mut s = state_with_detail(120, 40);
        let mut view = s
            .snapshot()
            .detail
            .as_deref()
            .unwrap()
            .project_settings
            .as_ref()
            .clone();
        view.integrations = (0..9)
            .map(|index| crate::NativeAgentIntegrationRow {
                agent: format!("agent-{index}"),
                display_name: format!("Agent {index}"),
                detection: crate::NativeAgentDetection::NotDetected,
                executable_name: None,
                installations: Vec::new(),
                wiring: crate::NativeAgentWiring::Missing,
                hook_wiring: None,
                alpha_supported: true,
                selected: false,
            })
            .collect();
        s.app.project_settings.open(view);
        s.app.mode = crate::input::Mode::ProjectSettings;
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        let close = crate::view::modal_close_rect(modal).unwrap();
        assert_eq!(
            hit_test(&v, close.x, close.y),
            Some(Target::ProjectSettingsClose)
        );
        let chips = crate::view::project_settings_action_rects(modal, &v.project_settings);
        assert!(!chips.is_empty());
        for (index, chip) in chips {
            assert_eq!(
                hit_test(&v, chip.x, chip.y),
                Some(Target::ProjectSettingsAction(index))
            );
            if chip.x > modal.x + 1 {
                assert_eq!(
                    hit_test(&v, chip.x - 1, chip.y),
                    Some(Target::OverlayBody),
                    "the label/gap is not an activation target"
                );
            }
        }
        s.app.project_settings.confirm_raw = true;
        let v = s.snapshot();
        let cancel = crate::view::project_settings_cancel_rect(modal, &v.project_settings).unwrap();
        assert_eq!(
            hit_test(&v, cancel.x, cancel.y),
            Some(Target::ProjectSettingsCancel)
        );
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
        s.app.overlay = Some(crate::dispatch::Overlay::running("x"));
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
        // Inert while an overlay owns the surface (the profile menu and header identity).
        s.app.overlay = Some(crate::dispatch::Overlay::running("x"));
        let v = s.snapshot();
        assert_eq!(hit_test(&v, span.start, 0), None);
    }

    #[test]
    fn settings_chips_and_new_button_hit_their_drawn_cells() {
        // modal-surface parity: every hub act has a click twin — the chips, the
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

    // ---- the destination explorer's mouse plane ----------------------------

    fn dir(name: &str) -> crate::explorer::ExplorerEntry {
        crate::explorer::ExplorerEntry {
            name: name.into(),
            is_dir: true,
            tag: None,
        }
    }

    /// A state browsing a listing of `n` directories at a full-grid size.
    fn explorer_state(n: usize) -> AppState {
        let mut s = AppState::new(vec![project("a")], Size::new(120, 40));
        s.app.splash.skip();
        s.app.home_dir = Some("/home/dev".into());
        crate::input::enter_explorer(&mut s.app);
        s.app.explorer.apply(crate::explorer::DirListing {
            path: std::path::PathBuf::from("/home/dev"),
            entries: (0..n).map(|i| dir(&format!("d{i:03}"))).collect(),
            omitted: 0,
            error: None,
        });
        s
    }

    #[test]
    fn explorer_pixels_equal_targets_on_every_drawn_affordance() {
        let mut s = explorer_state(4);
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        // The close X — the MODE's own, never `OverlayClose`.
        let cr = crate::view::modal_close_rect(modal).unwrap();
        assert_eq!(hit_test(&v, cr.x, cr.y), Some(Target::ExplorerClose));
        assert_eq!(
            hit_test(&v, cr.x + cr.w - 1, cr.y),
            Some(Target::ExplorerClose)
        );
        // The back row — and the dot-toggle chip that sits inside it, which
        // must outrank the row it overlaps (both ends of the chip).
        let up = crate::view::explorer_up_rect(modal);
        assert_eq!(hit_test(&v, up.x, up.y), Some(Target::ExplorerUp));
        let dots = crate::view::explorer_dotfiles_rect(modal);
        assert!(dots.w > 0, "the dot-toggle chip renders at this width");
        assert_eq!(hit_test(&v, dots.x, dots.y), Some(Target::ExplorerDotfiles));
        assert_eq!(
            hit_test(&v, dots.x + dots.w - 1, dots.y),
            Some(Target::ExplorerDotfiles)
        );
        assert_eq!(
            hit_test(&v, dots.x - 1, dots.y),
            Some(Target::ExplorerUp),
            "one cell left of the chip is the back row again"
        );
        // Every listed row, both ends.
        for i in 0..4usize {
            let rr = crate::view::explorer_row_rect(modal, i, 0);
            assert!(rr.h > 0);
            assert_eq!(hit_test(&v, rr.x, rr.y), Some(Target::ExplorerRow(i)));
            assert_eq!(
                hit_test(&v, rr.x + rr.w - 1, rr.y),
                Some(Target::ExplorerRow(i))
            );
        }
        // Both chips.
        for (chip, target, _) in crate::view::explorer_button_rects(modal) {
            assert_eq!(hit_test(&v, chip.x, chip.y), Some(target.clone()));
            assert_eq!(
                hit_test(&v, chip.x + chip.w - 1, chip.y),
                Some(target.clone()),
                "{target:?} resolves across its whole label"
            );
        }
        // A viewport row past the listing is inert body — never a phantom row.
        let empty = crate::view::explorer_row_rect(modal, 5, 0);
        assert!(empty.h > 0, "the viewport row exists…");
        assert_eq!(
            hit_test(&v, empty.x, empty.y),
            Some(Target::OverlayBody),
            "…but there is no fifth entry to click"
        );
        // Outside the frame nothing is clickable (the scrim is inert).
        assert_eq!(hit_test(&v, modal.x.saturating_sub(2), modal.y + 2), None);
    }

    #[test]
    fn a_scrolled_explorer_maps_the_viewport_row_back_to_its_visible_index() {
        let mut s = explorer_state(200);
        s.app.explorer.scroll = 20;
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        let list = crate::view::explorer_list_area(modal);
        // The viewport's top row is entry 20, not entry 0 (the profile menu's
        // zero-sizing would have made this a wrong-row click).
        assert_eq!(hit_test(&v, list.x, list.y), Some(Target::ExplorerRow(20)));
        assert_eq!(
            hit_test(&v, list.x, list.y + list.h - 1),
            Some(Target::ExplorerRow(20 + usize::from(list.h) - 1))
        );
    }

    #[test]
    fn the_explorer_scrollbar_resolves_where_it_is_drawn_and_only_on_overflow() {
        let mut s = explorer_state(200);
        let v = s.snapshot();
        let modal = crate::view::modal_rect(v.size);
        let list = crate::view::explorer_list_area(modal);
        let bar = crate::chrome::scrollbar(list, 0, 200).expect("200 rows overflow");
        for dy in 0..bar.track.h {
            assert_eq!(
                hit_test(&v, bar.track.x, bar.track.y + dy),
                Some(Target::Scrollbar(ScrollRegion::Modal)),
            );
        }
        // A fitting listing has no bar — the same cell is just a row.
        let mut s = explorer_state(2);
        let v = s.snapshot();
        assert_eq!(
            hit_test(&v, bar.track.x, bar.track.y),
            Some(Target::ExplorerRow(0)),
            "no overflow, no bar, no scrollbar target"
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

    /// the priced savings estimate: the chip's clickable cells are exactly the drawn glyphs
    /// — same solver on both sides — and it outranks the section under it;
    /// without an honest figure the same cells are plain section clicks.
    #[test]
    fn recall_cost_chip_wins_over_its_section_exactly_on_its_cells() {
        use crate::col3::{RecallView, SavingsData};
        let mut data = col3::Col3 {
            savings: RecallView::Data(SavingsData {
                target_pct: Some(90),
                usd_saved_micros: Some(6_250_000),
                usd_label: Some("~$6.25".to_owned()),
                usd_rows: vec![],
                usd_read_micros: None,
                usd_read_label: None,
                by_worktree: crate::col3::WorktreeAxis::default(),
                share: Box::new(crate::col3::ShareData::default()),
            }),
            ..col3::Col3::default()
        };
        let ids = col3::SectionId::ALL;
        // Tall enough that the expanded stack fits (the un-scrolled path).
        let content = col3::content_rows(&ids);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 40,
            h: content + 2,
        };
        let inner = inner_of(rect);
        // Recompute the chip cell the same way the resolver does.
        let slot = col3::solve_stack(&ids, inner.h)
            .into_iter()
            .find(|s| s.id == col3::SectionId::Recall)
            .expect("recall slot");
        assert!(!slot.collapsed, "fixture must exercise the wide path");
        let panel = Rect {
            x: inner.x,
            y: slot.y,
            w: inner.w,
            h: slot.rows,
        };
        let chip = col3::recall_cost_rect(&data, false, inner_of(panel)).expect("an honest figure");
        // Blended space → screen: y is content-space (no scroll ⇒ + inner.y).
        let screen_row = inner.y + chip.y;
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x, screen_row),
            Target::RecallCost,
            "the chip's own cells hit the chip"
        );
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x + chip.w - 1, screen_row),
            Target::RecallCost,
            "…all the way to its last cell"
        );
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x - 1, screen_row),
            Target::Section(col3::SectionId::Recall),
            "one cell left (the gap) is the plain section"
        );
        assert!(Target::RecallCost.is_button(), "button semantics");

        // No honest figure → the money chip is gone, and the target chip
        // slides onto the same cells: the same door, which is why the
        // breakdown is still reachable by mouse on a repository whose models
        // carry no seeded price.
        data.savings = RecallView::Data(SavingsData {
            target_pct: Some(90),
            ..SavingsData::default()
        });
        assert_eq!(col3::recall_cost_rect(&data, false, inner_of(panel)), None);
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x, screen_row),
            Target::RecallCost,
            "the target chip is the second door onto the same modal"
        );

        // Neither chip → the exact same cells are plain section clicks.
        data.savings = RecallView::Data(SavingsData::default());
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x, screen_row),
            Target::Section(col3::SectionId::Recall),
            "no figure, no target, no chip"
        );
    }

    /// The savings-share chip is a SECOND door onto the breakdown modal, not a
    /// new act: its own cells resolve to the same target the USD chip does,
    /// and — the case that matters — they still do when no seeded price
    /// exists, which is exactly when the USD chip is not drawn at all.
    #[test]
    fn the_share_chip_opens_the_same_modal_as_the_cost_chip() {
        use crate::col3::{RecallView, SavingsData, ShareData};
        let share = ShareData {
            window: None,
            overall: Some(3.0),
            honesty: 11_965,
            avoided_tokens: 12_000,
            read_tokens: 9_000,
            sessions_with_reads: 3,
            reads_unmeasured: 0,
            by_family: Some(3.0),
            excluding: Some(1.5),
            excluded: vec!["codex".to_owned()],
            families: Vec::new(),
        };
        // No dollar figure: the USD chip is absent and the share chip is the
        // only way in.
        let data = col3::Col3 {
            savings: RecallView::Data(SavingsData {
                target_pct: Some(90),
                usd_saved_micros: None,
                usd_label: None,
                usd_rows: vec![],
                usd_read_micros: None,
                usd_read_label: None,
                by_worktree: crate::col3::WorktreeAxis::default(),
                share: Box::new(share.clone()),
            }),
            ..col3::Col3::default()
        };
        let ids = col3::SectionId::ALL;
        let content = col3::content_rows(&ids);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 60,
            h: content + 2,
        };
        let inner = inner_of(rect);
        let slot = col3::solve_stack(&ids, inner.h)
            .into_iter()
            .find(|s| s.id == col3::SectionId::Recall)
            .expect("recall slot");
        let panel = Rect {
            x: inner.x,
            y: slot.y,
            w: inner.w,
            h: slot.rows,
        };
        assert_eq!(
            col3::recall_cost_rect(&data, false, inner_of(panel)),
            None,
            "no seeded price, no dollar chip"
        );
        let chip = col3::recall_share_rect(&data, false, inner_of(panel))
            .expect("an answerable share draws a chip");
        let screen_row = inner.y + chip.y;
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x, screen_row),
            Target::RecallCost,
            "the share chip's own cells open the breakdown"
        );
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x + chip.w - 1, screen_row),
            Target::RecallCost,
            "…all the way to its last cell"
        );
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x - 1, screen_row),
            Target::Section(col3::SectionId::Recall),
            "one cell left (the gap) is the plain section"
        );
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
        let content = col3::content_rows(&ids);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 40,
            h: content + 2,
        };
        let inner = inner_of(rect);
        let slot = col3::solve_stack(&ids, inner.h)
            .into_iter()
            .find(|s| s.id == col3::SectionId::Recall)
            .expect("recall slot");
        let panel = Rect {
            x: inner.x,
            y: slot.y,
            w: inner.w,
            h: slot.rows,
        };
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
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, after, inner.y + last.rect.y),
            Target::Section(col3::SectionId::Recall),
        );
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, buttons[0].rect.x, inner.y + slot.y),
            Target::Section(col3::SectionId::Recall),
            "the panel's title row is not the period row",
        );
    }

    /// The unit chip resolves to the toggle on exactly its own cells, and the
    /// gap between it and the last period button is a plain section click —
    /// so a press that misses lands on nothing rather than on a period.
    #[test]
    fn the_unit_chip_resolves_to_the_toggle_and_not_to_a_period() {
        let data = col3::Col3::default();
        let ids = col3::SectionId::ALL;
        let content = col3::content_rows(&ids);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 40,
            h: content + 2,
        };
        let inner = inner_of(rect);
        let slot = col3::solve_stack(&ids, inner.h)
            .into_iter()
            .find(|s| s.id == col3::SectionId::Recall)
            .expect("recall slot");
        let panel = Rect {
            x: inner.x,
            y: slot.y,
            w: inner.w,
            h: slot.rows,
        };
        let chip = col3::recall_unit_rect(&data, slot.collapsed, inner_of(panel))
            .expect("the fixture draws the unit chip");
        let screen_row = inner.y + chip.y;
        for x in chip.x..chip.x + chip.w {
            assert_eq!(
                section_or_focus(&ids, &data, rect, 0, x, screen_row),
                Target::RecallUnitToggle,
                "cell {x} of the unit chip"
            );
        }
        assert!(Target::RecallUnitToggle.is_button());
        assert_eq!(
            section_or_focus(&ids, &data, rect, 0, chip.x - 1, screen_row),
            Target::Section(col3::SectionId::Recall),
            "the gap before the chip belongs to no control"
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
        let content = col3::content_rows(&ids);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 40,
            h: content + 2,
        };
        let inner = inner_of(rect);
        let slot = col3::solve_stack(&ids, inner.h)
            .into_iter()
            .find(|s| s.id == col3::SectionId::TokenUsage)
            .expect("token slot");
        let panel = Rect {
            x: inner.x,
            y: slot.y,
            w: inner.w,
            h: slot.rows,
        };
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
    fn overlay_owns_its_region_and_buttons_resolve() {
        let mut s = AppState::new(vec![project("a")], Size::new(100, 32));
        s.app.splash.skip();
        s.app.overlay = Some(crate::dispatch::Overlay::Confirm {
            request: crate::dispatch::DispatchRequest::Rebuild,
            repo: "/r/a".into(),
        });
        let v = s.snapshot();
        // The modal is a centered box; its geometry comes from the
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
        // clickable and its drawn cells equal the hit region (the message-routing rule).
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
        s.app.open_overlay(crate::dispatch::Overlay::running("x"));
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

    #[test]
    fn recall_viewer_action_chips_hit_the_exact_shared_rectangles() {
        let mut state = state_with_detail(120, 40);
        let _ = state.open_recall_viewer(None);
        let view = state.snapshot();
        let geometry = crate::recall_viewer::geometry(view.size, view.viewer.pane);
        let controls = crate::recall_viewer::timeline_controls(geometry.timeline);
        assert_eq!(
            hit_test(&view, controls.full_session.x, controls.full_session.y),
            Some(Target::ViewerFullSession)
        );
        assert_eq!(
            hit_test(&view, controls.dashboard.x, controls.dashboard.y),
            Some(Target::ViewerDashboard)
        );
        let checkpoints = inner_of(geometry.checkpoints);
        assert_eq!(
            hit_test(&view, checkpoints.x, checkpoints.y),
            Some(Target::ViewerFilter)
        );
    }

    fn scanned_targets(state: &mut AppState) -> Vec<Target> {
        let view = state.snapshot();
        let mut targets = Vec::new();
        for row in 0..view.size.h {
            for col in 0..view.size.w {
                if let Some(target) = hit_test(&view, col, row) {
                    if !targets.contains(&target) {
                        targets.push(target);
                    }
                }
            }
        }
        targets
    }

    fn assert_scanned(state: &mut AppState, expected: &[Target]) {
        let targets = scanned_targets(state);
        for target in expected {
            assert!(
                targets.contains(target),
                "surface did not expose {target:?}; found {targets:?}"
            );
        }
    }

    fn populated_viewer_state(w: u16, h: u16) -> AppState {
        use crate::recall_viewer::{
            ViewerCheckpointRow, ViewerDetail, ViewerFilterProgress, ViewerLine, ViewerLineKind,
            ViewerTimelineItem, ViewerTimelineSegment, ViewerTimestampProvenance,
        };

        let mut state = state_with_detail(w, h);
        let _ = state.open_recall_viewer(None);
        let rows = (0_u32..80)
            .map(|index| ViewerCheckpointRow {
                checkpoint_id: format!("cp-{index:03}"),
                effective_at_ms: Some(i64::from(index)),
                timestamp_provenance: ViewerTimestampProvenance::Recorded,
                compact_utc: Some("2026-08-28 12:00Z".to_owned()),
                excerpt: Some(format!("checkpoint {index}")),
                excerpt_loading: false,
                anchor_commit: Some(format!("oid-{index:03}")),
                origins: Vec::new(),
                chain_position: Some(u64::from(index)),
                local_branches: vec!["main".to_owned()],
                strategy: Some("native".to_owned()),
                projection: "projected".to_owned(),
                issues: Vec::new(),
                metadata_search: format!("cp-{index:03}"),
                worktree: None,
            })
            .collect::<Vec<_>>();
        state.app.viewer.catalogue = std::sync::Arc::new(rows);
        state.app.viewer.visible = (0..80).collect();
        state.app.viewer.new_count = 2;
        state.app.viewer.filter_progress = Some(ViewerFilterProgress {
            scanned: 5,
            total: 80,
            complete: false,
        });
        state.app.viewer.detail = std::sync::Arc::new(ViewerDetail {
            checkpoint_id: "cp-000".to_owned(),
            segments: vec![ViewerTimelineSegment {
                title: "captured".to_owned(),
                boundary: Some("ordered by recorded time".to_owned()),
                ordered_by_time: true,
                items: (0..80)
                    .map(|index| ViewerTimelineItem {
                        id: format!("item-{index}"),
                        title: format!("event {index}"),
                        timestamp: Some("12:00:00Z".to_owned()),
                        summary: vec![ViewerLine::plain(
                            ViewerLineKind::Heading,
                            format!("summary {index}"),
                        )],
                        body: vec![ViewerLine::plain(ViewerLineKind::Text, "body")],
                        expanded_body: None,
                        collapsed: false,
                        full_session_only: false,
                    })
                    .collect(),
            }],
            notice: None,
        });
        state
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn every_live_surface_exposes_only_its_drawn_target_matrix() {
        // Exhaust every cell, not just one hand-picked coordinate per control.
        // This pins the negative space around chips, rows, scrollbars, tabs,
        // and modal frames as strongly as it pins the positive hit regions.
        let mut dashboard = state_with_detail(120, 40);
        assert_scanned(
            &mut dashboard,
            &[
                Target::HeaderHelp,
                Target::HeaderPalette,
                Target::HeaderEditor,
                Target::HeaderQuit,
                Target::ProjectRow(0),
                Target::RepositoryRecallViewer,
                Target::RepositorySettings,
                Target::CommandBar,
            ],
        );

        let mut rail_overflow = AppState::new(
            (0..80)
                .map(|index| project(&format!("p{index:03}")))
                .collect(),
            Size::new(120, 40),
        );
        rail_overflow.app.splash.skip();
        assert_scanned(
            &mut rail_overflow,
            &[Target::Scrollbar(ScrollRegion::Rail), Target::RailAdd],
        );

        for (width, height) in [(60, 20), (40, 16)] {
            for focus in [
                Panel::Projects,
                Panel::Detail,
                Panel::Graphs,
                Panel::Command,
            ] {
                let mut state = state_with_detail(width, height);
                state.app.set_focus(focus);
                assert!(!scanned_targets(&mut state).is_empty());
            }
        }

        let mut mixed = state_with_detail(120, 40);
        let repository = mixed.app.layout.columns[1].units.remove(0);
        mixed.app.layout.columns[2].units.insert(1, repository);
        assert_scanned(
            &mut mixed,
            &[Target::RepositoryRecallViewer, Target::RepositorySettings],
        );

        let mut editor = AppState::new(
            vec![project("a"), project("b"), project("c")],
            Size::new(120, 40),
        );
        editor.app.splash.skip();
        crate::input::enter_panel_editor(&mut editor.app);
        assert_scanned(
            &mut editor,
            &[
                Target::GripBar(crate::editor::Cursor { col: 0, unit: 0 }),
                Target::ColumnDivider(0),
            ],
        );

        let mut settings = state_with_detail(120, 40);
        let settings_view = settings
            .snapshot()
            .detail
            .as_deref()
            .expect("detail")
            .project_settings
            .as_ref()
            .clone();
        settings.app.project_settings.open(settings_view);
        settings.app.mode = crate::input::Mode::ProjectSettings;
        settings.app.project_settings.confirm_raw = true;
        assert_scanned(
            &mut settings,
            &[
                Target::ProjectSettingsClose,
                Target::ProjectSettingsCancel,
                Target::ProjectSettingsAction(0),
            ],
        );

        let mut profiles = profiled_state(120, 40);
        crate::input::enter_profile_menu(&mut profiles.app);
        assert_scanned(
            &mut profiles,
            &[
                Target::HeaderProfile,
                Target::ProfileRow(0),
                Target::ProfileSettings(0),
                Target::ProfileNew,
                Target::ProfileMenuClose,
            ],
        );
        profiles.app.profile_menu.stage = crate::profile_menu::MenuStage::Settings {
            profile: "pf0".to_owned(),
        };
        assert_scanned(&mut profiles, &[Target::SettingsRow(0)]);
        profiles.app.set_membership_lists(
            vec![project("a"), project("b")],
            vec![project("a"), project("b"), project("c")],
        );
        profiles.app.profile_menu.stage = crate::profile_menu::MenuStage::Members {
            profile: "pf0".to_owned(),
        };
        assert_scanned(
            &mut profiles,
            &[
                Target::MemberRow(0),
                Target::MemberUp,
                Target::MemberDown,
                Target::MemberRemove,
                Target::MemberAddOpen,
            ],
        );
        profiles.app.profile_menu.stage = crate::profile_menu::MenuStage::MembersAdd {
            profile: "pf0".to_owned(),
        };
        assert_scanned(&mut profiles, &[Target::MemberAddRow(0)]);

        profiles.app.mode = crate::input::Mode::ColourPicker;
        profiles.app.colour_picker.reset();
        assert_scanned(&mut profiles, &[Target::PickerRow(0), Target::PickerClose]);
        profiles.app.colour_picker.stage = crate::colour_picker::PickerStage::Presets;
        assert_scanned(
            &mut profiles,
            &[
                Target::PickerRow(crate::colour_picker::RESET_ROW),
                Target::PickerClose,
            ],
        );

        let mut explorer = explorer_state(200);
        assert_scanned(
            &mut explorer,
            &[
                Target::ExplorerClose,
                Target::ExplorerUp,
                Target::ExplorerDotfiles,
                Target::ExplorerRow(0),
                Target::Scrollbar(ScrollRegion::Modal),
            ],
        );

        let mut confirm = state_with_detail(100, 32);
        confirm.app.open_overlay(crate::dispatch::Overlay::Confirm {
            request: crate::dispatch::DispatchRequest::Rebuild,
            repo: "/r/a".to_owned(),
        });
        assert_scanned(
            &mut confirm,
            &[
                Target::OverlayConfirm,
                Target::OverlayCancel,
                Target::OverlayClose,
                Target::OverlayBody,
            ],
        );
        let mut results = state_with_detail(100, 32);
        results.app.open_overlay(crate::dispatch::Overlay::Results {
            title: "results".to_owned(),
            lines: (0..100).map(|index| format!("line {index}")).collect(),
            scroll: 10,
            footer: Some("footer".to_owned()),
            truncated: false,
        });
        assert_scanned(
            &mut results,
            &[
                Target::OverlayClose,
                Target::OverlayBody,
                Target::Scrollbar(ScrollRegion::Modal),
            ],
        );

        let mut suggestions = state_with_detail(120, 40);
        suggestions.app.mode = crate::input::Mode::Command;
        suggestions.app.command_line = "/".to_owned();
        assert_scanned(&mut suggestions, &[Target::SuggestRow(0)]);

        let mut viewer = populated_viewer_state(120, 40);
        assert_scanned(
            &mut viewer,
            &[
                Target::ProjectRow(0),
                Target::ViewerFilter,
                Target::ViewerLatest,
                Target::ViewerCheckpointRow(0),
                Target::ViewerFullSession,
                Target::ViewerDashboard,
                Target::ViewerTimelineLine(0),
                Target::Scrollbar(ScrollRegion::ViewerCheckpoints),
                Target::Scrollbar(ScrollRegion::ViewerTimeline),
            ],
        );
        for pane in [
            crate::recall_viewer::ViewerPane::Projects,
            crate::recall_viewer::ViewerPane::Checkpoints,
            crate::recall_viewer::ViewerPane::Timeline,
        ] {
            let mut viewer = populated_viewer_state(40, 20);
            viewer.app.viewer.pane = pane;
            assert_scanned(&mut viewer, &[Target::ViewerTab(pane)]);
        }
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

        /// …including over the destination explorer, whose scroll-aware solvers
        /// are the first geometry in the tree to take an offset — at any size,
        /// with any listing length and any offset (the destination explorer).
        #[test]
        fn explorer_hit_test_never_panics(
            w in 0u16..300,
            h in 0u16..120,
            col in 0u16..300,
            row in 0u16..120,
            entries in 0usize..40,
            scroll in 0usize..60,
        ) {
            let mut s = AppState::new(vec![project("a")], Size::new(w, h));
            s.app.splash.skip();
            crate::input::enter_explorer(&mut s.app);
            s.app.explorer.entries = (0..entries).map(|i| dir(&format!("d{i}"))).collect();
            s.app.explorer.scroll = scroll;
            let v = s.snapshot();
            let _ = hit_test(&v, col, row);
        }
    }
}
