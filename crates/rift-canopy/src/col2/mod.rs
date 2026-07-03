//! Column 2 — the single-project detail view (P5).
//!
//! Like [`crate::col1`], this module is **pure**: it holds the detail *data model*
//! ([`DetailView`] and its parts) and the terminal-escape [`sanitize`] gate, but no
//! I/O. The bin reads a project's projection + effective config off-thread, builds
//! a [`DetailView`], and pushes it to the reducer through the data bus; the renderer
//! draws it (every untrusted string first passed through
//! [`sanitize::sanitize_for_terminal`]). A cheap [`DetailView::thin`] fallback keeps
//! the panel populated before the bus's rich detail arrives.

pub mod manager;
pub mod sanitize;
pub mod toggles;

pub use manager::{FeatureManager, NullFeatureManager};
pub use sanitize::sanitize_for_terminal;
pub use toggles::{
    current_toggles, plan_flip, toggle_view, Feature, FeatureAction, FeatureActionKind,
    ToggleChange, ToggleEvent, ToggleRow, ToggleState, ToggleView,
};

use crate::col1::EntryKind;
use crate::ProjectStatus;

/// The full column-2 detail for the selected project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetailView {
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
    /// A safe summary of the project's effective policy (counts, not patterns).
    pub settings: SettingsSummary,
    /// Capture activity counts.
    pub capture: CaptureSummary,
    /// The six feature toggles' current (persisted) states.
    pub toggles: ToggleView,
}

impl DetailView {
    /// A minimal detail from just the list row — the fallback shown until the bus
    /// delivers the rich, projection-backed detail (so the panel is never blank).
    #[must_use]
    pub fn thin(path: String, label: String, status: ProjectStatus) -> Self {
        Self {
            path,
            label,
            kind: EntryKind::Repo,
            status,
            models: Vec::new(),
            settings: SettingsSummary::default(),
            capture: CaptureSummary::default(),
            toggles: ToggleView::default(),
        }
    }
}

/// One model and how many calls used it (aggregated across the project's sessions).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelUsage {
    /// The model identifier (untrusted → sanitized before render).
    pub model: String,
    /// How many model calls used it.
    pub calls: u64,
}

/// A **safe** projection of a project's effective policy. Deliberately exposes
/// *counts and booleans*, never the raw sensitive-path globs (surfacing those in
/// the dashboard would leak the very patterns the policy protects).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsSummary {
    /// How many sensitive-path globs are configured (never the globs themselves).
    pub sensitive_path_count: usize,
    /// Whether raw (un-redacted) transcripts are enabled — the dangerous toggle.
    pub raw_transcripts: bool,
    /// Whether tool calls are captured.
    pub captures_tool_calls: bool,
    /// Whether token usage is captured.
    pub captures_token_usage: bool,
}

/// Capture activity counts for the project (all read-only from the projection).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CaptureSummary {
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
        assert_eq!(d.settings, SettingsSummary::default());
        assert_eq!(d.capture, CaptureSummary::default());
        assert!(d.toggles.rows.is_empty());
    }
}
