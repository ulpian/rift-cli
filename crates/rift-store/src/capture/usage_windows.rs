//! Which model calls a checkpoint froze: its usage window.
//!
//! A checkpoint `C` is cut at a condense, so what it froze of its first
//! session `S` is the run of `S`'s turns since `S` was last checkpointed: the
//! calls of `S` whose start lies in `(created_at(P), created_at(C)]`, where `P`
//! is `S`'s previous dated checkpoint and the window opens at the beginning of
//! time when there is none. A checkpoint carrying the unknown-time sentinel
//! has no window at all — no instant bounds it, and a window guessed from a
//! derived time would put a figure on calls nothing says it froze.
//!
//! Its own module because the window is a fact about checkpoints and
//! sessions, not about prices: the cost relations price whatever calls they
//! are handed, and this module is the one place that says which calls those
//! are. Both the token sum and the money quote join through
//! [`CALL_IN_WINDOW`], so the two can never disagree about which calls are in.

use rift_core::Result;

use crate::{CanonicalStore, Value, CREATED_AT_UNKNOWN};

/// One dated checkpoint's window over its first session's model calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageWindow {
    /// The checkpoint the window is named after.
    pub checkpoint_id: String,
    /// Its first session by ordinal, the one whose calls the window frames.
    pub session_id: String,
    /// The session's previous dated checkpoint's time; `None` when this is
    /// the session's first, and the window then has no lower bound.
    pub after_exclusive: Option<i64>,
    /// The checkpoint's own recorded time, the window's inclusive end.
    pub until: i64,
}

/// Which checkpoints' windows a read covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowScope<'a> {
    /// Every dated checkpoint in the projection.
    Every,
    /// The one checkpoint with this id.
    Checkpoint(&'a str),
}

/// The windows as a CTE `w (checkpoint_id, session_id, after_exclusive,
/// until)`, one row per dated checkpoint that archives a session.
///
/// Binds `?1` = [`CREATED_AT_UNKNOWN`] and `?2` = the checkpoint filter
/// (`NULL` for every checkpoint); [`window_binds`] supplies both, so a caller
/// numbers nothing of its own. The first session ties on ordinal break by
/// session id, so the window never depends on row order. The previous
/// checkpoint is the greatest *dated* `created_at` strictly below this one's
/// among the checkpoints that archive the same session: an undated sibling is
/// no instant, so it cannot close a window.
pub(super) const USAGE_WINDOWS_CTE: &str = "WITH w AS (\
     SELECT c.checkpoint_id AS checkpoint_id, sic.session_id AS session_id, \
     (SELECT MAX(p.created_at) FROM session_in_checkpoint ps \
      JOIN checkpoint p ON p.checkpoint_id = ps.checkpoint_id \
      WHERE ps.session_id = sic.session_id AND p.created_at <> ?1 \
        AND p.created_at < c.created_at) AS after_exclusive, \
     c.created_at AS until \
     FROM checkpoint c \
     JOIN session_in_checkpoint sic ON sic.checkpoint_id = c.checkpoint_id \
     WHERE c.created_at <> ?1 AND (?2 IS NULL OR c.checkpoint_id = ?2) \
       AND sic.session_id = (SELECT f.session_id FROM session_in_checkpoint f \
        WHERE f.checkpoint_id = c.checkpoint_id \
        ORDER BY f.ordinal, f.session_id LIMIT 1)) ";

/// The join that puts a model call `mc` in the window `w` it falls in, through
/// its turn `t`: same session, started after the lower bound and no later
/// than the checkpoint.
///
/// Written once because two readers depend on it — the token sum and the
/// quote — and a second spelling is how the two would come to count
/// different calls under one checkpoint's name.
pub(super) const CALL_IN_WINDOW: &str = "JOIN turn t ON t.turn_id = mc.turn_id \
     JOIN w ON w.session_id = t.session_id AND mc.started_at <= w.until \
     AND (w.after_exclusive IS NULL OR mc.started_at > w.after_exclusive)";

