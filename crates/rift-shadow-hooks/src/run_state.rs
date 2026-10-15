//! Where a repository family keeps the run state of each of its members.
//!
//! Run state is what a hook leaves for the next hook: the session caches with
//! their progress sidecars and claim markers, the agent-hook job journal and
//! the post-commit capture queue. It is keyed by the family under the machine's
//! cache root, `<family_dir>/run/<member>/`, rather than living inside a git
//! dir, so a repository's run state does not depend on git's directory layout,
//! and the projection and the run state of one family sit side by side.
//!
//! `<member>` keeps the per-checkout scope the git dir used to give: a session
//! driven in one worktree must never be credited to a commit made in another,
//! so each member owns its own directory, and the main work tree's directory
//! is the shared registry a linked member consults.
//!
//! This module names paths and reads through them. The move out of the git
//! dir belongs to the binary's worker and verbs, never to a hook. Until a
//! member is marked moved, every read is a **dual read**, file by file: the
//! new tree first, the git dir's old tree when the new one lacks that file
//! ([`read_path`], [`list`]). One file is never read from both trees, and
//! writes go to the new tree only, so a write made before the move stands
//! after it. A member whose run directory records nothing yet (no new binary
//! has resolved it) has only its old tree ([`dir_for_read`]).

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The marker a member's directory carries once its run state was moved out of
/// the git dir. Readers that cannot move read the new tree only after it.
pub const MOVED_MARKER: &str = ".moved-v1";
/// The family-wide move lock, created exclusively under the run root.
pub const MOVE_LOCK: &str = ".moving";
/// The family's run-state directory under its cache directory.
const RUN_DIR: &str = "run";
/// The file a member's directory records its canonical work tree in.
///
/// The session cache stamps every cache with the work tree it was written for
/// (the discriminator that keeps one checkout's session off another's commit),
/// and a git dir could answer that from its own shape. A run directory cannot,
/// so the member records it once, when its directory is created.
const WORK_TREE_FILE: &str = "work_tree";
/// The file a member's run directory records its git dir in: where its old
/// tree is, for the dual read before the move.
const GIT_DIR_FILE: &str = "git_dir";
/// What a file preserved beside the one that stood is renamed to:
/// `<name>.late-<ms>`. No reader takes it for run state, because every
/// reader selects by an extension this suffix replaces.
const LATE_MARKER: &str = ".late-";

/// More than one family holds a member under one checkout's token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AmbiguousMember {
    /// The checkout's member token.
    pub token: String,
    /// Every run directory holding it, sorted.
    pub members: Vec<PathBuf>,
}

impl std::fmt::Display for AmbiguousMember {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "run state: {} families hold member {}; resolve the family to choose",
            self.members.len(),
            self.token
        )
    }
}

impl std::error::Error for AmbiguousMember {}

/// One root of a member's run state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunRoot {
    /// The session caches, their sidecars and claim markers.
    Sessions,
    /// The agent-hook job journal.
    AgentQueue,
    /// The post-commit capture queue and its notes.
    CaptureQueue,
}

impl RunRoot {
    /// Every root, in the order a move takes them.
    pub const ALL: [Self; 3] = [Self::Sessions, Self::AgentQueue, Self::CaptureQueue];

    /// This root's directory inside a member's run directory.
    #[must_use]
    pub fn under(self, member: &Path) -> PathBuf {
        member.join(self.dir_name())
    }

    /// Where this root lived inside a git dir before the move. Read only by
    /// the move itself, the doctor, and [`dir_for_read`] for a member that
    /// has not been moved yet.
    #[must_use]
    pub fn legacy_under(self, git_dir: &Path) -> PathBuf {
        git_dir.join(self.legacy_name())
    }

    /// The directory name inside a member's run directory.
    #[must_use]
    pub const fn dir_name(self) -> &'static str {
        match self {
            Self::Sessions => "sessions",
            Self::AgentQueue => "agent-queue",
            Self::CaptureQueue => "capture-queue",
        }
    }

    /// The directory name the root had inside a git dir.
    #[must_use]
    pub const fn legacy_name(self) -> &'static str {
        match self {
            Self::Sessions => "rift-sessions",
            Self::AgentQueue => "rift-agent-queue",
            Self::CaptureQueue => "rift-capture-queue",
        }
    }
}

/// The family's run root: `<family_dir>/run`, where `family_dir` is the
/// family's directory under the cache root (the one its projection lives in).
#[must_use]
pub fn run_root(family_dir: &Path) -> PathBuf {
    family_dir.join(RUN_DIR)
}

/// A member's run directory: `<family_dir>/run/<member token>`.
#[must_use]
pub fn member_dir(family_dir: &Path, work_tree: &Path) -> PathBuf {
    run_root(family_dir).join(member_token(work_tree))
}

