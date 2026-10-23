//! Project Settings: the modal's display rows in their section order
//! (Recall, Capture, Security / Policy, Integrations, the initialize row),
//! the body and chip geometry the hit-test shares, and the draw over them.
//!
//! Only an action row carries a hit target; policy facts and headings are
//! deliberately not cursor stops.

use crate::col2::sanitize_for_terminal;
use crate::layout::Rect;
use crate::render::frame::Frame;
use crate::state::ViewModel;
use crate::ProjectInitialization as Init;

use super::modal::{draw_mode_close, mode_chip_style};
use super::text::clip_at;
use super::ACCENT;

/// One logical Project Settings display row. Only [`Self::Action`] carries a
/// hit target; policy facts and headings are intentionally not cursor stops.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectSettingsDisplayRow {
    /// Section heading.
    Heading(String),
    /// Safe read-only fact.
    Info(String),
    /// Actionable row with its index in the modal's actionable-only list.
    Action {
        /// Actionable-only index.
        index: usize,
        /// Left-side label/detail.
        label: String,
        /// Action chip; empty for a leading toggle.
        chip: String,
        /// Boolean selection, rendered with the shared onboarding control.
        checked: Option<bool>,
    },
    /// One line of an integration finding's remedy, under its adapter row:
    /// the headline, or a wrapped line of the sentence. Not a cursor stop and
    /// not a click target; the adapter row above it is the act.
    Remedy {
        /// The finding's severity, which picks the colour.
        severity: crate::NativeAgentRemedySeverity,
        /// The line, already wrapped to a fixed column.
        text: String,
    },
    /// Visual spacer.
    Spacer,
}

/// The column a remedy sentence wraps at. Fixed rather than the body's width,
/// so the display rows stay a function of the state alone, as the hit-test
/// and the scroll clamp need them to be; it fits the body of the modal an
/// 80-column terminal draws, and a narrower modal clips.
const REMEDY_WRAP_COLS: usize = 56;

/// Project Settings rows in their required section order: Recall, Capture,
/// Security / Policy, Integrations, and the initialize row while setup is on
/// offer.
#[must_use]
pub fn project_settings_display_rows(
    state: &crate::ProjectSettingsModalState,
) -> Vec<ProjectSettingsDisplayRow> {
    use crate::ProjectSettingId;
    let Some(view) = state.view.as_deref() else {
        return Vec::new();
    };
    let items = state.items();
    let mut rows = Vec::new();
    if view.is_loading() {
        return vec![ProjectSettingsDisplayRow::Info(
            "Loading project settings…".into(),
        )];
    }
    if view.initialization.offers_setup() {
        return vec![
            ProjectSettingsDisplayRow::Info("Project setup runs before Canopy opens.".into()),
            ProjectSettingsDisplayRow::Info(
                "Exit Canopy and run `rift` in this project to start the CLI wizard.".into(),
            ),
            ProjectSettingsDisplayRow::Info(crate::col2::sanitize_for_terminal(&view.path)),
        ];
    }
    push_settings_section(
        &mut rows,
        &items,
        view,
        "Recall",
        &[
            ProjectSettingId::TotalRecall,
            ProjectSettingId::TurnInjection,
            ProjectSettingId::Documents,
            ProjectSettingId::Search,
        ],
    );
    rows.push(ProjectSettingsDisplayRow::Spacer);
    push_settings_section(
        &mut rows,
        &items,
        view,
        "Capture",
        &[
            ProjectSettingId::ToolCallCapture,
            ProjectSettingId::TokenUsageCapture,
            ProjectSettingId::RawTranscripts,
        ],
    );
    rows.push(ProjectSettingsDisplayRow::Spacer);
    push_policy_section(&mut rows, &view.policy);
    rows.push(ProjectSettingsDisplayRow::Spacer);
    rows.push(ProjectSettingsDisplayRow::Heading("Integrations".into()));
    for integration in &view.integrations {
        let item = crate::ProjectSettingsItem::Integration(integration.agent.clone());
        let Some(index) = item_index(&items, &item) else {
            continue;
        };
        rows.push(integration_row(index, integration, view.initialization));
        push_remedy_lines(&mut rows, &integration.remedies);
    }
    push_permissions_section(&mut rows, &items, view);
    rows
}

