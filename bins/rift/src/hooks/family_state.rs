//! The family's own run-state directory inside the git common dir.
//!
//! Neutral ground for every hook-side store that belongs to the repository
//! family rather than to one checkout: a subagent in a temporary worktree
//! fires its hooks with that worktree's paths, and state keyed by the member's
//! git dir would land where the parent session's archive never looks. The
//! common dir is the one every member shares.

use std::path::{Path, PathBuf};

/// The directory's name inside the common dir.
const FAMILY_STATE_DIR: &str = "rift-family";

/// The run-state directory of the family whose git common dir is
/// `common_dir`. Nothing is created here; each store creates its own
/// subdirectory under it, with the mode its contents need.
pub(in crate::hooks) fn family_state_dir(common_dir: &Path) -> PathBuf {
    common_dir.join(FAMILY_STATE_DIR)
}
