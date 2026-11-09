use super::*;
use crate::render::{Color, Frame, Size};
use crate::ProjectSettingId;

fn wizard(raw: bool) -> Wizard {
    let mut view = ProjectSettingsView::thin("/demo/project".into(), "project".into());
    view.settings = ProjectSettingId::ALL
        .iter()
        .map(|&id| crate::ProjectSettingRow {
            id,
            enabled: raw && id == ProjectSettingId::RawTranscripts,
            actionable: true,
            detail: None,
        })
        .collect();
    Wizard::new(view)
}

fn review(wizard: &mut Wizard) {
    for expected in [Stage::Permissions, Stage::Settings, Stage::Review] {
        wizard.cursor = wizard.choices().len();
        assert!(matches!(wizard.key(CanopyKey::Enter), Action::None));
        assert_eq!(wizard.stage, expected);
    }
    wizard.cursor = wizard.review().len();
}

#[test]
fn setup_only_applies_after_review_and_explicit_confirmation() {
    let mut wizard = wizard(false);
    review(&mut wizard);
    assert!(matches!(wizard.key(CanopyKey::Enter), Action::None));
    assert!(wizard.setup_confirmation);
    let Action::Apply(action) = wizard.key(CanopyKey::Enter) else {
        panic!("review must apply");
    };
    assert_eq!(action.path, "/demo/project");
    let ProjectSettingsActionKind::Initialize(plan) = action.kind else {
        panic!("setup action");
    };
    assert!(!plan.raw_transcripts);
    assert!(
        plan.permissions.is_empty(),
        "keep-current requires no writes"
    );
}

#[test]
fn cancellation_and_back_never_apply_a_plan() {
    let mut wizard = wizard(false);
    assert!(matches!(wizard.key(CanopyKey::Esc), Action::Cancel));
    wizard.key(CanopyKey::Enter);
    assert_eq!(wizard.stage, Stage::Permissions);
    assert!(matches!(wizard.key(CanopyKey::Esc), Action::None));
    assert_eq!(wizard.stage, Stage::Harnesses);
    assert!(matches!(wizard.key(CanopyKey::Quit), Action::Cancel));
}

#[test]
fn raw_retention_warning_precedes_a_single_final_confirmation() {
    let mut wizard = wizard(true);
    review(&mut wizard);
    assert!(matches!(wizard.key(CanopyKey::Enter), Action::None));
    assert!(wizard.setup_confirmation);
    let mut frame = Frame::new(Size::new(80, 24));
    wizard.draw(&mut frame, f32::NAN, true);
    let text: String = frame.cells().iter().map(|cell| cell.ch).collect();
    assert!(text.contains("Raw transcripts can contain secrets"));
    assert!(text.contains("acknowledges keeping them locally"));
    assert!(matches!(wizard.key(CanopyKey::Esc), Action::None));
    assert!(!wizard.setup_confirmation);
    assert!(matches!(wizard.key(CanopyKey::Enter), Action::None));
    let Action::Apply(action) = wizard.key(CanopyKey::Enter) else {
        panic!("one Enter on Confirm must apply even with raw transcripts enabled");
    };
    let ProjectSettingsActionKind::Initialize(plan) = action.kind else {
        panic!("setup plan")
    };
    assert!(plan.raw_transcripts);
}

#[test]
fn standalone_screen_has_no_dashboard_or_settings_box_and_tiles_animate() {
    let wizard = wizard(false);
    let mut first = Frame::new(Size::new(100, 32));
    let mut next = Frame::new(first.size());
    wizard.draw(&mut first, 0.0, true);
    wizard.draw(&mut next, 0.6, true);
    assert_ne!(
        first.get(2, 1).unwrap().style,
        next.get(2, 1).unwrap().style
    );
    let text: String = (0..first.size().h)
        .flat_map(|y| (0..first.size().w).map(move |x| (x, y)))
        .map(|(x, y)| first.get(x, y).unwrap().ch)
        .collect();
    assert!(text.contains("Welcome to Rift"));
    for absent in ["Project Settings", "[Home]", "[x]", "[ ]"] {
        assert!(!text.contains(absent));
    }
    wizard.draw(&mut next, f32::NAN, true);
    assert_eq!(next.get(2, 1).unwrap().style.fg, Color::Rgb(255, 255, 255));
}