fn push_permissions_section(
    rows: &mut Vec<ProjectSettingsDisplayRow>,
    items: &[crate::ProjectSettingsItem],
    view: &crate::ProjectSettingsView,
) {
    rows.push(ProjectSettingsDisplayRow::Spacer);
    rows.push(ProjectSettingsDisplayRow::Heading(
        "Harness Permissions · Project Only".into(),
    ));
    for permission in &view.permissions {
        let item = crate::ProjectSettingsItem::Permission(permission.agent.clone());
        if let (Some(index), Some(option)) = (
            item_index(items, &item),
            permission.options.get(permission.selected),
        ) {
            rows.push(ProjectSettingsDisplayRow::Action {
                index,
                label: view
                    .integrations
                    .iter()
                    .find(|row| row.agent == permission.agent)
                    .map_or_else(|| permission.agent.clone(), |row| row.display_name.clone()),
                chip: format!("[{}]", option.label),
                checked: None,
            });
            rows.extend(
                wrap_words(&option.detail, REMEDY_WRAP_COLS)
                    .into_iter()
                    .map(ProjectSettingsDisplayRow::Info),
            );
        }
    }
    rows.push(ProjectSettingsDisplayRow::Info(
        "Unsupported controls stay in the harness. Existing permissions are kept by default."
            .into(),
    ));
    if let Some(index) = item_index(items, &crate::ProjectSettingsItem::ApplyPermissions) {
        rows.push(ProjectSettingsDisplayRow::Action {
            index,
            label: "Confirm reviewed native permission changes".into(),
            chip: "[Apply permissions]".into(),
            checked: None,
        });
    }
}

/// Scrollable Project Settings body; the last three inner rows are reserved for
/// focused-row help, inline result text, and stage-honest hints.
#[must_use]
pub fn project_settings_body_rect(modal: Rect) -> Rect {
    Rect {
        x: modal.x + 1,
        y: modal.y + 1,
        w: modal.w.saturating_sub(2),
        h: modal.h.saturating_sub(5),
    }
}

/// Settings content starts directly inside the settings surface.
#[must_use]
pub fn project_settings_content_rect(
    modal: Rect,
    _state: &crate::ProjectSettingsModalState,
) -> Rect {
    project_settings_body_rect(modal)
}

/// Visible toggle rows and action-chip rects. The full toggle row is clickable;
/// non-toggle actions retain their exact chip targets.
#[must_use]
pub fn project_settings_action_rects(
    modal: Rect,
    state: &crate::ProjectSettingsModalState,
) -> Vec<(usize, Rect)> {
    let body = project_settings_content_rect(modal, state);
    let rows = project_settings_display_rows(state);
    let scroll = project_settings_scroll(state, &rows, usize::from(body.h));
    rows.iter()
        .enumerate()
        .filter_map(|(line_index, row)| {
            let ProjectSettingsDisplayRow::Action {
                index,
                chip,
                checked,
                ..
            } = row
            else {
                return None;
            };
            let dy = line_index.checked_sub(scroll)?;
            let dy = u16::try_from(dy).ok()?;
            if dy >= body.h {
                return None;
            }
            let w = if checked.is_some() {
                body.w
            } else {
                u16::try_from(chip.chars().count()).unwrap_or(0).min(body.w)
            };
            Some((
                *index,
                Rect {
                    x: body.x + body.w.saturating_sub(w),
                    y: body.y + dy,
                    w,
                    h: 1,
                },
            ))
        })
        .collect()
}

/// Raw-transcript warning's explicit cancel chip, paired with `Esc`.
#[must_use]
pub fn project_settings_cancel_rect(
    modal: Rect,
    state: &crate::ProjectSettingsModalState,
) -> Option<Rect> {
    if !state.confirm_raw || modal.w < 10 || modal.h < 4 {
        return None;
    }
    let inner = Rect {
        x: modal.x + 1,
        y: modal.y + 1,
        w: modal.w.saturating_sub(2),
        h: modal.h.saturating_sub(2),
    };
    let w = 8u16.min(inner.w);
    Some(Rect {
        x: inner.x + inner.w.saturating_sub(w),
        y: inner.y + inner.h.saturating_sub(1),
        w,
        h: 1,
    })
}

