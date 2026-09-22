//! Which member of a repository family a fact happened in.
//!
//! Its own module because every reader here is TOTAL over a projection that
//! predates the relations: a provenance verb must answer even when the label
//! cannot be read, and that rule is easier to hold in one file than to
//! remember at seven call sites.

use rift_store::{CanonicalStore, SqliteStore, Value};

use super::safe_text::Label;

/// What every provenance surface prints when nothing recorded a worktree
/// recorded a worktree. Borrowed from the dashboard rather than spelled again, so the
/// CLI and the Canopy panels say `unknown` with the same word — a reader who
/// learns it in one place has learned it in both.
pub(crate) const UNKNOWN_WORKTREE: &str = rift_canopy::col3::UNKNOWN_WORKTREE;

/// The label a provenance surface prints for a worktree fact: the member's
/// name, or [`UNKNOWN_WORKTREE`] when there is no fact.
///
/// **Never `main`.** Absence means the shard carries no `worktree` member or
/// the session was never observed — back-filling either is forbidden — and
/// answering `main` would convert an absence into a claim about the main work
/// tree.
pub(crate) fn worktree_label(name: Option<&Label>) -> String {
    name.map_or_else(|| UNKNOWN_WORKTREE.to_owned(), ToString::to_string)
}

/// One `session_worktree` observation, as `rift session info` reports it.
///
/// Every string is a [`Label`]: an administrative worktree name and a branch
/// name are repository data, printed beside the same untrusted git output the
/// rest of this module neutralizes, and the type is the only way to have one.
#[derive(Clone, Debug, PartialEq, Eq, ::serde::Serialize)]
pub(crate) struct WorktreeObservation {
    /// The member's administrative name, or `main`.
    pub(crate) name: Label,
    /// `RepoPaths::worktree_token()` — 12 hex.
    pub(crate) token: Label,
    /// Whether the observed checkout was a linked worktree.
    pub(crate) linked: bool,
    /// `HEAD`'s branch at the observation; `None` for detached or unreadable.
    pub(crate) branch: Option<Label>,
    /// Where the observation came from (`rift_store::schema::SESSION_WORKTREE_BASES`).
    pub(crate) basis: Label,
    /// Epoch milliseconds of the first insert of this state.
    pub(crate) first_observed_at: i64,
}

/// Record the worktree a **non-hook** writer observed a session in —
/// the `attach` and `import` bases for worktree provenance.
///
/// The agent-hook writer lives on the hot path and memoizes through the
/// session cache; this one does not, because these verbs run once, from a
/// terminal, for a session whose cache may not exist at all (`rift import`
/// adopts transcripts the journal never saw). The row's own id still dedups:
/// it is content-addressed over the observed state, so re-running `rift
/// import` over an unchanged transcript store writes no second row, exactly as
/// a redelivered hook does not.
///
/// The observed checkout is **this** worktree — the one the operator ran the
/// verb in — and the basis says so. Nothing is inferred about where the agent
/// originally worked: that would be a claim, and the honest one is that a
/// human adopted the transcript here.
///
/// Returns whether a row was written. Never fails the verb: a projection
/// without the v14 relation costs the observation, not the import.
pub(crate) async fn record_worktree_observation(
    store: &SqliteStore,
    repo: &rift_git::RepoPaths,
    session_row_id: &str,
    basis: &str,
    now: i64,
) -> bool {
    let head = rift_git::head_state(repo);
    let token = repo.worktree_token();
    let row = rift_store::SessionWorktreeRow {
        observation_id: rift_shadow::ids::session_worktree_observation_id(
            session_row_id,
            &token,
            head.branch.as_deref(),
            head.oid.as_deref(),
            None,
        ),
        session_id: session_row_id.to_owned(),
        worktree_token: token,
        worktree_name: repo.member_name(),
        is_linked: i64::from(repo.is_linked()),
        // A LOCAL ADDRESS, canonicalized so one checkout reached through two
        // spellings is one path — the same rule the hook writer holds itself to.
        work_tree: std::fs::canonicalize(&repo.work_tree)
            .unwrap_or_else(|_| repo.work_tree.clone())
            .to_string_lossy()
            .into_owned(),
        branch: head.branch,
        head_oid: head.oid,
        // A subagent cannot run these verbs: both refuse to run from a hook.
        subagent_id: None,
        basis: basis.to_owned(),
        first_observed_at: now,
        schema_version: 1,
    };
    rift_store::record_session_worktree(store, &row)
        .await
        .unwrap_or(false)
}

