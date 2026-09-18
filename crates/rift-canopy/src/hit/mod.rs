//! Mouse hit-testing: a click coordinate → the dashboard element under it.
//!
//! [`hit_test`] is a **pure** function of the render [`ViewModel`], recomputing
//! geometry from the *same* layout solvers the renderer uses ([`crate::layout`],
//! [`crate::col3::solve_stack`], `crate::view::visible_window`,
//! `crate::view::modal_rect`) — so a click always resolves to the exact
//! element that was drawn, with no stored/mutable geometry and no resize race.
//! It is engine-free (no `xre_*`), so it compiles and tests without the `xre`
//! feature, exactly like the keyboard reducer it feeds.
//!
//! One module per surface family holds its resolvers: `modal` the five
//! centered frames (Project Settings, the profile menu, the colour picker, the
//! destination explorer, the dispatch overlay), `grid` the dashboard tiers and
//! the rail and card columns, `sections` the column-3 panel stack with its
//! Recall chips, and `viewer` the Recall Viewer. This file keeps the
//! [`Target`] vocabulary, the dispatcher, and the cell arithmetic every
//! resolver shares.

mod grid;
mod modal;
mod sections;
mod viewer;

#[cfg(test)]
mod fixtures;
#[cfg(test)]
mod matrix;

use crate::col3;
use crate::input::Mode;
use crate::layout::{choose_mode, two_col, LayoutMode, Rect};
use crate::state::ViewModel;
use crate::Panel;

use grid::{hit_editor, hit_grid, hit_model, hit_single};
use modal::{hit_colour_picker, hit_explorer, hit_overlay, hit_profile_menu, hit_project_settings};
use viewer::hit_recall_viewer;

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
///
/// The surfaces are consulted in the order they are painted, last first: an
/// open overlay owns every click, then the suggest panel, then the open mode's
/// modal (or the panel editor's grid), then the Recall Viewer, then the
/// dashboard tier the size chooses — so the element the operator sees on top
/// is the one a click resolves to.
#[must_use]
pub fn hit_test(vm: &ViewModel, col: u16, row: u16) -> Option<Target> {
    let size = vm.size;
    if col >= size.w || row >= size.h || choose_mode(size) == LayoutMode::TooSmall {
        return None;
    }
    if row == 0 {
        return hit_header(vm, col);
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
    if let Some(target) = hit_suggest_panel(vm, col, row) {
        return Some(target);
    }
    // A mode modal owns the surface while open: its rows and chips act, the
    // rest of its frame is inert, and the covered panels beneath are not
    // clickable. Panel editing is Full3Col-only; the narrow tiers stay normal.
    match vm.mode {
        Mode::ProjectSettings => return hit_project_settings(vm, size, col, row),
        Mode::ProfileMenu => return hit_profile_menu(vm, size, col, row),
        Mode::ColourPicker => return hit_colour_picker(vm, size, col, row),
        Mode::Explorer => return hit_explorer(vm, size, col, row),
        Mode::PanelEditor if choose_mode(size) == LayoutMode::Full3Col => {
            return hit_editor(vm, body, col, row);
        }
        _ => (),
    }
    if vm.surface == crate::recall_viewer::Surface::RecallViewer {
        return hit_recall_viewer(vm, col, row);
    }
    hit_dashboard(vm, body, col, row)
}

/// Whether `(col, row)` lands on the overflow scrollbar for a region of `area`
/// with `(offset, content_len)` — the same `chrome::scrollbar` geometry the
/// renderer draws, so the bar's cells and its hit-region are identical (the scrollbar rule).
pub(super) fn on_scrollbar(
    area: Rect,
    offset: usize,
    content_len: usize,
    col: u16,
    row: u16,
) -> bool {
    crate::chrome::scrollbar(area, offset, content_len)
        .is_some_and(|bar| contains(bar.track, col, row))
}

/// Which third of a tab `strip` holds `col` — 0, 1 or 2. A coarse tab click
/// needs no glyph widths, and the single-panel strip and the Recall Viewer's
/// strip share one rule.
pub(super) fn third_of(col: u16, strip: Rect) -> usize {
    let rel = col.saturating_sub(strip.x);
    let third = (strip.w / 3).max(1);
    usize::from(rel / third).min(2)
}

/// The content rect inside a bordered panel (mirrors [`crate::Frame::border`]:
/// a one-cell ring, so content is inset by 1). Panels too small to border keep
/// their whole area.
pub(super) fn inner_of(r: Rect) -> Rect {
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
pub(super) fn local_row(inner: Rect, col: u16, row: u16) -> Option<u16> {
    if !contains(inner, col, row) {
        return None;
    }
    Some(row - inner.y)
}

/// Whether `(x, y)` lies within `r` (right/bottom exclusive).
pub(super) fn contains(r: Rect, x: u16, y: u16) -> bool {
    x >= r.x && y >= r.y && x < r.x.saturating_add(r.w) && y < r.y.saturating_add(r.h)
}

/// Row 0: the header's right-aligned actions, resolved by the cluster solver
/// (the profile hub) from the SAME spans the renderer draws — the identity
/// item included. Clickable only while no overlay owns the surface, mirroring
/// the overlay-owns-clicks rule for the body.
fn hit_header(vm: &ViewModel, col: u16) -> Option<Target> {
    use crate::bars::{header_action_at, HeaderAction};
    if vm.overlay.is_some() {
        return None;
    }
    let action = header_action_at(vm.size.w, vm.profile_name.as_deref(), col)?;
    Some(match action {
        HeaderAction::Help => Target::HeaderHelp,
        HeaderAction::Profile => Target::HeaderProfile,
        HeaderAction::Palette => Target::HeaderPalette,
        HeaderAction::Editor => Target::HeaderEditor,
        HeaderAction::Quit => Target::HeaderQuit,
    })
}

/// The suggest panel (drawn over the body, above the bar) owns its rows while
/// open — a click inserts, exactly like Tab; the rest of the panel focuses the
/// command bar. `None` when the panel is closed or the click is outside it, so
/// the surfaces beneath are consulted.
fn hit_suggest_panel(vm: &ViewModel, col: u16, row: u16) -> Option<Target> {
    if vm.suggestions.is_empty() {
        return None;
    }
    let rect = crate::view::suggest_panel_rect(vm.size, vm.suggestions.len());
    if rect.h == 0 || !contains(rect, col, row) {
        return None;
    }
    let inner = inner_of(rect);
    let shown = vm.suggestions.len().min(usize::from(inner.h));
    match local_row(inner, col, row) {
        Some(dy) if usize::from(dy) < shown => Some(Target::SuggestRow(usize::from(dy))),
        _ => Some(Target::PanelFocus(Panel::Command)),
    }
}

/// The dashboard tier the size chooses. The full grid is data-driven: it
/// hit-tests the *same* `solve_model` output the renderer draws, so pixels
/// equal targets for any arrangement. The degraded tiers keep the default
/// role layout.
fn hit_dashboard(vm: &ViewModel, body: Rect, col: u16, row: u16) -> Option<Target> {
    match choose_mode(vm.size) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hit::fixtures::{profiled_state, project, vm};
    use crate::render::frame::Size;
    use crate::state::AppState;

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
