//! The family view: how a list of registry entries becomes the rows the
//! dashboard shows, and how a path resolves to the repository family that
//! owns its one row.
//!
//! Its own module because the catalogue and every profile view project
//! through the same function, so the child-row semantics cannot diverge
//! between them, and because family resolution is read-only discovery that
//! writes nothing into the repository being registered.

use std::path::Path;

use rift_canopy::{EntryKind, Project, ProjectEntry, ProjectStatus};

/// The repository family a checkout belongs to, as the one-row-per-family rule
/// needs to see it (the family view).
///
/// The family **is** its common dir: two paths are checkouts of one
/// repository exactly when they share it. Resolved by discovery only — no
/// manifest is read and none is minted, because registering a project must not
/// write into the repository being registered.
pub(super) struct FamilyOf {
    /// The canonical common dir: this family's identity.
    common_dir: std::path::PathBuf,
    /// Every checkout of the family, canonical: the main work tree and each
    /// linked worktree, including one whose directory is gone (it still holds
    /// a row's worth of identity).
    members: Vec<std::path::PathBuf>,
    /// The main work tree, where the family has a provable one.
    main: Option<std::path::PathBuf>,
}

impl FamilyOf {
    /// Whether a registered row's `path` is a checkout of this family.
    pub(super) fn holds(&self, path: &str) -> bool {
        let canonical = comparable(path);
        self.members.contains(&canonical)
    }

    /// Whether `path` is the family's main work tree — the one path the family view
    /// lets supersede a member's row.
    pub(super) fn is_root(&self, path: &Path) -> bool {
        self.main.as_deref() == Some(path)
    }
}

