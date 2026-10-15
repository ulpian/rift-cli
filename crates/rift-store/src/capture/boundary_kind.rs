//! Sparse, append-only knowledge of boundary file kinds. Legacy rows without
//! a companion remain unknown; blob bytes cannot establish a file's kind.

use std::collections::{BTreeMap, BTreeSet};

use rift_core::Result;

use crate::{CanonicalStore, TurnBoundaryFileRow, Value};

/// The filesystem fact accompanying one boundary anchor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BoundaryFileKind {
    /// Regular file contents.
    Regular,
    /// Link target spelling, without following the link.
    Link,
    /// Confirmed absence, represented by an empty anchor.
    Absent,
    /// Refused or unstable observation; its bytes establish no baseline.
    Unknown,
}

impl BoundaryFileKind {
    /// Closed storage vocabulary.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Regular => "regular",
            Self::Link => "link",
            Self::Absent => "absent",
            Self::Unknown => "unknown",
        }
    }

    fn from_stored(value: &str) -> Self {
        match value {
            "regular" => Self::Regular,
            "link" => Self::Link,
            "absent" => Self::Absent,
            _ => Self::Unknown,
        }
    }
}

/// Insert a new anchor and its kind knowledge together. A pre-existing
/// anchor is never certified retrospectively, even when its bytes match.
/// A crash between the appends leaves kind unknown and stays conservative.
///
/// # Errors
/// [`rift_core::RiftError`] on a storage failure.
pub async fn record_turn_boundary_file_with_kind(
    store: &impl CanonicalStore,
    row: &TurnBoundaryFileRow,
    kind: BoundaryFileKind,
) -> Result<bool> {
    if !crate::record_turn_boundary_file(store, row).await? {
        return Ok(false);
    }
    record_turn_boundary_file_kind(store, &row.snapshot_id, &row.path, kind).await?;
    Ok(true)
}

async fn record_turn_boundary_file_kind(
    store: &impl CanonicalStore,
    snapshot_id: &str,
    path: &str,
    kind: BoundaryFileKind,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "turn_boundary_file_kind",
            &["snapshot_id", "path", "kind", "schema_version"],
            &[
                Value::text(snapshot_id.to_owned()),
                Value::text(path.to_owned()),
                Value::text(kind.as_str()),
                Value::Int(1),
            ],
        )
        .await
}

/// Known kinds for exactly one snapshot; absence remains unknown.
///
/// # Errors
/// [`rift_core::RiftError`] on a storage failure.
pub async fn turn_boundary_file_kinds_for_snapshot(
    store: &impl CanonicalStore,
    snapshot_id: &str,
) -> Result<BTreeMap<String, BoundaryFileKind>> {
    let rows = store
        .query(
            "SELECT path, kind FROM turn_boundary_file_kind WHERE snapshot_id = ?1 ORDER BY path",
            &[Value::text(snapshot_id.to_owned())],
        )
        .await?;
    let mut out = BTreeMap::new();
    for row in rows {
        out.insert(
            row.get_text(0)?,
            BoundaryFileKind::from_stored(&row.get_text(1)?),
        );
    }
    Ok(out)
}

