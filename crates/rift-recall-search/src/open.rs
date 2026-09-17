//! Getting an index in hand, cold or warm, without ever making a caller wait.
//!
//! There is no resident process here by ruling, so the index is a file, and a
//! caller that wants one is in one of three situations:
//!
//! - the file is there and still answers for this checkout — load it;
//! - it is missing, damaged or stale, and the tree is small enough to walk
//!   inside the budget — walk it, answer, and let the caller save the result;
//! - it is missing and the tree is *not* small enough — answer nothing, and let
//!   the caller do what it was going to do anyway while the detached worker
//!   builds the index for next time.
//!
//! The third case is the one that matters. A search lever exists to save an
//! agent a round trip; a lever that spends 900 ms deciding whether it can help
//! has already cost more than it saves. So the budget is checked *during* the
//! walk rather than after it, and a walk that runs out of time is abandoned
//! rather than finished and discarded.

use std::path::Path;
use std::time::{Duration, Instant};

use crate::index::{IndexKey, PathIndex};
use crate::walk;
use crate::SearchError;

/// What a caller may spend before deciding the index is not available.
///
/// The number the search levers are held to: a repository whose cold walk fits
/// in it gets ranked answers from the first query of a session, and one that
/// does not gets them from the second, after the worker has been round.
pub const COLD_BUDGET: Duration = Duration::from_millis(60);

/// The result of asking for this checkout's index.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Opened {
    /// The saved index answers for this checkout.
    Fresh(PathIndex),
    /// No saved index answered, and this one was walked inside the budget. It
    /// has not been written to disk: the caller decides whether it is worth
    /// saving, because a caller on a latency budget may not want to.
    Cold(PathIndex),
    /// No index, and not enough budget to build one. The caller runs the tool
    /// it was going to run and asks the worker for a rebuild.
    Absent,
}

impl Opened {
    /// The index, whichever way it arrived.
    #[must_use]
    pub fn index(&self) -> Option<&PathIndex> {
        match self {
            Self::Fresh(index) | Self::Cold(index) => Some(index),
            Self::Absent => None,
        }
    }
}

/// Get this checkout's index, within `budget`.
///
/// The head oid is passed in rather than read here so that a caller which
/// already knows it — every hook does — does not pay to open the repository
/// twice, and so that this function has no dependency on git at all.
#[must_use]
pub fn open_for(
    work_tree: &Path,
    head_oid: &str,
    sensitive: &[String],
    budget: Duration,
) -> Opened {
    // One `mkdir -p` before the key is taken, for the same reason the rebuild
    // does it: the directory Rift writes its index into sits under one the
    // walk counts, so a key taken before it exists describes a tree that the
    // first save would change. Failure is ignored — a checkout Rift cannot
    // write to still gets a cold walk and a ranking.
    let _ = std::fs::create_dir_all(crate::state_dir(work_tree));
    let deadline = Instant::now() + budget;
    // The probe is inside the budget too. On a tree big enough for the walk to
    // be unaffordable, deciding whether the saved index is stale is
    // unaffordable as well, and a caller that spent its whole budget finding
    // that out would have spent it for nothing.
    let Some(key) = walk::current_key_within(work_tree, head_oid, Some(deadline)) else {
        return Opened::Absent;
    };
    if let Some(index) = PathIndex::load(&crate::index_path(work_tree), &key) {
        // Loading beat the budget or it did not; either way the answer is
        // already in hand, and throwing it away to honour a deadline would
        // spend the walk's cost for nothing.
        return Opened::Fresh(index);
    }
    if Instant::now() >= deadline {
        return Opened::Absent;
    }
    match walk::walk_within(work_tree, sensitive, Some(deadline)) {
        Some(walked) => PathIndex::new(
            IndexKey {
                head_oid: head_oid.to_owned(),
                newest_dir_mtime_ms: walked.newest_dir_mtime_ms,
            },
            walked.entries,
        )
        .map_or(Opened::Absent, Opened::Cold),
        None => Opened::Absent,
    }
}

