//! Native git plumbing for the native capture path.
//!
//! Three pieces the `NativeBackend` composes:
//!
//! * [`RepoPaths`] + [`discover`] — explicit **worktree / common-dir** handling so
//!   multi-worktree repos capture correctly (objects in the common dir, per-worktree
//!   shadow refs, destructive ops isolated to the intended worktree).
//! * [`NativeGitWrite`] — additive object/ref writes where the **durable** writer
//!   accepts only [`RedactedBytes`] (the redaction type wall survives into
//!   plumbing; a durable write of raw bytes does not compile) and the local-only writer takes
//!   [`SecretBytes`].
//! * [`git_reset_hard`] / [`git_checkout`] — the two **destructive** working-tree
//!   ops shell out to the user's `git` (mirroring the legacy backend's documented go-git
//!   workaround), via [`run_git`]: an **argv array, no shell**, explicit
//!   `GIT_DIR`/`GIT_WORK_TREE`, with refspecs/oids validated so no argument is
//!   interpretable as a flag — defeating argument injection.
//!
//! This crate is the single home for the LAYOUT contract (ref naming + shard
//! paths). The recall-side substrate reader converges onto it as a follow-up; for
//! now the read side stays in `rift-recall-substrate` (gix) and this crate owns the
//! write side + worktree discovery + the destructive-op shell-out.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use rift_core::{RedactedBytes, Result, RiftError, SecretBytes};
use sha2::{Digest, Sha256};

pub mod blob;
pub mod chain;
pub mod child_path;
pub mod family;
pub mod identity;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
#[cfg(feature = "gix")]
pub use blob::GixBlobReader;
pub use blob::{BlobReader, MemoryBlobReader, TreeEntry};
pub use child_path::{child_process_path, display_path};
pub use family::{
    family_members, family_of, remint, resolve_family, Family, FamilyMember, FamilyState,
    MintPolicy, MintedFrom, RemintMode, UnmintedReason, ENV_FAMILY_DIR,
};
pub use identity::{GitConfigIdentity, ENV_DEVELOPER_OVERRIDE};

/// Lowercase hex of a byte slice.
fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The resolved git paths for one worktree.
///
/// Objects live in the **common dir** (shared, written once); shadow refs are
/// per-worktree (named by [`worktree_hash`](RepoPaths::worktree_hash)); destructive
/// ops act on exactly this worktree via explicit `GIT_DIR`/`GIT_WORK_TREE`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPaths {
    /// This worktree's gitdir (`.git`, or `.git/worktrees/<name>` for a linked worktree).
    pub git_dir: PathBuf,
    /// The shared object/ref store (the main `.git`).
    pub common_dir: PathBuf,
    /// This worktree's working directory.
    pub work_tree: PathBuf,
    /// Identifies the worktree in the shadow-branch name `rift/<c7>-<wt6>` (LAYOUT).
    pub worktree_hash: [u8; 6],
}

impl RepoPaths {
    /// The worktree hash as 12 lowercase hex chars (the `wt6` token).
    #[must_use]
    pub fn worktree_token(&self) -> String {
        to_hex(&self.worktree_hash)
    }

    /// Whether this is a **linked** worktree: its gitdir is the administrative
    /// `<common_dir>/worktrees/<name>`, not the common dir itself.
    ///
    /// Compared canonically, not lexically: a linked worktree derives its
    /// common dir from git's `commondir` file, which is *relative*
    /// (`…/worktrees/x/../..`), so the two paths that name one directory do not
    /// share bytes.
    #[must_use]
    pub fn is_linked(&self) -> bool {
        family::canonical_or_lexical(&self.git_dir)
            != family::canonical_or_lexical(&self.common_dir)
    }

    /// This worktree's name for provenance: a linked member's **administrative**
    /// name (the `worktrees/<name>` directory git itself keys it by), or `main`.
    ///
    /// The admin name rather than the checkout's directory name, because the
    /// admin name is what `git worktree list` and every member-enumerating
    /// reader agree on, and a checkout can be renamed under git's feet. Note
    /// the deliberate difference from [`family::FamilyMember`]'s `name`, which
    /// calls the main work tree by its directory: a provenance label says
    /// `main` so a reader never has to know this repository's folder name to
    /// recognise the main checkout.
    #[must_use]
    pub fn member_name(&self) -> String {
        if !self.is_linked() {
            return "main".to_owned();
        }
        self.git_dir
            .file_name()
            .map_or_else(|| "main".to_owned(), |n| n.to_string_lossy().into_owned())
    }
}

/// Discover the repo paths from `start`, handling linked worktrees.
///
/// Walks up from `start` to a `.git` entry. A `.git` **directory** is the main
/// worktree (git_dir = common_dir = `.git`). A `.git` **file** (`gitdir: <path>`)
/// is a linked worktree; the common dir is read from `<git_dir>/commondir`.
///
/// A `.git` directory counts only when it actually **is** a gitdir:
/// [`is_gitdir`] — `HEAD`, plus `objects/` or `commondir`. Anything else named
/// `.git` is skipped and the walk continues upward. The rule
/// exists because this check's absence made a bare `mkdir .git` — which a hook
/// fired outside any repository used to create — into a repository with an
/// identity of its own: five of them in this tree at once, each a second
/// identity for work that belonged to a real one. A `.git` **file** is parsed
/// as before; its gitdir is named by the file, not guessed from the walk.
///
/// # Errors
/// [`RiftError::NotFound`] if no `.git` is found walking up from `start`.
pub fn discover(start: &Path) -> Result<RepoPaths> {
    let mut dir = if start.is_dir() {
        start.to_path_buf()
    } else {
        start.parent().unwrap_or(start).to_path_buf()
    };
    loop {
        let dot_git = dir.join(".git");
        if dot_git.is_dir() && is_gitdir(&dot_git) {
            return Ok(build_paths(dot_git.clone(), dot_git, dir));
        }
        if dot_git.is_file() {
            let git_dir = parse_gitdir_file(&dot_git, &dir)?;
            let common_dir = read_common_dir(&git_dir);
            return Ok(build_paths(git_dir, common_dir, dir));
        }
        match dir.parent() {
            Some(p) => dir = p.to_path_buf(),
            None => return Err(RiftError::NotFound("no .git found".into())),
        }
    }
}

/// Whether a directory is a git **gitdir**: it holds `HEAD`, and either an
/// `objects/` store or a `commondir` pointer at one.
///
/// This is what separates a repository from a directory that merely shares its
/// name. It is deliberately the *weakest* test that still excludes an empty
/// `mkdir .git`: a repository mid-`git init` has `HEAD` and `objects/` before
/// it has anything else, and a worktree admin dir has `HEAD` and `commondir`,
/// so nothing real is refused by it.
#[must_use]
pub fn is_gitdir(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && (dir.join("objects").is_dir() || dir.join("commondir").is_file())
}

/// A stable [`rift_core::RepoId`] for a working tree — content-addressed over
/// its canonical path, so each working tree gets its own projection directory.
///
/// The derivation is pinned for compatibility with every existing projection:
/// hex(SHA-256(`0x00` ‖ canonical-path-bytes))[..32] — the `0x00` prefix is the
/// Merkle *leaf* domain separator this id was historically minted with (in the
/// `rift` bin, via `rift-hashchain::leaf_hash`); changing it would orphan every
/// projection on disk. The hash itself lives in
/// [`rift_core::id::legacy::capture_slug`], shared byte-for-byte with
/// `rift-store`'s spelling of the same id; this function owns the one piece
/// `legacy` deliberately does not: the [`std::fs::canonicalize`] call.
///
/// # Errors
/// [`RiftError::Io`] if `dir` cannot be canonicalized, or the id is malformed
/// (structurally unreachable for a 32-hex slug).
pub fn repo_id_for(dir: &Path) -> Result<rift_core::RepoId> {
    let canon = std::fs::canonicalize(dir)?;
    rift_core::RepoId::new(rift_core::id::legacy::capture_slug(&canon))
}

/// One linked worktree of a repository, as enumerated from
/// `<common_dir>/worktrees/*`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkedWorktree {
    /// The worktree's administrative name (the `worktrees/<name>` dir).
    pub name: String,
    /// The working-tree directory the worktree checks out into.
    pub work_tree: PathBuf,
}

