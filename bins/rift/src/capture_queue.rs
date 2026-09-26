//! The post-commit capture queue: the durable record of *intent* that
//! lets `post-commit` return inside the hook budget while condensation runs in
//! a detached child.
//!
//! The hook budget is git latency — the synchronous part of a commit's hooks
//! must return within 100 ms warm. Condense + archive spawns nine `git` child
//! processes and measured 103–204 ms on its own, so it does not fit and cannot
//! be made to fit; it is queued instead.
//!
//! **An entry carries no capture bytes of either kind.** Not raw (`SecretBytes`
//! must never persist) and not redacted: everything the worker needs is
//! re-derivable from the commit oid and the session cache, so the entry holds
//! identifiers and a timestamp. That is not only the safer shape, it is the only
//! available one — `RedactedBytes` has a single constructor
//! (`RedactedBytes::seal`, one call site, CI-enforced), so bytes written to a
//! file could not be read back into one without a second constructor, which the
//! redaction invariant forbids.
//!
//! Durability posture, stated because it is a trade taken on purpose: the
//! capture *input* is never lost (the commit is in git, the session cache is on
//! disk), but condensation can be *deferred* by a crash or a killed worker until
//! the next commit's sweep or `rift doctor` drains the queue.

use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

/// The queue directory under `<git_dir>` (sibling of `rift-sessions`).
const QUEUE_DIR: &str = "rift-capture-queue";
/// The worker's mutual-exclusion file inside the queue directory.
const LOCK_FILE: &str = ".lock";
/// How long a lock may live before a later worker steals it. A worker killed
/// mid-drain must not wedge the queue forever; the work is idempotent, so the
/// cost of stealing too early is duplicated effort, never a lost or double
/// checkpoint. The age is the session cache's stale-writer age, read from the
/// crate below this one, so the lock and the cache's temp sweep keep one
/// number with one meaning.
const LOCK_STALE_MS: i64 = rift_shadow::hooks::STALE_WRITER_MS;

/// One queued commit: what the worker needs to condense it later.
///
/// Unknown fields are deliberately **ignored** rather than rejected: a newer
/// writer's entry must still be drainable by an older worker, and the fields
/// here are all that a condense needs.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct QueueEntry {
    /// Contract schema version.
    pub schema_version: u32,
    /// The anchoring commit (HEAD when the hook ran). Also the file name.
    pub commit: String,
    /// The working tree whose facts this commit is keyed by.
    pub work_tree: PathBuf,
    /// The live session to condense for, or `None` (diff facts only).
    pub session_id: Option<String>,
    /// The live session's agent.
    pub agent: Option<String>,
    /// The live session's phase token at queue time.
    pub session_state: Option<String>,
    /// Transaction time. The worker uses it as the archive decision's clock, so
    /// a deferred run neither backdates nor advances the decision.
    pub queued_at_ms: i64,
    /// How many times a worker has already failed on this entry. Absent in a
    /// v1 writer's entry, which reads as zero.
    #[serde(default)]
    pub attempts: u32,
}

/// How many times one entry may fail before it is dropped with its reason.
///
/// A retry is worth having — an archive can lose a race for `index.lock` and
/// succeed a second later. An *unbounded* retry is not: measured on a repo
/// whose route needs a backend that fails every time, the queue grew one entry
/// per commit and every commit re-attempted all of them, so the deferred work
/// went quadratic in a repository that was never going to succeed. The C-diff
/// facts are already durable when the archive fails (`archive_and_capture_with`
/// persists them before it surfaces the error), so what a dropped entry costs
/// is the checkpoint, and what it buys is a queue that stays bounded.
pub(crate) const MAX_ATTEMPTS: u32 = 3;

/// The queue directory for `git_dir`.
pub(crate) fn queue_dir(git_dir: &Path) -> PathBuf {
    git_dir.join(QUEUE_DIR)
}

