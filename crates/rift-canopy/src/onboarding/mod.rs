//! Standalone project onboarding. No dashboard state, feeds, or settings surface.

mod chrome;
mod progress;
mod render;
#[cfg(test)]
mod tests;

use crate::{CanopyKey, ProjectSettingsAction, ProjectSettingsActionKind};
use crate::{ProjectSettingsView, ProjectSetupPlan};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Harnesses,
    Permissions,
    Settings,
    Review,
}

impl Stage {
    const ALL: [Self; 4] = [
        Self::Harnesses,
        Self::Permissions,
        Self::Settings,
        Self::Review,
    ];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Harnesses => "Choose your harnesses",
            Self::Permissions => "Review harness permissions",
            Self::Settings => "Choose project settings",
            Self::Review => "Review and set up",
        }
    }
}

pub(crate) enum Choice {
    Harness(usize),
    Permission(usize),
    Setting(usize),
}

pub(crate) enum Action {
    None,
    Cancel,
    Apply(ProjectSettingsAction),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose {
    Project,
    Installation,
}

pub(crate) struct Wizard {
    purpose: Purpose,
    view: ProjectSettingsView,
    stage: Stage,
    cursor: usize,
    setup_confirmation: bool,
    pub(crate) status: String,
    progress: Option<progress::SetupMeter>,
}

impl Wizard {
    pub(crate) fn new(view: ProjectSettingsView) -> Self {
        Self {
            purpose: Purpose::Project,
            view,
            stage: Stage::Harnesses,
            cursor: 0,
            setup_confirmation: false,
            status: String::new(),
            progress: None,
        }
    }

    pub(crate) fn installer(view: ProjectSettingsView) -> Self {
        Self {
            purpose: Purpose::Installation,
            ..Self::new(view)
        }
    }

    /// The installer asks only which harnesses to suggest. Every feature
    /// ships on and is switched per project in Project Settings, so a
    /// machine-wide feature page would be one more step with no decision in it.
    fn stages(&self) -> &'static [Stage] {
        if self.purpose == Purpose::Installation {
            &[Stage::Harnesses, Stage::Review]
        } else {
            &Stage::ALL
        }
    }

    pub(crate) fn input_stage(&self) -> (Stage, bool) {
        (self.stage, self.setup_confirmation)
    }

    fn choices(&self) -> Vec<Choice> {
        match self.stage {
            Stage::Harnesses => self
                .view
                .integrations
                .iter()
                .enumerate()
                .filter(|(_, row)| row.alpha_supported)
                .map(|(i, _)| Choice::Harness(i))
                .collect(),
            Stage::Permissions => self
                .view
                .permissions
                .iter()
                .enumerate()
                .filter(|(_, row)| self.harness_selected(&row.agent))
                .map(|(i, _)| Choice::Permission(i))
                .collect(),
            Stage::Settings => self
                .view
                .settings
                .iter()
                .enumerate()
                .map(|(i, _)| Choice::Setting(i))
                .collect(),
            Stage::Review => Vec::new(),
        }
    }

    fn harness_selected(&self, agent: &str) -> bool {
        self.view
            .integrations
            .iter()
            .any(|row| row.agent == agent && row.selected)
    }

    pub(crate) fn key(&mut self, key: CanopyKey) -> Action {
        if key == CanopyKey::Quit {
            return Action::Cancel;
        }
        if self.setup_confirmation {
            return self.confirm_setup(key);
        }
        let choices = self.choices();
        let count = if self.stage == Stage::Review {
            self.review().len()
        } else {
            choices.len()
        };
        match key {
            CanopyKey::Esc if self.stage == Stage::Harnesses => return Action::Cancel,
            CanopyKey::Esc => self.move_stage(false),
            CanopyKey::Up | CanopyKey::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            CanopyKey::Down | CanopyKey::Char('j') => {
                self.cursor = self.cursor.saturating_add(1).min(count);
            }
            CanopyKey::Enter if self.cursor == count => return self.continue_stage(),
            CanopyKey::Enter | CanopyKey::Char(' ') => {
                if let Some(choice) = choices.get(self.cursor) {
                    self.activate(choice);
                }
            }
            _ => {}
        }
        Action::None
    }