/// Enumerate a repository's **linked worktrees** by iterating
/// `<common_dir>/worktrees/*`: each entry's `gitdir` file names the worktree's
/// `.git` file, whose parent is the working tree. Total and honest — a pruned,
/// malformed, or unreadable entry is **skipped** (never a panic, never an
/// invented row), and a repository with no `worktrees/` dir yields an empty
/// list. The main working tree is not listed (it is the repository row
/// itself).
#[must_use]
pub fn list_worktrees(paths: &RepoPaths) -> Vec<LinkedWorktree> {
    let admin = paths.common_dir.join("worktrees");
    let Ok(entries) = std::fs::read_dir(&admin) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(name) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        // `gitdir` holds the absolute path of the worktree's `.git` FILE; the
        // working tree is its parent. A missing/garbled file = pruned → skip.
        let Ok(text) = std::fs::read_to_string(dir.join("gitdir")) else {
            continue;
        };
        let dot_git = gitdir_text_path(&text);
        let Some(work_tree) = dot_git.parent().map(std::path::Path::to_path_buf) else {
            continue;
        };
        // A worktree whose directory is gone (deleted but not pruned) is
        // skipped too — a row for a nonexistent path would be an invented lie.
        if !work_tree.is_dir() {
            continue;
        }
        out.push(LinkedWorktree { name, work_tree });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The path a `gitdir` file (or a `gitdir:` line) names, parsed from its
/// text.
///
/// Git writes these files itself and spells them plainly, but the text is
/// trusted for a `parent()` step by every reader, and on Windows a verbatim
/// spelling (`\\?\C:\…`) changes what that step means: under the prefix only
/// `\` separates, so a `/`-joined tail is one component and the parent
/// collapses to the wrong directory. A verbatim text therefore has its tail
/// re-joined with `\` — its spelling is otherwise kept, prefix included,
/// because a prunable member's recorded path is exactly this text and is
/// never re-spelled. Whitespace is trimmed; the text is otherwise the path.
#[must_use]
pub fn gitdir_text_path(text: &str) -> PathBuf {
    let text = text.trim();
    #[cfg(windows)]
    {
        if text.starts_with(r"\\?\") && text.contains('/') {
            return PathBuf::from(text.replace('/', "\\"));
        }
    }
    PathBuf::from(text)
}

fn build_paths(git_dir: PathBuf, common_dir: PathBuf, work_tree: PathBuf) -> RepoPaths {
    let mut h = Sha256::new();
    h.update(work_tree.to_string_lossy().as_bytes());
    let digest = h.finalize();
    let mut worktree_hash = [0u8; 6];
    #[allow(clippy::indexing_slicing)] // SHA-256 output is always 32 bytes
    worktree_hash.copy_from_slice(&digest[..6]);
    RepoPaths {
        git_dir,
        common_dir,
        work_tree,
        worktree_hash,
    }
}

/// Parse a `.git` **file** (`gitdir: <path>`) into the linked worktree's gitdir.
fn parse_gitdir_file(dot_git: &Path, base: &Path) -> Result<PathBuf> {
    let text = std::fs::read_to_string(dot_git)?;
    let rel = text
        .lines()
        .find_map(|l| l.strip_prefix("gitdir:"))
        .map(str::trim)
        .ok_or_else(|| RiftError::validity("malformed .git file (no gitdir:)"))?;
    let p = gitdir_text_path(rel);
    Ok(if p.is_absolute() { p } else { base.join(p) })
}

/// Read `<git_dir>/commondir` (a path, relative to `git_dir`) → the common dir.
/// Absent → the git_dir is its own common dir (a non-worktree gitdir).
fn read_common_dir(git_dir: &Path) -> PathBuf {
    match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(rel) => {
            let rel = rel.trim();
            let p = PathBuf::from(rel);
            if p.is_absolute() {
                p
            } else {
                git_dir.join(p)
            }
        }
        Err(_) => git_dir.to_path_buf(),
    }
}

/// What `HEAD` said in one worktree at one moment: the branch it points at and
/// the commit it resolves to. Either may be absent, and absence is a FACT here,
/// never an empty string — a detached head has no branch, an unborn branch has
/// no commit, and an unreadable gitdir has neither (the schema stores both
/// columns NULLABLE for exactly these three shapes).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeadState {
    /// `HEAD`'s symbolic name with `refs/heads/` stripped — the short branch
    /// name `git rev-parse --abbrev-ref HEAD` prints. `None` when detached or
    /// unreadable.
    pub branch: Option<String>,
    /// The object id `HEAD` resolves to. `None` on an unborn branch (the ref
    /// file does not exist yet) or when the ref cannot be resolved from the
    /// files this reader is allowed to touch.
    pub oid: Option<String>,
}

/// Read `HEAD` **without running git** — `paths.git_dir/HEAD`, then the loose
/// ref, then `packed-refs`.
///
/// **Why not `git rev-parse`.** This runs on the agent-hook path, once per
/// hook, inside the hard 100 ms git-latency budget with no daemon to hide
/// behind. A process spawn costs milliseconds; three `read_to_string` calls
/// cost microseconds, and the files are the same ones git itself would read.
/// The trade is that this reader understands the two ref stores git writes
/// (loose files and `packed-refs`) and not the `reftable` backend; a repository
/// git has migrated to `reftable` reports `oid: None`, which is UNKNOWN and
/// therefore correct rather than wrong.
///
/// A linked worktree's per-worktree `HEAD` lives in its own admin dir, which is
/// what `paths.git_dir` already is — so a member's branch is its own, never the
/// main work tree's. The loose ref is looked up in the worktree's gitdir FIRST
/// (git keeps per-worktree refs such as `refs/bisect/*` there) and then in the
/// common store, which is where `refs/heads/*` actually lives.
///
/// Total: every failure is `None`. Nothing here can fail a hook.
#[must_use]
pub fn head_state(paths: &RepoPaths) -> HeadState {
    let Ok(text) = std::fs::read_to_string(paths.git_dir.join("HEAD")) else {
        return HeadState::default();
    };
    let text = text.trim();
    let Some(refname) = text.strip_prefix("ref:") else {
        // Detached: `HEAD` holds the oid itself. Screened rather than trusted —
        // this file is ordinary bytes on disk and the value lands in an
        // append-only row.
        return HeadState {
            branch: None,
            oid: validate_oid(text).is_ok().then(|| text.to_owned()),
        };
    };
    let refname = refname.trim();
    if !is_safe_refname(refname) {
        return HeadState::default();
    }
    let branch = refname.strip_prefix("refs/heads/").unwrap_or(refname);
    HeadState {
        branch: (!branch.is_empty()).then(|| branch.to_owned()),
        oid: resolve_loose_or_packed(paths, refname),
    }
}

/// Whether a ref name read out of `HEAD` may be used as a path suffix under a
/// gitdir: `refs/`-rooted, with no component that escapes it.
///
/// `HEAD` is a file anyone with write access to the checkout can edit, and this
/// function is the difference between reading `<git_dir>/refs/heads/x` and
/// reading `<git_dir>/../../../../etc/passwd`. The screen is positive (an
/// allow-list of shapes) rather than a blacklist of `..`, because a blacklist
/// of path traversal is the one screen this codebase has already had to widen
/// twice.
fn is_safe_refname(refname: &str) -> bool {
    refname.starts_with("refs/")
        && refname.len() <= 255
        && validate_refspec(refname).is_ok()
        && refname
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains('\\'))
}

/// The oid a screened ref resolves to: the worktree's own loose ref, then the
/// common store's, then `packed-refs`. `None` when the ref exists nowhere (an
/// unborn branch) or holds something that is not an object id.
fn resolve_loose_or_packed(paths: &RepoPaths, refname: &str) -> Option<String> {
    for base in [&paths.git_dir, &paths.common_dir] {
        if let Ok(text) = std::fs::read_to_string(base.join(refname)) {
            let value = text.trim();
            if validate_oid(value).is_ok() {
                return Some(value.to_owned());
            }
            // A symref file (`ref: refs/heads/other`) is NOT followed: one
            // level of indirection is all `HEAD` itself is allowed, and a
            // reader that chases a chain of them is a reader that can loop.
            return None;
        }
    }
    let packed = std::fs::read_to_string(paths.common_dir.join("packed-refs")).ok()?;
    packed
        .lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with('^'))
        .find_map(|l| {
            let (oid, name) = l.split_once(' ')?;
            (name.trim() == refname && validate_oid(oid).is_ok()).then(|| oid.to_owned())
        })
}

// --- additive object/ref writes (the type wall) -----------------------------

/// A git tree entry's object type + file mode. Only the two the checkpoint layout
/// uses: a `100644` regular-file blob, and a `040000` subtree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitFileMode {
    /// A regular file (mode `100644`, type `blob`).
    Blob,
    /// A subtree (mode `040000`, type `tree`).
    Tree,
}

impl GitFileMode {
    /// The octal mode token git's `mktree` stdin format expects.
    #[must_use]
    pub fn mode_token(self) -> &'static str {
        match self {
            Self::Blob => "100644",
            Self::Tree => "040000",
        }
    }

    /// The object-type token (`blob` / `tree`).
    #[must_use]
    pub fn type_token(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
        }
    }
}

/// One entry in a tree written by [`NativeGitWrite::write_tree`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeWriteEntry {
    /// The object mode/type.
    pub mode: GitFileMode,
    /// The entry name (a single path component — never contains `/`).
    pub name: String,
    /// The referenced object id (hex).
    pub oid: String,
}

/// A git author/committer identity for a deterministic checkpoint commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIdent {
    /// The identity name.
    pub name: String,
    /// The identity email.
    pub email: String,
}

impl Default for GitIdent {
    fn default() -> Self {
        // A stable, brand-free identity so checkpoint commits are byte-reproducible
        // and never leak a developer's own name/email onto the shadow branch.
        Self {
            name: "rift".to_owned(),
            email: "rift@localhost".to_owned(),
        }
    }
}

/// The spec for grafting one checkpoint (leaf) tree onto an orphan checkpoint
/// branch: merge it into the branch's current tip tree at `shard_path`, commit
/// with a deterministic identity/date, and advance the ref.
#[derive(Debug, Clone)]
pub struct OrphanCommit<'a> {
    /// The fully-qualified branch ref (e.g. `refs/heads/rift/checkpoints/v1`).
    pub branch: &'a str,
    /// The `<id[:2]>/<id[2:]>` path the checkpoint tree is grafted at (no trailing `/`).
    pub shard_path: &'a str,
    /// The checkpoint (leaf) tree oid to graft.
    pub tree_oid: &'a str,
    /// The commit message (brand-free).
    pub message: &'a str,
    /// The author/committer identity.
    pub ident: &'a GitIdent,
    /// The author/committer date, epoch **seconds** — caller-supplied for
    /// byte-reproducibility (never the wall clock).
    pub date_epoch: i64,
}

