//! The push queue and its consumer trait.
//!
//! Capture writes refs locally. Getting those refs to somewhere else is a
//! *separate*, deferrable, retryable job — so a checkpoint write enqueues the
//! ref it moved, and a **shipper** drains the queue later: on demand
//! (`rift sync flush`) or on `pre-push`, when the developer is already talking
//! to the remote anyway.
//!
//! # There is no default destination, and that is the point
//!
//! The sync design describes the shipper's target as
//! "origin / `--checkpoint-remote`". **This implementation deliberately drops
//! the `origin` half**, and the reason is evidence rather than caution: on the
//! machine this was written on, `origin` in this very repository resolves to an
//! unrelated third party's project, and rift history has already reached it by
//! accident more than once. `origin` is not "this project's home"; it is
//! whatever the developer happened to clone. A shipper that fell back to it
//! would publish a capture ledger — session prompts, file paths, cost facts,
//! redacted or not — into someone else's repository, and it would do so
//! silently, as a side effect of a `git push` the developer meant for their own
//! code.
//!
//! So: the checkpoint remote must be **named** ([`rift_config::checkpoint_remote`],
//! `.rift/config.toml` `[checkpoint] remote`). Absent it the queue **accrues**
//! — which loses nothing, because a queue entry is a ref name and the refs are
//! already durable in git — and `rift sync flush` refuses with an error that
//! says what to configure. [`ShipTarget::resolve`] is the only place a
//! destination is decided, and it has no fallback arm to remove.
//!
//! # Never blocks
//!
//! The `pre-push` drain runs inside a git hook, so it obeys the hook contract:
//! a failure degrades to "still queued", never to a failed push. The drain
//! reports; it does not decide whether git proceeds.
//!
//! # Crash-safety, and why the drain renames before it ships
//!
//! A drain claims work by renaming `queue.jsonl` aside to a
//! `draining-<now_ms>-<pid>.jsonl` file under the queue lock, then ships, then
//! deletes the claimed file. The claim name carries BOTH the stamp and the
//! claiming process id (see [`claim`]) because either alone collides: two
//! drains in the same millisecond, or one process draining twice. The rename is
//! what makes it crash-safe in both directions:
//!
//! * a crash **after** the rename and **before** the ship leaves a
//!   `draining-*` file, which the next drain picks up along with the live queue
//!   — nothing is lost;
//! * a crash **after** the ship and **before** the delete leaves the same file,
//!   so the next drain ships those refs again — and shipping a ref that is
//!   already there is a git no-op, so re-draining *converges* rather than
//!   duplicating.
//!
//! Holding the lock only across the rename (never across the ship) is what
//! keeps a slow or hanging remote from blocking a checkpoint write.
//!
//! # Where the queue lives: the family's common dir, not a checkout's gitdir
//!
//! A queue entry names a **ref**, and refs live in the object/ref store that
//! every worktree of a repository shares. Keying the queue by `<git_dir>`
//! therefore split one family's shipping work across as many queues as it had
//! checkouts, and every consequence of that was a loss: a `pre-push` from a
//! linked worktree read its own admin dir, found nothing and shipped nothing,
//! while the main checkout's queue accrued unreachably — 62 entries on the
//! machine this was written on, with neither linked worktree holding a queue
//! at all (RESEARCH §6) — and `git worktree remove` deleted a member's queue
//! outright, refs and all. So the queue is `<common_dir>/rift-push-queue`
//! one family, one queue, drainable from any member.
//!
//! Nothing else moves with it. The capture and agent-hook queues stay under
//! each member's admin dir, because what they describe is one
//! checkout's private progress — the credit discriminator — while what this
//! queue describes is a ref the whole family already shares.
//!
//! A queue an older binary left under an admin dir is **adopted, not
//! stranded**: [`drain_legacy`] re-enqueues its entries into the common queue
//! — idempotent by `(ref_name, chain_seq)`, exactly as [`enqueue`] is — and
//! removes the directory it emptied, so the adoption happens once.

use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

/// Contract version for a queue entry line.
pub(crate) const PUSH_QUEUE_SCHEMA_VERSION: u32 = 1;

/// The queue directory under `<common_dir>` — a **family** artifact that
/// names git refs, unlike the capture and agent queues, which stay per member
/// in the family's run state.
const QUEUE_DIR: &str = "rift-push-queue";
/// The live queue: one JSON object per line, append-only until a drain claims it.
const QUEUE_FILE: &str = "queue.jsonl";
/// Prefix for a drain's claimed batch.
const CLAIM_PREFIX: &str = "draining-";
/// The mutual-exclusion file guarding *claims* (never a ship).
const LOCK_FILE: &str = ".lock";
/// How long a lock may live before a later worker steals it. The lock is only
/// ever held across a rename, so anything this old is a dead process.
const LOCK_STALE_MS: i64 = 60 * 1000;