/// The member a checkpoint was **archived in** — `checkpoint_worktree`'s
/// `worktree_name`, or `None` for every shard that carries no
/// member.
///
/// **Total by construction, and that is deliberate.** A projection still on a
/// pre-v14 schema has no such relation, and a provenance verb must not fail
/// over a label: an unreadable table answers `None`, which is the same answer a
/// v14 projection gives for a shard archived before worktree provenance
/// existed, and both render `unknown`. The
/// query is its own round trip rather than a join onto `checkpoint` for exactly
/// that reason — a join would take the whole verb down with the relation.
pub(crate) async fn checkpoint_worktree(store: &SqliteStore, id: &str) -> Option<Label> {
    let rows = store
        .query(
            "SELECT worktree_name FROM checkpoint_worktree WHERE checkpoint_id = ?",
            &[Value::text(id.to_owned())],
        )
        .await
        .ok()?;
    rows.first()
        .and_then(|r| r.get_text(0).ok())
        .map(|name| Label::of(&name))
}

/// The distinct members a set of checkpoints was archived in, in name order,
/// with [`UNKNOWN_WORKTREE`] present exactly when at least one of them carries
/// no row.
///
/// The window verbs' shape: a report over many checkpoints has many answers,
/// and collapsing them to one would name a worktree the report does not cover.
/// An empty input is an empty spread — the caller renders that as `unknown`,
/// because "which worktree did this window happen in" has no answer when the
/// window holds nothing.
pub(crate) async fn worktree_spread(store: &SqliteStore, ids: &[String]) -> Vec<String> {
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for id in ids {
        seen.insert(worktree_label(
            checkpoint_worktree(store, id).await.as_ref(),
        ));
    }
    seen.into_iter().collect()
}

/// Render a spread for a human line: the names, comma-separated, or
/// [`UNKNOWN_WORKTREE`] when there is nothing to name.
pub(crate) fn spread_text(spread: &[String]) -> String {
    if spread.is_empty() {
        return UNKNOWN_WORKTREE.to_owned();
    }
    spread.join(", ")
}

/// A session's **current** worktree: the observation with the greatest
/// `first_observed_at`, ties broken by name so the answer is deterministic.
///
/// `None` for every session captured before worktree provenance existed — and for every session
/// whose projection predates v14, on the same totality rule as
/// [`checkpoint_worktree`].
pub(crate) async fn session_worktree(store: &SqliteStore, row_id: &str) -> Option<Label> {
    session_worktree_history(store, row_id)
        .await
        .last()
        .map(|o| o.name.clone())
}

