//! The post-commit capture queue: the durable record of *intent* that
//! lets `post-commit` return inside the hook budget while condensation runs in
//! a detached child.
//!
//! The hook budget is git latency — the synchronous part of a commit's hooks
//! must return within 100 ms warm. Condense + archive spawns nine `git` child
//! processes and measured 103–204 ms on its own, so it does not fit and cannot
//! be made to fit; it is queued instead.
//!
//! Entries carry identifiers and queued context, not transcript or capture bytes.
//! The queue durably records intent. Execution uses the inputs still available
//! when a worker owns the checkout; missing historical or input proof can leave
//! a delivery owed rather than authorize a guessed completion.
//!
//! Execution failures raise a retry count and a backoff recorded on the
//! delivery; the third parks it — kept and named, never removed. Uncertain
//! input and ownership remain visible debt without spending that budget. Queue
//! publication syncs file contents; directory updates are not guaranteed
//! durable across power loss.

pub(crate) mod delivery;
pub(crate) mod request;

use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

/// Legacy worker marker; its presence blocks the new ownership protocol.
const LOCK_FILE: &str = ".lock";
/// One queued commit: what the worker needs to condense it later.
///
/// Supported fields drive execution. The complete validated JSON value retains
/// unknown fields in its context identity and through owned retries.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct QueueEntry {
    /// Contract schema version.
    pub schema_version: u32,
    /// The anchoring commit (HEAD when the hook ran); part of the logical stem.
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

/// How many execution failures **park** a delivery.
///
/// A retry is worth having — an archive can lose a race for `index.lock` and
/// succeed a second later. An *unbounded* retry is not: measured on a repo
/// whose route needs a backend that fails every time, the queue grew one entry
/// per commit and every commit re-attempted all of them, so the deferred work
/// went quadratic in a repository that was never going to succeed. This bound
/// used to **remove** the entry; it now parks it: the ordinary worker skips a
/// parked delivery (and a not-yet-due one, by the backoff recorded beside
/// `attempts`), `rift doctor` names it, and `rift doctor --fix` or
/// `rift session retry` drains it regardless. The checkpoint is never given
/// up on by a counter; it waits for a hand.
pub(crate) const PARK_AFTER: u32 = 3;

/// The queue directory for `git_dir`.
///
/// The member directory when recorded, or the legacy Git-directory location.
/// Resolution does not migrate; migration requires the matching owner.
pub(crate) fn queue_dir(git_dir: &Path) -> PathBuf {
    crate::family::run_state::capture_queue_dir(git_dir)
}

/// Whether `oid` is a full lowercase-hex commit id safe for a logical stem.
fn is_commit_oid(oid: &str) -> bool {
    oid.len() == 40
        && oid
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_lowercase() && c.is_ascii_hexdigit())
}

/// Publish a complete context through a private stage and no-clobber install.
///
/// # Errors
/// [`RiftError::Validity`] if `commit` is not a full hex oid;
/// [`RiftError::Io`] on a filesystem failure.
pub(crate) fn enqueue(git_dir: &Path, entry: &QueueEntry) -> Result<PathBuf> {
    if !is_commit_oid(&entry.commit) {
        return Err(RiftError::validity("capture queue: commit is not an oid"));
    }
    let value = serde_json::to_value(entry)
        .map_err(|_| RiftError::validity("capture queue: encode failed"))?;
    delivery::publish(&queue_dir(git_dir), &entry.commit, &value)
}

/// Every readable entry, oldest first. A file this reader cannot understand is
/// **skipped and left in place** — it may be a newer writer's — and reported by
/// `rift doctor` rather than deleted.
///
/// The filename must match the validated context or supported legacy stem.
/// Owned retries replace only the retry state at the same validated context path.
pub(crate) fn entries(git_dir: &Path) -> Vec<(PathBuf, QueueEntry)> {
    entries_from_paths(rift_shadow::hooks::run_state::list_entries(&queue_dir(
        git_dir,
    )))
}

