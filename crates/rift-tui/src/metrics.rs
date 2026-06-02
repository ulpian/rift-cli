//! The metrics snapshot rendered in the right-hand panel.
//!
//! [`MetricsSnapshot::load`] reads the `entire/checkpoints/v1` branch through a
//! [`ShellGitReader`] and aggregates token usage and file-change counts per
//! checkpoint. When the branch is absent or empty, an empty snapshot is returned
//! with an explanatory [`note`](MetricsSnapshot::note) shown in the headline.

use std::path::Path;

use rift_core::git::ShellGitReader;
use rift_core::metrics::{read_branch_metrics, BranchMetrics};
use rift_core::paths::CHECKPOINT_BRANCH;

/// Aggregated usage metrics for the dashboard.
#[derive(Debug, Clone, Default)]
pub struct MetricsSnapshot {
    /// Total tokens per checkpoint, chronological (drives the sparkline).
    pub token_series: Vec<u64>,
    /// File-change counts per checkpoint (drives the bar chart).
    pub file_changes: Vec<(String, u64)>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub checkpoints: usize,
    /// A headline note (e.g. "no checkpoint data") when there's nothing real to
    /// show; `None` when the data is live.
    pub note: Option<String>,
}

impl MetricsSnapshot {
    /// Read metrics from the `entire/checkpoints/v1` branch in `repo`.
    #[must_use]
    pub fn load(repo: &Path) -> Self {
        let git = ShellGitReader::new(repo);
        match read_branch_metrics(&git, CHECKPOINT_BRANCH) {
            Ok(metrics) if !metrics.is_empty() => Self::from_branch(&metrics),
            Ok(_) => Self::empty(format!("no checkpoint data on {CHECKPOINT_BRANCH}")),
            Err(e) => Self::empty(format!("could not read {CHECKPOINT_BRANCH}: {e}")),
        }
    }

    /// Build a snapshot from aggregated branch metrics.
    #[must_use]
    pub fn from_branch(metrics: &BranchMetrics) -> Self {
        let token_series = metrics
            .checkpoints
            .iter()
            .map(rift_core::CheckpointMetric::total_tokens)
            .collect();

        let file_changes = metrics
            .checkpoints
            .iter()
            .map(|c| {
                let label = c.id.chars().take(6).collect::<String>();
                (label, c.files_touched as u64)
            })
            .collect();

        Self {
            token_series,
            file_changes,
            total_input_tokens: metrics.total_input_tokens(),
            total_output_tokens: metrics.total_output_tokens(),
            checkpoints: metrics.checkpoints.len(),
            note: None,
        }
    }

    /// An empty snapshot carrying an explanatory note.
    #[must_use]
    pub fn empty(note: impl Into<String>) -> Self {
        Self {
            note: Some(note.into()),
            ..Self::default()
        }
    }

    /// Sample data for demos and tests (no live branch read).
    #[must_use]
    pub fn placeholder() -> Self {
        Self {
            token_series: vec![12, 30, 18, 45, 27, 60, 38, 52, 41, 70],
            file_changes: vec![
                ("ckpt-01".into(), 3),
                ("ckpt-02".into(), 7),
                ("ckpt-03".into(), 2),
                ("ckpt-04".into(), 9),
                ("ckpt-05".into(), 5),
            ],
            total_input_tokens: 184_000,
            total_output_tokens: 42_500,
            checkpoints: 5,
            note: Some("sample data".to_string()),
        }
    }
}
