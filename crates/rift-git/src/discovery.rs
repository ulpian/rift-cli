//! Repository discovery: which gitdir, common dir and work tree a path belongs to,
//! and the linked worktrees a repository has.

use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};
use sha2::{Digest, Sha256};

use crate::family;
use crate::to_hex;

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
///
/// Public because a hook decision needs the family's directory and has only
/// the git dir in hand: one small file read, where [`discover`] would walk
/// the work tree as well.
#[must_use]
pub fn read_common_dir(git_dir: &Path) -> PathBuf {
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
}
