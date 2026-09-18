//! The pure dashboard renderer: `view::draw(&ViewModel, backend, &mut Frame)`.
//!
//! The single renderer both backends flush — no `&mut AppState`, no I/O, no
//! engine types — so it is a deterministic function of the snapshot and every
//! panel step's headless/snapshot test reuses it. The backend participates
//! only through the [`RenderBackend::plot`] seam (sub-cell graphs choose
//! braille or the block floor there); everything else is pure [`Frame`]
//! drawing.
//!
//! One module per surface holds its geometry solvers and its `draw_*`:
//! `modal` the centered frame every modal surface shares (one solver feeds
//! the draw and the hit-test), `profile_menu`, `project_settings`,
//! `explorer` and `colour_picker` the four mode modals, `overlay` the
//! dispatch overlay family, `grid` the responsive column grid with its cards
//! and panels, `rail` the projects rail, and `text` the clipping and eliding
//! every surface writes through. This file keeps the root `draw`, the
//! startup sweep, the suggest panel, and the re-exports that leave every
//! `crate::view::…` path where it was.

mod colour_picker;
mod explorer;
mod grid;
mod modal;
mod overlay;
mod profile_menu;
mod project_settings;
mod rail;
mod text;

#[cfg(test)]
mod fixtures;

use crate::col2::sanitize_for_terminal;
use crate::layout::{bottom_chrome_rows, choose_mode, LayoutMode, Rect};
use crate::render::frame::{Frame, Style};
use crate::render::{Capabilities, RenderBackend};
use crate::state::ViewModel;

use colour_picker::draw_colour_picker;
use explorer::draw_explorer;
use grid::{draw_full, draw_single, draw_too_small, draw_two_col};
use overlay::draw_overlay;
use profile_menu::draw_profile_menu;
use project_settings::draw_project_settings;
use text::clip_line;

pub use colour_picker::{colour_picker_row_rect, colour_picker_rows};
pub use explorer::{
    explorer_button_rects, explorer_dotfiles_rect, explorer_list_area, explorer_row_rect,
    explorer_up_rect, EXPLORER_BUTTONS, EXPLORER_DOTFILES_CHIP,
};
pub(crate) use grid::draw_command;
pub use modal::{
    modal_close_rect, modal_primary_rect, modal_rect, CONFIRM_BUTTONS, MIN_MODAL_COLS,
    MIN_MODAL_ROWS,
};
pub use profile_menu::{
    members_action_chips, members_chip_target, profile_menu_row_rect, profile_menu_settings_rect,
    MEMBERS_CHIPS, PROFILE_NEW_ROW, PROFILE_SETTINGS_CHIP,
};
pub use project_settings::{
    project_settings_action_rects, project_settings_body_rect, project_settings_cancel_rect,
    project_settings_display_rows, ProjectSettingsDisplayRow,
};
pub(crate) use rail::{draw_projects_read_only, rail_action_layout, visible_window};

/// The suggest panel's row cap (a bounded list; adapts down to the body).
pub const SUGGEST_MAX_ROWS: usize = 8;

/// The Rift accent style (selection + overlay chrome) — the theme's accent.
pub(super) const ACCENT: Style = Style::fg(crate::theme::ACCENT);

