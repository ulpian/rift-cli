//! The click table: every left-press target mapped onto the same state
//! change its key produces.
//!
//! Its own module because the table is the pointer half of modal-surface
//! parity — one arm per target, each delegating to the act its key uses —
//! and the parity fixtures read it as one surface.

use crate::colour_picker::{PickerStage, RESET_ROW};
use crate::command::Verb;
use crate::dispatch::Overlay;
use crate::editor::{Cursor, Drag};
use crate::hit::Target;
use crate::input::{
    activate_settings_row, enter_explorer, enter_panel_editor, enter_profile_menu,
    enter_profile_settings, explorer_descend, explorer_select, explorer_toggle_hidden,
    explorer_type_path, explorer_up, insert_suggestion, Action,
};
use crate::profile_menu::{MenuStage, SettingsRow};
use crate::recall_viewer::{timeline_item_at, Surface, ViewerPane};
use crate::theme::{ThemeSlot, PRESETS};
use crate::{Mode, Panel};

use super::actions::{
    apply_theme_slot, profile_or_fold_action, remove_project_op, reorder_project_op,
    switch_profile_flow,
};
use super::effects::persist_flow;
use super::openers::{
    open_adopt_confirm, open_project_settings, open_savings_breakdown, open_worktree_axis,
    select_recall_window,
};
use super::reduce::{layout_persist_op, selection_change_effects};
use super::verbs::{confirm_dispatch, dashboard_verb_flow};
use super::viewer::{
    clamp_viewer_timeline, select_latest_checkpoint, sync_viewer_focus, toggle_full_session,
    viewer_selection_effects,
};
use super::{AppState, Effects, Flow};

/// Apply a left-click target: focus panels, select a project, flip a toggle, or
/// open the command bar — each the same mutation as the equivalent keypress.
/// One arm per target, so a new target has to be classified here; the
/// per-surface handlers below hold the bodies.
pub(super) fn apply_click(s: &mut AppState, target: Target) -> Flow {
    if s.app.mode == Mode::ProjectSettings && s.app.project_settings.running {
        return Flow::Continue(Effects::default());
    }
    // Acknowledge a button press with the flash — keyed by target so
    // the right button lights; a repeat press re-arms. Selection/focus/body
    // targets don't flash (the rail row has its own selection pulse).
    if target.is_button() {
        s.arm_press_pulse(target.clone());
    }
    match target {
        Target::ProjectDrawer => super::dashboard::toggle_drawer(s),
        Target::DashboardTab(tab) => super::dashboard::select_tab(s, tab),
        Target::PanelFocus(panel) => click_panel(s, panel),
        Target::CommandBar => click_command_bar(s),
        Target::ProjectRow(i) => click_project_row(s, i),
        Target::Section(_) => click_section(s),
        Target::RailAdd | Target::RailRemove | Target::RailReorderUp | Target::RailReorderDown => {
            apply_rail_action(s, &target)
        }
        Target::RepositorySettings => open_project_settings(s),
        Target::RepositoryRecallViewer => Flow::Continue(s.open_recall_viewer(None)),
        Target::RepositoryAdopt => open_adopt_confirm(s),
        Target::ViewerFilter
        | Target::ViewerLatest
        | Target::ViewerFullSession
        | Target::ViewerDashboard
        | Target::ViewerCheckpointRow(_)
        | Target::ViewerTimelineLine(_)
        | Target::ViewerTab(_) => click_viewer(s, target),
        Target::ProjectSettingsAction(_)
        | Target::ProjectSettingsClose
        | Target::ProjectSettingsCancel => click_project_settings(s, target),
        Target::OverlayConfirm
        | Target::OverlayCancel
        | Target::OverlayClose
        | Target::WizardAdd
        | Target::OverlayBody
        | Target::Scrollbar(_) => click_overlay(s, target),
        Target::SuggestRow(i) => click_suggestion(s, i),
        Target::TokensByWorktree | Target::RecallWindow(_) | Target::RecallHeadline => {
            click_graphs_card(s, target)
        }
        Target::HeaderHelp
        | Target::HeaderPalette
        | Target::HeaderEditor
        | Target::HeaderQuit
        | Target::HeaderProfile => click_header(s, target),
        Target::ProfileRow(_)
        | Target::ProfileSettings(_)
        | Target::ProfileNew
        | Target::ProfileMenuClose => click_profile_menu(s, target),
        Target::MemberRow(_)
        | Target::MemberAddRow(_)
        | Target::MemberUp
        | Target::MemberDown
        | Target::MemberRemove
        | Target::MemberAddOpen => click_members_editor(s, target),
        Target::SettingsRow(i) => click_settings_row(s, i),
        Target::PickerRow(_) | Target::PickerClose => click_picker(s, target),
        Target::ExplorerRow(_)
        | Target::ExplorerUp
        | Target::ExplorerSelect
        | Target::ExplorerTypePath
        | Target::ExplorerDotfiles
        | Target::ExplorerClose => click_explorer(s, target),
        Target::GripBar(cursor) => press_grip(s, cursor),
        Target::ColumnDivider(i) => press_divider(s, i),
    }
}

/// The rail action bar's clicks — the add/remove/reorder buttons raise
/// the same typed effects the `a` / `d` / ⇧-arrow keys do. Add raises the
/// destination explorer; remove/reorder act on the selected project (no
/// selection ⇒ inert).
fn apply_rail_action(s: &mut AppState, target: &Target) -> Flow {
    s.app.set_focus(Panel::Projects);
    s.dirty = true;
    match target {
        // The `+` chip raises the destination explorer through the ONE entry
        // door (the destination explorer) — the click twin of `a`, not a second seeding.
        Target::RailAdd => {
            let action = enter_explorer(&mut s.app);
            profile_or_fold_action(s, action)
        }
        Target::RailRemove => {
            let sel = s.app.selected_project().map(|p| p.path.clone());
            let op = sel.map(|path| remove_project_op(&s.app, path));
            persist_flow(op)
        }
        Target::RailReorderUp | Target::RailReorderDown => {
            let up = matches!(target, Target::RailReorderUp);
            let sel = s.app.selected_project().map(|p| p.path.clone());
            let op = sel.map(|path| reorder_project_op(&s.app, path, up));
            persist_flow(op)
        }
        _ => Flow::Continue(Effects::default()),
    }
}

