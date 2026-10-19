//! The scope chokepoint (containment by default).
//!
//! [`in_scope_nodes`] is the **single** function every recall query funnels
//! through to bound itself. It returns the in-scope checkpoint-id set; callers
//! filter their rows by [`ScopeSet::contains`], so no query can escape its
//! declared scope without an explicit [`rift_recall_types::CrossScopeGrant`].
//!
//! # The axes, and the one rule
//!
//! * **developer** (`n_checkpoint.developer`; `NULL` = the current developer);
//! * **branch** — real since the family store: a checkpoint is in scope when its anchor
//!   commit (`n_checkpoint_commit`, mirrored recall-side by the ingest) is
//!   reachable from the caller's `HEAD`. While every checkout owned its own
//!   `recall.db`, "one database = one branch line" held by accident and this
//!   axis was a no-op. One store per *family* ends that accident:
//!   the member on `feature/x` must not be handed the member on `main`'s last
//!   known state, because the files it holds are not the files that changed;
//! * **worktree** — `n_checkpoint_worktree`, the member a shard was *archived*
//!   in. Every member of the family is in scope, because a summary computed
//!   in member A being servable in member B is the whole point of a shared
//!   store;
//! * the **checkpoint window** (last-N by walk ordinal), applied last.
//!
//! The rule over all of them is literal and has not changed: **they never widen
//! on uncertainty**. An unknown `HEAD`, an ancestry walk that hit its cap, a
//! checkpoint with no anchor commit — each is *out* of scope and counted into
//! [`ScopeSet::omitted_out_of_branch`], so the caller can say how many rather
//! than pretend there were none.
//!
//! One narrow route exists for a caller whose branch has no commit yet: its
//! turn-end seals have no anchor, yet the branch has a name. When the scope
//! carries that name and the caller's member token, the checkpoints this
//! member archived on that branch are admitted, and nothing else is.

use std::collections::{BTreeMap, BTreeSet};

use rift_core::Result;
use rift_recall_types::{
    AncestorSet, AncestryOracle, BranchScope, CheckpointWindow, DeveloperScope, SameMember, Scope,
    ANCESTRY_WALK_CAP,
};
use rift_store::CanonicalStore;
use sha2::{Digest, Sha256};

/// The resolved in-scope membership: the set of in-scope checkpoint ids, plus
/// an honest account of what containment held back (the omission line
/// grows one cause).
#[derive(Clone, Debug, Default)]
pub struct ScopeSet {
    checkpoints: BTreeSet<String>,
    max_ordinal: i64,
    omitted_out_of_branch: usize,
    unknown_head: bool,
}

impl ScopeSet {
    /// Whether `checkpoint_id` is in scope.
    #[must_use]
    pub fn contains(&self, checkpoint_id: &str) -> bool {
        self.checkpoints.contains(checkpoint_id)
    }

    /// Whether the scope is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.checkpoints.is_empty()
    }

    /// The number of in-scope checkpoints.
    #[must_use]
    pub fn len(&self) -> usize {
        self.checkpoints.len()
    }

    /// The in-scope checkpoint ids.
    #[must_use]
    pub fn checkpoint_ids(&self) -> &BTreeSet<String> {
        &self.checkpoints
    }

    /// The highest walk ordinal in the set, or `0` when it is empty.
    #[must_use]
    pub fn max_ordinal(&self) -> i64 {
        self.max_ordinal
    }

    /// How many checkpoints this developer owns that the **branch** axis held
    /// back — off the caller's ancestry, or carrying no anchor commit at all.
    ///
    /// Counted before the last-N window, so it answers "how much of your
    /// family's history is on another line", not "how much fell off the end of
    /// the page".
    #[must_use]
    pub fn omitted_out_of_branch(&self) -> usize {
        self.omitted_out_of_branch
    }

    /// Whether the branch axis could not be decided at all — no `HEAD`, or an
    /// ancestry walk that hit its cap. Everything is out of scope in that
    /// state, and saying so is the difference between "nothing changed" and
    /// "I could not tell".
    #[must_use]
    pub fn unknown_head(&self) -> bool {
        self.unknown_head
    }
}