    pub(crate) fn mouse(&mut self, mouse: crate::MouseInput, size: crate::render::Size) -> Action {
        match mouse.kind {
            crate::MouseKind::ScrollUp => self.key(CanopyKey::Up),
            crate::MouseKind::ScrollDown => self.key(CanopyKey::Down),
            crate::MouseKind::Down(crate::MouseButton::Left) => {
                let count = if self.stage == Stage::Review {
                    self.review().len()
                } else {
                    self.choices().len()
                };
                for (index, area) in self.row_areas(size, count) {
                    if mouse.col >= area.x
                        && mouse.col < area.x + area.w
                        && mouse.row >= area.y
                        && mouse.row < area.y + area.h
                    {
                        if self.setup_confirmation && index <= self.review().len() {
                            return Action::None;
                        }
                        self.cursor = index;
                        return self.key(CanopyKey::Enter);
                    }
                }
                Action::None
            }
            _ => Action::None,
        }
    }

    fn move_stage(&mut self, forward: bool) {
        let index = self
            .stages()
            .iter()
            .position(|stage| *stage == self.stage)
            .unwrap_or(0);
        let next = if forward {
            index.saturating_add(1)
        } else {
            index.saturating_sub(1)
        };
        self.stage = self.stages().get(next).copied().unwrap_or(self.stage);
        self.cursor = 0;
        self.status.clear();
    }

    fn continue_stage(&mut self) -> Action {
        if self.stage != Stage::Review {
            self.move_stage(true);
            return Action::None;
        }
        self.setup_confirmation = true;
        self.cursor = self.review().len() + 1;
        Action::None
    }

    fn confirm_setup(&mut self, key: CanopyKey) -> Action {
        let confirm = self.review().len() + 1;
        match key {
            CanopyKey::Up | CanopyKey::Char('k') => self.cursor = confirm,
            CanopyKey::Down | CanopyKey::Char('j') => self.cursor = confirm + 1,
            CanopyKey::Enter if self.cursor == confirm => {
                let plan = ProjectSetupPlan::from_view(&self.view);
                return Action::Apply(ProjectSettingsAction {
                    path: self.view.path.clone(),
                    kind: ProjectSettingsActionKind::Initialize(plan),
                });
            }
            CanopyKey::Esc | CanopyKey::Enter => self.back_to_review(),
            _ => {}
        }
        Action::None
    }

    fn back_to_review(&mut self) {
        self.setup_confirmation = false;
        self.cursor = self.review().len();
    }

    pub(crate) fn start_setup(&mut self) {
        self.progress = Some(if self.purpose == Purpose::Installation {
            progress::SetupMeter::defaults()
        } else {
            progress::SetupMeter::new(&self.view)
        });
        self.status = if self.purpose == Purpose::Installation {
            "Saving installation defaults…"
        } else {
            "Preparing project setup…"
        }
        .into();
    }

    pub(crate) fn record_setup(&mut self, report: crate::SetupProgress) {
        self.status = report.line();
        if let Some(meter) = &mut self.progress {
            meter.record(&report);
        }
    }

    pub(crate) fn finish_setup(&mut self, success: bool) {
        if success {
            if let Some(meter) = &mut self.progress {
                meter.complete = true;
            }
            self.status = if self.purpose == Purpose::Installation {
                "Installation complete — defaults saved"
            } else {
                "Setup complete — opening Canopy"
            }
            .into();
        } else {
            self.progress = None;
            self.back_to_review();
        }
    }

    fn activate(&mut self, choice: &Choice) {
        self.status.clear();
        match *choice {
            Choice::Harness(i) => {
                if let Some(row) = self.view.integrations.get_mut(i) {
                    row.selected = !row.selected;
                    if !row.selected {
                        for permission in &mut self.view.permissions {
                            if permission.agent == row.agent {
                                permission.selected = 0;
                            }
                        }
                    }
                }
            }
            Choice::Permission(i) => {
                if let Some(row) = self.view.permissions.get_mut(i) {
                    row.selected = (row.selected + 1) % row.options.len().max(1);
                }
            }
            Choice::Setting(i) => {
                if let Some(row) = self.view.settings.get_mut(i).filter(|row| row.actionable) {
                    row.enabled = !row.enabled;
                }
            }
        }
    }
}
