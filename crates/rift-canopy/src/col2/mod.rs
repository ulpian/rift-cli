//! Column 2 — the single-project detail view.
//!
//! Like [`crate::col1`], this module is **pure**: it holds the detail *data model*
//! ([`DetailView`] and its parts) and the terminal-escape [`sanitize`] gate, but no
//! I/O. The bin reads a project's projection + effective config off-thread, builds
//! a [`DetailView`], and pushes it to the reducer through the data bus; the renderer
//! draws it (every untrusted string first passed through
//! [`sanitize::sanitize_for_terminal`]). A cheap [`DetailView::thin`] fallback keeps
//! the panel populated before the bus's rich detail arrives.

pub mod project_settings;
pub mod sanitize;

pub use project_settings::{
    NativeAgentDetection, NativeAgentHookWiring, NativeAgentIdeHost, NativeAgentInstallSource,
    NativeAgentInstallation, NativeAgentIntegrationRow, NativeAgentWiring,
    NullProjectSettingsManager, ProjectInitialization, ProjectPolicyState, ProjectPolicySummary,
    ProjectSettingId, ProjectSettingRow, ProjectSettingsAction, ProjectSettingsActionKind,
    ProjectSettingsManager, ProjectSettingsView, ProjectSetupPlan, SetupLog, SetupProgress,
    SetupStep, SetupStepStatus,
};
pub use sanitize::{escape_for_terminal, sanitize_for_terminal, MAX_ESCAPED};

use crate::col1::EntryKind;
use crate::ProjectStatus;

/// The full column-2 detail for the selected project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailView {
    /// Fresh worktree branch and tracked-file status, absent while unavailable.
    pub git_summary: Option<GitSummary>,
    /// The project's canonical path (its key).
    pub path: String,
    /// The display label.
    pub label: String,
    /// Repo vs linked worktree.
    pub kind: EntryKind,
    /// The live status.
    pub status: ProjectStatus,
    /// Models seen in the project's sessions, most-used first.
    pub models: Vec<ModelUsage>,
    /// Capture activity counts.
    ///
    /// The dedicated capture card that tiled these three counts is retired —
    /// the activity card strips the same three over a month and the dispatch
    /// panel reports them exactly — but the summary itself is a **fact the
    /// data plane reads**, not the card's private state, so it stays on the
    /// detail. `last_checkpoint_at_ms` in particular is a projection reading
    /// no card ever drew.
    pub capture: CaptureSummary,
    /// The family members this row's `Running` count is spread across, by name
    /// (`main` for the main work tree), in member order (the family view).
    ///
    /// Filled for a **family row**, whose session count is the sum over every
    /// member: the header names them, so a `3` on the badge can be read back to
    /// the checkouts it came from. Empty for a member row, whose count is its
    /// own and needs no breakdown, and empty whenever nothing is running.
    pub running_members: Vec<String>,
    /// Repository-owned Project Settings policy and native integration view.
    pub project_settings: Box<ProjectSettingsView>,
}

impl DetailView {
    /// A minimal detail from just the list row — the fallback shown until the bus
    /// delivers the rich, projection-backed detail (so the panel is never blank).
    #[must_use]
    pub fn thin(path: String, label: String, status: ProjectStatus) -> Self {
        Self {
            git_summary: None,
            path: path.clone(),
            label: label.clone(),
            kind: EntryKind::Repo,
            status,
            models: Vec::new(),
            capture: CaptureSummary::default(),
            running_members: Vec::new(),
            project_settings: Box::new(ProjectSettingsView::thin(path.clone(), label.clone())),
        }
    }
}

/// Worktree-local Git facts, gathered outside the renderer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitSummary {
    /// Branch name or a detached-HEAD label.
    pub branch: String,
    /// Tracked-file modifications; untracked files are not part of this probe.
    pub tracked_changes: Option<bool>,
}

/// One model and how many calls used it (aggregated across the project's sessions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelUsage {
    /// The model identifier (untrusted → sanitized before render).
    pub model: String,
    /// How many model calls used it.
    pub calls: u64,
    /// Where Rift learned this model identifier.
    pub source: ModelProvenance,
}

/// Provenance of a displayed model identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelProvenance {
    /// Observed in a native runtime hook or model call.
    Runtime,
    /// Read from a concrete client configuration value.
    Configured,
}

/// Capture activity counts for the project (all read-only from the projection).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureSummary {
    /// Latest attributed checkpoint identifier when a record is available.
    pub last_checkpoint_id: Option<String>,
    /// Total captured events.
    pub events: u64,
    /// Total sessions.
    pub sessions: u64,
    /// Total checkpoints.
    pub checkpoints: u64,
    /// The most recent checkpoint's time (epoch-ms), if any.
    pub last_checkpoint_at_ms: Option<i64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thin_detail_has_empty_sections() {
        let d = DetailView::thin("/r/a".into(), "a".into(), ProjectStatus::Idle);
        assert_eq!(d.path, "/r/a");
        assert_eq!(d.label, "a");
        assert_eq!(d.kind, EntryKind::Repo);
        assert!(d.models.is_empty());
        assert_eq!(d.capture, CaptureSummary::default());
        assert!(
            d.running_members.is_empty(),
            "a thin detail names no running member: the bus has not read one yet"
        );
    }
}
