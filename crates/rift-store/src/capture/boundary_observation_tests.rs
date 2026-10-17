//! Audit pairing cannot turn retries or partial legacy writes into new reads.

use super::*;
use crate::{record_turn_boundary_file, SqliteStore};

fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

fn fixture() -> (
    TurnBoundarySnapshotRow,
    Vec<(TurnBoundaryFileRow, BoundaryFileKind)>,
) {
    let file = TurnBoundaryFileRow {
        snapshot_id: "boundary".into(),
        path: "a.txt".into(),
        blob_oid: "blob".into(),
        content_sha256: "content".into(),
        schema_version: 1,
    };
    let hash = Sha256::digest(b"a.txt\0content\n");
    let snapshot = TurnBoundarySnapshotRow {
        snapshot_id: "boundary".into(),
        session_id: "session".into(),
        turn_ordinal: 0,
        boundary: "start".into(),
        actor: "human".into(),
        worktree_hash: format!("{hash:x}"),
        captured_at: 10,
        schema_version: 1,
    };
    (snapshot, vec![(file, BoundaryFileKind::Regular)])
}

fn interval() -> BoundaryCollectionInterval {
    BoundaryCollectionInterval {
        started_at: 30,
        finished_at: 31,
    }
}

#[tokio::test]
async fn bounded_audit_fetches_distinguish_complete_limited_and_invalid() {
    let s = store();
    let (snapshot, files) = fixture();
    assert_eq!(
        bounded_turn_boundary_observation(&s, "boundary", 1)
            .await
            .unwrap(),
        BoundedBoundaryObservation::Missing
    );
    record_turn_boundary_collection(&s, &snapshot, &files, interval())
        .await
        .unwrap();
    assert!(matches!(
        bounded_turn_boundary_observation(&s, "boundary", 1)
            .await
            .unwrap(),
        BoundedBoundaryObservation::Observed(_)
    ));
    assert_eq!(
        bounded_turn_boundary_observation(&s, "boundary", 0)
            .await
            .unwrap(),
        BoundedBoundaryObservation::Limited
    );
    let mut extra = files[0].0.clone();
    extra.path = "extra".into();
    record_turn_boundary_file_with_kind(&s, &extra, BoundaryFileKind::Link)
        .await
        .unwrap();
    assert_eq!(
        bounded_turn_boundary_observation(&s, "boundary", 1)
            .await
            .unwrap(),
        BoundedBoundaryObservation::Limited
    );
    assert!(bounded_turn_boundary_observation(&s, "boundary", 2)
        .await
        .is_err());
}

#[tokio::test]
async fn full_and_bounded_readers_refuse_unsupported_version_or_malformed_interval() {
    for fault in ["version", "snapshot-version", "interval", "hash"] {
        let s = store();
        let (mut snapshot, files) = fixture();
        if fault == "snapshot-version" {
            snapshot.schema_version = 2;
        }
        record_turn_boundary_snapshot(&s, &snapshot).await.unwrap();
        record_turn_boundary_file_with_kind(&s, &files[0].0, files[0].1)
            .await
            .unwrap();
        s.insert(
            "turn_boundary_observation",
            &[
                "snapshot_id",
                "worktree_hash",
                "collection_started_at",
                "collection_finished_at",
                "schema_version",
            ],
            &[
                Value::text("boundary"),
                Value::text(if fault == "hash" {
                    "wrong"
                } else {
                    &snapshot.worktree_hash
                }),
                Value::Int(30),
                Value::Int(if fault == "interval" { 29 } else { 31 }),
                Value::Int(if fault == "version" { 2 } else { 1 }),
            ],
        )
        .await
        .unwrap();
        assert!(turn_boundary_observation_for_snapshot(&s, "boundary")
            .await
            .is_err());
        assert!(bounded_turn_boundary_observation(&s, "boundary", 1)
            .await
            .is_err());
    }
}

#[tokio::test]
async fn an_orphan_audit_cannot_be_adopted_by_a_later_collection() {
    let s = store();
    let (snapshot, files) = fixture();
    s.insert(
        "turn_boundary_observation",
        &[
            "snapshot_id",
            "worktree_hash",
            "collection_started_at",
            "collection_finished_at",
            "schema_version",
        ],
        &[
            Value::text("boundary"),
            Value::text(snapshot.worktree_hash.clone()),
            Value::Int(1),
            Value::Int(2),
            Value::Int(1),
        ],
    )
    .await
    .unwrap();
    assert!(turn_boundary_observation_for_snapshot(&s, "boundary")
        .await
        .is_err());
    assert!(
        record_turn_boundary_collection(&s, &snapshot, &files, interval())
            .await
            .is_err()
    );
    assert_eq!(s.count("turn_boundary_snapshot").await.unwrap(), 0);
    assert_eq!(s.count("turn_boundary_file").await.unwrap(), 0);
}

