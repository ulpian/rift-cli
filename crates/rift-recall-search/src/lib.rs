//! Finding a file by name, and finding a line inside one, without the agent
//! paying for either in read tokens.
//!
//! An agent looking for `the settings modal` runs a glob, gets four hundred
//! paths back, opens six of them and reads two thousand lines to find the one
//! it wanted. Every step of that is context, and almost all of it is a lookup
//! that the machine could have answered. This crate is the lookup: a ranked
//! answer to *which paths* and, on request, a bounded answer to *which lines*.
//!
//! What is decided here, and why it is decided here rather than at a caller:
//!
//! - **Ranking is Rift's, not a library's.** The fuzzy score says how well a
//!   path matches the letters typed; it cannot know that this developer's
//!   agents opened `bus.rs` eleven times this week or that `layout.rs` is the
//!   file they have uncommitted right now. Those facts are already recorded —
//!   the window journals of every member of the worktree family, and the
//!   projection's own read counts — so frecency is a query over them and never
//!   a database of its own.
//! - **The frecency facts arrive as an argument.** [`FrecencyFacts`] is built
//!   by whoever can afford to build it and handed to the engine. That is what
//!   keeps a store off the decision path: a hook may consult the engine without
//!   opening the projection, because the counts it would have read are either
//!   already folded into the facts or simply absent, and an absent count ranks
//!   a file colder rather than wrongly.
//! - **The index is a file, per member, keyed by what would invalidate it.**
//!   `.rift/state/search/index.bin` in the checkout that produced it. A linked
//!   worktree of the same family has its own paths and its own index; what
//!   compounds across the family is frecency, not the file list. Nothing here
//!   is derived from a checkout path, and nothing here is a canonical record:
//!   losing the whole directory costs one rebuild.
//! - **Every failure leans the same way.** An index that will not parse, or one
//!   whose key no longer matches, is *absent*: the caller walks or declines to
//!   answer, never serves a stale list. A sensitive path is never indexed at
//!   all, so it cannot be ranked, named or grepped by anything downstream.
//!
//! There is no daemon and no watcher here, by ruling: the index is persisted
//! and rebuilt by the detached worker at the two moments the path set can have
//! changed, and a cold caller with no index answers from a bounded walk or
//! leaves the tool alone.
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

use rift_core::RiftError;

pub mod content;
pub mod engine;
pub mod frecency;
pub mod git;
pub mod index;
pub mod open;
pub mod query;
pub mod walk;

pub use content::{ContentHit, ContentQuery};
pub use engine::{NativeEngine, PathHit, PathQuery, Ranked, SearchEngine};
pub use frecency::{FrecencyFacts, PathFacts};
pub use git::{head_oid, StatusFacts};
pub use index::{FileEntry, IndexKey, PathIndex, INDEX_VERSION};
pub use open::{open_for, rebuild, Opened, COLD_BUDGET};
pub use walk::{build_index, current_key, walk};

/// Why a search could not be answered.
///
/// Every variant carries a path or a number and never a fragment of a file:
/// these strings reach logs and, through the callers, an agent's context.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SearchError {
    /// The index could not be written where it belongs.
    #[error("the search index could not be written to {path}")]
    Write {
        /// The file that could not be written, as Rift spelled it.
        path: String,
    },
    /// More paths than this build will index or decode.
    #[error("{found} paths is over the {cap}-path index cap")]
    TooManyPaths {
        /// How many paths were offered.
        found: usize,
        /// The cap they exceeded.
        cap: usize,
    },
}

impl From<SearchError> for RiftError {
    fn from(error: SearchError) -> Self {
        let message = error.to_string();
        match error {
            SearchError::Write { .. } => RiftError::Storage(message),
            SearchError::TooManyPaths { .. } => RiftError::PolicyDenied(message),
        }
    }
}

/// Where one checkout keeps its search state.
///
/// Per member, under the ignored run-state root, exactly as the window journals
/// are: the path set is a fact about *this* checkout, and a linked worktree of
/// the same family holds different files at the same names. The family-wide
/// caches live in the common directory; this one deliberately does not, and a
/// temporary worktree's index vanishes with it.
#[must_use]
pub fn state_dir(work_tree: &Path) -> PathBuf {
    work_tree.join(".rift/state/search")
}

/// The index file itself.
#[must_use]
pub fn index_path(work_tree: &Path) -> PathBuf {
    state_dir(work_tree).join("index.bin")
}

/// Where one checkout keeps the window journals frecency is a query over.
///
/// Named here because this crate reads them and does not own them: the rows
/// are written by the hook path, in a spelling this crate only parses.
#[must_use]
pub fn reads_dir(work_tree: &Path) -> PathBuf {
    work_tree.join(".rift/state/reads")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The three state paths are the ones the configuration guide publishes,
    /// and all three are inside the checkout that produced them. A cache keyed
    /// by anything derived from where a checkout happens to sit is the one
    /// shape the worktree family model forbids.
    #[test]
    fn search_state_lives_under_the_checkouts_own_run_state_root() {
        let member = Path::new("/w/feature");
        assert_eq!(
            state_dir(member),
            Path::new("/w/feature/.rift/state/search").to_path_buf()
        );
        assert_eq!(
            index_path(member),
            Path::new("/w/feature/.rift/state/search/index.bin").to_path_buf()
        );
        assert_eq!(
            reads_dir(member),
            Path::new("/w/feature/.rift/state/reads").to_path_buf()
        );
    }

    /// A failure to write local state is a storage fact and a refused size is
    /// a policy fact; neither is a validity claim about anything Rift served.
    #[test]
    fn search_errors_map_onto_the_workspace_error_kinds() {
        let write = RiftError::from(SearchError::Write {
            path: "/w/.rift/state/search/index.bin".to_owned(),
        });
        assert!(matches!(write, RiftError::Storage(_)));
        let too_many = RiftError::from(SearchError::TooManyPaths { found: 9, cap: 8 });
        assert!(matches!(too_many, RiftError::PolicyDenied(_)));
    }
}
