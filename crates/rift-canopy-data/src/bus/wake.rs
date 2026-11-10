//! The wake-signal seam: how the worker learns that something under the cache
//! root changed without polling for it.
//!
//! A separate module because the platform watcher is the one piece of the bus
//! that owns an OS resource, and every test of the loop injects a stand-in for
//! it — the trait is the seam, the `notify` implementation is one plug.

use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use notify::Watcher as _;

use super::WorkerMsg;

/// The wake-signal seam: implementations watch the cache root and deliver
/// [`WorkerMsg::Wake`]s for as long as they live. Failing to watch is not an
/// error path — the bus degrades to `PollOnly` (the adaptive cadence keeps
/// updating the dashboard; wakes only add promptness).
pub(crate) trait WakeWatcher: Send {
    /// Begin watching `root` recursively, sending wakes into `wake`.
    ///
    /// # Errors
    /// A human-readable reason when the platform watcher cannot start; the
    /// caller degrades to poll-only.
    fn watch(&mut self, root: &Path, wake: Sender<WorkerMsg>) -> Result<(), String>;

    /// Point the second, **narrow** watch at the ACTIVE project's family's
    /// session-cache dirs (each member's sessions directory, the rebuild-epoch rule
    /// amendment as the family view widens it) — the one class of change the
    /// cache-root watch cannot see. An empty slice unwatches everything.
    ///
    /// A **set**, because session caches stay per gitdir (family identity) and the
    /// active project's family may be running agents in any of its members;
    /// watching only the vantage's own dir made a member's session prompt
    /// nowhere. Bounded by member count and re-enumerated only when the active
    /// project changes or its projection is rebuilt — never one stream per
    /// tracked project, which is the macOS FSEvents caution this trait was
    /// written around.
    ///
    /// Best-effort by the same contract as [`Self::watch`]: a failure (commonly
    /// a dir not existing before the first hook event) leaves session
    /// promptness to the poll cadence, and the worker re-calls every cycle so
    /// attach retries for free. Default: poll-only watchers ignore it.
    fn set_active_session_dirs(&mut self, _dirs: &[PathBuf]) {}
}

/// Watcher implementation for the reference poll-only fallback.
pub(super) struct PollOnlyWatcher;

impl WakeWatcher for PollOnlyWatcher {
    fn watch(&mut self, _root: &Path, _wake: Sender<WorkerMsg>) -> Result<(), String> {
        Err("poll-only".to_string())
    }
}

/// The production watcher: `notify`'s recommended platform backend.
#[derive(Default)]
pub(super) struct NotifyWatcher {
    /// Kept alive for the worker's lifetime; dropping it unwatches.
    inner: Option<notify::RecommendedWatcher>,
    /// The session dirs currently attached — exactly those the platform
    /// watcher accepted, so a dir that did not exist yet is retried next cycle.
    session_dirs: Vec<PathBuf>,
}

impl WakeWatcher for NotifyWatcher {
    fn watch(&mut self, root: &Path, wake: Sender<WorkerMsg>) -> Result<(), String> {
        let mut watcher = notify::recommended_watcher(move |_event| {
            // Payload deliberately ignored — wake-only (the rebuild-epoch rule).
            let _ = wake.send(WorkerMsg::Wake);
        })
        .map_err(|e| e.to_string())?;
        // Only Rift's own subtree: the cache root is the user's whole
        // `~/.cache` or `%LOCALAPPDATA%`, and watching it recursively woke the
        // worker for every browser and build cache write, and on Linux spent
        // an inotify watch on each of its directories.
        let ours = root.join("rift");
        std::fs::create_dir_all(&ours).map_err(|e| e.to_string())?;
        watcher
            .watch(&ours, notify::RecursiveMode::Recursive)
            .map_err(|e| e.to_string())?;
        self.inner = Some(watcher);
        Ok(())
    }

    fn set_active_session_dirs(&mut self, dirs: &[PathBuf]) {
        if self.session_dirs == dirs {
            return; // already attached to exactly this set (or both empty)
        }
        let Some(w) = self.inner.as_mut() else {
            return; // no platform watcher at all → PollOnly for sessions too
        };
        // Diffed, not torn down and rebuilt: a member that is still in the set
        // keeps one uninterrupted stream when a sibling is added or removed.
        for old in &self.session_dirs {
            if !dirs.contains(old) {
                let _ = w.unwatch(old);
            }
        }
        let mut attached = Vec::with_capacity(dirs.len());
        for d in dirs {
            if self.session_dirs.contains(d) {
                attached.push(d.clone()); // already watched; leave it alone
                continue;
            }
            // Non-recursive: rift-sessions is a flat dir of small JSON caches.
            // Failure (dir not created yet) leaves it out of `session_dirs`, so
            // the worker's next call retries the attach.
            if w.watch(d, notify::RecursiveMode::NonRecursive).is_ok() {
                attached.push(d.clone());
            }
        }
        self.session_dirs = attached;
    }
}
