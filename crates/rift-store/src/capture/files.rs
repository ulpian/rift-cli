//! The diff-capture relations: a `file_revision` is one content-addressed
//! `path@blob_oid` fact and a `changed_hunk` is one hunk of a commit's diff
//! landing in such a revision.

use rift_core::Result;

use crate::{CanonicalStore, Value};

/// A `file_revision` row: a `path@blob_oid` is one immutable, content-addressed
/// fact, so re-ingesting it is a no-op rather than a second row.
#[derive(Debug, Clone)]
pub struct FileRevisionRow {
    /// `= sha256(path ‖ 0x00 ‖ blob_oid)` (hex).
    pub file_revision_id: String,
    /// File path.
    pub path: String,
    /// Git blob oid (hex).
    pub blob_oid: String,
    /// Byte-compatible content hash (hex).
    pub content_sha256: String,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `file_revision` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_file_revision(
    store: &impl CanonicalStore,
    r: &FileRevisionRow,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "file_revision",
            &[
                "file_revision_id",
                "path",
                "blob_oid",
                "content_sha256",
                "schema_version",
            ],
            &[
                Value::text(r.file_revision_id.clone()),
                Value::text(r.path.clone()),
                Value::text(r.blob_oid.clone()),
                Value::text(r.content_sha256.clone()),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// A `changed_hunk` row: a committed-diff hunk for `C`.
#[derive(Debug, Clone)]
pub struct ChangedHunkRow {
    /// `= hash(commit_id, file_revision_id, old_start, new_start)` (hex).
    pub hunk_id: String,
    /// The commit whose diff this hunk belongs to.
    pub commit_id: String,
    /// The file revision this hunk lands in (hex).
    pub file_revision_id: String,
    /// Old/new half-open line ranges.
    pub old_start: i64,
    /// Old length.
    pub old_len: i64,
    /// New start.
    pub new_start: i64,
    /// New length.
    pub new_len: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `changed_hunk` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_changed_hunk(store: &impl CanonicalStore, r: &ChangedHunkRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "changed_hunk",
            &[
                "hunk_id",
                "commit_id",
                "file_revision_id",
                "old_start",
                "old_len",
                "new_start",
                "new_len",
                "schema_version",
            ],
            &[
                Value::text(r.hunk_id.clone()),
                Value::text(r.commit_id.clone()),
                Value::text(r.file_revision_id.clone()),
                Value::Int(r.old_start),
                Value::Int(r.old_len),
                Value::Int(r.new_start),
                Value::Int(r.new_len),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;

    fn file_rev(id: &str) -> FileRevisionRow {
        FileRevisionRow {
            file_revision_id: id.into(),
            path: "src/lib.rs".into(),
            // distinct oid per id so UNIQUE(path, blob_oid) holds (in reality the
            // id is *derived* from (path, blob_oid), so they co-vary)
            blob_oid: format!("oid-{id}"),
            content_sha256: "def456".into(),
            schema_version: 1,
        }
    }

    #[tokio::test]
    async fn file_revision_insert_is_idempotent() {
        let s = store();
        assert!(
            record_file_revision(&s, &file_rev("rev1")).await.unwrap(),
            "first insert"
        );
        assert!(
            !record_file_revision(&s, &file_rev("rev1")).await.unwrap(),
            "re-insert of same id is a no-op (set-union)"
        );
        assert_eq!(s.count("file_revision").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn changed_hunk_round_trips_and_is_idempotent() {
        let s = store();
        record_file_revision(&s, &file_rev("rev1")).await.unwrap();
        let h = ChangedHunkRow {
            hunk_id: "hunk1".into(),
            commit_id: "commitA".into(),
            file_revision_id: "rev1".into(),
            old_start: 1,
            old_len: 1,
            new_start: 1,
            new_len: 2,
            schema_version: 1,
        };
        assert!(record_changed_hunk(&s, &h).await.unwrap());
        assert!(!record_changed_hunk(&s, &h).await.unwrap(), "idempotent");
        assert_eq!(s.count("changed_hunk").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn distinct_keys_accumulate() {
        let s = store();
        record_file_revision(&s, &file_rev("rev1")).await.unwrap();
        record_file_revision(&s, &file_rev("rev2")).await.unwrap();
        assert_eq!(s.count("file_revision").await.unwrap(), 2);
    }
}