/// Whether `oid` is a full **lowercase**-hex commit id.
///
/// The entry's file name **is** the oid, so this is the one place an oid becomes
/// a path component — the same discipline the session cache applies to session
/// ids, and for the same reason. Lowercase is required rather than merely
/// expected: git writes oids lowercase, and on a case-insensitive filesystem
/// (the macOS default) an uppercase spelling of the same commit would name the
/// same file while comparing unequal to it — one commit, two identities, one of
/// which silently overwrites the other.
fn is_commit_oid(oid: &str) -> bool {
    oid.len() == 40
        && oid
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() && c.is_ascii_hexdigit())
}

/// Append one entry (atomically: temp file + rename, owner-only).
///
/// # Errors
/// [`RiftError::Validity`] if `commit` is not a full hex oid;
/// [`RiftError::Io`] on a filesystem failure.
pub(crate) fn enqueue(git_dir: &Path, entry: &QueueEntry) -> Result<PathBuf> {
    if !is_commit_oid(&entry.commit) {
        return Err(RiftError::validity("capture queue: commit is not an oid"));
    }
    let dir = queue_dir(git_dir);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", entry.commit));
    let tmp = dir.join(format!("{}.json.tmp", entry.commit));
    let body = serde_json::to_vec(entry)
        .map_err(|e| RiftError::validity(format!("capture queue: encode: {e}")))?;
    std::fs::write(&tmp, body)?;
    restrict(&tmp)?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

/// Owner-only permissions on a queue file (unix).
fn restrict(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Every readable entry, oldest first. A file this reader cannot understand is
/// **skipped and left in place** — it may be a newer writer's — and reported by
/// `rift doctor` rather than deleted.
///
/// "Understand" includes **the file name agreeing with the `commit` field**,
/// and that is not tidiness. The name is the entry's identity: `record_failure`
/// rewrites an entry by enqueueing it again, which writes `<commit>.json`. So a
/// file named for one commit whose content names another is not a stale entry —
/// it is an entry that **cannot be updated or removed by its own retry path**,
/// because every write lands beside it. Left unscreened it re-condenses on every
/// drain with `attempts` frozen at 0, which is the one shape that escapes the
/// [`MAX_ATTEMPTS`] bound fully. Nothing this crate writes can produce it;
/// the reader screens it anyway, because that is what makes "an entry is
/// rewritten in place" a property of the format rather than of its only writer.
pub(crate) fn entries(git_dir: &Path) -> Vec<(PathBuf, QueueEntry)> {
    let Ok(rd) = std::fs::read_dir(queue_dir(git_dir)) else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, QueueEntry)> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let stem = p.file_stem()?.to_str()?.to_owned();
            let bytes = std::fs::read(&p).ok()?;
            let entry: QueueEntry = serde_json::from_slice(&bytes).ok()?;
            (is_commit_oid(&entry.commit) && stem == entry.commit).then_some((p, entry))
        })
        .collect();
    out.sort_by(|a, b| {
        a.1.queued_at_ms
            .cmp(&b.1.queued_at_ms)
            .then(a.1.commit.cmp(&b.1.commit))
    });
    out
}

/// How many entry FILES the queue holds, parseable or not.
///
/// Deliberately separate from [`entries`], and the difference between the two is
/// the report: an unreadable entry that is merely *counted* alongside readable
/// ones is indistinguishable from a healthy backlog, which is how a file this
/// build cannot drain hides behind one it can.
pub(crate) fn entry_file_count(git_dir: &Path) -> usize {
    std::fs::read_dir(queue_dir(git_dir)).map_or(0, |rd| {
        rd.flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .count()
    })
}

/// Drop one drained entry. Any marker it left behind outlives it deliberately:
/// the marker is a message to the developer and is cleared by being *said*
/// ([`take_errors`]), not by the work that produced it finishing.
pub(crate) fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Record why an entry could not be drained, or why it drained without a
/// checkpoint, beside the entry.
///
/// A failed condense must stay LOUD: with the archive detached, the failure no
/// longer lands on the commit that caused it — and the worker's stderr is
/// `/dev/null`, so printing there would be exactly the silence to be avoided.
/// The marker is what the next hook reads and says out loud instead.
pub(crate) fn mark_error(path: &Path, reason: &str) {
    let one_line: String = reason.chars().filter(|c| *c != '\n').take(300).collect();
    let _ = std::fs::write(path.with_extension("error"), one_line);
}

