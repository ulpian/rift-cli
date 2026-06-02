//! Aggregate usage metrics by reading the `entire/checkpoints/v1` branch.
//!
//! The branch is not checked out in the working tree, so we read it through a
//! [`GitReader`] (`git ls-tree` + `git show`). For each checkpoint shard we read
//! `metadata.json` (created-at, files touched) and, per session, the
//! `token_estimates.json` and `tool_calls.jsonl`. Results are returned in
//! chronological order so they can drive the dashboard's time-series widgets.

use std::collections::{BTreeMap, BTreeSet};

use serde::de::DeserializeOwned;

use crate::git::{GitError, GitReader};
use crate::jsonl;
use crate::model::{CheckpointSummary, TokenEstimates};

/// Errors reading branch metrics.
#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error(transparent)]
    Git(#[from] GitError),
}

/// Aggregated metrics for a single checkpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CheckpointMetric {
    pub id: String,
    pub created_at: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cached_input_tokens: u64,
    pub files_touched: usize,
    pub tool_calls: usize,
}

impl CheckpointMetric {
    /// Total tokens (input + output) for this checkpoint.
    #[must_use]
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

/// All checkpoints on a branch, in chronological order.
#[derive(Debug, Clone, Default)]
pub struct BranchMetrics {
    pub checkpoints: Vec<CheckpointMetric>,
}

impl BranchMetrics {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    #[must_use]
    pub fn total_input_tokens(&self) -> u64 {
        self.checkpoints.iter().map(|c| c.input_tokens).sum()
    }

    #[must_use]
    pub fn total_output_tokens(&self) -> u64 {
        self.checkpoints.iter().map(|c| c.output_tokens).sum()
    }
}

/// Read and aggregate metrics from `branch` via `git`.
///
/// Missing per-file artifacts are treated as zero rather than fatal, so a
/// partially-populated branch still yields useful numbers. Returns an empty
/// [`BranchMetrics`] when the branch ref does not exist.
///
/// # Errors
/// Propagates [`GitError`] from listing the tree (a missing ref is not an error).
pub fn read_branch_metrics<G: GitReader>(
    git: &G,
    branch: &str,
) -> Result<BranchMetrics, MetricsError> {
    if !git.ref_exists(branch)? {
        return Ok(BranchMetrics::default());
    }

    // Group tree entries into checkpoint ids and their session indices.
    let mut session_indices: BTreeMap<String, BTreeSet<usize>> = BTreeMap::new();
    for path in git.list_tree(branch, "")? {
        let comps: Vec<&str> = path.split('/').collect();
        // Shard layout is <id[:2]>/<id[2:]>/<session>/<file>.
        if comps.len() < 3 || comps[0].len() != 2 {
            continue;
        }
        let id = format!("{}{}", comps[0], comps[1]);
        let entry = session_indices.entry(id).or_default();
        if let Ok(idx) = comps[2].parse::<usize>() {
            entry.insert(idx);
        }
    }

    let mut checkpoints = Vec::with_capacity(session_indices.len());
    for (id, sessions) in session_indices {
        let shard = format!("{}/{}", &id[..2], &id[2..]);

        let summary: CheckpointSummary =
            read_json(git, branch, &format!("{shard}/metadata.json")).unwrap_or_default();

        let mut metric = CheckpointMetric {
            id: id.clone(),
            created_at: summary.created_at.clone(),
            files_touched: summary.files_touched.len(),
            ..Default::default()
        };

        for idx in sessions {
            if let Some(tokens) = read_json::<TokenEstimates, _>(
                git,
                branch,
                &format!("{shard}/{idx}/token_estimates.json"),
            ) {
                metric.input_tokens += tokens.input_tokens;
                metric.output_tokens += tokens.output_tokens;
                metric.cached_input_tokens += tokens.cached_input_tokens;
            }
            if let Ok(bytes) = git.show_file(branch, &format!("{shard}/{idx}/tool_calls.jsonl")) {
                metric.tool_calls += jsonl::count_records(&bytes[..]).unwrap_or(0);
            }
        }

        checkpoints.push(metric);
    }

    // Chronological by created_at (ISO-8601 sorts lexically), then by id.
    checkpoints.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));

    Ok(BranchMetrics { checkpoints })
}

