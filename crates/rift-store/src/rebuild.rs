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

use crate::migrate::default_migrations;
use crate::SqliteStore;

/// Delete the projection DB (and its SQLite sidecars) at `path`, then open a
/// fresh, fully-migrated, empty store whose **rebuild epoch** is the prior
/// projection's epoch + 1 (ADR-0010). Idempotent: a missing file is not an
/// error (its epoch reads as zero, so the first rebuild lands epoch 1); a
/// corrupt/unreadable prior file degrades the same way — the epoch still
/// *changes* whenever the prior store was readable, which is what the poller
/// keys on.
///
/// # Errors
/// [`rift_core::RiftError::Io`] if a file cannot be removed, or
/// [`rift_core::RiftError::Migration`] if the fresh schema cannot be applied.
pub fn rebuild(path: &Path) -> Result<SqliteStore> {
    let prior_epoch = read_epoch_best_effort(path);
    remove_if_exists(path)?;
    remove_if_exists(&sidecar(path, "-wal"))?;
    remove_if_exists(&sidecar(path, "-shm"))?;
    // Re-create through the standard open path (migrations + owner-only perms).
    let store = crate::migrate::open_migrated(path, &default_migrations())?;
    // The epoch row is written exactly once per projection lifetime (the file
    // was just recreated), so the append-only guards never see an UPDATE.
    store.with_conn(|conn| {
        conn.execute(
            "INSERT INTO projection_meta (key, value, schema_version)              VALUES ('rebuild_epoch', ?1, 1)",
            [prior_epoch.saturating_add(1)],
        )
    })?;
    Ok(store)
}

/// The prior projection's rebuild epoch, best-effort: any failure (missing
/// file, unreadable database, pre-epoch schema) reads as zero.
fn read_epoch_best_effort(path: &Path) -> i64 {
    if !path.exists() {
        return 0;
    }
    let Ok(store) = SqliteStore::open(path) else {
        return 0;
    };
    store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT value FROM projection_meta WHERE key = 'rebuild_epoch'",
                [],
                |row| row.get::<_, i64>(0),
            )
        })
        .unwrap_or(0)
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
    use crate::migrate::apply_migrations;
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

    /// The epoch read used by the tests below (through the seam's default).
    fn epoch_of(store: &SqliteStore) -> i64 {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(store.read_rebuild_epoch()).unwrap()
    }

    #[test]
    fn rebuild_bumps_the_epoch_and_absent_reads_zero() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");

        // A fresh (never-rebuilt) projection reads epoch 0 (ADR-0010: absent
        // reads as zero, so existing projections need no rebuild).
        let fresh = crate::migrate::open_migrated(&db, &default_migrations()).unwrap();
        assert_eq!(epoch_of(&fresh), 0);
        drop(fresh);

        // Each rebuild carries the prior epoch forward + 1.
        let first = rebuild(&db).unwrap();
        assert_eq!(epoch_of(&first), 1);
        drop(first);
        let second = rebuild(&db).unwrap();
        assert_eq!(epoch_of(&second), 2);
        drop(second);
    }

    #[test]
    fn rebuild_over_missing_or_corrupt_prior_reads_zero_then_bumps() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        // Missing prior → epoch 1.
        let s = rebuild(&db).unwrap();
        assert_eq!(epoch_of(&s), 1);
        drop(s);
        // Corrupt prior (not a database) → best-effort zero → epoch 1 again;
        // never a panic, never fake continuity.
        std::fs::write(&db, b"definitely not sqlite").unwrap();
        let s = rebuild(&db).unwrap();
        assert_eq!(epoch_of(&s), 1);
        drop(s);
    }

    #[test]
    fn pre_epoch_projection_reads_zero_through_the_seam() {
        // A projection with only the v1-v4 migrations (no projection_meta) —
        // the seam's epoch read degrades to 0, not an error.
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        let store = SqliteStore::open(&db).unwrap();
        let old: Vec<crate::migrate::Migration> = default_migrations()
            .into_iter()
            .filter(|m| m.version <= 4)
            .collect();
        apply_migrations(&store, &old).unwrap();
        assert_eq!(epoch_of(&store), 0);
    }
}
