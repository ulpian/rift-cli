//! The installation lock `install.sh`, `install.ps1`, `rift update` and
//! `rift uninstall` share, and its recovery after a run that was killed.
//!
//! The lock records `PID EPOCH_SECONDS` of its holder: a directory holding an
//! `owner` file (the shell installer and this binary), or a file holding that
//! line directly (the PowerShell installer). A lock whose holder is gone, or
//! that is older than [`STALE_AFTER_SECS`] whoever holds it (a recycled PID
//! must not pin it forever), is reclaimed; anything else is refused with the
//! same guidance all three surfaces print.

use super::refusal;
use rift_core::{Result, RiftError};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub(super) const NAME: &str = ".rift-install.lock";
const OWNER: &str = "owner";
/// Fifteen minutes: every download is bounded at two minutes, so a run this
/// old that still holds the lock is not an installer making progress.
const STALE_AFTER_SECS: u64 = 15 * 60;

/// A held installation lock, released when dropped.
pub(super) struct InstallLock {
    path: PathBuf,
}

/// What decides whether an existing lock is abandoned, injected so a test
/// reads neither the wall clock nor the process table.
struct Probe<'a> {
    now: u64,
    pid: u32,
    alive: &'a dyn Fn(u32) -> bool,
}

/// What a lock held when it was judged: compared again after it is moved
/// aside, so a lock another run took in between is handed back untouched.
struct Seen {
    is_dir: bool,
    owner: String,
    written: Option<u64>,
}

impl InstallLock {
    /// Take the lock in `dir`, reclaiming one an interrupted run left behind.
    pub(super) fn acquire(dir: &Path) -> Result<Self> {
        let probe = Probe {
            now: unix_now(),
            pid: std::process::id(),
            alive: &process_alive,
        };
        acquire_with(dir, &probe)
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for InstallLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.path.join(OWNER));
        let _ = std::fs::remove_dir(&self.path);
    }
}

impl Seen {
    fn abandoned(&self, probe: &Probe<'_>) -> bool {
        let expired = |since: u64| probe.now.saturating_sub(since) > STALE_AFTER_SECS;
        match parse_owner(&self.owner) {
            Some((pid, started)) => !(probe.alive)(pid) || expired(started),
            // An owner line not yet written, or a lock from an older installer:
            // only its age can speak for it.
            None => self.written.is_some_and(expired),
        }
    }
}

fn acquire_with(dir: &Path, probe: &Probe<'_>) -> Result<InstallLock> {
    let path = dir.join(NAME);
    match create(&path, probe) {
        Ok(()) => return Ok(InstallLock { path }),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    // An unreadable lock is one a live PowerShell installer holds open.
    let seen = observe(&path).ok_or_else(|| busy(&path))?;
    if !seen.abandoned(probe) {
        return Err(busy(&path));
    }
    reclaim(&path, &seen, probe.pid)?;
    create(&path, probe).map_err(|_| busy(&path))?;
    tracing::warn!("reclaimed a stale installation lock left by an interrupted run");
    Ok(InstallLock { path })
}

fn create(path: &Path, probe: &Probe<'_>) -> std::io::Result<()> {
    std::fs::create_dir(path)?;
    let written = std::fs::write(path.join(OWNER), format!("{} {}\n", probe.pid, probe.now));
    if let Err(error) = written {
        let _ = std::fs::remove_dir(path);
        return Err(error);
    }
    Ok(())
}

fn observe(path: &Path) -> Option<Seen> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() {
        return None;
    }
    let owner_file = if meta.is_dir() {
        path.join(OWNER)
    } else {
        path.to_path_buf()
    };
    let owner = match std::fs::File::open(&owner_file) {
        Ok(file) => {
            let mut text = String::new();
            file.take(64).read_to_string(&mut text).ok()?;
            text
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return None,
    };
    let written = meta
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|age| age.as_secs());
    Some(Seen {
        is_dir: meta.is_dir(),
        owner,
        written,
    })
}