/// A column body: focus it — on the viewer surface, the pane that column holds.
fn click_panel(s: &mut AppState, panel: Panel) -> Flow {
    match s.app.surface {
        Surface::RecallViewer => focus_viewer_pane(
            s,
            match panel {
                Panel::Projects => ViewerPane::Projects,
                Panel::Detail => ViewerPane::Checkpoints,
                Panel::Graphs => ViewerPane::Timeline,
                Panel::Command => ViewerPane::Command,
            },
        ),
        Surface::Dashboard => s.app.set_focus(panel),
    }
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// The command bar: open the palette with the bar focused; on the viewer
/// surface, remember the pane to return to.
fn click_command_bar(s: &mut AppState) -> Flow {
    match s.app.surface {
        Surface::RecallViewer => {
            if s.app.viewer.pane != ViewerPane::Command {
                s.app.viewer.pane_before_command = s.app.viewer.pane;
            }
            s.app.viewer.pane = ViewerPane::Command;
            s.app.focus = Panel::Command;
        }
        Surface::Dashboard => s.app.set_focus(Panel::Command),
    }
    s.app.mode = Mode::Command;
    s.app.command_line.clear();
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// A rail row: select it with the same bus effects the arrow keys raise; on
/// the viewer surface, open its catalogue instead.
fn click_project_row(s: &mut AppState, i: usize) -> Flow {
    let before = s.app.selected_project().map(|p| p.path.clone());
    if i < s.app.projects.len() {
        s.app.selected = i;
    }
    s.dirty = true;
    let mut eff = Effects::default();
    selection_change_effects(s, &before, &mut eff);
    if let Flow::Continue(closed) = super::dashboard::toggle_drawer(s) {
        eff.data_requests.extend(closed.data_requests);
    }
    Flow::Continue(eff)
}

/// A graphs-column section body: focus the column.
fn click_section(s: &mut AppState) -> Flow {
    s.app.set_focus(Panel::Graphs);
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// The viewer's chips, rows and tabs: each moves to the pane it belongs to,
/// then does what its key does.
fn click_viewer(s: &mut AppState, target: Target) -> Flow {
    let effects = match target {
        Target::ViewerFilter => {
            focus_viewer_pane(s, ViewerPane::Checkpoints);
            s.app.viewer.begin_filter_editing();
            Effects::default()
        }
        Target::ViewerLatest => {
            focus_viewer_pane(s, ViewerPane::Checkpoints);
            select_latest_checkpoint(s)
        }
        Target::ViewerFullSession => {
            focus_viewer_pane(s, ViewerPane::Timeline);
            toggle_full_session(s);
            Effects::default()
        }
        Target::ViewerDashboard => {
            s.close_recall_viewer();
            return Flow::Continue(Effects::default());
        }
        Target::ViewerCheckpointRow(index) => {
            focus_viewer_pane(s, ViewerPane::Checkpoints);
            select_checkpoint_row(s, index)
        }
        Target::ViewerTimelineLine(line) => {
            focus_viewer_pane(s, ViewerPane::Timeline);
            s.app.viewer.timeline_cursor = line;
            if let Some(id) = timeline_item_at(&s.app.viewer, line) {
                s.app.viewer.toggle_expanded(&id);
            }
            clamp_viewer_timeline(&mut s.app, s.size);
            Effects::default()
        }
        Target::ViewerTab(pane) => {
            focus_viewer_pane(s, pane);
            Effects::default()
        }
        _ => Effects::default(),
    };
    s.dirty = true;
    Flow::Continue(effects)
}

/// Move the viewer to `pane` and keep the dashboard focus in step.
fn focus_viewer_pane(s: &mut AppState, pane: ViewerPane) {
    s.app.viewer.pane = pane;
    sync_viewer_focus(&mut s.app);
}

/// A checkpoint row: select it, reading its detail only if that moved the
/// cursor; a click past the list's end leaves the selection alone.
fn select_checkpoint_row(s: &mut AppState, index: usize) -> Effects {
    let changed = index < s.app.viewer.visible.len() && s.app.viewer.selected != index;
    if index < s.app.viewer.visible.len() {
        s.app.viewer.selected = index;
    }
    if !changed {
        return Effects::default();
    }
    viewer_selection_effects(s)
}

/// The Project Settings page: a row activates like `Enter`, the close X
/// closes the mode, and the raw-transcript acknowledgement's cancel chip
/// returns to the list the way Esc does.
fn click_project_settings(s: &mut AppState, target: Target) -> Flow {
    s.dirty = true;
    match target {
        Target::ProjectSettingsAction(i) => {
            s.app.project_settings.select(i);
            Flow::Continue(Effects {
                project_settings_actions: s.app.project_settings.activate().into_iter().collect(),
                ..Effects::default()
            })
        }
        Target::ProjectSettingsClose => {
            s.app.dashboard_tab = crate::dashboard::DashboardTab::Home;
            s.app.mode = Mode::Normal;
            s.app.project_settings.close();
            Flow::Continue(Effects::default())
        }
        Target::ProjectSettingsCancel => {
            s.app.project_settings.confirm_raw = false;
            s.app.project_settings.result = None;
            Flow::Continue(Effects::default())
        }
        _ => Flow::Continue(Effects::default()),
    }
}

/// The overlay's mouse surface: Confirm is the mouse Enter, Cancel the mouse
/// Esc, and the close X the click/Tab equivalent of Esc on any modal that
/// carries a Close control — an empty registry keeps its wizard (dismissing
/// it just reveals the empty rail; it re-raises only if the surface is
/// otherwise free). The wizard's primary raises the destination explorer,
/// the click equivalent of `a` from the first-run modal, so all three
/// add-project affordances are one door. The body and a scrollbar are inert:
/// the scrollbar is an overflow indicator only — the wheel scrolls the region
/// in `apply_scroll`, and drag/click-to-page is a later rung on the same
/// track/thumb geometry.
fn click_overlay(s: &mut AppState, target: Target) -> Flow {
    match target {
        Target::OverlayConfirm => {
            s.dirty = true;
            confirm_dispatch(s)
        }
        Target::OverlayCancel => {
            if matches!(s.app.overlay, Some(Overlay::Confirm { .. })) {
                s.app.overlay = None;
            }
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::OverlayClose => {
            s.app.overlay = None;
            s.app.overlay_focus = 0;
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::WizardAdd => {
            s.dirty = true;
            let action = enter_explorer(&mut s.app);
            profile_or_fold_action(s, action)
        }
        _ => Flow::Continue(Effects::default()),
    }
}

/// A suggestion click INSERTS only — Tab's mouse equivalent, never execution.
fn click_suggestion(s: &mut AppState, i: usize) -> Flow {
    s.app.suggest_highlight = i;
    insert_suggestion(&mut s.app);
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// The graphs column's chips, each through the SAME opener its key uses so
/// the two doors cannot drift apart: the token panel's `Σ` total opens the
/// "by worktree" axis (the pointer twin of `w`), the Recall headline the
/// read-only breakdown (the pointer twin of `S`), and a period button adopts
/// its window in the same frame as the press and asks the data plane to
/// recompute over it (re-pressing the live window is inert beyond the
/// flash). The axis and the breakdown are `Overlay::Results` — the
/// existing modal surface, so the shared close X, focus ring, scroll and
/// hints come with zero new parity wiring — and read the TRUTH col3, never
/// the animated display.
fn click_graphs_card(s: &mut AppState, target: Target) -> Flow {
    match target {
        Target::TokensByWorktree => {
            s.dirty = true;
            open_worktree_axis(s)
        }
        Target::RecallWindow(window) => {
            s.dirty = true;
            select_recall_window(s, window)
        }
        Target::RecallHeadline => open_savings_breakdown(s),
        _ => Flow::Continue(Effects::default()),
    }
}

/// The header actions map onto the exact same effects their keys produce —
/// `?` help, `/` palette, `e`/Esc the panel editor, `q` quit, and `p`/Esc the
/// profile menu.
fn click_header(s: &mut AppState, target: Target) -> Flow {
    match target {
        Target::HeaderHelp => open_header_help(s),
        Target::HeaderPalette => {
            s.app.set_focus(Panel::Command);
            s.app.mode = Mode::Command;
            s.app.command_line.clear();
            s.arm_suggest_fade();
            s.app.suggest_highlight = 0;
            s.dirty = true;
            Flow::Continue(Effects::default())
        }
        Target::HeaderEditor => toggle_panel_editor(s),
        Target::HeaderQuit => Flow::Quit,
        Target::HeaderProfile => toggle_profile_menu(s),
        _ => Flow::Continue(Effects::default()),
    }
}

/// Open the palette help overlay — what the header's `? Help` does, whether
/// it is clicked or its `?` is pressed, so the two doors cannot drift apart.
pub(super) fn open_header_help(s: &mut AppState) -> Flow {
    s.dirty = true;
    dashboard_verb_flow(s, Verb::Help)
}

/// Enter the panel editor, or exit it if already editing — the click
/// equivalent of `e` / `Esc`. Exiting persists the arrangement.
fn toggle_panel_editor(s: &mut AppState) -> Flow {
    s.dirty = true;
    if s.app.mode != Mode::PanelEditor {
        enter_panel_editor(&mut s.app);
        return Flow::Continue(Effects::default());
    }
    s.app.mode = Mode::Normal;
    s.app.editor.reset();
    persist_flow(layout_persist_op(&s.app))
}

/// Open the profile menu, or close it if already open — the click
/// equivalent of `p` / `Esc`.
fn toggle_profile_menu(s: &mut AppState) -> Flow {
    s.dirty = true;
    if s.app.mode == Mode::ProfileMenu {
        return close_profile_menu(s);
    }
    enter_profile_menu(&mut s.app);
    Flow::Continue(Effects::default())
}

/// Close the profile menu's MODE — the pointer twin of Esc-from-Browse,
/// never the overlay close path.
fn close_profile_menu(s: &mut AppState) -> Flow {
    s.app.mode = Mode::Normal;
    s.app.profile_menu.reset();
    s.app.command_line.clear();
    Flow::Continue(Effects::default())
}

/// The profile hub: a row click moves the cursor there and switches, exactly
/// like `Enter`; a row's `[settings]` chip opens that profile's settings page,
/// the click twin of `s` on its row; `[ + new profile ]` is the click twin of
/// `n`; and the close `[x]` closes the mode.
fn click_profile_menu(s: &mut AppState, target: Target) -> Flow {
    s.dirty = true;
    match target {
        Target::ProfileRow(i) if i < s.app.profiles.len() => {
            s.app.profile_menu.cursor = i;
            switch_profile_flow(s)
        }
        Target::ProfileSettings(i) => {
            if let Some(p) = s.app.profiles.get(i) {
                let id = p.profile_id.clone();
                s.app.profile_menu.cursor = i;
                enter_profile_settings(&mut s.app, id);
            }
            Flow::Continue(Effects::default())
        }
        Target::ProfileNew => {
            s.app.profile_menu.stage = MenuStage::NamingCreate;
            s.app.profile_menu.name_error = false;
            s.app.command_line.clear();
            Flow::Continue(Effects::default())
        }
        Target::ProfileMenuClose => close_profile_menu(s),
        _ => Flow::Continue(Effects::default()),
    }
}

/// The members editor: a row click selects (the chips act on the cursor); an
/// add-stage candidate row includes it, exactly like `Enter`; and the action
/// chips are the click twins of ⇧↑ / ⇧↓ / d / a on the cursor row
/// (modal-surface parity).
fn click_members_editor(s: &mut AppState, target: Target) -> Flow {
    match target {
        Target::MemberRow(i) => {
            s.dirty = true;
            let last = s.app.profile_menu.members.len().saturating_sub(1);
            s.app.profile_menu.member_cursor = i.min(last);
            Flow::Continue(Effects::default())
        }
        Target::MemberAddRow(i) => {
            s.dirty = true;
            let last = s.app.profile_menu.candidates().len().saturating_sub(1);
            s.app.profile_menu.add_cursor = i.min(last);
            profile_or_fold_action(s, Action::MemberAdd)
        }
        Target::MemberUp => profile_or_fold_action(s, Action::MemberReorder(true)),
        Target::MemberDown => profile_or_fold_action(s, Action::MemberReorder(false)),
        Target::MemberRemove => profile_or_fold_action(s, Action::MemberRemove),
        Target::MemberAddOpen => open_member_add_stage(s),
        _ => Flow::Continue(Effects::default()),
    }
}

/// The `[ add ]` chip: step from the members list into the add stage, whose
/// rows are the catalogue minus the members.
fn open_member_add_stage(s: &mut AppState) -> Flow {
    s.dirty = true;
    if let MenuStage::Members { profile } = s.app.profile_menu.stage.clone() {
        s.app.profile_menu.stage = MenuStage::MembersAdd { profile };
        s.app.profile_menu.add_cursor = 0;
    }
    Flow::Continue(Effects::default())
}

/// A settings row (the profile hub): activate it, exactly like `Enter`.
fn click_settings_row(s: &mut AppState, i: usize) -> Flow {
    s.dirty = true;
    let rows_last = SettingsRow::ALL.len() - 1;
    let (MenuStage::Settings { profile }, Some(row)) = (
        s.app.profile_menu.stage.clone(),
        SettingsRow::ALL.get(i.min(rows_last)).copied(),
    ) else {
        return Flow::Continue(Effects::default());
    };
    s.app.profile_menu.settings_cursor = i.min(rows_last);
    let action = activate_settings_row(&mut s.app, &profile, row);
    profile_or_fold_action(s, action)
}

/// A picker row is the mouse twin of cursoring + `Enter` — a slot click opens
/// its presets; a preset/reset click applies live. The picker's close `[x]`
/// returns to the profile menu it opened from — the menu stage underneath is
/// intact.
fn click_picker(s: &mut AppState, target: Target) -> Flow {
    s.dirty = true;
    match target {
        Target::PickerRow(i) => match s.app.colour_picker.stage {
            PickerStage::Slots => {
                s.app.colour_picker.slot_cursor = i.min(ThemeSlot::ALL.len() - 1);
                s.app.colour_picker.stage = PickerStage::Presets;
                s.app.colour_picker.preset_cursor = 0;
                Flow::Continue(Effects::default())
            }
            PickerStage::Presets => {
                s.app.colour_picker.preset_cursor = i.min(RESET_ROW);
                let slot = s.app.colour_picker.slot();
                let colour = PRESETS.get(i).map(|(_, c)| *c);
                apply_theme_slot(s, slot, colour)
            }
        },
        Target::PickerClose => {
            s.app.mode = Mode::ProfileMenu;
            s.app.colour_picker.reset();
            Flow::Continue(Effects::default())
        }
        _ => Flow::Continue(Effects::default()),
    }
}

/// The destination explorer's mouse plane — every arm delegates to the SAME
/// act helper its key uses, so click ≡ key by construction rather than by a
/// parallel implementation. A directory row descends (cursor + Enter); a
/// file row only moves the cursor — files are context, never choices. The
/// close `[x]` closes the MODE — the pointer twin of Esc, never the overlay
/// close path.
fn click_explorer(s: &mut AppState, target: Target) -> Flow {
    s.dirty = true;
    match target {
        Target::ExplorerRow(i) => {
            s.app.explorer.cursor = i.min(s.app.explorer.visible_len().saturating_sub(1));
            s.app.clamp_scrolls(s.size);
            let action = explorer_descend(&s.app);
            profile_or_fold_action(s, action)
        }
        Target::ExplorerUp => {
            let action = explorer_up(&s.app);
            profile_or_fold_action(s, action)
        }
        Target::ExplorerSelect => {
            let action = explorer_select(&mut s.app);
            profile_or_fold_action(s, action)
        }
        Target::ExplorerTypePath => {
            explorer_type_path(&mut s.app);
            Flow::Continue(Effects::default())
        }
        Target::ExplorerDotfiles => {
            explorer_toggle_hidden(&mut s.app);
            s.app.clamp_scrolls(s.size);
            Flow::Continue(Effects::default())
        }
        Target::ExplorerClose => {
            s.app.mode = Mode::Normal;
            s.app.explorer.reset();
            Flow::Continue(Effects::default())
        }
        _ => Flow::Continue(Effects::default()),
    }
}

/// A grip press begins a unit drag: pick the unit up and arm the drag, which
/// `drag_update`/`drop_drag` carry to the release. The keyboard
/// (pick/move/drop) is the equivalent path.
fn press_grip(s: &mut AppState, cursor: Cursor) -> Flow {
    s.app.editor.cursor = cursor;
    s.app.editor.clamp(&s.app.layout);
    s.app.editor.picked = true;
    s.app.editor.drag = Some(Drag::Unit(cursor));
    s.dirty = true;
    Flow::Continue(Effects::default())
}

/// A divider press begins a resize drag toward the pointer.
fn press_divider(s: &mut AppState, i: usize) -> Flow {
    s.app.editor.cursor.col = i;
    s.app.editor.clamp(&s.app.layout);
    s.app.editor.drag = Some(Drag::Divider(i));
    s.dirty = true;
    Flow::Continue(Effects::default())
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{CanopyKey, MouseButton, MouseInput, MouseKind};
    use crate::render::frame::Size;
    use crate::state::fixtures::*;
    use crate::state::{update, DataRequest, Event, PersistOp};

    #[test]
    fn header_click_targets_map_to_their_key_equivalents() {
        // HeaderQuit quits; HeaderPalette opens command mode; HeaderHelp opens
        // the help overlay — each the same effect as its key.
        let mut s = ready(vec![project("a")]);
        assert!(matches!(
            apply_click(&mut s, Target::HeaderQuit),
            Flow::Quit
        ));

        let mut s = ready(vec![project("a")]);
        let _ = apply_click(&mut s, Target::HeaderPalette);
        assert_eq!(s.app.mode, Mode::Command);
        assert_eq!(s.app.focus, Panel::Command);

        let mut s = ready(vec![project("a")]);
        let _ = apply_click(&mut s, Target::HeaderHelp);
        assert!(
            matches!(s.app.overlay, Some(Overlay::Results { .. })),
            "the help overlay opened"
        );
    }

    #[test]
    fn pressing_question_mark_on_the_dashboard_opens_the_overlay_the_header_click_opens() {
        let mut clicked = ready(vec![project("a")]);
        let _ = apply_click(&mut clicked, Target::HeaderHelp);
        let mut keyed = ready(vec![project("a")]);
        let _ = update(&mut keyed, Event::Key(CanopyKey::Char('?')));
        assert!(
            matches!(keyed.app.overlay, Some(Overlay::Results { .. })),
            "the help overlay opened"
        );
        assert_eq!(keyed.app.overlay, clicked.app.overlay);
        assert_eq!(keyed.app.mode, clicked.app.mode);
    }

    #[test]
    fn a_question_mark_typed_into_the_palette_is_text_not_a_help_open() {
        let mut s = ready(vec![project("a")]);
        let _ = update(&mut s, Event::Key(CanopyKey::Char('/')));
        let _ = update(&mut s, Event::Key(CanopyKey::Char('?')));
        assert_eq!(s.app.mode, Mode::Command);
        assert_eq!(s.app.command_line, "?");
        assert_eq!(s.app.overlay, None, "no help overlay over the palette");
    }

    #[test]
    fn identity_click_toggles_the_menu_and_row_click_switches() {
        let mut s = ready_with_profiles(vec![project("a")]);
        let _ = apply_click(&mut s, Target::HeaderProfile);
        assert_eq!(s.app.mode, Mode::ProfileMenu);
        let _ = apply_click(&mut s, Target::HeaderProfile);
        assert_eq!(s.app.mode, Mode::Normal, "a second click closes");

        let _ = apply_click(&mut s, Target::HeaderProfile);
        let e = effects_of(apply_click(&mut s, Target::ProfileRow(2)));
        assert_eq!(
            e.persist,
            vec![PersistOp::SelectProfile {
                profile: "pf2".into()
            }]
        );
        assert!(e.reload_profiles);
        assert_eq!(s.app.mode, Mode::Normal, "click-to-activate closes");
    }

    #[test]
    fn members_chips_mirror_the_keyboard() {
        use crate::profile_menu::MenuStage;
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('j')));
        s.app.profile_menu.stage = MenuStage::Members {
            profile: "pf1".into(),
        };
        s.app.set_membership_lists(
            vec![project("m1"), project("m2")],
            vec![project("m1"), project("m2"), project("free")],
        );
        // [remove] acts on the cursor row, exactly like `d` (modal-surface parity).
        let e = effects_of(apply_click(&mut s, Target::MemberRemove));
        assert_eq!(
            e.persist,
            vec![PersistOp::RemoveProfileProject {
                profile: "pf1".into(),
                path: "/r/m1".into()
            }]
        );
        // A row click selects; [↓] then reorders THAT row.
        let e = effects_of(apply_click(&mut s, Target::MemberRow(1)));
        assert!(e.is_empty());
        let e = effects_of(apply_click(&mut s, Target::MemberDown));
        assert_eq!(
            e.persist,
            vec![PersistOp::ReorderProfileProject {
                profile: "pf1".into(),
                path: "/r/m2".into(),
                up: false
            }]
        );
        // [ add ] opens the add stage; a candidate click includes it.
        let e = effects_of(apply_click(&mut s, Target::MemberAddOpen));
        assert!(e.is_empty());
        assert!(matches!(
            s.app.profile_menu.stage,
            MenuStage::MembersAdd { .. }
        ));
        let e = effects_of(apply_click(&mut s, Target::MemberAddRow(0)));
        assert_eq!(
            e.persist,
            vec![PersistOp::AddProfileProject {
                profile: "pf1".into(),
                path: "/r/free".into()
            }]
        );
    }

    #[test]
    fn picker_close_x_returns_to_the_menu() {
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        update(&mut s, Event::Key(CanopyKey::Char('j'))); // → Colours
        let _ = update(&mut s, Event::Key(CanopyKey::Enter));
        assert_eq!(s.app.mode, Mode::ColourPicker);
        // The `[x]` closes the PICKER — back to the menu it opened from,
        // its settings stage intact (modal-surface parity).
        let e = effects_of(apply_click(&mut s, Target::PickerClose));
        assert!(e.is_empty());
        assert_eq!(s.app.mode, Mode::ProfileMenu);
        assert!(matches!(
            s.app.profile_menu.stage,
            crate::profile_menu::MenuStage::Settings { .. }
        ));
    }

    /// The close X's cell on the current frame, resolved through the pure
    /// hit-test so the sweep exercises drawn cells rather than a target a
    /// test invented.
    fn close_x_target(s: &mut AppState) -> Option<Target> {
        let vm = s.snapshot();
        let cr = crate::view::modal_close_rect(crate::view::modal_rect(vm.size)).unwrap();
        crate::hit::hit_test(&vm, cr.x, cr.y)
    }

    // modal-surface parity: every mode drawn as a modal carries the close `[x]` —
    // the pointer twin of Esc — resolved through the pure hit-test and
    // closing through the MODE's own path. (The Overlay shell's X is
    // proven by the modal focus-and-close rule suite; this sweep covers the
    // mode-modals, so a future stage that forgets its X turns CI red.) One
    // test per modal surface: the set IS the inventory, and shortening it by
    // dropping a surface is the failure these tests exist to prevent. For
    // whoever adds the next modal surface: registering here is a DUTY, not a
    // courtesy. The draw and hit `if vm.mode ==` chains compile clean when a
    // surface is forgotten, and these hand-listed fixtures — this sweep,
    // `keymap`'s `ALL`/`app_in`/`fingerprint`, `Target::is_button`, and the
    // `click_on` mirrors — are the only things that turn CI red when it is.

    #[test]
    fn the_profile_menu_carries_the_close_x_in_every_stage() {
        use crate::profile_menu::MenuStage;
        let stages = vec![
            MenuStage::Browse,
            MenuStage::Settings {
                profile: "pf0".into(),
            },
            MenuStage::Members {
                profile: "pf0".into(),
            },
            MenuStage::MembersAdd {
                profile: "pf0".into(),
            },
            MenuStage::NamingCreate,
            MenuStage::ConfirmDelete {
                profile: "pf1".into(),
                back_to_settings: false,
            },
        ];
        for stage in stages {
            let mut s = ready_with_profiles(vec![project("a")]);
            update(&mut s, Event::Key(CanopyKey::Char('p')));
            s.app.profile_menu.stage = stage.clone();
            assert_eq!(
                close_x_target(&mut s),
                Some(Target::ProfileMenuClose),
                "menu stage {stage:?} lost its close X"
            );
            let _ = apply_click(&mut s, Target::ProfileMenuClose);
            assert_eq!(s.app.mode, Mode::Normal, "the X closes from {stage:?}");
        }
    }

    #[test]
    fn the_colour_picker_carries_the_close_x_in_both_stages() {
        for picker_stage in [
            crate::colour_picker::PickerStage::Slots,
            crate::colour_picker::PickerStage::Presets,
        ] {
            let mut s = ready_with_profiles(vec![project("a")]);
            update(&mut s, Event::Key(CanopyKey::Char('p')));
            update(&mut s, Event::Key(CanopyKey::Char('s')));
            update(&mut s, Event::Key(CanopyKey::Char('j'))); // → Colours
            let _ = update(&mut s, Event::Key(CanopyKey::Enter));
            s.app.colour_picker.stage = picker_stage;
            assert_eq!(
                close_x_target(&mut s),
                Some(Target::PickerClose),
                "picker stage {picker_stage:?} lost its close X"
            );
            let _ = apply_click(&mut s, Target::PickerClose);
            assert_eq!(
                s.app.mode,
                Mode::ProfileMenu,
                "the picker's X returns to the menu"
            );
        }
    }

    #[test]
    fn the_explorer_carries_the_close_x_with_and_without_entries() {
        // The destination explorer, driven through REAL entry — and in both
        // its visual states, because the X must survive every one.
        for with_entries in [true, false] {
            let mut s = ready(vec![project("a")]);
            s.app.home_dir = Some("/home/dev".into());
            let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('a'))));
            assert_eq!(
                e.list_dir,
                Some(std::path::PathBuf::from("/home/dev")),
                "entry requests the start listing on the way in"
            );
            s.app.explorer.apply(if with_entries {
                home_listing()
            } else {
                crate::explorer::DirListing {
                    path: std::path::PathBuf::from("/home/dev"),
                    entries: vec![],
                    omitted: 0,
                    error: Some("permission denied".into()),
                }
            });
            assert_eq!(
                close_x_target(&mut s),
                Some(Target::ExplorerClose),
                "the explorer lost its close X (entries={with_entries})"
            );
            let _ = apply_click(&mut s, Target::ExplorerClose);
            assert_eq!(s.app.mode, Mode::Normal, "the X closes the MODE");
            assert!(s.app.overlay.is_none(), "…and never touches the overlay");
            assert_eq!(
                s.app.explorer,
                crate::explorer::ExplorerState::default(),
                "closing rests the state"
            );
        }
    }

    #[test]
    fn project_settings_carries_the_close_x_in_both_acting_states() {
        // Driven through real entry (`s` on the rail), in the two states it
        // accepts acts in: the plain list, and the raw-transcript
        // acknowledgement, which puts its own Cancel chip on the same frame.
        for (label, arm) in [
            ("plain", &(|_: &mut AppState| {}) as &dyn Fn(&mut AppState)),
            ("confirm-raw", &|s: &mut AppState| {
                s.app.project_settings.confirm_raw = true;
            }),
        ] {
            let mut s = project_settings_state(crate::ProjectInitialization::Initialized);
            let _ = update(&mut s, Event::Key(CanopyKey::Char('s')));
            assert_eq!(s.app.mode, Mode::ProjectSettings, "real entry ({label})");
            arm(&mut s);
            assert_eq!(
                close_x_target(&mut s),
                Some(Target::ProjectSettingsClose),
                "project settings lost its close X ({label})"
            );
            let _ = apply_click(&mut s, Target::ProjectSettingsClose);
            assert_eq!(s.app.mode, Mode::Normal, "the X closes the MODE ({label})");
            assert!(s.app.overlay.is_none(), "…and never touches the overlay");
        }
    }

    #[test]
    fn a_running_project_settings_keeps_its_close_x_cells_and_refuses_both_doors() {
        // Its third state is the one exception, and it is symmetric rather
        // than missing: while an operation is in flight the modal is FROZEN
        // — `apply_click` refuses every target and `on_key_project_settings`
        // returns `Action::None` — so no act exists by either door, and the
        // hint bar now teaches none (`HintContext::ProjectSettingsRunning`).
        // That is modal-surface parity satisfied, not waived; the X's cells stay resolved
        // so the parity returns with the operation.
        let mut s = project_settings_state(crate::ProjectInitialization::Initialized);
        let _ = update(&mut s, Event::Key(CanopyKey::Char('s')));
        s.app.project_settings.running = true;
        assert_eq!(
            close_x_target(&mut s),
            Some(Target::ProjectSettingsClose),
            "the X keeps its cells while running"
        );
        let _ = apply_click(&mut s, Target::ProjectSettingsClose);
        assert_eq!(s.app.mode, Mode::ProjectSettings, "the click is refused");
        let _ = update(&mut s, Event::Key(CanopyKey::Esc));
        assert_eq!(
            s.app.mode,
            Mode::ProjectSettings,
            "and so is the key — the freeze is symmetric"
        );
        assert!(
            crate::keymap::hints(crate::keymap::HintContext::ProjectSettingsRunning).is_empty(),
            "a frozen surface teaches nothing"
        );
    }

    #[test]
    fn every_add_project_affordance_arrives_through_the_one_door() {
        // the destination explorer: `a`, the rail `+`, and both twins of the wizard primary
        // land in Mode::Explorer requesting the SAME start directory.
        let start = Some(std::path::PathBuf::from("/home/dev"));

        let mut s = ready(vec![project("a")]);
        s.app.home_dir = Some("/home/dev".into());
        let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('a'))));
        assert_eq!((s.app.mode, e.list_dir), (Mode::Explorer, start.clone()));

        let mut s = ready(vec![project("a")]);
        s.app.home_dir = Some("/home/dev".into());
        let m = click_on(Target::RailAdd, &mut s);
        let e = effects_of(update(&mut s, Event::Mouse(m)));
        assert_eq!((s.app.mode, e.list_dir), (Mode::Explorer, start.clone()));

        // The wizard, by key and by click, on an empty registry.
        for by_click in [false, true] {
            let mut s = AppState::new(vec![], Size::new(120, 40));
            s.app.splash.skip();
            s.app.home_dir = Some("/home/dev".into());
            assert!(s.app.showing_wizard());
            let e = if by_click {
                let m = click_on(Target::WizardAdd, &mut s);
                effects_of(update(&mut s, Event::Mouse(m)))
            } else {
                effects_of(update(&mut s, Event::Key(CanopyKey::Enter)))
            };
            assert_eq!(s.app.mode, Mode::Explorer, "by_click={by_click}");
            assert_eq!(e.list_dir, start.clone(), "by_click={by_click}");
            assert!(s.app.overlay.is_none(), "the wizard's overlay dropped");
        }
    }

    #[test]
    fn explorer_clicks_mirror_their_keys() {
        // modal-surface parity: every act the hints teach is reachable by click, and every
        // click is a key's twin — proven through the PURE hit-test (`click_on`
        // finds the drawn cell), so a solver drift shows up here.
        //
        // A directory-row click ≡ cursor + Enter.
        let mut s = browsing();
        let m = click_on(Target::ExplorerRow(1), &mut s); // → beta
        let e = effects_of(update(&mut s, Event::Mouse(m)));
        assert_eq!(e.list_dir, Some(std::path::PathBuf::from("/home/dev/beta")));
        let mut keyed = browsing();
        update(&mut keyed, Event::Key(CanopyKey::Char('j')));
        let ek = effects_of(update(&mut keyed, Event::Key(CanopyKey::Enter)));
        assert_eq!(e.list_dir, ek.list_dir, "row-click ≡ Enter");

        // The back row ≡ Backspace.
        let mut s = browsing();
        let m = click_on(Target::ExplorerUp, &mut s);
        let e = effects_of(update(&mut s, Event::Mouse(m)));
        assert_eq!(e.list_dir, Some(std::path::PathBuf::from("/home")));

        // The select chip ≡ `s` — both persist the OPEN folder and land in
        // Normal (so the success notice reaches the dashboard).
        let mut s = browsing();
        let m = click_on(Target::ExplorerSelect, &mut s);
        let e = effects_of(update(&mut s, Event::Mouse(m)));
        assert_eq!(
            e.persist,
            vec![PersistOp::AddProject {
                path: "/home/dev".into()
            }]
        );
        assert_eq!(s.app.mode, Mode::Normal);
        let mut keyed = browsing();
        let ek = effects_of(update(&mut keyed, Event::Key(CanopyKey::Char('s'))));
        assert_eq!(e.persist, ek.persist, "the chip ≡ `s`");
        assert_eq!(keyed.app.mode, Mode::Normal);

        // The type-path chip ≡ `i`.
        let mut s = browsing();
        let m = click_on(Target::ExplorerTypePath, &mut s);
        update(&mut s, Event::Mouse(m));
        assert_eq!(s.app.mode, Mode::AddProject);
        assert!(s.app.command_line.is_empty(), "today's flow exactly");

        // The dotfiles chip ≡ `.` — the act the status line teaches, reachable
        // by pointer (modal-surface parity; the one mirror the first cut was missing).
        let mut s = browsing();
        let m = click_on(Target::ExplorerDotfiles, &mut s);
        update(&mut s, Event::Mouse(m));
        assert!(s.app.explorer.show_hidden, "the chip revealed dot-entries");
        assert_eq!(s.app.explorer.visible_len(), 4);
        let mut keyed = browsing();
        update(&mut keyed, Event::Key(CanopyKey::Char('.')));
        assert_eq!(
            s.app.explorer, keyed.app.explorer,
            "the chip and the key leave IDENTICAL state"
        );
        // …and it toggles back.
        let m = click_on(Target::ExplorerDotfiles, &mut s);
        update(&mut s, Event::Mouse(m));
        assert!(!s.app.explorer.show_hidden);

        // The X ≡ Esc.
        let mut s = browsing();
        let m = click_on(Target::ExplorerClose, &mut s);
        update(&mut s, Event::Mouse(m));
        assert_eq!(s.app.mode, Mode::Normal);
        assert_eq!(s.app.explorer, crate::explorer::ExplorerState::default());
        let mut keyed = browsing();
        update(&mut keyed, Event::Key(CanopyKey::Esc));
        assert_eq!(keyed.app.mode, Mode::Normal);
    }

    #[test]
    fn a_file_row_click_only_moves_the_cursor() {
        let mut s = browsing();
        let m = click_on(Target::ExplorerRow(2), &mut s); // → notes.md
        let e = effects_of(update(&mut s, Event::Mouse(m)));
        assert!(e.list_dir.is_none(), "files are context, never choices");
        assert_eq!(s.app.explorer.cursor, 2, "the cursor still moved");
        assert_eq!(s.app.mode, Mode::Explorer);
    }

    #[test]
    fn menu_close_x_closes_the_mode_not_an_overlay() {
        use crate::profile_menu::MenuStage;
        let mut s = ready_with_profiles(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('p')));
        update(&mut s, Event::Key(CanopyKey::Char('s')));
        assert!(matches!(
            s.app.profile_menu.stage,
            MenuStage::Settings { .. }
        ));
        // The `[x]` closes the MODE from any stage and never
        // touches the overlay plane.
        let e = effects_of(apply_click(&mut s, Target::ProfileMenuClose));
        assert!(e.is_empty());
        assert_eq!(s.app.mode, Mode::Normal);
        assert_eq!(s.app.profile_menu.stage, MenuStage::Browse);
        assert!(s.app.overlay.is_none());
    }

    #[test]
    fn picker_clicks_mirror_the_keyboard() {
        use crate::colour_picker::PickerStage;
        use crate::theme::{ThemeSlot, PRESETS};
        let mut s = ready_with_profiles(vec![project("a")]);
        for k in ['p', 'c'] {
            update(&mut s, Event::Key(CanopyKey::Char(k)));
        }
        // A slot click opens its presets…
        let _ = apply_click(&mut s, Target::PickerRow(4)); // text
        assert_eq!(s.app.colour_picker.stage, PickerStage::Presets);
        assert_eq!(s.app.colour_picker.slot_cursor, 4);
        // …and a preset click applies live, exactly like Enter.
        let e = effects_of(apply_click(&mut s, Target::PickerRow(2))); // green
        assert_eq!(
            s.snapshot().theme.overrides().get(ThemeSlot::Text),
            Some(PRESETS[2].1)
        );
        assert_eq!(
            e.persist,
            vec![PersistOp::SetProfileTheme {
                profile: "pf0".into(),
                slot: "text".into(),
                colour: crate::theme::colour_to_hex(PRESETS[2].1),
            }]
        );
    }

    #[test]
    fn clicking_the_close_x_dismisses_the_modal_and_flashes() {
        let mut s = ready(vec![project("a")]);
        s.app.open_overlay(Overlay::Results {
            title: "t".into(),
            lines: vec!["ok".into()],
            scroll: 0,
            footer: None,
            truncated: false,
        });
        let _ = apply_click(&mut s, Target::OverlayClose);
        assert!(s.app.overlay.is_none(), "the X dismisses the modal");
        assert_eq!(
            s.press_pulse, 1.0,
            "the close X press-flashes like any button"
        );
        assert_eq!(s.pressed, Some(Target::OverlayClose));
    }

    #[test]
    fn rail_buttons_raise_the_same_effects_as_keys_and_flash() {
        // Add → raises the destination explorer (like the `a` key, through the
        // one entry door — the destination explorer) and press-flashes.
        let mut s = ready(vec![project("a"), project("b")]);
        s.app.home_dir = Some("/home/dev".into());
        let e = effects_of(apply_click(&mut s, Target::RailAdd));
        assert_eq!(s.app.mode, Mode::Explorer);
        assert_eq!(e.list_dir, Some(std::path::PathBuf::from("/home/dev")));
        assert_eq!(s.pressed, Some(Target::RailAdd));
        // Remove selected → the same PersistOp the `d` key raises.
        let mut s = ready(vec![project("a"), project("b")]);
        match apply_click(&mut s, Target::RailRemove) {
            Flow::Continue(e) => assert!(matches!(
                e.persist.as_slice(),
                [PersistOp::RemoveProject { .. }]
            )),
            Flow::Quit => panic!("remove is not quit"),
        }
        // Reorder up → ReorderProject { up: true } on the selected path.
        let mut s = ready(vec![project("a"), project("b")]);
        match apply_click(&mut s, Target::RailReorderUp) {
            Flow::Continue(e) => assert!(matches!(
                e.persist.as_slice(),
                [PersistOp::ReorderProject { up: true, .. }]
            )),
            Flow::Quit => panic!("reorder is not quit"),
        }
    }

    #[test]
    fn every_button_target_flashes_on_click() {
        for target in [
            Target::HeaderHelp,
            Target::HeaderPalette,
            Target::HeaderQuit,
            Target::OverlayConfirm,
            Target::OverlayCancel,
            Target::OverlayClose,
            Target::RailAdd,
            Target::RailReorderUp,
            Target::RailReorderDown,
            Target::RailRemove,
            Target::RepositorySettings,
            Target::WizardAdd,
            Target::ExplorerUp,
            Target::ExplorerSelect,
            Target::ExplorerTypePath,
            Target::ExplorerDotfiles,
            Target::ExplorerClose,
        ] {
            assert!(target.is_button(), "{target:?} must be a button");
            let mut s = ready(vec![project("a")]);
            let _ = apply_click(&mut s, target.clone());
            assert_eq!(
                s.pressed.as_ref(),
                Some(&target),
                "clicking {target:?} arms the press-pulse"
            );
        }
        // A selection/focus target is not a button and does not flash.
        assert!(!Target::ProjectRow(0).is_button());
        assert!(
            !Target::ExplorerRow(0).is_button(),
            "a list row selects; it is not a chip"
        );
        let mut s = ready(vec![project("a")]);
        let _ = apply_click(&mut s, Target::ProjectRow(0));
        assert!(s.pressed.is_none());
    }

    #[test]
    fn suggestion_click_inserts_and_never_executes() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, Event::Key(CanopyKey::Char('/')));
        for c in "stat".chars() {
            update(&mut s, Event::Key(CanopyKey::Char(c)));
        }
        // Click the first suggestion row (geometry-independent: drive the
        // reducer's target arm directly through a real hit on the panel).
        let vm = s.snapshot();
        assert!(!vm.suggestions.is_empty());
        let rect = crate::view::suggest_panel_rect(vm.size, vm.suggestions.len());
        assert!(rect.h > 0, "panel present at 80x24? size={:?}", vm.size);
        let flow = update(
            &mut s,
            Event::Mouse(MouseInput {
                kind: MouseKind::Down(MouseButton::Left),
                col: rect.x + 2,
                row: rect.y + 1,
            }),
        );
        match flow {
            Flow::Continue(e) => {
                assert!(e.dispatches.is_empty(), "insert-only: no dispatch");
                assert!(e.persist.is_empty());
            }
            Flow::Quit => panic!("a suggestion click must never quit"),
        }
        assert_eq!(s.app.command_line, "status", "the click inserted the row");
        assert_eq!(s.app.mode, Mode::Command, "still editing");
    }

    #[test]
    fn clicking_a_project_row_matches_the_keyboard_selection() {
        // A click on the second project row must produce the same selection and
        // the same bus effects as pressing Down from the top.
        let mut sk = ready(vec![project("a"), project("b"), project("c")]);
        let fk = update(&mut sk, Event::Key(CanopyKey::Down));
        let mut sm = ready(vec![project("a"), project("b"), project("c")]);
        // col1 inner content starts at (1,2); project rows at y=2,3,4.
        let fm = update(&mut sm, click(3, 3));
        assert_eq!(sk.app.selected, 1);
        assert_eq!(sm.app.selected, 1, "the click selected the clicked row");
        match (fk, fm) {
            (Flow::Continue(ek), Flow::Continue(em)) => {
                assert_eq!(ek.data_requests, em.data_requests, "same bus effects");
                assert!(em
                    .data_requests
                    .contains(&DataRequest::SetActive(Some("/r/b".into()))));
            }
            _ => panic!("expected Continue"),
        }
    }

    #[test]
    fn clicking_a_column_focuses_that_panel() {
        let mut s = ready(vec![project("a")]);
        assert_eq!(s.app.focus, Panel::Projects);
        update(&mut s, click(40, 10)); // deep in the detail column
        assert_eq!(s.app.focus, Panel::Detail);
        update(&mut s, click(100, 6)); // deep in the graphs column
        assert_eq!(s.app.focus, Panel::Graphs);
    }

    #[test]
    fn clicking_the_command_bar_opens_command_mode() {
        let mut s = ready(vec![project("a")]);
        update(&mut s, click(10, 39)); // the bottom command row
        assert_eq!(s.app.focus, Panel::Command);
        assert_eq!(s.app.mode, Mode::Command);
    }

    #[test]
    fn the_wizard_primary_click_raises_the_explorer_and_flashes() {
        let mut s = AppState::new(vec![], Size::new(120, 40));
        s.app.splash.skip();
        s.reduced_motion = false;
        s.app.home_dir = Some("/home/dev".into());
        assert!(s.app.showing_wizard());
        let e = effects_of(apply_click(&mut s, Target::WizardAdd));
        assert_eq!(
            e.list_dir,
            Some(std::path::PathBuf::from("/home/dev")),
            "the click twin requests the same start listing the key does"
        );
        assert_eq!(
            s.app.mode,
            Mode::Explorer,
            "the wizard begins with browsing (the destination explorer)"
        );
        assert!(s.app.overlay.is_none(), "the wizard dismissed");
        assert_eq!(s.press_pulse, 1.0, "the primary button flashed on click");
        assert_eq!(s.pressed, Some(Target::WizardAdd));
    }
}