/// One queued ref: "this ref moved, it has not been shipped yet".
///
/// It carries **no capture bytes** — not raw and not redacted. A ref name and a
/// stream position are enough, because everything a shipper needs is already in
/// git; the same reasoning that keeps the capture queue byte-free
/// (`RedactedBytes` has one constructor and a file could never feed it) applies
/// here with nothing to weigh against it.
///
/// Unknown fields are **ignored** rather than rejected: a newer writer's entry
/// must still be drainable by an older shipper, and the whole job is "push this
/// ref", which cannot stop being expressible.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct PushEntry {
    /// Contract schema version.
    pub schema_version: u32,
    /// The fully-qualified ref that moved (e.g. `refs/rift/chain/v1`).
    pub ref_name: String,
    /// The stream position this enqueue was caused by (`chain_seq`), or 0 when
    /// the ref is not the checkpoint stream's own.
    #[serde(default)]
    pub chain_seq: u64,
    /// Transaction time.
    pub enqueued_at_ms: i64,
}

/// Where a drain would ship to — the resolved, explicitly configured remote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShipTarget {
    /// The configured remote token (a git remote name, or a URL).
    pub name: String,
    /// The URL git resolves that token to, when it is a configured remote name.
    /// A token that is already a URL resolves to itself.
    pub url: String,
}

impl ShipTarget {
    /// Resolve the repository's checkpoint remote, or `None` when none is
    /// configured.
    ///
    /// **The only place a destination is decided.** There is no `origin` arm
    /// and no environment fallback: not configured means not shipped.
    ///
    /// # Errors
    /// [`RiftError::Config`] when `[checkpoint] remote` is present but unusable.
    pub(crate) fn resolve(repo: &rift_git::RepoPaths) -> Result<Option<Self>> {
        let Some(name) = rift_config::checkpoint_remote(&repo.work_tree)? else {
            return Ok(None);
        };
        let url = remote_url(repo, &name).unwrap_or_else(|| name.clone());
        Ok(Some(Self { name, url }))
    }

    /// Whether a `pre-push` invocation naming `(remote, url)` is a push **to
    /// this target**.
    ///
    /// Identity is both halves, compared byte-for-byte, and deliberately not
    /// normalized. Two spellings of one URL (`ssh://git@host/x` vs
    /// `git@host:x`, a trailing `.git`, a trailing slash) are treated as
    /// different remotes here, which errs toward *not* shipping — the queue
    /// simply accrues and `rift sync flush` ships it deliberately. A normalizer
    /// is where two genuinely different remotes get to become one, and this is
    /// the comparison that decides whether a capture ledger leaves the machine.
    #[must_use]
    pub(crate) fn matches_push(&self, remote: &str, url: &str) -> bool {
        !remote.is_empty() && remote == self.name && !url.is_empty() && url == self.url
    }
}

/// `git remote get-url <name>`, or `None` when the token is not a configured
/// remote (in which case git treats it as a URL and so do we).
fn remote_url(repo: &rift_git::RepoPaths, name: &str) -> Option<String> {
    let args = vec![
        "remote".to_owned(),
        "get-url".to_owned(),
        "--".to_owned(),
        name.to_owned(),
    ];
    let (ok, out) = rift_git::run_git_status(repo, &args, &[], &[]).ok()?;
    if !ok {
        return None;
    }
    let text = String::from_utf8_lossy(&out).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// A queue consumer: something that can get refs to a destination.
///
/// The queue does not know what a destination is. Today the only implementation
/// is [`GitPushShipper`]; a future hosted shipper implements this trait and the
/// queue does not change — which is the whole reason the trait exists rather
/// than the drain calling `git push` directly.
pub(crate) trait Shipper {
    /// Ship `refs` (fully-qualified, de-duplicated, in a stable order).
    ///
    /// Must be **idempotent**: shipping a ref already at the destination is a
    /// success, because a re-drain after a crash will do exactly that.
    ///
    /// # Errors
    /// Any failure leaves the batch unshipped; the caller re-queues it whole.
    fn ship(&self, refs: &[String]) -> Result<()>;

    /// How to name this destination in output. Never a credential.
    fn describe(&self) -> String;
}

/// The one shipper in this program: `git push` to an explicitly configured
/// checkpoint remote.
pub(crate) struct GitPushShipper<'a> {
    repo: &'a rift_git::RepoPaths,
    target: ShipTarget,
}

impl<'a> GitPushShipper<'a> {
    /// A shipper aimed at `target`. Constructing one requires a resolved
    /// target, so there is no way to build an unaimed shipper.
    pub(crate) fn new(repo: &'a rift_git::RepoPaths, target: ShipTarget) -> Self {
        Self { repo, target }
    }
}

