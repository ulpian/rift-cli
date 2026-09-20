//! What the tests of several sibling modules share: the hook-file paths they
//! write to, and the diagnosis row they read back.

use std::path::{Path, PathBuf};

use crate::ClientFamily;

use super::hooks::client_hook_path;
use super::{diagnose_all_with_environment, DetectionEnvironment, NativeIntegrationStatus};

pub(super) fn codex_hook_path(repo: &Path) -> PathBuf {
    client_hook_path(repo, ClientFamily::Codex)
}

pub(super) fn claude_hook_path(repo: &Path) -> PathBuf {
    client_hook_path(repo, ClientFamily::ClaudeCode)
}

pub(super) fn antigravity_hook_path(repo: &Path) -> PathBuf {
    client_hook_path(repo, ClientFamily::Antigravity)
}

/// One adapter's row from a diagnosis over the default (empty) machine.
pub(super) fn row_for(repo: &Path, agent: &str) -> NativeIntegrationStatus {
    diagnose_all_with_environment(repo, &DetectionEnvironment::default())
        .into_iter()
        .find(|row| row.agent == agent)
        .unwrap()
}