/// The entries an ordinary pass runs at `now_ms`: due and unparked
/// (`DrainPolicy::Due`). A repair road lists [`entries`] instead.
pub(crate) fn entries_due(git_dir: &Path, now_ms: i64) -> Vec<(PathBuf, QueueEntry)> {
    entry_states(git_dir)
        .into_iter()
        .filter(|(_, _, state)| state.is_due(now_ms))
        .map(|(path, entry, _)| (path, entry))
        .collect()
}

/// Every readable entry with its retry state, oldest first: the doctor's and
/// the drain policy's reading.
pub(crate) fn entry_states(git_dir: &Path) -> Vec<(PathBuf, QueueEntry, delivery::RetryState)> {
    let mut out: Vec<(PathBuf, QueueEntry, delivery::RetryState)> =
        rift_shadow::hooks::run_state::list_entries(&queue_dir(git_dir))
            .into_iter()
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .filter_map(|p| {
                let (value, entry) = read_entry(&p).ok()?;
                let state = delivery::retry_state(&value).ok()?;
                Some((p, entry, state))
            })
            .collect();
    out.sort_by(|a, b| {
        a.1.queued_at_ms
            .cmp(&b.1.queued_at_ms)
            .then_with(|| a.0.cmp(&b.0))
    });
    out
}

/// Advisory readable entries without resolving a writer or journaling migration.
pub(crate) fn entries_read_only(git_dir: &Path) -> Vec<(PathBuf, QueueEntry)> {
    let mut paths = std::collections::BTreeMap::new();
    for dir in crate::family::run_state::capture_queue_read_dirs(git_dir) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                paths
                    .entry(entry.file_name())
                    .or_insert_with(|| entry.path());
            }
        }
    }
    entries_from_paths(paths.into_values().collect())
}

fn entries_from_paths(paths: Vec<PathBuf>) -> Vec<(PathBuf, QueueEntry)> {
    let mut out: Vec<(PathBuf, QueueEntry)> = paths
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .filter_map(|p| {
            let (_, entry) = read_entry(&p).ok()?;
            Some((p, entry))
        })
        .collect();
    out.sort_by(|a, b| {
        a.1.queued_at_ms
            .cmp(&b.1.queued_at_ms)
            .then(a.1.commit.cmp(&b.1.commit))
    });
    out
}

pub(crate) fn read_entry(path: &Path) -> Result<(serde_json::Value, QueueEntry)> {
    let value = delivery::decode(&std::fs::read(path)?)?;
    let entry: QueueEntry = serde_json::from_value(value.clone())
        .map_err(|_| RiftError::validity("capture queue: unsupported entry"))?;
    if !is_commit_oid(&entry.commit) || !delivery::matches_name(path, &entry.commit, &value)? {
        return Err(RiftError::validity(
            "capture queue: name and context disagree",
        ));
    }
    Ok((value, entry))
}