impl Shipper for GitPushShipper<'_> {
    fn ship(&self, refs: &[String]) -> Result<()> {
        if refs.is_empty() {
            return Ok(());
        }
        // `--no-verify`: this push IS the sanctioned egress of `rift/*` refs, to
        // the one remote the repository named for them. The `pre-push` screen
        // exists to stop those refs riding a *developer's* push to an arbitrary
        // remote, and it would otherwise reject this one too. Everything else
        // stays on: no force, no tags, no mirror — an append-only ref history
        // fast-forwards or the push fails.
        let mut args = vec![
            "push".to_owned(),
            "--no-verify".to_owned(),
            "--no-tags".to_owned(),
            "--".to_owned(),
            self.target.name.clone(),
        ];
        for r in refs {
            rift_git::validate_refspec(r)?;
            args.push(format!("{r}:{r}"));
        }
        let (ok, _) = rift_git::run_git_status(self.repo, &args, &[], &[])?;
        if ok {
            Ok(())
        } else {
            Err(RiftError::storage(format!(
                "push to the checkpoint remote '{}' did not succeed",
                self.target.name
            )))
        }
    }

    fn describe(&self) -> String {
        self.target.name.clone()
    }
}

/// The family's queue directory, from any spelling of its common dir.
///
/// **Canonicalized**, because discovery hands a linked worktree the *lexical*
/// `…/worktrees/<name>/../..` form (`rift_git`'s `commondir` read): the two
/// spellings name one directory, so the queue was already shared without this,
/// but a doctor line or a path comparison built on the lexical form names a
/// directory nobody recognises. A common dir that cannot be canonicalized (it
/// went away under us) keeps its given spelling — an unusable path is still a
/// truer answer than a panic.
pub(crate) fn queue_dir(common_dir: &Path) -> PathBuf {
    std::fs::canonicalize(common_dir)
        .unwrap_or_else(|_| common_dir.to_path_buf())
        .join(QUEUE_DIR)
}

/// Append one entry to the live queue.
///
/// **Idempotent by value against everything still queued**: an entry naming a
/// `(ref_name, chain_seq)` that [`pending`] would report — the live file *or* a
/// batch some drain has claimed and not yet finished — is not written twice, so
/// a retried checkpoint write does not grow the queue. Best-effort by design:
/// this runs beside a checkpoint write, and a queue that could fail a capture
/// would be worse than a queue that misses an entry the next checkpoint re-adds.
///
/// # Why the claimed batches count, when for one release they did not
///
/// The de-duplication read only `queue.jsonl`, while [`pending`] counted the
/// claimed `draining-*` batches too — so enqueue, claim, re-enqueue the same
/// pair and `pending()` measured **2** where this docstring said 1. That is a
/// docstring making a claim the code did not keep, which is the defect whatever
/// the downstream cost (here: small — a drain collapses a batch to its distinct
/// ref names, and shipping a ref already at the destination is a git no-op).
///
/// Suppressing against a claimed batch is **safe**, and the argument is the
/// claim protocol rather than timing: a `draining-*` file exists only between
/// [`claim`]'s rename and the delete that follows a *successful* ship, so while
/// one is on disk that batch's refs are still going to be shipped — by this
/// drain, by the retry after a refusal, or by the next drain that adopts the
/// file after a crash. And a `(ref_name, chain_seq)` names one chain position,
/// which one ref state satisfies: shipping `ref_name` once at or after that
/// position discharges every entry naming it.
///
/// # Errors
/// [`RiftError::Io`] if the queue directory or file cannot be written.
pub(crate) fn enqueue(common_dir: &Path, entry: &PushEntry) -> Result<()> {
    if entry.ref_name.is_empty() {
        return Err(RiftError::validity("push queue: empty ref name"));
    }
    rift_git::validate_refspec(&entry.ref_name)?;
    let dir = queue_dir(common_dir);
    std::fs::create_dir_all(&dir)?;
    let _lock = acquire_lock(common_dir, entry.enqueued_at_ms);
    let path = dir.join(QUEUE_FILE);
    // `pending`, not `read_entries(&path)` — one reader for "what is still
    // queued", so the de-duplication and the count can never disagree again.
    if pending(common_dir)
        .iter()
        .any(|e| e.ref_name == entry.ref_name && e.chain_seq == entry.chain_seq)
    {
        return Ok(());
    }
    let mut line = serde_json::to_string(entry).map_err(|e| RiftError::Serde(e.to_string()))?;
    line.push('\n');
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// Every entry currently queued (live file plus any batch a previous drain
/// claimed and did not finish), oldest first.
pub(crate) fn pending(common_dir: &Path) -> Vec<PushEntry> {
    let mut all = Vec::new();
    for path in queue_files(common_dir) {
        all.extend(read_entries(&path));
    }
    all
}

/// The number of distinct refs waiting to be shipped.
pub(crate) fn pending_refs(common_dir: &Path) -> Vec<String> {
    distinct_refs(&pending(common_dir))
}

/// Outcome of a drain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DrainReport {
    /// Refs handed to the shipper (empty when there was nothing to do).
    pub shipped: Vec<String>,
    /// Entries removed from the queue.
    pub entries: usize,
}

