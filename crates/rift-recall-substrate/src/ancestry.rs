//! [`GixAncestry`] — the production [`AncestryOracle`].
//!
//! Branch containment asks one question: *is this checkpoint's anchor commit
//! reachable from the caller's `HEAD`?* This module answers it by walking the
//! commit graph read-only through `gix`, with three properties the serving path
//! depends on:
//!
//! * **Once per serve, memoized.** The chokepoint asks once per call and this
//!   type remembers the answer for the life of the oracle, so a second question
//!   about the same `HEAD` is a map lookup. No verdict is persisted — a future
//!   append-only `ancestry_verdict` cache is designed, not built.
//! * **Capped.** Past [`ANCESTRY_WALK_CAP`] commits the verdict is *unknown*,
//!   which the predicate reads as *exclude and report*. A serve must not spend
//!   an unbounded slice of a hook's latency budget deciding containment: the
//!   hook path is held to a hard millisecond budget by standing rule.
//! * **Narrow when it cannot see.** A commit whose object is missing (a shallow
//!   or partial clone) contributes no parents rather than aborting the walk:
//!   the set comes out *smaller* than the true ancestry, which excludes rather
//!   than admits. Uncertainty never widens.
//!
//! `gix` types never cross the seam: object ids go in and out as hex strings.

use std::collections::{BTreeSet, VecDeque};
use std::path::Path;
use std::sync::Mutex;

use rift_core::{Result, RiftError};
use rift_recall_types::{AncestorSet, AncestryOracle};

/// A read-only, local-only ancestry oracle over one repository, backed by `gix`.
pub struct GixAncestry {
    repo: gix::Repository,
    /// `(head oid, cap) → verdict`, including the negative verdict: a walk
    /// that came back *unknown* is not worth repeating inside one serve either.
    ///
    /// The cap is part of the key because it is part of the question. A verdict
    /// of *unknown* can mean "this walk is bigger than the cap you gave me",
    /// and replaying that answer to a caller who raised the cap would be a
    /// remembered refusal to look.
    memo: Mutex<std::collections::BTreeMap<(String, usize), Option<AncestorSet>>>,
}

impl GixAncestry {
    /// Open a repository for read-only commit-graph access.
    ///
    /// # Errors
    /// [`rift_core::RiftError::Substrate`] if the path is not a readable
    /// repository.
    pub fn open(repo_root: &Path) -> Result<Self> {
        let repo = gix::open(repo_root)
            .map_err(|e| RiftError::substrate(format!("open repo for ancestry: {e}")))?;
        Ok(Self {
            repo,
            memo: Mutex::new(std::collections::BTreeMap::new()),
        })
    }

    /// The uncached walk: breadth-first over every parent, inclusive of `head`.
    ///
    /// **Every** parent, not first-parents only: "an ancestor of `HEAD`" is the
    /// question `git merge-base --is-ancestor` answers, and a first-parent walk
    /// would call a merged-in branch's commits foreign to the branch that
    /// merged them — which would delete exactly the compounding a shared store
    /// exists to produce.
    fn walk(&self, head: &str, cap: usize) -> Option<AncestorSet> {
        let start = gix::ObjectId::from_hex(head.as_bytes()).ok()?;
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut queue: VecDeque<gix::ObjectId> = VecDeque::new();
        queue.push_back(start);
        while let Some(oid) = queue.pop_front() {
            if !seen.insert(oid.to_string()) {
                continue; // already expanded (a merge reaches one commit twice)
            }
            if seen.len() > cap {
                return None; // the capped verdict: unknown, reported, never guessed
            }
            let Ok(object) = self.repo.find_object(oid) else {
                continue; // unreadable object: contribute nothing, narrow not wide
            };
            let Ok(commit) = object.try_into_commit() else {
                continue;
            };
            for parent in commit.parent_ids() {
                queue.push_back(parent.detach());
            }
        }
        Some(AncestorSet::new(head, seen))
    }
}

impl AncestryOracle for GixAncestry {
    fn ancestors_of(&self, head: &str, cap: usize) -> Option<AncestorSet> {
        let key = (head.to_ascii_lowercase(), cap);
        if let Ok(memo) = self.memo.lock() {
            if let Some(hit) = memo.get(&key) {
                return hit.clone();
            }
        }
        let walked = self.walk(&key.0, cap);
        if let Ok(mut memo) = self.memo.lock() {
            memo.insert(key, walked.clone());
        }
        walked
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "rift-test")
            .env("GIT_AUTHOR_EMAIL", "test@rift.local")
            .env("GIT_COMMITTER_NAME", "rift-test")
            .env("GIT_COMMITTER_EMAIL", "test@rift.local")
            .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
            .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    fn commit(dir: &Path, name: &str) -> String {
        std::fs::write(dir.join(name), name).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", name]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// The shape containment turns on: a divergent sibling's commit is not
    /// in this member's ancestry, and a merged one is.
    #[test]
    fn a_divergent_commit_is_not_an_ancestor_and_a_merged_one_is() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        let base = commit(dir, "base");
        let on_main = commit(dir, "on-main");

        git(dir, &["checkout", "-q", "-b", "feature", &base]);
        let on_feature = commit(dir, "on-feature");

        let oracle = GixAncestry::open(dir).unwrap();

        let from_main = oracle
            .ancestors_of(&on_main, rift_recall_types::ANCESTRY_WALK_CAP)
            .expect("main's ancestry is knowable");
        assert!(from_main.contains(&on_main), "HEAD is its own ancestor");
        assert!(from_main.contains(&base));
        assert!(
            !from_main.contains(&on_feature),
            "a divergent sibling's commit is NOT in this member's ancestry"
        );

        // Merge the feature in: the same commit is now reachable, so the same
        // question answers the other way. Compounding, not a special case.
        git(dir, &["checkout", "-q", "main"]);
        git(dir, &["merge", "-q", "--no-ff", "-m", "merge", "feature"]);
        let merged = git(dir, &["rev-parse", "HEAD"]);
        let after = GixAncestry::open(dir)
            .unwrap()
            .ancestors_of(&merged, rift_recall_types::ANCESTRY_WALK_CAP)
            .expect("the merge commit's ancestry is knowable");
        assert!(
            after.contains(&on_feature),
            "a merged branch's commits ARE ancestors of the branch that merged them"
        );
    }

    /// Past the cap the answer is *unknown* — `None`, never a partial set the
    /// predicate would mistake for a complete one.
    #[test]
    fn a_walk_past_the_cap_answers_unknown() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        commit(dir, "one");
        let two = commit(dir, "two");
        let oracle = GixAncestry::open(dir).unwrap();
        assert!(oracle.ancestors_of(&two, 1).is_none(), "cap 1 < 2 commits");
        assert!(oracle.ancestors_of(&two, 2).is_some(), "cap 2 fits");
    }

    /// An id that is not a commit id is unknown, not empty — and the negative
    /// verdict is memoized like any other.
    #[test]
    fn an_unresolvable_head_is_unknown_and_memoized() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        git(dir, &["init", "-q", "-b", "main"]);
        commit(dir, "one");
        let oracle = GixAncestry::open(dir).unwrap();
        assert!(oracle.ancestors_of("not-an-oid", 50).is_none());
        assert!(oracle.ancestors_of("not-an-oid", 50).is_none());
        assert_eq!(oracle.memo.lock().unwrap().len(), 1);
    }
}