/// Render the whole dashboard for `vm` into `frame`, choosing the responsive
/// layout tier for the current size. `backend` participates through
/// the plot seam (column-3 graphs) and, once at the top, the Unicode
/// capability — the single glyph-substitution switch the theme's icon table
/// resolves through (draw code never branches on capability elsewhere).
///
/// The layers paint in a fixed order: the surface background, the header,
/// the body (a dashboard tier or the Recall Viewer), then the modal layer,
/// then the startup sweep over everything — so a notice always lands on top
/// of a mode modal, and the sweep's reveal line wipes the modals in with the
/// panels.
pub fn draw(vm: &ViewModel, backend: &mut dyn RenderBackend, frame: &mut Frame) {
    let area = frame.rect();
    if area.w == 0 || area.h == 0 {
        return;
    }
    let unicode = backend.capabilities().contains(Capabilities::UNICODE);

    // The surface paints ITS OWN background first (the runtime surface theme: the background
    // slot is every element's backdrop, not just the header's) — every
    // foreground-only write beneath inherits it through the frame's
    // transparent-Default compose rule, so a profile's background repaints
    // panels, gaps, and bars alike. Floor tiers resolve the colour away.
    crate::chrome::fill(frame, area, surface_fill(vm));

    // Below the minimum, show only the centered notice (no header/panels).
    if choose_mode(vm.size) == LayoutMode::TooSmall {
        draw_too_small(frame, area);
        return;
    }

    let header = Rect {
        x: area.x,
        y: area.y,
        w: area.w,
        h: 1,
    };
    crate::bars::draw_header(vm, vm.theme, unicode, frame, header);
    let body = Rect {
        x: area.x,
        y: area.y + 1,
        w: area.w,
        h: area.h.saturating_sub(1),
    };
    draw_body(vm, backend, frame, body, unicode);
    draw_modal_layer(vm, unicode, frame);
    apply_startup_sweep(vm, frame, body);
}

/// Where the suggest panel sits: directly above the bottom chrome (hint bar +
/// command bar), bounded by [`SUGGEST_MAX_ROWS`], the suggestion count, and
/// the body's spare rows — zero-sized when there are none (the panel clamps
/// away; the bar's own line remains). Shared with `hit_test` so clicks
/// resolve to the drawn rows.
#[must_use]
pub fn suggest_panel_rect(size: crate::render::frame::Size, rows: usize) -> Rect {
    let body = Rect {
        x: 0,
        y: 1,
        w: size.w,
        h: size.h.saturating_sub(1),
    };
    let chrome = bottom_chrome_rows(body.h);
    let avail = body.h.saturating_sub(chrome + 1);
    let want = u16::try_from(rows.min(SUGGEST_MAX_ROWS))
        .unwrap_or(0)
        .saturating_add(2);
    let h = want.min(avail);
    if h < 3 || body.w < 12 {
        return Rect {
            x: body.x,
            y: body.y,
            w: 0,
            h: 0,
        };
    }
    let y = body
        .y
        .saturating_add(body.h)
        .saturating_sub(chrome)
        .saturating_sub(h);
    Rect {
        x: body.x,
        y,
        w: body.w,
        h,
    }
}

/// The body under the header: the Recall Viewer when that surface is up,
/// else the dashboard tier the size chooses.
fn draw_body(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    body: Rect,
    unicode: bool,
) {
    if vm.surface == crate::recall_viewer::Surface::RecallViewer {
        crate::recall_viewer::draw(vm, frame, unicode);
        return;
    }
    match choose_mode(vm.size) {
        LayoutMode::TwoCol => draw_two_col(vm, backend, frame, body, unicode),
        LayoutMode::SinglePanel => draw_single(vm, backend, frame, body, unicode),
        // Full3Col (TooSmall handled above).
        _ => draw_full(vm, backend, frame, body, unicode),
    }
}

/// The modal layer over the body: the suggest panel, the open mode's modal,
/// and the dispatch overlay last — a notice paints on top of a mode modal
/// exactly as it does over the dashboard.
fn draw_modal_layer(vm: &ViewModel, unicode: bool, frame: &mut Frame) {
    use crate::input::Mode;
    // The suggest panel expands ABOVE the command bar, borrowing body rows as
    // an overlay draw — the fixed command rows and the layout solver are
    // untouched; it clamps away on tiny terminals.
    if !vm.suggestions.is_empty() {
        draw_suggest_panel(vm, frame);
    }
    // A mode modal is a mode, not an overlay: a centered frame over the shared
    // modal geometry and scrim, its state on the snapshot.
    match vm.mode {
        Mode::ProfileMenu => draw_profile_menu(vm, unicode, frame),
        Mode::ColourPicker => draw_colour_picker(vm, unicode, frame),
        Mode::Explorer => draw_explorer(vm, unicode, frame),
        Mode::ProjectSettings => draw_project_settings(vm, frame),
        _ => (),
    }
    // The dispatch overlay layers over the body (the dispatch posture):
    // results scroll, confirms gate, the running line holds during a dispatch.
    // It sizes itself as a centered modal from `vm.size`.
    if vm.overlay.is_some() {
        draw_overlay(vm, unicode, frame);
    }
}