/// Claim everything queued, ship it, and drop the claim — see the module docs
/// for why this order is the crash-safe one.
///
/// Returns the refs shipped. On a shipper failure the claim is **kept on disk**
/// (renamed, not deleted), so the next drain retries the same batch.
///
/// # Errors
/// Propagates the shipper's error after the batch has been left queued.
pub(crate) fn drain(common_dir: &Path, shipper: &impl Shipper, now_ms: i64) -> Result<DrainReport> {
    let claimed = claim(common_dir, now_ms);
    if claimed.is_empty() {
        return Ok(DrainReport {
            shipped: Vec::new(),
            entries: 0,
        });
    }
    let mut entries = Vec::new();
    for path in &claimed {
        entries.extend(read_entries(path));
    }
    let refs = distinct_refs(&entries);
    shipper.ship(&refs)?;
    // Only now: the work is done, so the claim may go.
    for path in &claimed {
        let _ = std::fs::remove_file(path);
    }
    Ok(DrainReport {
        shipped: refs,
        entries: entries.len(),
    })
}

/// Rename the live queue aside (and adopt any orphaned claim from a crashed
/// drain). Under the lock, because two drains must not claim the same batch.
fn claim(common_dir: &Path, now_ms: i64) -> Vec<PathBuf> {
    let dir = queue_dir(common_dir);
    let _lock = acquire_lock(common_dir, now_ms);
    let live = dir.join(QUEUE_FILE);
    if live.exists() {
        let mine = dir.join(format!(
            "{CLAIM_PREFIX}{now_ms}-{}.jsonl",
            std::process::id()
        ));
        let _ = std::fs::rename(&live, &mine);
    }
    let mut claims: Vec<PathBuf> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(CLAIM_PREFIX))
        })
        .collect();
    claims.sort();
    claims
}

/// Every file a drain would read: claims first (oldest work first), then live.
fn queue_files(common_dir: &Path) -> Vec<PathBuf> {
    queue_files_in(&queue_dir(common_dir))
}

/// The same, for a queue **directory** named outright — the one shape
/// [`drain_legacy`] needs, because an abandoned admin-dir queue is a queue
/// whose owning common dir is not the one being drained into.
fn queue_files_in(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(CLAIM_PREFIX))
        })
        .collect();
    files.sort();
    let live = dir.join(QUEUE_FILE);
    if live.exists() {
        files.push(live);
    }
    files
}

// --- adopting the queues an older binary left per gitdir --------------------

/// What one legacy adoption pass found and moved.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LegacyDrain {
    /// The admin-dir queues found (the common queue is never one of them).
    pub dirs: Vec<PathBuf>,
    /// Entries carried into the common queue. A pair already queued there
    /// counts once and is not written twice — the adoption is idempotent by
    /// `(ref_name, chain_seq)` because [`enqueue`] is.
    pub adopted: usize,
    /// Entries the queue's own validation refuses (a ref name that is not one).
    /// They could never have been shipped from where they were either, so they
    /// are reported rather than allowed to pin a directory open forever.
    pub refused: usize,
    /// Directories that were emptied but could not be removed. The next pass
    /// re-reads them, suppresses every entry as a duplicate, and tries again.
    pub kept: Vec<PathBuf>,
}

impl LegacyDrain {
    /// Whether this pass found anything at all — the report's one predicate,
    /// so a caller never has to decide what "nothing happened" looks like.
    pub(crate) fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }
}

/// Move every admin-dir queue into the family's common queue, once.
///
/// `admin_dirs` are the gitdirs of the family's members ([`adopt_legacy`]
/// supplies them); a member whose queue directory **is** the common queue is
/// skipped, which is every main work tree, whose gitdir is the common dir.
///
/// Removal after a full adoption is what makes it happen once; the
/// `(ref_name, chain_seq)` de-duplication is what makes a *failed* removal
/// harmless rather than a doubling. Neither can fail a caller: this runs on a
/// `pre-push` path, where the contract is that a queue never breaks a push.
pub(crate) fn drain_legacy(common_dir: &Path, admin_dirs: &[PathBuf]) -> LegacyDrain {
    let mut report = LegacyDrain::default();
    for legacy in legacy_queue_dirs(common_dir, admin_dirs) {
        report.dirs.push(legacy.clone());
        for entry in queue_files_in(&legacy).iter().flat_map(|p| read_entries(p)) {
            if enqueue(common_dir, &entry).is_ok() {
                report.adopted += 1;
            } else {
                report.refused += 1;
            }
        }
        if std::fs::remove_dir_all(&legacy).is_err() {
            report.kept.push(legacy);
        }
    }
    report
}

/// The admin-dir queues among `admin_dirs` that are not the common queue
/// itself. A main work tree's gitdir **is** the common dir, so without this
/// filter an adoption would move the family's queue into itself and then
/// delete it.
fn legacy_queue_dirs(common_dir: &Path, admin_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let target = queue_dir(common_dir);
    admin_dirs
        .iter()
        .map(|admin| admin.join(QUEUE_DIR))
        .filter(|dir| dir.is_dir() && !same_dir(dir, &target))
        .collect()
}

/// Every member's gitdir, the prunable ones included — a checkout that is gone
/// is exactly the case whose queue nothing else will ever reach.
fn member_git_dirs(repo: &rift_git::RepoPaths) -> Vec<PathBuf> {
    rift_git::family_members(repo)
        .into_iter()
        .map(|m| m.git_dir)
        .collect()
}