/// Paths whose recorded hunk epoch has no compatible kind evidence. Skip
/// each entire file when deriving attribution rather than splice an edit
/// stack across an unknowable historical window.
///
/// # Errors
/// [`rift_core::RiftError`] on a storage failure.
pub async fn unproven_turn_hunk_paths(
    store: &impl CanonicalStore,
    session_id: &str,
) -> Result<BTreeSet<String>> {
    let rows = store
        .query(
            "SELECT DISTINCT h.path FROM turn_diff_hunk h \
         LEFT JOIN turn_boundary_snapshot s ON s.session_id = h.session_id \
           AND s.turn_ordinal = h.turn_ordinal \
           AND s.boundary = CASE h.edit_kind WHEN 'agent' THEN 'end' ELSE 'start' END \
         LEFT JOIN turn_boundary_file_kind k ON k.snapshot_id = s.snapshot_id AND k.path = h.path \
         WHERE h.session_id = ?1 AND (k.kind IS NULL OR k.kind NOT IN ('regular', 'absent')) \
         UNION SELECT DISTINCT f.path FROM turn_boundary_file f \
         JOIN turn_boundary_snapshot s ON s.snapshot_id = f.snapshot_id \
         LEFT JOIN turn_boundary_file_kind k ON k.snapshot_id = f.snapshot_id AND k.path = f.path \
         WHERE s.session_id = ?1 AND (k.kind IS NULL OR k.kind NOT IN ('regular', 'absent')) \
         AND EXISTS (SELECT 1 FROM turn_diff_hunk h WHERE h.session_id = ?1 AND h.path = f.path)",
            &[Value::text(session_id.to_owned())],
        )
        .await?;
    rows.iter().map(|row| row.get_text(0)).collect()
}

