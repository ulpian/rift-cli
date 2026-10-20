//! The old-format capture-queue lock. An older Rift wrote a timestamp and
//! no pid into `rift-capture-queue/.lock`; every current capture treats that
//! file as a live owner forever, so a leftover one blocks recovery, the
//! run-state move and condensing. Report-only, this leg names the file and
//! its age and writes nothing. Under `--fix` it moves the file aside only
//! when its writer is provably gone: this process holds the current owner
//! lock, and no Rift process that started before the file was last written
//! is still running. The file is renamed, never deleted, so renaming it back
//! restores it; an unprovable answer leaves it where it is.
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::json;

/// What the fix did with the marker.
#[derive(Debug, PartialEq, Eq)]
enum Retirement {
    Absent,
    Retired(PathBuf),
    Kept(String),
}

/// The leg as the doctor prints it; it runs before recovery so that a
/// retired marker no longer blocks the rest of the same `--fix`.
pub(super) fn doctor_legacy_queue_lock(root: &Path, repair: bool) -> (String, serde_json::Value) {
    let Ok(repo) = rift_git::discover(root) else {
        return (String::new(), json!({ "present": false }));
    };
    if !repair {
        let (mut line, mut value) = report(&repo.git_dir);
        let request = &crate::capture_queue::request::stale_line;
        if let Some(stale) = request(&repo.git_dir, SystemTime::now(), rift_process_ages) {
            line.push_str(&stale);
            if let Some(fields) = value.as_object_mut() {
                fields.insert("stale_recovery_request".to_owned(), json!(true));
            }
        }
        return (line, value);
    }
    match retire(&repo.git_dir, SystemTime::now(), rift_process_ages) {
        Retirement::Absent => (String::new(), json!({ "present": false })),
        Retirement::Retired(to) => (
            format!(
                "\n  legacy queue lock: moved aside to {}; no Rift process older than it was \
                 running (rename it back to restore it)",
                to.display()
            ),
            json!({ "present": false, "retired_to": to.display().to_string() }),
        ),
        Retirement::Kept(why) => (
            format!("\n  legacy queue lock: kept, because {why}; capture stays blocked"),
            json!({ "present": true, "kept": why }),
        ),
    }
}

/// Names the marker without opening a lock or touching a file.
fn report(git_dir: &Path) -> (String, serde_json::Value) {
    let marker = crate::capture_queue::legacy_queue_lock(git_dir);
    let Ok(meta) = marker.symlink_metadata() else {
        return (String::new(), json!({ "present": false }));
    };
    let written = meta.modified().ok().and_then(utc_stamp);
    let when = written.as_ref().map_or_else(
        || "an unknown time".to_owned(),
        |(d, t)| format!("{d} {t} UTC"),
    );
    (
        format!(
            "\n  legacy queue lock: {} (last written {when}, no pid recorded) blocks capture; \
             `rift doctor --fix` moves it aside once no Rift process older than it runs",
            marker.display()
        ),
        json!({ "present": true, "path": marker.display().to_string(), "written": when }),
    )
}

/// The retirement itself. `ages` lists how long each running Rift process
/// has run; `None` means the list could not be read, which proves nothing.
fn retire(
    git_dir: &Path,
    now: SystemTime,
    ages: impl FnOnce() -> Option<Vec<Duration>>,
) -> Retirement {
    let marker = crate::capture_queue::legacy_queue_lock(git_dir);
    if marker.symlink_metadata().is_err() {
        return Retirement::Absent;
    }
    // Held to the end of this function, so no current capture starts while
    // the marker is judged and moved.
    let Ok(owner) = crate::capture_queue::lock_owner(git_dir) else {
        return Retirement::Kept("another process holds the capture owner lock".to_owned());
    };
    let marker = crate::capture_queue::legacy_queue_lock(owner.git_dir());
    let Some(written) = marker.symlink_metadata().and_then(|m| m.modified()).ok() else {
        return Retirement::Kept("its last-written time could not be read".to_owned());
    };
    let Ok(marker_age) = now.duration_since(written) else {
        return Retirement::Kept("it was written after this run began".to_owned());
    };
    let Some(ages) = ages() else {
        return Retirement::Kept("running process start times could not be read".to_owned());
    };
    // A process that started in the same second as the write may be its
    // writer, so an equal age counts as older.
    if ages.iter().any(|age| age.as_secs() >= marker_age.as_secs()) {
        return Retirement::Kept("a Rift process older than it is still running".to_owned());
    }
    let Some((date, _)) = utc_stamp(written) else {
        return Retirement::Kept("its last-written time could not be read".to_owned());
    };
    let aside = marker.with_file_name(format!(".lock.stale-{date}"));
    if aside.symlink_metadata().is_ok() {
        return Retirement::Kept(format!("{} already exists", aside.display()));
    }
    match std::fs::rename(&marker, &aside) {
        Ok(()) => Retirement::Retired(aside),
        Err(_) => Retirement::Kept("it could not be renamed".to_owned()),
    }
}

