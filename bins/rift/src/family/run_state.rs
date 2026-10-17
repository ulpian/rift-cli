//! Move one checkout's run state from its Git directory into family storage.
//!
//! Hooks write the new tree and read missing names from the unmoved legacy
//! tree. Workers, sweeps and doctor move only under the matching canonical
//! Git-directory owner and the family mover lock, in that acquisition order.
//! Both locks belong to open files; neither age nor Drop unlinks ownership.
//!
//! Any legacy queue lock or mover marker blocks admission regardless of age.
//! Older installed binaries do not honor the new locks: controlled exclusion
//! of their producers and owners is required before removing their markers.
//!
//! Session caches keep the new copy and preserve a different old copy under
//! a late name. Queue tickets use complete context identity instead: migration
//! discharges a source only after the installed complete value, including its
//! attempt counter, matches. Conflicting or unreadable tickets stay owed and
//! prevent the moved marker. Publication never clobbers a newly arrived file.
//!
//! Private queue stages are retained as unfinished-publication evidence;
//! only their producer cleans its own stage. Session-cache temp age handling
//! is separate and unchanged. The moved marker is written last, after ready
//! migration, and readers retain dual-root visibility until it is present.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rift_shadow::hooks::run_state::{self as layout, RunRoot, MOVED_MARKER, MOVE_LOCK};
use rift_shadow::hooks::session_cache;

/// The capture queue's drain lock inside its directory. Never moved: an old
/// binary may be draining under it.
const QUEUE_LOCK: &str = ".lock";
/// How long a mover waits for another mover's lock, step by step, before it
/// leaves the move to a later pass: 25 ms in all, never on a hook (a hook
/// never moves), and with no timer or thread — the caller sleeps.
const MOVE_WAIT_STEPS_MS: [u64; 5] = [1, 2, 4, 8, 10];

/// One member's resolution, memoized for the life of the process.
/// Held while a report-only command runs on this thread (`rift doctor`
/// without `--fix`): every run-state path it resolves is found without
/// recording member metadata, journaling the move, or minting a family, so a
/// report changes nothing it reports on. Dropping it restores what was held.
#[must_use = "the report-only resolution ends when the guard drops"]
pub(crate) struct ReportOnly(bool);

#[derive(Clone, Debug)]
struct Resolved {
    run_root: PathBuf,
    member: PathBuf,
    /// The work tree the move job names.
    work_tree: PathBuf,
    /// Whether this process already journaled the move job.
    move_owed: bool,
    /// Files an older binary wrote to the old tree after the member was
    /// marked moved, as this process's move found them.
    late_writes: usize,
}

/// What one pass of [`adopt`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MoveReport {
    /// Files renamed or copied into the new tree.
    pub(crate) moved: usize,
    /// Files dropped because the new tree held the same bytes.
    pub(crate) superseded: usize,
    /// Files kept beside the new tree's copy under a late name.
    pub(crate) preserved: usize,
    /// Another mover held the lock; nothing was moved.
    pub(crate) busy: bool,
}

/// The inputs of one move.
pub(crate) struct MoveCtx<'a> {
    /// The member's git dir, holding the old tree.
    pub(crate) git_dir: &'a Path,
    /// The family's run root, holding the move lock.
    pub(crate) run_root: &'a Path,
    /// The member's run directory.
    pub(crate) member: &'a Path,
    /// The clock the lock and the dead-writer rules read.
    pub(crate) now_ms: i64,
}

/// What the doctor reports about a member's old tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Leftovers {
    /// Files still under `<git_dir>/rift-*`.
    pub(crate) files: usize,
    /// Files this process moved or preserved that an older binary wrote
    /// after the move.
    pub(crate) late_writes: usize,
    /// Late files preserved in the new tree, by any move.
    pub(crate) preserved: usize,
    /// Whether the member is marked moved.
    pub(crate) moved: bool,
}

impl Leftovers {
    /// Whether an older binary is still writing the old tree: something
    /// arrived there after the member was marked moved.
    pub(crate) fn old_binary_writing(&self) -> bool {
        self.moved && (self.files > 0 || self.late_writes > 0 || self.preserved > 0)
    }
}

/// Where a member's run state lives.
struct Located {
    /// The family's run root, holding the move lock.
    run_root: PathBuf,
    /// The member's run directory.
    member: PathBuf,
    /// The member's work tree, as its git dir names it.
    work_tree: PathBuf,
}