/// The member's 12-hex token: the first six bytes of SHA-256 over the work
/// tree's path, the token shard metadata already carries.
///
/// Hashed over the **canonical** path. Discovery hashes the path as it walked
/// it, and an agent hook and a git hook can reach one checkout by two
/// spellings (a symlinked directory, `/tmp` and `/private/tmp`); two tokens
/// would split one member's sessions across two directories, and the commit
/// hook would find none of them.
#[must_use]
pub fn member_token(work_tree: &Path) -> String {
    let canonical = canonical_through_ancestor(work_tree);
    let digest = Sha256::digest(canonical.to_string_lossy().as_bytes());
    digest.iter().take(6).fold(String::new(), |mut acc, byte| {
        let _ = write!(acc, "{byte:02x}");
        acc
    })
}

/// The run directory of the member checked out at `work_tree`, found by its
/// token among the families under `families` (`<cache_root>/rift`, the
/// directory every family's projection directory sits in).
///
/// For a reader that cannot resolve a family. The token hashes the canonical
/// work tree, so two different checkouts never share one. One checkout path
/// can still be a member of two families, when a repository was removed and
/// cloned again at the same path; which one is live is the family manifest's
/// to say, so this refuses to choose. `Ok(None)` when no family holds the
/// member, and the git dir is then its only tree.
///
/// # Errors
/// [`AmbiguousMember`] when more than one family holds a member by this
/// token.
pub fn find_member(
    families: &Path,
    work_tree: &Path,
) -> std::result::Result<Option<PathBuf>, AmbiguousMember> {
    let token = member_token(work_tree);
    let Ok(entries) = std::fs::read_dir(families) else {
        return Ok(None);
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|family| run_root(&family.path()).join(&token))
        .filter(|member| member.is_dir())
        .collect();
    if found.len() > 1 {
        found.sort();
        return Err(AmbiguousMember {
            token,
            members: found,
        });
    }
    Ok(found.pop())
}

/// Whether the member's run state was moved out of its git dir.
#[must_use]
pub fn is_moved(member: &Path) -> bool {
    member.join(MOVED_MARKER).exists()
}

/// Where a reader that resolves no family finds `root` for this member: the
/// new tree once a new binary has recorded the member — read through
/// [`read_path`] and [`list`], which fall back to the old tree file by file
/// until the member is marked moved — and the git dir's old tree before.
#[must_use]
pub fn dir_for_read(root: RunRoot, member: &Path, git_dir: &Path) -> PathBuf {
    if is_moved(member) || recorded_git_dir(member).is_some() {
        return root.under(member);
    }
    root.legacy_under(git_dir)
}

/// The old-tree directory a read of `dir` falls back to: `dir` is a
/// directory inside a recorded member's run directory (a root, or a
/// directory within one such as a session's subagent claims), and the
/// member is not marked moved. `None` otherwise.
#[must_use]
pub fn fallback_dir(dir: &Path) -> Option<PathBuf> {
    for member in dir.ancestors().skip(1).take(2) {
        let Some(git_dir) = recorded_git_dir(member) else {
            continue;
        };
        if is_moved(member) {
            return None;
        }
        let rel = dir.strip_prefix(member).ok()?;
        let mut parts = rel.components();
        let first = parts.next()?.as_os_str();
        let root = RunRoot::ALL.into_iter().find(|r| first == r.dir_name())?;
        return Some(root.legacy_under(&git_dir).join(parts.as_path()));
    }
    None
}

/// The file a reader of `dir` reads for `name`: the new tree's when it has
/// one, the old tree's while the member is unmoved and only it has one, and
/// the new tree's (absent) path otherwise.
#[must_use]
pub fn read_path(dir: &Path, name: &str) -> PathBuf {
    let primary = dir.join(name);
    if primary.symlink_metadata().is_ok() {
        return primary;
    }
    match fallback_dir(dir) {
        Some(old) if old.join(name).symlink_metadata().is_ok() => old.join(name),
        _ => primary,
    }
}

/// The directories a reader of `dir` lists: `dir`, then its old tree while
/// the member is unmoved.
#[must_use]
pub fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    std::iter::once(dir.to_path_buf())
        .chain(fallback_dir(dir))
        .collect()
}

/// Every entry a reader of `dir` sees, by path: the new tree's entries, then
/// the old tree's whose names the new tree lacks — each name once, from one
/// tree. Sorted by name.
#[must_use]
pub fn list(dir: &Path) -> Vec<PathBuf> {
    let mut seen = std::collections::BTreeMap::new();
    for source in read_dirs(dir) {
        let Ok(entries) = std::fs::read_dir(&source) else {
            continue;
        };
        for entry in entries.flatten() {
            seen.entry(entry.file_name())
                .or_insert_with(|| entry.path());
        }
    }
    seen.into_values().collect()
}

