//! The family's binary of record: which `rift` this family's hooks point at,
//! when that was recorded and by what act, with the records it superseded
//! behind it.
//!
//! Separate from the install because it is a fact with its own lifetime: it is
//! read before an install decides anything, written after one succeeds, and
//! read again by `rift doctor` long after. The atomic write lives here too —
//! nothing else in the module writes a file a concurrent reader may be halfway
//! through.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};
use serde::{Deserialize, Serialize};

use super::is_executable_file;

/// `binary.json`, beside the family manifest.
const BINARY_FILE: &str = "binary.json";

/// How many superseded records `binary.json` keeps. Old enough to answer "what
/// was it pointing at before?" through a week of builds, small enough that the
/// file a hook may one day read stays one page.
const HISTORY_CAP: usize = 16;

/// The record's `schema_version`. A file carrying any other value is not read.
const BINARY_SCHEMA_VERSION: u32 = 1;

/// Which act recorded a binary of record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecordedBy {
    /// `rift init` (and Project Settings, which is the same provisioning act
    /// through a different surface).
    Init,
    /// An explicit `rift hooks install`.
    HooksInstall,
    /// A [`HookInstallPolicy::KeepIfHealthy`](super::HookInstallPolicy::KeepIfHealthy)
    /// install that found a dangling
    /// link or a vanished binary and repaired it.
    SelfHeal,
    /// An explicit `rift hooks install --repoint`.
    Repoint,
}

impl RecordedBy {
    /// The wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::HooksInstall => "hooks-install",
            Self::SelfHeal => "self-heal",
            Self::Repoint => "repoint",
        }
    }
}

impl core::fmt::Display for RecordedBy {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One superseded binary of record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryRecord {
    /// The absolute path that was of record.
    pub rift_bin: String,
    /// When it was recorded (epoch milliseconds). Audit context only — nothing
    /// reads it back, exactly like `family.history.jsonl`'s `at_ms`.
    pub recorded_at_ms: i64,
    /// The act that recorded it.
    pub recorded_by: RecordedBy,
}

/// The family's binary of record: `<family_dir>/binary.json`.
///
/// Machine-local, never tracked, never synced — it names a path on **this**
/// machine. Written with tmp+rename so a reader sees either the old record or
/// the new one, and never half of either.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryOfRecord {
    /// The record's schema version — **1**, and a file carrying any other
    /// value is not read.
    pub schema_version: u32,
    /// The absolute path the family's hooks point at.
    pub rift_bin: String,
    /// When it was recorded (epoch milliseconds).
    pub recorded_at_ms: i64,
    /// The act that recorded it.
    pub recorded_by: RecordedBy,
    /// Previous records, oldest first; the oldest are dropped past 16.
    #[serde(default)]
    pub history: Vec<BinaryRecord>,
}

impl BinaryOfRecord {
    /// The recorded path.
    #[must_use]
    pub fn path(&self) -> &Path {
        Path::new(&self.rift_bin)
    }

    /// Whether the recorded binary is still an executable file.
    #[must_use]
    pub fn is_live(&self) -> bool {
        is_executable_file(self.path())
    }

    fn superseded(&self) -> BinaryRecord {
        BinaryRecord {
            rift_bin: self.rift_bin.clone(),
            recorded_at_ms: self.recorded_at_ms,
            recorded_by: self.recorded_by,
        }
    }
}

/// Read a family's binary of record.
///
/// `None` covers every "there is nothing to honour here": no file, an
/// unreadable one, a shape this version does not know. A garbled record is not
/// an install failure — the hooks matter more than the note about them — so the
/// next record replaces it, and `rift doctor`'s `binary-of-record` leg is where
/// the file is named.
#[must_use]
pub fn read_binary_of_record(family_dir: &Path) -> Option<BinaryOfRecord> {
    let bytes = std::fs::read(family_dir.join(BINARY_FILE)).ok()?;
    let record: BinaryOfRecord = serde_json::from_slice(&bytes).ok()?;
    (record.schema_version == BINARY_SCHEMA_VERSION).then_some(record)
}

/// The path `binary.json` is read from and written to.
#[must_use]
pub fn binary_of_record_path(family_dir: &Path) -> PathBuf {
    family_dir.join(BINARY_FILE)
}