/// A session's worktree **history**: every observation, oldest first — the
/// ordered set is what an auditor reads.
///
/// Ordered by `first_observed_at` then `observation_id`, which is the same
/// total order the writer's own tests read the relation in, so two runs cannot
/// disagree about which observation is current.
pub(crate) async fn session_worktree_history(
    store: &SqliteStore,
    row_id: &str,
) -> Vec<WorktreeObservation> {
    let Ok(rows) = store
        .query(
            "SELECT worktree_name, worktree_token, is_linked, branch, basis, first_observed_at \
             FROM session_worktree WHERE session_id = ? \
             ORDER BY first_observed_at, observation_id",
            &[Value::text(row_id.to_owned())],
        )
        .await
    else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| {
            Some(WorktreeObservation {
                name: Label::of(&r.get_text(0).ok()?),
                token: Label::of(&r.get_text(1).ok()?),
                linked: r.get_i64(2).ok()? != 0,
                branch: r.get_opt_text(3).ok()?.map(|b| Label::of(&b)),
                basis: Label::of(&r.get_text(4).ok()?),
                first_observed_at: r.get_i64(5).ok()?,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A projection with the v14 relations, and the two rows every worktree
    /// test here needs seeded through the production writers.
    fn worktree_fixture() -> (
        tempfile::TempDir,
        SqliteStore,
        tokio::runtime::Runtime,
        String,
    ) {
        let dir = tempfile::tempdir().expect("temp dir");
        let store = rift_store::open_capture_projection(dir.path().join("projection.db"))
            .expect("open projection");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let labelled: String = std::iter::repeat_n('a', 64).collect();
        rt.block_on(rift_store::record_checkpoint_worktree_if_new(
            &store,
            &labelled,
            "aabbccddeeff",
            "swt-d",
            true,
            Some("swt/d-surfaces"),
        ))
        .expect("checkpoint_worktree row");
        (dir, store, rt, labelled)
    }

    /// One `session_worktree` observation through the production writer.
    fn observe(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        session: &str,
        name: &str,
        branch: Option<&str>,
        at: i64,
    ) {
        rt.block_on(rift_store::record_session_worktree(
            store,
            &rift_store::SessionWorktreeRow {
                observation_id: rift_shadow::ids::session_worktree_observation_id(
                    session,
                    "aabbccddeeff",
                    branch,
                    None,
                    None,
                ),
                session_id: session.to_owned(),
                worktree_token: "aabbccddeeff".to_owned(),
                worktree_name: name.to_owned(),
                is_linked: i64::from(name != "main"),
                work_tree: format!("/tmp/{name}"),
                branch: branch.map(str::to_owned),
                head_oid: None,
                subagent_id: None,
                basis: "payload_cwd".to_owned(),
                first_observed_at: at,
                schema_version: 1,
            },
        ))
        .expect("session_worktree row");
    }

    /// ★ The rendering clause at the reader: a checkpoint the
    /// projection has no row for is `unknown`, and a checkpoint it does have a
    /// row for is that member — so the absence can never be read as `main`.
    #[test]
    fn a_checkpoint_with_no_row_renders_unknown_and_never_main() {
        let (_dir, store, rt, labelled) = worktree_fixture();
        let bare: String = std::iter::repeat_n('b', 64).collect();

        let named = rt.block_on(checkpoint_worktree(&store, &labelled));
        assert_eq!(worktree_label(named.as_ref()), "swt-d");

        let absent = rt.block_on(checkpoint_worktree(&store, &bare));
        assert_eq!(absent, None, "no row is no fact, not a default");
        assert_eq!(worktree_label(absent.as_ref()), UNKNOWN_WORKTREE);
        assert_ne!(
            worktree_label(absent.as_ref()),
            "main",
            "back-filling the member is forbidden; naming main would invent it"
        );
        // …and the spelling is the dashboard's, so one word means one thing.
        assert_eq!(UNKNOWN_WORKTREE, rift_canopy::col3::UNKNOWN_WORKTREE);
    }

    /// A session id can be observed in several checkouts over its
    /// life, the ordered set is the history, and the **latest** observation is
    /// its current worktree.
    #[test]
    fn the_latest_observation_is_the_sessions_current_worktree() {
        let (_dir, store, rt, _labelled) = worktree_fixture();
        observe(&store, &rt, "s1", "main", Some("main"), 1_000);
        observe(&store, &rt, "s1", "swt-d", Some("swt/d-surfaces"), 2_000);

        let history = rt.block_on(session_worktree_history(&store, "s1"));
        let names: Vec<String> = history.iter().map(|o| o.name.to_string()).collect();
        assert_eq!(names, vec!["main", "swt-d"], "oldest first");
        assert_eq!(
            history.first().map(|o| o.basis.to_string()),
            Some("payload_cwd".to_owned()),
            "the basis rides so an auditor sees how the observation was decided"
        );
        let current = rt.block_on(session_worktree(&store, "s1"));
        assert_eq!(worktree_label(current.as_ref()), "swt-d");

        // A session nothing ever observed is unknown at both readers.
        assert!(rt
            .block_on(session_worktree_history(&store, "s2"))
            .is_empty());
        assert_eq!(
            worktree_label(rt.block_on(session_worktree(&store, "s2")).as_ref()),
            UNKNOWN_WORKTREE
        );
    }

    /// ★ A projection that predates v14 has neither relation. Every reader here
    /// is TOTAL over that: the verb answers, the label is `unknown`, and
    /// nothing propagates an error out of a provenance surface over a badge.
    #[test]
    fn a_pre_v14_projection_costs_the_label_and_not_the_verb() {
        let (_dir, store, rt, labelled) = worktree_fixture();
        observe(&store, &rt, "s1", "swt-d", None, 1);
        // Both relations gone — the shape of every projection still on v13.
        rt.block_on(async {
            let conn = rift_store::Store::conn(&store).await?;
            conn.execute("DROP TABLE checkpoint_worktree", &[]).await?;
            conn.execute("DROP TABLE session_worktree", &[]).await
        })
        .expect("drop the v14 relations");
        assert_eq!(rt.block_on(checkpoint_worktree(&store, &labelled)), None);
        assert!(rt
            .block_on(session_worktree_history(&store, "s1"))
            .is_empty());
        assert_eq!(rt.block_on(session_worktree(&store, "s1")), None);
        assert_eq!(
            rt.block_on(worktree_spread(&store, &[labelled])),
            vec![UNKNOWN_WORKTREE.to_owned()]
        );
    }

    /// The unknown rule for a WINDOW verb: the spread names every member the
    /// window's checkpoints were archived in and carries `unknown` exactly when
    /// one of them records none — so a mixed window cannot be read as if it
    /// happened in one place.
    #[test]
    fn the_spread_names_every_member_and_marks_the_absent_ones() {
        let (_dir, store, rt, labelled) = worktree_fixture();
        let bare: String = std::iter::repeat_n('b', 64).collect();
        assert_eq!(
            rt.block_on(worktree_spread(&store, &[labelled.clone(), bare.clone()])),
            vec!["swt-d".to_owned(), UNKNOWN_WORKTREE.to_owned()]
        );
        assert_eq!(
            rt.block_on(worktree_spread(&store, &[labelled.clone(), labelled])),
            vec!["swt-d".to_owned()],
            "distinct members, not one row per checkpoint"
        );
        // An empty window has nothing to name, and the human line says so
        // rather than printing an empty list.
        assert!(rt.block_on(worktree_spread(&store, &[])).is_empty());
        assert_eq!(spread_text(&[]), UNKNOWN_WORKTREE);
        assert_eq!(
            spread_text(&["main".to_owned(), "swt-d".to_owned()]),
            "main, swt-d"
        );
    }
}