/// Move the abandoned lock aside, confirm it is still the one judged, and
/// remove it. Renaming first means two runs reclaiming at once cannot both
/// delete: the second finds nothing to move.
fn reclaim(path: &Path, seen: &Seen, pid: u32) -> Result<()> {
    let aside = path.with_file_name(format!("{NAME}.stale-{pid}"));
    std::fs::rename(path, &aside).map_err(|_| busy(path))?;
    let moved = observe(&aside);
    let same = moved
        .as_ref()
        .is_some_and(|moved| moved.is_dir == seen.is_dir && moved.owner == seen.owner);
    if !same {
        if !path.exists() {
            let _ = std::fs::rename(&aside, path);
        }
        return Err(busy(path));
    }
    if seen.is_dir {
        std::fs::remove_dir_all(&aside)?;
    } else {
        std::fs::remove_file(&aside)?;
    }
    Ok(())
}

fn parse_owner(text: &str) -> Option<(u32, u64)> {
    let (pid, started) = text.trim().split_once(' ')?;
    Some((pid.parse().ok()?, started.parse().ok()?))
}

fn busy(path: &Path) -> RiftError {
    refusal(&format!(
        "Another Rift install, update or uninstall is running. Wait for it to finish; \
         if none is, remove {} and try again (a lock left by a closed terminal is \
         reclaimed after 15 minutes).",
        path.display()
    ))
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |age| age.as_secs())
}

/// Whether `pid` is running. A probe that cannot run says "alive", leaving
/// the lock to the age bound rather than reclaiming a live run's lock.
#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_or(true, |status| status.success())
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    let filter = format!("PID eq {pid}");
    std::process::Command::new("tasklist")
        .args(["/FI", &filter, "/FO", "CSV", "/NH"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_or(true, |output| {
            String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\""))
        })
}

#[cfg(not(any(unix, windows)))]
fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000;

    fn probe(alive: &dyn Fn(u32) -> bool) -> Probe<'_> {
        Probe {
            now: NOW,
            pid: 4242,
            alive,
        }
    }

    fn plant(dir: &Path, owner: &str) -> PathBuf {
        let lock = dir.join(NAME);
        std::fs::create_dir(&lock).unwrap();
        std::fs::write(lock.join(OWNER), owner).unwrap();
        lock
    }

    #[test]
    fn a_lock_whose_holder_died_is_reclaimed_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let lock = plant(dir.path(), &format!("77 {}\n", NOW - 5));
        let held = acquire_with(dir.path(), &probe(&|_| false)).unwrap();
        assert_eq!(
            std::fs::read_to_string(lock.join(OWNER)).unwrap(),
            format!("4242 {NOW}\n")
        );
        drop(held);
        assert!(!lock.exists());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_recent_lock_with_a_live_holder_is_refused_and_left_intact() {
        let dir = tempfile::tempdir().unwrap();
        let owner = format!("77 {}\n", NOW - 60);
        let lock = plant(dir.path(), &owner);
        let error = acquire_with(dir.path(), &probe(&|_| true))
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("Another Rift install"), "{error}");
        assert!(error.contains(NAME), "{error}");
        assert_eq!(std::fs::read_to_string(lock.join(OWNER)).unwrap(), owner);
    }

    #[test]
    fn a_lock_older_than_the_bound_is_reclaimed_even_if_its_pid_was_recycled() {
        let dir = tempfile::tempdir().unwrap();
        plant(dir.path(), &format!("77 {}\n", NOW - STALE_AFTER_SECS - 1));
        assert!(acquire_with(dir.path(), &probe(&|_| true)).is_ok());
    }

    #[test]
    fn a_lock_without_an_owner_line_is_judged_by_its_age_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(NAME)).unwrap();
        let young = Probe {
            now: 0,
            pid: 1,
            alive: &|_| false,
        };
        assert!(acquire_with(dir.path(), &young).is_err());
        let old = Probe {
            now: u64::MAX,
            pid: 1,
            alive: &|_| true,
        };
        assert!(acquire_with(dir.path(), &old).is_ok());
    }

    #[test]
    fn a_powershell_file_lock_with_a_dead_holder_is_reclaimed_as_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join(NAME);
        std::fs::write(&lock, format!("77 {NOW}\n")).unwrap();
        let held = acquire_with(dir.path(), &probe(&|pid| pid != 77)).unwrap();
        assert!(held.path().is_dir());
    }

    #[test]
    fn an_owner_line_parses_only_as_pid_then_seconds() {
        assert_eq!(parse_owner("12 34\n"), Some((12, 34)));
        assert_eq!(parse_owner("12"), None);
        assert_eq!(parse_owner("x 34"), None);
        assert_eq!(parse_owner(""), None);
    }
}
