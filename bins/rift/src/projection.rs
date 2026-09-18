//! Opening the projection, and the facts every command family asks first.
//!
//! The init guard, the worktree root a repo-scoped verb answers for, a
//! project's display name, the store openers (always through the
//! checksum-locked migration engine, or SQLite-enforced read-only for the
//! viewer tier), and the short-lived runtime the async store seam runs on.
//! One module, so the two projection openers stay the only two and a caller
//! cannot reach the store another way.

use std::path::Path;

use crate::cli::{exit, Outcome};
use crate::{agents, family, hooks};

/// The worktree root for repo-scoped legs: walk up from `start` through the
/// rift-git discovery seam, so a subdirectory invocation reports the REPO,
/// never a false-empty cwd — the zero-checkpoints incident class, resurfaced.
/// Outside any repo, `start` passes through unchanged and the legs report
/// their honest empties.
pub(crate) fn worktree_root(start: &Path) -> std::path::PathBuf {
    rift_git::discover(start).map_or_else(|_| start.to_path_buf(), |p| p.work_tree)
}

/// Derive a project's display name from its path (directory basename).
pub(crate) fn project_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

/// Run a read-only projection query: guard on init state, open the store + a
/// runtime, and hand both to `f` (which drives the async query to completion).
pub(crate) fn projection_read<F>(root: &Path, f: F) -> rift_core::Result<Outcome>
where
    F: FnOnce(&rift_store::SqliteStore, &tokio::runtime::Runtime) -> rift_core::Result<Outcome>,
{
    if let Some(o) = not_init_guard(root) {
        return Ok(o);
    }
    // Preserve the established standalone CLI contract: initialized legacy
    // repositories are lazily brought onto the current projection schema
    // before ordinary read commands run.
    let store = open_projection(root)?;
    let rt = runtime()?;
    f(&store, &rt)
}

/// Run a palette/viewer projection query against an already-existing database
/// with SQLite-enforced read-only access. Unlike [`projection_read`], this
/// cannot create or migrate state; that distinction is what lets navigating
/// into Recall Viewer from the ordinary dashboard retain the zero-write
/// contract without changing standalone CLI compatibility.
///
/// Reached from one caller: the palette's dispatcher, which only an
/// interactive dashboard has, so a build without one does not carry it.
#[cfg(feature = "tui")]
pub(crate) fn projection_read_strict<F>(root: &Path, f: F) -> rift_core::Result<Outcome>
where
    F: FnOnce(&rift_store::SqliteStore, &tokio::runtime::Runtime) -> rift_core::Result<Outcome>,
{
    if let Some(o) = not_init_guard(root) {
        return Ok(o);
    }
    let store = open_projection_read_only(root)?;
    let rt = runtime()?;
    f(&store, &rt)
}

/// `Some(NOT_INIT outcome)` when the cwd is not set up on this machine.
///
/// Three causes share the exit code (scripts key on it) and differ in the
/// sentence, because each asks for a different act: a repository never
/// initialized anywhere, a repository whose committed `.rift/` arrived with
/// the clone but whose family has no projection here, and a directory
/// outside any repository, which no `rift init` can give one.
pub(crate) fn not_init_guard(root: &Path) -> Option<Outcome> {
    if is_initialized(root) {
        return None;
    }
    let (human, reason) = if rift_git::discover(root).is_err() {
        (
            "not inside a git repository — Rift records nothing outside one",
            "not_a_repository",
        )
    } else if root.join(".rift").is_dir() {
        (
            "configured by the repository, not set up on this machine (run `rift init`)",
            "configured_not_provisioned",
        )
    } else {
        ("not a rift repo (run `rift init`)", "never_initialized")
    };
    Some(Outcome::coded(
        exit::NOT_INIT,
        human,
        serde_json::json!({ "error": "not_initialized", "reason": reason }),
    ))
}

/// Whether `root` has been initialized on this machine
/// ([`family::provisioned_here`]: the family's projection exists here).
fn is_initialized(root: &Path) -> bool {
    family::provisioned_here(root)
}

/// Open (creating + migrating if needed) the per-repo projection store through
/// the checksum-locked migration engine, for an explicit repository root.
pub(crate) fn open_projection(root: &Path) -> rift_core::Result<rift_store::SqliteStore> {
    rift_store::open_capture_projection(hooks::projection_db_at(root)?)
}

/// Open an existing per-repository projection with SQLite-enforced read-only
/// access. No creation or migration is possible through this handle.
pub(crate) fn open_projection_read_only(root: &Path) -> rift_core::Result<rift_store::SqliteStore> {
    rift_store::SqliteStore::open_read_only(hooks::projection_db_at(root)?)
}

/// `rift enable --agent`: wire one native integration. Opens the projection
/// without the init guard — enabling may run before `init` and create
/// `.rift/` — which is why the command line and the palette both route here
/// rather than through [`projection_read`].
pub(crate) fn enable_agent(root: &Path, agent: &str) -> rift_core::Result<Outcome> {
    let store = open_projection(root)?;
    runtime()?.block_on(agents::enable(&store, root, agent, hooks::now_ms()))
}

/// `rift disable --agent`: the same open, for the removal.
pub(crate) fn disable_agent(root: &Path, agent: &str) -> rift_core::Result<Outcome> {
    let store = open_projection(root)?;
    runtime()?.block_on(agents::disable(&store, root, agent, hooks::now_ms()))
}

/// A short-lived current-thread async runtime for the store seam.
pub(crate) fn runtime() -> rift_core::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_root_discovers_the_repo_from_a_subdirectory() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let sub = repo.join("a").join("b");
        // A real gitdir (HEAD + objects/): hardened discovery skips
        // a bare directory that merely shares the name.
        rift_git::test_support::fake_repo(&repo).unwrap();
        std::fs::create_dir_all(&sub).unwrap();
        assert_eq!(worktree_root(&sub), repo, "walks up to the worktree root");
        assert_eq!(worktree_root(&repo), repo, "the root maps to itself");
        // Outside any repo: passthrough — the legs report honest empties.
        let bare = tmp.path().join("norepo");
        std::fs::create_dir_all(&bare).unwrap();
        assert_eq!(worktree_root(&bare), bare);
    }
}