/// [`drain_legacy`] over the family `repo` belongs to.
pub(crate) fn adopt_legacy(repo: &rift_git::RepoPaths) -> LegacyDrain {
    drain_legacy(&repo.common_dir, &member_git_dirs(repo))
}

/// The legacy queues the family still carries, **found without moving
/// anything** — what `rift doctor` reports before `--fix` is asked for.
pub(crate) fn legacy_dirs(repo: &rift_git::RepoPaths) -> Vec<PathBuf> {
    legacy_queue_dirs(&repo.common_dir, &member_git_dirs(repo))
}

/// Whether two paths name one directory. Canonical where both resolve (the
/// lexical `…/worktrees/x/../..` a member carries resolves to the common dir),
/// literal where they do not.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// Parse one queue file. A line that does not parse is **skipped**, never fatal:
/// one corrupt line must not strand every other ref in the file.
fn read_entries(path: &Path) -> Vec<PushEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<PushEntry>(l).ok())
        .filter(|e| !e.ref_name.is_empty())
        .collect()
}

/// The distinct refs in `entries`, in first-seen order — the shipper's argument.
fn distinct_refs(entries: &[PushEntry]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for e in entries {
        if !out.contains(&e.ref_name) {
            out.push(e.ref_name.clone());
        }
    }
    out
}

/// The queue lock, released on drop.
pub(crate) struct QueueLock {
    path: PathBuf,
}

impl Drop for QueueLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// How many times a caller retries the lock before giving up on it. The lock is
/// only ever held across an append or a rename, so contention clears in
/// microseconds; the budget exists so a dead holder cannot stall a caller, not
/// because waiting is expected.
const LOCK_TRIES: u32 = 200;
/// Pause between lock attempts.
const LOCK_RETRY_MS: u64 = 2;