/// The one-shot startup sweep: a top-to-bottom reveal as the panels first
/// fill after the splash. At 1.0 (reduced motion / headless / settled) it is
/// a no-op; while sweeping, the body rows below the reveal line are blanked
/// so the dashboard wipes in rather than popping. Pure dt-tween — the settled
/// frame (which every headless/snapshot render sees) is always the full one.
fn apply_startup_sweep(vm: &ViewModel, frame: &mut Frame, body: Rect) {
    let sweep = vm.sweep.clamp(0.0, 1.0);
    if sweep >= 1.0 || body.h == 0 {
        return;
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let revealed = (f32::from(body.h) * sweep) as u16;
    let hidden = Rect {
        y: body.y + revealed,
        h: body.h.saturating_sub(revealed),
        ..body
    };
    crate::chrome::fill(frame, hidden, surface_fill(vm));
}

/// Draw the suggest panel: `verb  usage` + a muted one-line description per
/// row, the highlight bar, and the animator contract fade — unselected text ramps from
/// the muted tone to the settled foreground as `suggest_fade` advances
/// (reduced motion and ASCII land at 1.0, snapshotting the final frame).
/// Every string passes the shared sanitizer (labels are user-controlled).
fn draw_suggest_panel(vm: &ViewModel, frame: &mut Frame) {
    let rect = suggest_panel_rect(vm.size, vm.suggestions.len());
    if rect.h == 0 {
        return;
    }
    crate::chrome::fill(frame, rect, surface_fill(vm));
    let inner = frame.border(rect, "suggest", ACCENT);
    let t = vm.suggest_fade.clamp(0.0, 1.0);
    let text_fg = crate::theme::lerp_color(crate::theme::MUTED, vm.theme.body_fg(), t);
    let visible = usize::from(inner.h);
    for (i, sug) in vm.suggestions.iter().take(visible).enumerate() {
        let Ok(dy) = u16::try_from(i) else { break };
        let selected = i == vm.suggest_highlight;
        let marker = if selected { '>' } else { ' ' };
        let line = format!(
            "{marker} {:<30} {}",
            sanitize_for_terminal(&format!("{}  {}", sug.insert, sug.usage)),
            sanitize_for_terminal(sug.description)
        );
        let style = if selected { ACCENT } else { Style::fg(text_fg) };
        clip_line(frame, inner, dy, &line, style);
    }
}

/// The style every surface blanks its backdrop with: the theme's background
/// slot, so a profile's colour reaches modal interiors and the sweep alike.
fn surface_fill(vm: &ViewModel) -> Style {
    Style::DEFAULT.with_bg(vm.theme.surface_bg())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;
    use crate::state::AppState;
    use crate::view::fixtures::{
        explorer_state, profiled_state, project, render_observation, sized, state_with_detail,
    };
    use crate::{Panel, ProjectStatus};

    // ---- the cursor cue on the floor ----------------------------------------
    //
    // Every modal list surface, rendered through the glyph-only ASCII backend,
    // shows exactly one row wearing the marker in the modal's first inner
    // column — the cursor's row — and the marker moves with the cursor. This
    // is the property the colour-only pill lacked (the text goldens could not
    // see it, and neither could a Windows operator, whose console the
    // environment probe read as colourless).

    /// The rows inside the modal whose first inner cell carries the marker.
    fn marker_rows(s: &mut AppState) -> (Rect, Vec<u16>) {
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&vm, &mut b, &mut f);
        let modal = modal_rect(vm.size);
        let rows = (modal.y..modal.y + modal.h)
            .filter(|&y| {
                f.get(modal.x + 1, y)
                    .is_some_and(|c| c.ch == crate::chrome::CURSOR_MARKER)
            })
            .collect();
        (modal, rows)
    }

    fn assert_one_marker(name: &str, s: &mut AppState, expected_y: impl Fn(Rect) -> u16) {
        let (modal, rows) = marker_rows(s);
        assert_eq!(
            rows,
            vec![expected_y(modal)],
            "{name}: exactly one marked row, and it is the cursor's"
        );
    }

    fn entry(id: &str, name: &str) -> crate::profile::ProfileEntry {
        crate::profile::ProfileEntry {
            profile_id: id.to_owned(),
            name: name.to_owned(),
            created_at: 0,
        }
    }

    /// A state in the profile menu with two live profiles, `default` active.
    fn two_profile_menu() -> AppState {
        let mut profile = profiled_state();
        profile.app.set_profiles(
            vec![entry("pf0", "default"), entry("pf1", "work")],
            "pf0".into(),
        );
        crate::input::enter_profile_menu(&mut profile.app);
        profile
    }

    /// A state in Project Settings over the detail fixture's view.
    fn settings_state() -> AppState {
        let mut settings = state_with_detail(ProjectStatus::Idle, "model");
        let view = settings.snapshot().detail.as_deref().map_or_else(
            || crate::ProjectSettingsView::thin("/r/p".into(), "p".into()),
            |detail| detail.project_settings.as_ref().clone(),
        );
        settings.app.project_settings.open(view);
        settings.app.mode = crate::input::Mode::ProjectSettings;
        settings
    }

    /// The destination explorer: over directories and the file alike.
    #[test]
    fn the_explorer_marks_exactly_one_cursor_row_on_the_floor() {
        let mut explorer = explorer_state();
        for cursor in 0..3 {
            explorer.app.explorer.cursor = cursor;
            assert_one_marker(&format!("explorer cursor {cursor}"), &mut explorer, |m| {
                explorer_row_rect(m, cursor, 0).y
            });
        }
    }

    /// The profile menu, every list stage: the hub, the settings page, the
    /// members editor and its add stage.
    #[test]
    fn every_profile_menu_stage_marks_exactly_one_cursor_row_on_the_floor() {
        use crate::profile_menu::MenuStage;
        let mut profile = two_profile_menu();
        for cursor in 0..2 {
            profile.app.profile_menu.cursor = cursor;
            assert_one_marker(&format!("profile hub cursor {cursor}"), &mut profile, |m| {
                profile_menu_row_rect(m, cursor).y
            });
        }
        profile.app.profile_menu.stage = MenuStage::Settings {
            profile: "pf1".to_owned(),
        };
        for cursor in 0..2 {
            profile.app.profile_menu.settings_cursor = cursor;
            assert_one_marker(
                &format!("profile settings cursor {cursor}"),
                &mut profile,
                |m| profile_menu_row_rect(m, cursor).y,
            );
        }
        profile.app.set_membership_lists(
            vec![project("alpha"), project("beta")],
            vec![project("alpha"), project("beta"), project("gamma")],
        );
        profile.app.profile_menu.stage = MenuStage::Members {
            profile: "pf1".to_owned(),
        };
        for cursor in 0..2 {
            profile.app.profile_menu.member_cursor = cursor;
            assert_one_marker(&format!("members cursor {cursor}"), &mut profile, |m| {
                profile_menu_row_rect(m, cursor).y
            });
        }
        profile.app.profile_menu.stage = MenuStage::MembersAdd {
            profile: "pf1".to_owned(),
        };
        profile.app.profile_menu.add_cursor = 0;
        assert_one_marker("members add cursor 0", &mut profile, |m| {
            profile_menu_row_rect(m, 0).y
        });
    }

    /// The colour picker, both stages.
    #[test]
    fn the_colour_picker_marks_exactly_one_cursor_row_on_the_floor() {
        let mut picker = profiled_state();
        crate::input::enter_profile_menu(&mut picker.app);
        picker.app.mode = crate::input::Mode::ColourPicker;
        for cursor in 0..2 {
            picker.app.colour_picker.slot_cursor = cursor;
            assert_one_marker(&format!("picker slot cursor {cursor}"), &mut picker, |m| {
                colour_picker_row_rect(m, cursor).y
            });
        }
        picker.app.colour_picker.stage = crate::colour_picker::PickerStage::Presets;
        for cursor in [0, crate::RESET_ROW] {
            picker.app.colour_picker.preset_cursor = cursor;
            assert_one_marker(
                &format!("picker preset cursor {cursor}"),
                &mut picker,
                |m| colour_picker_row_rect(m, cursor).y,
            );
        }
    }

    /// Project Settings: the action rows, which the cursor walks.
    #[test]
    fn project_settings_marks_exactly_one_cursor_row_on_the_floor() {
        let mut settings = settings_state();
        let rows = project_settings_display_rows(&settings.app.project_settings);
        let actions: Vec<(usize, usize)> = rows
            .iter()
            .enumerate()
            .filter_map(|(line, row)| match row {
                ProjectSettingsDisplayRow::Action { index, .. } => Some((line, *index)),
                _ => None,
            })
            .collect();
        assert!(
            !actions.is_empty(),
            "the fixture lists at least one action: {actions:?}"
        );
        for (line, index) in actions.iter().take(2) {
            settings.app.project_settings.cursor = *index;
            let line = u16::try_from(*line).expect("a short listing");
            assert_one_marker(
                &format!("project settings action {index}"),
                &mut settings,
                |m| project_settings_body_rect(m).y + line,
            );
        }
    }

    // ---- every factual surface state renders through the root view ---------
    //
    // One case per live modal / layout state, each rendered through `draw`
    // with a capability-bearing backend and required to leave visible cells.

    /// Render `vm` on `caps` and require that it drew something.
    fn assert_renders(name: &str, vm: &ViewModel, caps: crate::render::Capabilities) {
        let drawn = render_observation(vm, caps);
        assert!(drawn > 0, "{name}: the state drew visible cells");
    }

    fn members() -> Vec<crate::Project> {
        vec![project("alpha"), project("beta")]
    }

    fn catalogue() -> Vec<crate::Project> {
        vec![project("alpha"), project("beta"), project("gamma")]
    }

    #[test]
    fn every_profile_hub_settings_and_members_stage_renders_through_the_root_view() {
        use crate::profile_menu::{MenuStage, SettingsRow};
        use crate::render::Capabilities;
        let mut profile = two_profile_menu();
        profile.app.profile_menu.cursor = 1;
        let mut view = profile.snapshot();
        view.pressed = Some(crate::hit::Target::ProfileSettings(1));
        view.press_pulse = 0.75;
        assert_renders(
            "hub with a pressed settings chip",
            &view,
            Capabilities::UNICODE,
        );

        profile.app.profile_menu.stage = MenuStage::Settings {
            profile: "pf1".to_owned(),
        };
        profile.app.profile_menu.settings_cursor = SettingsRow::ALL.len() - 1;
        assert_renders("settings page", &profile.snapshot(), Capabilities::empty());
        profile.app.startup_profile = Some("pf1".to_owned());
        assert_renders(
            "settings page, pinned",
            &profile.snapshot(),
            Capabilities::UNICODE,
        );

        profile.app.set_membership_lists(members(), catalogue());
        profile.app.profile_menu.stage = MenuStage::Members {
            profile: "pf1".to_owned(),
        };
        profile.app.profile_menu.member_cursor = 1;
        let mut view = profile.snapshot();
        view.pressed = Some(crate::hit::Target::MemberDown);
        view.press_pulse = 0.5;
        assert_renders("members with a pressed chip", &view, Capabilities::UNICODE);
        profile.app.profile_menu.members.clear();
        assert_renders("members, empty", &profile.snapshot(), Capabilities::empty());

        profile.app.set_membership_lists(members(), catalogue());
        profile.app.profile_menu.stage = MenuStage::MembersAdd {
            profile: "pf1".to_owned(),
        };
        assert_renders("members add", &profile.snapshot(), Capabilities::UNICODE);
        profile.app.set_membership_lists(catalogue(), catalogue());
        assert_renders(
            "members add, exhausted",
            &profile.snapshot(),
            Capabilities::empty(),
        );
    }

    #[test]
    fn every_naming_and_confirm_stage_renders_through_the_root_view() {
        use crate::profile_menu::MenuStage;
        use crate::render::Capabilities;
        let mut profile = two_profile_menu();
        for stage in [
            MenuStage::NamingCreate,
            MenuStage::NamingRename {
                profile: "pf1".to_owned(),
            },
            MenuStage::NamingDuplicate {
                source: "pf1".to_owned(),
            },
        ] {
            let name = format!("{stage:?}");
            profile.app.profile_menu.stage = stage;
            profile.app.profile_menu.name_error = true;
            profile.app.command_line = "duplicate name".to_owned();
            assert_renders(&name, &profile.snapshot(), Capabilities::UNICODE);
        }
        profile.app.profile_menu.stage = MenuStage::ConfirmDelete {
            profile: "pf1".to_owned(),
            back_to_settings: true,
        };
        assert_renders("confirm delete", &profile.snapshot(), Capabilities::empty());
    }

    #[test]
    fn every_project_settings_state_renders_through_the_root_view() {
        use crate::render::Capabilities;
        let mut settings = settings_state();
        settings.app.project_settings.running = true;
        settings.app.project_settings.result = Some("landed".to_owned());
        assert_renders(
            "running with a result",
            &settings.snapshot(),
            Capabilities::UNICODE,
        );
        settings.app.project_settings.running = false;
        settings.app.project_settings.confirm_raw = true;
        let mut view = settings.snapshot();
        view.pressed = Some(crate::hit::Target::ProjectSettingsCancel);
        view.press_pulse = 1.0;
        assert_renders(
            "raw confirm with a pressed cancel",
            &view,
            Capabilities::empty(),
        );
        settings.app.project_settings.confirm_raw = false;
        settings.app.project_settings.failed_action = Some(crate::ProjectSettingsAction {
            path: "/r/p".into(),
            kind: crate::ProjectSettingsActionKind::Retry,
        });
        assert_renders("failed action", &settings.snapshot(), Capabilities::UNICODE);
    }

    #[test]
    fn the_explorer_at_the_root_renders_through_the_root_view() {
        let mut explorer = explorer_state();
        explorer.app.explorer.cwd = std::path::PathBuf::from("/");
        explorer.app.explorer.show_hidden = true;
        explorer.app.explorer.entries.clear();
        assert_renders(
            "explorer at the root",
            &explorer.snapshot(),
            crate::render::Capabilities::UNICODE,
        );
    }

    #[test]
    fn every_overlay_renders_through_the_root_view() {
        use crate::dispatch::{DispatchRequest, Message, Overlay};
        let mut overlay = sized(100, 32);
        let cases = [
            Overlay::Results {
                title: "long results".to_owned(),
                lines: (0..80).map(|index| format!("line {index}")).collect(),
                scroll: 5,
                footer: Some("failed with class 2".to_owned()),
                truncated: true,
            },
            Overlay::Results {
                title: "empty results".to_owned(),
                lines: Vec::new(),
                scroll: 0,
                footer: None,
                truncated: false,
            },
            Overlay::Confirm {
                request: DispatchRequest::Rebuild,
                repo: "/r/alpha".to_owned(),
            },
            Overlay::running("rebuild"),
            Overlay::Running {
                title: "setup of alpha".to_owned(),
                lines: vec![
                    " ok  registry       alpha registered".to_owned(),
                    " ..  git hooks".to_owned(),
                ],
            },
            Overlay::Alert(Message::warning("warning", vec!["body".to_owned()])),
            Overlay::Notice(Message::success("success", vec!["body".to_owned()])),
        ];
        for (focus, case) in cases.into_iter().enumerate() {
            let name = format!("overlay {focus}");
            overlay.app.open_overlay(case);
            overlay.app.overlay_focus = focus;
            let mut view = overlay.snapshot();
            view.pressed = Some(crate::hit::Target::OverlayClose);
            view.press_pulse = 0.5;
            assert_renders(&name, &view, crate::render::Capabilities::UNICODE);
        }
    }

    #[test]
    fn the_command_bar_and_the_panel_editor_render_through_the_root_view() {
        use crate::render::Capabilities;
        let mut command = sized(80, 24);
        command.app.mode = crate::input::Mode::Command;
        command.app.focus = Panel::Command;
        command.app.command_line = "re".to_owned();
        let mut view = command.snapshot();
        view.sweep = 0.35;
        view.suggest_fade = 0.25;
        assert_renders("command bar mid-sweep", &view, Capabilities::UNICODE);

        let mut editor = sized(120, 40);
        let repository = editor.app.layout.columns[1].units.remove(0);
        editor.app.layout.columns[2].units.insert(1, repository);
        crate::input::enter_panel_editor(&mut editor.app);
        editor.app.editor.cursor = crate::editor::Cursor { col: 2, unit: 1 };
        editor.app.editor.picked = true;
        assert_renders("panel editor", &editor.snapshot(), Capabilities::UNICODE);
    }

    #[test]
    fn the_background_slot_repaints_panels_and_modals() {
        // A finding from real use (2026-07-17): a background override repainted
        // the header but not the panel or modal backdrops. The surface now
        // paints its own background and every foreground-only write inherits
        // it (the frame's transparent-Default compose rule) — so the slot
        // applies to EVERY element's backdrop, per the runtime surface theme
        let bg = crate::render::frame::Color::Rgb(0x11, 0x22, 0x33);
        let mut s = profiled_state();
        let mut overrides = crate::theme::ThemeOverrides::NONE;
        overrides.set(crate::theme::ThemeSlot::Background, Some(bg));
        s.set_theme(overrides);
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::UNICODE);
        draw(&vm, &mut b, &mut f);
        // A panel-interior cell (mid-body) wears the override…
        let mid = f
            .get(vm.size.w / 2, vm.size.h / 2)
            .expect("mid-body cell exists");
        assert_eq!(mid.style.bg, bg, "panel backdrops wear the background slot");
        // …and so does a modal's interior once the menu opens.
        crate::input::enter_profile_menu(&mut s.app);
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        draw(&vm, &mut b, &mut f);
        let modal = modal_rect(vm.size);
        let inside = f
            .get(modal.x + modal.w / 2, modal.y + modal.h - 2)
            .expect("modal interior cell exists");
        assert_eq!(
            inside.style.bg, bg,
            "modal backdrops wear the background slot"
        );
        // Unthemed, the surface paints Rift dark for itself — never the
        // terminal's own background.
        let mut s = profiled_state();
        s.set_theme(crate::theme::ThemeOverrides::NONE);
        let vm = s.snapshot();
        let mut f = Frame::new(vm.size);
        draw(&vm, &mut b, &mut f);
        let mid = f.get(vm.size.w / 2, vm.size.h / 2).unwrap();
        assert_eq!(mid.style.bg, crate::theme::SURFACE_BG);
    }

    #[test]
    fn tiny_terminal_does_not_panic() {
        for (w, h) in [(0, 0), (1, 1), (2, 3), (5, 2)] {
            let mut s = AppState::new(vec![], Size::new(w, h));
            s.app.splash.skip();
            let mut f = Frame::new(Size::new(w, h));
            let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
            draw(&s.snapshot(), &mut b, &mut f); // must not panic
        }
    }

    proptest::proptest! {
        /// Every terminal size yields a renderable layout — no panic, at any size.
        #[test]
        fn every_size_yields_a_renderable_layout(w in 0u16..400, h in 0u16..400) {
            let mut s = sized(w, h);
            let mut f = Frame::new(Size::new(w, h));
            let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
            draw(&s.snapshot(), &mut b, &mut f); // must not panic at any size
        }
    }
}