/// List the complete dual-read view without hiding an unreadable registry.
///
/// # Errors
/// Any directory or entry read failure except an absent registry.
pub fn list_checked(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut seen = std::collections::BTreeMap::new();
    for source in read_dirs(dir) {
        let entries = match std::fs::read_dir(source) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            seen.entry(entry.file_name())
                .or_insert_with(|| entry.path());
        }
    }
    Ok(seen.into_values().collect())
}

/// Whether a file name is a copy preserved beside the one that stood.
#[must_use]
pub fn is_late(name: &str) -> bool {
    name.contains(LATE_MARKER)
}

/// The name a file is preserved under beside the one that stood.
#[must_use]
pub fn late_name(name: &str, now_ms: i64) -> String {
    format!("{name}{LATE_MARKER}{now_ms}")
}

/// Record the member's canonical work tree in its run directory, creating the
/// directory owner-only. A no-op once recorded.
///
/// # Errors
/// Any I/O error creating the directory or writing the file.
pub fn record_work_tree(member: &Path, work_tree: &Path) -> std::io::Result<()> {
    let path = member.join(WORK_TREE_FILE);
    if path.exists() {
        return Ok(());
    }
    create_private_dir(member)?;
    let canonical = canonical_through_ancestor(work_tree);
    std::fs::write(path, canonical.to_string_lossy().as_bytes())
}

/// Record the member's git dir in its run directory: where the dual read
/// finds the old tree. Rewritten only when it differs (a moved checkout).
///
/// # Errors
/// Any I/O error creating the directory or writing the file.
pub fn record_git_dir(member: &Path, git_dir: &Path) -> std::io::Result<()> {
    if recorded_git_dir(member).as_deref() == Some(git_dir) {
        return Ok(());
    }
    create_private_dir(member)?;
    std::fs::write(
        member.join(GIT_DIR_FILE),
        git_dir.to_string_lossy().as_bytes(),
    )
}

/// The git dir a member's run directory recorded, if it recorded one.
#[must_use]
pub fn recorded_git_dir(member: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(member.join(GIT_DIR_FILE)).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(PathBuf::from(text))
}

/// The work tree a member's run directory recorded, if it recorded one.
#[must_use]
pub fn recorded_work_tree(member: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(member.join(WORK_TREE_FILE)).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    Some(PathBuf::from(text))
}