#[test]
fn deselecting_a_harness_excludes_its_permission_writes() {
    let mut wizard = wizard(false);
    wizard
        .view
        .integrations
        .push(crate::NativeAgentIntegrationRow {
            agent: "codex".into(),
            display_name: "Codex".into(),
            detection: crate::NativeAgentDetection::Detected,
            executable_name: None,
            installations: Vec::new(),
            wiring: crate::NativeAgentWiring::Managed,
            hook_wiring: None,
            alpha_supported: true,
            selected: true,
            remedies: Vec::new(),
        });
    wizard.view.permissions.push(crate::AgentPermissionRow {
        agent: "codex".into(),
        selected: 1,
        revision: "reviewed".into(),
        options: vec![
            crate::PermissionOptionRow {
                token: "keep".into(),
                label: "Keep current".into(),
                detail: "Preserve permissions".into(),
            },
            crate::PermissionOptionRow {
                token: "restricted".into(),
                label: "Restricted".into(),
                detail: "Read only".into(),
            },
        ],
    });
    wizard.key(CanopyKey::Char(' '));
    assert!(!wizard.view.integrations.first().unwrap().selected);
    review(&mut wizard);
    assert!(matches!(wizard.key(CanopyKey::Enter), Action::None));
    assert!(wizard.setup_confirmation);
    let Action::Apply(action) = wizard.key(CanopyKey::Enter) else {
        panic!("setup action");
    };
    let ProjectSettingsActionKind::Initialize(plan) = action.kind else {
        panic!("setup plan");
    };
    assert!(plan.integrations.is_empty());
    assert!(plan.permissions.is_empty());
}

#[test]
fn choices_have_white_empty_circles_and_green_selected_circles() {
    let mut wizard = wizard(false);
    wizard.stage = Stage::Settings;
    let size = Size::new(80, 24);
    let areas = wizard.row_areas(size, wizard.choices().len());
    let (_, area) = areas.first().unwrap();
    let mut frame = Frame::new(size);
    wizard.draw(&mut frame, f32::NAN, true);
    let circle = frame.get(4, area.y).unwrap();
    assert_eq!(circle.ch, '○');
    assert_eq!(circle.style.fg, Color::Rgb(255, 255, 255));
    assert!(frame
        .cells()
        .iter()
        .all(|cell| !matches!(cell.ch, '╭' | '╮' | '╰' | '╯')));
    assert!(frame
        .cells()
        .iter()
        .all(|cell| cell.style.fg != Color::Rgb(240, 196, 92)));
    let mouse = crate::MouseInput {
        kind: crate::MouseKind::Down(crate::MouseButton::Left),
        col: 4,
        row: area.y,
    };
    assert!(matches!(wizard.mouse(mouse, size), Action::None));
    wizard.draw(&mut frame, f32::NAN, true);
    assert_eq!(frame.get(4, area.y).unwrap().ch, '●');
    assert_eq!(
        frame.get(4, area.y).unwrap().style.fg,
        crate::theme::HEALTH_OK
    );
    wizard.key(CanopyKey::Char(' '));
    assert!(!wizard.view.settings.first().unwrap().enabled);
}

#[test]
fn scrolled_buttons_stay_above_descriptions_and_mouse_uses_visible_rows() {
    let mut wizard = wizard(false);
    wizard.stage = Stage::Settings;
    for size in [Size::new(40, 16), Size::new(80, 24), Size::new(113, 34)] {
        let count = wizard.choices().len();
        wizard.cursor = count;
        let areas = wizard.row_areas(size, count);
        assert!(!areas.is_empty());
        assert_eq!(areas.last().unwrap().0, count);
        for (_, area) in &areas {
            assert!(area.y + area.h <= size.h - 5);
        }
        let (_, area) = areas.last().unwrap();
        let mouse = crate::MouseInput {
            kind: crate::MouseKind::Down(crate::MouseButton::Left),
            col: area.x + 3,
            row: area.y,
        };
        assert!(matches!(wizard.mouse(mouse, size), Action::None));
        assert_eq!(wizard.stage, Stage::Review);
        wizard.stage = Stage::Settings;
    }
}