/// Record a condition that belongs to the **repository** rather than to one
/// queued commit, on the same said-once channel.
///
/// A redactor pack that will not compile is a fact about `.rift/`, not about
/// whichever commit happened to notice it, and it is noticed on paths that have
/// no entry to hang a marker on (`prepare-commit-msg`) or no terminal to print
/// to (the detached worker). `key` names the condition, so re-noticing it
/// overwrites rather than accumulating, and one broken pack costs one line per
/// commit until it is fixed rather than one line per commit forever.
///
/// `key` is screened to non-empty `[a-z0-9-]`, because it becomes a path
/// component and this module's rule is that nothing becomes one unchecked. A key
/// that fails is dropped in silence: a marker is a diagnostic, and refusing to
/// write one must never be louder than what it was going to say.
pub(crate) fn mark_note(git_dir: &Path, key: &str, reason: &str) {
    let usable = !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !usable {
        return;
    }
    let dir = queue_dir(git_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    mark_error(&dir.join(key), reason);
}

/// Record a failed attempt: keep the entry with its attempt count raised, or —
/// once it has spent [`MAX_ATTEMPTS`] — drop it. Either way the reason is left
/// as a marker, so the next hook says it out loud exactly once.
///
/// Returns whether the entry survives for another try, which is the only thing
/// the caller can act on and the only thing worth asserting about.
pub(crate) fn record_failure(
    path: &Path,
    entry: &QueueEntry,
    git_dir: &Path,
    reason: &str,
) -> bool {
    let attempts = entry.attempts.saturating_add(1);
    if attempts >= MAX_ATTEMPTS {
        mark_error(
            path,
            &format!("{reason} (gave up after {attempts} attempts)"),
        );
        remove(path);
        return false;
    }
    mark_error(path, reason);
    let _ = enqueue(
        git_dir,
        &QueueEntry {
            attempts,
            ..entry.clone()
        },
    );
    true
}

/// Read every marker and **delete it**, so the sweep that prints it says it
/// exactly once. A marker that survived being reported would print on every
/// commit forever, and a warning that always fires is a warning nobody reads.
pub(crate) fn take_errors(git_dir: &Path) -> Vec<String> {
    read_errors(git_dir, true)
}

/// Read every marker without consuming it. The read-only Recall Viewer uses
/// this diagnostic form so `/doctor` cannot mutate repository state.
pub(crate) fn peek_errors(git_dir: &Path) -> Vec<String> {
    read_errors(git_dir, false)
}

fn read_errors(git_dir: &Path, consume: bool) -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(queue_dir(git_dir)) else {
        return Vec::new();
    };
    let mut out: Vec<String> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "error"))
        .filter_map(|p| {
            let body = std::fs::read_to_string(&p).ok()?;
            if consume {
                let _ = std::fs::remove_file(&p);
            }
            Some(body)
        })
        .collect();
    out.sort();
    out
}

/// The suffix [`enqueue`] writes before its rename.
const TMP_SUFFIX: &str = ".json.tmp";