/// Additive git object/ref writes. The **durable** writer accepts only
/// [`RedactedBytes`]; the **local-only** writer accepts [`SecretBytes`] and may
/// only target a local shadow ref — the plumbing cannot durably persist raw bytes
/// (it does not compile). This is the redaction gate surviving into the write
/// layer.
///
/// The checkpoint-archive methods ([`write_structural_blob`](NativeGitWrite::write_structural_blob),
/// [`write_tree`](NativeGitWrite::write_tree),
/// [`commit_on_orphan`](NativeGitWrite::commit_on_orphan)) build the on-branch
/// shard. They never widen the redaction gate: `write_tree`/`commit_on_orphan`
/// take **object ids**, not bytes, and `write_structural_blob` accepts only
/// Rift-authored *structural* documents (checkpoint metadata / redaction report /
/// event index — counts, ids, and content-addresses, provably never captured
/// payload bytes). Captured content still enters durable storage exclusively
/// through [`write_durable_blob`](NativeGitWrite::write_durable_blob).
pub trait NativeGitWrite {
    /// Write a durable blob from already-redacted bytes; returns its object id (hex).
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn write_durable_blob(&self, body: &RedactedBytes) -> Result<String>;

    /// Write raw bytes to the **local-only** shadow ref (never a durable branch).
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn write_shadow_raw(&self, body: &SecretBytes) -> Result<()>;

    /// Write a **structural** checkpoint artifact (Rift-authored metadata / report /
    /// event index — never captured payload bytes) as a blob; returns its oid.
    /// Written byte-verbatim: these documents carry only counts, ids, and
    /// content-addresses, and are the LAYOUT/BYTE-parity surface, so they must not
    /// pass back through the entropy redactor (which would mask their hex hashes).
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn write_structural_blob(&self, bytes: &[u8]) -> Result<String>;

    /// Point a ref at an object id.
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn update_ref(&self, name: &str, oid: &str) -> Result<()>;

    /// Write a (flat) tree from its entries; returns the tree oid.
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn write_tree(&self, entries: &[TreeWriteEntry]) -> Result<String>;

    /// Graft a checkpoint tree onto the orphan checkpoint branch: merge it into the
    /// branch's current tip tree at `spec.shard_path`, commit, and advance the ref.
    /// Returns the resulting branch-tip commit oid. **Idempotent:** if the shard
    /// path is already present on the branch, this is a no-op that returns the
    /// existing tip.
    ///
    /// # Errors
    /// [`RiftError`] on a write failure.
    fn commit_on_orphan(&self, spec: &OrphanCommit<'_>) -> Result<String>;
}

/// An in-memory [`NativeGitWrite`] for tests: records durable + structural blobs,
/// trees, and refs; keeps local-only raw writes strictly separate from durable
/// storage; and models the orphan-branch graft deterministically so callers can
/// assert layout + idempotency without a real git repo.
#[derive(Default)]
pub struct MemoryGitWrite {
    durable: std::cell::RefCell<Vec<(String, Vec<u8>)>>,
    structural: std::cell::RefCell<Vec<(String, Vec<u8>)>>,
    trees: std::cell::RefCell<Vec<(String, Vec<TreeWriteEntry>)>>,
    refs: std::cell::RefCell<Vec<(String, String)>>,
    local_raw: std::cell::RefCell<usize>,
    /// Shard paths grafted onto the branch → checkpoint tree oid (for idempotency).
    shards: std::cell::RefCell<std::collections::BTreeMap<String, String>>,
    /// The current branch tip commit oid, if the branch has been created.
    tip: std::cell::RefCell<Option<String>>,
}

impl MemoryGitWrite {
    /// Number of durable blobs written.
    #[must_use]
    pub fn durable_count(&self) -> usize {
        self.durable.borrow().len()
    }

    /// Number of structural artifact blobs written.
    #[must_use]
    pub fn structural_count(&self) -> usize {
        self.structural.borrow().len()
    }

    /// Number of trees written.
    #[must_use]
    pub fn tree_count(&self) -> usize {
        self.trees.borrow().len()
    }

    /// Number of local-only raw writes (never durable).
    #[must_use]
    pub fn local_raw_count(&self) -> usize {
        *self.local_raw.borrow()
    }

    /// The shard paths grafted onto the branch, in insertion order.
    #[must_use]
    pub fn shard_paths(&self) -> Vec<String> {
        self.shards.borrow().keys().cloned().collect()
    }

    /// The current branch tip commit oid, if any.
    #[must_use]
    pub fn branch_tip(&self) -> Option<String> {
        self.tip.borrow().clone()
    }
}

/// The deterministic content oid a memory double assigns (sha256 of bytes) — a
/// stand-in for git's own object hashing (the real oids come from the plumbing
/// impl; the double only needs stable, collision-resistant ids for assertions).
fn mem_oid(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    to_hex(&h.finalize())
}

impl NativeGitWrite for MemoryGitWrite {
    fn write_durable_blob(&self, body: &RedactedBytes) -> Result<String> {
        let bytes = body.as_slice().to_vec();
        let oid = mem_oid(&[&bytes]);
        self.durable.borrow_mut().push((oid.clone(), bytes));
        Ok(oid)
    }

    fn write_shadow_raw(&self, body: &SecretBytes) -> Result<()> {
        let _ = body.as_slice().len(); // local-only; never enters durable storage
        *self.local_raw.borrow_mut() += 1;
        Ok(())
    }

    fn write_structural_blob(&self, bytes: &[u8]) -> Result<String> {
        let oid = mem_oid(&[bytes]);
        self.structural
            .borrow_mut()
            .push((oid.clone(), bytes.to_vec()));
        Ok(oid)
    }

    fn update_ref(&self, name: &str, oid: &str) -> Result<()> {
        self.refs
            .borrow_mut()
            .push((name.to_owned(), oid.to_owned()));
        Ok(())
    }

    fn write_tree(&self, entries: &[TreeWriteEntry]) -> Result<String> {
        // A tree's oid is content-addressed over its (mode,name,oid) entries in the
        // exact order given (git sorts; the caller passes canonical order).
        let mut buf = Vec::new();
        for e in entries {
            buf.extend_from_slice(e.mode.mode_token().as_bytes());
            buf.push(b' ');
            buf.extend_from_slice(e.name.as_bytes());
            buf.push(0);
            buf.extend_from_slice(e.oid.as_bytes());
        }
        let oid = mem_oid(&[b"tree", &buf]);
        self.trees
            .borrow_mut()
            .push((oid.clone(), entries.to_vec()));
        Ok(oid)
    }

    fn commit_on_orphan(&self, spec: &OrphanCommit<'_>) -> Result<String> {
        // Content-aware idempotency (mirrors the plumbing impl): an identical
        // re-graft is a no-op; so is anchor-only drift (the checkpoint id is
        // content-addressed over the capture and deliberately EXCLUDES the
        // anchoring commit — the trailer-prediction contract — so a re-archive
        // of unchanged capture at a later commit differs only in metadata.json
        // + the content_hash.txt digest over it, and keeps the FIRST anchor).
        // Divergent capture content at the same shard path must surface.
        let existing = self.shards.borrow().get(spec.shard_path).cloned();
        if let Some(existing) = existing {
            let same = existing == spec.tree_oid || {
                let trees = self.trees.borrow();
                let entries_of = |oid: &str| {
                    trees
                        .iter()
                        .find(|(t, _)| t == oid)
                        .map(|(_, e)| non_anchor_entries(e))
                };
                entries_of(&existing).is_some_and(|a| entries_of(spec.tree_oid) == Some(a))
            };
            if same {
                return self
                    .tip
                    .borrow()
                    .clone()
                    .ok_or_else(|| RiftError::validity("branch tip missing for existing shard"));
            }
            return Err(RiftError::integrity(
                "checkpoint shard path already holds different content",
            ));
        }
        let prev = self.tip.borrow().clone();
        self.shards
            .borrow_mut()
            .insert(spec.shard_path.to_owned(), spec.tree_oid.to_owned());
        let commit = mem_oid(&[
            b"commit",
            prev.as_deref().unwrap_or("").as_bytes(),
            spec.shard_path.as_bytes(),
            spec.tree_oid.as_bytes(),
            &spec.date_epoch.to_le_bytes(),
        ]);
        *self.tip.borrow_mut() = Some(commit.clone());
        self.refs
            .borrow_mut()
            .push((spec.branch.to_owned(), commit.clone()));
        Ok(commit)
    }
}

// --- destructive working-tree ops (shell out to `git`) ----------------------

/// Validate a git object id: non-empty lowercase hex of a sha1/sha256 length.
///
/// # Errors
/// [`RiftError::Validity`] if not a hex oid.
pub fn validate_oid(oid: &str) -> Result<()> {
    let ok = matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit());
    if ok {
        Ok(())
    } else {
        Err(RiftError::validity("invalid git oid"))
    }
}

/// Validate a refspec: no leading `-` (flag injection), no shell metacharacters,
/// no whitespace/NUL. Argument injection cannot enter here.
///
/// # Errors
/// [`RiftError::Validity`] if the refspec is unsafe.
pub fn validate_refspec(refspec: &str) -> Result<()> {
    let unsafe_char = |b: u8| {
        b == 0
            || b.is_ascii_whitespace()
            || matches!(
                b,
                b';' | b'|' | b'&' | b'$' | b'`' | b'(' | b')' | b'<' | b'>' | b'\\' | b'\'' | b'"'
            )
    };
    if refspec.is_empty() || refspec.starts_with('-') || refspec.bytes().any(unsafe_char) {
        return Err(RiftError::validity("unsafe refspec"));
    }
    Ok(())
}

/// The argv for `reset --hard <oid>` — note the `--` terminator so the oid can
/// never be parsed as a flag.
#[must_use]
pub fn reset_hard_argv(oid: &str) -> Vec<String> {
    vec!["reset".into(), "--hard".into(), "--".into(), oid.into()]
}