#[test]
fn review_settings_use_leading_green_and_red_circles_without_trailing_on_off() {
    let mut wizard = wizard(false);
    wizard.view.settings.get_mut(0).unwrap().enabled = true;
    wizard.stage = Stage::Review;
    let mut frame = Frame::new(Size::new(100, 32));
    wizard.draw(&mut frame, f32::NAN, true);
    let areas = wizard.row_areas(frame.size(), wizard.review().len());
    for (offset, setting) in wizard.view.settings.iter().enumerate() {
        let (_, area) = areas.get(offset + 1).unwrap();
        let circle = frame.get(4, area.y).unwrap();
        assert_eq!(circle.ch, '●');
        assert_eq!(
            circle.style.fg,
            if setting.enabled {
                crate::theme::HEALTH_OK
            } else {
                crate::theme::HEALTH_DOWN
            }
        );
        let text: String = (6..frame.size().w)
            .map(|x| frame.get(x, area.y).unwrap().ch)
            .collect();
        assert_eq!(text.trim_end(), setting.id.label());
    }
    let mut plain = Frame::new(frame.size());
    wizard.draw(&mut plain, f32::NAN, false);
    assert_eq!(plain.get(4, areas.get(1).unwrap().1.y).unwrap().ch, '*');
    assert_eq!(plain.get(4, areas.get(2).unwrap().1.y).unwrap().ch, 'o');
}

fn permission_wizard() -> Wizard {
    let mut wizard = wizard(false);
    wizard
        .view
        .integrations
        .push(crate::NativeAgentIntegrationRow {
            agent: "codex".into(),
            display_name: "Codex".into(),
            detection: crate::NativeAgentDetection::Detected,
            executable_name: None,
            installations: Vec::new(),
            wiring: crate::NativeAgentWiring::Managed,
            hook_wiring: None,
            alpha_supported: true,
            selected: true,
            remedies: Vec::new(),
        });
    wizard.view.permissions.push(crate::AgentPermissionRow {
        agent: "codex".into(),
        selected: 0,
        revision: "reviewed".into(),
        options: [
            ("keep", "Keep current"),
            ("restricted", "Restricted / Plan"),
            ("workspace", "Workspace edits"),
        ]
        .into_iter()
        .map(|(token, label)| crate::PermissionOptionRow {
            token: token.into(),
            label: label.into(),
            detail: "Permission detail".into(),
        })
        .collect(),
    });
    wizard
}

#[test]
fn permission_labels_keep_their_colors_in_selection_and_review() {
    let mut wizard = permission_wizard();
    for stage in [Stage::Permissions, Stage::Review] {
        wizard.stage = stage;
        for (selected, color) in [
            Color::Rgb(240, 196, 92),
            Color::Rgb(92, 168, 255),
            Color::Rgb(255, 165, 0),
        ]
        .into_iter()
        .enumerate()
        {
            wizard.view.permissions.first_mut().unwrap().selected = selected;
            let permission = wizard.view.permissions.first().unwrap();
            let label = &permission.options.get(selected).unwrap().label;
            let mut frame = Frame::new(Size::new(100, 32));
            wizard.draw(&mut frame, f32::NAN, true);
            let y = super::chrome::header_top(frame.size()) + 4 + u16::from(stage == Stage::Review);
            assert_eq!(frame.get(4, y).unwrap().ch, 'c');
            assert_eq!(frame.get(4, y).unwrap().style.fg, Color::Default);
            for (offset, ch) in label.chars().enumerate() {
                let cell = frame.get(11 + u16::try_from(offset).unwrap(), y).unwrap();
                assert_eq!(cell.ch, ch);
                assert_eq!(cell.style.fg, color);
            }
        }
    }
}