/// The branch axis, resolved **once** per call — the ancestry walk is one
/// question per serve, never one per checkpoint — the hook path is held to a
/// hard millisecond budget.
enum BranchVerdict {
    /// Everything passes: [`BranchScope::Unbounded`], or a cross-branch grant.
    Open,
    /// The ancestry of a known `HEAD`.
    Ancestry(AncestorSet),
    /// One named branch, matched against the archiving member's branch.
    Named(String),
    /// An unborn caller: its own member's checkpoints on the branch its
    /// symbolic `HEAD` names, and no one else's.
    SameMember(SameMember),
    /// Undecidable — no `HEAD`, or the oracle could not answer within the cap.
    /// Excludes everything, and reports.
    Unknown,
}

fn branch_verdict(scope: &Scope, ancestry: &dyn AncestryOracle) -> BranchVerdict {
    if scope.cross.branches() {
        return BranchVerdict::Open; // an explicit, reason-bearing, audited grant
    }
    match &scope.branch {
        BranchScope::Unbounded => BranchVerdict::Open,
        BranchScope::Exact(name) => BranchVerdict::Named(name.clone()),
        BranchScope::CurrentAndAncestry => {
            let Some(head) = scope.head.as_deref() else {
                return scope
                    .unborn
                    .clone()
                    .map_or(BranchVerdict::Unknown, BranchVerdict::SameMember);
            };
            ancestry
                .ancestors_of(head, ANCESTRY_WALK_CAP)
                .map_or(BranchVerdict::Unknown, BranchVerdict::Ancestry)
        }
    }
}

/// Whether a checkpoint owned by `dev` (`None` = current/unknown) is in scope.
fn developer_in_scope(scope: &Scope, dev: Option<&str>) -> bool {
    if scope.cross.developers() {
        return true; // explicit cross-developer grant
    }
    match &scope.developer {
        DeveloperScope::CurrentOnly => dev.is_none(),
        DeveloperScope::Specific(d) => dev == Some(d.as_str()),
    }
}

/// Whether the branch axis admits one checkpoint.
fn branch_admits(
    verdict: &BranchVerdict,
    id: &str,
    anchors: &BTreeMap<String, String>,
    members: &BTreeMap<String, Member>,
) -> bool {
    match verdict {
        BranchVerdict::Open => true,
        BranchVerdict::Unknown => false,
        // No anchor commit is the same answer as an anchor off the line: this
        // checkpoint cannot be shown to belong to the caller's history.
        BranchVerdict::Ancestry(set) => anchors.get(id).is_some_and(|oid| set.contains(oid)),
        BranchVerdict::Named(name) => members
            .get(id)
            .and_then(|m| m.branch.as_deref())
            .is_some_and(|b| b == name),
        BranchVerdict::SameMember(want) => members.get(id).is_some_and(|m| {
            m.token == want.token && m.branch.as_deref() == Some(want.branch.as_str())
        }),
    }
}

/// The archiving member of one checkpoint, as the projection recorded it.
struct Member {
    token: String,
    branch: Option<String>,
}