/// Draw Project Settings: the shared modal frame + scrim, the stage in the
/// title, the scrolled section rows, then the three footer rows — the focused
/// row's description, the inline result, and the stage-honest hints — and
/// the raw-transcript warning's explicit cancel chip.
pub(super) fn draw_project_settings(vm: &ViewModel, frame: &mut Frame, unicode: bool) {
    let rect = crate::dashboard::geometry(vm.size).0;
    if rect.w < 20 || rect.h < 7 {
        return;
    }
    crate::chrome::fill(frame, rect, vm.theme.body().with_bg(vm.theme.surface_bg()));
    let inner = frame.border(rect, &settings_title(vm), ACCENT);
    draw_mode_close(vm, frame, rect, &crate::hit::Target::ProjectSettingsClose);
    let content = project_settings_content_rect(rect, &vm.project_settings);
    draw_settings_rows(vm, frame, content, unicode);
    draw_settings_footer(vm, frame, inner);
    if let Some(cancel) = project_settings_cancel_rect(rect, &vm.project_settings) {
        let style = mode_chip_style(vm, &crate::hit::Target::ProjectSettingsCancel);
        frame.put_str(cancel.x, cancel.y, "[cancel]", style);
    }
}

/// A setting's position in the modal's actionable-only list, or `None` when
/// the setting is locked on this machine and shows as a fact instead.
fn item_index(
    items: &[crate::ProjectSettingsItem],
    wanted: &crate::ProjectSettingsItem,
) -> Option<usize> {
    items.iter().position(|item| item == wanted)
}

/// One settings section: its heading, then the listed settings in the given
/// order, each an action row or a locked fact.
fn push_settings_section(
    rows: &mut Vec<ProjectSettingsDisplayRow>,
    items: &[crate::ProjectSettingsItem],
    view: &crate::ProjectSettingsView,
    heading: &str,
    ids: &[crate::ProjectSettingId],
) {
    rows.push(ProjectSettingsDisplayRow::Heading(heading.into()));
    for id in ids {
        if let Some(row) = view.settings.iter().find(|row| row.id == *id) {
            push_setting_row(rows, items, row, view.initialization);
        }
    }
}

/// One setting: an action row when the setting is actionable here, else a
/// fact. A configured-elsewhere project shows the committed value as a fact:
/// setup on this machine never rewrites the repository's shared
/// configuration. Anything else locked says so.
fn push_setting_row(
    rows: &mut Vec<ProjectSettingsDisplayRow>,
    items: &[crate::ProjectSettingsItem],
    row: &crate::ProjectSettingRow,
    initialization: Init,
) {
    let label = row.detail.as_ref().map_or_else(
        || row.id.label().to_owned(),
        |detail| format!("{} · {detail}", row.id.label()),
    );
    let setting = crate::ProjectSettingsItem::Setting(row.id);
    let display_row = match item_index(items, &setting) {
        Some(index) => ProjectSettingsDisplayRow::Action {
            index,
            label,
            chip: String::new(),
            checked: Some(row.enabled),
        },
        None if initialization == Init::NotProvisioned => ProjectSettingsDisplayRow::Info(format!(
            "{label} · {} · committed by the repository",
            checkbox(row.enabled)
        )),
        None => ProjectSettingsDisplayRow::Info(format!("{label} · locked")),
    };
    rows.push(display_row);
}