/// Build this checkout's index and save it, with no deadline at all.
///
/// What the detached worker runs. Nobody is waiting on it, so it walks the
/// whole tree; and it saves, because the point of the pass is precisely that
/// the next caller finds a file rather than a walk.
///
/// # Errors
///
/// [`SearchError::TooManyPaths`] for a tree over the cap, and
/// [`SearchError::Write`] where the index could not be written — which costs
/// the saving and nothing else, since the caller was not waiting for it.
pub fn rebuild(
    work_tree: &Path,
    head_oid: &str,
    sensitive: &[String],
) -> Result<PathIndex, SearchError> {
    // Before the walk, not after it. The state directory is pruned from the
    // walk, but *creating* it changes the modification time of the `.rift`
    // directory above it — which is not pruned — so a first rebuild that made
    // the directory afterwards would key its index to a tree that no longer
    // existed by the time it finished writing, and every first rebuild in a
    // checkout would be thrown away unread.
    let _ = std::fs::create_dir_all(crate::state_dir(work_tree));
    let index = walk::build_index(work_tree, head_oid, sensitive)?;
    index.save(&crate::index_path(work_tree))?;
    Ok(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    /// The saved index is used when it answers, rebuilt inline when it does
    /// not, and a saved index for another state is not used at all.
    #[test]
    fn a_saved_index_is_used_only_while_it_answers() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/main.rs", "fn main() {}\n");
        let head = "a".repeat(40);

        assert!(
            matches!(
                open_for(root, &head, &[], COLD_BUDGET),
                Opened::Cold(index) if index.entries().len() == 1
            ),
            "the first query walks"
        );

        let built = rebuild(root, &head, &[]).unwrap();
        let opened = open_for(root, &head, &[], COLD_BUDGET);
        assert_eq!(
            opened.index(),
            Some(&built),
            "the rebuild keyed its index to a tree its own directory had \
             already changed"
        );
        assert!(matches!(opened, Opened::Fresh(_)), "the saved index");
        assert_eq!(opened.index().unwrap().entries().len(), 1);

        // A commit moves the key, and the file that no longer answers is not
        // used even though it is perfectly readable.
        assert!(matches!(
            open_for(root, &"b".repeat(40), &[], COLD_BUDGET),
            Opened::Cold(_)
        ));
    }

    /// An index built inline can be saved by the caller and loaded by the
    /// next one: the state directory exists before the key is taken, so the
    /// save cannot invalidate what it saves.
    #[test]
    fn an_index_built_inline_can_be_saved_for_the_next_caller() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/main.rs", "fn main() {}\n");
        let Opened::Cold(cold) = open_for(root, "a", &[], COLD_BUDGET) else {
            panic!("the first query walks");
        };
        cold.save(&crate::index_path(root)).unwrap();
        assert_eq!(open_for(root, "a", &[], COLD_BUDGET), Opened::Fresh(cold));
    }

    /// A budget that has already run out starts no walk — and still uses an
    /// index that is already there.
    ///
    /// The two halves are deliberately different. Walking is the expensive
    /// half and it is refused; reading a file that answers is the cheap half,
    /// and refusing it to honour a deadline would throw away the answer the
    /// budget existed to protect. The probe is bounded too, so a tree big
    /// enough for the walk to be unaffordable cannot spend the whole budget
    /// merely deciding that.
    #[test]
    fn a_walk_that_cannot_fit_the_budget_is_not_started() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/main.rs", "fn main() {}\n");
        assert_eq!(open_for(root, "a", &[], Duration::ZERO), Opened::Absent);
        let built = rebuild(root, "a", &[]).unwrap();
        assert_eq!(
            open_for(root, "a", &[], Duration::ZERO),
            Opened::Fresh(built)
        );
        assert_eq!(Opened::Absent.index(), None);
    }

    /// The worker's pass leaves a file the next caller can load.
    #[test]
    fn the_rebuild_leaves_an_index_on_disk() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(root, "src/main.rs", "fn main() {}\n");
        write(root, "secrets/prod.env", "TOKEN=hunter2\n");
        let built = rebuild(root, "a", &["secrets/**".to_owned()]).unwrap();
        assert_eq!(built.entries().len(), 1, "the sensitive path is not in it");
        assert_eq!(
            PathIndex::load(&crate::index_path(root), built.key()),
            Some(built)
        );
    }
}
