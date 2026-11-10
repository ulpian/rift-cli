//! A cheap stat fingerprint of a projection: whether it can have changed,
//! answered without opening it.
//!
//! Two callers ask the same question on a hot path — the prompt hook ("has
//! recall moved since this session last accounted for it?") and Canopy's
//! watermark poller ("has this project's capture moved since the last
//! tick?") — so the answer lives here, beside the store that writes the
//! files, rather than once per caller.
//!
//! Failure direction: only an exact match may skip work. A stamp that
//! differs for any reason — including a file that came or went — sends the
//! caller to the full read, so the worst a stamp can do is cost one read.

use std::path::{Path, PathBuf};

/// `<db>,<db-wal>`, each as [`file_stamp`].
///
/// The `-wal` file is part of the stamp, and must be: writable projections
/// run in `journal_mode=WAL`, where a commit lands in the log and leaves the
/// main file untouched until the next checkpoint. The `-shm` index is not:
/// it changes on every read and says nothing about content.
#[must_use]
pub fn projection_file_stamp(db: &Path) -> String {
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    format!("{},{}", file_stamp(db), file_stamp(&PathBuf::from(wal)))
}

/// `<len>:<mtime-nanos>` for one file, or `-` when it is not there.
#[must_use]
pub fn file_stamp(path: &Path) -> String {
    let Ok(md) = std::fs::metadata(path) else {
        return "-".to_owned();
    };
    let nanos = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{}:{nanos}", md.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CanonicalStore as _, Value};

    #[test]
    fn a_missing_projection_stamps_as_absent_in_both_halves() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            projection_file_stamp(&dir.path().join("projection.db")),
            "-,-"
        );
    }

    #[test]
    fn a_commit_that_lands_only_in_the_log_moves_the_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("projection.db");
        let store = crate::open_capture_projection(&db).unwrap();
        store
            .with_conn(|c| c.execute_batch("PRAGMA wal_autocheckpoint=0"))
            .unwrap();
        let main = file_stamp(&db);
        let before = projection_file_stamp(&db);
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(store.insert(
                "projection_meta",
                &["key", "value", "schema_version"],
                &[Value::text("stamp_probe"), Value::Int(1), Value::Int(1)],
            ))
            .unwrap();
        assert_eq!(file_stamp(&db), main, "the main file did not move");
        assert_ne!(projection_file_stamp(&db), before, "the log did");
    }
}
