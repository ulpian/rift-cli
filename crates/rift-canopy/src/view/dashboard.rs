//! Full-width tab views, retaining the existing card and graph renderers.

use super::text::clip_at;
use crate::dashboard::{geometry, tab_rects, DashboardTab};
use crate::render::{Frame, RenderBackend};
use crate::state::ViewModel;

pub(super) fn draw(
    vm: &ViewModel,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
    unicode: bool,
) {
    let (body, hint, command) = geometry(vm.size);
    match vm.dashboard_tab {
        DashboardTab::Home => {
            let (repo, recall) = crate::dashboard::home_rects(vm.size);
            clip_at(
                frame,
                recall.x + 1,
                recall.y,
                recall.w.saturating_sub(2),
                &recall_status(vm),
                vm.theme.dim(),
            );
            let recall = crate::dashboard::home_recall_content(vm.size);
            super::grid::draw_detail(
                vm,
                backend,
                frame,
                repo,
                unicode,
                &[crate::cards::CardKind::Repository],
            );
            super::grid::draw_graphs(
                vm,
                backend,
                frame,
                recall,
                &[crate::col3::SectionId::Recall],
            );
        }
        DashboardTab::Models => super::grid::draw_detail(
            vm,
            backend,
            frame,
            body,
            unicode,
            &[crate::cards::CardKind::Models],
        ),
        DashboardTab::Activity => super::grid::draw_detail(
            vm,
            backend,
            frame,
            body,
            unicode,
            &[crate::cards::CardKind::Activity],
        ),
        DashboardTab::Tokens => super::grid::draw_graphs(
            vm,
            backend,
            frame,
            body,
            &[crate::col3::SectionId::TokenUsage],
        ),
        DashboardTab::Recall | DashboardTab::Settings => {}
    }
    let hints = "← Projects  [/] tabs  s Settings  r Recall viewer  / command  q Quit";
    clip_at(frame, hint.x, hint.y, hint.w, hints, vm.theme.dim());
    super::grid::draw_command(vm, frame, command);
}

pub(super) fn draw_navigation(vm: &ViewModel, frame: &mut Frame) {
    let arrow = if vm.projects_open { "[>]" } else { "[<]" };
    clip_at(frame, 0, 1, vm.size.w, arrow, vm.theme.key_hint());
    for (tab, rect) in tab_rects(vm.size, vm.dashboard_tab) {
        let label = tab.label(vm.size.w < 82);
        let text = if tab == vm.dashboard_tab {
            format!("[{label}]")
        } else {
            format!(" {label} ")
        };
        let style = if tab == vm.dashboard_tab {
            vm.theme.key_hint()
        } else {
            vm.theme.dim()
        };
        clip_at(frame, rect.x, rect.y, rect.w, &text, style);
    }
    for ((_, rect), label) in crate::dashboard::tab_steps(vm.size, vm.dashboard_tab)
        .into_iter()
        .zip(["< ", "> "])
    {
        clip_at(frame, rect.x, rect.y, rect.w, label, vm.theme.key_hint());
    }
    if let Some(detail) = vm.detail.as_deref() {
        let text = crate::col2::sanitize_for_terminal(&format!(
            "{} · {} · {}",
            detail.label,
            detail
                .git_summary
                .as_ref()
                .map_or("branch unknown", |git| git.branch.as_str()),
            detail.path
        ));
        clip_at(
            frame,
            1,
            2,
            vm.size.w.saturating_sub(2),
            &text,
            vm.theme.dim(),
        );
    }
    let scope = if vm.size.w < 82 {
        "Local facts only; Recall: family"
    } else {
        "Models/activity/tokens: this worktree; unknown attribution excluded. Recall: family."
    };
    clip_at(
        frame,
        1,
        3,
        vm.size.w.saturating_sub(2),
        scope,
        vm.theme.dim(),
    );
}

pub(super) fn draw_drawer(vm: &ViewModel, frame: &mut Frame, unicode: bool) {
    if vm.projects_open {
        let rect = crate::dashboard::drawer_rect(vm.size);
        crate::chrome::fill(frame, rect, vm.theme.dim().with_bg(vm.theme.surface_bg()));
        super::rail::draw_projects(vm, frame, rect, unicode);
    }
}

fn recall_status(vm: &ViewModel) -> String {
    let Some(detail) = vm.detail.as_deref() else {
        return "Recall readiness unavailable".into();
    };
    let settings = &detail.project_settings;
    if matches!(
        settings.policy.state,
        crate::ProjectPolicyState::Invalid | crate::ProjectPolicyState::Refused
    ) {
        return "Recall degraded: policy needs repair (Settings)".into();
    }
    if settings.initialization != crate::ProjectInitialization::Initialized {
        return "Recall setup required (Settings)".into();
    }
    if !settings
        .settings
        .iter()
        .any(|row| row.id == crate::ProjectSettingId::TotalRecall && row.enabled)
    {
        return "Recall disabled (Settings)".into();
    }
    "Recall enabled · runtime readiness: rift recall status".into()
}