fn read_json<T: DeserializeOwned, G: GitReader>(git: &G, branch: &str, path: &str) -> Option<T> {
    let bytes = git.show_file(branch, path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct FakeGit {
        files: HashMap<String, Vec<u8>>,
    }

    impl FakeGit {
        fn new(files: &[(&str, &str)]) -> Self {
            Self {
                files: files
                    .iter()
                    .map(|(p, c)| ((*p).to_string(), c.as_bytes().to_vec()))
                    .collect(),
            }
        }
    }

    impl GitReader for FakeGit {
        fn ref_exists(&self, _refname: &str) -> Result<bool, GitError> {
            Ok(true)
        }
        fn show_file(&self, _treeish: &str, path: &str) -> Result<Vec<u8>, GitError> {
            self.files
                .get(path)
                .cloned()
                .ok_or_else(|| GitError::NonZeroExit {
                    args: format!("show {path}"),
                    code: 128,
                    stderr: "path not found".to_string(),
                })
        }
        fn list_tree(&self, _treeish: &str, _path: &str) -> Result<Vec<String>, GitError> {
            Ok(self.files.keys().cloned().collect())
        }
    }

    #[test]
    fn missing_ref_yields_empty() {
        struct NoBranch;
        impl GitReader for NoBranch {
            fn ref_exists(&self, _r: &str) -> Result<bool, GitError> {
                Ok(false)
            }
            fn show_file(&self, _t: &str, _p: &str) -> Result<Vec<u8>, GitError> {
                unreachable!()
            }
            fn list_tree(&self, _t: &str, _p: &str) -> Result<Vec<String>, GitError> {
                unreachable!()
            }
        }
        let m = read_branch_metrics(&NoBranch, "entire/checkpoints/v1").unwrap();
        assert!(m.is_empty());
    }

    #[test]
    fn aggregates_and_orders_checkpoints() {
        let git = FakeGit::new(&[
            // Newer checkpoint first in the tree listing, to prove sorting.
            (
                "ff/0011/metadata.json",
                r#"{"id":"ff0011","created_at":"2026-02-01T00:00:00Z","files_touched":["x"]}"#,
            ),
            (
                "ff/0011/0/token_estimates.json",
                r#"{"input_tokens":50,"output_tokens":5}"#,
            ),
            (
                "a3/b2c4d5/metadata.json",
                r#"{"id":"a3b2c4d5","created_at":"2026-01-01T00:00:00Z","files_touched":["a","b"]}"#,
            ),
            (
                "a3/b2c4d5/0/token_estimates.json",
                r#"{"input_tokens":100,"output_tokens":20,"cached_input_tokens":10}"#,
            ),
            ("a3/b2c4d5/0/tool_calls.jsonl", "{}\n{}\n\n{}\n"),
        ]);

        let m = read_branch_metrics(&git, "entire/checkpoints/v1").unwrap();

        assert_eq!(m.checkpoints.len(), 2);
        // Chronological: a3b2c4d5 (Jan) before ff0011 (Feb).
        assert_eq!(m.checkpoints[0].id, "a3b2c4d5");
        assert_eq!(m.checkpoints[1].id, "ff0011");

        let first = &m.checkpoints[0];
        assert_eq!(first.input_tokens, 100);
        assert_eq!(first.output_tokens, 20);
        assert_eq!(first.cached_input_tokens, 10);
        assert_eq!(first.files_touched, 2);
        assert_eq!(first.tool_calls, 3);
        assert_eq!(first.total_tokens(), 120);

        assert_eq!(m.total_input_tokens(), 150);
        assert_eq!(m.total_output_tokens(), 25);
    }
}