/// Resolve a [`Scope`] to its in-scope checkpoint set — the one membership
/// chokepoint. Applies the developer filter, then branch containment against
/// `ancestry`, then the worktree axis, then the last-N window (by ordinal).
///
/// `ancestry` is asked at most one question per call. An accounting caller with
/// [`BranchScope::Unbounded`] is asked none, which is why
/// [`rift_recall_types::NoAncestry`] is a sufficient argument there.
///
/// # Errors
/// Propagates store read errors.
pub async fn in_scope_nodes(
    store: &impl CanonicalStore,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
) -> Result<ScopeSet> {
    let verdict = branch_verdict(scope, ancestry);
    // Only load the facts this verdict actually consults: an accounting pass
    // over the whole store reads neither relation.
    let anchors: BTreeMap<String, String> = if matches!(verdict, BranchVerdict::Ancestry(_)) {
        store
            .query("SELECT node_id, commit_oid FROM n_checkpoint_commit", &[])
            .await?
            .iter()
            .filter_map(|r| Some((r.get_text(0).ok()?, r.get_text(1).ok()?)))
            .collect()
    } else {
        BTreeMap::new()
    };
    // The branch each checkpoint's archiving member was on, as the projection
    // recorded it — read only when the branch axis names one.
    let names_member = matches!(
        verdict,
        BranchVerdict::Named(_) | BranchVerdict::SameMember(_)
    );
    let members: BTreeMap<String, Member> = if names_member {
        store
            .query(
                "SELECT node_id, worktree_token, branch FROM n_checkpoint_worktree",
                &[],
            )
            .await?
            .iter()
            .filter_map(|r| {
                let member = Member {
                    token: r.get_text(1).ok()?,
                    branch: r.get_opt_text(2).ok()?,
                };
                Some((r.get_text(0).ok()?, member))
            })
            .collect()
    } else {
        BTreeMap::new()
    };

    let rows = store
        .query(
            "SELECT node_id, ordinal, developer FROM n_checkpoint ORDER BY ordinal DESC",
            &[],
        )
        .await?;
    let mut kept: Vec<(String, i64)> = Vec::new();
    let mut omitted_out_of_branch = 0usize;
    for r in &rows {
        let id = r.get_text(0)?;
        let ordinal = r.get_i64(1)?;
        let developer = r.get_opt_text(2)?;
        if !developer_in_scope(scope, developer.as_deref()) {
            continue;
        }
        if !branch_admits(&verdict, &id, &anchors, &members) {
            omitted_out_of_branch += 1;
            continue;
        }
        kept.push((id, ordinal));
    }
    // `kept` is already ordered newest-first; the window takes the most recent.
    let windowed: Vec<(String, i64)> = match scope.checkpoints {
        CheckpointWindow::LastN(n) => kept.into_iter().take(n as usize).collect(),
        CheckpointWindow::All => kept,
    };
    let max_ordinal = windowed.iter().map(|(_, o)| *o).max().unwrap_or(0);
    Ok(ScopeSet {
        checkpoints: windowed.into_iter().map(|(id, _)| id).collect(),
        max_ordinal,
        omitted_out_of_branch,
        unknown_head: matches!(verdict, BranchVerdict::Unknown),
    })
}