/// Create `dir` and its missing parents owner-only (`0700` on unix), the
/// mode every run-state directory had in the git dir's keeping.
///
/// # Errors
/// Any I/O error creating a directory.
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// `path` canonicalized through its deepest existing ancestor, with the
/// missing tail joined back on.
///
/// A member's checkout can be removed while its run state still holds work
/// (a queued condense the doctor drains), and its token must not change when
/// it goes: canonicalizing only an existing path would hash `/var/…` once the
/// checkout that resolved to `/private/var/…` is gone.
fn canonical_through_ancestor(path: &Path) -> PathBuf {
    let mut tail = Vec::new();
    let mut probe = path;
    loop {
        if let Ok(canonical) = std::fs::canonicalize(probe) {
            return tail
                .iter()
                .rev()
                .fold(canonical, |acc: PathBuf, part| acc.join(part));
        }
        let (Some(parent), Some(name)) = (probe.parent(), probe.file_name()) else {
            return path.to_path_buf();
        };
        tail.push(name);
        probe = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_member_keeps_each_root_in_its_own_directory_under_the_family() {
        let family = Path::new("/cache/rift/fam");
        let tree = Path::new("/nowhere/checkout");
        let member = member_dir(family, tree);
        assert_eq!(member.parent(), Some(Path::new("/cache/rift/fam/run")));
        assert_eq!(member_token(tree).len(), 12);
        assert_eq!(
            RunRoot::Sessions.under(&member),
            member.join("sessions"),
            "the sessions root"
        );
        assert_eq!(
            RunRoot::AgentQueue.under(&member),
            member.join("agent-queue")
        );
        assert_eq!(
            RunRoot::CaptureQueue.under(&member),
            member.join("capture-queue")
        );
        assert_eq!(
            RunRoot::CaptureQueue.legacy_under(Path::new("/r/.git")),
            Path::new("/r/.git/rift-capture-queue")
        );
    }

    #[cfg(unix)]
    #[test]
    fn two_spellings_of_one_checkout_are_one_member() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = tmp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert_eq!(member_token(&real), member_token(&link));
        assert_ne!(member_token(&real), member_token(tmp.path()));
    }

    #[cfg(unix)]
    #[test]
    fn a_removed_checkout_keeps_the_token_it_had() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real");
        std::fs::create_dir(&real).unwrap();
        std::os::unix::fs::symlink(&real, tmp.path().join("link")).unwrap();
        let checkout = tmp.path().join("link").join("checkout");
        std::fs::create_dir(&checkout).unwrap();
        let token = member_token(&checkout);
        std::fs::remove_dir(&checkout).unwrap();
        assert_eq!(member_token(&checkout), token);
    }

    #[test]
    fn a_reader_reads_the_old_tree_until_a_new_binary_has_recorded_the_member() {
        let tmp = tempfile::tempdir().unwrap();
        let member = tmp.path().join("run").join("abc");
        let git_dir = tmp.path().join(".git");
        assert_eq!(
            dir_for_read(RunRoot::Sessions, &member, &git_dir),
            git_dir.join("rift-sessions")
        );
        record_git_dir(&member, &git_dir).unwrap();
        assert_eq!(
            dir_for_read(RunRoot::Sessions, &member, &git_dir),
            member.join("sessions")
        );
    }

    /// Before the move each file is read from the new tree when it is there
    /// and from the old tree when only the old tree has it — never one name
    /// from both — and the marker ends the fallback.
    #[test]
    fn an_unmoved_member_is_read_file_by_file_new_tree_first() {
        let tmp = tempfile::tempdir().unwrap();
        let member = tmp.path().join("run").join("abc");
        let git_dir = tmp.path().join(".git");
        record_git_dir(&member, &git_dir).unwrap();
        let new = RunRoot::Sessions.under(&member);
        let old = RunRoot::Sessions.legacy_under(&git_dir);
        for (dir, name, body) in [
            (&new, "both.json", "new"),
            (&old, "both.json", "old"),
            (&old, "old-only.json", "old"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join(name), body).unwrap();
        }
        let read = |name: &str| std::fs::read_to_string(read_path(&new, name)).unwrap();
        assert_eq!(read("both.json"), "new");
        assert_eq!(read("old-only.json"), "old");
        assert_eq!(
            list(&new),
            [new.join("both.json"), old.join("old-only.json")]
        );
        let nested = new.join("s.subagents");
        assert_eq!(fallback_dir(&nested), Some(old.join("s.subagents")));

        std::fs::write(member.join(MOVED_MARKER), b"").unwrap();
        assert_eq!(fallback_dir(&new), None);
        assert_eq!(read_path(&new, "old-only.json"), new.join("old-only.json"));
        assert_eq!(list(&new), [new.join("both.json")]);
    }

    #[test]
    fn a_preserved_late_file_is_named_so_no_reader_selects_it() {
        let late = late_name("s1.json", 7);
        assert!(is_late(&late));
        assert_ne!(Path::new(&late).extension(), Some("json".as_ref()));
        assert!(!is_late("s1.json"));
    }

    #[test]
    fn a_member_is_found_by_its_token_and_two_families_holding_it_is_an_error() {
        let tmp = tempfile::tempdir().unwrap();
        let families = tmp.path().join("rift");
        let tree = tmp.path().join("checkout");
        std::fs::create_dir_all(&tree).unwrap();
        assert_eq!(find_member(&families, &tree), Ok(None), "no families yet");
        std::fs::create_dir_all(families.join("other").join("run").join("ffffffffffff")).unwrap();
        assert_eq!(
            find_member(&families, &tree),
            Ok(None),
            "another member only"
        );
        let member = member_dir(&families.join("fam"), &tree);
        create_private_dir(&member).unwrap();
        assert_eq!(find_member(&families, &tree), Ok(Some(member.clone())));
        let recloned = member_dir(&families.join("fam2"), &tree);
        create_private_dir(&recloned).unwrap();
        assert_eq!(
            find_member(&families, &tree),
            Err(AmbiguousMember {
                token: member_token(&tree),
                members: vec![member, recloned],
            })
        );
    }

    #[test]
    fn the_recorded_work_tree_is_written_once_and_read_back() {
        let tmp = tempfile::tempdir().unwrap();
        let member = tmp.path().join("run").join("abc");
        assert_eq!(recorded_work_tree(&member), None);
        record_work_tree(&member, tmp.path()).unwrap();
        let canonical = std::fs::canonicalize(tmp.path()).unwrap();
        assert_eq!(recorded_work_tree(&member), Some(canonical.clone()));
        record_work_tree(&member, Path::new("/elsewhere")).unwrap();
        assert_eq!(recorded_work_tree(&member), Some(canonical));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&member).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }
}
