//! The worktree provenance relations: which worktree a session was observed
//! in, and which worktree a checkpoint's shard names as its archiver.

use rift_core::Result;

use crate::{CanonicalStore, Value};

/// A session worktree observation: one OBSERVATION of the worktree a session
/// was seen in, stored in `session_worktree` or, under a basis that relation's
/// CHECK cannot admit, in `session_worktree_cd`.
///
/// Append-only and content-addressed over the observed state, so a session's
/// history is the ordered set of its rows and nothing is ever updated. The
/// caller derives `observation_id`
/// (`rift_shadow::ids::session_worktree_observation_id`) — as every other
/// content-addressed row here does — and screens `basis` against the closed
/// vocabularies before this DAO sees it (the CHECK constraints refuse anything
/// else regardless). An observation filed in `session_worktree_cd` carries an
/// id that also covers its basis, so it never shares an id with a row of the
/// older relation.
#[derive(Debug, Clone)]
pub struct SessionWorktreeRow {
    /// Content-addressed over `(session, token, branch, head, subagent)`.
    pub observation_id: String,
    /// The capture session row id (FK: session).
    pub session_id: String,
    /// `RepoPaths::worktree_token()` — 12 hex.
    pub worktree_token: String,
    /// The linked worktree's administrative name, or `main`.
    pub worktree_name: String,
    /// Whether this is a linked worktree (0/1).
    pub is_linked: i64,
    /// The checkout's absolute path — a LOCAL ADDRESS. It is why this relation
    /// is projection-only: paths never enter shards.
    pub work_tree: String,
    /// `HEAD`'s symbolic name at observation; `None` when detached/unreadable.
    pub branch: Option<String>,
    /// `HEAD`'s oid at observation; `None` when unborn/unresolvable.
    pub head_oid: Option<String>,
    /// The subagent session id when a subagent's hook made the observation —
    /// recorded under the PARENT's session id, which is what the payload
    /// carries inside a subagent.
    pub subagent_id: Option<String>,
    /// Where the observed directory came from — one of
    /// [`crate::schema::SESSION_WORKTREE_BASES`], or `shell_cd`.
    pub basis: String,
    /// Epoch ms of the FIRST insert. A repeat observation keeps this value,
    /// because the id dedups before the timestamp is ever compared — which is
    /// exactly why the id must not include the time.
    pub first_observed_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Both observation relations as one, for a reader on a projection that has
/// the second. Same columns in the same order, so `SELECT *` lines them up.
const BOTH_OBSERVATION_RELATIONS: &str =
    "(SELECT * FROM session_worktree UNION ALL SELECT * FROM session_worktree_cd)";

impl SessionWorktreeRow {
    /// The relation every reader of session observations selects from: the
    /// union of `session_worktree` and `session_worktree_cd`, or the first
    /// alone on a projection opened before the second existed.
    ///
    /// One answer for every reader, because a reader that forgot the second
    /// relation would file a `cd`-decided read under the payload's checkout
    /// again, silently. The fallback keeps every reader working on a
    /// projection a read-only open never migrates; a reader orders the union
    /// exactly as it ordered the one table.
    pub async fn relation(store: &impl CanonicalStore) -> &'static str {
        let probe = store
            .query(
                "SELECT 1 FROM sqlite_master \
                 WHERE type = 'table' AND name = 'session_worktree_cd'",
                &[],
            )
            .await;
        match probe {
            Ok(rows) if !rows.is_empty() => BOTH_OBSERVATION_RELATIONS,
            _ => "session_worktree",
        }
    }
}