/// The Security / Policy section: safe read-only facts, never cursor stops.
fn push_policy_section(
    rows: &mut Vec<ProjectSettingsDisplayRow>,
    policy: &crate::ProjectPolicySummary,
) {
    use crate::ProjectPolicyState;
    rows.push(ProjectSettingsDisplayRow::Heading(
        "Security / Policy".into(),
    ));
    let source = match policy.state {
        ProjectPolicyState::Declared => "policy: declared · valid",
        ProjectPolicyState::Default => "policy: secure defaults · valid",
        ProjectPolicyState::Invalid => "policy: invalid · fail-closed",
        ProjectPolicyState::Refused => "policy: refused · fail-closed",
    };
    rows.push(ProjectSettingsDisplayRow::Info(source.into()));
    rows.push(ProjectSettingsDisplayRow::Info(format!(
        "Sensitive Paths: {}",
        policy.sensitive_path_count
    )));
    rows.push(ProjectSettingsDisplayRow::Info(format!(
        "Redactor Packs: {} loaded · {} failed",
        policy.loaded_redactor_packs, policy.failed_redactor_packs
    )));
    if let Some(detail) = &policy.detail {
        rows.push(ProjectSettingsDisplayRow::Info(detail.clone()));
    }
}

/// An integration's action row: its name, install evidence, capture wiring
/// and (where the agent carries one) its hooks state, beside its checkbox —
/// the wired state once initialized, the wizard's selection before.
fn integration_row(
    index: usize,
    integration: &crate::NativeAgentIntegrationRow,
    initialization: Init,
) -> ProjectSettingsDisplayRow {
    let detection = integration_detection_label(integration);
    let wiring = integration_wiring_label(integration);
    let enabled = if initialization == Init::Initialized {
        integration.enabled()
    } else {
        integration.selected
    };
    let label = match integration.hook_wiring {
        Some(hooks) => format!(
            "{} · {detection} · capture {wiring} · {}",
            integration.display_name,
            hook_wiring_label(hooks)
        ),
        None => format!("{} · {detection} · {wiring}", integration.display_name),
    };
    ProjectSettingsDisplayRow::Action {
        index,
        label,
        chip: String::new(),
        checked: Some(enabled),
    }
}

/// The wiring half of an integration row label. A coming-soon row says so
/// instead of implying a wiring the manager is guaranteed to refuse; a
/// leftover artifact's real state stays visible beside the scope fact rather
/// than hidden by it.
fn integration_wiring_label(integration: &crate::NativeAgentIntegrationRow) -> String {
    use crate::NativeAgentWiring;
    let wiring = match integration.wiring {
        NativeAgentWiring::Missing => "not wired",
        NativeAgentWiring::Managed => "managed",
        NativeAgentWiring::Conflict => "conflict",
        NativeAgentWiring::Invalid => "invalid",
    };
    if integration.alpha_supported {
        wiring.to_owned()
    } else if integration.wiring == NativeAgentWiring::Missing {
        "coming soon".to_owned()
    } else {
        format!("{wiring} · coming soon")
    }
}

/// The row's install evidence, one rule for every agent: the sources found,
/// without versions (the modal's description carries those), or `not
/// installed`. A Codex-only branch used to live here; the data plane now
/// reads every agent's extensions and vendor installs, so the row reads them
/// too.
fn integration_detection_label(integration: &crate::NativeAgentIntegrationRow) -> String {
    if integration.installations.is_empty() {
        return "not installed".to_owned();
    }
    integration
        .installations
        .iter()
        .map(|installation| installation.source.label())
        .collect::<Vec<_>>()
        .join(" + ")
}

/// The hooks half of an integration row label.
fn hook_wiring_label(hooks: crate::NativeAgentHookWiring) -> &'static str {
    use crate::NativeAgentHookWiring;
    match hooks {
        NativeAgentHookWiring::Missing => "hooks absent",
        NativeAgentHookWiring::Managed => "hooks managed",
        NativeAgentHookWiring::Stale => "hooks need refresh",
        NativeAgentHookWiring::Conflict => "hooks conflict",
        NativeAgentHookWiring::Invalid => "hooks invalid",
    }
}

/// An adapter's remedies under its row: per finding, its severity and
/// headline, then its sentence wrapped and indented under it — the shape the
/// command line prints. Nothing for a healthy agent.
fn push_remedy_lines(
    rows: &mut Vec<ProjectSettingsDisplayRow>,
    remedies: &[crate::NativeAgentRemedy],
) {
    use crate::NativeAgentRemedySeverity as Severity;
    for remedy in remedies {
        let word = match remedy.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        let headline = format!("{word}: {}", remedy.headline);
        let sentence = wrap_words(&remedy.sentence, REMEDY_WRAP_COLS - 2)
            .into_iter()
            .map(|line| format!("  {line}"));
        for text in std::iter::once(headline).chain(sentence) {
            rows.push(ProjectSettingsDisplayRow::Remedy {
                severity: remedy.severity,
                text,
            });
        }
    }
}