#[test]
fn setup_expands_confirm_and_back_with_keyboard_and_mouse_parity() {
    let mut wizard = wizard(false);
    review(&mut wizard);
    let before = wizard.input_stage();
    assert!(matches!(wizard.key(CanopyKey::Enter), Action::None));
    assert_ne!(wizard.input_stage(), before);
    for size in [Size::new(40, 16), Size::new(80, 24)] {
        let mut frame = Frame::new(size);
        wizard.draw(&mut frame, f32::NAN, true);
        let text: String = frame.cells().iter().map(|cell| cell.ch).collect();
        assert!(text.contains("Confirm setup"));
        assert!(text.contains("Back"));
    }
    wizard.key(CanopyKey::Down);
    assert!(matches!(wizard.key(CanopyKey::Enter), Action::None));
    assert!(!wizard.setup_confirmation);
    assert_eq!(wizard.cursor, wizard.review().len());
    wizard.key(CanopyKey::Enter);
    let size = Size::new(80, 24);
    let areas = wizard.row_areas(size, wizard.review().len());
    let (_, back) = areas.last().unwrap();
    let click = |area: &crate::layout::Rect| crate::MouseInput {
        kind: crate::MouseKind::Down(crate::MouseButton::Left),
        col: area.x + 3,
        row: area.y,
    };
    assert!(matches!(wizard.mouse(click(back), size), Action::None));
    assert!(!wizard.setup_confirmation);
    wizard.key(CanopyKey::Enter);
    let areas = wizard.row_areas(size, wizard.review().len());
    let (_, confirm) = areas.iter().find(|(i, _)| *i == wizard.cursor).unwrap();
    assert!(matches!(
        wizard.mouse(click(confirm), size),
        Action::Apply(_)
    ));
}

#[test]
fn progress_reserves_verification_and_retry_requires_new_confirmation() {
    let mut wizard = wizard(false);
    review(&mut wizard);
    wizard.key(CanopyKey::Enter);
    wizard.start_setup();
    let mut frame = Frame::new(Size::new(80, 24));
    wizard.draw(&mut frame, f32::NAN, true);
    let text: String = frame.cells().iter().map(|cell| cell.ch).collect();
    assert!(text.contains("Setting up Rift"));
    assert!(text.contains("0%"));
    assert!(!text.contains("Confirm setup"));
    for step in [
        crate::SetupStep::Registry,
        crate::SetupStep::Configuration,
        crate::SetupStep::Projection,
        crate::SetupStep::GitHooks,
        crate::SetupStep::Policy,
        crate::SetupStep::Recall,
    ] {
        wizard.record_setup(crate::SetupProgress::done(step, "done"));
    }
    frame = Frame::new(frame.size());
    wizard.draw(&mut frame, f32::NAN, true);
    let text: String = frame.cells().iter().map(|cell| cell.ch).collect();
    assert!(text.contains("85%"));
    assert!(!text.contains("100%"));
    wizard.status = "Setup stopped: fixture refusal".into();
    wizard.finish_setup(false);
    assert!(wizard.progress.is_none());
    assert!(!wizard.setup_confirmation);
    assert_eq!(wizard.status, "Setup stopped: fixture refusal");
    assert!(matches!(wizard.key(CanopyKey::Enter), Action::None));
    assert!(matches!(wizard.key(CanopyKey::Enter), Action::Apply(_)));
    wizard.start_setup();
    wizard.finish_setup(true);
    frame = Frame::new(frame.size());
    wizard.draw(&mut frame, f32::NAN, true);
    let text: String = frame.cells().iter().map(|cell| cell.ch).collect();
    assert!(text.contains("100%"));
}

