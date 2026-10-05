//! The target matrix: every live surface, exhausted cell by cell, exposes
//! the targets it draws — the negative space around chips, rows, scrollbars,
//! tabs and frames pinned as strongly as the positive regions, one test per
//! surface so a regression names the surface that lost a target.

use crate::hit::fixtures::{
    explorer_state, populated_viewer_state, profiled_state, project, state_with_detail,
};
use crate::hit::{hit_test, ScrollRegion, Target};
use crate::input::{CanopyKey, Mode, MouseButton, MouseInput, MouseKind};
use crate::profile_menu::MenuStage;
use crate::render::frame::Size;
use crate::state::{update, AppState, Flow};
use crate::{Event, Panel};

/// Every distinct target a scan of every cell of `state`'s surface resolves.
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

/// The profiled dashboard with nothing over it, so its header is live.
fn header_dashboard() -> AppState {
    let mut dashboard = profiled_state(120, 40);
    dashboard.app.overlay = None;
    dashboard
}

/// A left press and release at `col` on the header row; the flow the press
/// produced, which is where a click acts.
fn click_header_at(state: &mut AppState, col: u16) -> Flow {
    let press = |kind| Event::Mouse(MouseInput { kind, col, row: 0 });
    let flow = update(state, press(MouseKind::Down(MouseButton::Left)));
    let _ = update(state, press(MouseKind::Up(MouseButton::Left)));
    flow
}

/// A profile plane with its menu open on the hub.
fn open_profile_menu() -> AppState {
    let mut profiles = profiled_state(120, 40);
    crate::input::enter_profile_menu(&mut profiles.app);
    profiles
}

#[test]
fn the_dashboard_exposes_its_header_rail_card_and_command_targets() {
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
}

/// The header teaches a key glyph beside every item; a click on the item and
/// a press of the glyph must leave the dashboard in the same state, so no
/// header item is clickable without being keyable or the other way round.
#[test]
fn every_header_item_is_reachable_by_its_key_and_by_its_click() {
    let profile_name = header_dashboard().snapshot().profile_name;
    let items = crate::bars::header_cluster(120, profile_name.as_deref());
    assert_eq!(items.len(), 5, "the identity item is in the cluster");
    for item in items {
        let glyph = item.key.chars().next().unwrap();
        let mut keyed = header_dashboard();
        let key_flow = update(&mut keyed, Event::Key(CanopyKey::Char(glyph)));
        let mut clicked = header_dashboard();
        let click_flow = click_header_at(&mut clicked, item.span.start);
        let action = item.span.action;
        assert_eq!(
            matches!(key_flow, Flow::Quit),
            matches!(click_flow, Flow::Quit),
            "{action:?}: quit"
        );
        assert_eq!(keyed.app.mode, clicked.app.mode, "{action:?}: mode");
        assert_eq!(keyed.app.focus, clicked.app.focus, "{action:?}: focus");
        assert_eq!(
            keyed.app.overlay, clicked.app.overlay,
            "{action:?}: overlay"
        );
        assert_eq!(
            keyed.app.command_line, clicked.app.command_line,
            "{action:?}: command line"
        );
    }
}

#[test]
fn an_overflowing_rail_exposes_its_scrollbar_and_add_button() {
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
}

#[test]
fn every_degraded_tier_exposes_targets_under_every_focus() {
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
}

#[test]
fn a_mixed_column_keeps_the_repository_buttons() {
    let mut mixed = state_with_detail(120, 40);
    let repository = mixed.app.layout.columns[1].units.remove(0);
    mixed.app.layout.columns[2].units.insert(1, repository);
    assert_scanned(
        &mut mixed,
        &[Target::RepositoryRecallViewer, Target::RepositorySettings],
    );
}

#[test]
fn the_panel_editor_exposes_grips_and_dividers() {
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
}

#[test]
fn project_settings_exposes_close_cancel_and_the_first_action() {
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
    settings.app.mode = Mode::ProjectSettings;
    settings.app.project_settings.confirm_raw = true;
    assert_scanned(
        &mut settings,
        &[
            Target::ProjectSettingsClose,
            Target::ProjectSettingsCancel,
            Target::ProjectSettingsAction(0),
        ],
    );
}

#[test]
fn the_profile_hub_exposes_its_rows_chips_new_row_and_close() {
    let mut profiles = open_profile_menu();
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
}

#[test]
fn the_profile_settings_members_and_add_stages_expose_their_rows_and_chips() {
    let mut profiles = open_profile_menu();
    profiles.app.profile_menu.stage = MenuStage::Settings {
        profile: "pf0".to_owned(),
    };
    assert_scanned(&mut profiles, &[Target::SettingsRow(0)]);
    profiles.app.set_membership_lists(
        vec![project("a"), project("b")],
        vec![project("a"), project("b"), project("c")],
    );
    profiles.app.profile_menu.stage = MenuStage::Members {
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
    profiles.app.profile_menu.stage = MenuStage::MembersAdd {
        profile: "pf0".to_owned(),
    };
    assert_scanned(&mut profiles, &[Target::MemberAddRow(0)]);
}

#[test]
fn both_colour_picker_stages_expose_their_rows_and_close() {
    let mut profiles = open_profile_menu();
    profiles.app.mode = Mode::ColourPicker;
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
}

#[test]
fn the_explorer_exposes_its_close_back_row_chip_rows_and_scrollbar() {
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
}

#[test]
fn a_confirm_overlay_exposes_its_buttons_close_and_body() {
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
}

#[test]
fn a_results_overlay_exposes_close_body_and_scrollbar() {
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
}

#[test]
fn the_suggest_panel_exposes_its_first_row() {
    let mut suggestions = state_with_detail(120, 40);
    suggestions.app.mode = Mode::Command;
    suggestions.app.command_line = "/".to_owned();
    assert_scanned(&mut suggestions, &[Target::SuggestRow(0)]);
}

#[test]
fn the_recall_viewer_exposes_every_pane_target() {
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
}

#[test]
fn each_single_column_viewer_pane_exposes_its_tab() {
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
