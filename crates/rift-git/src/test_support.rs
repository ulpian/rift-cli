//! Repository fixtures for tests, shared so every crate builds the **same**
//! shape.
//!
//! [`discover`](crate::discover) accepts a `.git` directory only when it looks
//! like a gitdir: `HEAD`, plus `objects/` or `commondir`. Before
//! that rule, fixtures across the workspace made a repository by creating an
//! empty `<root>/.git` — the same shape a stray `mkdir .git` leaves behind, and
//! the shape that minted five phantom repositories in this tree. Those fixtures
//! come here instead of each learning the rule separately, because a fixture
//! that drifts from the production check tests a repository the product would
//! refuse.
//!
//! Nothing here shells out to `git`: the files are the ones `git init` and
//! `git worktree add` write, by hand, so a test needs no git binary and no
//! network.
//!
//! Available to other crates behind the `test-support` feature (a
//! dev-dependency on `rift-git` with `features = ["test-support"]`); it is off
//! by default, so none of this reaches a shipped binary.

use std::path::{Path, PathBuf};

/// The `HEAD` an unborn repository has: a symbolic ref to a branch with no
/// commits yet. Byte-identical to what `git init` writes.
const UNBORN_HEAD: &str = "ref: refs/heads/main\n";

/// Make `git_dir` itself a gitdir: `HEAD`, `objects/` and `refs/`. Use this
/// for the shapes that are **not** a `<root>/.git` directory — a bare
/// repository, a `--separate-git-dir` target, a submodule's `modules/<name>`.
///
/// # Errors
/// [`std::io::Error`] if the directories or `HEAD` cannot be written.
pub fn fake_gitdir(git_dir: &Path) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(git_dir.join("objects"))?;
    std::fs::create_dir_all(git_dir.join("refs"))?;
    std::fs::write(git_dir.join("HEAD"), UNBORN_HEAD)?;
    Ok(git_dir.to_path_buf())
}

/// Make `<root>/.git` a directory [`discover`](crate::discover) accepts: an
/// ordinary main work tree. Returns the gitdir.
///
/// # Errors
/// [`std::io::Error`] if the directories or `HEAD` cannot be written.
pub fn fake_repo(root: &Path) -> std::io::Result<PathBuf> {
    fake_gitdir(&root.join(".git"))
}

/// Add a linked worktree to a [`fake_repo`]: the administrative directory
/// under `<main_git>/worktrees/<name>` with its `gitdir`, `commondir` and
/// `HEAD`, and the member checkout at `work_tree` with its `.git` **file**.
/// Returns the administrative gitdir.
///
/// This is the exact file contract `git worktree add` writes, which is what
/// makes a fixture built from it a real member of a real family.
///
/// # Errors
/// [`std::io::Error`] if any of those files cannot be written.
pub fn fake_worktree(main_git: &Path, work_tree: &Path, name: &str) -> std::io::Result<PathBuf> {
    let admin = main_git.join("worktrees").join(name);
    std::fs::create_dir_all(&admin)?;
    std::fs::create_dir_all(work_tree)?;
    let dot_git = work_tree.join(".git");
    std::fs::write(&dot_git, format!("gitdir: {}\n", admin.display()))?;
    std::fs::write(admin.join("gitdir"), format!("{}\n", dot_git.display()))?;
    std::fs::write(admin.join("commondir"), "../..\n")?;
    std::fs::write(admin.join("HEAD"), UNBORN_HEAD)?;
    Ok(admin)
}

/// Make `<root>/.git` the shape hardened discovery must **refuse**: a directory
/// named `.git` with no `HEAD`. This is a phantom gitdir — what a hook fired
/// with a cwd outside any repository used to mint — and it is a fixture rather
/// than an accident so the refusal has something to be tested against.
///
/// # Errors
/// [`std::io::Error`] if the directory cannot be created.
pub fn phantom_gitdir(root: &Path) -> std::io::Result<PathBuf> {
    let git_dir = root.join(".git");
    std::fs::create_dir_all(&git_dir)?;
    Ok(git_dir)
}
