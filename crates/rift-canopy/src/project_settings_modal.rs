//! Pure interaction state for the repository Project Settings modal.

use crate::{
    ProjectInitialization, ProjectSettingId, ProjectSettingsAction, ProjectSettingsActionKind,
    ProjectSettingsView, ProjectSetupPlan,
};

/// One actionable modal row. Informational policy rows are deliberately absent
/// so keyboard navigation visits actions only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectSettingsItem {
    /// Cycle one supported native permission choice; stages only.
    Permission(String),
    /// Apply all staged permission changes on an initialized project.
    ApplyPermissions,
    /// A policy setting.
    Setting(ProjectSettingId),
    /// A native adapter by stable registry token.
    Integration(String),
    /// The staged setup's final local initialization act.
    Initialize,
}

/// Transient state for one open Project Settings modal.
#[derive(Clone, Debug, Default)]
pub struct ProjectSettingsModalState {
    /// Injected onboarding animation time; stationary under reduced motion.
    pub animation_seconds: f32,
    /// Last verified view.
    pub view: Option<Box<ProjectSettingsView>>,
    /// Cursor in [`Self::items`], never on informational rows.
    pub cursor: usize,
    /// Scroll offset in display lines.
    pub scroll: usize,
    /// Keep keyboard/mouse action selection visible; wheel scrolling clears it
    /// until the cursor moves again.
    pub follow_cursor: bool,
    /// A synchronous lifecycle operation is in flight; all controls lock.
    pub running: bool,
    /// Inline success or failure text.
    pub result: Option<String>,
    /// Last failed operation, retained for `r`.
    pub failed_action: Option<ProjectSettingsAction>,
    /// Raw-transcript enable warning is awaiting explicit confirmation.
    pub confirm_raw: bool,
    /// A successful staged setup has crossed to `Initialized`; reject queued
    /// pre-setup polls until the first fresh initialized poll arrives.
    reject_pre_init_poll: bool,
}

impl ProjectSettingsModalState {
    /// Open on a freshly projected view.
    pub fn open(&mut self, view: ProjectSettingsView) {
        self.view = Some(Box::new(view));
        self.cursor = 0;
        self.scroll = 0;
        self.follow_cursor = true;
        self.running = false;
        self.result = None;
        self.failed_action = None;
        self.confirm_raw = false;
        self.reject_pre_init_poll = false;
        self.clamp_cursor();
    }

    /// Close and clear transient operation state.
    pub fn close(&mut self) {
        *self = Self::default();
    }

    /// Actionable rows in stable section/registry order.
    #[must_use]
    pub fn items(&self) -> Vec<ProjectSettingsItem> {
        let Some(view) = self.view.as_deref() else {
            return Vec::new();
        };
        if view.settings.is_empty() {
            return Vec::new();
        }
        let mut items = view
            .settings
            .iter()
            .filter(|row| row.actionable)
            .map(|row| ProjectSettingsItem::Setting(row.id))
            .collect::<Vec<_>>();
        items.extend(
            view.integrations
                .iter()
                .map(|row| ProjectSettingsItem::Integration(row.agent.clone())),
        );
        items.extend(
            view.permissions
                .iter()
                .filter(|row| view.integration_selected(&row.agent))
                .map(|row| ProjectSettingsItem::Permission(row.agent.clone())),
        );
        if !view.initialization.offers_setup() && !view.permission_changes().is_empty() {
            items.push(ProjectSettingsItem::ApplyPermissions);
        }
        if view.initialization.offers_setup() {
            items.push(ProjectSettingsItem::Initialize);
        }
        items
    }