/// `text` broken at spaces into lines of at most `width` characters; a word
/// longer than a line is cut, so no line ever exceeds it.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        while word.len() > width {
            flush_line(&mut lines, &mut current);
            let rest = word.split_off(width);
            lines.push(word.into_iter().collect());
            word = rest;
        }
        let needed = current.chars().count() + usize::from(!current.is_empty()) + word.len();
        if needed > width {
            flush_line(&mut lines, &mut current);
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.extend(word);
    }
    flush_line(&mut lines, &mut current);
    lines
}

/// Push `current` as a line when it holds anything, and empty it.
fn flush_line(lines: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        lines.push(std::mem::take(current));
    }
}

fn checkbox(enabled: bool) -> String {
    if enabled { "●" } else { "○" }.to_owned()
}

fn project_settings_scroll(
    state: &crate::ProjectSettingsModalState,
    rows: &[ProjectSettingsDisplayRow],
    visible: usize,
) -> usize {
    let selected_line = rows.iter().position(|row| {
        matches!(row, ProjectSettingsDisplayRow::Action { index, .. } if *index == state.cursor)
    });
    let max = rows.len().saturating_sub(visible);
    let mut scroll = state.scroll.min(max);
    if state.follow_cursor {
        if let Some(line) = selected_line {
            // The selected row's remedies follow it into view, as far as the
            // body holds them: they are what the row asks the reader to do.
            let end = line + trailing_remedy_lines(rows, line);
            if line < scroll {
                scroll = line;
            } else if end >= scroll.saturating_add(visible) {
                scroll = end.saturating_add(1).saturating_sub(visible).min(line);
            }
        }
    }
    scroll.min(max)
}

/// How many remedy lines directly follow the row at `line`.
fn trailing_remedy_lines(rows: &[ProjectSettingsDisplayRow], line: usize) -> usize {
    rows.iter()
        .skip(line.saturating_add(1))
        .take_while(|row| matches!(row, ProjectSettingsDisplayRow::Remedy { .. }))
        .count()
}

/// The title names the project and, unless it is initialized, its stage.
fn settings_title(vm: &ViewModel) -> String {
    let view = vm.project_settings.view.as_deref();
    let name = view.map_or("project", |view| view.project_name.as_str());
    let stage = view.map_or("", |view| match view.initialization {
        Init::Initialized => "",
        Init::NotInitialized => " · setup required",
        Init::NotProvisioned => " · not set up on this machine",
        Init::NotRepository => " · not a git repository",
        Init::SetupIncomplete => " · setup incomplete",
    });
    format!("Project Settings · {}{stage}", sanitize_for_terminal(name))
}

/// The section rows from the scroll offset down, as many as the body holds.
fn draw_settings_rows(vm: &ViewModel, frame: &mut Frame, body: Rect, unicode: bool) {
    let rows = project_settings_display_rows(&vm.project_settings);
    let scroll = project_settings_scroll(&vm.project_settings, &rows, usize::from(body.h));
    for (line_index, row) in rows.iter().enumerate().skip(scroll) {
        let Ok(dy) = u16::try_from(line_index - scroll) else {
            break;
        };
        if dy >= body.h {
            break;
        }
        draw_settings_row(vm, frame, body, body.y + dy, row, unicode);
    }
}

