//! The recovery request. On a machine in use some capture worker is nearly
//! always draining, so `rift doctor --fix` would never find the owner lock
//! free. Instead it leaves a request marker beside the owner lock and waits a
//! bounded time: a draining worker reads the marker between passes and lets
//! go at the next pass boundary, and a hook that would start a new drain only
//! enqueues while the marker is fresh. Every check is one `stat`, so no hook
//! opens a store or reads a file to decide.
//!
//! A marker is fresh for a fixed time after it was written. Its writer
//! removes it on every exit and never waits longer than [`RECOVERY_WAIT`],
//! so an older marker was left by a writer that died, and it is ignored.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::{OwnerBlock, QueueLock};

/// The marker's name, in the same directory as the owner lock.
pub(crate) const REQUEST_NAME: &str = ".rift-capture-recovery.request";

/// How long `rift doctor --fix` waits for a draining worker to let go.
pub(crate) const RECOVERY_WAIT: Duration = Duration::from_mins(2);

/// The sleep between two tries for the owner lock while waiting.
pub(crate) const RECOVERY_WAIT_PAUSE: Duration = Duration::from_millis(500);

/// After this a marker is ignored: the wait plus room for the checks the
/// writer runs before it starts waiting.
pub(crate) const STALE_AFTER: Duration = Duration::from_mins(3);

/// A placed request; dropping it removes the marker if this process wrote it.
pub(crate) struct Request {
    path: PathBuf,
    owned: bool,
}

impl Request {
    /// Write the marker without replacing a fresh one. A fresh marker from
    /// another `--fix` already asks the workers to yield, so this run waits
    /// under it and leaves it for its writer to remove. A stale one is
    /// replaced, since its writer is gone.
    pub(crate) fn place(git_dir: &Path, now: SystemTime) -> Request {
        let path = request_path(git_dir);
        let owned = match write_new(&path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                !requested(git_dir, now)
                    && std::fs::remove_file(&path).is_ok()
                    && write_new(&path).is_ok()
            }
            Err(_) => false,
        };
        Request { path, owned }
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        if self.owned {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

pub(crate) fn request_path(git_dir: &Path) -> PathBuf {
    git_dir.join(REQUEST_NAME)
}

/// Whether a fresh request stands: one `stat`, no read, no lock.
pub(crate) fn requested(git_dir: &Path, now: SystemTime) -> bool {
    let Ok(written) = request_path(git_dir)
        .symlink_metadata()
        .and_then(|meta| meta.modified())
    else {
        return false;
    };
    // A marker stamped a little ahead of this clock was just written.
    !matches!(now.duration_since(written), Ok(age) if age > STALE_AFTER)
}

/// Try for the owner lock until it is free or `wait` has been spent, sleeping
/// `pause` between tries. Only a live owner is waited on: a legacy barrier or
/// an unreadable lock will not clear by waiting.
pub(crate) fn acquire_owner_within(
    git_dir: &Path,
    wait: Duration,
    pause: Duration,
) -> Result<QueueLock, OwnerBlock> {
    let tries = wait.as_millis() / pause.as_millis().max(1);
    let mut attempts = 0;
    loop {
        match super::acquire_owner(git_dir) {
            Err(OwnerBlock::ActiveOwner) if attempts < tries => {
                attempts += 1;
                std::thread::sleep(pause);
            }
            other => return other,
        }
    }
}

/// The report-only doctor line for a marker whose writer is gone: older
/// than [`STALE_AFTER`], or with no Rift process running that is as old as
/// it. `ages` lists how long each running Rift process has run.
pub(crate) fn stale_line(
    git_dir: &Path,
    now: SystemTime,
    ages: impl FnOnce() -> Option<Vec<Duration>>,
) -> Option<String> {
    let path = request_path(git_dir);
    let written = path.symlink_metadata().and_then(|m| m.modified()).ok()?;
    let age = now.duration_since(written).ok()?;
    let writer_gone = age > STALE_AFTER
        || ages().is_some_and(|ages| ages.iter().all(|a| a.as_secs() < age.as_secs()));
    if !writer_gone {
        return None;
    }
    Some(format!(
        "\n  recovery request: {} was written {} s ago and its writer is gone; capture \
         ignores it after {} s, and the next `rift doctor --fix` replaces it",
        path.display(),
        age.as_secs(),
        STALE_AFTER.as_secs()
    ))
}

fn write_new(path: &Path) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(std::process::id().to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A marker last written `age` before the returned `now`. The wall clock
    /// only seeds the fixture.
    fn marker_aged(git_dir: &Path, age: Duration) -> SystemTime {
        std::fs::write(request_path(git_dir), b"1").unwrap();
        let now = SystemTime::now();
        let file = std::fs::File::options()
            .write(true)
            .open(request_path(git_dir))
            .unwrap();
        file.set_modified(now - age).unwrap();
        now
    }

    #[test]
    fn recovery_request_is_written_without_clobbering_and_removed_when_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let now = SystemTime::now();
        let first = Request::place(dir.path(), now);
        assert!(requested(dir.path(), now));
        let second = Request::place(dir.path(), now);
        drop(second);
        assert!(
            requested(dir.path(), now),
            "a second request leaves the first"
        );
        drop(first);
        assert!(!request_path(dir.path()).exists());
    }

    #[test]
    fn recovery_request_older_than_its_bound_is_ignored_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let now = marker_aged(dir.path(), STALE_AFTER + Duration::from_secs(1));
        assert!(!requested(dir.path(), now));
        let placed = Request::place(dir.path(), now);
        assert!(requested(dir.path(), now), "the stale marker was replaced");
        drop(placed);
        assert!(!request_path(dir.path()).exists());
    }

    #[test]
    fn recovery_request_doctor_names_a_marker_whose_writer_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let now = marker_aged(dir.path(), Duration::from_mins(1));
        let live = || Some(vec![Duration::from_secs(61)]);
        assert_eq!(stale_line(dir.path(), now, live), None);
        let gone = stale_line(dir.path(), now, || Some(vec![Duration::from_secs(5)])).unwrap();
        assert!(gone.contains("its writer is gone"), "{gone}");
        assert_eq!(stale_line(dir.path(), now, || None), None);
    }

    #[test]
    fn recovery_wait_gives_up_on_a_live_owner_and_takes_a_free_one() {
        let dir = tempfile::tempdir().unwrap();
        let held = super::super::acquire_owner(dir.path()).unwrap();
        let pause = Duration::from_millis(1);
        let waited = acquire_owner_within(dir.path(), Duration::from_millis(3), pause);
        assert_eq!(waited.err(), Some(OwnerBlock::ActiveOwner));
        drop(held);
        assert!(acquire_owner_within(dir.path(), Duration::ZERO, pause).is_ok());
    }
}
