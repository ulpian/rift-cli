//! The worktree provenance relations: which worktree a session was observed
//! in, and which worktree a checkpoint's shard names as its archiver.

use rift_core::Result;

use crate::{CanonicalStore, Value};

/// A `session_worktree` row: one OBSERVATION of the worktree a session was
/// seen in.
///
/// Append-only and content-addressed over the observed state, so a session's
/// history is the ordered set of its rows and nothing is ever updated. The
/// caller derives `observation_id`
/// (`rift_shadow::ids::session_worktree_observation_id`) — as every other
/// content-addressed row here does — and screens `basis` against
/// [`crate::schema::SESSION_WORKTREE_BASES`] before this DAO sees it (the CHECK
/// constraint refuses anything else regardless).
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
    /// [`crate::schema::SESSION_WORKTREE_BASES`].
    pub basis: String,
    /// Epoch ms of the FIRST insert. A repeat observation keeps this value,
    /// because the id dedups before the timestamp is ever compared — which is
    /// exactly why the id must not include the time.
    pub first_observed_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `session_worktree` observation (insert-only, idempotent on the
/// state's own id): `true` when this state had not been observed before.
///
/// `basis` must be one of [`crate::schema::SESSION_WORKTREE_BASES`]. The CHECK
/// constraint enforces that, and — because this is `INSERT OR IGNORE`, which
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
    store
        .insert_or_ignore(
            "session_worktree",
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