/// Both paths share ownership and readiness; only exact hold admission differs.
#[derive(Clone, Copy)]
enum Admission<'a> {
    Ordinary(&'a crate::capture_queue::QueueLock),
    Recovery(&'a crate::capture_recovery::hold::OwnHold<'a>),
}

impl<'a> Admission<'a> {
    fn owner(self) -> &'a crate::capture_queue::QueueLock {
        match self {
            Self::Ordinary(owner) => owner,
            Self::Recovery(hold) => hold.owner(),
        }
    }

    fn check(self) -> rift_core::Result<()> {
        self.owner().check_current()?;
        match self {
            Self::Ordinary(owner) => owner.check_recovery_hold(),
            Self::Recovery(hold) => hold.check_current().map_err(|_| {
                crate::capture_queue::deferred("recovery hold admission is unavailable")
            }),
        }
    }

    fn check_io(self) -> std::io::Result<()> {
        self.check()
            .map_err(|_| std::io::Error::other("member ownership or hold admission is unavailable"))
    }
}

/// The merge rules of one root.
struct Merge<'a> {
    now_ms: i64,
    root: RunRoot,
    admission: Admission<'a>,
    mover: &'a MoveLock,
}

/// A persistent descriptor owns migration; the legacy marker is only a barrier.
struct MoveLock {
    file: std::fs::File,
    path: PathBuf,
}