/// One row: a heading in the accent, a fact dimmed and indented, an action
/// row with its label left and its chip right-aligned — under the shared
/// cursor cue when it is the cursor's (the row filled, and the marker in its
/// first cell, so the floor tiers still show which row is live).
fn draw_settings_row(
    vm: &ViewModel,
    frame: &mut Frame,
    body: Rect,
    y: u16,
    row: &ProjectSettingsDisplayRow,
    unicode: bool,
) {
    match row {
        ProjectSettingsDisplayRow::Heading(text) => {
            clip_at(frame, body.x, y, body.w, text, ACCENT);
        }
        ProjectSettingsDisplayRow::Info(text) => {
            let text = sanitize_for_terminal(text);
            let dim = vm.theme.dim();
            clip_at(frame, body.x + 2, y, body.w.saturating_sub(2), &text, dim);
        }
        ProjectSettingsDisplayRow::Action {
            index,
            label,
            chip,
            checked,
        } => {
            let selected = *index == vm.project_settings.cursor;
            let style = if selected {
                vm.theme.selection()
            } else {
                vm.theme.body()
            };
            if selected {
                let row = Rect {
                    x: body.x,
                    y,
                    w: body.w,
                    h: 1,
                };
                crate::chrome::cursor_row(frame, row, style);
            }
            let chip_w = u16::try_from(chip.chars().count()).unwrap_or(0).min(body.w);
            let prefix = if let Some(enabled) = checked {
                let (glyph, toggle_style) = crate::controls::toggle(*enabled, unicode);
                frame.set(body.x + 2, y, glyph, toggle_style.with_bg(style.bg));
                4
            } else {
                2
            };
            let label_w = body.w.saturating_sub(chip_w + prefix + 1);
            let label = sanitize_for_terminal(label);
            clip_at(frame, body.x + prefix, y, label_w, &label, style);
            let chip_x = body.x + body.w.saturating_sub(chip_w);
            clip_at(frame, chip_x, y, chip_w, chip, style);
        }
        ProjectSettingsDisplayRow::Remedy { severity, text } => {
            let text = sanitize_for_terminal(text);
            let style = remedy_style(vm, *severity);
            clip_at(frame, body.x + 4, y, body.w.saturating_sub(4), &text, style);
        }
        ProjectSettingsDisplayRow::Spacer => {}
    }
}

/// The error or warning colour of the dashboard's health dot, so a remedy
/// reads with the same weight as the status it explains.
fn remedy_style(
    vm: &ViewModel,
    severity: crate::NativeAgentRemedySeverity,
) -> crate::render::frame::Style {
    let health = match severity {
        crate::NativeAgentRemedySeverity::Error => crate::theme::Health::Down,
        crate::NativeAgentRemedySeverity::Warning => crate::theme::Health::Warn,
    };
    vm.theme.health(health)
}

/// The three footer rows: the focused row's description, the inline result
/// (or `working…` while an operation runs), and the hints the stage honours.
fn draw_settings_footer(vm: &ViewModel, frame: &mut Frame, inner: Rect) {
    let state = &vm.project_settings;
    let description = state
        .focused_description()
        .map_or_else(String::new, |text| format!("About: {text}"));
    let result = if state.running {
        "working…"
    } else {
        state.result.as_deref().unwrap_or("")
    };
    let rows = [
        (3, sanitize_for_terminal(&description)),
        (2, sanitize_for_terminal(result)),
        (1, settings_hints(state).to_owned()),
    ];
    for (from_bottom, text) in rows {
        let y = inner.y + inner.h.saturating_sub(from_bottom);
        clip_at(frame, inner.x, y, inner.w, &text, vm.theme.dim());
    }
}

