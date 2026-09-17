//! Query scope — containment by default, closing the over-retrieval weakness.
//!
//! Every recall query (metrics, ranking, serving) is bounded to the current
//! repo / branch ancestry / worktree / developer / last-N checkpoints.
//! Cross-scope access is **not** a free enum variant — it requires an explicit
//! [`CrossScopeGrant`] that carries a `reason` (audited), so widening scope is a
//! deliberate, loud act and never an accident.
//!
//! # Two kinds of caller, two constructors
//!
//! Once a repository *family* shares one `recall.db`, the branch axis
//! stops being a no-op: a checkpoint minted in the member on `main` is not the
//! last known state of the member on `feature/x`. The rule that keeps that safe
//! is literal — **never widen on uncertainty** — so a *serving* caller builds
//! [`Scope::for_worktree`], which carries the caller's `HEAD` and treats an
//! unknown one as "exclude and report".
//!
//! That rule would be wrong for a caller computing a *family fact* — the ledger
//! metrics, the ranking candidates a report is built from, the eval harness —
//! because a family total that silently dropped every member but one would be a
//! number nobody could audit. Those callers say so with [`Scope::unbounded`],
//! whose [`BranchScope::Unbounded`] is an explicit whole-store branch axis. The
//! developer axis is NOT widened by it: that containment is a
//! security property and stays on.

use std::collections::BTreeSet;

/// Which branch(es) are in scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BranchScope {
    /// The caller's `HEAD` and its ancestry (the safe default). Decided against
    /// an [`AncestryOracle`]; an undecidable `HEAD` excludes and reports.
    CurrentAndAncestry,
    /// Exactly one named branch — matched against the branch a checkpoint's
    /// **archiving** worktree was on (the recall-side `n_checkpoint_worktree`
    /// fact). A checkpoint with no such fact, or one recorded while detached,
    /// is out of scope: the axis narrows, it never widens.
    Exact(String),
    /// The whole store, branch-wise: an **accounting** scope for callers that
    /// compute family facts rather than serve an agent. It is a
    /// deliberate spelling, never a default — a serving path that reached for
    /// it would be widening on uncertainty by another name.
    Unbounded,
}

/// Which worktree(s) are in scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorktreeScope {
    /// Every member of the family (the default, and what every serving path
    /// uses): the point of one shared store is that a summary computed in
    /// member A is served in member B.
    Any,
    /// Only checkpoints whose **archiving** worktree carries this token
    /// (`RepoPaths::worktree_token`, 12 lowercase hex — the stated
    /// semantics). A narrowing the caller asked for explicitly.
    Exact(String),
}

/// Which developer's sessions are in scope.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeveloperScope {
    /// Only the current developer (the safe default).
    CurrentOnly,
    /// A specific developer id.
    Specific(String),
}

/// How many checkpoints are in scope.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointWindow {
    /// The most recent `n` checkpoints (by walk ordinal).
    LastN(u32),
    /// Every checkpoint.
    All,
}

/// The commits reachable from one `HEAD`, **inclusive of `HEAD` itself**.
///
/// Ids are stored lowercase and compared lowercase, so a shard header that
/// spelled its anchor in upper case still matches the walk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AncestorSet {
    head: String,
    commits: BTreeSet<String>,
}

impl AncestorSet {
    /// Build a set for `head` from its ancestors. `head` is inserted for you —
    /// the containment rule is "an ancestor of `HEAD`, **or equal to it**", and
    /// making that a property of the type keeps every oracle honest about it.
    #[must_use]
    pub fn new(head: impl Into<String>, commits: impl IntoIterator<Item = String>) -> Self {
        let head = head.into().to_ascii_lowercase();
        let mut set: BTreeSet<String> = commits
            .into_iter()
            .map(|c| c.to_ascii_lowercase())
            .collect();
        set.insert(head.clone());
        Self { head, commits: set }
    }

    /// Whether `oid` is `HEAD` or one of its ancestors.
    #[must_use]
    pub fn contains(&self, oid: &str) -> bool {
        self.commits.contains(&oid.to_ascii_lowercase())
    }

    /// The `HEAD` this set was walked from.
    #[must_use]
    pub fn head(&self) -> &str {
        &self.head
    }

    /// How many commits the set holds (`HEAD` included).
    #[must_use]
    pub fn len(&self) -> usize {
        self.commits.len()
    }

    /// Whether the set is empty. It never is — [`AncestorSet::new`] always
    /// inserts `HEAD` — but clippy asks, and answering honestly is cheaper than
    /// an allow.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.commits.is_empty()
    }
}

