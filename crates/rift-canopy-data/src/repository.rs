//! Small worktree-local Git summary. No capture or provisioning side effects.

use std::path::Path;

pub(crate) fn summary(path: &Path) -> Option<rift_canopy::col2::GitSummary> {
    let paths = rift_git::discover(path).ok()?;
    let head = std::fs::read_to_string(paths.git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    let branch = head.strip_prefix("ref: refs/heads/").map_or_else(
        || format!("detached {}", head.chars().take(12).collect::<String>()),
        str::to_owned,
    );
    Some(rift_canopy::col2::GitSummary {
        branch,
        tracked_changes: rift_git::tracked_changes(&paths).ok().flatten(),
    })
}