/// Parent-commit watermarks backed by regular-file kind evidence. A link
/// payload can equal regular source bytes without certifying a source anchor.
///
/// # Errors
/// [`rift_core::RiftError`] on a storage failure.
pub async fn regular_turn_boundary_files_for_session_path(
    store: &impl CanonicalStore,
    session_id: &str,
    path: &str,
) -> Result<Vec<(i64, String, String)>> {
    let rows = store
        .query(
            "SELECT s.turn_ordinal, s.boundary, f.blob_oid FROM turn_boundary_file f \
         JOIN turn_boundary_snapshot s ON s.snapshot_id = f.snapshot_id \
         JOIN turn_boundary_file_kind k ON k.snapshot_id = f.snapshot_id AND k.path = f.path \
         WHERE s.session_id = ?1 AND f.path = ?2 AND k.kind = 'regular' \
         ORDER BY s.turn_ordinal, CASE s.boundary WHEN 'start' THEN 0 ELSE 1 END",
            &[
                Value::text(session_id.to_owned()),
                Value::text(path.to_owned()),
            ],
        )
        .await?;
    rows.iter()
        .map(|row| Ok((row.get_i64(0)?, row.get_text(1)?, row.get_text(2)?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SqliteStore;

    #[tokio::test]
    async fn a_link_payload_equal_to_a_parent_blob_never_certifies_a_watermark() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        for (ordinal, kind) in [(0, BoundaryFileKind::Link), (1, BoundaryFileKind::Regular)] {
            let snapshot = format!("s{ordinal}");
            crate::record_turn_boundary_snapshot(
                &store,
                &crate::TurnBoundarySnapshotRow {
                    snapshot_id: snapshot.clone(),
                    session_id: "session".to_owned(),
                    turn_ordinal: ordinal,
                    boundary: "end".to_owned(),
                    actor: "agent".to_owned(),
                    worktree_hash: "wh".to_owned(),
                    captured_at: 0,
                    schema_version: 1,
                },
            )
            .await
            .unwrap();
            record_turn_boundary_file_with_kind(
                &store,
                &crate::TurnBoundaryFileRow {
                    snapshot_id: snapshot.clone(),
                    path: "file".to_owned(),
                    blob_oid: "identical".to_owned(),
                    content_sha256: "same".to_owned(),
                    schema_version: 1,
                },
                kind,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            regular_turn_boundary_files_for_session_path(&store, "session", "file")
                .await
                .unwrap(),
            vec![(1, "end".to_owned(), "identical".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_kind_is_append_only_and_missing_knowledge_stays_missing() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        assert!(turn_boundary_file_kinds_for_snapshot(&store, "legacy")
            .await
            .unwrap()
            .is_empty());
        assert!(record_turn_boundary_file_with_kind(
            &store,
            &file_row("s", "p"),
            BoundaryFileKind::Link
        )
        .await
        .unwrap());
        assert!(!record_turn_boundary_file_with_kind(
            &store,
            &file_row("s", "p"),
            BoundaryFileKind::Regular
        )
        .await
        .unwrap());
        assert_eq!(
            turn_boundary_file_kinds_for_snapshot(&store, "s")
                .await
                .unwrap()
                .get("p"),
            Some(&BoundaryFileKind::Link)
        );
    }

    #[tokio::test]
    async fn version_18_upgrades_without_rewriting_anchors_or_old_migration_checksums() {
        let store = SqliteStore::open_in_memory().unwrap();
        let migrations = crate::default_migrations();
        let old: Vec<_> = migrations
            .iter()
            .filter(|migration| migration.version <= 18)
            .cloned()
            .collect();
        crate::apply_migrations(&store, &old).unwrap();
        store
            .execute_raw(
                "INSERT INTO turn_boundary_file VALUES ('legacy', 'file', 'oid', 'sha', 1)",
            )
            .unwrap();
        let checksum_sql = "SELECT group_concat(checksum) FROM (SELECT checksum FROM _migrations WHERE version <= 18 ORDER BY version)";
        let before = store.query(checksum_sql, &[]).await.unwrap()[0]
            .get_text(0)
            .unwrap();
        let result = crate::apply_migrations(&store, &migrations[..19]).unwrap();
        assert_eq!(
            (result.applied, result.skipped, result.high_water),
            (1, 18, 19)
        );
        let current = store
            .query("SELECT checksum FROM _migrations WHERE version = 19", &[])
            .await
            .unwrap();
        assert_eq!(
            current[0].get_text(0).unwrap(),
            "0c6bd126662e017fd2c4cdb89c2a14f497f139f07b8420ab7fc4c576ccab35e3"
        );
        let after = store.query(checksum_sql, &[]).await.unwrap()[0]
            .get_text(0)
            .unwrap();
        assert_eq!(before, after);
        let legacy = crate::turn_boundary_files_for_snapshot(&store, "legacy")
            .await
            .unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].blob_oid, "oid");
        assert!(turn_boundary_file_kinds_for_snapshot(&store, "legacy")
            .await
            .unwrap()
            .is_empty());
        record_turn_boundary_file_with_kind(
            &store,
            &file_row("new", "p"),
            BoundaryFileKind::Regular,
        )
        .await
        .unwrap();
        for sql in [
            "UPDATE turn_boundary_file_kind SET kind = 'link'",
            "DELETE FROM turn_boundary_file_kind",
        ] {
            assert!(store.execute_raw(sql).is_err());
        }
        assert!(store
            .execute_raw("INSERT INTO turn_boundary_file_kind VALUES ('bad', 'p', 'invented', 1)")
            .is_err());
    }

    fn file_row(snapshot: &str, path: &str) -> TurnBoundaryFileRow {
        TurnBoundaryFileRow {
            snapshot_id: snapshot.to_owned(),
            path: path.to_owned(),
            blob_oid: "oid".to_owned(),
            content_sha256: "sha".to_owned(),
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn an_existing_legacy_anchor_cannot_be_given_kind_knowledge_on_retry() {
        let store = SqliteStore::open_in_memory().unwrap();
        store.create_canonical_schema().unwrap();
        let legacy = file_row("legacy", "p");
        assert!(crate::record_turn_boundary_file(&store, &legacy)
            .await
            .unwrap());
        assert!(
            !record_turn_boundary_file_with_kind(&store, &legacy, BoundaryFileKind::Regular)
                .await
                .unwrap()
        );
        let altered = TurnBoundaryFileRow {
            blob_oid: "replacement".to_owned(),
            ..legacy
        };
        assert!(
            !record_turn_boundary_file_with_kind(&store, &altered, BoundaryFileKind::Link)
                .await
                .unwrap()
        );
        assert!(turn_boundary_file_kinds_for_snapshot(&store, "legacy")
            .await
            .unwrap()
            .is_empty());
        assert_eq!(
            crate::turn_boundary_files_for_snapshot(&store, "legacy")
                .await
                .unwrap()[0]
                .blob_oid,
            "oid"
        );
    }
}