/// The default cap on an ancestry walk. Beyond it the verdict is
/// *unknown* and is reported, never guessed: a serve must not spend an
/// unbounded amount of a hook's latency budget deciding containment.
pub const ANCESTRY_WALK_CAP: usize = 50_000;

/// Answers "is this commit an ancestor of that `HEAD`" for the containment
/// predicate.
///
/// A trait, in the types crate, for two reasons: the chokepoint stays testable
/// with an in-memory ancestry (no git), and `rift-recall-ledger` keeps taking
/// `rift-recall-substrate` with `default-features = false` — it takes the trait
/// object, so no `gix` enters the ledger build.
pub trait AncestryOracle {
    /// The commits reachable from `head`, or `None` when the walk cannot answer
    /// — an unreadable/unknown `head`, or more than `cap` commits.
    ///
    /// `None` is **unknown**, and the predicate treats unknown as *exclude and
    /// report*. It is never "everything".
    fn ancestors_of(&self, head: &str, cap: usize) -> Option<AncestorSet>;
}

/// An oracle that knows nothing — every question answers *unknown*.
///
/// This is what an **accounting** caller passes: with [`BranchScope::Unbounded`]
/// no ancestry question is ever asked, and naming the absence of an oracle is
/// more honest than an `Option` parameter a serving caller could pass `None` to
/// by accident.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoAncestry;

impl AncestryOracle for NoAncestry {
    fn ancestors_of(&self, _head: &str, _cap: usize) -> Option<AncestorSet> {
        None
    }
}

/// A cross-scope capability. Constructible **only** via [`CrossScopeGrant::grant`]
/// (which requires a non-empty `reason`); [`CrossScopeGrant::none`] is the
/// default and grants nothing. Fields are private so the only way to set a flag
/// is the reason-bearing constructor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CrossScopeGrant {
    branches: bool,
    developers: bool,
    repos: bool,
    reason: Option<String>,
}

impl CrossScopeGrant {
    /// No cross-scope access (the default).
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Grant cross-scope access with an audited `reason`. A blank reason grants
    /// nothing (cross-scope is reason-required by construction).
    #[must_use]
    pub fn grant(reason: impl Into<String>, branches: bool, developers: bool, repos: bool) -> Self {
        let reason = reason.into();
        if reason.trim().is_empty() {
            return Self::none();
        }
        Self {
            branches,
            developers,
            repos,
            reason: Some(reason),
        }
    }

    /// Whether cross-branch access was granted.
    #[must_use]
    pub fn branches(&self) -> bool {
        self.branches
    }
    /// Whether cross-developer access was granted.
    #[must_use]
    pub fn developers(&self) -> bool {
        self.developers
    }
    /// Whether cross-repo access was granted.
    #[must_use]
    pub fn repos(&self) -> bool {
        self.repos
    }
    /// The audited reason, if any cross-scope access was granted.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
    /// Whether any cross-scope axis is granted.
    #[must_use]
    pub fn is_any(&self) -> bool {
        self.branches || self.developers || self.repos
    }
}

/// The default query containment. A query never returns rows outside this scope
/// unless [`CrossScopeGrant`] widens the relevant axis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    /// The repo this scope is pinned to (always) — the family recall id.
    pub repo_id: String,
    /// Branch axis.
    pub branch: BranchScope,
    /// Worktree axis.
    pub worktree: WorktreeScope,
    /// The caller's `HEAD` object id. `None` = unknown, which the containment
    /// predicate reads as *exclude and report*, never as *everything*.
    pub head: Option<String>,
    /// Developer axis.
    pub developer: DeveloperScope,
    /// Checkpoint window.
    pub checkpoints: CheckpointWindow,
    /// Cross-scope grant (none by default).
    pub cross: CrossScopeGrant,
}

impl Scope {
    /// The **safe** default: current repo, current branch + ancestry, current
    /// developer only, the last `last_n` checkpoints, no cross-scope.
    ///
    /// `head` is `None` and `worktree` is [`WorktreeScope::Any`]. A `None` head
    /// on [`BranchScope::CurrentAndAncestry`] is *unknown*, so this scope admits
    /// no checkpoint at all through the branch axis and reports how many it
    /// held back. That is deliberate: a caller that wants rows must say which
    /// kind of caller it is — [`Scope::for_worktree`] to serve, or
    /// [`Scope::unbounded`] to account.
    #[must_use]
    pub fn default_for(repo_id: impl Into<String>, last_n: u32) -> Self {
        Self {
            repo_id: repo_id.into(),
            branch: BranchScope::CurrentAndAncestry,
            worktree: WorktreeScope::Any,
            head: None,
            developer: DeveloperScope::CurrentOnly,
            checkpoints: CheckpointWindow::LastN(last_n),
            cross: CrossScopeGrant::none(),
        }
    }

