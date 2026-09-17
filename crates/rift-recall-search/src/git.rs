//! The two things search asks git: what `HEAD` is, and what the developer has
//! in flight.
//!
//! Both go through gix, and both fail into silence. A repository that cannot be
//! opened, a `HEAD` that is unborn, a status that errors halfway — each yields
//! the empty answer, which ranks every file as though nothing were in flight
//! and rebuilds the index against an empty `HEAD` oid. Neither can invent a
//! path, and neither can make a stale index look fresh: the empty oid is a
//! value like any other, and an index taken under it is invalidated by the
//! first commit exactly as any other index would be.
//!
//! No `git2`, no shelling out, and nothing here is on the hook path.

use std::collections::BTreeSet;
use std::path::Path;

/// `HEAD`'s object id, or the empty string where there is not one.
///
/// The empty string covers three cases that mean the same thing to an index —
/// no repository, no commits yet, an unreadable one — and all three are safe:
/// the key still changes the moment a commit exists.
#[must_use]
pub fn head_oid(work_tree: &Path) -> String {
    let Ok(repo) = gix::open(work_tree) else {
        return String::new();
    };
    repo.head_id()
        .map(|id| id.to_hex().to_string())
        .unwrap_or_default()
}

/// What the developer is working on right now.
///
/// Collected by whoever can afford it and handed to the engine, exactly as the
/// frecency facts are: a caller with a tight budget passes
/// [`StatusFacts::default`] and gets a ranking that knows nothing about the
/// working tree, rather than a ranking that arrives too late to be used.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatusFacts {
    /// Tracked files whose worktree contents differ from the index.
    pub modified: BTreeSet<String>,
    /// Files the working tree holds that git does not track.
    pub untracked: BTreeSet<String>,
    /// Files whose index entry differs from `HEAD`.
    pub staged: BTreeSet<String>,
}

impl StatusFacts {
    /// Ask gix what has changed in this checkout.
    ///
    /// Never fails: every error path leaves the sets it had already filled and
    /// stops, because a partial status ranks slightly worse and an absent one
    /// ranks like an untouched tree.
    #[must_use]
    pub fn collect(work_tree: &Path) -> Self {
        let mut facts = Self::default();
        let Ok(repo) = gix::open(work_tree) else {
            return facts;
        };
        let Ok(platform) = repo.status(gix::progress::Discard) else {
            return facts;
        };
        // Every untracked file by name, not the collapsed directory git's own
        // porcelain prints. A ranking is about paths: "src/ is untracked" is a
        // sentence, and what a caller needs to know is whether the path in
        // front of it is one of them.
        let platform = platform.untracked_files(gix::status::UntrackedFiles::Files);
        let Ok(iter) = platform.into_iter(std::iter::empty()) else {
            return facts;
        };
        for item in iter.flatten() {
            let path = item.location().to_string();
            match &item {
                gix::status::Item::IndexWorktree(
                    gix::status::index_worktree::Item::DirectoryContents { .. },
                ) => facts.untracked.insert(path),
                gix::status::Item::IndexWorktree(_) => facts.modified.insert(path),
                gix::status::Item::TreeIndex(_) => facts.staged.insert(path),
            };
        }
        facts
    }

    /// Whether this path is one git does not track.
    #[must_use]
    pub fn is_untracked(&self, path: &str) -> bool {
        self.untracked.contains(path)
    }

    /// Whether this path has changed in the working tree or the index.
    #[must_use]
    pub fn is_changed(&self, path: &str) -> bool {
        self.modified.contains(path) || self.staged.contains(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory that is not a repository is not an error: it has no `HEAD`
    /// and nothing in flight, which is exactly what an index taken over it
    /// should be keyed by.
    #[test]
    fn a_directory_that_is_not_a_repository_answers_emptily() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(head_oid(dir.path()), "");
        assert_eq!(StatusFacts::collect(dir.path()), StatusFacts::default());
    }

    /// A real (if empty) repository, with a file in it git has never seen:
    /// gix reports it as untracked, and that is what a ranking may lean on.
    #[test]
    fn status_reads_what_git_would_report() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        rift_git::test_support::fake_repo(root).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/scratch.rs"), "fn f() {}\n").unwrap();

        let facts = StatusFacts::collect(root);
        assert!(
            facts.is_untracked("src/scratch.rs"),
            "gix saw nothing in a repository that has a file in it: {facts:?}"
        );
        assert!(!facts.is_changed("src/scratch.rs"));
        assert_eq!(head_oid(root), "", "an unborn HEAD is the empty key");
    }

    /// The three sets are three different questions, and a caller reads them
    /// through the two predicates rather than by guessing which set a path is
    /// in.
    #[test]
    fn status_answers_tracked_and_changed_separately() {
        let facts = StatusFacts {
            modified: BTreeSet::from(["src/bus.rs".to_owned()]),
            untracked: BTreeSet::from(["notes.md".to_owned()]),
            staged: BTreeSet::from(["Cargo.toml".to_owned()]),
        };
        assert!(facts.is_changed("src/bus.rs"));
        assert!(facts.is_changed("Cargo.toml"));
        assert!(!facts.is_changed("notes.md"));
        assert!(facts.is_untracked("notes.md"));
        assert!(!facts.is_untracked("src/bus.rs"));
    }
}
