//! Persisted boundary collections do not establish historical authorship.

use rift_core::Result;
use rift_git::BlobReader;
use rift_shadow_attribution::CommitCapture;
use rift_store::CanonicalStore;

/// Preserve a commit capture unchanged. Current boundary facts do not carry
/// immutable event-time bytes and causal linkage before the target commit.
/// File kind, collection intervals and plausible replay lengths cannot supply
/// that missing evidence. Historical stored attribution rows remain unchanged.
///
/// Pure replay over explicitly supplied actors remains available in
/// `rift_shadow_attribution`; this compatibility API certifies no stored rows.
///
/// # Errors
/// This conservative implementation performs no I/O and returns success.
pub async fn enrich_attribution(
    _store: &impl CanonicalStore,
    _reader: &impl BlobReader,
    _agent: &str,
    _native_session: &str,
    _capture: &mut CommitCapture,
) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids;
    use rift_git::TreeEntryKind;
    use rift_git::{MemoryBlobReader, TreeEntry};
    use rift_shadow_attribution::capture_commit;
    use rift_store::BoundaryFileKind;
    use rift_store::{
        record_turn_boundary_file_with_kind, record_turn_boundary_snapshot, record_turn_diff_hunk,
        SqliteStore, TurnBoundaryFileRow, TurnBoundarySnapshotRow, TurnDiffHunkRow,
    };
    use std::collections::BTreeMap;

    use crate::turn_boundary::Boundary;

    const AGENT: &str = "claude-code";
    const NATIVE: &str = "sess-1";
    const COMMIT_STUB: &str = rift_shadow_attribution::capture::METHOD_VERSION;

    #[tokio::test]
    async fn stored_observations_preserve_nondefault_confidence_and_detail_exactly() {
        let s = store();
        seed(&s, &[(0, Boundary::Start)], ("a.txt", "A0")).await;
        let r = reader();
        let mut capture = capture_commit(&r, "c1").unwrap();
        let f = capture
            .files
            .iter_mut()
            .find(|f| f.path == "a.txt")
            .unwrap();
        f.confidence = 0.731;
        f.hunk_categories = vec![rift_shadow_attribution::HunkAttribution {
            hunk_ordinal: 0,
            new_start: f.hunks[0].new_start,
            new_len: 1,
            category: "unknown".into(),
            line_count: 1,
        }];
        let before = format!("{capture:?}");
        enrich_attribution(&s, &r, AGENT, NATIVE, &mut capture)
            .await
            .unwrap();
        assert_eq!(format!("{capture:?}"), before);
    }

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    fn entry(path: &str, oid: &str) -> TreeEntry {
        TreeEntry {
            path: path.into(),
            blob_oid: oid.into(),
        }
    }

    /// parent `c0`: `a.txt` = "one\ntwo\n"; commit `c1` appends "three\n" and
    /// ADDS `added.txt`. `other.txt` is added by `c1` and never observed.
    fn reader() -> MemoryBlobReader {
        MemoryBlobReader::new()
            .with_blob("A0", b"one\ntwo\n")
            .with_blob("A1", b"one\ntwo\nthree\n")
            .with_blob("O1", b"unobserved\n")
            .with_blob("ADD1", b"x\ny\n")
            .with_tree("c0", vec![entry("a.txt", "A0")])
            .with_tree(
                "c1",
                vec![
                    entry("a.txt", "A1"),
                    entry("other.txt", "O1"),
                    entry("added.txt", "ADD1"),
                ],
            )
            .with_parent("c1", "c0")
            .with_tree_kinds(
                "c0",
                BTreeMap::from([("a.txt".to_owned(), TreeEntryKind::Regular)]),
            )
            .with_tree_kinds(
                "c1",
                ["a.txt", "other.txt", "added.txt"]
                    .into_iter()
                    .map(|path| (path.to_owned(), TreeEntryKind::Regular))
                    .collect(),
            )
    }

    /// Record the given boundaries in order; the anchor file `(path, blob_oid)`
    /// is attached to the LAST one recorded — the snapshot the ordered read
    /// selects, so the actor and the anchor always come from the same row.
    async fn seed(s: &SqliteStore, boundaries: &[(u32, Boundary)], anchor: (&str, &str)) {
        seed_with_kind(s, boundaries, anchor, BoundaryFileKind::Regular).await;
    }

    async fn seed_with_kind(
        s: &SqliteStore,
        boundaries: &[(u32, Boundary)],
        anchor: (&str, &str),
        kind: BoundaryFileKind,
    ) {
        let session_row = ids::session_row_id(AGENT, NATIVE);
        let mut last = String::new();
        for (ordinal, b) in boundaries {
            let snapshot_id = ids::turn_boundary_snapshot_id(&session_row, *ordinal, b.as_str());
            record_turn_boundary_snapshot(
                s,
                &TurnBoundarySnapshotRow {
                    snapshot_id: snapshot_id.clone(),
                    session_id: session_row.clone(),
                    turn_ordinal: i64::from(*ordinal),
                    boundary: b.as_str().to_owned(),
                    actor: b.actor().to_owned(),
                    worktree_hash: "wh".into(),
                    captured_at: 1,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
            last = snapshot_id;
        }
        record_turn_boundary_file_with_kind(
            s,
            &TurnBoundaryFileRow {
                snapshot_id: last,
                path: anchor.0.to_owned(),
                blob_oid: anchor.1.to_owned(),
                content_sha256: "sha".into(),
                schema_version: 1,
            },
            kind,
        )
        .await
        .unwrap();
    }

    async fn enrich(s: &SqliteStore, r: &MemoryBlobReader) -> CommitCapture {
        let mut capture = capture_commit(r, "c1").unwrap();
        let before = format!("{capture:?}");
        enrich_attribution(s, r, AGENT, NATIVE, &mut capture)
            .await
            .unwrap();
        assert_eq!(
            format!("{capture:?}"),
            before,
            "all commit facts remain unchanged"
        );
        capture
    }

    fn file<'a>(c: &'a CommitCapture, path: &str) -> &'a rift_shadow_attribution::FileChange {
        c.files.iter().find(|f| f.path == path).expect("file")
    }

    // ----- a file committed mid-session -----------------------------------

    /// One boundary snapshot with one observed file row.
    async fn boundary_with_file(
        s: &SqliteStore,
        ordinal: u32,
        b: Boundary,
        path: &str,
        blob_oid: &str,
    ) {
        let session_row = ids::session_row_id(AGENT, NATIVE);
        let snapshot_id = ids::turn_boundary_snapshot_id(&session_row, ordinal, b.as_str());
        record_turn_boundary_snapshot(
            s,
            &TurnBoundarySnapshotRow {
                snapshot_id: snapshot_id.clone(),
                session_id: session_row.clone(),
                turn_ordinal: i64::from(ordinal),
                boundary: b.as_str().to_owned(),
                actor: b.actor().to_owned(),
                worktree_hash: "wh".into(),
                captured_at: 1,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
        record_turn_boundary_file_with_kind(
            s,
            &TurnBoundaryFileRow {
                snapshot_id: snapshot_id.clone(),
                path: path.to_owned(),
                blob_oid: blob_oid.to_owned(),
                content_sha256: "sha".into(),
                schema_version: 1,
            },
            BoundaryFileKind::Regular,
        )
        .await
        .unwrap();
    }

    /// One recorded hunk for `a.txt` (0-based, half-open coordinates).
    async fn hunk_row(s: &SqliteStore, ordinal: u32, kind: &str, coords: (u32, u32, u32, u32)) {
        let session_row = ids::session_row_id(AGENT, NATIVE);
        let (os, ol, ns, nl) = coords;
        record_turn_diff_hunk(
            s,
            &TurnDiffHunkRow {
                hunk_id: ids::turn_diff_hunk_id(
                    &session_row,
                    ordinal,
                    kind,
                    "a.txt",
                    os,
                    ol,
                    ns,
                    nl,
                ),
                session_id: session_row,
                turn_ordinal: i64::from(ordinal),
                edit_kind: kind.to_owned(),
                path: "a.txt".into(),
                old_start: i64::from(os),
                old_len: i64::from(ol),
                new_start: i64::from(ns),
                new_len: i64::from(nl),
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    }

    /// The mid-session shape: `c0` holds `a.txt` = "base"; turn 1's agent
    /// appends two lines (committed as `c1` MID-SESSION); turn 2's human
    /// appends one and its agent one more; `c2` commits those two over `c1`.
    fn mid_session_reader() -> MemoryBlobReader {
        MemoryBlobReader::new()
            .with_blob("A0", b"base\n")
            .with_blob("A1", b"base\na1\na2\n")
            .with_blob("A3", b"base\na1\na2\nh1\na3\n")
            .with_tree("c0", vec![entry("a.txt", "A0")])
            .with_tree("c1", vec![entry("a.txt", "A1")])
            .with_tree("c2", vec![entry("a.txt", "A3")])
            .with_parent("c1", "c0")
            .with_parent("c2", "c1")
            .with_tree_kinds(
                "c0",
                BTreeMap::from([("a.txt".to_owned(), TreeEntryKind::Regular)]),
            )
            .with_tree_kinds(
                "c1",
                BTreeMap::from([("a.txt".to_owned(), TreeEntryKind::Regular)]),
            )
            .with_tree_kinds(
                "c2",
                BTreeMap::from([("a.txt".to_owned(), TreeEntryKind::Regular)]),
            )
    }

    /// The session's full hunk stack for the shape above, with the boundary
    /// that closed turn 1 observing `first_end_blob` (the parent of `c2` when
    /// the record is honest).
    async fn seed_mid_session(s: &SqliteStore, first_end_blob: &str) {
        hunk_row(s, 1, "agent", (1, 0, 1, 2)).await;
        boundary_with_file(s, 1, Boundary::End, "a.txt", first_end_blob).await;
        hunk_row(s, 2, "human", (3, 0, 3, 1)).await;
        boundary_with_file(s, 2, Boundary::Start, "a.txt", "A2").await;
        hunk_row(s, 2, "agent", (4, 0, 4, 1)).await;
        boundary_with_file(s, 2, Boundary::End, "a.txt", "A3").await;
    }

    /// A stored parent OID and hunk stack cannot certify historical writers.
    #[tokio::test]
    async fn a_stored_parent_watermark_cannot_certify_mid_session_authorship() {
        let s = store();
        let r = mid_session_reader();
        seed_mid_session(&s, "A1").await;
        let mut capture = capture_commit(&r, "c2").unwrap();
        assert_eq!(file(&capture, "a.txt").base_blob_oid.as_deref(), Some("A1"));
        enrich_attribution(&s, &r, AGENT, NATIVE, &mut capture)
            .await
            .unwrap();
        let f = file(&capture, "a.txt");
        assert_eq!(f.attribution.human_lines, 0, "{:?}", f.attribution);
        assert_eq!(f.attribution.agent_lines, 0, "{:?}", f.attribution);
        assert_eq!(f.attribution.unknown_lines, 2, "{:?}", f.attribution);
        assert_eq!(f.method_version, COMMIT_STUB);
    }

    /// Missing parent observation is another unsupported stored history.
    /// Global refusal performs no watermark lookup or replay.
    #[tokio::test]
    async fn no_boundary_carries_the_parent_so_the_stack_is_not_rebased() {
        let s = store();
        let r = mid_session_reader();
        seed_mid_session(&s, "A1-never-observed").await;
        let mut capture = capture_commit(&r, "c2").unwrap();
        enrich_attribution(&s, &r, AGENT, NATIVE, &mut capture)
            .await
            .unwrap();
        let f = file(&capture, "a.txt");
        assert_eq!(f.attribution.human_lines, 0, "{:?}", f.attribution);
        assert_eq!(f.attribution.agent_lines, 0, "{:?}", f.attribution);
        assert_eq!(f.attribution.unknown_lines, 2, "{:?}", f.attribution);
    }

    /// Assert that the global conservative policy has not named any writer.
    fn assert_refused(c: &CommitCapture, path: &str) {
        let f = file(c, path);
        assert_eq!(f.attribution.agent_lines, 0, "{path}: no invented author");
        assert_eq!(f.attribution.human_lines, 0, "{path}: no invented author");
        assert_eq!(
            f.attribution.human_modified_agent_lines, 0,
            "{path}: no invented author"
        );
        assert_eq!(f.method_version, COMMIT_STUB, "{path}: keeps the stub");
    }

    // --- ordinary stored observations are also insufficient ----------------

    /// A stored start token is not proof of the changed line's writer.
    #[tokio::test]
    async fn a_stored_open_agent_window_keeps_commit_authorship_unknown() {
        let s = store();
        seed(&s, &[(0, Boundary::Start)], ("a.txt", "A0")).await;
        let c = enrich(&s, &reader()).await;
        let a = file(&c, "a.txt");
        assert_eq!(
            a.attribution.agent_lines, 0,
            "a stored start token cannot identify the writer"
        );
        assert_eq!(a.attribution.human_lines, 0);
        assert_eq!(a.attribution.unknown_lines, 1);
        assert_eq!(a.method_version, COMMIT_STUB);
        assert_refused(&c, "a.txt");
    }

    /// Even bracketed start/end counts do not prove a historical writer.
    #[tokio::test]
    async fn a_stored_bracketed_turn_keeps_commit_authorship_unknown() {
        let s = store();
        seed(
            &s,
            &[(0, Boundary::Start), (0, Boundary::End)],
            ("a.txt", "A0"),
        )
        .await;
        let c = enrich(&s, &reader()).await;
        let a = file(&c, "a.txt");
        assert_eq!(a.attribution.human_lines, 0);
        assert_eq!(a.attribution.agent_lines, 0);
    }

    // --- ATTRv's plants: each must be REFUSED, not merely re-signed ---------

    /// Dropped-end facts remain unmodified under global conservative refusal.
    /// This does not execute the retired per-window actor inference.
    #[tokio::test]
    async fn plant_a_dropped_end_is_refused() {
        let s = store();
        // turn 0 start (its `end` never fired), turn 1 start+end.
        seed(
            &s,
            &[
                (0, Boundary::Start),
                (1, Boundary::Start),
                (1, Boundary::End),
            ],
            ("a.txt", "A0"),
        )
        .await;
        let c = enrich(&s, &reader()).await;
        assert_refused(&c, "a.txt");
    }

    /// Dropped-start facts cannot certify a writer.
    #[tokio::test]
    async fn plant_b_dropped_start_is_refused() {
        let s = store();
        // turn 0's `start` never fired; turn 1 opened and is still open.
        seed(
            &s,
            &[(0, Boundary::End), (1, Boundary::Start)],
            ("a.txt", "A0"),
        )
        .await;
        let c = enrich(&s, &reader()).await;
        assert_refused(&c, "a.txt");
    }

    /// Unavailable historical bytes cannot establish added-file authorship.
    #[tokio::test]
    async fn plant_c_absent_anchor_blob_on_added_file_is_refused() {
        let s = store();
        seed(&s, &[(0, Boundary::Start)], ("added.txt", "COLLECTED")).await;
        let r = reader(); // "COLLECTED" is deliberately absent from the reader
        let c = enrich(&s, &r).await;
        assert_refused(&c, "added.txt");
    }

    /// Unavailable historical bytes cannot establish modified-file authorship.
    #[tokio::test]
    async fn control_d_absent_anchor_blob_on_modified_file_is_refused() {
        let s = store();
        seed(&s, &[(0, Boundary::Start)], ("a.txt", "COLLECTED")).await;
        let c = enrich(&s, &reader()).await;
        assert_refused(&c, "a.txt");
    }

    // --- the ★ honesty guarantees -----------------------------------------

    /// Seed one `turn_diff_hunk` row directly — the shape a phantom takes once
    /// the write side has already produced it.
    async fn seed_hunk(
        s: &SqliteStore,
        turn: u32,
        kind: &str,
        path: &str,
        h: (i64, i64, i64, i64),
    ) {
        let session_row = ids::session_row_id(AGENT, NATIVE);
        record_turn_diff_hunk(
            s,
            &TurnDiffHunkRow {
                hunk_id: ids::turn_diff_hunk_id(
                    &session_row,
                    turn,
                    kind,
                    path,
                    u32::try_from(h.0).unwrap(),
                    u32::try_from(h.1).unwrap(),
                    u32::try_from(h.2).unwrap(),
                    u32::try_from(h.3).unwrap(),
                ),
                session_id: session_row,
                turn_ordinal: i64::from(turn),
                edit_kind: kind.to_owned(),
                path: path.to_owned(),
                old_start: h.0,
                old_len: h.1,
                new_start: h.2,
                new_len: h.3,
                schema_version: 1,
            },
        )
        .await
        .unwrap();
    }

    /// Old referent-derived hunks remain immutable, but cannot acquire new
    /// certification through a sibling's typed regular anchor. This fixture
    /// deliberately has no kind evidence for added.txt's stored legacy hunk.
    #[tokio::test]
    async fn a_legacy_untyped_phantom_hunk_cannot_be_recertified_by_a_typed_sibling() {
        let s = store();
        seed(&s, &[(0, Boundary::Start)], ("a.txt", "A0")).await;
        seed_hunk(&s, 0, "human", "added.txt", (0, 0, 0, 2)).await;

        let c = enrich(&s, &reader()).await;
        let f = file(&c, "added.txt");
        assert_refused(&c, "added.txt");
        assert_eq!(f.attribution.unknown_lines, 2);
        assert!(crate::turn_boundary::attribution_preview(&s, AGENT, NATIVE)
            .await
            .unwrap()
            .is_none());
    }

    /// Stored observations of one sibling do not certify either file's writer.
    #[tokio::test]
    async fn unanchored_and_observed_siblings_both_stay_unknown() {
        let s = store();
        seed(&s, &[(0, Boundary::Start)], ("a.txt", "A0")).await;
        let c = enrich(&s, &reader()).await;
        assert_refused(&c, "other.txt");
        assert_eq!(file(&c, "other.txt").attribution.unknown_lines, 1);
        assert_refused(&c, "a.txt");
    }

    #[tokio::test]
    async fn session_with_no_observations_enriches_nothing() {
        let s = store();
        let c = enrich(&s, &reader()).await;
        for f in &c.files {
            assert_refused(&c, &f.path);
        }
    }

    #[tokio::test]
    async fn a_refused_empty_anchor_cannot_credit_the_final_window() {
        let s = store();
        seed_with_kind(
            &s,
            &[(0, Boundary::Start)],
            ("added.txt", "EMPTY"),
            BoundaryFileKind::Unknown,
        )
        .await;
        let r = reader().with_blob("EMPTY", b"");
        let capture = enrich(&s, &r).await;
        assert_refused(&capture, "added.txt");
        assert_eq!(file(&capture, "added.txt").attribution.unknown_lines, 2);
    }

    #[tokio::test]
    async fn a_link_payload_that_equals_source_bytes_is_not_a_source_anchor() {
        let s = store();
        seed_with_kind(
            &s,
            &[(0, Boundary::Start)],
            ("a.txt", "A0"),
            BoundaryFileKind::Link,
        )
        .await;
        assert_refused(&enrich(&s, &reader()).await, "a.txt");
    }

    #[tokio::test]
    async fn committed_and_parent_link_modes_cannot_be_replayed_as_regular_source() {
        for commit in ["c0", "c1"] {
            let s = store();
            seed(&s, &[(0, Boundary::Start)], ("a.txt", "A0")).await;
            let r = reader().with_tree_kinds(
                commit,
                BTreeMap::from([("a.txt".to_owned(), TreeEntryKind::Link)]),
            );
            assert_refused(&enrich(&s, &r).await, "a.txt");
        }
    }

    #[tokio::test]
    async fn preview_and_commit_skip_a_whole_file_when_any_hunk_epoch_is_unproven() {
        let s = store();
        seed_mid_session(&s, "A1").await;
        seed_hunk(&s, 3, "human", "a.txt", (0, 0, 0, 1)).await;
        assert!(crate::turn_boundary::attribution_preview(&s, AGENT, NATIVE)
            .await
            .unwrap()
            .is_none());
        let r = mid_session_reader();
        let mut capture = capture_commit(&r, "c2").unwrap();
        enrich_attribution(&s, &r, AGENT, NATIVE, &mut capture)
            .await
            .unwrap();
        assert_refused(&capture, "a.txt");
        assert_eq!(file(&capture, "a.txt").attribution.unknown_lines, 2);
    }

    #[tokio::test]
    async fn preview_does_not_certify_regular_stored_windows() {
        let s = store();
        seed_mid_session(&s, "A1").await;
        assert!(crate::turn_boundary::attribution_preview(&s, AGENT, NATIVE)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn a_known_link_gap_cannot_be_bridged_by_later_regular_hunks() {
        let s = store();
        seed_with_kind(
            &s,
            &[(0, Boundary::Start)],
            ("a.txt", "A0"),
            BoundaryFileKind::Link,
        )
        .await;
        seed_mid_session(&s, "A1").await;
        assert!(crate::turn_boundary::attribution_preview(&s, AGENT, NATIVE)
            .await
            .unwrap()
            .is_none());
        let r = mid_session_reader();
        let mut capture = capture_commit(&r, "c2").unwrap();
        enrich_attribution(&s, &r, AGENT, NATIVE, &mut capture)
            .await
            .unwrap();
        assert_refused(&capture, "a.txt");
    }
}