/// Project a list of registry entries onto Canopy's [`Project`] view (statuses
/// start `Idle`; the live bus refines them), with each repository's **linked
/// worktrees** listed as child rows directly under it. Worktree rows
/// are derived from git — never persisted; a worktree that is itself a listed
/// entry keeps its own row instead. Shared by the catalogue and every profile
/// view, so the child-row semantics can never diverge between them.
///
/// The children come from `family_members` rather than `list_worktrees`
/// (the family view, which refines that listing here). Two consequences, both deliberate:
///
/// * A **`prunable`** member — the checkout deleted, the administrative entry
///   still there — now has a row. `list_worktrees` skips it on the grounds that
///   "a row for a nonexistent path would be an invented lie", which was right
///   while nothing could say what was wrong with it; the bus now reads such a
///   row as `Error(RepoMissing)` whose hint names the one act that clears it,
///   and a member the operator half-removed is a fact worth showing rather than
///   one worth hiding.
/// * Both sides of the "it has its own registry row" comparison are now
///   **canonical**: `CanopyStore::add` canonicalises a registered path on the
///   way in, and `family_members` canonicalises each checkout, where
///   `list_worktrees` returned whatever `<admin>/gitdir` happened to spell. A
///   worktree added through a symlink or a path with a `..` segment therefore
///   used to get BOTH its own registry row and a duplicate child under its
///   repository — the self-comparison defect, in the one place it had
///   not yet been closed.
pub(super) fn view_with_worktrees(entries: Vec<ProjectEntry>) -> Vec<Project> {
    let listed: std::collections::BTreeSet<std::path::PathBuf> = entries
        .iter()
        .map(|e| std::path::PathBuf::from(&e.path))
        .collect();
    let mut out = Vec::new();
    for e in entries {
        let repo_path = std::path::PathBuf::from(&e.path);
        out.push(Project {
            name: e.label,
            path: e.path,
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        });
        // Children: this repository's linked worktrees (best-effort — a
        // non-git or unreadable repo simply has none).
        let Ok(paths) = rift_git::discover(&repo_path) else {
            continue;
        };
        for member in rift_git::family_members(&paths) {
            if member.is_main {
                continue; // the repository's own row, already pushed
            }
            if listed.contains(&member.work_tree) {
                continue; // it has its own registry row
            }
            out.push(Project {
                name: member.name,
                path: member.work_tree.to_string_lossy().into_owned(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Worktree,
            });
        }
    }
    out
}

/// Resolve `path`'s family, or `None` when it is in no repository at all (a
/// plain directory is still registrable, and dedups by path alone).
pub(super) fn family_of(path: &Path) -> Option<FamilyOf> {
    let paths = rift_git::discover(path).ok()?;
    let common_dir = std::fs::canonicalize(&paths.common_dir).ok()?;
    let members = rift_git::family_members(&paths);
    Some(FamilyOf {
        common_dir,
        main: members
            .iter()
            .find(|member| member.is_main)
            .map(|member| member.work_tree.clone()),
        members: members.into_iter().map(|member| member.work_tree).collect(),
    })
}

/// One entry per repository family, for the legacy import.
///
/// The family **root** wins where the file names it, whatever order it is
/// listed in; otherwise the first entry of the family keeps the seat, so the
/// import is a pure function of the file and re-running it cannot shuffle the
/// dashboard. An entry in no repository is never deduplicated — there is no
/// family to compare it against.
pub(super) fn one_row_per_family(
    entries: &[(String, std::path::PathBuf)],
) -> Vec<&(String, std::path::PathBuf)> {
    let mut chosen: Vec<(Option<std::path::PathBuf>, &(String, std::path::PathBuf))> = Vec::new();
    for entry in entries {
        let Some(family) = family_of(&entry.1) else {
            chosen.push((None, entry));
            continue;
        };
        if let Some(slot) = chosen
            .iter_mut()
            .find(|(key, _)| key.as_ref() == Some(&family.common_dir))
        {
            if family.is_root(&comparable(&entry.1)) {
                slot.1 = entry;
            }
            continue;
        }
        chosen.push((Some(family.common_dir), entry));
    }
    chosen.into_iter().map(|(_, entry)| entry).collect()
}

/// A path as the registry compares it: canonical where the directory resolves,
/// the recorded spelling where it does not (an unmounted volume still has a
/// row, and no canonical form can be invented for it).
pub(super) fn comparable(path: impl AsRef<Path>) -> std::path::PathBuf {
    rift_git::family::canonical_or_lexical(path.as_ref())
}

#[cfg(test)]
mod tests {
    use rift_canopy::PersistOp;

    use super::*;
    use crate::store::fixtures::{add_worktree_fixture, make_dir, seed_legacy_row, store};

    #[test]
    fn linked_worktrees_list_as_child_rows_under_their_repo() {
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        rift_git::test_support::fake_repo(std::path::Path::new(&repo)).unwrap();
        let wt_path = add_worktree_fixture(std::path::Path::new(&repo), "feature-wt");
        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();

        let projects = cs.projects().unwrap();
        assert_eq!(projects.len(), 2, "repo + its worktree child");
        assert_eq!(projects[0].path, repo);
        assert_eq!(projects[0].kind, EntryKind::Repo);
        assert_eq!(projects[1].name, "feature-wt");
        assert_eq!(projects[1].path, wt_path);
        assert_eq!(
            projects[1].kind,
            EntryKind::Worktree,
            "the child row carries the reserved Worktree kind"
        );
    }

    #[test]
    fn a_prunable_member_still_gets_a_row_to_be_missing_in() {
        // `git worktree remove`'s other half, and the row the family view asks for:
        // the checkout is deleted, the administrative entry survives, git
        // calls it prunable. Before this the rail simply stopped listing it,
        // so a half-removed member was indistinguishable from one that had
        // never existed and nothing on screen could tell the operator that a
        // `git worktree prune` was owed.
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        rift_git::test_support::fake_repo(std::path::Path::new(&repo)).unwrap();
        add_worktree_fixture(std::path::Path::new(&repo), "alive");
        let gone = add_worktree_fixture(std::path::Path::new(&repo), "gone");
        std::fs::remove_dir_all(&gone).unwrap();
        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();

        let projects = cs.projects().unwrap();
        let names: Vec<&str> = projects.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["repo", "alive", "gone"],
            "the member whose checkout is gone keeps its row"
        );
        assert_eq!(projects[2].path, gone, "named by the path it had");
        assert_eq!(projects[2].kind, EntryKind::Worktree);
        // The status is the live bus's to derive (`Error(RepoMissing)`, whose
        // worktree-row hint says prunable); the registry view starts every row
        // `Idle` and says nothing it has not read.
        assert_eq!(projects[2].status, ProjectStatus::Idle);
    }

    #[test]
    fn a_child_is_recognised_as_its_own_row_whatever_its_gitdir_file_spells_it() {
        // The dedup compares canonically, and the drift it closes is on the
        // WORKTREE side: a registered path is canonicalised on the way in
        // (`add`), while a member's checkout is whatever `<admin>/gitdir`
        // names — which for a checkout reached through a symlink, or added
        // from a path with a `.` segment, is not the canonical spelling. A raw
        // string compare then gave that project BOTH its own row and a
        // duplicate child under its repository. Written without a symlink so
        // it runs on every platform CI builds.
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        rift_git::test_support::fake_repo(std::path::Path::new(&repo)).unwrap();
        let wt = add_worktree_fixture(std::path::Path::new(&repo), "own-row");
        // Respell the administrative pointer the way git records it when the
        // worktree was added through a non-canonical path.
        let admin = std::path::Path::new(&repo).join(".git/worktrees/own-row");
        let parent = std::path::Path::new(&wt).parent().unwrap().to_path_buf();
        // A `..` hop, not a `.` one: `Path` equality already normalises `.`
        // away, so only a spelling that survives component comparison and
        // differs after `canonicalize` states the claim (a symlinked checkout
        // is the real-world case, and needs a platform this cannot assume).
        // Spelled from a non-verbatim base: under a `\\?\` prefix only `\`
        // separates, so a `/`-joined tail would be one component and the
        // hop would collapse the checkout to the temp root. Git itself never
        // writes the verbatim form into `gitdir`.
        let parent = rift_git::child_process_path(&parent);
        std::fs::write(
            admin.join("gitdir"),
            format!("{p}/own-row/../own-row/.git\n", p = parent.display()),
        )
        .unwrap();

        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();
        // A row `AddProject` would now refuse (the family view), seeded as the old
        // build left it: the rule under test is how such a row RENDERS.
        seed_legacy_row(&cs, &wt, "own-row", 2);

        let projects = cs.projects().unwrap();
        assert_eq!(
            projects.len(),
            2,
            "the repo and the worktree's own row, no duplicate child: {:?}",
            projects.iter().map(|p| &p.path).collect::<Vec<_>>()
        );
        assert_eq!(projects[1].path, wt);
        assert_eq!(projects[1].kind, EntryKind::Repo, "it is a registry row");
    }

    #[test]
    fn repo_without_worktrees_renders_exactly_as_today() {
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "plain");
        rift_git::test_support::fake_repo(std::path::Path::new(&repo)).unwrap();
        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();
        let projects = cs.projects().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].kind, EntryKind::Repo);
    }

    #[test]
    fn a_registered_worktree_keeps_its_own_row_not_a_duplicate_child() {
        let (dir, cs) = store();
        let repo = make_dir(dir.path(), "repo");
        rift_git::test_support::fake_repo(std::path::Path::new(&repo)).unwrap();
        let wt_path = add_worktree_fixture(std::path::Path::new(&repo), "wt-own");
        cs.apply(&PersistOp::AddProject { path: repo.clone() }, 1)
            .unwrap();
        // Written before the one-row-per-family rule, as above: the registration is
        // refused today, the row
        // it left behind is not.
        seed_legacy_row(&cs, &wt_path, "wt-own", 2);

        let projects = cs.projects().unwrap();
        let wt_rows: Vec<_> = projects.iter().filter(|p| p.path == wt_path).collect();
        assert_eq!(
            wt_rows.len(),
            1,
            "no duplicate row for a registered worktree"
        );
        assert_eq!(
            wt_rows[0].kind,
            EntryKind::Repo,
            "its own registry row wins over the derived child"
        );
    }
}
