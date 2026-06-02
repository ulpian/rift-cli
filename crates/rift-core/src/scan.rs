//! Source scanning: detect whether `entire` is set up in a repo.
//!
//! Two entry points:
//! - [`scan`] / [`scan_with`] inspect a single repo root and report the markers
//!   that indicate an `entire` setup.
//! - [`find_entire_repos`] walks a directory tree (bounded depth) to surface
//!   every nested repo that already has `.entire/settings.json` — useful when
//!   pointing Rift at a folder full of projects.

use std::path::{Path, PathBuf};

use serde::Serialize;
use walkdir::WalkDir;

use crate::git::{GitReader, ShellGitReader};
use crate::model::Settings;
use crate::paths::{CHECKPOINT_BRANCH, SESSIONS_DIR, SETTINGS_FILE};

/// What a scan of a single repo root found.
#[derive(Debug, Clone, Serialize)]
pub struct ScanReport {
    pub root: PathBuf,
    pub is_git_repo: bool,
    pub settings_present: bool,
    pub sessions_dir_present: bool,
    pub checkpoint_branch_present: bool,
    pub enabled_agents: Vec<String>,
}

impl ScanReport {
    /// Whether the repo looks like it has `entire` enabled at all.
    #[must_use]
    pub fn is_entire_enabled(&self) -> bool {
        self.settings_present || self.checkpoint_branch_present
    }
}

/// Scan a repo root using the system `git` binary for the branch check.
#[must_use]
pub fn scan(root: impl AsRef<Path>) -> ScanReport {
    let root = root.as_ref();
    let git = ShellGitReader::new(root);
    scan_with(root, &git)
}

/// Scan a repo root with an injectable [`GitReader`] (for tests).
pub fn scan_with<G: GitReader>(root: impl AsRef<Path>, git: &G) -> ScanReport {
    let root = root.as_ref();
    let is_git_repo = root.join(".git").exists();
    let settings_path = root.join(SETTINGS_FILE);
    let settings_present = settings_path.is_file();
    let sessions_dir_present = root.join(".git").join(SESSIONS_DIR).is_dir();
    let checkpoint_branch_present =
        is_git_repo && git.ref_exists(CHECKPOINT_BRANCH).unwrap_or(false);

    let enabled_agents = std::fs::read(&settings_path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Settings>(&bytes).ok())
        .map(|s| s.agents)
        .unwrap_or_default();

    ScanReport {
        root: root.to_path_buf(),
        is_git_repo,
        settings_present,
        sessions_dir_present,
        checkpoint_branch_present,
        enabled_agents,
    }
}

/// Walk `root` (up to `max_depth` directories deep) and return every repo that
/// contains a `.entire/settings.json`.
#[must_use]
pub fn find_entire_repos(root: impl AsRef<Path>, max_depth: usize) -> Vec<PathBuf> {
    WalkDir::new(root)
        .max_depth(max_depth)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_name() == "settings.json")
        .filter(|e| e.path().parent().is_some_and(|p| p.ends_with(".entire")))
        .filter_map(|e| {
            // repo root is the parent of the `.entire` directory
            e.path()
                .parent()
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::GitError;

    struct FakeGit {
        has_branch: bool,
    }
    impl GitReader for FakeGit {
        fn ref_exists(&self, _refname: &str) -> Result<bool, GitError> {
            Ok(self.has_branch)
        }
        fn show_file(&self, _t: &str, _p: &str) -> Result<Vec<u8>, GitError> {
            Ok(Vec::new())
        }
        fn list_tree(&self, _t: &str, _p: &str) -> Result<Vec<String>, GitError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn detects_settings_and_agents() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".entire")).unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(
            dir.path().join(SETTINGS_FILE),
            r#"{"agents":["claude-code","codex"]}"#,
        )
        .unwrap();

        let report = scan_with(dir.path(), &FakeGit { has_branch: true });
        assert!(report.is_entire_enabled());
        assert!(report.settings_present);
        assert!(report.checkpoint_branch_present);
        assert_eq!(report.enabled_agents, vec!["claude-code", "codex"]);
    }

    #[test]
    fn empty_dir_is_not_enabled() {
        let dir = tempfile::tempdir().unwrap();
        let report = scan_with(dir.path(), &FakeGit { has_branch: false });
        assert!(!report.is_entire_enabled());
        assert!(!report.is_git_repo);
    }

    #[test]
    fn finds_nested_repos() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("projects").join("svc");
        std::fs::create_dir_all(repo.join(".entire")).unwrap();
        std::fs::write(repo.join(SETTINGS_FILE), "{}").unwrap();

        let found = find_entire_repos(dir.path(), 6);
        assert_eq!(found, vec![repo]);
    }
}