    /// One-line explanation for the currently focused actionable row.
    #[must_use]
    pub fn focused_description(&self) -> Option<String> {
        let view = self.view.as_deref()?;
        match self.items().get(self.cursor)? {
            ProjectSettingsItem::Permission(agent) => {
                let row = view.permissions.iter().find(|row| &row.agent == agent)?;
                row.options.get(row.selected).map(|option| option.detail.clone())
            }
            ProjectSettingsItem::ApplyPermissions => Some("Apply the permission changes shown above to this project only. Keep-current makes no change.".into()),
            ProjectSettingsItem::Setting(id) => Some(id.description().to_owned()),
            ProjectSettingsItem::Integration(agent) => {
                let row = view.integrations.iter().find(|row| &row.agent == agent)?;
                let wiring = match row.wiring {
                    crate::NativeAgentWiring::Missing => "absent",
                    crate::NativeAgentWiring::Managed => "managed",
                    crate::NativeAgentWiring::Conflict => "conflict protected",
                    crate::NativeAgentWiring::Invalid => "invalid protected",
                };
                // The description predicts the manager's refusal instead of
                // letting the toggle discover it. No agent list here — the
                // per-row fact travels from the data plane.
                let coming_soon = if row.alpha_supported {
                    ""
                } else {
                    "; capture coming soon in this alpha"
                };
                // One availability sentence for every agent — the same words
                // the CLI prints — so the reason Rift believes an agent is
                // installed (and would auto-wire it) is on screen. Detection
                // is availability only: nothing here claims the agent is in
                // use.
                let availability = if row.installations.is_empty() {
                    "Not installed on this machine".to_owned()
                } else {
                    format!(
                        "Installed: {} (availability only)",
                        row.availability()
                            .strip_prefix("present via ")
                            .unwrap_or_default()
                    )
                };
                let Some(hooks) = row.hook_wiring else {
                    return Some(format!(
                        "{availability}; wiring: {wiring}{coming_soon}."
                    ));
                };
                let hooks = match hooks {
                    crate::NativeAgentHookWiring::Missing => " hooks: absent",
                    crate::NativeAgentHookWiring::Managed => " hooks: Rift-managed",
                    crate::NativeAgentHookWiring::Stale => " hooks: managed but need refresh",
                    crate::NativeAgentHookWiring::Conflict => " hooks: conflict protected",
                    crate::NativeAgentHookWiring::Invalid => " hooks: invalid protected",
                };
                Some(format!(
                    "{availability}; capture descriptor: {wiring};{hooks}{coming_soon}."
                ))
            }
            ProjectSettingsItem::Initialize => Some(
                "Apply selected policy, capture wiring, Recall ingest, and skills as resumable setup."
                    .to_owned(),
            ),
        }
    }

    /// Move over actionable rows only.
    pub fn move_cursor(&mut self, down: bool, page: bool) {
        let last = self.items().len().saturating_sub(1);
        let step = if page { 6 } else { 1 };
        self.cursor = if down {
            self.cursor.saturating_add(step).min(last)
        } else {
            self.cursor.saturating_sub(step)
        };
        self.follow_cursor = true;
        self.scroll = self.scroll.min(self.display_line_count().saturating_sub(1));
    }

    /// Select an actionable row by index (mouse path).
    pub fn select(&mut self, index: usize) {
        self.cursor = index.min(self.items().len().saturating_sub(1));
        self.follow_cursor = true;
    }

    /// Activate the cursor. During staged setup this edits the in-memory plan;
    /// initialized projects return an immediate lifecycle action.
    #[must_use]
    pub fn activate(&mut self) -> Option<ProjectSettingsAction> {
        if self.running {
            return None;
        }
        if self.confirm_raw {
            self.confirm_raw = false;
            let initialized =
                self.view.as_ref()?.initialization == ProjectInitialization::Initialized;
            if initialized {
                return self.setting_action(ProjectSettingId::RawTranscripts, true);
            }
            if let Some(row) = self
                .view
                .as_mut()?
                .settings
                .iter_mut()
                .find(|row| row.id == ProjectSettingId::RawTranscripts)
            {
                row.enabled = true;
            }
            self.result = Some("setup choices updated".to_owned());
            return None;
        }
        let item = self.items().get(self.cursor)?.clone();
        match item {
            ProjectSettingsItem::Permission(agent) => {
                let row = self
                    .view
                    .as_mut()?
                    .permissions
                    .iter_mut()
                    .find(|row| row.agent == agent)?;
                row.selected = (row.selected + 1) % row.options.len().max(1);
                self.result = Some("Permission choice staged. Review its exact effect below, then confirm Apply / Initialize.".into());
                None
            }
            ProjectSettingsItem::ApplyPermissions => Some(ProjectSettingsAction {
                path: self.view.as_ref()?.path.clone(),
                kind: ProjectSettingsActionKind::SetPermissions(
                    self.view.as_ref()?.permission_changes(),
                ),
            }),
            ProjectSettingsItem::Setting(id) => self.activate_setting(id),
            ProjectSettingsItem::Integration(agent) => self.activate_integration(&agent),
            ProjectSettingsItem::Initialize => self.initialize_action(),
        }
    }

    /// Retry the last failed action.
    #[must_use]
    pub fn retry(&self) -> Option<ProjectSettingsAction> {
        (!self.running)
            .then(|| self.failed_action.clone())
            .flatten()
    }

    /// Replace state after a verified lifecycle operation.
    pub fn refresh(&mut self, view: ProjectSettingsView) {
        self.reject_pre_init_poll = self.view.as_deref().is_some_and(|current| {
            current.path == view.path
                && current.initialization != ProjectInitialization::Initialized
                && view.initialization == ProjectInitialization::Initialized
        });
        self.view = Some(Box::new(view));
        self.running = false;
        self.confirm_raw = false;
        self.clamp_cursor();
    }