impl MoveLock {
    fn take(run_root: &Path, _now_ms: i64) -> std::io::Result<Option<Self>> {
        layout::create_private_dir(run_root)?;
        match run_root.join(MOVE_LOCK).symlink_metadata() {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Ok(_) => return Ok(None),
            Err(error) => return Err(error),
        }
        let path = run_root.join(".move-owner.lock");
        if path
            .symlink_metadata()
            .is_ok_and(|meta| !meta.file_type().is_file())
        {
            return Err(std::io::Error::other(
                "migration owner is not a regular file",
            ));
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
            options.share_mode(3);
        }
        let file = options.open(&path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { file, path })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(error)) => Err(error),
        }
    }
    fn check_current(&self) -> std::io::Result<()> {
        let current = self.path.symlink_metadata()?;
        if !current.file_type().is_file() {
            return Err(std::io::Error::other("move owner changed"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            let held = self.file.metadata()?;
            if (held.dev(), held.ino()) != (current.dev(), current.ino()) {
                return Err(std::io::Error::other("move owner changed"));
            }
        }
        Ok(())
    }
}

impl Drop for MoveLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl Merge<'_> {
    fn check_owners(&self) -> std::io::Result<()> {
        self.admission.check_io()?;
        self.mover.check_current()
    }

    /// Merge every entry of `from` into `to`, recursing into directories (a
    /// session's subagent claims). Symbolic links are neither followed nor
    /// moved; the doctor counts them as leftovers.
    fn dir(&self, from: &Path, to: &Path, report: &mut MoveReport) -> std::io::Result<()> {
        self.check_owners()?;
        let entries = match std::fs::read_dir(from) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        layout::create_private_dir(to)?;
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            let source = entry.path();
            let target = to.join(entry.file_name());
            if kind.is_dir() {
                self.dir(&source, &target, report)?;
                self.check_owners()?;
                let _ = std::fs::remove_dir(&source);
                continue;
            }
            if kind.is_file() {
                let name = entry.file_name().to_string_lossy().into_owned();
                self.file(&source, &target, &name, report)?;
            }
        }
        Ok(())
    }

    /// Merge one file: an in-flight write stays, a dead writer's leftover is
    /// removed, and a file the new tree already holds keeps the new tree's
    /// copy (the module's one rule).
    fn file(
        &self,
        source: &Path,
        target: &Path,
        name: &str,
        report: &mut MoveReport,
    ) -> std::io::Result<()> {
        self.check_owners()?;
        if !matches!(self.root, RunRoot::Sessions)
            && (is_in_flight(name)
                || name
                    .rsplit_once('.')
                    .is_some_and(|(_, extension)| extension == "stage"))
        {
            report.busy = true;
            return Ok(());
        }
        if !matches!(self.root, RunRoot::Sessions) && has_extension(name, "json") {
            return self.queue_file(source, target, report);
        }
        if is_in_flight(name) {
            let dead = modified_ms(source).is_some_and(|written| {
                self.now_ms.saturating_sub(written) >= rift_shadow::hooks::STALE_WRITER_MS
            });
            if dead {
                self.check_owners()?;
                let _ = std::fs::remove_file(source);
            }
            return Ok(());
        }
        if target.symlink_metadata().is_ok() {
            return self.settle_both(source, target, name, report);
        }
        #[cfg(test)]
        tests::pause_migration(tests::MigrationPausePoint::TargetAbsent);
        match move_file(source, target, &|| self.check_owners()) {
            Ok(()) => report.moved += 1,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                self.settle_both(source, target, name, report)?;
            }
            Err(_) if taken_by_another(source) => (),
            Err(e) => return Err(e),
        }
        Ok(())
    }

    fn queue_file(
        &self,
        source: &Path,
        target: &Path,
        report: &mut MoveReport,
    ) -> std::io::Result<()> {
        self.check_owners()?;
        let (value, stem) = queue_value(self.root, source)?;
        if source == target
            && crate::capture_queue::delivery::is_versioned(source, &stem, &value)
                .map_err(|_| std::io::Error::other("queue context is unsupported"))?
        {
            return Ok(());
        }
        let parent = target
            .parent()
            .ok_or_else(|| std::io::Error::other("queue has no parent"))?;
        let (installed, fresh) =
            crate::capture_queue::delivery::publish_exact_observed(parent, &stem, &value)
                .map_err(|_| std::io::Error::other("queue migration context conflict"))?;
        #[cfg(test)]
        tests::pause_migration(tests::MigrationPausePoint::QueuePublished);
        if installed != source {
            self.check_owners()?;
            ignore_not_found(std::fs::remove_file(source))?;
            if fresh {
                report.moved += 1;
            } else {
                report.superseded += 1;
            }
        }
        Ok(())
    }

    /// A file both trees hold: the new tree's copy stands; the old copy is
    /// removed when it is the same bytes and preserved beside it otherwise.
    fn settle_both(
        &self,
        source: &Path,
        target: &Path,
        name: &str,
        report: &mut MoveReport,
    ) -> std::io::Result<()> {
        if same_bytes(source, target) {
            self.check_owners()?;
            ignore_not_found(std::fs::remove_file(source))?;
            report.superseded += 1;
            return Ok(());
        }
        loop {
            let late = free_late_path(target, name, self.now_ms);
            match move_file(source, &late, &|| self.check_owners()) {
                Ok(()) => {
                    report.preserved += 1;
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(_) if taken_by_another(source) => break,
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

/// The member's sessions directory: caches, sidecars, claim markers.
pub(crate) fn sessions_dir(git_dir: &Path) -> PathBuf {
    root_dir(git_dir, RunRoot::Sessions)
}

/// The member's agent-hook job journal.
pub(crate) fn agent_queue_dir(git_dir: &Path) -> PathBuf {
    root_dir(git_dir, RunRoot::AgentQueue)
}

/// Queue roots for an advisory read without recording metadata or move work.
/// An unavailable/unminted family leaves only its known legacy root visible.
pub(crate) fn capture_queue_read_dirs(git_dir: &Path) -> Vec<PathBuf> {
    let legacy = RunRoot::CaptureQueue.legacy_under(git_dir);
    let Some(member) = minted_member(git_dir) else {
        return vec![legacy];
    };
    let current = RunRoot::CaptureQueue.under(&member);
    // The dual read's rule: after the move, the old directory is read only
    // while it still stands, which is what an older binary wrote since.
    if layout::is_moved(&member) && legacy.symlink_metadata().is_err() {
        vec![current]
    } else {
        vec![current, legacy]
    }
}

/// The member's post-commit capture queue.
pub(crate) fn capture_queue_dir(git_dir: &Path) -> PathBuf {
    root_dir(git_dir, RunRoot::CaptureQueue)
}

/// The old mover marker, resolved without acquiring or moving run state.
pub(crate) fn legacy_move_marker(git_dir: &Path) -> Option<PathBuf> {
    locate(git_dir).map(|found| found.run_root.join(MOVE_LOCK))
}

/// The old tree of a member, as the doctor reports it; `None` when the
/// member has no family to move into, which leaves the git dir its home.
pub(crate) fn leftovers(git_dir: &Path) -> Option<Leftovers> {
    let key = rift_git::family::canonical_or_lexical(git_dir);
    let member = locate(&key)?.member;
    let preserved = RunRoot::ALL
        .into_iter()
        .map(|root| count_late(&root.under(&member)))
        .sum();
    let late_writes = memo()
        .lock()
        .ok()
        .and_then(|m| m.get(&key).map(|r| r.late_writes))
        .unwrap_or(0);
    let files = RunRoot::ALL
        .into_iter()
        .map(|root| count_files(&root.legacy_under(&key)))
        .sum();
    Some(Leftovers {
        files,
        late_writes,
        preserved,
        moved: layout::is_moved(&member),
    })
}

/// Move a member's old tree into its run directory, then mark it moved.
///
/// Cheap when there is nothing to do: one `stat` of the marker and one of
/// each old directory. A crash anywhere leaves both trees readable by the
/// next pass, which finishes the move under the same merge rules; the marker
/// is written last, so it never claims a move that did not finish.
///
/// # Errors
/// An I/O error the move could not step past; the marker is then not written
/// and the next resolution tries again.
pub(crate) fn adopt(ctx: &MoveCtx<'_>) -> std::io::Result<MoveReport> {
    let Ok(owner) = crate::capture_queue::acquire_owner(ctx.git_dir) else {
        return Ok(MoveReport {
            busy: true,
            ..MoveReport::default()
        });
    };
    adopt_owned(ctx, &owner)
}

pub(crate) fn adopt_owned(
    ctx: &MoveCtx<'_>,
    owner: &crate::capture_queue::QueueLock,
) -> std::io::Result<MoveReport> {
    adopt_with(ctx, Admission::Ordinary(owner))
}

fn adopt_with(ctx: &MoveCtx<'_>, admission: Admission<'_>) -> std::io::Result<MoveReport> {
    let owner = admission.owner();
    if !owner.matches_git_dir(ctx.git_dir) || owner.check_current().is_err() {
        return Err(std::io::Error::other(
            "migration owner does not match member",
        ));
    }
    admission.check_io()?;
    let Some(mover) = MoveLock::take(ctx.run_root, ctx.now_ms)? else {
        return Ok(MoveReport {
            busy: true,
            ..MoveReport::default()
        });
    };
    if queue_conflict(ctx)? {
        return Ok(MoveReport {
            busy: true,
            ..MoveReport::default()
        });
    }
    let marked = layout::is_moved(ctx.member);
    let mut report = MoveReport::default();
    for root in RunRoot::ALL {
        let target = root.under(ctx.member);
        let merge = Merge {
            now_ms: ctx.now_ms,
            root,
            admission,
            mover: &mover,
        };
        if !matches!(root, RunRoot::Sessions) {
            for path in queue_paths(&target)? {
                if path
                    .extension()
                    .is_some_and(|extension| extension == "json")
                {
                    merge.queue_file(&path, &path, &mut report)?;
                }
            }
        }
        let from = root.legacy_under(ctx.git_dir);
        merge.dir(&from, &target, &mut report)?;
        merge.check_owners()?;
        let _ = std::fs::remove_dir(&from);
        if !matches!(root, RunRoot::Sessions) && !queue_paths(&from)?.is_empty() {
            report.busy = true;
        }
    }
    report.busy |= queue_conflict(ctx)?;
    if !marked && !report.busy {
        #[cfg(test)]
        tests::pause_migration(tests::MigrationPausePoint::BeforeMarker);
        mover.check_current()?;
        admission.check_io()?;
        write_marker(ctx.member, ctx.now_ms)?;
    }
    Ok(report)
}

fn queue_paths(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    entries
        .map(|entry| entry.map(|entry| entry.path()))
        .collect()
}

fn queue_value(root: RunRoot, path: &Path) -> std::io::Result<(serde_json::Value, String)> {
    let parsed = match root {
        RunRoot::CaptureQueue => {
            crate::capture_queue::read_entry(path).map(|(value, entry)| (value, entry.commit))
        }
        RunRoot::AgentQueue => crate::agent_queue::read_job(path).and_then(|(value, job)| {
            crate::agent_queue::logical_stem(&job)
                .map(|stem| (value, stem))
                .ok_or_else(|| rift_core::RiftError::validity("queue identity unavailable"))
        }),
        RunRoot::Sessions => return Err(std::io::Error::other("not a queue")),
    };
    parsed.map_err(|_| std::io::Error::other("queue payload unsupported or inconsistent"))
}

fn queue_conflict(ctx: &MoveCtx<'_>) -> std::io::Result<bool> {
    for root in [RunRoot::AgentQueue, RunRoot::CaptureQueue] {
        let mut seen = Seen::default();
        for dir in [root.legacy_under(ctx.git_dir), root.under(ctx.member)] {
            for path in queue_paths(&dir)? {
                if path.file_name().is_some_and(|name| name == ".lock") {
                    return Ok(true);
                }
                if path.extension().is_none_or(|extension| extension != "json") {
                    continue;
                }
                let (value, stem) = queue_value(root, &path)?;
                let versioned = crate::capture_queue::delivery::is_versioned(&path, &stem, &value)
                    .map_err(|_| std::io::Error::other("queue context is unsupported"))?;
                if seen.conflicts(root, stem, versioned, value) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}

/// The copies of each logical delivery seen so far in one queue root.
///
/// A context-addressed file is a distinct delivery: two of one stem with
/// different contexts both stay owed (a changed instant keeps both). A
/// pre-context file names only its stem, so it conflicts with any other copy
/// of that stem that differs, and the move then leaves every copy in place.
#[derive(Default)]
struct Seen {
    legacy: BTreeMap<(&'static str, String), serde_json::Value>,
    versioned: BTreeMap<(&'static str, String), Vec<serde_json::Value>>,
}

impl Seen {
    fn conflicts(
        &mut self,
        root: RunRoot,
        stem: String,
        versioned: bool,
        value: serde_json::Value,
    ) -> bool {
        let key = (root.dir_name(), stem);
        let differs =
            |other: &serde_json::Value| *other != value && !repeatable_request(root, other, &value);
        let clash = self.legacy.get(&key).is_some_and(differs)
            || (!versioned
                && self
                    .versioned
                    .get(&key)
                    .is_some_and(|copies| copies.iter().any(differs)));
        if versioned {
            self.versioned.entry(key).or_default().push(value);
        } else {
            self.legacy.insert(key, value);
        }
        clash
    }
}

fn repeatable_request(
    root: RunRoot,
    first: &serde_json::Value,
    second: &serde_json::Value,
) -> bool {
    use crate::agent_queue::AgentJob;
    if !matches!(root, RunRoot::AgentQueue) {
        return false;
    }
    serde_json::from_value::<AgentJob>(first.clone()).is_ok_and(|job| job.kind.is_repeatable())
        && crate::agent_queue::same_request(first, second)
}

/// Readiness shares the already-held member guard; it never reacquires it.
pub(crate) fn prepare_owned(owner: &crate::capture_queue::QueueLock) -> rift_core::Result<()> {
    prepare_with(Admission::Ordinary(owner))
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "called by the CFX-4 recovery controller (eligibility → selection → retirement), not yet wired"
    )
)]
pub(crate) fn prepare_recovery(
    hold: &crate::capture_recovery::hold::OwnHold<'_>,
) -> rift_core::Result<()> {
    prepare_with(Admission::Recovery(hold))
}

fn prepare_with(admission: Admission<'_>) -> rift_core::Result<()> {
    admission.check()?;
    let git_dir = admission.owner().git_dir();
    if let Some(located) = locate(git_dir) {
        let ctx = MoveCtx {
            git_dir,
            run_root: &located.run_root,
            member: &located.member,
            now_ms: crate::hooks::now_ms(),
        };
        let report = adopt_with(&ctx, admission)
            .map_err(|_| crate::capture_queue::deferred("run-state migration is unresolved"))?;
        if report.busy {
            return Err(crate::capture_queue::deferred(
                "run-state work remains owed",
            ));
        }
        return Ok(());
    }
    let mut seen = Seen::default();
    for root in [RunRoot::AgentQueue, RunRoot::CaptureQueue] {
        let dir = root.legacy_under(git_dir);
        for path in queue_paths(&dir)
            .map_err(|_| crate::capture_queue::deferred("queue cannot be listed"))?
        {
            if path
                .extension()
                .is_some_and(|extension| extension == "stage" || extension == "tmp")
            {
                return Err(crate::capture_queue::deferred(
                    "queue publication remains in flight",
                ));
            }
            if path.extension().is_none_or(|extension| extension != "json") {
                continue;
            }
            let (value, stem) = queue_value(root, &path)
                .map_err(|_| crate::capture_queue::deferred("queue context is unreadable"))?;
            let versioned = crate::capture_queue::delivery::is_versioned(&path, &stem, &value)?;
            if seen.conflicts(root, stem.clone(), versioned, value.clone()) {
                return Err(crate::capture_queue::deferred("queue contexts conflict"));
            }
            if versioned {
                continue;
            }
            admission.check()?;
            let installed = crate::capture_queue::delivery::publish_exact(&dir, &stem, &value)
                .map_err(|_| crate::capture_queue::deferred("queue migration contexts conflict"))?;
            #[cfg(test)]
            tests::pause_migration(tests::MigrationPausePoint::QueuePublished);
            if installed != path {
                admission.check()?;
                std::fs::remove_file(path)?;
            }
        }
    }
    Ok(())
}

/// Move the member's old tree now: the worker before each drain, and the
/// doctor. Never a hook's. `None` when the member has no family to move into.
///
/// What it moved that an older binary wrote after the member was marked is
/// remembered for this process's doctor.
pub(crate) fn move_now(git_dir: &Path) -> Option<MoveReport> {
    let key = rift_git::family::canonical_or_lexical(git_dir);
    let cached = memo().lock().ok().and_then(|m| m.get(&key).cloned());
    let resolved = match cached {
        Some(resolved) => resolved,
        None => resolve(&key)?,
    };
    let marked = layout::is_moved(&resolved.member);
    let ctx = MoveCtx {
        git_dir: &key,
        run_root: &resolved.run_root,
        member: &resolved.member,
        now_ms: crate::hooks::now_ms(),
    };
    let report = match settle(&ctx, &mut std::thread::sleep) {
        Ok(report) => report,
        Err(e) => {
            tracing::warn!(error = %e, "run state not moved yet; the next pass retries");
            return None;
        }
    };
    if marked {
        if let Ok(mut m) = memo().lock() {
            if let Some(entry) = m.get_mut(&key) {
                entry.late_writes += report.moved + report.preserved;
            }
        }
    }
    Some(report)
}

/// [`adopt`], waiting for another mover's lock in [`MOVE_WAIT_STEPS_MS`]
/// steps while the member is unmarked, through the injected `sleep`.
///
/// After the wait the holder has either marked the member — and one more
/// pass takes whatever an older binary wrote meanwhile — or is still moving,
/// and this pass leaves it to them: readers stay correct either way, because
/// until the marker every read is the dual read and every write goes to the
/// new tree.
fn settle(ctx: &MoveCtx<'_>, sleep: &mut dyn FnMut(Duration)) -> std::io::Result<MoveReport> {
    let report = adopt(ctx)?;
    if !report.busy || layout::is_moved(ctx.member) {
        return Ok(report);
    }
    for step in MOVE_WAIT_STEPS_MS {
        sleep(Duration::from_millis(step));
        if layout::is_moved(ctx.member) {
            return adopt(ctx);
        }
    }
    Ok(report)
}

/// Resolve run state for a report on this thread until the guard drops.
pub(crate) fn report_only() -> ReportOnly {
    ReportOnly(REPORTING.with(|reporting| reporting.replace(true)))
}

impl Drop for ReportOnly {
    fn drop(&mut self) {
        REPORTING.with(|reporting| reporting.set(self.0));
    }
}

thread_local! {
    /// Whether a [`ReportOnly`] guard is held on this thread.
    static REPORTING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Resolve `root` for the member whose git dir is `git_dir`: under the
/// family's run directory, or in the git dir when no family can be resolved
/// (outside a repository, or before one is minted), which is then the
/// member's one home. Under [`ReportOnly`], [`report_dir`].
fn root_dir(git_dir: &Path, root: RunRoot) -> PathBuf {
    if REPORTING.with(std::cell::Cell::get) {
        return report_dir(git_dir, root);
    }
    match member_of(git_dir) {
        Some(member) => root.under(&member),
        None => root.legacy_under(git_dir),
    }
}

/// [`root_dir`] for a report: the member found through a family that is
/// never minted here, nothing recorded and no move journaled. A member a new
/// binary has recorded is read as every reader reads it, the new tree with
/// the dual read's fallback; an unrecorded one is read where its root stands,
/// the new tree when it holds one and the old tree otherwise, because the
/// fallback a recorded member gets is the record this resolver never writes.
fn report_dir(git_dir: &Path, root: RunRoot) -> PathBuf {
    let key = rift_git::family::canonical_or_lexical(git_dir);
    let Some(member) = minted_member(&key) else {
        return root.legacy_under(git_dir);
    };
    let current = root.under(&member);
    if current.is_dir() {
        return current;
    }
    layout::dir_for_read(root, &member, &key)
}

/// The member's run directory under an already-minted family, read without
/// minting, recording or journaling anything.
fn minted_member(git_dir: &Path) -> Option<PathBuf> {
    let work_tree = session_cache::work_tree_of(git_dir)?;
    let repo = rift_git::discover(&work_tree)
        .or_else(|_| rift_git::discover(git_dir))
        .ok()?;
    let Ok(rift_git::FamilyState::Minted(family)) =
        rift_git::resolve_family(&repo, rift_git::MintPolicy::Never)
    else {
        return None;
    };
    let family_dir = rift_store::projection_dir_under(&super::cache_root(), &family.family_id);
    Some(layout::member_dir(&family_dir, &work_tree))
}

/// Resolved members, by canonical git dir. Only successes are kept, so a
/// family minted later in a long-lived process is picked up.
fn memo() -> &'static Mutex<BTreeMap<PathBuf, Resolved>> {
    static MEMO: OnceLock<Mutex<BTreeMap<PathBuf, Resolved>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The member's run directory, for a hook as for every other caller: never
/// moves anything and never waits.
///
/// The family lookup is memoized; the old-tree check is not, because an older
/// binary may write there at any moment. While the member is unmoved and its
/// old tree holds anything, this journals the worker's move job, once per
/// process; readers meanwhile read through the dual read.
fn member_of(git_dir: &Path) -> Option<PathBuf> {
    // Canonical: a git hook may name its git dir relatively (`.git`), and a
    // linked member's common dir arrives as `…/worktrees/x/../..`.
    let key = rift_git::family::canonical_or_lexical(git_dir);
    let cached = memo().lock().ok().and_then(|m| m.get(&key).cloned());
    let resolved = match cached {
        Some(resolved) => resolved,
        None => resolve(&key)?,
    };
    if !resolved.move_owed && owes_move(&key, &resolved.member) {
        owe_move(&key, &resolved);
    }
    Some(resolved.member)
}

/// Whether the member still has run state in its old tree: one `stat` of
/// its marker and, before the move, one of each old directory.
fn owes_move(git_dir: &Path, member: &Path) -> bool {
    let pending = || {
        RunRoot::ALL
            .into_iter()
            .any(|root| root.legacy_under(git_dir).exists())
    };
    !layout::is_moved(member) && pending()
}

/// Journal the worker's move job in the member's own agent queue, and note
/// in the memo that this process has.
fn owe_move(git_dir: &Path, resolved: &Resolved) {
    let job = crate::agent_queue::AgentJob {
        schema_version: 1,
        work_tree: resolved.work_tree.clone(),
        queued_at_ms: crate::hooks::now_ms(),
        attempts: 0,
        kind: crate::agent_queue::JobKind::MoveRunState,
    };
    let queue = RunRoot::AgentQueue.under(&resolved.member);
    // The move is a repeatable request: a delivery another process already
    // journaled answers this one, whatever instant it carries.
    if crate::agent_queue::enqueue_in(&queue, &job).is_err() {
        return;
    }
    if let Ok(mut m) = memo().lock() {
        if let Some(entry) = m.get_mut(git_dir) {
            entry.move_owed = true;
        }
    }
}

/// Locate a member for the first time in this process, record its work tree
/// and git dir (what the dual read falls back through) and memoize it.
fn resolve(git_dir: &Path) -> Option<Resolved> {
    let Located {
        run_root,
        member,
        work_tree,
    } = locate(git_dir)?;
    layout::record_work_tree(&member, &work_tree).ok()?;
    layout::record_git_dir(&member, git_dir).ok()?;
    let resolved = Resolved {
        run_root,
        member,
        work_tree,
        move_owed: false,
        late_writes: 0,
    };
    if let Ok(mut m) = memo().lock() {
        m.insert(git_dir.to_path_buf(), resolved.clone());
    }
    Some(resolved)
}

/// Locate a member by canonical Git directory. Family resolution may mint an
/// identity; this helper does not record member metadata or publish move work.
fn locate(git_dir: &Path) -> Option<Located> {
    let work_tree = session_cache::work_tree_of(git_dir)?;
    // From the work tree first; from the git dir when the checkout is gone,
    // so a member whose checkout was removed still finds the run state it
    // left, which the doctor's sweep and drain still owe work to.
    let family = super::try_at(&work_tree).or_else(|| super::try_at(git_dir))?;
    let family_dir = rift_store::projection_dir_under(&super::cache_root(), &family.family_id);
    Some(Located {
        run_root: layout::run_root(&family_dir),
        member: layout::member_dir(&family_dir, &work_tree),
        work_tree,
    })
}

/// Write and sync the marker that tells readers to stop using the old tree.
/// Directory updates are not guaranteed durable across power loss.
fn write_marker(member: &Path, now_ms: i64) -> std::io::Result<()> {
    layout::create_private_dir(member)?;
    let mut file = std::fs::File::create(member.join(MOVED_MARKER))?;
    file.write_all(now_ms.to_string().as_bytes())?;
    file.sync_all()
}

/// Install without clobbering: a hard link on one volume, or a private synced
/// copy across volumes, then unlink the source. Both preserve mode. Directory
/// updates are not guaranteed durable across power loss.
fn move_file(
    source: &Path,
    target: &Path,
    check: &dyn Fn() -> std::io::Result<()>,
) -> std::io::Result<()> {
    check()?;
    match std::fs::hard_link(source, target) {
        Ok(()) => {
            #[cfg(test)]
            tests::pause_migration(tests::MigrationPausePoint::SessionPublished);
            check()?;
            ignore_not_found(std::fs::remove_file(source))
        }
        Err(error) if error.kind() == std::io::ErrorKind::CrossesDevices => {
            copy_across(source, target, check)
        }
        Err(error) => Err(error),
    }
}

fn copy_across(
    source: &Path,
    target: &Path,
    check: &dyn Fn() -> std::io::Result<()>,
) -> std::io::Result<()> {
    check()?;
    let parent = target
        .parent()
        .ok_or_else(|| std::io::Error::other("move has no parent"))?;
    for _ in 0..16 {
        let staging = parent.join(format!(
            ".moving-{}-{}.stage",
            std::process::id(),
            next_seq()
        ));
        let mut output = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&staging)
        {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };
        let result = (|| {
            let mut input = std::fs::File::open(source)?;
            output.set_permissions(input.metadata()?.permissions())?;
            std::io::copy(&mut input, &mut output)?;
            output.sync_all()?;
            check()?;
            std::fs::hard_link(&staging, target)?;
            #[cfg(test)]
            tests::pause_migration(tests::MigrationPausePoint::SessionPublished);
            check()?;
            ignore_not_found(std::fs::remove_file(source))
        })();
        drop(output);
        check()?;
        let _ = std::fs::remove_file(staging);
        return result;
    }
    Err(std::io::Error::other("move staging unavailable"))
}

/// A sequence that never repeats within this process, so two threads never
/// stage a lock under one name.
fn next_seq() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Whether a name is a write in flight: a writer's temp file or the capture
/// queue's drain lock.
fn is_in_flight(name: &str) -> bool {
    has_extension(name, "tmp") || name == QUEUE_LOCK
}

/// Whether a file name ends in `.<extension>`, exactly as the writers spell
/// it: every name in the run state is written lowercase by Rift itself.
fn has_extension(name: &str, extension: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|found| found == extension)
}

/// Whether two files hold the same bytes; unreadable reads as different, so
/// nothing is removed on a guess.
fn same_bytes(left: &Path, right: &Path) -> bool {
    match (std::fs::read(left), std::fs::read(right)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A late name beside `target` no file holds yet.
fn free_late_path(target: &Path, name: &str, now_ms: i64) -> PathBuf {
    let base = layout::late_name(name, now_ms);
    let mut candidate = target.with_file_name(&base);
    let mut n = 1_u32;
    while candidate.symlink_metadata().is_ok() {
        candidate = target.with_file_name(format!("{base}-{n}"));
        n += 1;
    }
    candidate
}

/// Every preserved late file under `dir`, recursively.
fn count_late(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => count_late(&entry.path()),
            Ok(_) if layout::is_late(&entry.file_name().to_string_lossy()) => 1,
            _ => 0,
        })
        .sum()
}

/// A file's mtime as epoch ms.
fn modified_ms(path: &Path) -> Option<i64> {
    let modified = std::fs::metadata(path).ok()?.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// Every file under `dir`, recursively, without following links.
fn count_files(dir: &Path) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => count_files(&entry.path()),
            Ok(_) => 1,
            Err(_) => 0,
        })
        .sum()
}

/// Whether a move failed because another mover took the source first. Not
/// only `NotFound`: APFS answers a rename whose source vanished under it with
/// `EINVAL`, so the source's absence is what is asked, not the error's kind.
fn taken_by_another(source: &Path) -> bool {
    source
        .symlink_metadata()
        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

/// `Ok` for a file that was already gone.
fn ignore_not_found(res: std::io::Result<()>) -> std::io::Result<()> {
    match res {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests;