/// How long each running Rift process has run, read from `ps`. Elapsed
/// times are compared against the marker's age, so neither side needs a
/// time zone. Any process whose name starts with `rift` counts, which can
/// only make the answer more cautious.
#[cfg(unix)]
fn rift_process_ages() -> Option<Vec<Duration>> {
    let out = std::process::Command::new("ps")
        .args(["-A", "-o", "etime=,comm="])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let mut ages = Vec::new();
    for line in text.lines() {
        let line = line.trim_start();
        let Some((elapsed, command)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        let name = Path::new(command.trim()).file_name()?.to_string_lossy();
        if name.starts_with("rift") {
            ages.push(parse_elapsed(elapsed)?);
        }
    }
    Some(ages)
}

/// No process list is read off unix, so the writer is never provably gone.
#[cfg(not(unix))]
fn rift_process_ages() -> Option<Vec<Duration>> {
    None
}

/// `ps` elapsed time: `[[dd-]hh:]mm:ss`.
fn parse_elapsed(text: &str) -> Option<Duration> {
    let (days, clock) = match text.split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, text),
    };
    let mut secs = 0u64;
    for part in clock.split(':') {
        secs = secs
            .checked_mul(60)?
            .checked_add(part.parse::<u64>().ok()?)?;
    }
    days.checked_mul(86_400)?
        .checked_add(secs)
        .map(Duration::from_secs)
}

/// The UTC calendar date and clock time of `at`, from the civil-from-days
/// conversion, so no date crate is needed for one file name.
fn utc_stamp(at: SystemTime) -> Option<(String, String)> {
    let secs = at.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let days = i64::try_from(secs / 86_400).ok()?;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    Some((
        format!("{year:04}-{month:02}-{day:02}"),
        format!("{:02}:{:02}:{:02}", rem / 3_600, rem % 3_600 / 60, rem % 60),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A git dir holding an old-format marker last written an hour before
    /// `now`. The wall clock only seeds the fixture.
    fn fixture() -> (tempfile::TempDir, PathBuf, SystemTime) {
        let dir = tempfile::tempdir().unwrap();
        let marker = crate::capture_queue::legacy_queue_lock(dir.path());
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"1760722620000").unwrap();
        let now = SystemTime::now();
        let file = std::fs::File::options().write(true).open(&marker).unwrap();
        file.set_modified(now - Duration::from_hours(1)).unwrap();
        (dir, marker, now)
    }

    fn stale_name(marker: &Path, now: SystemTime) -> PathBuf {
        let (date, _) = utc_stamp(now - Duration::from_hours(1)).unwrap();
        marker.with_file_name(format!(".lock.stale-{date}"))
    }

    #[test]
    fn doctor_fix_moves_a_stale_marker_aside_and_the_queue_opens() {
        let (dir, marker, now) = fixture();
        assert!(crate::capture_queue::acquire_owner(dir.path()).is_err());
        let canonical = std::fs::canonicalize(&marker).unwrap();
        let done = retire(dir.path(), now, || Some(vec![Duration::from_secs(5)]));
        let aside = stale_name(&canonical, now);
        assert_eq!(done, Retirement::Retired(aside.clone()));
        assert!(!marker.exists());
        assert_eq!(std::fs::read(&aside).unwrap(), b"1760722620000");
        assert!(crate::capture_queue::acquire_owner(dir.path()).is_ok());
    }

    #[test]
    fn doctor_fix_keeps_a_marker_older_than_a_running_rift_process() {
        let (dir, marker, now) = fixture();
        let done = retire(dir.path(), now, || Some(vec![Duration::from_hours(2)]));
        assert!(
            matches!(done, Retirement::Kept(ref why) if why.contains("older")),
            "{done:?}"
        );
        assert!(marker.exists());
    }

    #[test]
    fn doctor_fix_keeps_a_marker_when_process_start_times_are_unreadable() {
        let (dir, marker, now) = fixture();
        assert!(matches!(
            retire(dir.path(), now, || None),
            Retirement::Kept(_)
        ));
        assert!(marker.exists());
    }

    #[test]
    fn doctor_fix_never_overwrites_an_existing_stale_name() {
        let (dir, marker, now) = fixture();
        let aside = stale_name(&marker, now);
        std::fs::write(&aside, b"earlier").unwrap();
        let done = retire(dir.path(), now, || Some(Vec::new()));
        assert!(matches!(done, Retirement::Kept(ref why) if why.contains("already exists")));
        assert!(marker.exists());
        assert_eq!(std::fs::read(&aside).unwrap(), b"earlier");
    }

    #[test]
    fn doctor_report_names_the_marker_and_changes_no_file() {
        let (dir, marker, _) = fixture();
        let before = std::fs::read_dir(marker.parent().unwrap()).unwrap().count();
        let (human, value) = report(dir.path());
        assert!(human.contains("no pid recorded"), "{human}");
        assert!(human.contains(&marker.display().to_string()), "{human}");
        assert_eq!(value["present"], true);
        assert!(marker.exists());
        assert_eq!(
            std::fs::read_dir(marker.parent().unwrap()).unwrap().count(),
            before
        );
        assert!(!dir.path().join(".rift-capture-owner.lock").exists());
    }

    #[test]
    fn doctor_reads_ps_elapsed_times_and_utc_dates() {
        assert_eq!(parse_elapsed("05:07"), Some(Duration::from_secs(307)));
        assert_eq!(
            parse_elapsed("1-02:03:04"),
            Some(Duration::from_secs(93_784))
        );
        assert_eq!(parse_elapsed("x"), None);
        let at = UNIX_EPOCH + Duration::from_mins(29_345_377);
        assert_eq!(
            utc_stamp(at),
            Some(("2025-10-17".to_owned(), "17:37:00".to_owned()))
        );
    }
}