/// How many entry FILES the queue holds, parseable or not.
///
/// Deliberately separate from [`entries`], and the difference between the two is
/// the report: an unreadable entry that is merely *counted* alongside readable
/// ones is indistinguishable from a healthy backlog, which is how a file this
/// build cannot drain hides behind one it can.
pub(crate) fn entry_file_count(git_dir: &Path) -> usize {
    rift_shadow::hooks::run_state::list_entries(&queue_dir(git_dir))
        .iter()
        .filter(|path| path.extension().is_some_and(|x| x == "json"))
        .count()
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
    if !is_note_key(key) {
        return;
    }
    let dir = queue_dir(git_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    mark_error(&dir.join(key), reason);
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

/// Read the one note under `key` (`mark_note`'s channel) without consuming
/// it: how report-only doctor pairs a parked job with its last note. A
/// second report reads the same text.
pub(crate) fn peek_note(git_dir: &Path, key: &str) -> Option<String> {
    let path =
        rift_shadow::hooks::run_state::read_path(&queue_dir(git_dir), &format!("{key}.error"));
    std::fs::read_to_string(path).ok()
}

/// The last failure reason recorded under `key`, which no hook consumes: the
/// note itself is said once and deleted, so "one note per new reason" and the
/// doctor's parked line both need the reason to outlive the saying.
pub(crate) fn last_reason(git_dir: &Path, key: &str) -> Option<String> {
    let path =
        rift_shadow::hooks::run_state::read_path(&queue_dir(git_dir), &format!("{key}.reason"));
    std::fs::read_to_string(path).ok()
}

/// Record `reason` as the last failure reason under `key` (see
/// [`last_reason`]), screened and best-effort exactly as [`mark_note`];
/// whether it differs from the reason recorded before it.
pub(crate) fn record_reason(git_dir: &Path, key: &str, reason: &str) -> bool {
    let one_line: String = reason.chars().filter(|c| *c != '\n').take(300).collect();
    let changed = last_reason(git_dir, key).as_deref() != Some(one_line.as_str());
    if !is_note_key(key) {
        return changed;
    }
    let dir = queue_dir(git_dir);
    if std::fs::create_dir_all(&dir).is_ok() {
        let _ = std::fs::write(dir.join(format!("{key}.reason")), one_line);
    }
    changed
}

/// Forget the last failure reason under `key`: the job it described landed.
pub(crate) fn forget_reason(git_dir: &Path, key: &str) {
    if is_note_key(key) {
        let _ = std::fs::remove_file(queue_dir(git_dir).join(format!("{key}.reason")));
    }
}

/// Whether `key` may name a note file: non-empty `[a-z0-9-]`, at most 64 bytes.
fn is_note_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 64
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn read_errors(git_dir: &Path, consume: bool) -> Vec<String> {
    let mut out: Vec<String> = rift_shadow::hooks::run_state::list_entries(&queue_dir(git_dir))
        .into_iter()
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

/// Queue stages have no age-based cleanup: only their publishing owner removes
/// its own stage. Abandoned stages remain visible debt for explicit recovery.
pub(crate) fn sweep_stale_tmps(_git_dir: &Path, _now_ms: i64) -> usize {
    // Only a producer can clean its own staging path. Age is not ownership.
    0
}

#[derive(Debug)]
pub(crate) struct QueueDeferred(&'static str);

impl std::fmt::Display for QueueDeferred {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for QueueDeferred {}

pub(crate) fn deferred(reason: &'static str) -> RiftError {
    RiftError::Io(std::io::Error::other(QueueDeferred(reason)))
}

/// Proof reads preserve lock contention; other unavailable evidence stays owed.
pub(crate) fn proof_read(
    error: rift_core::RiftError,
    reason: &'static str,
) -> rift_core::RiftError {
    if matches!(error, rift_core::RiftError::ProjectionBusy { .. }) {
        error
    } else {
        deferred(reason)
    }
}

pub(crate) fn is_deferred(error: &(dyn std::error::Error + 'static)) -> bool {
    defer_reason(error).is_some()
}

pub(crate) fn defer_reason(error: &(dyn std::error::Error + 'static)) -> Option<&'static str> {
    let mut cause = Some(error);
    while let Some(error) = cause {
        if let Some(deferred) = error.downcast_ref::<QueueDeferred>() {
            return Some(deferred.0);
        }
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            if let Some(deferred) = io
                .get_ref()
                .and_then(|inner| inner.downcast_ref::<QueueDeferred>())
            {
                return Some(deferred.0);
            }
        }
        cause = error.source();
    }
    None
}

pub(crate) fn remove_owned(path: &Path, owner: &QueueLock) -> Result<()> {
    owner.check_path(path)?;
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Record one execution failure at `failed_at_ms` (the worker's clock, not
/// the entry's): the retry state advances and the reason lands
/// beside the entry, said as [`failure_note`] spells it. The entry is **kept**
/// whatever the count — the third failure parks it. Returns the new state.
pub(crate) fn record_failure_owned(
    path: &Path,
    entry: &QueueEntry,
    reason: &str,
    owner: &QueueLock,
    failed_at_ms: i64,
) -> Result<delivery::RetryState> {
    owner.check_path(path)?;
    let (value, actual) = read_entry(path)?;
    if actual != *entry {
        return Err(deferred("capture delivery changed before retry"));
    }
    let next = delivery::retry_state(&value)?.after_failure(failed_at_ms);
    delivery::replace_retry_state(path, &entry.commit, &value, next, owner)?;
    mark_error(path, &failure_note(reason, next, None));
    Ok(next)
}

/// The note one failure leaves, by where the retry state stands:
/// before the park, the reason and when it is retried; at the park, the two
/// roads that retry it, naming the session when the job has one; past the
/// park, the reason alone — the caller rewrites it only when it changed.
pub(crate) fn failure_note(
    reason: &str,
    state: delivery::RetryState,
    session: Option<&str>,
) -> String {
    use std::cmp::Ordering;
    match state.attempts.cmp(&PARK_AFTER) {
        Ordering::Less => format!(
            "{reason}; retried after {}",
            rift_core::Timestamp::from_millis(state.next_due_ms)
        ),
        Ordering::Equal => {
            let retry = session.map_or_else(
                || "`rift session retry --all`".to_owned(),
                |session| format!("`rift session retry --session {session}`"),
            );
            format!(
                "{reason} — parked after {PARK_AFTER} attempts; `rift doctor --fix` or {retry} retries it"
            )
        }
        Ordering::Greater => reason.to_owned(),
    }
}

/// Why ownership could not be established. No queue payload belongs in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerBlock {
    ActiveOwner,
    LegacyBarrier,
    Unavailable,
}

/// One member's stable descriptor owner, independent of queue migration.
pub(crate) struct QueueLock {
    file: std::fs::File,
    git_dir: PathBuf,
}

impl Drop for QueueLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl QueueLock {
    pub(crate) fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    pub(crate) fn check_current(&self) -> Result<()> {
        let current = std::fs::symlink_metadata(owner_path(&self.git_dir)?)?;
        if !current.file_type().is_file() {
            return Err(RiftError::validity("queue owner: marker was replaced"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let held = self.file.metadata()?;
            if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
                return Err(RiftError::validity("queue owner: marker was replaced"));
            }
        }
        Ok(())
    }

    pub(crate) fn matches_git_dir(&self, git_dir: &Path) -> bool {
        std::fs::canonicalize(git_dir).is_ok_and(|path| path == self.git_dir)
    }

    /// Ordinary work requires absence of any recovery hold, without parsing it.
    pub(crate) fn check_recovery_hold(&self) -> Result<()> {
        match self
            .git_dir
            .join(".rift-capture-recovery.hold")
            .symlink_metadata()
        {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            _ => Err(deferred("capture recovery hold is present or unavailable")),
        }
    }

    pub(crate) fn check_path(&self, path: &Path) -> Result<()> {
        self.check_current()?;
        self.check_recovery_hold()?;
        if legacy_barrier(self.git_dir()) {
            return Err(deferred("legacy ownership remains unresolved"));
        }
        let parent = path.parent().and_then(|p| std::fs::canonicalize(p).ok());
        let roots = [
            queue_dir(&self.git_dir),
            crate::agent_queue::queue_dir(&self.git_dir),
            rift_shadow::hooks::run_state::RunRoot::CaptureQueue.legacy_under(&self.git_dir),
            rift_shadow::hooks::run_state::RunRoot::AgentQueue.legacy_under(&self.git_dir),
        ];
        if !roots
            .iter()
            .any(|root| std::fs::canonicalize(root).ok() == parent)
            || parent.is_none()
        {
            return Err(RiftError::validity(
                "queue owner: path belongs to another member",
            ));
        }
        Ok(())
    }
}

pub(crate) fn owner_path(git_dir: &Path) -> std::io::Result<PathBuf> {
    Ok(std::fs::canonicalize(git_dir)?.join(".rift-capture-owner.lock"))
}

pub(crate) fn acquire_owner(git_dir: &Path) -> std::result::Result<QueueLock, OwnerBlock> {
    let owner = lock_owner(git_dir)?;
    if legacy_barrier(owner.git_dir()) {
        return Err(OwnerBlock::LegacyBarrier);
    }
    Ok(owner)
}

/// The owner flock alone, with the legacy markers left unread. Only the
/// doctor's retirement of a stale legacy marker takes it this way: holding
/// it proves no current-format capture runs while the marker is judged.
pub(crate) fn lock_owner(git_dir: &Path) -> std::result::Result<QueueLock, OwnerBlock> {
    let git_dir = std::fs::canonicalize(git_dir).map_err(|_| OwnerBlock::Unavailable)?;
    let path = owner_path(&git_dir).map_err(|_| OwnerBlock::Unavailable)?;
    if path
        .symlink_metadata()
        .is_ok_and(|meta| !meta.file_type().is_file())
    {
        return Err(OwnerBlock::Unavailable);
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        options.share_mode(3); // readers/writers, never delete-sharing while owned
    }
    let file = options.open(path).map_err(|_| OwnerBlock::Unavailable)?;
    file.try_lock().map_err(|error| match error {
        std::fs::TryLockError::WouldBlock => OwnerBlock::ActiveOwner,
        std::fs::TryLockError::Error(_) => OwnerBlock::Unavailable,
    })?;
    let owner = QueueLock { file, git_dir };
    owner.check_current().map_err(|_| OwnerBlock::Unavailable)?;
    Ok(owner)
}

/// The old-format lock file in the legacy capture-queue directory. It holds
/// a timestamp and no pid, so nothing in it says whether its writer lives.
pub(crate) fn legacy_queue_lock(git_dir: &Path) -> PathBuf {
    rift_shadow::hooks::run_state::RunRoot::CaptureQueue
        .legacy_under(git_dir)
        .join(LOCK_FILE)
}

fn legacy_barrier(git_dir: &Path) -> bool {
    let mut markers = vec![
        legacy_queue_lock(git_dir),
        queue_dir(git_dir).join(LOCK_FILE),
    ];
    markers.extend(crate::family::run_state::legacy_move_marker(git_dir));
    markers.iter().any(|marker| !matches!(marker.symlink_metadata(), Err(error) if error.kind() == std::io::ErrorKind::NotFound))
}

/// Compatibility clock parameter carries no ownership or expiry authority.
pub(crate) fn acquire_lock(git_dir: &Path, _now_ms: i64) -> Option<QueueLock> {
    acquire_owner(git_dir).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomicity_an_old_capture_completion_does_not_remove_a_new_delivery() {
        let tmp = tempfile::tempdir().unwrap();
        let owner = acquire_lock(tmp.path(), 1_000).unwrap();
        let first = entry(A, 1_000);
        enqueue(tmp.path(), &first).unwrap();
        let observed = entries(tmp.path());
        assert_eq!(observed.len(), 1);
        assert_eq!(observed[0].1, first);
        let mut second = entry(A, 2_000);
        second.session_id = Some("replacement-session".into());
        let published = enqueue(tmp.path(), &second).unwrap();
        assert!(entries(tmp.path()).contains(&(published.clone(), second.clone())));
        eprintln!("capture old/new share path: {}", observed[0].0 == published);
        remove_owned(&observed[0].0, &owner).unwrap();
        assert!(
            entries(tmp.path()).contains(&(published, second)),
            "completing observed A must leave successfully published B owed"
        );
    }

    #[test]
    fn simultaneous_os_lock_contenders_hold_at_most_one_queue_owner() {
        let tmp = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(3);
        let held = std::thread::scope(|scope| {
            let a = scope.spawn(|| {
                barrier.wait();
                acquire_lock(tmp.path(), 601_000)
            });
            let b = scope.spawn(|| {
                barrier.wait();
                acquire_lock(tmp.path(), 601_000)
            });
            barrier.wait();
            [a.join().unwrap(), b.join().unwrap()]
        });
        assert_eq!(held.iter().filter(|owner| owner.is_some()).count(), 1);
    }

    #[test]
    fn ownership_a_live_queue_owner_is_not_stolen_after_ten_minutes() {
        let tmp = tempfile::tempdir().unwrap();
        let owner = acquire_lock(tmp.path(), 1_000).unwrap();
        let contender = acquire_lock(tmp.path(), 601_000);
        assert!(
            contender.is_none(),
            "elapsed time does not release a live owner"
        );
        drop(owner);
    }

    #[cfg(unix)]
    #[test]
    fn ownership_dropping_a_queue_predecessor_preserves_its_successor() {
        use std::os::unix::fs::MetadataExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let predecessor = acquire_lock(tmp.path(), 1_000).unwrap();
        let marker = owner_path(tmp.path()).unwrap();
        std::fs::rename(&marker, tmp.path().join("replaced-owner")).unwrap();
        assert!(predecessor.check_current().is_err());
        let successor = acquire_lock(tmp.path(), 2_000).unwrap();
        let identity = std::fs::metadata(&marker).unwrap().ino();
        assert!(acquire_lock(tmp.path(), 2_001).is_none());
        drop(predecessor);
        assert_eq!(std::fs::metadata(&marker).unwrap().ino(), identity);
        assert!(acquire_lock(tmp.path(), 2_002).is_none());
        drop(successor);
    }

    #[test]
    fn ownership_an_empty_legacy_queue_marker_is_not_proof_of_abandonment() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = queue_dir(tmp.path()).join(LOCK_FILE);
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"").unwrap();
        let contender = acquire_lock(tmp.path(), 601_000);
        assert!(
            contender.is_none(),
            "an old worker may be publishing its marker"
        );
        assert_eq!(std::fs::read(marker).unwrap(), b"");
    }

    /// A serial control, not a deterministic simultaneous stale-read schedule.
    #[test]
    fn ownership_two_serial_queue_contenders_preserve_unknown_legacy_debt() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = queue_dir(tmp.path()).join(LOCK_FILE);
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, b"0").unwrap();
        assert!(acquire_lock(tmp.path(), 601_000).is_none());
        assert!(acquire_lock(tmp.path(), 601_001).is_none());
        assert_eq!(std::fs::read(marker).unwrap(), b"0");
    }

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
    fn a_queue_stage_is_retained_regardless_of_an_invented_death_age() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(queue_dir(dir.path())).unwrap();
        let path = queue_dir(dir.path()).join(format!("{A}.json.tmp"));
        std::fs::write(&path, b"in-flight bytes").unwrap();
        for instant in [0, 601_000, i64::MAX / 2] {
            assert_eq!(sweep_stale_tmps(dir.path(), instant), 0);
            assert_eq!(std::fs::read(&path).unwrap(), b"in-flight bytes");
        }
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
        let foreign = queue_dir(dir.path()).join("not-an-oid.json.tmp");
        std::fs::write(&foreign, b"{}").unwrap();

        // Far past any bound: everything below is exempt by KIND, not by age.
        assert_eq!(sweep_stale_tmps(dir.path(), i64::MAX / 2), 0);
        assert!(path.exists(), "a queued entry is not a temp file");
        assert!(path.with_extension("error").exists(), "nor is its marker");
        assert!(owner_path(dir.path()).unwrap().exists(), "nor the lock");
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

        remove_owned(&path, &acquire_owner(tmp.path()).unwrap()).unwrap();
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
        remove_owned(&path, &acquire_owner(tmp.path()).unwrap()).unwrap();
        assert!(entries(tmp.path()).is_empty());
        assert_eq!(take_errors(tmp.path()).len(), 1, "the reason survives");
    }

    #[test]
    fn a_second_worker_does_not_drain_while_a_recent_one_holds_the_lock() {
        let tmp = tempfile::tempdir().unwrap();
        let held = acquire_lock(tmp.path(), 1_000).unwrap();
        assert!(
            acquire_lock(tmp.path(), 601_000).is_none(),
            "a live holder excludes"
        );
        drop(held);
        assert!(
            acquire_lock(tmp.path(), 2_000).is_some(),
            "releasing frees it"
        );
    }

    #[test]
    fn an_unheld_persistent_owner_file_is_reusable_without_an_age_guess() {
        let tmp = tempfile::tempdir().unwrap();
        let marker = owner_path(tmp.path()).unwrap();
        std::fs::write(&marker, b"old owner metadata").unwrap();
        let owner = acquire_lock(tmp.path(), 1).unwrap();
        assert!(acquire_lock(tmp.path(), 601_000).is_none());
        drop(owner);
        assert!(
            marker.exists(),
            "descriptor release never unlinks the owner file"
        );
        assert!(acquire_lock(tmp.path(), 2).is_some());
    }

    /// Re-pinned when parking replaced removal: the bound is still three
    /// execution failures, and it still keeps the queue off the quadratic road
    /// (a parked entry is skipped by the ordinary pass, so nothing re-attempts
    /// it on every commit) — but the third failure **parks** the entry rather
    /// than removing it. Both halves are proven: the file is still present
    /// after the third failure AND `parked == true` with `next_due_ms` set.
    #[test]
    fn an_entry_that_keeps_failing_is_parked_rather_than_retried_forever_or_dropped() {
        // Measured, not imagined: on a repository whose route needs a backend
        // that fails every time, the queue grew one entry per commit and every
        // commit re-attempted all of them — the deferred work went quadratic
        // in a repository that was never going to succeed.
        let tmp = tempfile::tempdir().unwrap();
        let owner = acquire_owner(tmp.path()).unwrap();
        let now = 1_790_000_000_000;
        let mut e = entry(A, 1);
        let mut path = enqueue(tmp.path(), &e).unwrap();
        for attempt in 1..PARK_AFTER {
            let state = record_failure_owned(&path, &e, "boom", &owner, now).unwrap();
            assert_eq!(state.attempts, attempt);
            assert!(!state.parked, "attempt {attempt} keeps the entry due later");
            let back = entries(tmp.path());
            assert_eq!(back.len(), 1, "one entry, rewritten in place");
            e = back[0].1.clone();
            path = back[0].0.clone();
            assert_eq!(e.attempts, attempt, "the count is durable");
            assert!(entries_due(tmp.path(), now).is_empty(), "not due yet");
            assert_eq!(entries_due(tmp.path(), state.next_due_ms).len(), 1);
        }
        let parked = record_failure_owned(&path, &e, "boom", &owner, now).unwrap();
        assert_eq!(parked.attempts, PARK_AFTER);
        assert!(parked.parked, "the third failure parks");
        assert_eq!(parked.next_due_ms, now + 120_000);
        assert!(path.exists(), "the entry is kept, never dropped");
        assert_eq!(entries(tmp.path()).len(), 1, "a repair road still lists it");
        assert!(
            entries_due(tmp.path(), i64::MAX).is_empty(),
            "the ordinary pass never runs a parked entry"
        );
        let states = entry_states(tmp.path());
        assert_eq!(states[0].2, parked);
        let said = take_errors(tmp.path());
        assert_eq!(said.len(), 1);
        assert!(
            said[0].contains("parked after 3 attempts") && said[0].contains("rift doctor --fix"),
            "and says so: {said:?}"
        );
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
        remove_owned(&path, &acquire_owner(tmp.path()).unwrap()).unwrap();
        assert_eq!(entry_file_count(tmp.path()), 0, "drained");
    }

    /// Re-asserted when parking replaced removal: three execution failures is
    /// still the bound (the sync-format note says so), and what it bounds is
    /// now the park, not a removal — `PARK_AFTER` has no "remove" meaning.
    #[test]
    fn the_retry_bound_remains_three_actual_execution_failures() {
        assert_eq!(
            PARK_AFTER, 3,
            "sync-format says three execution failures park"
        );
        let third = delivery::RetryState::default()
            .after_failure(0)
            .after_failure(0)
            .after_failure(0);
        assert!(third.parked && third.attempts == PARK_AFTER);
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