    /// The scope every **serving** path builds: the eight CLI verbs, the hook
    /// injector, `rift-memd`.
    ///
    /// `head` is the caller's own `HEAD` oid, read from the discovered
    /// `RepoPaths` (`rift_git::head_state`, a file read — no `git` process on
    /// the hook path, by standing rule); `None` when it is unborn or
    /// unreadable, which excludes and reports rather than widening.
    ///
    /// `worktree` is [`WorktreeScope::Any`] for every live serving caller, and
    /// that is the point of the shared store rather than an oversight: a
    /// summary computed in member A must be servable in member B, so narrowing
    /// to the member's own token here would delete the compounding the shared
    /// store exists to produce. The axis is a parameter so a caller that genuinely
    /// wants one member's archives asks for it in one word.
    #[must_use]
    pub fn for_worktree(
        repo_id: impl Into<String>,
        last_n: u32,
        head: Option<String>,
        worktree: WorktreeScope,
    ) -> Self {
        Self {
            worktree,
            head,
            ..Self::default_for(repo_id, last_n)
        }
    }

    /// The explicit whole-store scope for **accounting** callers — ledger
    /// metrics, ranking candidates behind a report, the eval harness — that
    /// compute a family fact rather than serve an agent.
    ///
    /// It widens the **branch** axis only. The developer axis stays
    /// [`DeveloperScope::CurrentOnly`] (containment is a security property,
    /// not an accounting convenience) and the repo axis is still pinned.
    #[must_use]
    pub fn unbounded(repo_id: impl Into<String>) -> Self {
        Self {
            repo_id: repo_id.into(),
            branch: BranchScope::Unbounded,
            worktree: WorktreeScope::Any,
            head: None,
            developer: DeveloperScope::CurrentOnly,
            checkpoints: CheckpointWindow::All,
            cross: CrossScopeGrant::none(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_scope_has_no_cross_grant() {
        let s = Scope::default_for("repo1", 50);
        assert_eq!(s.developer, DeveloperScope::CurrentOnly);
        assert!(!s.cross.is_any());
        assert!(s.cross.reason().is_none());
    }

    #[test]
    fn cross_scope_requires_a_reason() {
        // a blank reason grants nothing (reason-required by construction)
        let blank = CrossScopeGrant::grant("  ", false, true, false);
        assert!(!blank.is_any());
        // a real reason widens and is recorded for audit
        let granted = CrossScopeGrant::grant("reviewing teammate branch", false, true, false);
        assert!(granted.developers());
        assert_eq!(granted.reason(), Some("reviewing teammate branch"));
    }

    /// The three constructors say three different things, and the difference is
    /// the whole point of the split: the default knows no head (so it decides
    /// nothing), the serving one carries one, and the accounting one declares
    /// the branch axis open on purpose while leaving the developer axis shut.
    #[test]
    fn the_three_constructors_differ_only_on_the_axes_they_declare() {
        let d = Scope::default_for("fam", 50);
        assert_eq!(d.branch, BranchScope::CurrentAndAncestry);
        assert_eq!(d.worktree, WorktreeScope::Any);
        assert!(d.head.is_none());

        let s = Scope::for_worktree("fam", 50, Some("abc".into()), WorktreeScope::Any);
        assert_eq!(s.branch, BranchScope::CurrentAndAncestry);
        assert_eq!(s.head.as_deref(), Some("abc"));
        assert_eq!(s.checkpoints, CheckpointWindow::LastN(50));

        let u = Scope::unbounded("fam");
        assert_eq!(u.branch, BranchScope::Unbounded);
        assert_eq!(u.checkpoints, CheckpointWindow::All);
        assert_eq!(
            u.developer,
            DeveloperScope::CurrentOnly,
            "unbounded widens the BRANCH axis; the developer axis stays shut"
        );
        assert!(!u.cross.is_any());
    }

    /// `HEAD` is in its own ancestor set, and the comparison is case-folded:
    /// the walk's ids and a shard header's `commit` need not agree on case.
    #[test]
    fn an_ancestor_set_contains_its_head_and_folds_case() {
        let set = AncestorSet::new("AABB", ["ccdd".to_owned()]);
        assert!(set.contains("aabb"));
        assert!(set.contains("AABB"));
        assert!(set.contains("CCDD"));
        assert!(!set.contains("eeff"));
        assert_eq!(set.head(), "aabb");
        assert_eq!(set.len(), 2);
        assert!(!set.is_empty());
    }

    /// The absence of an oracle answers *unknown*, never *everything*.
    #[test]
    fn the_null_oracle_answers_unknown() {
        assert!(NoAncestry.ancestors_of("aabb", ANCESTRY_WALK_CAP).is_none());
    }
}