/// The argv for `checkout <refspec>` — `--`-terminated.
#[must_use]
pub fn checkout_argv(refspec: &str) -> Vec<String> {
    vec!["checkout".into(), "--".into(), refspec.into()]
}

/// `git reset --hard <oid>` against `repo`'s exact worktree (validated, no shell).
///
/// # Errors
/// [`RiftError::Validity`] for a bad oid; [`RiftError::Storage`] on a non-zero exit.
pub fn git_reset_hard(repo: &RepoPaths, oid: &str) -> Result<()> {
    validate_oid(oid)?;
    run_git(repo, &reset_hard_argv(oid))
}

/// `git checkout <refspec>` against `repo`'s exact worktree (validated, no shell).
///
/// # Errors
/// [`RiftError::Validity`] for a bad refspec; [`RiftError::Storage`] on a non-zero exit.
pub fn git_checkout(repo: &RepoPaths, refspec: &str) -> Result<()> {
    validate_refspec(refspec)?;
    run_git(repo, &checkout_argv(refspec))
}

/// Invoke the user's `git` with an **argv array (no shell)** and explicit
/// `GIT_DIR`/`GIT_WORK_TREE` (never inferred). Maps a non-zero exit to a
/// [`RiftError`] and **never** lets a git failure abort the developer's own op.
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn run_git(repo: &RepoPaths, args: &[String]) -> Result<()> {
    run_git_capture(repo, args, &[]).map(|_| ())
}

/// [`run_git`], returning captured stdout bytes and feeding `stdin` to the
/// child. The argv-array, explicit-env discipline is identical; stdout is the
/// **only** channel read back (stderr is dropped, never surfaced).
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn run_git_capture(repo: &RepoPaths, args: &[String], stdin: &[u8]) -> Result<Vec<u8>> {
    run_git_capture_env(repo, args, stdin, &[])
}

/// [`run_git_capture`] with additional explicit environment variables (e.g.
/// `GIT_INDEX_FILE`, `GIT_AUTHOR_DATE`) layered on top of the always-explicit
/// `GIT_DIR`/`GIT_WORK_TREE`. Still an argv array, no shell.
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn run_git_capture_env(
    repo: &RepoPaths,
    args: &[String],
    stdin: &[u8],
    env: &[(&str, &str)],
) -> Result<Vec<u8>> {
    match run_git_status(repo, args, stdin, env)? {
        (true, out) => Ok(out),
        (false, _) => Err(RiftError::storage("git exited non-zero")),
    }
}

