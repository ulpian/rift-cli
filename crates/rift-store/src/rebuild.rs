//! Rebuild the (rebuildable) projection from scratch.
//!
//! The projection is derived from canonical git ([`04 §4.1`]), so deleting the DB
//! file is **not** data loss — it is the recovery path when the schema advances or
//! when the OS-keychain KEK is lost (the wrapped DEK can no longer be unwrapped, so
//! data encrypted under the old key is unreadable; we re-create the projection from
//! git with a fresh key, `05-security` Step 5). This deliberately works at the
//! filesystem level, sidestepping the additive-only migration guard (which forbids
//! `DROP`): a rebuild removes the file and re-applies the migrations from zero.

use std::path::{Path, PathBuf};

use rift_core::Result;

use crate::migrate::{apply_migrations, default_migrations};
use crate::SqliteStore;

/// Delete the projection DB (and its SQLite sidecars) at `path`, then open a fresh,
/// fully-migrated, empty store. Idempotent: a missing file is not an error.
///
/// # Errors
/// [`rift_core::RiftError::Io`] if a file cannot be removed, or
/// [`rift_core::RiftError::Migration`] if the fresh schema cannot be applied.
pub fn rebuild(path: &Path) -> Result<SqliteStore> {
    remove_if_exists(path)?;
    remove_if_exists(&sidecar(path, "-wal"))?;
    remove_if_exists(&sidecar(path, "-shm"))?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let store = SqliteStore::open(path)?;
    apply_migrations(&store, &default_migrations())?;
    Ok(store)
}

fn remove_if_exists(p: &Path) -> Result<()> {
    match std::fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanonicalStore, Value};

    #[tokio::test]
    async fn rebuild_recreates_a_clean_migrated_projection() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");

        // Populate a projection.
        {
            let store = SqliteStore::open(&db).unwrap();
            apply_migrations(&store, &default_migrations()).unwrap();
            store
                .insert(
                    "checkpoint",
                    &[
                        "checkpoint_id",
                        "strategy",
                        "created_at",
                        "merkle_root",
                        "prev_hash",
                        "schema_version",
                    ],
                    &[
                        Value::text("ckpt_1"),
                        Value::text("s"),
                        Value::Int(1),
                        Value::text("r"),
                        Value::Null,
                        Value::Int(1),
                    ],
                )
                .await
                .unwrap();
            assert_eq!(store.count("checkpoint").await.unwrap(), 1);
        }

        // Rebuild: the file is recreated, migrated, and empty — and usable again.
        let fresh = rebuild(&db).unwrap();
        assert!(db.exists());
        assert_eq!(fresh.count("checkpoint").await.unwrap(), 0);
        fresh
            .insert(
                "checkpoint",
                &[
                    "checkpoint_id",
                    "strategy",
                    "created_at",
                    "merkle_root",
                    "prev_hash",
                    "schema_version",
                ],
                &[
                    Value::text("ckpt_2"),
                    Value::text("s"),
                    Value::Int(2),
                    Value::text("r2"),
                    Value::Null,
                    Value::Int(1),
                ],
            )
            .await
            .unwrap();
        assert_eq!(fresh.count("checkpoint").await.unwrap(), 1);
    }

    #[test]
    fn rebuild_is_idempotent_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("sub").join("projection.db");
        // No file yet — rebuild still produces a fresh migrated store.
        let store = rebuild(&db).unwrap();
        assert!(db.exists());
        drop(store);
    }
}