#[test]
fn confirmation_is_separated_and_inactive_review_is_gray() {
    let mut wizard = permission_wizard();
    review(&mut wizard);
    wizard.key(CanopyKey::Enter);
    for size in [Size::new(40, 16), Size::new(80, 24), Size::new(100, 32)] {
        let areas = wizard.row_areas(size, wizard.review().len());
        let setup_index = wizard.review().len();
        let (_, setup) = areas
            .iter()
            .find(|(index, _)| *index == setup_index)
            .unwrap();
        let (_, confirm) = areas
            .iter()
            .find(|(index, _)| *index == setup_index + 1)
            .unwrap();
        let (_, back) = areas.last().unwrap();
        assert_eq!(confirm.y, setup.y + 2);
        assert_eq!(back.y, confirm.y + 1);
        assert!(back.y < size.h - 5);
        let mut frame = Frame::new(size);
        wizard.draw(&mut frame, f32::NAN, true);
        assert!((0..size.w).all(|x| frame.get(x, setup.y + 1).unwrap().ch == ' '));
        for (_, area) in areas.iter().filter(|(index, _)| *index <= setup_index) {
            for x in area.x..area.x + area.w {
                let cell = frame.get(x, area.y).unwrap();
                if cell.ch != ' ' {
                    assert_eq!(cell.style.fg, Color::Rgb(144, 144, 144));
                }
            }
        }
        assert_eq!(frame.get(4, confirm.y).unwrap().style.fg, Color::Default);
        assert!(frame
            .get(4, confirm.y)
            .unwrap()
            .style
            .mods
            .contains(crate::render::Mods::BOLD));
        let click_gap = crate::MouseInput {
            kind: crate::MouseKind::Down(crate::MouseButton::Left),
            col: 4,
            row: setup.y + 1,
        };
        assert!(matches!(wizard.mouse(click_gap, size), Action::None));
        assert!(wizard.setup_confirmation);
    }
}

#[test]
fn installer_skips_project_permissions_and_requires_review_before_saving() {
    let mut view = wizard(false).view;
    // As shipped: every feature on, which is what the installer's view holds.
    for row in &mut view.settings {
        row.enabled = true;
    }
    let mut wizard = Wizard::installer(view);
    wizard.cursor = wizard.choices().len();
    wizard.key(CanopyKey::Enter);
    assert_eq!(
        wizard.stage,
        Stage::Review,
        "the installer has no feature page between harnesses and review"
    );
    wizard.key(CanopyKey::Esc);
    assert_eq!(wizard.stage, Stage::Harnesses);
    wizard.cursor = wizard.choices().len();
    wizard.key(CanopyKey::Enter);
    assert_eq!(wizard.stage, Stage::Review);
    wizard.cursor = wizard.review().len();
    assert!(matches!(wizard.key(CanopyKey::Enter), Action::None));
    let mut frame = Frame::new(Size::new(80, 24));
    wizard.draw(&mut frame, 0.6, true);
    let text: String = frame.cells().iter().map(|cell| cell.ch).collect();
    assert!(text.contains("Confirm defaults"));
    assert!(text.contains("No project is changed"));
    assert!(
        text.contains("Features: all on"),
        "review names the default"
    );
    let Action::Apply(action) = wizard.key(CanopyKey::Enter) else {
        panic!("save action")
    };
    assert!(matches!(
        action.kind,
        ProjectSettingsActionKind::Initialize(_)
    ));
}

#[test]
fn milestone_nodes_count_remaining_steps_and_intro_is_gray_in_both_wizards() {
    for installer in [false, true] {
        let mut wizard = wizard(false);
        if installer {
            wizard.purpose = Purpose::Installation;
        }
        let mut frame = Frame::new(Size::new(100, 32));
        wizard.draw(&mut frame, 0.0, true);
        let text: String = frame.cells().iter().map(|cell| cell.ch).collect();
        assert!(text.contains(if installer {
            "Step 2/4 · 2 left"
        } else {
            "Step 1/5 · 4 left"
        }));
        assert!(text.contains("Rift v1.0.0-alpha"));
        assert!(text.contains("Capture agent sessions."));
        assert!(text.contains(if installer {
            "rift update --from"
        } else {
            "rift doctor"
        }));
        for y in 5..=8 {
            for x in 2..frame.size().w {
                let cell = frame.get(x, y).unwrap();
                if cell.ch != ' ' {
                    assert_eq!(cell.style.fg, Color::Rgb(144, 144, 144));
                }
            }
        }
        let top = super::chrome::header_top(frame.size());
        assert_eq!(
            frame.get(if installer { 5 } else { 2 }, top).unwrap().ch,
            '◉'
        );
        wizard.start_setup();
        wizard.finish_setup(true);
        frame = Frame::new(frame.size());
        wizard.draw(&mut frame, f32::NAN, true);
        let nodes: &[u16] = if installer {
            &[2, 5, 8, 11]
        } else {
            &[2, 5, 8, 11, 14]
        };
        for x in nodes {
            assert_eq!(frame.get(*x, top).unwrap().ch, '●');
        }
    }
}