/// Take the queue lock, stealing one older than [`LOCK_STALE_MS`].
///
/// `None` means the lock could not be had within [`LOCK_TRIES`]; every caller
/// here proceeds anyway, because the lock is a *serializer*, not a correctness
/// guarantee — losing it costs at worst a duplicate line, which `distinct_refs`
/// collapses, and stalling on it would put a hook behind a dead process.
fn acquire_lock(common_dir: &Path, now_ms: i64) -> Option<QueueLock> {
    let dir = queue_dir(common_dir);
    let path = dir.join(LOCK_FILE);
    for _ in 0..LOCK_TRIES {
        if std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .is_ok()
        {
            return Some(QueueLock { path });
        }
        std::thread::sleep(std::time::Duration::from_millis(LOCK_RETRY_MS));
    }
    let stale = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| {
            i64::try_from(age.as_millis()).unwrap_or(i64::MAX) > LOCK_STALE_MS || now_ms < 0
        });
    if stale {
        let _ = std::fs::remove_file(&path);
        if std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
            .is_ok()
        {
            return Some(QueueLock { path });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shipper that records what it was asked to ship, and can be told to
    /// fail — the control for "a failed ship leaves the batch queued".
    struct Recorder {
        calls: std::cell::RefCell<Vec<Vec<String>>>,
        fail: bool,
    }

    impl Recorder {
        fn new(fail: bool) -> Self {
            Self {
                calls: std::cell::RefCell::new(Vec::new()),
                fail,
            }
        }
    }

    impl Shipper for Recorder {
        fn ship(&self, refs: &[String]) -> Result<()> {
            self.calls.borrow_mut().push(refs.to_vec());
            if self.fail {
                Err(RiftError::storage("test shipper refuses"))
            } else {
                Ok(())
            }
        }
        fn describe(&self) -> String {
            "recorder".to_owned()
        }
    }

    fn entry(name: &str, seq: u64) -> PushEntry {
        PushEntry {
            schema_version: PUSH_QUEUE_SCHEMA_VERSION,
            ref_name: name.to_owned(),
            chain_seq: seq,
            enqueued_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn an_entry_queued_twice_is_queued_once() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        enqueue(gd, &entry("refs/rift/chain/v1", 3)).unwrap();
        enqueue(gd, &entry("refs/rift/chain/v1", 3)).unwrap();
        assert_eq!(pending(gd).len(), 1);
        // A different position is different work.
        enqueue(gd, &entry("refs/rift/chain/v1", 4)).unwrap();
        assert_eq!(pending(gd).len(), 2);
        // …but it is still one ref to push.
        assert_eq!(pending_refs(gd), vec!["refs/rift/chain/v1".to_owned()]);
    }

    #[test]
    fn a_drain_ships_each_ref_once_and_empties_the_queue() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        enqueue(gd, &entry("refs/rift/chain/v1", 1)).unwrap();
        enqueue(gd, &entry("refs/heads/rift/checkpoints/v1", 1)).unwrap();
        enqueue(gd, &entry("refs/rift/chain/v1", 2)).unwrap();
        let ship = Recorder::new(false);
        let report = drain(gd, &ship, 1).unwrap();
        assert_eq!(
            report.shipped,
            vec![
                "refs/rift/chain/v1".to_owned(),
                "refs/heads/rift/checkpoints/v1".to_owned()
            ]
        );
        assert_eq!(report.entries, 3);
        assert!(pending(gd).is_empty(), "a shipped batch leaves the queue");
        // A second drain with nothing queued ships nothing at all.
        let again = drain(gd, &ship, 2).unwrap();
        assert!(again.shipped.is_empty());
        assert_eq!(ship.calls.borrow().len(), 1, "no empty ship call");
    }

    /// The failure direction: a shipper that refuses must leave the work on
    /// disk, and the retry must see the same batch.
    #[test]
    fn a_refused_ship_leaves_the_batch_queued_and_the_retry_converges() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        enqueue(gd, &entry("refs/rift/chain/v1", 1)).unwrap();
        let bad = Recorder::new(true);
        assert!(drain(gd, &bad, 1).is_err());
        assert_eq!(
            pending_refs(gd),
            vec!["refs/rift/chain/v1".to_owned()],
            "a refused ship must not consume the queue"
        );
        let good = Recorder::new(false);
        let report = drain(gd, &good, 2).unwrap();
        assert_eq!(report.shipped, vec!["refs/rift/chain/v1".to_owned()]);
        assert!(pending(gd).is_empty());
    }

    /// The crash the module docs describe: a claim exists on disk and the
    /// process that made it is gone. The next drain must adopt it rather than
    /// leave those refs stranded forever.
    #[test]
    fn a_claim_orphaned_by_a_crash_is_adopted_by_the_next_drain() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        enqueue(gd, &entry("refs/rift/chain/v1", 1)).unwrap();
        // Simulate the crash window exactly: the claim rename happened, the
        // ship did not, and the process died holding nothing else.
        let dir = queue_dir(gd);
        std::fs::rename(
            dir.join(QUEUE_FILE),
            dir.join(format!("{CLAIM_PREFIX}1-999.jsonl")),
        )
        .unwrap();
        assert_eq!(
            pending_refs(gd),
            vec!["refs/rift/chain/v1".to_owned()],
            "an orphaned claim is still pending work"
        );
        let ship = Recorder::new(false);
        let report = drain(gd, &ship, 5).unwrap();
        assert_eq!(report.shipped, vec!["refs/rift/chain/v1".to_owned()]);
        assert!(pending(gd).is_empty());
    }

    /// Entries that arrive while a drain is in flight belong to the *next*
    /// drain, and must survive this one — the claim rename is what buys that.
    #[test]
    fn an_entry_enqueued_during_a_drain_survives_it() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        enqueue(gd, &entry("refs/rift/chain/v1", 1)).unwrap();
        let claimed = claim(gd, 10);
        assert_eq!(claimed.len(), 1);
        // The window: the batch is claimed, the ship has not happened yet.
        enqueue(gd, &entry("refs/heads/rift/checkpoints/v1", 2)).unwrap();
        for p in &claimed {
            let _ = std::fs::remove_file(p);
        }
        assert_eq!(
            pending_refs(gd),
            vec!["refs/heads/rift/checkpoints/v1".to_owned()],
            "the late entry must still be queued after the drain completes"
        );
    }

    /// **De-duplication covers exactly what `pending` counts** — the claimed
    /// batches included.
    ///
    /// Measured before it was fixed, by a verifier and then again here: enqueue
    /// `(R, 7)`, let a drain claim it, enqueue `(R, 7)` again, and `pending()`
    /// reported **2** while `enqueue`'s docstring said one entry. The sentence
    /// and the bytes disagreed, and this is the test that stops them drifting
    /// apart again — narrow the read back to `queue.jsonl` and it reds here.
    ///
    /// The re-enqueue is not a lost push: the claim on disk still names `R`, so
    /// the drain that owns it — or the retry, or the adopter after a crash —
    /// ships `R` regardless, which the second half asserts.
    #[test]
    fn an_entry_duplicated_across_a_claimed_batch_is_still_queued_once() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        enqueue(gd, &entry("refs/rift/chain/v1", 7)).unwrap();
        let claimed = claim(gd, 10);
        assert_eq!(claimed.len(), 1, "the batch must be claimed, not shipped");
        assert!(
            !queue_dir(gd).join(QUEUE_FILE).exists(),
            "the live file is gone: the narrow reader saw an empty queue here"
        );

        // The window: claimed, unshipped. The same pair arrives again.
        enqueue(gd, &entry("refs/rift/chain/v1", 7)).unwrap();
        assert_eq!(
            pending(gd).len(),
            1,
            "an entry already queued — in the live file OR in a claimed batch — \
             is not written twice"
        );

        // …and a genuinely different position is still different work.
        enqueue(gd, &entry("refs/rift/chain/v1", 8)).unwrap();
        assert_eq!(pending(gd).len(), 2);

        // The suppressed pair is not a dropped push: the next drain adopts the
        // claim and ships the ref it named.
        let ship = Recorder::new(false);
        let report = drain(gd, &ship, 20).unwrap();
        assert_eq!(report.shipped, vec!["refs/rift/chain/v1".to_owned()]);
        assert_eq!(report.entries, 2, "the claimed entry and the live one");
        assert!(pending(gd).is_empty());
    }

    /// One corrupt line must not strand the rest of the file.
    #[test]
    fn a_corrupt_line_is_skipped_not_fatal() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        enqueue(gd, &entry("refs/rift/chain/v1", 1)).unwrap();
        let path = queue_dir(gd).join(QUEUE_FILE);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{not json\n\n");
        std::fs::write(&path, text).unwrap();
        assert_eq!(pending_refs(gd), vec!["refs/rift/chain/v1".to_owned()]);
    }

    /// A ref name is an argv word in a `git push`; the queue refuses the shapes
    /// that stop being one before they ever reach a file.
    #[test]
    fn the_queue_refuses_a_ref_name_that_is_not_one() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path();
        assert!(enqueue(gd, &entry("", 1)).is_err());
        assert!(enqueue(gd, &entry("--upload-pack=evil", 1)).is_err());
        assert!(enqueue(gd, &entry("refs/a b", 1)).is_err());
        assert!(pending(gd).is_empty());
    }

    /// The stress exit criterion, as a conservation law: with writers
    /// appending while a drain claims and ships, **every entry ever enqueued is
    /// either shipped exactly once or still queued** — none is lost to an
    /// interleaved write, and none is shipped twice.
    ///
    /// The interesting failure is silent: two appends interleaving inside one
    /// line makes both lines unparseable, `read_entries` skips them, and the
    /// refs vanish with no error anywhere. So the assertion is on the *union* of
    /// what the shipper saw and what remains, not on either alone.
    #[test]
    fn a_drain_under_concurrent_enqueue_loses_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let gd = tmp.path().to_path_buf();
        std::fs::create_dir_all(queue_dir(&gd)).unwrap();

        const WRITERS: u64 = 8;
        const PER_WRITER: u64 = 25;
        let start = std::sync::Arc::new(std::sync::Barrier::new(
            usize::try_from(WRITERS).unwrap() + 1,
        ));
        let mut handles = Vec::new();
        for w in 0..WRITERS {
            let dir = gd.clone();
            let barrier = std::sync::Arc::clone(&start);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for i in 0..PER_WRITER {
                    let e = PushEntry {
                        schema_version: PUSH_QUEUE_SCHEMA_VERSION,
                        // A distinct ref per (writer, i): the queue de-duplicates
                        // by (ref, seq), so identical entries would make the
                        // conservation check vacuous.
                        ref_name: format!("refs/rift/stress/w{w}/n{i}"),
                        chain_seq: i,
                        enqueued_at_ms: 1_700_000_000_000 + i64::try_from(i).unwrap_or(0),
                    };
                    enqueue(&dir, &e).unwrap();
                }
            }));
        }

        let ship = Recorder::new(false);
        start.wait();
        // Drain repeatedly *while* the writers run, then once more after they
        // finish so the final tail is claimed too.
        let mut shipped: Vec<String> = Vec::new();
        for round in 0..12 {
            let report = drain(&gd, &ship, 1000 + round).unwrap();
            shipped.extend(report.shipped);
        }
        for h in handles {
            h.join().unwrap();
        }
        shipped.extend(drain(&gd, &ship, 9999).unwrap().shipped);

        let mut seen = shipped.clone();
        seen.extend(pending_refs(&gd));
        seen.sort();
        let before = seen.len();
        seen.dedup();
        assert_eq!(
            before,
            seen.len(),
            "a ref was both shipped and left queued, or shipped twice"
        );
        let expected = usize::try_from(WRITERS * PER_WRITER).unwrap();
        assert_eq!(
            seen.len(),
            expected,
            "conservation: every enqueued ref must be shipped or still queued \
             (shipped {}, pending {})",
            shipped.len(),
            pending_refs(&gd).len()
        );
    }

    /// Identity, not resemblance. Every one of these is a *different remote*
    /// from `rift-origin` at `ssh://git@h/x.git`, and each must fail to match —
    /// because matching is what lets a capture ledger leave the machine.
    #[test]
    fn a_push_matches_the_target_only_on_both_halves_exactly() {
        let t = ShipTarget {
            name: "rift-origin".to_owned(),
            url: "ssh://git@h/x.git".to_owned(),
        };
        assert!(t.matches_push("rift-origin", "ssh://git@h/x.git"));
        assert!(!t.matches_push("origin", "ssh://git@h/x.git"), "other name");
        assert!(
            !t.matches_push("rift-origin", "ssh://git@h/y.git"),
            "other url"
        );
        assert!(!t.matches_push("rift-origin", "ssh://git@h/x"), "no .git");
        assert!(
            !t.matches_push("rift-origin", "git@h:x.git"),
            "other spelling"
        );
        assert!(!t.matches_push("", ""), "an unnamed push matches nothing");
        assert!(
            !t.matches_push("rift-origin", ""),
            "a urlless push matches nothing"
        );
    }

    /// **One family, one queue.** What a linked member enqueues,
    /// a `pre-push` from any other member ships — because both spellings of the
    /// common dir (a member's lexical `…/worktrees/x/../..` and the main
    /// checkout's own `.git`) name the same directory, and the queue is keyed
    /// there rather than under the gitdir each checkout has to itself.
    ///
    /// The negative half is the defect this replaces: the member's admin dir
    /// holds no queue at all, so a drain keyed on it would have shipped nothing
    /// while the work sat one directory away — 62 entries' worth, measured
    /// (RESEARCH §6).
    #[test]
    fn one_family_has_one_queue_whichever_member_names_it() {
        use rift_git::test_support::{fake_repo, fake_worktree};
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        let main_git = fake_repo(&main).unwrap();
        let member = tmp.path().join("member");
        let admin = fake_worktree(&main_git, &member, "member").unwrap();

        let from_member = rift_git::discover(&member).unwrap();
        let from_main = rift_git::discover(&main).unwrap();
        assert_ne!(
            from_member.git_dir, from_main.git_dir,
            "the two members have different gitdirs — the split this fixes"
        );

        // The member enqueues.
        enqueue(&from_member.common_dir, &entry("refs/rift/chain/v1", 1)).unwrap();
        assert!(
            !admin.join(QUEUE_DIR).exists(),
            "nothing is written under the member's own admin dir any more"
        );
        assert_eq!(
            queue_dir(&from_member.common_dir),
            queue_dir(&from_main.common_dir),
            "both members resolve to one queue directory"
        );

        // The main work tree drains, and ships what the member queued.
        let ship = Recorder::new(false);
        let report = drain(&from_main.common_dir, &ship, 10).unwrap();
        assert_eq!(report.shipped, vec!["refs/rift/chain/v1".to_owned()]);
        assert!(pending(&from_member.common_dir).is_empty());
    }

    /// A queue an older binary left under an admin dir is adopted into the
    /// common queue **once**, and the directory it emptied is gone.
    ///
    /// Idempotency has two halves here and both are asserted: a pair already in
    /// the common queue is not written twice (`(ref_name, chain_seq)`, the same
    /// key [`enqueue`] de-duplicates on), and a second pass over a family whose
    /// legacy directory is already gone finds nothing to do.
    #[test]
    fn legacy_admin_dir_queues_drain_once_into_the_common_queue() {
        use rift_git::test_support::{fake_repo, fake_worktree};
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        let main_git = fake_repo(&main).unwrap();
        let member = tmp.path().join("member");
        let admin = fake_worktree(&main_git, &member, "member").unwrap();
        let from_main = rift_git::discover(&main).unwrap();

        // The old shape, written by hand: a queue under the member's admin dir.
        let legacy = admin.join(QUEUE_DIR);
        std::fs::create_dir_all(&legacy).unwrap();
        let lines = format!(
            "{}\n{}\n{}\n",
            serde_json::to_string(&entry("refs/rift/chain/v1", 1)).unwrap(),
            serde_json::to_string(&entry("refs/heads/rift/checkpoints/v1", 1)).unwrap(),
            serde_json::to_string(&entry("--upload-pack=evil", 1)).unwrap(),
        );
        std::fs::write(legacy.join(QUEUE_FILE), lines).unwrap();
        // …and one pair the common queue already holds.
        enqueue(&from_main.common_dir, &entry("refs/rift/chain/v1", 1)).unwrap();

        let report = adopt_legacy(&from_main);
        assert_eq!(report.dirs.len(), 1, "one legacy queue found: {report:?}");
        assert_eq!(report.adopted, 2, "the two shippable entries");
        assert_eq!(
            report.refused, 1,
            "a ref name that is not one is never shipped"
        );
        assert!(report.kept.is_empty(), "the emptied directory was removed");
        assert!(
            !legacy.exists(),
            "adopted once: the legacy directory is gone"
        );

        assert_eq!(
            pending(&from_main.common_dir).len(),
            2,
            "the duplicate pair was suppressed, not doubled"
        );
        assert_eq!(
            pending_refs(&from_main.common_dir),
            vec![
                "refs/rift/chain/v1".to_owned(),
                "refs/heads/rift/checkpoints/v1".to_owned()
            ]
        );

        // Second pass: nothing left to adopt, and nothing added.
        let again = adopt_legacy(&from_main);
        assert!(again.is_empty(), "a second pass finds no legacy queue");
        assert_eq!(pending(&from_main.common_dir).len(), 2);
    }

    /// The main work tree's gitdir **is** the common dir, so its own queue must
    /// never be mistaken for a legacy one and moved into itself.
    #[test]
    fn the_common_queue_is_never_adopted_into_itself() {
        use rift_git::test_support::fake_repo;
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        fake_repo(&main).unwrap();
        let paths = rift_git::discover(&main).unwrap();
        enqueue(&paths.common_dir, &entry("refs/rift/chain/v1", 1)).unwrap();

        let report = adopt_legacy(&paths);
        assert!(report.is_empty(), "the common queue is not a legacy queue");
        assert_eq!(
            pending_refs(&paths.common_dir),
            vec!["refs/rift/chain/v1".to_owned()],
            "and the entry it holds is untouched"
        );
        assert!(queue_dir(&paths.common_dir).is_dir());
    }
}
