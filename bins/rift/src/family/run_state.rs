//! A family member's run state, out of its git dir: where it lives and the
//! one-time move that takes it there.
//!
//! The session caches and the two queues lived in `<git_dir>/rift-*`. They
//! now live under the family's cache directory,
//! `<cache_root>/rift/<family_id>/run/<member>/` (the layout is
//! [`rift_shadow::hooks::run_state`]'s), and every path this binary derives for
//! them goes through the three functions at the top of this module.
//!
//! **The move is safe under an older binary still writing the old tree.** An
//! installed hook binary is not rebuilt with this one, so for a while it keeps
//! writing `<git_dir>/rift-*` after the move. Every resolution therefore
//! checks the old tree — one `stat` of the marker and one of each old
//! directory — and moves whatever it finds. The move is
//! idempotent and resumable: a file present in both trees keeps the newer
//! session cache by its own `updated_at_ms`, the newer file by mtime
//! otherwise, the first claim marker for an ownership claim, and a queue's
//! jobs are unioned by their file name, which is the job's identity. Every
//! decision reads one tree: the move runs before the path is handed back.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use rift_shadow::hooks::run_state::{self as layout, RunRoot, MOVED_MARKER, MOVE_LOCK};
use rift_shadow::hooks::session_cache::{self, SessionCache};

/// How long a move lock may live before a later mover takes it over. A move
/// is a handful of renames, so a lock this old belongs to a mover that died.
const MOVE_LOCK_STALE_MS: i64 = 30_000;
/// The capture queue's drain lock inside its directory. Never moved: an old
/// binary may be draining under it.
const QUEUE_LOCK: &str = ".lock";

/// One member's resolution, memoized for the life of the process.
#[derive(Clone, Debug)]
struct Resolved {
    run_root: PathBuf,
    member: PathBuf,
    /// Files an older binary wrote to the old tree after the member was
    /// marked moved, as this process's move found them.
    late_writes: usize,
}

/// What one pass of [`adopt`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MoveReport {
    /// Files renamed or copied into the new tree.
    pub(crate) moved: usize,
    /// Files dropped because the new tree already held a newer copy.
    pub(crate) superseded: usize,
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
    /// Files this process moved that an older binary wrote after the move.
    pub(crate) late_writes: usize,
    /// Whether the member is marked moved.
    pub(crate) moved: bool,
}