/// The hints line: only the keys the stage answers to.
fn settings_hints(state: &crate::ProjectSettingsModalState) -> &'static str {
    if state
        .view
        .as_ref()
        .is_some_and(|view| view.initialization.offers_setup())
    {
        " Tab/Shift-Tab views · esc/x close"
    } else if state.confirm_raw {
        " enter acknowledge · esc cancel"
    } else if state.running {
        " settings locked while operation completes"
    } else if state.failed_action.is_some() {
        " j/k move · enter/space apply · r retry · esc/x close"
    } else {
        " j/k move · enter/space apply · pgup/pgdn scroll · esc/x close"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_toggles_use_onboarding_marks_and_the_visible_row_is_clickable() {
        let size = crate::render::Size::new(80, 24);
        let mut state = crate::AppState::new(vec![], size);
        let vm = state.snapshot();
        let body = Rect {
            x: 2,
            y: 2,
            w: 60,
            h: 10,
        };
        for (enabled, glyph, color) in [
            (false, '○', crate::render::Color::Rgb(255, 255, 255)),
            (true, '●', crate::theme::HEALTH_OK),
        ] {
            let mut frame = Frame::new(size);
            let row = ProjectSettingsDisplayRow::Action {
                index: 0,
                label: "Total Recall".into(),
                chip: String::new(),
                checked: Some(enabled),
            };
            draw_settings_row(&vm, &mut frame, body, 3, &row, true);
            assert_eq!(frame.get(4, 3).unwrap().ch, glyph);
            assert_eq!(frame.get(4, 3).unwrap().style.fg, color);
        }
        let settings = integrations_view(vec![adapter("codex", Vec::new())]);
        let rects = project_settings_action_rects(Rect { h: 40, ..body }, &settings);
        assert_eq!(rects.len(), 1);
        assert_eq!(rects.first().unwrap().1.w, body.w - 2);
    }

    /// The label matrix for the alpha capture scope: a coming-soon row says
    /// so instead of "not wired" (which advertises a toggle the manager
    /// refuses), while a leftover artifact keeps its real state visible
    /// beside the scope fact and a supported row is untouched.
    #[test]
    fn a_coming_soon_integration_row_reads_coming_soon_not_not_wired() {
        let row = |agent: &str, wiring, alpha_supported| crate::NativeAgentIntegrationRow {
            agent: agent.to_owned(),
            display_name: agent.to_owned(),
            detection: crate::NativeAgentDetection::Detected,
            executable_name: Some(agent.to_owned()),
            installations: vec![crate::NativeAgentInstallation {
                source: crate::NativeAgentInstallSource::PathCommand,
                version: None,
            }],
            wiring,
            hook_wiring: None,
            alpha_supported,
            selected: false,
            remedies: Vec::new(),
        };
        let mut state = crate::ProjectSettingsModalState::default();
        state.open(crate::ProjectSettingsView {
            permissions: Vec::new(),
            path: "/r/demo".into(),
            project_name: "demo".into(),
            initialization: crate::ProjectInitialization::Initialized,
            settings: Vec::new(),
            policy: crate::ProjectPolicySummary::default(),
            integrations: vec![
                row("gemini", crate::NativeAgentWiring::Missing, false),
                row("antigravity", crate::NativeAgentWiring::Missing, true),
                row("cursor", crate::NativeAgentWiring::Conflict, false),
            ],
        });
        let labels: Vec<String> = project_settings_display_rows(&state)
            .into_iter()
            .filter_map(|row| match row {
                ProjectSettingsDisplayRow::Action { label, .. } => Some(label),
                _ => None,
            })
            .collect();
        assert!(
            labels
                .iter()
                .any(|l| l.contains("gemini · PATH command · coming soon")),
            "{labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|l| l.contains("antigravity · PATH command · not wired")),
            "{labels:?}"
        );
        assert!(
            labels
                .iter()
                .any(|l| l.contains("cursor · PATH command · conflict · coming soon")),
            "{labels:?}"
        );
    }

    fn integrations_view(
        integrations: Vec<crate::NativeAgentIntegrationRow>,
    ) -> crate::ProjectSettingsModalState {
        let mut state = crate::ProjectSettingsModalState::default();
        state.open(crate::ProjectSettingsView {
            permissions: Vec::new(),
            path: "/r/demo".into(),
            project_name: "demo".into(),
            initialization: crate::ProjectInitialization::Initialized,
            settings: Vec::new(),
            policy: crate::ProjectPolicySummary::default(),
            integrations,
        });
        state
    }

    fn adapter(
        agent: &str,
        remedies: Vec<crate::NativeAgentRemedy>,
    ) -> crate::NativeAgentIntegrationRow {
        crate::NativeAgentIntegrationRow {
            agent: agent.to_owned(),
            display_name: agent.to_owned(),
            detection: crate::NativeAgentDetection::Detected,
            executable_name: None,
            installations: Vec::new(),
            wiring: crate::NativeAgentWiring::Managed,
            hook_wiring: None,
            alpha_supported: true,
            selected: true,
            remedies,
        }
    }

    fn missing_binary() -> crate::NativeAgentRemedy {
        crate::NativeAgentRemedy {
            severity: crate::NativeAgentRemedySeverity::Error,
            headline: "Codex: hook binary missing".to_owned(),
            sentence: "The hooks run a Rift binary that is no longer there; run `rift \
                       integrations refresh` in this repository to point them at this one."
                .to_owned(),
        }
    }

    fn remedy_lines_after(rows: &[ProjectSettingsDisplayRow], agent: &str) -> Vec<String> {
        let at = rows
            .iter()
            .position(|row| matches!(row, ProjectSettingsDisplayRow::Action { label, .. } if label.starts_with(agent)))
            .unwrap();
        rows[at + 1..]
            .iter()
            .map_while(|row| match row {
                ProjectSettingsDisplayRow::Remedy { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn an_adapter_row_shows_its_agents_remedy_and_a_healthy_agent_shows_nothing_extra() {
        let state = integrations_view(vec![
            adapter("codex", vec![missing_binary()]),
            adapter("claude-code", Vec::new()),
        ]);
        let rows = project_settings_display_rows(&state);
        let codex = remedy_lines_after(&rows, "codex");
        assert_eq!(codex[0], "error: Codex: hook binary missing");
        assert!(codex.len() > 2, "the sentence wraps: {codex:?}");
        assert!(codex[1..].iter().all(|line| line.starts_with("  ")));
        assert!(codex
            .iter()
            .all(|line| line.chars().count() <= REMEDY_WRAP_COLS));
        let joined: Vec<&str> = codex[1..].iter().map(|line| line.trim()).collect();
        assert_eq!(joined.join(" "), missing_binary().sentence);
        assert!(remedy_lines_after(&rows, "claude-code").is_empty());
    }

    #[test]
    fn remedy_lines_add_no_click_target() {
        let healthy = integrations_view(vec![adapter("codex", Vec::new())]);
        let failing = integrations_view(vec![adapter("codex", vec![missing_binary()])]);
        let modal = Rect {
            x: 0,
            y: 0,
            w: 100,
            h: 40,
        };
        let chips = |state| project_settings_action_rects(modal, state);
        assert_eq!(chips(&healthy), chips(&failing));
    }

    #[test]
    fn the_selected_rows_remedies_scroll_into_view_with_it() {
        let integrations = (0..6)
            .map(|index| adapter(&format!("agent-{index}"), vec![missing_binary()]))
            .collect();
        let mut state = integrations_view(integrations);
        state.cursor = state.items().len() - 1;
        state.follow_cursor = true;
        let rows = project_settings_display_rows(&state);
        let visible = 8;
        let scroll = project_settings_scroll(&state, &rows, visible);
        let last = rows
            .iter()
            .rposition(|row| matches!(row, ProjectSettingsDisplayRow::Remedy { .. }))
            .unwrap();
        assert!(
            last < scroll + visible,
            "scroll {scroll}, rows {}",
            rows.len()
        );
    }

    #[test]
    fn the_wheel_scrolls_far_enough_to_show_the_last_remedy_line_of_the_last_adapter() {
        let integrations = (0..6)
            .map(|index| adapter(&format!("agent-{index}"), vec![missing_binary()]))
            .collect();
        let mut state = integrations_view(integrations);
        let visible = 8;
        // The wheel's own clamp: it scrolls no further than this.
        state.scroll = state.display_line_count().saturating_sub(visible);
        state.follow_cursor = false;
        let rows = project_settings_display_rows(&state);
        let scroll = project_settings_scroll(&state, &rows, visible);
        let last_remedy = rows
            .iter()
            .rposition(|row| matches!(row, ProjectSettingsDisplayRow::Remedy { .. }))
            .unwrap();
        assert!(last_remedy >= scroll && last_remedy < scroll + visible);
        assert!(
            rows.len() <= scroll + visible,
            "scroll {scroll}, rows {}",
            rows.len()
        );
    }

    #[test]
    fn a_word_longer_than_the_line_is_cut_and_nothing_is_lost() {
        let lines = wrap_words("ab abcdefgh c", 4);
        assert_eq!(lines, ["ab", "abcd", "efgh", "c"]);
        assert!(wrap_words("   ", 4).is_empty());
    }
}