    /// Refresh external policy and integration facts without discarding a
    /// staged setup plan or an in-progress raw-transcript acknowledgement.
    /// A transition to `Initialized` is authoritative and replaces the staged
    /// values after successful initialization.
    pub fn poll(&mut self, mut view: ProjectSettingsView) {
        if self.running {
            return;
        }
        if let Some(current) = self.view.as_deref() {
            if current.path == view.path {
                for row in &mut view.permissions {
                    if let Some(staged) = current
                        .permissions
                        .iter()
                        .find(|old| old.agent == row.agent && old.selected != 0)
                    {
                        row.selected = staged.selected;
                        row.revision.clone_from(&staged.revision);
                    }
                }
            }
            if self.reject_pre_init_poll && current.path == view.path {
                if view.initialization != ProjectInitialization::Initialized {
                    return;
                }
                self.reject_pre_init_poll = false;
            }
            let same_staged_project = current.path == view.path
                && current.initialization != ProjectInitialization::Initialized
                && view.initialization != ProjectInitialization::Initialized;
            if same_staged_project {
                for row in &mut view.settings {
                    if let Some(staged) = current.settings.iter().find(|old| old.id == row.id) {
                        row.enabled = staged.enabled;
                    }
                }
                for row in &mut view.integrations {
                    if let Some(staged) = current
                        .integrations
                        .iter()
                        .find(|old| old.agent == row.agent)
                    {
                        row.selected = staged.selected;
                    }
                }
            }
        }
        self.view = Some(Box::new(view));
        self.clamp_cursor();
    }

    /// Number of logical display rows, including section headings and policy
    /// facts. Used only to clamp wheel scrolling.
    #[must_use]
    pub fn display_line_count(&self) -> usize {
        crate::view::project_settings_display_rows(self).len()
    }

    fn activate_setting(&mut self, id: ProjectSettingId) -> Option<ProjectSettingsAction> {
        let initialized = self.view.as_ref()?.initialization == ProjectInitialization::Initialized;
        let current = self
            .view
            .as_ref()?
            .settings
            .iter()
            .find(|row| row.id == id)?
            .enabled;
        if id == ProjectSettingId::RawTranscripts && !current {
            self.confirm_raw = true;
            self.result = Some(
                "Raw transcripts may retain sensitive source and prompts. Press Enter again to acknowledge, or Esc to cancel."
                    .to_owned(),
            );
            return None;
        }
        if initialized {
            self.setting_action(id, !current)
        } else {
            if let Some(row) = self
                .view
                .as_mut()?
                .settings
                .iter_mut()
                .find(|row| row.id == id)
            {
                row.enabled = !current;
            }
            self.result = Some("setup choices updated".to_owned());
            None
        }
    }

    fn setting_action(
        &self,
        setting: ProjectSettingId,
        enabled: bool,
    ) -> Option<ProjectSettingsAction> {
        Some(ProjectSettingsAction {
            path: self.view.as_ref()?.path.clone(),
            kind: ProjectSettingsActionKind::Set { setting, enabled },
        })
    }

    fn activate_integration(&mut self, agent: &str) -> Option<ProjectSettingsAction> {
        let view = self.view.as_mut()?;
        let row = view
            .integrations
            .iter_mut()
            .find(|row| row.agent == agent)?;
        if !row.alpha_supported && !row.enabled() {
            self.result =
                Some("Capture support for this harness is coming soon in this alpha.".into());
            return None;
        }
        let enabled = !row.enabled();
        if view.initialization == ProjectInitialization::Initialized {
            Some(ProjectSettingsAction {
                path: view.path.clone(),
                kind: ProjectSettingsActionKind::SetIntegration {
                    agent: agent.to_owned(),
                    enabled,
                },
            })
        } else {
            row.selected = !row.selected;
            self.result = Some("setup choices updated".to_owned());
            None
        }
    }

    fn initialize_action(&self) -> Option<ProjectSettingsAction> {
        let view = self.view.as_ref()?;
        Some(ProjectSettingsAction {
            path: view.path.clone(),
            kind: ProjectSettingsActionKind::Initialize(ProjectSetupPlan::from_view(view)),
        })
    }