/// Record a session worktree observation (insert-only, idempotent on the
/// state's own id): `true` when this state had not been observed before.
///
/// A basis in [`crate::schema::SESSION_WORKTREE_BASES`] goes to
/// `session_worktree`, as it always has; any other goes to
/// `session_worktree_cd`, whose CHECK admits that vocabulary plus `shell_cd`.
/// One observation is written to one relation, never both. The CHECK
/// constraints enforce the vocabulary, and — because this is `INSERT OR IGNORE`, which
/// SQLite applies to CHECK violations as well as to key conflicts — an unknown
/// token DROPS THE ROW and returns `false` rather than raising. Said plainly
/// rather than left to be discovered: every caller in this workspace passes a
/// `&'static str` from the closed set, which is what keeps the silent arm
/// unreachable.
///
/// # Errors
/// [`rift_core::RiftError::Storage`]/[`rift_core::RiftError::Immutability`] on
/// a non-conflict failure.
pub async fn record_session_worktree(
    store: &impl CanonicalStore,
    r: &SessionWorktreeRow,
) -> Result<bool> {
    let table = if crate::schema::SESSION_WORKTREE_BASES.contains(&r.basis.as_str()) {
        "session_worktree"
    } else {
        "session_worktree_cd"
    };
    store
        .insert_or_ignore(
            table,
            &[
                "observation_id",
                "session_id",
                "worktree_token",
                "worktree_name",
                "is_linked",
                "work_tree",
                "branch",
                "head_oid",
                "subagent_id",
                "basis",
                "first_observed_at",
                "schema_version",
            ],
            &[
                Value::text(r.observation_id.clone()),
                Value::text(r.session_id.clone()),
                Value::text(r.worktree_token.clone()),
                Value::text(r.worktree_name.clone()),
                Value::Int(r.is_linked),
                Value::text(r.work_tree.clone()),
                r.branch.clone().map_or(Value::Null, Value::text),
                r.head_oid.clone().map_or(Value::Null, Value::text),
                r.subagent_id.clone().map_or(Value::Null, Value::text),
                Value::text(r.basis.clone()),
                Value::Int(r.first_observed_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// Record a `checkpoint_worktree` projection row (insert-only, idempotent):
/// the worktree the shard's `metadata.json` names as its archiver.
///
/// A PROJECTION, not a new fact: the bridge re-derives it from the shard on
/// every rebuild, so re-recording an existing checkpoint is a no-op (`false`),
/// never an `UPDATE`. A shard WITHOUT the member calls this not at all — the
/// absent row is what every reader renders as `unknown`; a sealed shard is
/// never given an invented one.
///
/// # Errors
/// [`rift_core::RiftError::Storage`]/[`rift_core::RiftError::Immutability`] on
/// a non-conflict failure.
pub async fn record_checkpoint_worktree_if_new(
    store: &impl CanonicalStore,
    checkpoint_id: &str,
    worktree_token: &str,
    worktree_name: &str,
    is_linked: bool,
    branch: Option<&str>,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "checkpoint_worktree",
            &[
                "checkpoint_id",
                "worktree_token",
                "worktree_name",
                "is_linked",
                "branch",
                "schema_version",
            ],
            &[
                Value::text(checkpoint_id.to_owned()),
                Value::text(worktree_token.to_owned()),
                Value::text(worktree_name.to_owned()),
                Value::Int(i64::from(is_linked)),
                branch.map_or(Value::Null, |b| Value::text(b.to_owned())),
                Value::Int(1),
            ],
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;
    use crate::SqliteStore;

    fn observation(id: &str, branch: Option<&str>) -> SessionWorktreeRow {
        SessionWorktreeRow {
            observation_id: id.into(),
            session_id: "sess".into(),
            worktree_token: "0123456789ab".into(),
            worktree_name: "lane-a".into(),
            is_linked: 1,
            work_tree: "/Users/x/rift-worktrees/lane-a".into(),
            branch: branch.map(str::to_owned),
            head_oid: None,
            subagent_id: None,
            basis: "payload_cwd".into(),
            first_observed_at: 10,
            schema_version: 1,
        }
    }

    /// A store holding two observed states of one session: `o1` on branch
    /// `wt/a` and `o2` detached.
    async fn store_with_two_observations() -> SqliteStore {
        let s = store();
        assert!(
            record_session_worktree(&s, &observation("o1", Some("wt/a")))
                .await
                .unwrap()
        );
        assert!(record_session_worktree(&s, &observation("o2", None))
            .await
            .unwrap());
        s
    }

    /// The SAME id is a no-op, never an UPDATE — what makes a redelivered
    /// hook, a sweep and a rebuild collapse to one row — while a different
    /// observed state is a different row: this is history.
    #[tokio::test]
    async fn a_repeated_observation_is_a_no_op_and_a_new_state_is_a_new_row() {
        let s = store_with_two_observations().await;
        assert!(
            !record_session_worktree(&s, &observation("o1", Some("other")))
                .await
                .unwrap()
        );
        assert_eq!(s.count("session_worktree").await.unwrap(), 2);
    }

    /// NULL where the fact is absent: a detached HEAD, an unread oid and a
    /// main-agent observation are NULL, never the empty string.
    #[tokio::test]
    async fn an_absent_branch_head_or_subagent_is_null_not_empty() {
        let s = store_with_two_observations().await;
        let rows = s
            .query(
                "SELECT branch, head_oid, subagent_id FROM session_worktree ORDER BY observation_id",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].get_opt_text(0).unwrap().as_deref(), Some("wt/a"));
        assert_eq!(
            rows[1].get_opt_text(0).unwrap(),
            None,
            "a detached HEAD is NULL, never the empty string"
        );
        assert_eq!(rows[0].get_opt_text(1).unwrap(), None);
        assert_eq!(rows[0].get_opt_text(2).unwrap(), None);
    }

    /// `basis` is a closed vocabulary at the SQL layer, not only at the
    /// caller's: a token outside `SESSION_WORKTREE_BASES` never lands.
    /// `INSERT OR IGNORE` makes that a DROPPED ROW rather than an error —
    /// stated as an assertion because a silent drop is worth knowing about,
    /// and unreachable from the product path only because every caller's
    /// basis is a `&'static str` from the closed set (pinned in
    /// `bins/rift`'s `the_basis_tokens_the_hook_writes_are_the_ones_the_
    /// schema_admits`).
    #[tokio::test]
    async fn an_unknown_basis_writes_nothing() {
        let s = store();
        let mut bogus = observation("o3", None);
        bogus.basis = "guessed".into();
        assert!(
            !record_session_worktree(&s, &bogus).await.unwrap(),
            "an unknown basis writes nothing"
        );
        assert_eq!(
            s.query(
                "SELECT observation_id FROM session_worktree WHERE basis = 'guessed'",
                &[]
            )
            .await
            .unwrap()
            .len(),
            0
        );
    }

    /// The checkpoint twin: one row per checkpoint, re-derived by every
    /// rebuild, and a shard that names no worktree simply never calls it.
    #[tokio::test]
    async fn a_checkpoint_worktree_is_recorded_once_and_a_re_ingest_is_not_a_correction() {
        let s = store();
        assert!(record_checkpoint_worktree_if_new(
            &s,
            "cp1",
            "0123456789ab",
            "lane-a",
            true,
            Some("wt/a")
        )
        .await
        .unwrap());
        assert!(
            !record_checkpoint_worktree_if_new(&s, "cp1", "ffffffffffff", "other", false, None)
                .await
                .unwrap(),
            "a re-ingest of the same checkpoint is a no-op, not a correction"
        );
        let kept = s
            .query(
                "SELECT worktree_name, is_linked, branch FROM checkpoint_worktree",
                &[],
            )
            .await
            .unwrap();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].get_text(0).unwrap(), "lane-a");
        assert_eq!(kept[0].get_i64(1).unwrap(), 1);
        assert_eq!(kept[0].get_opt_text(2).unwrap().as_deref(), Some("wt/a"));
    }
}

#[cfg(test)]
mod worktree_cd_tests {
    use super::*;
    use crate::capture::fixtures::store;

    fn observation(id: &str, basis: &str, at: i64) -> SessionWorktreeRow {
        SessionWorktreeRow {
            observation_id: id.into(),
            session_id: "sess".into(),
            worktree_token: "0123456789ab".into(),
            worktree_name: format!("{basis}-member"),
            is_linked: 1,
            work_tree: "/w/member".into(),
            branch: None,
            head_oid: None,
            subagent_id: None,
            basis: basis.into(),
            first_observed_at: at,
            schema_version: 1,
        }
    }

    /// Every observation of one session, in the order a reader takes them.
    async fn names_in_order(s: &impl CanonicalStore) -> Vec<String> {
        let relation = SessionWorktreeRow::relation(s).await;
        s.query(
            &format!(
                "SELECT worktree_name FROM {relation} WHERE session_id = 'sess' \
                 ORDER BY first_observed_at, observation_id"
            ),
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get_text(0).unwrap())
        .collect()
    }

    /// A `shell_cd` observation lands in the new relation and only there; an
    /// old word lands in the old relation and only there.
    #[tokio::test]
    async fn each_observation_lands_in_exactly_one_relation() {
        let s = store();
        assert!(
            record_session_worktree(&s, &observation("o1", "payload_cwd", 10))
                .await
                .unwrap()
        );
        assert!(
            record_session_worktree(&s, &observation("o2", "shell_cd", 20))
                .await
                .unwrap()
        );
        assert!(
            !record_session_worktree(&s, &observation("o2", "shell_cd", 30))
                .await
                .unwrap()
        );
        assert_eq!(s.count("session_worktree").await.unwrap(), 1);
        assert_eq!(s.count("session_worktree_cd").await.unwrap(), 1);
    }

    /// An unknown word still writes nothing anywhere: the new relation's
    /// CHECK refuses it exactly as the old one did.
    #[tokio::test]
    async fn an_unknown_basis_lands_in_neither_relation() {
        let s = store();
        assert!(
            !record_session_worktree(&s, &observation("o3", "guessed", 10))
                .await
                .unwrap()
        );
        assert_eq!(s.count("session_worktree").await.unwrap(), 0);
        assert_eq!(s.count("session_worktree_cd").await.unwrap(), 0);
    }

    /// The reader's relation is the union, ordered by first observation, and
    /// on a projection without the new relation it is the old one alone.
    #[tokio::test]
    async fn the_reader_relation_is_the_ordered_union_and_falls_back_without_it() {
        let s = store();
        for (id, basis, at) in [
            ("o1", "payload_cwd", 30),
            ("o2", "shell_cd", 20),
            ("o3", "process_cwd", 10),
        ] {
            assert!(record_session_worktree(&s, &observation(id, basis, at))
                .await
                .unwrap());
        }
        assert_eq!(
            names_in_order(&s).await,
            [
                "process_cwd-member",
                "shell_cd-member",
                "payload_cwd-member"
            ]
        );

        s.with_conn(|conn| conn.execute_batch("DROP TABLE session_worktree_cd"))
            .unwrap();
        assert_eq!(SessionWorktreeRow::relation(&s).await, "session_worktree");
        assert_eq!(
            names_in_order(&s).await,
            ["process_cwd-member", "payload_cwd-member"]
        );
    }
}
