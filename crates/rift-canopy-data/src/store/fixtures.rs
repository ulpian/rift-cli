//! Test fixtures the store modules share: a store over a tempdir, a real
//! directory to register, a linked worktree beside a repository, a registry
//! row written the way a build before the family rule wrote one, and the
//! seeded default profile.
//!
//! A separate module because the catalogue, the family view and every
//! profile-scoped relation register the same directories, and a fixture
//! copied per module drifts.

use std::path::Path;

use rift_core::{ProjectId, UlidSource};
use rift_git::repo_id_for;

use super::catalogue::EventRow;
use super::CanopyStore;

pub(super) fn store() -> (tempfile::TempDir, CanopyStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = CanopyStore::open(dir.path()).unwrap();
    (dir, store)
}

/// Create `name` as a real directory under `root` and return its canonical path.
pub(super) fn make_dir(root: &Path, name: &str) -> String {
    let p = root.join(name);
    std::fs::create_dir_all(&p).unwrap();
    std::fs::canonicalize(&p)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Add a linked worktree beside `repo`, through the one builder that writes
/// the file layout `git worktree add` writes (family identity). Hand-building
/// it here is how this fixture used to drift from the shape the product
/// accepts — the admin dir had no `HEAD`, and neither did `<repo>/.git`.
pub(super) fn add_worktree_fixture(repo: &Path, name: &str) -> String {
    let wt = repo.parent().unwrap().join(name);
    rift_git::test_support::fake_worktree(&repo.join(".git"), &wt, name).unwrap();
    wt.to_string_lossy().into_owned()
}

/// A registry row written the way a build **before** the family view wrote one:
/// straight into the log, past the family rule `add` now enforces.
///
/// The door is closed; the rows are not. They are on every machine that ran
/// the old build and the log is append-only, so the family view's rendering
/// rules still have to hold for them — which is exactly what the two `views`
/// tests using this helper have always asserted. Seeding them any other way
/// would be asserting a state the product can no longer be in.
pub(super) fn seed_legacy_row(cs: &CanopyStore, path: &str, label: &str, now_ms: i64) {
    let project_id = ProjectId::new(cs.ulids.next_ulid()).to_string();
    let repo = repo_id_for(Path::new(path)).unwrap();
    cs.append(
        &project_id,
        &EventRow::added(repo.as_str(), path, label, now_ms),
    )
    .unwrap();
}

/// Seed and return the active (default) profile's id.
pub(super) fn seed(cs: &CanopyStore, now_ms: i64) -> String {
    cs.seed_default_profile(now_ms).unwrap();
    cs.profiles_view().unwrap().0.unwrap()
}