    fn clamp_cursor(&mut self) {
        self.cursor = self.cursor.min(self.items().len().saturating_sub(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        NativeAgentDetection, NativeAgentIntegrationRow, NativeAgentWiring, ProjectPolicySummary,
        ProjectSettingRow,
    };

    fn view(initialization: ProjectInitialization) -> ProjectSettingsView {
        ProjectSettingsView {
            permissions: Vec::new(),
            path: "/r/demo".into(),
            project_name: "demo".into(),
            initialization,
            settings: ProjectSettingId::ALL
                .iter()
                .map(|id| ProjectSettingRow {
                    id: *id,
                    enabled: *id != ProjectSettingId::RawTranscripts,
                    actionable: true,
                    detail: None,
                })
                .collect(),
            policy: ProjectPolicySummary::default(),
            integrations: vec![NativeAgentIntegrationRow {
                agent: "codex".into(),
                display_name: "Codex".into(),
                detection: NativeAgentDetection::Detected,
                executable_name: Some("codex".into()),
                installations: vec![
                    crate::NativeAgentInstallation {
                        source: crate::NativeAgentInstallSource::PathCommand,
                        version: None,
                    },
                    crate::NativeAgentInstallation {
                        source: crate::NativeAgentInstallSource::IdeExtension(
                            crate::NativeAgentIdeHost::VsCode,
                        ),
                        version: Some("26.803.61601".into()),
                    },
                ],
                wiring: NativeAgentWiring::Missing,
                hook_wiring: Some(crate::NativeAgentHookWiring::Missing),
                alpha_supported: true,
                selected: true,
                remedies: Vec::new(),
            }],
        }
    }

    #[test]
    fn raw_enable_always_requires_a_second_explicit_enter() {
        let mut state = ProjectSettingsModalState::default();
        state.open(view(ProjectInitialization::Initialized));
        state.cursor = 6;
        assert!(state.activate().is_none());
        assert!(state.confirm_raw);
        assert!(matches!(
            state.activate().map(|action| action.kind),
            Some(ProjectSettingsActionKind::Set {
                setting: ProjectSettingId::RawTranscripts,
                enabled: true
            })
        ));
    }

    #[test]
    fn every_actionable_row_has_focus_help_and_movement_changes_it() {
        let mut state = ProjectSettingsModalState::default();
        state.open(view(ProjectInitialization::NotInitialized));
        for cursor in 0..state.items().len() {
            state.cursor = cursor;
            assert!(
                state
                    .focused_description()
                    .is_some_and(|text| !text.is_empty()),
                "item {cursor:?} has no focused description"
            );
        }

        state.cursor = 0;
        assert!(state
            .focused_description()
            .unwrap()
            .contains("git-native project memory"));
        state.cursor = 7;
        let integration = state.focused_description().unwrap();
        assert!(
            integration.contains(
                "Installed: PATH command + VS Code extension 26.803.61601 (availability only)"
            ),
            "{integration}"
        );
        assert!(integration.contains("hooks: absent"));
        state.cursor = state.items().len() - 1;
        assert!(state
            .focused_description()
            .unwrap()
            .contains("resumable setup"));
    }

    #[test]
    fn setup_edits_locally_then_emits_one_complete_plan() {
        let mut state = ProjectSettingsModalState::default();
        state.open(view(ProjectInitialization::NotInitialized));
        state.cursor = state.items().len() - 1;
        let action = state.activate().unwrap();
        let ProjectSettingsActionKind::Initialize(plan) = action.kind else {
            panic!("expected initialize")
        };
        assert!(plan.total_recall);
        assert!(plan.integrations.contains(&"codex".to_owned()));
        assert!(!plan.raw_transcripts);
    }

    #[test]
    fn setup_poll_preserves_local_choices_and_raw_confirmation() {
        let mut state = ProjectSettingsModalState::default();
        state.open(view(ProjectInitialization::NotInitialized));
        state.cursor = 0;
        assert!(state.activate().is_none());
        state.cursor = 6;
        assert!(state.activate().is_none());
        assert!(state.confirm_raw);

        state.poll(view(ProjectInitialization::NotInitialized));
        assert!(!state.view.as_ref().unwrap().settings[0].enabled);
        assert!(state.confirm_raw, "a fact poll cannot dismiss the warning");
        assert!(state.activate().is_none());
        state.cursor = state.items().len() - 1;
        let action = state.activate().unwrap();
        let ProjectSettingsActionKind::Initialize(plan) = action.kind else {
            panic!("expected initialize")
        };
        assert!(
            plan.raw_transcripts,
            "acknowledgement updates the staged plan"
        );
    }

    #[test]
    fn successful_setup_rejects_queued_pre_init_polls_until_fresh_truth_arrives() {
        let mut state = ProjectSettingsModalState::default();
        state.open(view(ProjectInitialization::NotInitialized));

        let initialized = view(ProjectInitialization::Initialized);
        state.refresh(initialized.clone());
        state.poll(view(ProjectInitialization::SetupIncomplete));
        assert_eq!(
            state.view.as_deref().unwrap().initialization,
            ProjectInitialization::Initialized
        );

        state.poll(initialized);
        state.poll(view(ProjectInitialization::NotInitialized));
        assert_eq!(
            state.view.as_deref().unwrap().initialization,
            ProjectInitialization::NotInitialized,
            "a later external removal is visible after fresh initialized truth"
        );
    }
}
