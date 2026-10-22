//! A bounded, read-only tracked-file status probe for the dashboard.

use crate::RepoPaths;
use rift_core::Result;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Whether tracked files have staged or working changes. Untracked files are
/// deliberately outside this inexpensive probe; `None` means Git could not
/// answer within the deadline. No index locks, fsmonitor, external diff, or
/// text conversion is allowed to turn a dashboard refresh into other work.
///
/// # Errors
/// Returns a process I/O error if Git cannot be started or stopped.
pub fn tracked_changes(paths: &RepoPaths) -> Result<Option<bool>> {
    match quiet_diff(paths, &[])? {
        Some(true) => Ok(Some(true)),
        Some(false) => quiet_diff(paths, &["--cached"]),
        None => Ok(None),
    }
}

fn quiet_diff(paths: &RepoPaths, extra: &[&str]) -> Result<Option<bool>> {
    let mut command = Command::new("git");
    command.args([
        "--no-pager",
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "diff",
        "--quiet",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=dirty",
    ]);
    command.args(extra);
    let mut child = command
        .arg("--")
        .current_dir(&paths.work_tree)
        .env("GIT_DIR", crate::child_process_path(&paths.git_dir))
        .env("GIT_WORK_TREE", crate::child_process_path(&paths.work_tree))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(match status.code() {
                Some(0) => Some(false),
                Some(1) => Some(true),
                _ => None,
            });
        }
        if start.elapsed() >= Duration::from_secs(2) {
            child.kill()?;
            child.wait()?;
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