impl Leftovers {
    /// Whether an older binary is still writing the old tree: something
    /// arrived there after the member was marked moved.
    pub(crate) fn old_binary_writing(&self) -> bool {
        self.moved && (self.files > 0 || self.late_writes > 0)
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

/// The merge rules of one root.
struct Merge {
    root: RunRoot,
    now_ms: i64,
}

/// The exclusive move lock, removed when dropped.
struct MoveLock {
    path: PathBuf,
}

impl MoveLock {
    /// Take the family's move lock, or `None` while another live mover holds
    /// it. A lock older than [`MOVE_LOCK_STALE_MS`] is taken over.
    ///
    /// The lock is published whole: its time is written to a file of this
    /// mover's own and then hard-linked into place, which fails when the lock
    /// exists — the exclusive create, with no window in which another mover
    /// can read a lock that names no time and has to guess its age.
    ///
    /// The lock keeps two movers from doing the work twice; it is not what
    /// makes the move safe. Every step tolerates a file another mover took
    /// first, so a lock taken over from a mover that was only slow costs
    /// duplicated stats, never a lost file.
    fn take(run_root: &Path, now_ms: i64) -> std::io::Result<Option<Self>> {
        layout::create_private_dir(run_root)?;
        let path = run_root.join(MOVE_LOCK);
        let staged = run_root.join(format!(
            "{MOVE_LOCK}.{}-{}.tmp",
            std::process::id(),
            next_seq()
        ));
        std::fs::write(&staged, now_ms.to_string())?;
        let taken = Self::link(&staged, &path, now_ms);
        let _ = std::fs::remove_file(&staged);
        taken
    }

    /// Link the staged lock into place, taking over a stale one once.
    fn link(staged: &Path, path: &Path, now_ms: i64) -> std::io::Result<Option<Self>> {
        for _ in 0..2 {
            match std::fs::hard_link(staged, path) {
                Ok(()) => {
                    return Ok(Some(Self {
                        path: path.to_path_buf(),
                    }))
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !lock_is_stale(path, now_ms) {
                        return Ok(None);
                    }
                    let _ = std::fs::remove_file(path);
                }
                Err(e) => return Err(e),
            }
        }
        Ok(None)
    }
}

impl Drop for MoveLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Merge {
    /// Merge every entry of `from` into `to`, recursing into directories (a
    /// session's subagent claims). Symbolic links are neither followed nor
    /// moved; the doctor counts them as leftovers.
    fn dir(&self, from: &Path, to: &Path, report: &mut MoveReport) -> std::io::Result<()> {
        let entries = match std::fs::read_dir(from) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(e),
        };
        layout::create_private_dir(to)?;
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            let source = entry.path();
            let target = to.join(entry.file_name());
            if kind.is_dir() {
                self.dir(&source, &target, report)?;
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
    /// removed, and a file the new tree already holds keeps the newer copy.
    fn file(
        &self,
        source: &Path,
        target: &Path,
        name: &str,
        report: &mut MoveReport,
    ) -> std::io::Result<()> {
        if is_in_flight(name) {
            let dead = modified_ms(source).is_some_and(|written| {
                self.now_ms.saturating_sub(written) >= rift_shadow::hooks::STALE_WRITER_MS
            });
            if dead {
                let _ = std::fs::remove_file(source);
            }
            return Ok(());
        }
        if target.exists() && !self.source_wins(source, target, name) {
            ignore_not_found(std::fs::remove_file(source))?;
            report.superseded += 1;
            return Ok(());
        }
        match move_file(source, target) {
            Ok(()) => report.moved += 1,
            // Another mover took it first.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
        Ok(())
    }

    /// Whether the old tree's copy replaces the new tree's.
    fn source_wins(&self, source: &Path, target: &Path, name: &str) -> bool {
        // An ownership claim is first-writer-wins; the one already standing
        // in the new tree is the first.
        if has_extension(name, "agent") {
            return false;
        }
        if self.root == RunRoot::Sessions && has_extension(name, "json") {
            return cache_updated_ms(source) > cache_updated_ms(target);
        }
        modified_ms(source) > modified_ms(target)
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

/// The member's post-commit capture queue.
pub(crate) fn capture_queue_dir(git_dir: &Path) -> PathBuf {
    root_dir(git_dir, RunRoot::CaptureQueue)
}

/// The old tree of a member, as the doctor reports it; `None` when the
/// member has no family to move into, which leaves the git dir its home.
pub(crate) fn leftovers(git_dir: &Path) -> Option<Leftovers> {
    let key = rift_git::family::canonical_or_lexical(git_dir);
    let member = locate(&key)?.member;
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
    let marked = layout::is_moved(ctx.member);
    let pending: Vec<RunRoot> = RunRoot::ALL
        .into_iter()
        .filter(|root| root.legacy_under(ctx.git_dir).exists())
        .collect();
    if marked && pending.is_empty() {
        return Ok(MoveReport::default());
    }
    let Some(_lock) = MoveLock::take(ctx.run_root, ctx.now_ms)? else {
        return Ok(MoveReport {
            busy: true,
            ..MoveReport::default()
        });
    };
    let mut report = MoveReport::default();
    for root in pending {
        let from = root.legacy_under(ctx.git_dir);
        let merge = Merge {
            root,
            now_ms: ctx.now_ms,
        };
        merge.dir(&from, &root.under(ctx.member), &mut report)?;
        // Removed only when empty: a file left in flight keeps it.
        let _ = std::fs::remove_dir(&from);
    }
    if !marked {
        write_marker(ctx.member, ctx.now_ms)?;
    }
    Ok(report)
}

/// Resolve `root` for the member whose git dir is `git_dir`: under the
/// family's run directory, or in the git dir when no family can be resolved
/// (outside a repository, or before one is minted), which is then the
/// member's one home.
fn root_dir(git_dir: &Path, root: RunRoot) -> PathBuf {
    match member_of(git_dir) {
        Some(member) => root.under(&member),
        None => root.legacy_under(git_dir),
    }
}

/// Resolved members, by canonical git dir. Only successes are kept, so a
/// family minted later in a long-lived process is picked up.
fn memo() -> &'static Mutex<BTreeMap<PathBuf, Resolved>> {
    static MEMO: OnceLock<Mutex<BTreeMap<PathBuf, Resolved>>> = OnceLock::new();
    MEMO.get_or_init(|| Mutex::new(BTreeMap::new()))
}

/// The member's run directory, after moving whatever its old tree holds.
///
/// The family lookup is memoized; the old-tree check is not, because an
/// older binary may write there at any moment, and the check is what moves
/// that write before this resolution's caller reads.
fn member_of(git_dir: &Path) -> Option<PathBuf> {
    // Canonical: a git hook may name its git dir relatively (`.git`), and a
    // linked member's common dir arrives as `…/worktrees/x/../..`.
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
    let late_writes = match adopt(&ctx) {
        Ok(report) if marked => report.moved,
        Ok(_) => 0,
        Err(e) => {
            tracing::warn!(error = %e, "run state not moved yet; the next resolution retries");
            0
        }
    };
    if let Ok(mut m) = memo().lock() {
        if let Some(entry) = m.get_mut(&key) {
            entry.late_writes += late_writes;
        }
    }
    Some(resolved.member)
}

/// Locate a member for the first time in this process, record its work tree
/// and memoize it.
fn resolve(git_dir: &Path) -> Option<Resolved> {
    let Located {
        run_root,
        member,
        work_tree,
    } = locate(git_dir)?;
    layout::record_work_tree(&member, &work_tree).ok()?;
    let resolved = Resolved {
        run_root,
        member,
        late_writes: 0,
    };
    if let Ok(mut m) = memo().lock() {
        m.insert(git_dir.to_path_buf(), resolved.clone());
    }
    Some(resolved)
}

/// Locate a member by its canonical git dir. Creates nothing.
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

/// Write the member's moved marker durably: it is what tells a reader to
/// stop looking at the old tree.
fn write_marker(member: &Path, now_ms: i64) -> std::io::Result<()> {
    layout::create_private_dir(member)?;
    let mut file = std::fs::File::create(member.join(MOVED_MARKER))?;
    file.write_all(now_ms.to_string().as_bytes())?;
    file.sync_all()
}

/// Move one file: a rename on one volume, and across volumes a copy that is
/// synced and renamed into place before the source is unlinked, so the target
/// is never seen half written and the source goes only once the copy is
/// durable. Both keep the file's mode.
fn move_file(source: &Path, target: &Path) -> std::io::Result<()> {
    match std::fs::rename(source, target) {
        Err(e) if e.kind() == std::io::ErrorKind::CrossesDevices => copy_across(source, target),
        other => other,
    }
}

/// The cross-volume arm of [`move_file`].
fn copy_across(source: &Path, target: &Path) -> std::io::Result<()> {
    let name = target.file_name().unwrap_or_default().to_string_lossy();
    let staging = target.with_file_name(format!(".{name}.moving.tmp"));
    let copied = std::fs::copy(source, &staging)
        .and_then(|_| std::fs::File::open(&staging)?.sync_all())
        .and_then(|()| std::fs::rename(&staging, target));
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&staging);
        return Err(e);
    }
    ignore_not_found(std::fs::remove_file(source))
}

/// Whether a lock's holder is presumed dead: the time it records is
/// [`MOVE_LOCK_STALE_MS`] old. A lock that records no time was not written by
/// a mover (every mover publishes it whole) and is taken over.
fn lock_is_stale(path: &Path, now_ms: i64) -> bool {
    let taken = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok());
    match taken {
        Some(taken) => now_ms.saturating_sub(taken) >= MOVE_LOCK_STALE_MS,
        None => true,
    }
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

/// A session cache's own `updated_at_ms`, or its mtime when it does not
/// parse.
fn cache_updated_ms(path: &Path) -> Option<i64> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<SessionCache>(&bytes).ok())
        .map(|cache| cache.updated_at_ms)
        .or_else(|| modified_ms(path))
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

/// `Ok` for a file that was already gone.
fn ignore_not_found(res: std::io::Result<()>) -> std::io::Result<()> {
    match res {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

#[cfg(test)]
mod tests;