#[tokio::test]
async fn appended_file_or_kind_rows_make_a_prior_audit_inconsistent() {
    for extra_file in [false, true] {
        let s = store();
        let (snapshot, files) = fixture();
        record_turn_boundary_collection(&s, &snapshot, &files, interval())
            .await
            .unwrap();
        if extra_file {
            let mut extra = files[0].0.clone();
            extra.path = "extra.txt".into();
            record_turn_boundary_file_with_kind(&s, &extra, BoundaryFileKind::Regular)
                .await
                .unwrap();
        } else {
            s.insert(
                "turn_boundary_file_kind",
                &["snapshot_id", "path", "kind", "schema_version"],
                &[
                    Value::text("boundary"),
                    Value::text("extra.txt"),
                    Value::text("regular"),
                    Value::Int(1),
                ],
            )
            .await
            .unwrap();
        }
        assert!(turn_boundary_observation_for_snapshot(&s, "boundary")
            .await
            .is_err());
    }
}

#[tokio::test]
async fn collection_audit_binds_the_new_exact_set_and_retry_is_a_noop() {
    let s = store();
    let (snapshot, files) = fixture();
    assert!(
        record_turn_boundary_collection(&s, &snapshot, &files, interval())
            .await
            .unwrap()
    );
    let before = turn_boundary_observation_for_snapshot(&s, "boundary")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(before.interval, interval());
    assert_eq!(before.worktree_hash, snapshot.worktree_hash);
    assert_eq!(
        turn_boundary_files_for_snapshot(&s, "boundary")
            .await
            .unwrap(),
        vec![files[0].0.clone()]
    );
    assert!(!record_turn_boundary_collection(
        &s,
        &snapshot,
        &files,
        BoundaryCollectionInterval {
            started_at: 99,
            finished_at: 100
        }
    )
    .await
    .unwrap());
    assert_eq!(
        turn_boundary_observation_for_snapshot(&s, "boundary")
            .await
            .unwrap(),
        Some(before)
    );
}

#[tokio::test]
async fn legacy_and_partial_snapshots_never_gain_retry_files_or_audit() {
    for partial in [false, true] {
        let s = store();
        let (snapshot, mut files) = fixture();
        record_turn_boundary_snapshot(&s, &snapshot).await.unwrap();
        if partial {
            record_turn_boundary_file(&s, &files[0].0).await.unwrap();
        }
        files[0].0.blob_oid = "changed-on-retry".into();
        assert!(
            !record_turn_boundary_collection(&s, &snapshot, &files, interval())
                .await
                .unwrap()
        );
        assert_eq!(
            s.count("turn_boundary_file").await.unwrap(),
            i64::from(partial)
        );
        assert_eq!(s.count("turn_boundary_file_kind").await.unwrap(), 0);
        assert!(turn_boundary_observation_for_snapshot(&s, "boundary")
            .await
            .unwrap()
            .is_none());
        if partial {
            assert_eq!(
                turn_boundary_files_for_snapshot(&s, "boundary")
                    .await
                    .unwrap()[0]
                    .blob_oid,
                "blob"
            );
        }
    }
}

#[tokio::test]
async fn wrong_input_identity_hash_or_interval_never_creates_a_snapshot() {
    for fault in 0..6 {
        let s = store();
        let (mut snapshot, mut files) = fixture();
        let mut span = interval();
        match fault {
            0 => files[0].0.snapshot_id = "other".into(),
            1 => snapshot.worktree_hash = "wrong".into(),
            2 => span.finished_at = span.started_at - 1,
            3 => span.started_at = -1,
            4 => snapshot.schema_version = 2,
            _ => files[0].0.schema_version = 2,
        }
        assert!(record_turn_boundary_collection(&s, &snapshot, &files, span)
            .await
            .is_err());
        assert_eq!(s.count("turn_boundary_snapshot").await.unwrap(), 0);
        assert_eq!(s.count("turn_boundary_observation").await.unwrap(), 0);
    }
}

#[tokio::test]
async fn colliding_files_or_kind_facts_leave_collection_audit_absent() {
    for collision in ["file", "kind", "extra"] {
        let s = store();
        let (snapshot, files) = fixture();
        if collision == "file" {
            record_turn_boundary_file(&s, &files[0].0).await.unwrap();
        } else {
            s.insert(
                "turn_boundary_file_kind",
                &["snapshot_id", "path", "kind", "schema_version"],
                &[
                    Value::text("boundary"),
                    Value::text(if collision == "extra" {
                        "orphan"
                    } else {
                        "a.txt"
                    }),
                    Value::text("link"),
                    Value::Int(1),
                ],
            )
            .await
            .unwrap();
        }
        assert!(
            record_turn_boundary_collection(&s, &snapshot, &files, interval())
                .await
                .is_err()
        );
        assert_eq!(s.count("turn_boundary_observation").await.unwrap(), 0);
    }
}

#[tokio::test]
async fn collection_audit_has_sql_update_and_delete_guards() {
    let s = store();
    let (snapshot, files) = fixture();
    record_turn_boundary_collection(&s, &snapshot, &files, interval())
        .await
        .unwrap();
    for sql in [
        "UPDATE turn_boundary_observation SET collection_finished_at = 99",
        "DELETE FROM turn_boundary_observation",
    ] {
        assert!(s.with_conn(|conn| conn.execute(sql, [])).is_err());
    }
    assert_eq!(s.count("turn_boundary_observation").await.unwrap(), 1);
}