/// The window of every dated checkpoint in `scope` and its first session,
/// ordered by checkpoint id. An undated checkpoint, or one that archives no
/// session, has no row.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn checkpoint_usage_windows(
    store: &impl CanonicalStore,
    scope: WindowScope<'_>,
) -> Result<Vec<UsageWindow>> {
    let rows = store
        .query(
            &format!(
                "{USAGE_WINDOWS_CTE}SELECT checkpoint_id, session_id, after_exclusive, until \
                 FROM w ORDER BY checkpoint_id"
            ),
            &window_binds(scope),
        )
        .await?;
    rows.into_iter()
        .map(|r| {
            Ok(UsageWindow {
                checkpoint_id: r.get_text(0)?,
                session_id: r.get_text(1)?,
                after_exclusive: r.get_opt_i64(2)?,
                until: r.get_i64(3)?,
            })
        })
        .collect()
}

/// The two binds [`USAGE_WINDOWS_CTE`] reads: the sentinel, read from the
/// crate that writes it rather than re-spelled as a literal, then the filter.
pub(super) fn window_binds(scope: WindowScope<'_>) -> Vec<Value> {
    let filter = match scope {
        WindowScope::Every => Value::Null,
        WindowScope::Checkpoint(id) => Value::text(id.to_owned()),
    };
    vec![Value::Int(CREATED_AT_UNKNOWN), filter]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;
    use crate::capture::{record_session_in_checkpoint, SessionInCheckpointRow};
    use crate::SqliteStore;

    /// A checkpoint row recorded at `at`, archiving `sessions` in order.
    async fn checkpoint(s: &SqliteStore, id: &str, at: i64, sessions: &[&str]) {
        s.insert(
            "checkpoint",
            &[
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "schema_version",
            ],
            &[
                Value::text(id.to_owned()),
                Value::text("manual-commit".to_owned()),
                Value::Int(at),
                Value::text("00".to_owned()),
                Value::Int(1),
            ],
        )
        .await
        .unwrap();
        for (ordinal, session) in (0..).zip(sessions) {
            let row = SessionInCheckpointRow {
                checkpoint_id: id.into(),
                session_id: (*session).into(),
                ordinal,
                schema_version: 1,
            };
            record_session_in_checkpoint(s, &row).await.unwrap();
        }
    }

    fn window(ck: &str, session: &str, after: Option<i64>, until: i64) -> UsageWindow {
        UsageWindow {
            checkpoint_id: ck.into(),
            session_id: session.into(),
            after_exclusive: after,
            until,
        }
    }

    #[tokio::test]
    async fn two_checkpoints_of_one_session_split_its_calls_at_the_first() {
        let s = store();
        checkpoint(&s, "ck-1", 100, &["s"]).await;
        checkpoint(&s, "ck-2", 200, &["s"]).await;
        let windows = checkpoint_usage_windows(&s, WindowScope::Every)
            .await
            .unwrap();
        assert_eq!(
            windows,
            [
                window("ck-1", "s", None, 100),
                window("ck-2", "s", Some(100), 200)
            ]
        );
        let one = checkpoint_usage_windows(&s, WindowScope::Checkpoint("ck-2"))
            .await
            .unwrap();
        assert_eq!(one, [window("ck-2", "s", Some(100), 200)]);
    }

    #[tokio::test]
    async fn a_second_session_in_a_checkpoint_does_not_move_the_first_sessions_window() {
        let s = store();
        checkpoint(&s, "ck-1", 100, &["s"]).await;
        checkpoint(&s, "ck-other", 150, &["other"]).await;
        checkpoint(&s, "ck-2", 200, &["s", "other"]).await;
        let one = checkpoint_usage_windows(&s, WindowScope::Checkpoint("ck-2"))
            .await
            .unwrap();
        assert_eq!(one, [window("ck-2", "s", Some(100), 200)]);
    }

    #[tokio::test]
    async fn an_undated_checkpoint_has_no_window_and_closes_none() {
        let s = store();
        checkpoint(&s, "ck-undated", CREATED_AT_UNKNOWN, &["s"]).await;
        checkpoint(&s, "ck-2", 200, &["s"]).await;
        checkpoint(&s, "ck-empty", 300, &[]).await;
        let windows = checkpoint_usage_windows(&s, WindowScope::Every)
            .await
            .unwrap();
        assert_eq!(windows, [window("ck-2", "s", None, 200)]);
    }
}