/// A file's mtime as epoch ms, or `None` if the filesystem will not say.
fn modified_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// Delete the `<oid>.json.tmp` corpses a dead writer left behind — and only
/// those.
///
/// [`enqueue`] writes `<oid>.json.tmp` and renames it into place, so a process
/// killed between the two leaves the temp file forever. Nothing else ever looks
/// at it: [`entries`] and [`entry_file_count`] both filter on the `json`
/// extension, which `<oid>.json.tmp` does not have. So it is invisible, it is
/// not garbage-collected by the drain that removes real entries, and it
/// accumulates one file per crashed write — the leak a review of this path
/// found.
///
/// **A FRESH temp file is never touched**, and that direction matters more than
/// the sweep does: a young temp file is far more likely to be a write in flight
/// than a corpse, and deleting one under a live writer turns its rename into an
/// error — a cleanup that loses a capture entry. The bound is [`LOCK_STALE_MS`],
/// the age at which this queue already declares a lock holder dead; a second
/// invented constant would just be another number to keep in agreement.
///
/// **The stem must be an oid**, so the sweep can only ever remove a file this
/// module could itself have written. Deleting is the direction where being
/// wrong is expensive, so it is the direction that fails closed.
///
/// Returns how many were removed.
pub(crate) fn sweep_stale_tmps(git_dir: &Path, now_ms: i64) -> usize {
    let Ok(rd) = std::fs::read_dir(queue_dir(git_dir)) else {
        return 0;
    };
    let mut removed = 0usize;
    for path in rd.flatten().map(|e| e.path()) {
        let is_ours = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(TMP_SUFFIX))
            .is_some_and(is_commit_oid);
        if !is_ours {
            continue;
        }
        // An unknown age is not an old age: leave it and let the next sweep,
        // on a filesystem that answers, decide.
        let Some(written_ms) = modified_ms(&path) else {
            continue;
        };
        if now_ms.saturating_sub(written_ms) >= LOCK_STALE_MS && std::fs::remove_file(&path).is_ok()
        {
            removed += 1;
        }
    }
    removed
}

/// The worker's lock: present means a worker is draining. Removed on drop.
pub(crate) struct QueueLock {
    path: PathBuf,
}