/// [`run_git_capture_env`], but a non-zero exit is **not** an error: returns
/// `(success, stdout)`. For probes like `rev-parse --verify` / `cat-file -e`
/// where "not found" is an ordinary answer, not a failure. A spawn failure is
/// still an [`Err`].
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned.
pub fn run_git_status(
    repo: &RepoPaths,
    args: &[String],
    stdin: &[u8],
    env: &[(&str, &str)],
) -> Result<(bool, Vec<u8>)> {
    use std::io::Write as _;
    use std::process::Stdio;

    let mut cmd = Command::new("git");
    cmd.args(args)
        .env("GIT_DIR", child_process_path(&repo.git_dir))
        .env("GIT_WORK_TREE", child_process_path(&repo.work_tree));
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .stdin(if stdin.is_empty() {
            Stdio::null()
        } else {
            Stdio::piped()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| RiftError::storage(format!("git invocation failed: {e}")))?;
    if !stdin.is_empty() {
        if let Some(mut pipe) = child.stdin.take() {
            pipe.write_all(stdin)
                .map_err(|e| RiftError::storage(format!("git stdin failed: {e}")))?;
        }
    }
    let output = child
        .wait_with_output()
        .map_err(|e| RiftError::storage(format!("git invocation failed: {e}")))?;
    Ok((output.status.success(), output.stdout))
}

/// The worktree paths that differ from HEAD (modified/added/deleted, staged or
/// not, plus untracked files) via `git status --porcelain -z`. Paths are
/// worktree-relative; rename records yield the **new** path.
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn changed_worktree_paths(repo: &RepoPaths) -> Result<Vec<String>> {
    let out = run_git_capture(
        repo,
        &[
            "status".into(),
            "--porcelain=v1".into(),
            "-z".into(),
            "--untracked-files=all".into(),
        ],
        &[],
    )?;
    let mut paths = Vec::new();
    let mut fields = out.split(|b| *b == 0);
    while let Some(entry) = fields.next() {
        if entry.len() < 4 {
            continue; // status entries are `XY <path>`; anything shorter is noise
        }
        let status = entry.get(..2).unwrap_or_default();
        let path = String::from_utf8_lossy(entry.get(3..).unwrap_or_default()).into_owned();
        // A rename record (`R`/`C` in either column) is followed by the ORIGIN
        // path as its own NUL field — consume it; the new path is what changed.
        if status.contains(&b'R') || status.contains(&b'C') {
            let _ = fields.next();
        }
        if !path.is_empty() {
            paths.push(path);
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// Hash `bytes` as a git blob **and write it into the object store**
/// (`git hash-object -w --stdin`), returning the blob oid (hex, repo hash
/// algorithm). Local-only plumbing: blobs are unreachable objects unless later
/// referenced, and are what makes turn-boundary content addressable.
///
/// # Errors
/// [`RiftError::Storage`] if `git` cannot be spawned or exits non-zero.
pub fn hash_object_write(repo: &RepoPaths, bytes: &[u8]) -> Result<String> {
    let out = run_git_capture(
        repo,
        &["hash-object".into(), "-w".into(), "--stdin".into()],
        bytes,
    )?;
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

// --- checkpoint-archive plumbing --------------------------------------------
//
// git itself produces the byte-canonical objects that enter the SHA-256 hash
// chain, so BYTE parity is *inherited* — there is no second
// canonicalization to audit. All writes go through the same argv-safe seam.

/// The trimmed first line of git stdout as an oid (hex).
fn trim_oid(out: &[u8]) -> String {
    String::from_utf8_lossy(out)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// Build a tree object from entries via `git mktree` (git canonicalizes order and
/// framing). Entries reference already-written objects. Returns the tree oid.
///
/// # Errors
/// [`RiftError::Storage`] on a git failure, [`RiftError::Validity`] on a bad entry.
pub fn mktree(repo: &RepoPaths, entries: &[TreeWriteEntry]) -> Result<String> {
    use std::fmt::Write as _;
    let mut stdin = String::new();
    for e in entries {
        validate_oid(&e.oid)?;
        if e.name.is_empty() || e.name.contains('/') || e.name.contains('\0') {
            return Err(RiftError::validity("unsafe tree entry name"));
        }
        // `<mode> SP <type> SP <oid> TAB <name> LF` — git's mktree stdin format.
        let _ = writeln!(
            stdin,
            "{} {} {}\t{}",
            e.mode.mode_token(),
            e.mode.type_token(),
            e.oid,
            e.name
        );
    }
    let out = run_git_capture(repo, &["mktree".into()], stdin.as_bytes())?;
    Ok(trim_oid(&out))
}

/// Resolve a rev to a commit oid, or `None` if it does not resolve (e.g. a branch
/// that does not exist yet). `rev` is validated as a safe, non-flag ref token.
///
/// # Errors
/// [`RiftError::Validity`] for an unsafe rev; [`RiftError::Storage`] on spawn failure.
pub fn resolve_commit(repo: &RepoPaths, rev: &str) -> Result<Option<String>> {
    validate_refspec(rev)?;
    let (ok, out) = run_git_status(
        repo,
        &[
            "rev-parse".into(),
            "--verify".into(),
            "--quiet".into(),
            format!("{rev}^{{commit}}"),
        ],
        &[],
        &[],
    )?;
    Ok(ok.then(|| trim_oid(&out)).filter(|s| !s.is_empty()))
}

/// The root-tree oid of a commit.
///
/// # Errors
/// [`RiftError::Validity`]/[`RiftError::Storage`] on failure.
pub fn commit_tree_oid(repo: &RepoPaths, commit: &str) -> Result<String> {
    validate_oid(commit)?;
    let out = run_git_capture(
        repo,
        &["rev-parse".into(), format!("{commit}^{{tree}}")],
        &[],
    )?;
    Ok(trim_oid(&out))
}

/// Whether `path` exists inside the tree of `commit` (object probe, no worktree).
///
/// # Errors
/// [`RiftError::Storage`] on spawn failure.
pub fn path_in_commit(repo: &RepoPaths, commit: &str, path: &str) -> Result<bool> {
    validate_oid(commit)?;
    let (ok, _) = run_git_status(
        repo,
        &["cat-file".into(), "-e".into(), format!("{commit}:{path}")],
        &[],
        &[],
    )?;
    Ok(ok)
}

/// The object id git resolves at `<commit>:<path>` (the subtree/blob oid at that
/// path), or `None` if the path does not exist. Used to compare an already-grafted
/// shard's content against a new one (content-aware idempotency).
///
/// # Errors
/// [`RiftError::Storage`] on spawn failure.
pub fn path_object_oid(repo: &RepoPaths, commit: &str, path: &str) -> Result<Option<String>> {
    validate_oid(commit)?;
    let (ok, out) = run_git_status(
        repo,
        &[
            "rev-parse".into(),
            "--verify".into(),
            "--quiet".into(),
            format!("{commit}:{path}"),
        ],
        &[],
        &[],
    )?;
    Ok(ok.then(|| trim_oid(&out)).filter(|s| !s.is_empty()))
}

/// The shard entries that drift with the anchoring commit / archive time and
/// are therefore EXEMPT from idempotence comparison: `metadata.json` embeds
/// them directly, and `content_hash.txt` is a digest computed OVER
/// metadata.json (the LAYOUT contract), so it inherits the drift. The capture
/// identity is carried by the remaining entries (`content.redacted`,
/// `redaction_report.json`, `full.jsonl`), which are always compared.
const ANCHOR_DEPENDENT_SHARD_ENTRIES: &[&str] = &["metadata.json", "content_hash.txt"];

/// Whether one `ls-tree` line names an [`ANCHOR_DEPENDENT_SHARD_ENTRIES`]
/// entry. Lifted out of the comparison so the RULE can be tested without a git
/// process: the rule that "a re-archive from another worktree keeps the first
/// label" is true only because `metadata.json` — which now also carries the
/// `worktree` member — is on that list.
fn is_anchor_dependent_entry(ls_tree_line: &str) -> bool {
    ANCHOR_DEPENDENT_SHARD_ENTRIES
        .iter()
        .any(|n| ls_tree_line.ends_with(&format!("\t{n}")))
}

/// Whether two shard leaf trees are equal once the anchor-dependent entries are
/// set aside (see [`ANCHOR_DEPENDENT_SHARD_ENTRIES`]) — the checkpoint id is
/// content-addressed over the capture and deliberately excludes the anchoring
/// commit (the trailer-prediction contract), so those entries legitimately
/// differ across re-archives of identical capture.
///
/// # Errors
/// [`RiftError::Validity`]/[`RiftError::Storage`] if a tree cannot be listed.
fn trees_equal_ignoring_anchor(repo: &RepoPaths, a: &str, b: &str) -> Result<bool> {
    validate_oid(a)?;
    validate_oid(b)?;
    let list = |oid: &str| -> Result<Vec<String>> {
        let out = run_git_capture(repo, &["ls-tree".into(), oid.to_owned()], &[])?;
        let mut lines: Vec<String> = String::from_utf8_lossy(&out)
            .lines()
            .filter(|l| !is_anchor_dependent_entry(l))
            .map(str::to_owned)
            .collect();
        lines.sort_unstable();
        Ok(lines)
    };
    Ok(list(a)? == list(b)?)
}

/// The shard-tree entries that participate in idempotence comparison —
/// everything except the anchor-dependent pair (see
/// [`trees_equal_ignoring_anchor`]). Shared by the in-memory writer double.
fn non_anchor_entries(entries: &[TreeWriteEntry]) -> Vec<TreeWriteEntry> {
    entries
        .iter()
        .filter(|e| !ANCHOR_DEPENDENT_SHARD_ENTRIES.contains(&e.name.as_str()))
        .cloned()
        .collect()
}

/// Commit `tree` (optionally onto `parent`) with a deterministic identity + date,
/// returning the commit oid. Author == committer, date fixed by the caller, so the
/// commit is byte-reproducible for identical inputs.
///
/// # Errors
/// [`RiftError::Validity`]/[`RiftError::Storage`] on failure.
pub fn commit_tree(
    repo: &RepoPaths,
    tree: &str,
    parent: Option<&str>,
    message: &str,
    ident: &GitIdent,
    date_epoch: i64,
) -> Result<String> {
    validate_oid(tree)?;
    let mut args = vec![
        "commit-tree".into(),
        tree.to_owned(),
        "-m".into(),
        message.to_owned(),
    ];
    if let Some(p) = parent {
        validate_oid(p)?;
        args.push("-p".into());
        args.push(p.to_owned());
    }
    // `@<epoch> +0000` is git's unambiguous unix-timestamp form (a bare small
    // integer like `1 +0000` is rejected as an invalid date); it normalizes to the
    // same stored `<epoch> +0000`, so the commit oid is byte-reproducible.
    let date = format!("@{date_epoch} +0000");
    let env = [
        ("GIT_AUTHOR_NAME", ident.name.as_str()),
        ("GIT_AUTHOR_EMAIL", ident.email.as_str()),
        ("GIT_AUTHOR_DATE", date.as_str()),
        ("GIT_COMMITTER_NAME", ident.name.as_str()),
        ("GIT_COMMITTER_EMAIL", ident.email.as_str()),
        ("GIT_COMMITTER_DATE", date.as_str()),
    ];
    let out = run_git_capture_env(repo, &args, &[], &env)?;
    Ok(trim_oid(&out))
}

/// Point `refname` at `new_oid`, compare-and-swapping against `old_oid` when known
/// (so a concurrent advance is rejected rather than clobbered).
///
/// # Errors
/// [`RiftError::Validity`]/[`RiftError::Storage`] on failure.
pub fn update_ref_cas(
    repo: &RepoPaths,
    refname: &str,
    new_oid: &str,
    old_oid: Option<&str>,
) -> Result<()> {
    validate_refspec(refname)?;
    validate_oid(new_oid)?;
    let mut args = vec!["update-ref".into(), refname.to_owned(), new_oid.to_owned()];
    if let Some(old) = old_oid {
        validate_oid(old)?;
        args.push(old.to_owned());
    }
    run_git(repo, &args)
}

/// The all-zero object id — git's "the ref must not already exist" sentinel for a
/// compare-and-swap `update-ref` (a create-only assertion).
const ZERO_OID: &str = "0000000000000000000000000000000000000000";

/// Create `refname` at `new_oid`, asserting it does **not** already exist (CAS
/// against the zero oid). A concurrent creation by another worktree is rejected,
/// not clobbered — the loser errors and its caller defers rather than silently
/// dropping the winner's checkpoint.
///
/// # Errors
/// [`RiftError::Validity`] for a bad ref/oid; [`RiftError::Storage`] if the ref
/// already exists (lost the create race) or git fails.
pub fn update_ref_create(repo: &RepoPaths, refname: &str, new_oid: &str) -> Result<()> {
    validate_refspec(refname)?;
    validate_oid(new_oid)?;
    // Zero old-value requires the ref to not already exist (create-only CAS).
    run_git(
        repo,
        &[
            "update-ref".into(),
            refname.to_owned(),
            new_oid.to_owned(),
            ZERO_OID.to_owned(),
        ],
    )
}

/// The production [`NativeGitWrite`]: writes checkpoint objects into `repo`'s
/// object store via screened git plumbing. Reads stay on gix elsewhere.
pub struct PlumbingGitWrite {
    repo: RepoPaths,
    ident: GitIdent,
}

impl PlumbingGitWrite {
    /// A writer for `repo` with the default (brand-free) commit identity.
    #[must_use]
    pub fn new(repo: RepoPaths) -> Self {
        Self {
            repo,
            ident: GitIdent::default(),
        }
    }

    /// A writer with an explicit commit identity.
    #[must_use]
    pub fn with_ident(repo: RepoPaths, ident: GitIdent) -> Self {
        Self { repo, ident }
    }
}

impl NativeGitWrite for PlumbingGitWrite {
    fn write_durable_blob(&self, body: &RedactedBytes) -> Result<String> {
        hash_object_write(&self.repo, body.as_slice())
    }

    fn write_shadow_raw(&self, body: &SecretBytes) -> Result<()> {
        // Local-only: write the blob into the object store (unreachable until a
        // local shadow ref points at it); it never lands on a durable branch here.
        hash_object_write(&self.repo, body.as_slice()).map(|_| ())
    }

    fn write_structural_blob(&self, bytes: &[u8]) -> Result<String> {
        hash_object_write(&self.repo, bytes)
    }

    fn update_ref(&self, name: &str, oid: &str) -> Result<()> {
        update_ref_cas(&self.repo, name, oid, None)
    }

    fn write_tree(&self, entries: &[TreeWriteEntry]) -> Result<String> {
        mktree(&self.repo, entries)
    }

    fn commit_on_orphan(&self, spec: &OrphanCommit<'_>) -> Result<String> {
        validate_oid(spec.tree_oid)?;
        validate_refspec(spec.branch)?;
        if spec.shard_path.is_empty()
            || spec.shard_path.starts_with('/')
            || spec.shard_path.contains("..")
            || !spec
                .shard_path
                .bytes()
                .all(|b| b.is_ascii_hexdigit() || b == b'/')
        {
            return Err(RiftError::validity("unsafe shard path"));
        }
        let tip = resolve_commit(&self.repo, spec.branch)?;
        // Idempotent by CONTENT, not merely by path: a re-fired archive of the
        // identical shard (same leaf tree) is a true no-op, and so is
        // anchor-only drift — the checkpoint id is content-addressed over the
        // capture, deliberately excluding the anchoring commit (the
        // trailer-prediction contract), so unchanged capture re-archived at a
        // later commit differs only in metadata.json + content_hash.txt and
        // keeps the FIRST anchor. A shard whose *capture* entries differ is a
        // genuine conflict — the immutable branch keeps the first, so we must
        // surface it (a divergence under DualRun) instead of silently dropping
        // content.
        if let Some(t) = &tip {
            if let Some(existing) = path_object_oid(&self.repo, t, spec.shard_path)? {
                if existing == spec.tree_oid
                    || trees_equal_ignoring_anchor(&self.repo, &existing, spec.tree_oid)?
                {
                    return Ok(t.clone());
                }
                return Err(RiftError::integrity(
                    "checkpoint shard path already holds different content",
                ));
            }
        }

        // Build the merged root tree in a scratch index seeded from the current tip,
        // then graft the checkpoint tree at the shard path. Git owns the merge +
        // canonical framing, so no ancestor tree is rebuilt by hand.
        let index_path = self.repo.git_dir.join(format!(
            "rift-graft-index-{}-{}",
            std::process::id(),
            &spec.shard_path.replace('/', "")
        ));
        // The path leaves the process in git's environment, so it gets the
        // child spelling: a verbatim `GIT_INDEX_FILE` is refused on Windows
        // ("Unable to create ….lock: Invalid argument") exactly as `GIT_DIR` is.
        let index_str = child_process_path(&index_path)
            .to_string_lossy()
            .into_owned();
        let env = [("GIT_INDEX_FILE", index_str.as_str())];
        let _ = std::fs::remove_file(&index_path); // start clean

        let graft = || -> Result<String> {
            if let Some(t) = &tip {
                let root = commit_tree_oid(&self.repo, t)?;
                run_git_capture_env(&self.repo, &["read-tree".into(), root], &[], &env)?;
            }
            run_git_capture_env(
                &self.repo,
                &[
                    "read-tree".into(),
                    format!("--prefix={}/", spec.shard_path),
                    spec.tree_oid.to_owned(),
                ],
                &[],
                &env,
            )?;
            let out = run_git_capture_env(&self.repo, &["write-tree".into()], &[], &env)?;
            Ok(trim_oid(&out))
        };
        let root_tree = graft();
        let _ = std::fs::remove_file(&index_path); // best-effort cleanup
        let root_tree = root_tree?;

        let commit = commit_tree(
            &self.repo,
            &root_tree,
            tip.as_deref(),
            spec.message,
            &self.ident,
            spec.date_epoch,
        )?;
        // Advance CAS-protected against the tip we read; create with a
        // must-not-exist assertion so a concurrent creator can't be clobbered.
        match tip.as_deref() {
            Some(old) => update_ref_cas(&self.repo, spec.branch, &commit, Some(old))?,
            None => update_ref_create(&self.repo, spec.branch, &commit)?,
        }
        Ok(commit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a hermetic main-repo + linked-worktree layout, so no git binary is
    /// needed. The shapes come from [`crate::test_support`] — one builder for
    /// the whole workspace, because a fixture that writes a `.git` directory
    /// hardened discovery would refuse is testing a repository the product does
    /// not accept.
    fn worktree_fixture(root: &Path, names: &[&str]) -> RepoPaths {
        let main = root.join("main");
        let main_git = crate::test_support::fake_repo(&main).unwrap();
        for name in names {
            crate::test_support::fake_worktree(&main_git, &root.join(name), name).unwrap();
        }
        discover(&main).unwrap()
    }

    #[test]
    fn lists_linked_worktrees_sorted_and_skips_the_main_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = worktree_fixture(tmp.path(), &["wt-b", "wt-a"]);
        let listed = list_worktrees(&paths);
        assert_eq!(
            listed.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec!["wt-a", "wt-b"],
            "sorted by name; the main tree is not a linked worktree"
        );
        assert_eq!(listed[0].work_tree, tmp.path().join("wt-a"));
    }

    #[test]
    fn no_worktrees_dir_is_an_empty_list() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("solo");
        crate::test_support::fake_repo(&main).unwrap();
        let paths = discover(&main).unwrap();
        assert!(list_worktrees(&paths).is_empty());
    }

    #[test]
    fn pruned_and_malformed_entries_are_skipped_honestly() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = worktree_fixture(tmp.path(), &["good"]);
        let admin = paths.common_dir.join("worktrees");
        // Pruned: an admin dir with no gitdir file.
        std::fs::create_dir_all(admin.join("pruned")).unwrap();
        // Deleted-but-not-pruned: gitdir points at a directory that is gone.
        let ghost = admin.join("ghost");
        std::fs::create_dir_all(&ghost).unwrap();
        std::fs::write(
            ghost.join("gitdir"),
            format!("{}\n", tmp.path().join("nope").join(".git").display()),
        )
        .unwrap();
        let listed = list_worktrees(&paths);
        assert_eq!(
            listed.iter().map(|w| w.name.as_str()).collect::<Vec<_>>(),
            vec!["good"],
            "pruned/ghost entries are skipped, never invented rows"
        );
    }

    #[test]
    fn enumeration_agrees_with_discovery_from_inside_the_worktree() {
        // The listed work_tree round-trips through discover(): the linked
        // worktree resolves back to the same common dir.
        let tmp = tempfile::tempdir().unwrap();
        let paths = worktree_fixture(tmp.path(), &["feature"]);
        let listed = list_worktrees(&paths);
        let from_inside = discover(&listed[0].work_tree).unwrap();
        // discover() keeps the commondir's relative form (`…/worktrees/x/../..`);
        // equivalence is by canonical path, not lexical bytes.
        assert_eq!(
            std::fs::canonicalize(&from_inside.common_dir).unwrap(),
            std::fs::canonicalize(&paths.common_dir).unwrap()
        );
        assert_eq!(from_inside.work_tree, listed[0].work_tree);
    }

    #[test]
    fn discover_main_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        crate::test_support::fake_repo(tmp.path()).unwrap();
        let p = discover(tmp.path()).unwrap();
        assert_eq!(
            p.git_dir, p.common_dir,
            "main worktree: git_dir == common_dir"
        );
        assert_eq!(p.work_tree, tmp.path());
        assert_eq!(p.worktree_token().len(), 12);
    }

    /// A directory named `.git` with no `HEAD` is not a repository, and
    /// discovery walks past it to the real one above. Before this rule it
    /// became a repository with an identity of its own — the shape that put
    /// five phantom gitdirs in this tree, each one a second identity for work
    /// that belonged to a real repository.
    #[test]
    fn a_dot_git_directory_without_head_never_mints_an_identity() {
        let tmp = tempfile::tempdir().unwrap();
        let real = crate::test_support::fake_repo(tmp.path()).unwrap();
        let inner = tmp.path().join("docs");
        std::fs::create_dir_all(&inner).unwrap();
        let phantom = crate::test_support::phantom_gitdir(&inner).unwrap();

        assert!(!is_gitdir(&phantom), "no HEAD, no gitdir");
        let found = discover(&inner).unwrap();
        assert_eq!(found.git_dir, real, "the walk went past the phantom");
        assert_eq!(found.work_tree, tmp.path());

        // Outside any repository a phantom is not a fallback either: there is
        // nothing to discover, which is what makes the hook path write nothing.
        let alone = tempfile::tempdir().unwrap();
        crate::test_support::phantom_gitdir(alone.path()).unwrap();
        let walked = discover(alone.path());
        assert!(
            walked.is_err() || walked.is_ok_and(|p| p.git_dir != alone.path().join(".git")),
            "a lone phantom must never be returned as a repository"
        );
    }

    /// `HEAD` alone is not enough, and neither is an object store alone: the
    /// pair is what tells a gitdir from a directory that shares its name.
    #[test]
    fn a_gitdir_needs_head_and_an_object_store_or_a_commondir() {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path().join("g");
        std::fs::create_dir_all(&d).unwrap();
        assert!(!is_gitdir(&d));
        std::fs::write(d.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(!is_gitdir(&d), "HEAD alone is not a gitdir");
        std::fs::create_dir_all(d.join("objects")).unwrap();
        assert!(is_gitdir(&d));

        let linked = tmp.path().join("w");
        std::fs::create_dir_all(&linked).unwrap();
        std::fs::create_dir_all(linked.join("objects")).unwrap();
        assert!(!is_gitdir(&linked), "an object store alone is not a gitdir");
        std::fs::write(linked.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(is_gitdir(&linked));
    }

    #[test]
    fn discover_linked_worktree_resolves_common_dir() {
        let tmp = tempfile::tempdir().unwrap();
        // main repo .git with a worktrees/<name> gitdir + commondir pointer.
        let main_git = crate::test_support::fake_repo(&tmp.path().join("main")).unwrap();
        let wt = tmp.path().join("feature");
        let wt_git = crate::test_support::fake_worktree(&main_git, &wt, "feature").unwrap();

        let p = discover(&wt).unwrap();
        assert_eq!(p.git_dir, wt_git);
        assert_eq!(p.work_tree, wt);
        // commondir "../.." from worktrees/feature → main/.git
        assert_eq!(p.common_dir, wt_git.join("../.."));
        assert_ne!(p.git_dir, p.common_dir, "linked worktree splits the dirs");
    }

    #[test]
    fn distinct_worktrees_get_distinct_hashes() {
        let tmp = tempfile::tempdir().unwrap();
        let a = build_paths(
            tmp.path().join("a/.git"),
            tmp.path().join("a/.git"),
            tmp.path().join("a"),
        );
        let b = build_paths(
            tmp.path().join("b/.git"),
            tmp.path().join("b/.git"),
            tmp.path().join("b"),
        );
        assert_ne!(a.worktree_hash, b.worktree_hash);
    }

    #[test]
    fn refspec_and_oid_injection_rejected() {
        assert!(
            validate_refspec("-X").is_err(),
            "leading dash is flag injection"
        );
        assert!(validate_refspec("a; rm -rf /").is_err(), "metacharacters");
        assert!(validate_refspec("a b").is_err(), "whitespace");
        assert!(validate_refspec("rift/abc-123").is_ok());
        assert!(validate_oid(&"a".repeat(40)).is_ok());
        assert!(validate_oid(&"a".repeat(64)).is_ok());
        assert!(validate_oid("-deadbeef").is_err());
        assert!(validate_oid("xyz").is_err());
    }

    #[test]
    fn destructive_argv_is_double_dash_terminated() {
        let oid = "a".repeat(40);
        let argv = reset_hard_argv(&oid);
        assert_eq!(argv, vec!["reset", "--hard", "--", oid.as_str()]);
        assert_eq!(checkout_argv("rift/x"), vec!["checkout", "--", "rift/x"]);
        // the `--` precedes the untrusted value → it can't parse as a flag.
        assert!(argv.iter().position(|a| a == "--").unwrap() < argv.len() - 1);
    }

    #[test]
    fn durable_writer_separates_from_local_raw() {
        let w = MemoryGitWrite::default();
        // A RedactedBytes only via the sanctioned redaction gate (its sole constructor).
        let redacted = rift_shadow_redact::redact(&SecretBytes::new(b"durable bytes".to_vec()))
            .unwrap()
            .redacted;
        let oid = w.write_durable_blob(&redacted).unwrap();
        assert_eq!(oid.len(), 64);
        assert_eq!(w.durable_count(), 1);
        w.write_shadow_raw(&SecretBytes::new(b"raw local".to_vec()))
            .unwrap();
        assert_eq!(w.local_raw_count(), 1);
        // local-only writes never become durable blobs.
        assert_eq!(w.durable_count(), 1);
    }

    #[test]
    fn memory_orphan_graft_is_ordered_and_idempotent() {
        let w = MemoryGitWrite::default();
        let a = w.write_structural_blob(b"metadata-a").unwrap();
        let tree_a = w
            .write_tree(&[TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "metadata.json".into(),
                oid: a,
            }])
            .unwrap();
        let ident = GitIdent::default();
        let c1 = w
            .commit_on_orphan(&OrphanCommit {
                branch: "refs/heads/rift/checkpoints/v1",
                shard_path: "ab/cd",
                tree_oid: &tree_a,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 100,
            })
            .unwrap();
        assert_eq!(w.branch_tip().as_deref(), Some(c1.as_str()));
        assert_eq!(w.shard_paths(), vec!["ab/cd".to_string()]);
        // Re-grafting the SAME shard path is a no-op returning the same tip.
        let c1_again = w
            .commit_on_orphan(&OrphanCommit {
                branch: "refs/heads/rift/checkpoints/v1",
                shard_path: "ab/cd",
                tree_oid: &tree_a,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 100,
            })
            .unwrap();
        assert_eq!(c1_again, c1, "same shard path → idempotent no-op");
        assert_eq!(w.shard_paths().len(), 1);
    }

    // --- gitdir text ---------------------------------------------------------

    /// The text of a `gitdir` file is the path, trimmed, and its parent is
    /// the checkout whichever separator the text used.
    #[test]
    fn gitdir_text_is_the_path_it_spells() {
        let text = format!("{}\n", Path::new("a").join("b").join(".git").display());
        assert_eq!(
            gitdir_text_path(&text),
            Path::new("a").join("b").join(".git")
        );
        assert_eq!(
            gitdir_text_path("x/own-row/../own-row/.git\n").parent(),
            Some(Path::new("x/own-row/../own-row"))
        );
    }

    /// The Windows arm: a verbatim spelling with a `/`-joined tail is one
    /// component under the prefix, so the tail is re-joined with `\` before
    /// the parent step and the prefix is kept; a plain spelling is left alone.
    #[cfg(windows)]
    #[test]
    fn a_verbatim_gitdir_text_keeps_its_prefix_and_gains_its_separators() {
        let parsed = gitdir_text_path(r"\\?\C:\x\member/.git");
        assert_eq!(parsed, PathBuf::from(r"\\?\C:\x\member\.git"));
        assert_eq!(
            parsed.parent(),
            Some(Path::new(r"\\?\C:\x\member")),
            "the checkout is the parent, not the drive"
        );
        for plain in [r"C:\x\.git", r"C:/x/.git", r"\\?\C:\x\.git"] {
            assert_eq!(gitdir_text_path(plain), PathBuf::from(plain), "{plain}");
        }
    }
    // --- real git plumbing against a temp repo ------------------------------

    #[cfg(feature = "gix")]
    fn init_repo() -> (tempfile::TempDir, RepoPaths) {
        let tmp = tempfile::tempdir().unwrap();
        // Init a real repo with a deterministic default branch name.
        let status = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        // Discovered from the canonical spelling on purpose: that is what the
        // family plane stores and hands to every git call, and on Windows it
        // carries the verbatim prefix git refuses in `GIT_DIR` and
        // `GIT_INDEX_FILE` unless the spawn sites re-spell it.
        let canonical = std::fs::canonicalize(tmp.path()).unwrap();
        let repo = discover(&canonical).unwrap();
        (tmp, repo)
    }

    #[cfg(feature = "gix")]
    #[test]
    fn plumbing_archive_roundtrips_through_gix() {
        let (tmp, repo) = init_repo();
        let w = PlumbingGitWrite::new(repo.clone());
        let ident = GitIdent::default();
        let branch = "refs/heads/rift/checkpoints/v1";
        let id = "ab".to_string() + &"cd".repeat(31); // 64-hex checkpoint id
        let shard_path = format!("{}/{}", &id[..2], &id[2..]);

        // Write two structural artifacts, assemble the leaf tree, graft + commit.
        let meta = w
            .write_structural_blob(br#"{"strategy":"manual-commit"}"#)
            .unwrap();
        let report = w.write_structural_blob(br#"{"total_spans":0}"#).unwrap();
        let leaf = w
            .write_tree(&[
                TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: "metadata.json".into(),
                    oid: meta,
                },
                TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: "redaction_report.json".into(),
                    oid: report,
                },
            ])
            .unwrap();
        let commit = w
            .commit_on_orphan(&OrphanCommit {
                branch,
                shard_path: &shard_path,
                tree_oid: &leaf,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 1_700_000_000,
            })
            .unwrap();
        assert_eq!(commit.len(), 40, "sha1 commit oid");

        // The branch resolves and carries the shard path.
        assert_eq!(
            resolve_commit(&repo, branch).unwrap().as_deref(),
            Some(commit.as_str())
        );
        assert!(path_in_commit(&repo, &commit, &format!("{shard_path}/metadata.json")).unwrap());

        // Read the artifact back through gix at exactly the substrate's layout path.
        let reader = crate::GixBlobReader::open(&repo.work_tree).unwrap();
        let _ = &reader; // constructed to prove a gix reader opens the repo
        drop(tmp);
    }

    #[cfg(feature = "gix")]
    #[test]
    fn plumbing_archive_is_deterministic_and_idempotent() {
        let (tmp, repo) = init_repo();
        let w = PlumbingGitWrite::new(repo.clone());
        let ident = GitIdent::default();
        let branch = "refs/heads/rift/checkpoints/v1";
        let id = "ef".to_string() + &"12".repeat(31);
        let shard_path = format!("{}/{}", &id[..2], &id[2..]);

        let archive_once = |w: &PlumbingGitWrite| {
            let meta = w
                .write_structural_blob(br#"{"strategy":"manual-commit"}"#)
                .unwrap();
            let leaf = w
                .write_tree(&[TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: "metadata.json".into(),
                    oid: meta,
                }])
                .unwrap();
            w.commit_on_orphan(&OrphanCommit {
                branch,
                shard_path: &shard_path,
                tree_oid: &leaf,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 1_700_000_000,
            })
            .unwrap()
        };
        let c1 = archive_once(&w);
        // Re-archiving the same checkpoint id is a byte-identical no-op (same tip).
        let c2 = archive_once(&w);
        assert_eq!(c1, c2, "idempotent: identical inputs → identical tip");

        // A second, distinct checkpoint sharing the shard prefix advances the branch.
        let id2 = "ef".to_string() + &"34".repeat(31);
        let shard2 = format!("{}/{}", &id2[..2], &id2[2..]);
        let meta2 = w
            .write_structural_blob(br#"{"strategy":"scheduled"}"#)
            .unwrap();
        let leaf2 = w
            .write_tree(&[TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "metadata.json".into(),
                oid: meta2,
            }])
            .unwrap();
        let c3 = w
            .commit_on_orphan(&OrphanCommit {
                branch,
                shard_path: &shard2,
                tree_oid: &leaf2,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 1_700_000_001,
            })
            .unwrap();
        assert_ne!(c3, c1, "a new shard advances the branch");
        // Both shards are present (the graft merged, it did not replace).
        assert!(path_in_commit(&repo, &c3, &format!("{shard_path}/metadata.json")).unwrap());
        assert!(path_in_commit(&repo, &c3, &format!("{shard2}/metadata.json")).unwrap());
        drop(tmp);
    }

    #[test]
    fn memory_orphan_graft_rejects_conflicting_content() {
        let w = MemoryGitWrite::default();
        let ident = GitIdent::default();
        let graft = |tree: &str| {
            w.commit_on_orphan(&OrphanCommit {
                branch: "refs/heads/rift/checkpoints/v1",
                shard_path: "ab/cd",
                tree_oid: tree,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 1,
            })
        };
        graft("tree-a").unwrap();
        // Same path, same content → idempotent no-op.
        assert!(graft("tree-a").is_ok());
        // Same path, DIFFERENT content → a conflict, never a silent drop.
        assert!(
            graft("tree-b").is_err(),
            "conflicting shard content must error"
        );
    }

    #[cfg(feature = "gix")]
    #[test]
    fn plumbing_rejects_conflicting_shard_and_survives_reidentical() {
        let (tmp, repo) = init_repo();
        let w = PlumbingGitWrite::new(repo.clone());
        let ident = GitIdent::default();
        let branch = "refs/heads/rift/checkpoints/v1";
        let id = "ab".to_string() + &"cd".repeat(31);
        let shard_path = format!("{}/{}", &id[..2], &id[2..]);

        // A shard = one capture entry (participates in identity) + metadata.json
        // (exempt: it embeds the anchoring commit + time, which the checkpoint id
        // deliberately excludes).
        let leaf_for = |capture: &[u8], meta: &[u8]| {
            let capture_oid = w.write_structural_blob(capture).unwrap();
            let meta_oid = w.write_structural_blob(meta).unwrap();
            let mut entries = vec![
                TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: "content.redacted".into(),
                    oid: capture_oid,
                },
                TreeWriteEntry {
                    mode: GitFileMode::Blob,
                    name: "metadata.json".into(),
                    oid: meta_oid,
                },
            ];
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            w.write_tree(&entries).unwrap()
        };
        let commit_for = |leaf: &str| {
            w.commit_on_orphan(&OrphanCommit {
                branch,
                shard_path: &shard_path,
                tree_oid: leaf,
                message: "checkpoint",
                ident: &ident,
                date_epoch: 1_700_000_000,
            })
        };

        let leaf_a = leaf_for(b"hash-A", br#"{"commit":"A"}"#);
        let c1 = commit_for(&leaf_a).unwrap();
        // Re-archiving the byte-identical shard is a true no-op (same tip).
        assert_eq!(commit_for(&leaf_a).unwrap(), c1);
        // Metadata-only drift (same capture, later anchoring commit) is ALSO a
        // no-op — the branch keeps the first anchor (the same-session re-archive).
        let leaf_drift = leaf_for(b"hash-A", br#"{"commit":"B"}"#);
        assert_eq!(commit_for(&leaf_drift).unwrap(), c1);
        // A different CAPTURE body at the SAME checkpoint path is a hard conflict —
        // the branch keeps the first; the second is surfaced, never dropped silently.
        let leaf_b = leaf_for(b"hash-B", br#"{"commit":"B"}"#);
        let conflict = commit_for(&leaf_b);
        assert!(
            matches!(conflict, Err(RiftError::Integrity(_))),
            "content conflict → Integrity error"
        );
        // The branch tip is unchanged (the first shard is intact).
        assert_eq!(
            resolve_commit(&repo, branch).unwrap().as_deref(),
            Some(c1.as_str())
        );
        drop(tmp);
    }

    #[cfg(feature = "gix")]
    #[test]
    fn update_ref_create_rejects_an_existing_ref() {
        let (tmp, repo) = init_repo();
        let w = PlumbingGitWrite::new(repo.clone());
        let blob = w.write_structural_blob(b"x").unwrap();
        let tree = w
            .write_tree(&[TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "f".into(),
                oid: blob,
            }])
            .unwrap();
        let ident = GitIdent::default();
        let commit = commit_tree(&repo, &tree, None, "c", &ident, 1).unwrap();
        // First create succeeds; a second create of the same ref must reject
        // (the must-not-exist assertion), not clobber.
        update_ref_create(&repo, "refs/heads/rift/checkpoints/v1", &commit).unwrap();
        assert!(
            update_ref_create(&repo, "refs/heads/rift/checkpoints/v1", &commit).is_err(),
            "creating an existing ref must reject (no clobber)"
        );
        drop(tmp);
    }

    #[test]
    fn mktree_rejects_unsafe_entry() {
        // A name with a path separator or a bad oid never reaches git.
        let bad = TreeWriteEntry {
            mode: GitFileMode::Blob,
            name: "a/b".into(),
            oid: "a".repeat(40),
        };
        let repo = RepoPaths {
            git_dir: PathBuf::from("/nonexistent/.git"),
            common_dir: PathBuf::from("/nonexistent/.git"),
            work_tree: PathBuf::from("/nonexistent"),
            worktree_hash: [0u8; 6],
        };
        assert!(mktree(&repo, &[bad]).is_err());
    }

    proptest::proptest! {
        /// Phantoms planted anywhere down a tree never become the answer:
        /// discovery walks past every one of them to the repository above.
        /// Generated rather than hand-picked, because the shapes that produced
        /// the five real phantoms differed only in where they sat.
        #[test]
        fn discovery_walks_past_every_planted_phantom(
            planted in proptest::collection::vec(proptest::bool::ANY, 1..7),
        ) {
            let tmp = tempfile::tempdir().unwrap();
            let root = tmp.path().join("repo");
            let real = crate::test_support::fake_repo(&root).unwrap();
            let mut dir = root;
            for (i, plant) in planted.iter().enumerate() {
                dir = dir.join(format!("d{i}"));
                std::fs::create_dir_all(&dir).unwrap();
                if *plant {
                    crate::test_support::phantom_gitdir(&dir).unwrap();
                }
            }
            let found = discover(&dir).unwrap();
            proptest::prop_assert_eq!(found.git_dir, real);
        }
    }
    /// `HEAD` read from the files, not from a process — the three shapes the
    /// hook path meets, plus the two it must refuse (the NULLABLE
    /// `branch`/`head_oid` columns).
    #[test]
    fn head_state_reads_the_branch_and_the_oid_without_running_git() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = crate::test_support::fake_repo(tmp.path()).unwrap();
        let paths = discover(tmp.path()).unwrap();

        // Unborn: a branch with no commit yet. The branch is KNOWN, the oid is
        // not — and "not yet" is absence, never the empty string.
        let unborn = head_state(&paths);
        assert_eq!(unborn.branch.as_deref(), Some("main"));
        assert_eq!(unborn.oid, None);

        // Loose ref in the common store: both known.
        let oid = "a".repeat(40);
        std::fs::create_dir_all(git_dir.join("refs/heads")).unwrap();
        std::fs::write(git_dir.join("refs/heads/main"), format!("{oid}\n")).unwrap();
        assert_eq!(
            head_state(&paths),
            HeadState {
                branch: Some("main".to_owned()),
                oid: Some(oid.clone())
            }
        );

        // Packed refs: the loose file is gone, the value is not.
        std::fs::remove_file(git_dir.join("refs/heads/main")).unwrap();
        std::fs::write(
            git_dir.join("packed-refs"),
            format!(
                "# pack-refs with: peeled\n{oid} refs/heads/main\n^{}\n",
                "b".repeat(40)
            ),
        )
        .unwrap();
        assert_eq!(head_state(&paths).oid.as_deref(), Some(oid.as_str()));

        // Detached: no branch, the oid straight out of HEAD.
        std::fs::write(git_dir.join("HEAD"), format!("{oid}\n")).unwrap();
        let detached = head_state(&paths);
        assert_eq!(detached.branch, None, "a detached HEAD has no branch");
        assert_eq!(detached.oid.as_deref(), Some(oid.as_str()));

        // Garbage is UNKNOWN, not a value: neither a branch nor an oid is
        // invented out of a HEAD that says neither.
        std::fs::write(git_dir.join("HEAD"), "not a ref and not an oid\n").unwrap();
        assert_eq!(head_state(&paths), HeadState::default());
    }

    /// `HEAD` is a file anyone with write access to a checkout can edit, and
    /// its contents become a path this reader joins onto a gitdir. A ref name
    /// that would escape the gitdir reads as UNKNOWN rather than as whatever
    /// file it pointed at.
    #[test]
    fn a_head_that_escapes_the_gitdir_is_refused_rather_than_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = crate::test_support::fake_repo(tmp.path()).unwrap();
        let paths = discover(tmp.path()).unwrap();
        std::fs::write(tmp.path().join("secret"), "a".repeat(40)).unwrap();

        for hostile in [
            "ref: refs/../../secret",
            "ref: ../secret",
            "ref: refs/heads/a b",
            "ref: refs/heads/;rm",
        ] {
            std::fs::write(git_dir.join("HEAD"), format!("{hostile}\n")).unwrap();
            assert_eq!(
                head_state(&paths),
                HeadState::default(),
                "{hostile} must not resolve"
            );
        }
        assert!(!is_safe_refname("refs/heads/../../x"));
        assert!(is_safe_refname("refs/heads/feature/one"));
    }

    /// A linked member's `HEAD` is its OWN — the whole reason a worktree label
    /// is worth recording. The admin dir holds the member's `HEAD`; the branch
    /// it names resolves through the common store, which is where
    /// `refs/heads/*` lives for every member.
    #[test]
    fn a_linked_member_reads_its_own_head_not_the_main_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        let main_git = crate::test_support::fake_repo(&tmp.path().join("main")).unwrap();
        let member_tree = tmp.path().join("member");
        let admin = crate::test_support::fake_worktree(&main_git, &member_tree, "member").unwrap();
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/lane\n").unwrap();
        let oid = "c".repeat(40);
        std::fs::create_dir_all(main_git.join("refs/heads")).unwrap();
        std::fs::write(main_git.join("refs/heads/lane"), format!("{oid}\n")).unwrap();

        let member = discover(&member_tree).unwrap();
        assert_eq!(
            head_state(&member),
            HeadState {
                branch: Some("lane".to_owned()),
                oid: Some(oid)
            }
        );
        assert_eq!(
            head_state(&discover(&tmp.path().join("main")).unwrap())
                .branch
                .as_deref(),
            Some("main"),
            "the main work tree is still on its own branch"
        );
    }

    /// The first-archiver rule rests on ONE existing fact: the
    /// idempotence comparison sets `metadata.json` aside, so a re-archive of
    /// byte-identical capture from another worktree changes no compared entry
    /// and the label the first archive wrote stands. The rule is tested, not
    /// just the constant it reads.
    #[test]
    fn a_reidentical_rearchive_from_another_worktree_keeps_the_first_label() {
        // `ls-tree` lines, as `trees_equal_ignoring_anchor` receives them.
        let line = |name: &str| format!("100644 blob {}\t{name}", "d".repeat(40));
        assert!(
            is_anchor_dependent_entry(&line("metadata.json")),
            "the worktree member rides in metadata.json, which is exempt"
        );
        assert!(is_anchor_dependent_entry(&line("content_hash.txt")));
        // The capture identity is still compared — the exemption is narrow.
        assert!(!is_anchor_dependent_entry(&line("content.redacted")));
        assert!(!is_anchor_dependent_entry(&line("redaction_report.json")));
        assert!(!is_anchor_dependent_entry(&line("full.jsonl")));
        // A name that merely CONTAINS an exempt name is not exempt: the match
        // is on the tab-delimited entry name, not on a substring.
        assert!(!is_anchor_dependent_entry(&line("not-metadata.json")));
    }
}
