//! The turn relations: a turn, its model calls, and the turn-boundary
//! snapshots, files and hunks the attribution replay reads back in order.
//! The ordered selection of a session's latest boundary lives here and
//! nowhere else.

use rift_core::Result;

use crate::{CanonicalStore, Value};

/// A `turn` row: one prompt→response cycle within a session.
#[derive(Debug, Clone)]
pub struct TurnRow {
    /// Content-addressed turn id (`session ‖ ordinal`).
    pub turn_id: String,
    /// The session.
    pub session_id: String,
    /// Order within the session.
    pub ordinal: i64,
    /// Valid-time (epoch ms).
    pub started_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `turn` (idempotent on its content-addressed key; the
/// `UNIQUE(session_id, ordinal)` constraint also absorbs replays).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_turn(store: &impl CanonicalStore, r: &TurnRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "turn",
            &[
                "turn_id",
                "session_id",
                "ordinal",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(r.turn_id.clone()),
                Value::text(r.session_id.clone()),
                Value::Int(r.ordinal),
                Value::Int(r.started_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `model_call` row: one LLM invocation within a turn.
#[derive(Debug, Clone)]
pub struct ModelCallRow {
    /// Content-addressed call id.
    pub call_id: String,
    /// The turn.
    pub turn_id: String,
    /// The model invoked (may differ from `session.model_primary`).
    pub model: String,
    /// Valid-time (epoch ms).
    pub started_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `model_call` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_model_call(store: &impl CanonicalStore, r: &ModelCallRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "model_call",
            &[
                "call_id",
                "turn_id",
                "model",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(r.call_id.clone()),
                Value::text(r.turn_id.clone()),
                Value::text(r.model.clone()),
                Value::Int(r.started_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `turn_boundary_snapshot` row: the working-tree observation that closes an
/// H_i (`start`, human) or A_i (`end`, agent) edit window.
#[derive(Debug, Clone)]
pub struct TurnBoundarySnapshotRow {
    /// `= sha256(session_id ‖ turn_ordinal ‖ boundary)` (hex).
    pub snapshot_id: String,
    /// The session.
    pub session_id: String,
    /// The turn ordinal this boundary belongs to.
    pub turn_ordinal: i64,
    /// `start` | `end`.
    pub boundary: String,
    /// Whose edit window this boundary closes: `human` | `agent`.
    pub actor: String,
    /// Content hash over the boundary's `(path, content_sha256)` set.
    pub worktree_hash: String,
    /// Valid-time (epoch ms).
    pub captured_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `turn_boundary_snapshot` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_turn_boundary_snapshot(
    store: &impl CanonicalStore,
    r: &TurnBoundarySnapshotRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "turn_boundary_snapshot",
            &[
                "snapshot_id",
                "session_id",
                "turn_ordinal",
                "boundary",
                "actor",
                "worktree_hash",
                "captured_at",
                "schema_version",
            ],
            &[
                Value::text(r.snapshot_id.clone()),
                Value::text(r.session_id.clone()),
                Value::Int(r.turn_ordinal),
                Value::text(r.boundary.clone()),
                Value::text(r.actor.clone()),
                Value::text(r.worktree_hash.clone()),
                Value::Int(r.captured_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `turn_boundary_file` row: one file's content hash at a boundary. Only
/// hashes/oids are durable — the bytes stay in the local git object store.
#[derive(Debug, Clone)]
pub struct TurnBoundaryFileRow {
    /// The boundary snapshot (FK).
    pub snapshot_id: String,
    /// File path.
    pub path: String,
    /// Git blob oid of the file bytes at this boundary (hex).
    pub blob_oid: String,
    /// Content hash of those bytes (hex).
    pub content_sha256: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `turn_boundary_file` (idempotent on `(snapshot, path)`).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_turn_boundary_file(
    store: &impl CanonicalStore,
    r: &TurnBoundaryFileRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "turn_boundary_file",
            &[
                "snapshot_id",
                "path",
                "blob_oid",
                "content_sha256",
                "schema_version",
            ],
            &[
                Value::text(r.snapshot_id.clone()),
                Value::text(r.path.clone()),
                Value::text(r.blob_oid.clone()),
                Value::text(r.content_sha256.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `turn_diff_hunk` row: one H_i/A_i hunk (coordinates only) — the edit-stack
/// input the LIFO attribution replay consumes at commit time.
#[derive(Debug, Clone)]
pub struct TurnDiffHunkRow {
    /// `= sha256(session ‖ ordinal ‖ edit_kind ‖ path ‖ coords)` (hex).
    pub hunk_id: String,
    /// The session.
    pub session_id: String,
    /// The turn ordinal.
    pub turn_ordinal: i64,
    /// `human` (H_i) | `agent` (A_i).
    pub edit_kind: String,
    /// File path (boundary-time; renames resolve at commit).
    pub path: String,
    /// Old-side start line.
    pub old_start: i64,
    /// Old-side length.
    pub old_len: i64,
    /// New-side start line.
    pub new_start: i64,
    /// New-side length.
    pub new_len: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `turn_diff_hunk` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_turn_diff_hunk(
    store: &impl CanonicalStore,
    r: &TurnDiffHunkRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "turn_diff_hunk",
            &[
                "hunk_id",
                "session_id",
                "turn_ordinal",
                "edit_kind",
                "path",
                "old_start",
                "old_len",
                "new_start",
                "new_len",
                "schema_version",
            ],
            &[
                Value::text(r.hunk_id.clone()),
                Value::text(r.session_id.clone()),
                Value::Int(r.turn_ordinal),
                Value::text(r.edit_kind.clone()),
                Value::text(r.path.clone()),
                Value::Int(r.old_start),
                Value::Int(r.old_len),
                Value::Int(r.new_start),
                Value::Int(r.new_len),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// **THE ordered selection of a session's last turn-boundary snapshot — one
/// rule, one home.** Highest turn ordinal wins; within a turn `end` supersedes
/// `start`; the snapshot id breaks a tie the unique key
/// `(session_id, turn_ordinal, boundary)` makes unreachable anyway.
///
/// This exists as a function because the rule used to be written out twice —
/// here and in `rift-shadow`'s `turn_boundary.rs` — in two `ORDER BY` clauses
/// differing only by a table alias, across a crate boundary. The property that
/// matters is that the WINDOW'S ACTOR and the ANCHOR BYTES come from the same
/// snapshot, and while the duplicate clauses agreed they made that property a
/// textual coincidence: nothing structural stopped one from being edited. It
/// now holds because there is only one selection to edit, and callers that need
/// both facts take them from a single returned row (see
/// [`turn_boundary_files_for_snapshot`]).
///
/// `None` when the session has no boundary at all.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn latest_turn_boundary_snapshot(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<Option<TurnBoundarySnapshotRow>> {
    let rows = store
        .query(
            "SELECT snapshot_id, session_id, turn_ordinal, boundary, actor, worktree_hash, \
                    captured_at, schema_version \
             FROM turn_boundary_snapshot WHERE session_id = ?1 \
             ORDER BY turn_ordinal DESC, \
                      CASE boundary WHEN 'end' THEN 1 ELSE 0 END DESC, \
                      snapshot_id DESC LIMIT 1",
            &[Value::text(session_id.to_owned())],
        )
        .await?;
    let Some(r) = rows.first() else {
        return Ok(None);
    };
    Ok(Some(TurnBoundarySnapshotRow {
        snapshot_id: r.get_text(0)?,
        session_id: r.get_text(1)?,
        turn_ordinal: r.get_i64(2)?,
        boundary: r.get_text(3)?,
        actor: r.get_text(4)?,
        worktree_hash: r.get_text(5)?,
        captured_at: r.get_i64(6)?,
        schema_version: r.get_i64(7)?,
    }))
}

/// The per-file rows of ONE boundary snapshot, in path order. Paired with
/// [`latest_turn_boundary_snapshot`] this lets a caller take the actor and the
/// anchor bytes from a snapshot it has already identified, rather than running
/// the selection twice and hoping.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn turn_boundary_files_for_snapshot(
    store: &impl CanonicalStore,
    snapshot_id: &str,
) -> Result<Vec<TurnBoundaryFileRow>> {
    let rows = store
        .query(
            "SELECT snapshot_id, path, blob_oid, content_sha256, schema_version \
             FROM turn_boundary_file WHERE snapshot_id = ?1 ORDER BY path ASC",
            &[Value::text(snapshot_id.to_owned())],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(TurnBoundaryFileRow {
            snapshot_id: r.get_text(0)?,
            path: r.get_text(1)?,
            blob_oid: r.get_text(2)?,
            content_sha256: r.get_text(3)?,
            schema_version: r.get_i64(4)?,
        });
    }
    Ok(out)
}

/// The per-file rows of the session's **latest** boundary snapshot (the state
/// the next boundary diffs against) — the composition of the two functions
/// above, and no ordering rule of its own.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn latest_turn_boundary_files(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<Vec<TurnBoundaryFileRow>> {
    let Some(snapshot) = latest_turn_boundary_snapshot(store, session_id).await? else {
        return Ok(Vec::new());
    };
    turn_boundary_files_for_snapshot(store, &snapshot.snapshot_id).await
}

/// Every boundary at which a session observed `path`, oldest first:
/// `(turn_ordinal, boundary, blob_oid)` from `turn_boundary_file` joined to its
/// `turn_boundary_snapshot`, ordered by turn ascending and `start` before
/// `end` within a turn — the same order the session's hunk stack replays in.
/// Read-only; the content-addressed `blob_oid` is what lets a commit's parent
/// blob be matched to the boundary that last observed it.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn turn_boundary_files_for_session_path(
    store: &impl CanonicalStore,
    session_id: &str,
    path: &str,
) -> Result<Vec<(i64, String, String)>> {
    let rows = store
        .query(
            "SELECT s.turn_ordinal, s.boundary, f.blob_oid \
             FROM turn_boundary_file f \
             JOIN turn_boundary_snapshot s ON s.snapshot_id = f.snapshot_id \
             WHERE s.session_id = ?1 AND f.path = ?2 \
             ORDER BY s.turn_ordinal ASC, \
                      CASE s.boundary WHEN 'end' THEN 1 ELSE 0 END ASC, \
                      s.snapshot_id ASC",
            &[
                Value::text(session_id.to_owned()),
                Value::text(path.to_owned()),
            ],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push((r.get_i64(0)?, r.get_text(1)?, r.get_text(2)?));
    }
    Ok(out)
}

/// How many `start` / `end` boundary snapshots exist for a session — the
/// capture-coverage signal attribution confidence keys off.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn turn_boundary_counts(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<(i64, i64)> {
    let starts = store
        .query_opt_i64(
            "SELECT COUNT(*) FROM turn_boundary_snapshot \
             WHERE session_id = ?1 AND boundary = 'start'",
            &[Value::text(session_id.to_owned())],
        )
        .await?
        .unwrap_or(0);
    let ends = store
        .query_opt_i64(
            "SELECT COUNT(*) FROM turn_boundary_snapshot \
             WHERE session_id = ?1 AND boundary = 'end'",
            &[Value::text(session_id.to_owned())],
        )
        .await?
        .unwrap_or(0);
    Ok((starts, ends))
}

/// All `turn_diff_hunk` rows for one session, in replay order: by turn ordinal,
/// then H (human) before A (agent) within a turn — the LIFO edit-stack order
/// `[H_0, A_0, H_1, A_1, …]` — then by path/position for determinism.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn turn_diff_hunks_for_session(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<Vec<TurnDiffHunkRow>> {
    let rows = store
        .query(
            "SELECT hunk_id, session_id, turn_ordinal, edit_kind, path, \
                    old_start, old_len, new_start, new_len, schema_version \
             FROM turn_diff_hunk WHERE session_id = ?1 \
             ORDER BY turn_ordinal ASC, \
                      CASE edit_kind WHEN 'human' THEN 0 ELSE 1 END ASC, \
                      path ASC, new_start ASC, hunk_id ASC",
            &[Value::text(session_id.to_owned())],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(TurnDiffHunkRow {
            hunk_id: r.get_text(0)?,
            session_id: r.get_text(1)?,
            turn_ordinal: r.get_i64(2)?,
            edit_kind: r.get_text(3)?,
            path: r.get_text(4)?,
            old_start: r.get_i64(5)?,
            old_len: r.get_i64(6)?,
            new_start: r.get_i64(7)?,
            new_len: r.get_i64(8)?,
            schema_version: r.get_i64(9)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;

    // --- relations whose READER and WRITER must agree -----------------------
    //
    // Each reader below is paired with its writer: a writer's column list and a reader's `SELECT` list,
    // written apart and kept in step by nothing but attention. The invariant is
    // not "this line executes" — it is that the value which goes in comes back
    // out of the field it went into, and that the reader's stated ordering rule
    // is the one it implements. A transposed pair of same-typed columns is
    // invisible to every other test in this crate.

    fn snapshot(id: &str, turn: i64, boundary: &str) -> TurnBoundarySnapshotRow {
        TurnBoundarySnapshotRow {
            snapshot_id: id.into(),
            session_id: "sess1".into(),
            turn_ordinal: turn,
            boundary: boundary.into(),
            actor: if boundary == "start" {
                "human"
            } else {
                "agent"
            }
            .into(),
            worktree_hash: format!("wt-{id}"),
            captured_at: turn,
            schema_version: 1,
        }
    }

    fn boundary_file(snapshot_id: &str, path: &str) -> TurnBoundaryFileRow {
        TurnBoundaryFileRow {
            snapshot_id: snapshot_id.into(),
            path: path.into(),
            blob_oid: format!("oid-{snapshot_id}-{path}"),
            content_sha256: format!("sha-{snapshot_id}-{path}"),
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn the_latest_boundary_is_the_newest_turn_and_end_beats_start_inside_it() {
        // The documented rule, which is the one attribution diffs against:
        // highest turn ordinal wins, and within a turn `end` supersedes `start`.
        // Both halves are asserted, and THE IDS ARE CHOSEN ADVERSARIALLY: the
        // reader's last tiebreak is `snapshot_id DESC`, so the `start` of the
        // newest turn is named `s9` and its `end` `s2`. A first draft of this
        // test used s1/s2 and stayed green when the `end`-beats-`start` clause
        // was deleted — the fallback sort happened to pick the right row, and
        // the test proved nothing about the clause it names.
        let s = store();
        assert!(latest_turn_boundary_files(&s, "sess1")
            .await
            .unwrap()
            .is_empty());

        for (id, turn, boundary) in [("s0", 0, "start"), ("s9", 1, "start"), ("s2", 1, "end")] {
            assert!(
                record_turn_boundary_snapshot(&s, &snapshot(id, turn, boundary))
                    .await
                    .unwrap()
            );
            assert!(record_turn_boundary_file(&s, &boundary_file(id, "b.rs"))
                .await
                .unwrap());
            assert!(record_turn_boundary_file(&s, &boundary_file(id, "a.rs"))
                .await
                .unwrap());
        }
        assert!(
            !record_turn_boundary_snapshot(&s, &snapshot("s2", 1, "end"))
                .await
                .unwrap(),
            "a retried boundary capture is a no-op"
        );

        let files = latest_turn_boundary_files(&s, "sess1").await.unwrap();
        assert_eq!(files.len(), 2);
        // Reader/writer agreement, field by field — a transposed pair of
        // same-typed text columns is invisible to a count.
        assert_eq!(
            files.iter().map(|f| f.path.clone()).collect::<Vec<_>>(),
            vec!["a.rs".to_string(), "b.rs".to_string()],
            "files come back path-ordered"
        );
        for f in &files {
            assert_eq!(f.snapshot_id, "s2", "the `end` of the newest turn");
            assert_eq!(f.blob_oid, format!("oid-s2-{}", f.path));
            assert_eq!(f.content_sha256, format!("sha-s2-{}", f.path));
            assert_eq!(f.schema_version, 1);
        }
        assert!(latest_turn_boundary_files(&s, "other")
            .await
            .unwrap()
            .is_empty());

        // The coverage signal attribution confidence keys off.
        assert_eq!(turn_boundary_counts(&s, "sess1").await.unwrap(), (2, 1));
        assert_eq!(turn_boundary_counts(&s, "other").await.unwrap(), (0, 0));
    }

    /// **ONE ordering rule, ONE home.** The row that names the open window's
    /// actor and the rows that supply the anchor bytes must be the SAME
    /// snapshot — the property `rift-shadow`'s replay depends on, which used to
    /// rest on two `ORDER BY` clauses in two crates happening to match.
    ///
    /// Same adversarial ids as the test above (the newest turn's `start` sorts
    /// AFTER its `end` by id, so a missing `end`-beats-`start` clause picks the
    /// wrong row rather than falling back to the right one by luck).
    #[tokio::test]
    async fn the_ordered_selection_has_one_home_and_both_readers_name_the_same_snapshot() {
        let s = store();
        assert!(latest_turn_boundary_snapshot(&s, "sess1")
            .await
            .unwrap()
            .is_none());

        for (id, turn, boundary) in [("s0", 0, "start"), ("s9", 1, "start"), ("s2", 1, "end")] {
            record_turn_boundary_snapshot(&s, &snapshot(id, turn, boundary))
                .await
                .unwrap();
            record_turn_boundary_file(&s, &boundary_file(id, "a.rs"))
                .await
                .unwrap();
        }

        let latest = latest_turn_boundary_snapshot(&s, "sess1")
            .await
            .unwrap()
            .expect("a boundary exists");
        assert_eq!(latest.snapshot_id, "s2", "the `end` of the newest turn");
        assert_eq!(
            latest.boundary, "end",
            "the token the open-window derivation reads"
        );
        assert_eq!(latest.turn_ordinal, 1);
        assert_eq!(latest.session_id, "sess1");

        // The files reader draws from that SAME row — not from a rule restated.
        let files = latest_turn_boundary_files(&s, "sess1").await.unwrap();
        assert!(!files.is_empty());
        for f in &files {
            assert_eq!(
                f.snapshot_id, latest.snapshot_id,
                "anchor bytes and window actor must come from one snapshot"
            );
        }
        // And the by-id reader is the same selection with the choice made
        // explicit, so a caller can hold the row and its files together.
        let by_id: Vec<(String, String)> =
            turn_boundary_files_for_snapshot(&s, &latest.snapshot_id)
                .await
                .unwrap()
                .into_iter()
                .map(|f| (f.path, f.blob_oid))
                .collect();
        assert_eq!(
            by_id,
            files
                .iter()
                .map(|f| (f.path.clone(), f.blob_oid.clone()))
                .collect::<Vec<_>>()
        );
        assert!(turn_boundary_files_for_snapshot(&s, "s0")
            .await
            .unwrap()
            .iter()
            .all(|f| f.snapshot_id == "s0"));
    }

    fn diff_hunk(id: &str, turn: i64, kind: &str, path: &str, new_start: i64) -> TurnDiffHunkRow {
        TurnDiffHunkRow {
            hunk_id: id.into(),
            session_id: "sess1".into(),
            turn_ordinal: turn,
            edit_kind: kind.into(),
            path: path.into(),
            old_start: new_start,
            old_len: 1,
            new_start,
            new_len: 2,
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn diff_hunks_come_back_in_lifo_edit_stack_order() {
        // `[H_0, A_0, H_1, A_1, …]` — the order the attribution replay consumes.
        // Inserted deliberately scrambled so insertion order cannot pass for the
        // sort.
        let s = store();
        assert!(turn_diff_hunks_for_session(&s, "sess1")
            .await
            .unwrap()
            .is_empty());
        for h in [
            diff_hunk("h4", 1, "agent", "a.rs", 1),
            diff_hunk("h1", 0, "agent", "a.rs", 5),
            diff_hunk("h3", 1, "human", "a.rs", 1),
            diff_hunk("h0", 0, "human", "b.rs", 1),
            diff_hunk("h2", 0, "agent", "a.rs", 1),
        ] {
            assert!(record_turn_diff_hunk(&s, &h).await.unwrap());
        }
        assert!(
            !record_turn_diff_hunk(&s, &diff_hunk("h0", 0, "human", "b.rs", 1))
                .await
                .unwrap(),
            "a replayed turn must not duplicate its hunks"
        );
        // Another session's hunks must not leak into this replay.
        let mut other = diff_hunk("h9", 0, "human", "z.rs", 1);
        other.session_id = "sess2".into();
        assert!(record_turn_diff_hunk(&s, &other).await.unwrap());

        let hunks = turn_diff_hunks_for_session(&s, "sess1").await.unwrap();
        assert_eq!(
            hunks
                .iter()
                .map(|h| (h.turn_ordinal, h.edit_kind.clone(), h.new_start))
                .collect::<Vec<_>>(),
            vec![
                (0, "human".to_string(), 1),
                (0, "agent".to_string(), 1),
                (0, "agent".to_string(), 5),
                (1, "human".to_string(), 1),
                (1, "agent".to_string(), 1),
            ],
            "turn ascending, human before agent inside a turn, then position"
        );
        let first = hunks.first().unwrap();
        assert_eq!(first.hunk_id, "h0");
        assert_eq!(first.session_id, "sess1");
        assert_eq!(first.path, "b.rs");
        assert_eq!((first.old_start, first.old_len), (1, 1));
        assert_eq!((first.new_start, first.new_len), (1, 2));
        assert_eq!(first.schema_version, 1);
    }
}