impl Drop for QueueLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Take the drain lock, stealing one older than [`LOCK_STALE_MS`].
///
/// `None` means another worker holds it and is recent — the caller exits
/// without draining, because that worker will drain what this one would have.
pub(crate) fn acquire_lock(git_dir: &Path, now_ms: i64) -> Option<QueueLock> {
    let dir = queue_dir(git_dir);
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(LOCK_FILE);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(mut f) => {
            use std::io::Write as _;
            let _ = write!(f, "{now_ms}");
            Some(QueueLock { path })
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let held_at: i64 = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            if now_ms.saturating_sub(held_at) < LOCK_STALE_MS {
                return None;
            }
            // Stale: the holder is gone. Steal it — draining twice is safe
            // (every write the worker makes is idempotent), wedging is not.
            std::fs::write(&path, now_ms.to_string()).ok()?;
            Some(QueueLock { path })
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(commit: &str, at: i64) -> QueueEntry {
        QueueEntry {
            schema_version: 1,
            commit: commit.to_owned(),
            work_tree: PathBuf::from("/w"),
            session_id: Some("01JSESSION".into()),
            agent: Some("claude-code".into()),
            session_state: Some("active".into()),
            queued_at_ms: at,
            attempts: 0,
        }
    }

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    /// A repository-level note rides the entry channel: said once by the sweep
    /// that prints it, overwritten rather than accumulated when the same
    /// condition is noticed again, and refused outright when its key could name
    /// a path outside the queue.
    #[test]
    fn a_repository_note_is_said_once_and_a_bad_key_never_becomes_a_path() {
        let tmp = tempfile::tempdir().unwrap();
        mark_note(tmp.path(), "redactor-pack-0", "pattern did not compile");
        assert_eq!(take_errors(tmp.path()), vec!["pattern did not compile"]);
        assert!(take_errors(tmp.path()).is_empty(), "said once, not twice");

        mark_note(tmp.path(), "redactor-pack-0", "first");
        mark_note(tmp.path(), "redactor-pack-0", "second");
        assert_eq!(
            take_errors(tmp.path()),
            vec!["second"],
            "one condition, one marker — noticing it again is not a second note"
        );

        for bad in ["", "../evil", "a/b", "Upper", "with space", "dot."] {
            mark_note(tmp.path(), bad, "x");
        }
        assert!(
            take_errors(tmp.path()).is_empty(),
            "a key that is not a bare lowercase name writes nothing"
        );
        assert!(
            !tmp.path().join("evil.error").exists(),
            "and nothing escaped the queue directory"
        );
    }

    /// Both directions of the temp sweep, on the file's OWN mtime rather than
    /// the wall clock, so the boundary is exact and the test cannot flake.
    ///
    /// An in-flight write is not garbage: at one millisecond under the bound
    /// the file survives, and it survives having been *looked at* — the sweep
    /// reports 0 rather than reporting a removal it did not make.
    #[test]
    fn a_temp_file_is_swept_only_once_it_is_older_than_the_lock_steal_age() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(queue_dir(dir.path())).unwrap();
        let tmp = queue_dir(dir.path()).join(format!("{A}{TMP_SUFFIX}"));
        std::fs::write(&tmp, b"{}").unwrap();
        let written = modified_ms(&tmp).expect("the filesystem answers mtime");

        assert_eq!(sweep_stale_tmps(dir.path(), written), 0, "same instant");
        assert_eq!(
            sweep_stale_tmps(dir.path(), written + LOCK_STALE_MS - 1),
            0,
            "one millisecond under the bound is still an in-flight write"
        );
        assert!(tmp.exists(), "a fresh temp file must survive the sweep");

        assert_eq!(
            sweep_stale_tmps(dir.path(), written + LOCK_STALE_MS),
            1,
            "at the bound it is a corpse"
        );
        assert!(!tmp.exists());
        assert_eq!(sweep_stale_tmps(dir.path(), written + LOCK_STALE_MS), 0);
    }

    /// The false-positive direction, which is the expensive one: the sweep
    /// deletes, so it must be shown to leave every sibling alone even when they
    /// are ancient — a live entry, its marker, the lock, and a temp file this
    /// module could not have written.
    #[test]
    fn the_temp_sweep_never_touches_a_sibling_however_old() {
        let dir = tempfile::tempdir().unwrap();
        let path = enqueue(dir.path(), &entry(A, 7)).unwrap();
        mark_error(&path, "boom");
        let held = acquire_lock(dir.path(), 1_000).unwrap();
        let foreign = queue_dir(dir.path()).join(format!("not-an-oid{TMP_SUFFIX}"));
        std::fs::write(&foreign, b"{}").unwrap();

        // Far past any bound: everything below is exempt by KIND, not by age.
        assert_eq!(sweep_stale_tmps(dir.path(), i64::MAX / 2), 0);
        assert!(path.exists(), "a queued entry is not a temp file");
        assert!(path.with_extension("error").exists(), "nor is its marker");
        assert!(
            queue_dir(dir.path()).join(LOCK_FILE).exists(),
            "nor the lock"
        );
        assert!(
            foreign.exists(),
            "nor a temp file we could not have written"
        );
        drop(held);
    }

    #[test]
    fn an_entry_round_trips_and_is_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let e = entry(A, 7);
        let path = enqueue(tmp.path(), &e).unwrap();
        assert!(path.is_file(), "the entry landed at {}", path.display());
        let read = entries(tmp.path());
        assert_eq!(read.len(), 1);
        assert_eq!(read[0].1, e);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "queue entries are owner-only: {mode:o}");
        }
    }

    #[test]
    fn a_commit_that_is_not_an_oid_never_becomes_a_path() {
        // The file name IS the oid, so this is the one place an oid becomes a
        // path component — the same rule the session cache applies to ids. The
        // uppercase case is not pedantry: this filesystem is case-insensitive,
        // so it would name the same file while comparing unequal.
        let tmp = tempfile::tempdir().unwrap();
        let upper = "A".repeat(40);
        for bad in ["", "../../etc/passwd", "HEAD", &"a".repeat(39), &upper] {
            let e = entry(bad, 1);
            assert!(enqueue(tmp.path(), &e).is_err(), "accepted {bad:?}");
        }
        assert!(entries(tmp.path()).is_empty());
    }

    #[test]
    fn entries_come_back_oldest_first() {
        let tmp = tempfile::tempdir().unwrap();
        enqueue(tmp.path(), &entry(B, 50)).unwrap();
        enqueue(tmp.path(), &entry(A, 10)).unwrap();
        let got: Vec<String> = entries(tmp.path())
            .into_iter()
            .map(|(_, e)| e.commit)
            .collect();
        assert_eq!(got, vec![A.to_string(), B.to_string()]);
    }

    #[test]
    fn an_unreadable_entry_is_skipped_and_left_in_place() {
        // It may be a newer writer's. Deleting what this reader cannot parse
        // would silently drop a checkpoint; skipping leaves it for `doctor`.
        let tmp = tempfile::tempdir().unwrap();
        enqueue(tmp.path(), &entry(A, 1)).unwrap();
        let junk = queue_dir(tmp.path()).join(format!("{B}.json"));
        std::fs::write(&junk, b"{not json").unwrap();
        assert_eq!(entries(tmp.path()).len(), 1, "only the readable one");
        assert!(junk.exists(), "the unreadable one is not deleted");
        assert_eq!(
            entry_file_count(tmp.path()),
            2,
            "both are counted, so the difference is reportable"
        );
    }

    #[test]
    fn a_marker_is_reported_exactly_once() {
        // The marker is cleared by being SAID, not by the entry draining: a
        // warning that fires on every commit forever is one nobody reads, and
        // one that never fires is the silence this exists to prevent.
        let tmp = tempfile::tempdir().unwrap();
        let path = enqueue(tmp.path(), &entry(A, 1)).unwrap();
        mark_error(&path, "archive failed: git exited non-zero\nsecond line");

        let said = take_errors(tmp.path());
        assert_eq!(said.len(), 1);
        assert!(!said[0].contains('\n'), "a marker is one line");
        assert!(said[0].contains("git exited non-zero"));
        assert!(take_errors(tmp.path()).is_empty(), "said once, not twice");
        assert_eq!(entries(tmp.path()).len(), 1, "the entry still retries");

        remove(&path);
        assert!(entries(tmp.path()).is_empty());
    }

    #[test]
    fn a_marker_outlives_the_entry_that_drained_without_a_checkpoint() {
        // The skip case: the work finished as far as it could, so the entry
        // goes, but the developer has still not been told why no checkpoint
        // landed. Dropping the marker with the entry would lose that.
        let tmp = tempfile::tempdir().unwrap();
        let path = enqueue(tmp.path(), &entry(A, 1)).unwrap();
        mark_error(&path, "no commit to anchor a checkpoint to");
        remove(&path);
        assert!(entries(tmp.path()).is_empty());
        assert_eq!(take_errors(tmp.path()).len(), 1, "the reason survives");
    }

    #[test]
    fn a_second_worker_does_not_drain_while_a_recent_one_holds_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let held = acquire_lock(tmp.path(), 1_000).unwrap();
        assert!(
            acquire_lock(tmp.path(), 1_000 + LOCK_STALE_MS - 1).is_none(),
            "a live holder excludes"
        );
        drop(held);
        assert!(
            acquire_lock(tmp.path(), 2_000).is_some(),
            "releasing frees it"
        );
    }

    #[test]
    fn a_stale_lock_is_stolen_rather_than_wedging_the_queue() {
        // A killed worker must not stop condensation forever. Every write the
        // worker makes is idempotent, so stealing costs duplicated effort at
        // worst — wedging costs every future checkpoint.
        let tmp = tempfile::tempdir().unwrap();
        let held = acquire_lock(tmp.path(), 1_000).unwrap();
        std::mem::forget(held); // simulate a worker killed before Drop ran
        assert!(acquire_lock(tmp.path(), 1_000 + LOCK_STALE_MS).is_some());
    }

    #[test]
    fn an_entry_that_keeps_failing_is_dropped_rather_than_retried_forever() {
        // Measured, not imagined: on a repository whose route needs a backend
        // that fails every time, the queue grew one entry per commit and every
        // commit re-attempted all of them — the deferred work went quadratic
        // in a repository that was never going to succeed.
        let tmp = tempfile::tempdir().unwrap();
        let mut e = entry(A, 1);
        let mut path = enqueue(tmp.path(), &e).unwrap();
        for attempt in 1..MAX_ATTEMPTS {
            assert!(
                record_failure(&path, &e, tmp.path(), "boom"),
                "attempt {attempt} keeps the entry"
            );
            let back = entries(tmp.path());
            assert_eq!(back.len(), 1, "one entry, rewritten in place");
            e = back[0].1.clone();
            path = back[0].0.clone();
            assert_eq!(e.attempts, attempt, "the count is durable");
        }
        assert!(
            !record_failure(&path, &e, tmp.path(), "boom"),
            "the last attempt gives up"
        );
        assert!(entries(tmp.path()).is_empty(), "queue stays bounded");
        let said = take_errors(tmp.path());
        assert_eq!(said.len(), 1);
        assert!(said[0].contains("gave up after"), "and says so: {said:?}");
    }

    #[test]
    fn a_v1_entry_without_an_attempt_count_reads_as_fresh() {
        // Backward compatibility with the first writer, asserted rather than
        // assumed: an entry queued before the counter existed must still drain.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(queue_dir(tmp.path())).unwrap();
        std::fs::write(
            queue_dir(tmp.path()).join(format!("{A}.json")),
            format!(
                r#"{{"schema_version":1,"commit":"{A}","work_tree":"/w",
                    "session_id":null,"agent":null,"session_state":null,
                    "queued_at_ms":5}}"#
            ),
        )
        .unwrap();
        let back = entries(tmp.path());
        assert_eq!(back.len(), 1, "a v1 entry is still readable");
        assert_eq!(back[0].1.attempts, 0);
    }

    #[test]
    fn an_empty_queue_holds_no_files() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(entry_file_count(tmp.path()), 0, "no directory at all");
        let path = enqueue(tmp.path(), &entry(A, 1)).unwrap();
        assert_eq!(entry_file_count(tmp.path()), 1);
        remove(&path);
        assert_eq!(entry_file_count(tmp.path()), 0, "drained");
    }

    #[test]
    fn the_two_values_the_registry_publishes_are_pinned_to_their_literals() {
        // `docs/sync-format.md` states these as a contract a second
        // implementation must match: a lock "stolen after 10 minutes", an entry
        // "dropped at 3". Every other test names them symbolically, so both can
        // be changed to anything at all and the whole suite stays green — which
        // is exactly what a verifier demonstrated (1 s and 7, both green). A
        // published constant needs one assertion that fails when the published
        // number stops being true.
        assert_eq!(LOCK_STALE_MS, 600_000, "sync-format says ten minutes");
        assert_eq!(MAX_ATTEMPTS, 3, "sync-format says three attempts");
    }

    #[test]
    fn an_entry_whose_file_name_disagrees_with_its_commit_is_not_drained() {
        // The one shape that escapes the attempt bound: `record_failure`
        // rewrites an entry by enqueueing it, which writes `<commit>.json`, so
        // a file named for a DIFFERENT commit is never updated and never
        // removed by its own retry path — it re-condenses on every drain with
        // `attempts` frozen at 0, forever. The fixture is the plant itself.
        let tmp = tempfile::tempdir().unwrap();
        let mut misnamed = entry(B, 1);
        misnamed.commit = A.to_string(); // content says A, file will say B
        let dir = queue_dir(tmp.path());
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{B}.json"));
        std::fs::write(&path, serde_json::to_vec(&misnamed).unwrap()).unwrap();

        assert!(
            entries(tmp.path()).is_empty(),
            "a name/content mismatch is not a drainable entry"
        );
        assert!(path.exists(), "left in place — it may be a newer writer's");
        assert_eq!(
            entry_file_count(tmp.path()),
            1,
            "and counted, so doctor can report it"
        );
    }
}