/// Record `rift_bin` as the family's binary of record, pushing the record it
/// supersedes onto the history.
///
/// Idempotent by construction: recording the path that is already of record
/// changes nothing and grows no history, so a `rift init` run twice leaves one
/// record, not two.
///
/// # Errors
/// [`RiftError::Io`] on a filesystem failure, [`RiftError::Validity`] if the
/// record cannot be serialized.
pub fn record_binary_of_record(
    family_dir: &Path,
    rift_bin: &Path,
    recorded_by: RecordedBy,
) -> Result<BinaryOfRecord> {
    let previous = read_binary_of_record(family_dir);
    let rift_bin = rift_bin.to_string_lossy().into_owned();
    if let Some(current) = previous.as_ref().filter(|r| r.rift_bin == rift_bin) {
        return Ok(current.clone());
    }
    let mut history = previous
        .as_ref()
        .map(|r| r.history.clone())
        .unwrap_or_default();
    if let Some(current) = previous.as_ref() {
        history.push(current.superseded());
    }
    // Oldest first, oldest dropped: the cap bounds the file, and what it drops
    // is the record furthest from anything anyone is still debugging.
    if history.len() > HISTORY_CAP {
        history.drain(..history.len() - HISTORY_CAP);
    }
    let record = BinaryOfRecord {
        schema_version: BINARY_SCHEMA_VERSION,
        rift_bin,
        recorded_at_ms: rift_core::Timestamp::now().as_millis(),
        recorded_by,
        history,
    };
    let bytes = serde_json::to_vec_pretty(&record)
        .map_err(|e| RiftError::validity(format!("serialize binary of record: {e}")))?;
    std::fs::create_dir_all(family_dir)?;
    write_atomic(&binary_of_record_path(family_dir), &bytes)?;
    Ok(record)
}

/// `binary.json.tmp-<pid>-<n>`, in the destination directory so the rename is
/// atomic, with a per-process counter so two threads cannot share a name.
fn tmp_name() -> String {
    use core::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    format!(
        "{BINARY_FILE}.tmp-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// Write, fsync, rename — so a concurrent reader sees one whole record.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let Some(dir) = path.parent() else {
        return Err(RiftError::validity("binary of record has no directory"));
    };
    let tmp = dir.join(tmp_name());
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&tmp);
        return Err(RiftError::from(e));
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(RiftError::from(e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_json_history_is_capped() {
        let tmp = tempfile::tempdir().unwrap();
        let family = tmp.path().join("rift-family");
        for i in 0..20 {
            record_binary_of_record(
                &family,
                &PathBuf::from(format!("/opt/rift-{i}")),
                RecordedBy::Repoint,
            )
            .unwrap();
        }
        let record = read_binary_of_record(&family).unwrap();
        assert_eq!(record.rift_bin, "/opt/rift-19");
        assert_eq!(record.history.len(), 16, "capped at 16");
        assert_eq!(
            record.history.first().map(|r| r.rift_bin.as_str()),
            Some("/opt/rift-3"),
            "the oldest are the ones dropped"
        );
        assert_eq!(
            record.history.last().map(|r| r.rift_bin.as_str()),
            Some("/opt/rift-18"),
        );
        // No temporary file survives a completed write.
        let strays: Vec<_> = std::fs::read_dir(&family)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "binary.json")
            .collect();
        assert!(
            strays.is_empty(),
            "tmp+rename leaves nothing behind: {strays:?}"
        );
    }

    #[test]
    fn a_garbled_record_is_not_honoured_and_is_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        let family = tmp.path().join("rift-family");
        std::fs::create_dir_all(&family).unwrap();
        std::fs::write(binary_of_record_path(&family), b"{not json").unwrap();
        assert!(read_binary_of_record(&family).is_none());
        record_binary_of_record(&family, Path::new("/opt/rift"), RecordedBy::Init).unwrap();
        assert_eq!(
            read_binary_of_record(&family).unwrap().rift_bin,
            "/opt/rift"
        );

        // A shape from a future schema is not guessed at either.
        std::fs::write(
            binary_of_record_path(&family),
            br#"{"schema_version":2,"rift_bin":"/opt/x","recorded_at_ms":0,"recorded_by":"init"}"#,
        )
        .unwrap();
        assert!(read_binary_of_record(&family).is_none());
    }
}