/// A fingerprint of what one member's scope currently admits — the **second**
/// level of the injection stamp.
///
/// The first level is a cheap file stamp of `recall.db`: if nothing moved
/// anywhere, the answer is the silence the last turn already established and no
/// database is opened. But once a family shares one store, "moved" stops
/// meaning "moved for me": a sibling landing facts on a divergent branch moves
/// the file without changing one byte of what this member may be told. Injecting
/// on that would bill the member for a turn it learns nothing from — the
/// double-billing hazard the first audit named.
///
/// So this is the level that decides, and it is computed **through the same
/// chokepoint the serve goes through** rather than beside it: the set the stamp
/// fingerprints is by construction the set the manifest would be built from,
/// which is what makes "unchanged" mean "there is nothing to say" instead of
/// "nothing I happened to look at".
///
/// # Errors
/// Propagates store read errors.
pub async fn scoped_stamp(
    store: &impl CanonicalStore,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
) -> Result<String> {
    let set = in_scope_nodes(store, scope, ancestry).await?;
    let mut h = Sha256::new();
    h.update(b"rift.scoped-stamp.v1\0");
    h.update(set.max_ordinal().to_le_bytes());
    for id in set.checkpoint_ids() {
        h.update((id.len() as u64).to_le_bytes());
        h.update(id.as_bytes());
    }
    let mut out = String::with_capacity(64);
    use core::fmt::Write as _;
    for b in h.finalize() {
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

/// An [`AncestryOracle`] backed by a table rather than a repository — the
/// offline half of the seam, exactly as [`rift_recall_substrate::MemorySubstrate`]
/// is for the reader.
///
/// It is a *lookup*, deliberately, not a second graph walk: a test that checked
/// the predicate against another implementation of reachability would be
/// checking two walks agree. Here the test states the ancestry and the
/// predicate is measured against the statement.
#[derive(Clone, Debug, Default)]
pub struct MemoryAncestry {
    by_head: BTreeMap<String, BTreeSet<String>>,
}

impl MemoryAncestry {
    /// An oracle that knows nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare `head`'s ancestry. `head` itself is always included (that is
    /// [`AncestorSet`]'s contract, not this type's convenience).
    #[must_use]
    pub fn with_ancestry<I, S>(mut self, head: &str, ancestors: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut set: BTreeSet<String> = ancestors
            .into_iter()
            .map(|a| a.into().to_ascii_lowercase())
            .collect();
        set.insert(head.to_ascii_lowercase());
        self.by_head.insert(head.to_ascii_lowercase(), set);
        self
    }
}

impl AncestryOracle for MemoryAncestry {
    fn ancestors_of(&self, head: &str, cap: usize) -> Option<AncestorSet> {
        let known = self.by_head.get(&head.to_ascii_lowercase())?;
        if known.len() > cap {
            return None; // the capped verdict: unknown, never a partial set
        }
        Some(AncestorSet::new(head, known.iter().cloned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_types::{CrossScopeGrant, DeveloperScope, NoAncestry, WorktreeScope};
    use rift_store::{recall_migrations, SqliteStore, Value};

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    fn recall_store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&s, &recall_migrations()).unwrap();
        s
    }

    fn put_checkpoint(store: &SqliteStore, id: &str, ordinal: i64, developer: Option<&str>) {
        block(store.insert_or_ignore(
            "n_checkpoint",
            &[
                "node_id",
                "dialect",
                "ordinal",
                "prev_checkpoint",
                "strategy",
                "merkle_root",
                "developer",
            ],
            &[
                Value::text(id),
                Value::text("rift"),
                Value::Int(ordinal),
                Value::Null,
                Value::Null,
                Value::Null,
                developer.map_or(Value::Null, Value::text),
            ],
        ))
        .unwrap();
    }

    fn put_anchor(store: &SqliteStore, id: &str, commit_oid: &str) {
        block(store.insert_or_ignore(
            "n_checkpoint_commit",
            &["node_id", "commit_oid", "schema_version"],
            &[Value::text(id), Value::text(commit_oid), Value::Int(1)],
        ))
        .unwrap();
    }

    fn put_member(store: &SqliteStore, id: &str, token: &str, name: &str, branch: Option<&str>) {
        block(store.insert_or_ignore(
            "n_checkpoint_worktree",
            &[
                "node_id",
                "worktree_token",
                "worktree_name",
                "is_linked",
                "branch",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text(token),
                Value::text(name),
                Value::Int(1),
                branch.map_or(Value::Null, Value::text),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// The accounting scope: the branch axis is open by declaration, so the
    /// developer axis is the only one filtering — which is what this containment
    /// keystone has always been about.
    #[test]
    fn current_developer_scope_excludes_other_developers() {
        let store = recall_store();
        put_checkpoint(&store, "mine", 1, None);
        put_checkpoint(&store, "theirs", 2, Some("mallory"));
        let scope = Scope::unbounded("repo");
        let set = block(in_scope_nodes(&store, &scope, &NoAncestry)).unwrap();
        assert!(set.contains("mine"));
        assert!(!set.contains("theirs"), "another developer is out of scope");
    }

    #[test]
    fn last_n_window_limits_checkpoints() {
        let store = recall_store();
        for i in 1..=5 {
            put_checkpoint(&store, &format!("c{i}"), i, None);
        }
        let mut scope = Scope::unbounded("repo");
        scope.checkpoints = CheckpointWindow::LastN(2);
        scope.developer = DeveloperScope::CurrentOnly;
        let set = block(in_scope_nodes(&store, &scope, &NoAncestry)).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains("c5") && set.contains("c4"));
    }

    #[test]
    fn cross_developer_grant_widens_axis() {
        let store = recall_store();
        put_checkpoint(&store, "mine", 1, None);
        put_checkpoint(&store, "theirs", 2, Some("mallory"));
        let mut scope = Scope::unbounded("repo");
        scope.cross = CrossScopeGrant::grant("review", false, true, false);
        let set = block(in_scope_nodes(&store, &scope, &NoAncestry)).unwrap();
        assert!(set.contains("mine") && set.contains("theirs"));
    }

    /// A serving scope with no `HEAD` decides nothing — and says how much it
    /// could not decide. The failure direction is the whole point: an unknown
    /// head must read as "I cannot tell", never as "everything".
    #[test]
    fn an_unknown_head_excludes_and_reports() {
        let store = recall_store();
        put_checkpoint(&store, "c1", 1, None);
        put_checkpoint(&store, "c2", 2, None);
        put_anchor(&store, "c1", "aa");
        put_anchor(&store, "c2", "bb");
        let scope = Scope::for_worktree("repo", u32::MAX, None, WorktreeScope::Any);
        let oracle = MemoryAncestry::new().with_ancestry("aa", ["bb"]);
        let set = block(in_scope_nodes(&store, &scope, &oracle)).unwrap();
        assert!(set.is_empty());
        assert!(set.unknown_head());
        assert_eq!(set.omitted_out_of_branch(), 2);
    }

    fn here_on_main() -> SameMember {
        SameMember {
            branch: "main".into(),
            token: "aaaaaaaaaaaa".into(),
        }
    }

    /// Before the first commit a turn-end seal has no anchor. An unborn caller
    /// is served its own member's seals on the branch HEAD names, and the
    /// route stops there: a sibling member's seal and another branch's seal
    /// stay out and are counted.
    #[test]
    fn an_unborn_head_is_served_its_own_members_seals_on_its_branch() {
        let store = recall_store();
        put_checkpoint(&store, "mine", 1, None);
        put_checkpoint(&store, "sibling", 2, None);
        put_checkpoint(&store, "elsewhere", 3, None);
        put_member(&store, "mine", "aaaaaaaaaaaa", "main", Some("main"));
        put_member(&store, "sibling", "bbbbbbbbbbbb", "wt", Some("main"));
        put_member(&store, "elsewhere", "aaaaaaaaaaaa", "main", Some("other"));
        let scope = Scope::for_worktree("repo", u32::MAX, None, WorktreeScope::Any)
            .with_unborn(here_on_main());
        let set = block(in_scope_nodes(&store, &scope, &NoAncestry)).unwrap();
        assert_eq!(set.checkpoint_ids().iter().collect::<Vec<_>>(), ["mine"]);
        assert!(!set.unknown_head(), "a named unborn branch is decided");
        assert_eq!(set.omitted_out_of_branch(), 2);
    }

    /// A born HEAD keeps the ancestry route: the unborn pair cannot widen it
    /// to anchorless rows.
    #[test]
    fn a_born_head_ignores_the_unborn_route() {
        let store = recall_store();
        put_checkpoint(&store, "mine", 1, None);
        put_member(&store, "mine", "aaaaaaaaaaaa", "main", Some("main"));
        let scope = Scope::for_worktree("repo", u32::MAX, Some("head".into()), WorktreeScope::Any)
            .with_unborn(here_on_main());
        let oracle = MemoryAncestry::new().with_ancestry("head", ["head"]);
        let set = block(in_scope_nodes(&store, &scope, &oracle)).unwrap();
        assert!(set.is_empty());
        assert_eq!(set.omitted_out_of_branch(), 1);
    }

    /// A walk past its cap is *unknown*, not a partial answer — and reports the
    /// same way an absent head does.
    #[test]
    fn a_cap_hit_excludes_and_reports() {
        let store = recall_store();
        put_checkpoint(&store, "c1", 1, None);
        put_anchor(&store, "c1", "aa");
        // An ancestry larger than the cap the predicate asks with is refused by
        // the oracle, which is how a 50k-commit walk reports rather than stalls.
        let huge: Vec<String> = (0..=ANCESTRY_WALK_CAP).map(|i| format!("{i:x}")).collect();
        let oracle = MemoryAncestry::new().with_ancestry("head", huge);
        let scope = Scope::for_worktree("repo", u32::MAX, Some("head".into()), WorktreeScope::Any);
        let set = block(in_scope_nodes(&store, &scope, &oracle)).unwrap();
        assert!(set.is_empty());
        assert!(set.unknown_head());
        assert_eq!(set.omitted_out_of_branch(), 1);
    }

    /// The divergent-member case in miniature, which is the whole point: the
    /// sibling's checkpoint is off this member's line, so it is excluded and
    /// counted; a checkpoint with no anchor at all is treated the same way.
    #[test]
    fn a_divergent_siblings_checkpoint_is_excluded_and_counted() {
        let store = recall_store();
        put_checkpoint(&store, "mine", 3, None);
        put_checkpoint(&store, "theirs", 2, None);
        put_checkpoint(&store, "anchorless", 1, None);
        put_anchor(&store, "mine", "c0ffee");
        put_anchor(&store, "theirs", "deadbe");
        let oracle = MemoryAncestry::new().with_ancestry("head", ["c0ffee"]);
        let scope = Scope::for_worktree("repo", u32::MAX, Some("head".into()), WorktreeScope::Any);
        let set = block(in_scope_nodes(&store, &scope, &oracle)).unwrap();
        assert!(set.contains("mine"));
        assert!(!set.contains("theirs"));
        assert!(!set.contains("anchorless"));
        assert!(!set.unknown_head(), "the head was perfectly knowable");
        assert_eq!(set.omitted_out_of_branch(), 2);

        // The grant is the one thing that widens it, and it is reason-bearing.
        let mut granted = scope.clone();
        granted.cross = CrossScopeGrant::grant("reviewing the sibling", true, false, false);
        let set = block(in_scope_nodes(&store, &granted, &oracle)).unwrap();
        assert!(set.contains("theirs") && set.contains("anchorless"));
        assert_eq!(set.omitted_out_of_branch(), 0);
    }

    /// `BranchScope::Exact` matches the branch the ARCHIVING member was on, and
    /// a checkpoint with no member fact is out — the axis narrows, never widens.
    #[test]
    fn an_exact_branch_scope_matches_the_archiving_members_branch() {
        let store = recall_store();
        put_checkpoint(&store, "on_main", 3, None);
        put_checkpoint(&store, "on_feature", 2, None);
        put_checkpoint(&store, "unlabelled", 1, None);
        put_member(&store, "on_main", "0123456789ab", "main", Some("main"));
        put_member(
            &store,
            "on_feature",
            "ba9876543210",
            "swt-c",
            Some("feature"),
        );

        let mut scope = Scope::unbounded("repo");
        scope.branch = BranchScope::Exact("main".to_owned());
        let set = block(in_scope_nodes(&store, &scope, &NoAncestry)).unwrap();
        assert!(set.contains("on_main"));
        assert!(!set.contains("on_feature"));
        assert!(!set.contains("unlabelled"));
        assert_eq!(set.omitted_out_of_branch(), 2);
    }

    proptest::proptest! {
        /// The containment keystone: over random multi-developer ledgers, a default
        /// (current-developer) scope never admits a checkpoint owned by another
        /// developer — containment is a security property.
        #[test]
        fn query_never_returns_out_of_scope_rows(
            devs in proptest::collection::vec(proptest::option::of("dev[0-9]"), 1..15)
        ) {
            let store = recall_store();
            for (i, d) in devs.iter().enumerate() {
                put_checkpoint(&store, &format!("c{i}"), i as i64 + 1, d.as_deref());
            }
            // Branch-unbounded so only the dev axis filters.
            let scope = Scope::unbounded("repo");
            let set = block(in_scope_nodes(&store, &scope, &NoAncestry)).unwrap();
            // every admitted checkpoint must be a current-developer (None) one
            for (i, d) in devs.iter().enumerate() {
                let admitted = set.contains(&format!("c{i}"));
                proptest::prop_assert_eq!(admitted, d.is_none());
            }
        }

        /// The branch-containment keystone, over random DAGs: a checkpoint is served
        /// **iff** its anchor commit is reachable from the caller's `HEAD`, or
        /// a cross-branch grant widened it. Everything not served is counted.
        ///
        /// The DAG is generated in topological order (commit `i` may parent
        /// only on commits before it), the reachable set is computed here from
        /// the edges, and the oracle is told that set — so what is measured is
        /// the predicate, not a second walk.
        #[test]
        fn a_checkpoint_is_served_iff_its_anchor_is_an_ancestor_or_a_grant_exists(
            parents in proptest::collection::vec(
                proptest::collection::vec(0usize..8, 0..3), 1..9),
            anchors in proptest::collection::vec(0usize..8, 1..9),
        ) {
            let n = parents.len();
            let oid = |i: usize| format!("{:040x}", i + 1);

            // Reachability from the last commit, over edges that respect the
            // topological order (a parent index is taken modulo the child's).
            let mut reachable: BTreeSet<usize> = BTreeSet::new();
            let mut stack = vec![n - 1];
            while let Some(i) = stack.pop() {
                if !reachable.insert(i) {
                    continue;
                }
                if let Some(ps) = parents.get(i) {
                    for p in ps {
                        if i > 0 {
                            stack.push(p % i);
                        }
                    }
                }
            }

            let store = recall_store();
            for (k, a) in anchors.iter().enumerate() {
                let anchor = a % n;
                put_checkpoint(&store, &format!("ck{k}"), k as i64 + 1, None);
                put_anchor(&store, &format!("ck{k}"), &oid(anchor));
            }

            let head = oid(n - 1);
            let oracle = MemoryAncestry::new()
                .with_ancestry(&head, reachable.iter().map(|i| oid(*i)));
            let scope = Scope::for_worktree(
                "repo", u32::MAX, Some(head.clone()), WorktreeScope::Any);
            let set = block(in_scope_nodes(&store, &scope, &oracle)).unwrap();

            let mut expected_omitted = 0usize;
            for (k, a) in anchors.iter().enumerate() {
                let served = set.contains(&format!("ck{k}"));
                let is_ancestor = reachable.contains(&(a % n));
                proptest::prop_assert_eq!(served, is_ancestor, "ck{} anchored at {}", k, a % n);
                if !is_ancestor {
                    expected_omitted += 1;
                }
            }
            proptest::prop_assert_eq!(set.omitted_out_of_branch(), expected_omitted);
            proptest::prop_assert!(!set.unknown_head());

            // The grant is the only widening, and it admits exactly the rest.
            let mut granted = scope;
            granted.cross = CrossScopeGrant::grant("review", true, false, false);
            let all = block(in_scope_nodes(&store, &granted, &oracle)).unwrap();
            for k in 0..anchors.len() {
                let id = format!("ck{k}");
                proptest::prop_assert!(all.contains(&id));
            }
            proptest::prop_assert_eq!(all.omitted_out_of_branch(), 0);
        }
    }

    /// The scoped stamp is a function of what this member may be told, and of
    /// nothing else. A divergent sibling's checkpoint moves the store and not
    /// the stamp; a checkpoint on this member's own line moves both.
    #[test]
    fn the_scoped_stamp_moves_only_when_this_members_scope_does() {
        let store = recall_store();
        put_checkpoint(&store, "mine", 1, None);
        put_anchor(&store, "mine", "c0ffee");
        let oracle = MemoryAncestry::new().with_ancestry("head", ["c0ffee", "beefee"]);
        let scope = Scope::for_worktree("repo", u32::MAX, Some("head".into()), WorktreeScope::Any);

        let before = block(scoped_stamp(&store, &scope, &oracle)).unwrap();
        assert_eq!(before.len(), 64);

        // A divergent sibling lands facts: the store moved, this member's scope
        // did not, and neither did the stamp.
        put_checkpoint(&store, "theirs", 2, None);
        put_anchor(&store, "theirs", "deadbe");
        let after_divergent = block(scoped_stamp(&store, &scope, &oracle)).unwrap();
        assert_eq!(after_divergent, before);

        // A sibling lands facts on this member's ancestry: the stamp moves, and
        // that is the compounding, not an exception to it.
        put_checkpoint(&store, "ours", 3, None);
        put_anchor(&store, "ours", "beefee");
        let after_ancestry = block(scoped_stamp(&store, &scope, &oracle)).unwrap();
        assert_ne!(after_ancestry, before);
    }
}
