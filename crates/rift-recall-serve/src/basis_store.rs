//! The family's read-time basis snapshots: the bytes of a revision a window
//! was handed, kept so a delta against it can still be computed after the
//! working file has moved on.
//!
//! ## Why the store exists
//!
//! The delta frame and `changed-hunks` answer one question — *what changed in
//! this file since the bytes you hold* — and both need those bytes. Until now
//! the only route to them was the `content_sha256 → blob_oid` bridge and the
//! git object store, so a revision that was never committed had no basis at
//! all. That is exactly the revision an agent is most likely to be holding: it
//! read the file, then it (or the developer) edited it, and the next read of
//! it ran at full price with `(changed)` and no lines
//! (`docs/guide/18-known-gaps.md` §18.6).
//!
//! So the bytes of a revision a read recorded are kept here, and the basis
//! ladder becomes: the git blob, then this snapshot, then `basis_unavailable`.
//!
//! ## Why it is safe to keep them
//!
//! **Only redacted bytes are durable.** The writer takes a
//! [`rift_core::RedactedBytes`], whose sole constructor lives in
//! `rift-shadow-redact::redact` — a snapshot that had not been through the
//! gate cannot be passed to [`BasisStore::put`] at all, and the compiler says
//! so. That is the same rule a checkpoint shard lives by
//! (`docs/guide/04-redaction-and-secrets.md`), and the reason the sidecar
//! store next door could skip it does **not** hold here: a converted document
//! is a re-encoding of a file the checkout still has, while a snapshot is
//! bytes that no longer exist anywhere else, kept precisely because the
//! working file has moved on.
//!
//! **What redaction did is recorded beside the bytes.** A mask is a difference
//! Rift wrote, not one the repository made, so a diff that cannot tell the two
//! apart would report a changed place that never changed — and, on the verb
//! that serves lines, would serve the *live* bytes of a span the gate masked
//! in the basis. The meta file carries the runs of lines the mask touched, the
//! redaction policy's fingerprint and the rule-pack versions that produced
//! them, and [`crate::held_diff`] refuses to name a place that rests on one.
//!
//! **A mask that moves a line number is not a snapshot.** A multi-line secret
//! collapses to a single placeholder, and a basis whose line numbering is not
//! the revision's numbering can only produce wrong line numbers. The writer's
//! caller compares the two counts and records nothing when they differ; the
//! reader enforces the same rule against the file it reads, so neither half
//! trusts the other.
//!
//! ## The layout
//!
//! ```text
//! <common_dir>/rift-family/basis/<content_sha256>.bin    the redacted bytes
//! <common_dir>/rift-family/basis/<content_sha256>.json   what was masked, and under what policy
//! ```
//!
//! Keyed by the **read's own identity** — the SHA-256 of the bytes as they
//! were read — because that is the address every held revision travels under:
//! the read ledger's hold lines, the manifest's `@sha256:` entries, the pull
//! verbs' `--known-hash`. One family, one copy: a linked worktree is a member
//! of its repository's family, and a snapshot keyed by a checkout path would
//! be re-taken in every one of them and lost with the first temporary
//! worktree removed (`docs/guide/09-worktrees-and-families.md`).
//!
//! Nothing here opens a store, spawns a process or initialises a tokenizer.
//! The writer is the detached worker's; a serving path only ever reads.
//!
//! [`BasisStore::put`]: crate::basis_store::BasisStore::put

use std::path::{Path, PathBuf};

use rift_core::{RedactedBytes, Result, RiftError};

/// The family's own directory inside the git common dir — the same one the
/// family manifest and the document sidecars live in.
const FAMILY_DIR: &str = "rift-family";

/// The snapshot subdirectory under it.
const BASIS_DIR: &str = "basis";

/// The extension the redacted bytes are written under.
const BYTES_EXT: &str = "bin";

/// The extension the meta document is written under.
const META_EXT: &str = "json";

/// The contract version of the meta document. A snapshot written under a
/// version this build does not know is **absent**, never guessed at.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// The largest revision snapshotted at all: the gate path's own inline-hash
/// ceiling (`[interception] max_inline_hash_bytes`, 1 MiB), which is also
/// [`crate::held_diff::MAX_DIFF_BYTES`]. Past it nothing is diffed anyway, so
/// a snapshot would be disk spent on an answer no caller can be given.
pub const MAX_SNAPSHOT_BYTES: usize = 1024 * 1024;

/// What was masked in one snapshot, and under what policy.
///
/// Counts, line numbers, hashes and version strings — never a byte of what was
/// masked, and never a byte of what was not.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SnapshotMeta {
    /// The contract version this document was written under.
    pub schema_version: u32,
    /// SHA-256 (hex) of the bytes **as they were read** — this snapshot's key,
    /// and the address every held revision travels under.
    pub content_sha256: String,
    /// The repository-relative path the read named, in the `/` spelling the
    /// read ledger keys by. A content address is half a name, so the reader
    /// binds both (`held_diff::basis_bytes_for_path` gives the reasoning).
    pub path: String,
    /// SHA-256 (hex) of the redacted bytes actually on disk — what a reader
    /// checks the file it read against.
    pub redacted_sha256: String,
    /// The redaction policy's fingerprint at the moment of the seal, recorded
    /// beside the bytes exactly as a checkpoint shard records it.
    pub policy_fingerprint: String,
    /// The rule packs that cleared the bytes, `name@version`.
    pub rule_packs: Vec<String>,
    /// How many lines the revision has. The reader refuses a file that no
    /// longer splits into this many: a basis whose numbering is not the
    /// revision's numbering can only produce wrong line numbers.
    pub line_count: u32,
    /// The runs of lines the mask touched, 1-based and inclusive at both ends,
    /// ordered and non-overlapping. Empty means the gate changed nothing, and
    /// the snapshot is the revision byte for byte.
    pub redacted_lines: Vec<(u32, u32)>,
    /// When the snapshot was taken (epoch ms, the job's own clock).
    pub captured_at_ms: i64,
}

/// One snapshot as a reader gets it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The redacted bytes of the revision.
    pub bytes: Vec<u8>,
    /// What the mask touched, from [`SnapshotMeta::redacted_lines`].
    pub redacted_lines: Vec<(u32, u32)>,
    /// The path the revision was read at.
    pub path: String,
    /// The policy the mask ran under.
    pub policy_fingerprint: String,
}

/// The family's snapshots, as one member reaches them.
///
/// One path and no state: every operation is a filesystem operation, so a
/// crash leaves either a whole snapshot or none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BasisStore {
    dir: PathBuf,
}

impl BasisStore {
    /// The store for the family whose git common directory is `common_dir`.
    ///
    /// The common dir is the family's — shared by every member — so two
    /// checkouts of one repository name the same file and a member removed
    /// tomorrow still contributes to it today.
    #[must_use]
    pub fn for_family(common_dir: &Path) -> Self {
        Self {
            dir: common_dir.join(FAMILY_DIR).join(BASIS_DIR),
        }
    }

    /// The directory itself, for the sweep and the doctor's count.
    #[must_use]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Where `content_sha256`'s bytes live.
    #[must_use]
    pub fn bytes_path(&self, content_sha256: &str) -> PathBuf {
        self.dir.join(format!("{content_sha256}.{BYTES_EXT}"))
    }

    /// Where its meta document lives.
    #[must_use]
    pub fn meta_path(&self, content_sha256: &str) -> PathBuf {
        self.dir.join(format!("{content_sha256}.{META_EXT}"))
    }

    /// Whether this revision is already held — one `stat`, no read and no
    /// write. The whole of what a caller with a budget may ask.
    #[must_use]
    pub fn holds(&self, content_sha256: &str) -> bool {
        is_sha256(content_sha256) && self.bytes_path(content_sha256).is_file()
    }

    /// Record one revision's redacted bytes and what the mask did to them.
    ///
    /// The bytes argument is a [`RedactedBytes`], which is the whole of the
    /// safety argument: its only constructor is the redaction gate's, so
    /// nothing that has not been through the gate can be handed to this
    /// function.
    ///
    /// Written meta-last, temp-file-then-rename, so a reader finds either both
    /// halves or neither, and a snapshot already held is not rewritten — the
    /// name is the hash of what this would have described.
    ///
    /// # Errors
    /// [`RiftError::Validity`] when the key is not a hash, when the meta
    /// disagrees with the bytes, or when the mask moved a line number;
    /// [`RiftError::Io`] when the directory cannot be written.
    pub fn put(&self, bytes: &RedactedBytes, meta: &SnapshotMeta) -> Result<()> {
        if !is_sha256(&meta.content_sha256) {
            return Err(RiftError::validity(
                "basis snapshot: the key is not a content address",
            ));
        }
        if meta.schema_version != SNAPSHOT_SCHEMA_VERSION {
            return Err(RiftError::validity(
                "basis snapshot: refusing to write a contract version this build does not read",
            ));
        }
        if meta.redacted_sha256 != bytes.redacted_sha256().to_hex() {
            return Err(RiftError::validity(
                "basis snapshot: the meta names bytes other than the ones sealed",
            ));
        }
        if bytes.as_slice().len() > MAX_SNAPSHOT_BYTES {
            return Err(RiftError::validity(
                "basis snapshot: past the inline-hash ceiling, which nothing diffs anyway",
            ));
        }
        if line_count(bytes.as_slice()) != meta.line_count {
            return Err(RiftError::validity(
                "basis snapshot: the mask moved a line number, so this is not the revision's \
                 numbering",
            ));
        }
        if self.bytes_path(&meta.content_sha256).is_file()
            && self.meta_path(&meta.content_sha256).is_file()
        {
            return Ok(());
        }
        std::fs::create_dir_all(&self.dir)?;
        // Named for the writing process, so two workers snapshotting one
        // revision at the same instant cannot tear each other's temp file.
        // Both then rename onto the same names with the same bytes.
        let pid = std::process::id();
        let body = serde_json::to_vec(meta)
            .map_err(|e| RiftError::validity(format!("basis snapshot: encode: {e}")))?;
        let tmp_bytes = self
            .dir
            .join(format!("{}.{BYTES_EXT}.tmp{pid}", meta.content_sha256));
        let tmp_meta = self
            .dir
            .join(format!("{}.{META_EXT}.tmp{pid}", meta.content_sha256));
        std::fs::write(&tmp_bytes, bytes.as_slice())?;
        restrict(&tmp_bytes);
        std::fs::write(&tmp_meta, &body)?;
        restrict(&tmp_meta);
        // Bytes first, meta second: the meta is what a reader looks for, so a
        // process that dies between the two leaves a file no reader reaches
        // rather than a promise nothing backs.
        std::fs::rename(&tmp_bytes, self.bytes_path(&meta.content_sha256))?;
        std::fs::rename(&tmp_meta, self.meta_path(&meta.content_sha256))?;
        Ok(())
    }

    /// The snapshot of `content_sha256` **as `path`**, or `None`.
    ///
    /// Every step can honestly fail and every failure is `None`: a revision
    /// nobody snapshotted, a meta this build does not read, a torn write, a
    /// file whose bytes are not the ones the meta describes, a mask whose line
    /// count no longer matches, or a snapshot recorded against another file.
    /// The path is part of the question for the reason
    /// [`crate::held_diff::basis_bytes_for_path`] gives: a content address is
    /// half a name, and a caller that chose the hash must not be able to pair
    /// it with a path it merely has permission to read.
    #[must_use]
    pub fn get_for_path(&self, path: &str, content_sha256: &str) -> Option<Snapshot> {
        let meta = self.meta(content_sha256)?;
        if meta.path != path {
            return None;
        }
        self.load(meta)
    }

    /// The snapshot of `content_sha256`, whatever file it was read at.
    ///
    /// The unbound question, for the callers that ask it about a revision the
    /// *window's own record* named — a manifest entry and a recovery-map line
    /// carry the path and the hash together from one row, so the pairing was
    /// never the caller's to choose.
    #[must_use]
    pub fn get(&self, content_sha256: &str) -> Option<Snapshot> {
        let meta = self.meta(content_sha256)?;
        self.load(meta)
    }

    /// The meta document, when this build can read it.
    fn meta(&self, content_sha256: &str) -> Option<SnapshotMeta> {
        if !is_sha256(content_sha256) {
            return None;
        }
        let body = std::fs::read(self.meta_path(content_sha256)).ok()?;
        let meta: SnapshotMeta = serde_json::from_slice(&body).ok()?;
        (meta.schema_version == SNAPSHOT_SCHEMA_VERSION
            && meta.content_sha256 == content_sha256
            && ranges_are_ordered(&meta.redacted_lines))
        .then_some(meta)
    }

    /// The bytes behind a meta this build accepted, checked against it.
    fn load(&self, meta: SnapshotMeta) -> Option<Snapshot> {
        let bytes = std::fs::read(self.bytes_path(&meta.content_sha256)).ok()?;
        if bytes.len() > MAX_SNAPSHOT_BYTES || line_count(&bytes) != meta.line_count {
            return None;
        }
        Some(Snapshot {
            bytes,
            redacted_lines: meta.redacted_lines,
            path: meta.path,
            policy_fingerprint: meta.policy_fingerprint,
        })
    }

    /// How many snapshots the family holds and what they occupy — the
    /// doctor's two numbers.
    #[must_use]
    pub fn survey(&self) -> Survey {
        let mut survey = Survey::default();
        for (_, path) in snapshots(&self.dir) {
            survey.snapshots += 1;
            survey.bytes += std::fs::metadata(&path).map_or(0, |m| m.len());
        }
        survey
    }

    /// Collect what has expired, and say how much.
    ///
    /// A snapshot untouched for longer than the TTL goes, with its meta.
    /// Losing one costs a delta frame and never an answer: the file is still
    /// in the checkout, the blob route is untouched, and the next read of the
    /// revision takes a new snapshot.
    ///
    /// Total and quiet, like every other sweep in this tool: a name this
    /// module did not write is left alone however it got there, a file whose
    /// age cannot be read is kept, and a removal that fails is not counted.
    #[must_use]
    pub fn sweep(&self, now_ms: i64, ttl_ms: i64, dry_run: bool) -> SweepReport {
        let cutoff = now_ms.saturating_sub(ttl_ms);
        let mut report = SweepReport::default();
        for (key, path) in snapshots(&self.dir) {
            if !older_than(&path, cutoff) {
                continue;
            }
            report.expired += 1;
            if !dry_run {
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::remove_file(self.meta_path(&key));
            }
        }
        report.swept_tmps = sweep_tmps(&self.dir, cutoff, dry_run);
        // A meta with no bytes behind it is the remains of a torn write or of
        // an expired snapshot, and nothing will ever refresh it.
        for (key, path) in metas(&self.dir) {
            if self.bytes_path(&key).is_file() {
                continue;
            }
            report.orphan_metas += 1;
            if !dry_run {
                let _ = std::fs::remove_file(&path);
            }
        }
        report
    }
}

/// What the family holds, for `rift doctor`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Survey {
    /// Snapshots in the family store.
    pub snapshots: usize,
    /// Bytes they occupy.
    pub bytes: u64,
}

/// What one sweep removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// Snapshots past the TTL.
    pub expired: usize,
    /// Meta documents with no bytes behind them.
    pub orphan_metas: usize,
    /// Temp files left by a write that died before its rename.
    pub swept_tmps: usize,
}

/// How many lines these bytes split into, by the same rule the diff engine
/// uses (`byte_lines_with_terminator`): a trailing newline does not open a
/// line of its own, and empty bytes are no lines at all.
#[must_use]
pub fn line_count(bytes: &[u8]) -> u32 {
    u32::try_from(rift_recall_ledger::hunks::split_lines(bytes).len()).unwrap_or(u32::MAX)
}

/// Whether `runs` is ordered, non-overlapping and one-based — the shape a
/// reader is willing to believe about a list it did not compute.
fn ranges_are_ordered(runs: &[(u32, u32)]) -> bool {
    let mut last = 0u32;
    for &(lo, hi) in runs {
        if lo == 0 || lo > hi || lo <= last {
            return false;
        }
        last = hi;
    }
    true
}

/// The `(key, path)` of every snapshot in `dir` — and only the snapshots: a
/// name this module did not mint is not one, whatever its extension.
fn snapshots(dir: &Path) -> Vec<(String, PathBuf)> {
    named(dir, BYTES_EXT)
}

/// The same, for the meta documents.
fn metas(dir: &Path) -> Vec<(String, PathBuf)> {
    named(dir, META_EXT)
}

fn named(dir: &Path, ext: &str) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?;
            let key = name.strip_suffix(&format!(".{ext}"))?;
            is_sha256(key).then(|| (key.to_owned(), path.clone()))
        })
        .collect()
}

/// Whether `path` was last modified before `cutoff` (epoch ms). A time the
/// filesystem will not report is **not** old.
fn older_than(path: &Path, cutoff_ms: i64) -> bool {
    let Ok(modified) = std::fs::metadata(path).and_then(|m| m.modified()) else {
        return false;
    };
    let Ok(since) = modified.duration_since(std::time::UNIX_EPOCH) else {
        return false;
    };
    i64::try_from(since.as_millis()).unwrap_or(i64::MAX) < cutoff_ms
}

/// Remove the temp files of writes that died before their rename.
fn sweep_tmps(dir: &Path, cutoff_ms: i64, dry_run: bool) -> usize {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        let is_tmp = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.contains(&format!(".{BYTES_EXT}.tmp"))
                    || name.contains(&format!(".{META_EXT}.tmp"))
            });
        if !is_tmp || !older_than(&path, cutoff_ms) {
            continue;
        }
        removed += 1;
        if !dry_run {
            let _ = std::fs::remove_file(&path);
        }
    }
    removed
}

/// Whether `s` is 64 lowercase hex characters — the one shape that becomes a
/// path component here.
fn is_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Owner-only permissions on a snapshot (unix): it is one revision of a file
/// in the checkout, and it inherits that file's audience rather than the
/// umask's.
fn restrict(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A sealed buffer, minted the one way a buffer is ever minted.
    fn through_the_gate(bytes: &[u8]) -> RedactedBytes {
        rift_shadow_redact::redact(&rift_core::SecretBytes::new(bytes.to_vec()))
            .expect("the gate clears ordinary text")
            .redacted
    }

    fn meta(key: &str, path: &str, bytes: &[u8], runs: &[(u32, u32)]) -> SnapshotMeta {
        SnapshotMeta {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            content_sha256: key.to_owned(),
            path: path.to_owned(),
            redacted_sha256: rift_recall_substrate::worktree::sha256_hex(bytes),
            policy_fingerprint: "b3:0000".to_owned(),
            rule_packs: vec!["builtin@1".to_owned()],
            line_count: line_count(bytes),
            redacted_lines: runs.to_vec(),
            captured_at_ms: 1_700_000_000_000,
        }
    }

    /// The whole round trip, and the two halves of the address: a snapshot
    /// comes back for the file it was recorded at and for no other.
    #[test]
    fn a_snapshot_comes_back_under_its_own_path_and_no_other() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BasisStore::for_family(tmp.path());
        let text = b"alpha\nbravo\ncharlie\n";
        let sealed = through_the_gate(text);
        let key = rift_recall_substrate::worktree::sha256_hex(text);
        assert!(!store.holds(&key));
        store
            .put(&sealed, &meta(&key, "src/a.rs", sealed.as_slice(), &[]))
            .unwrap();
        assert!(store.holds(&key));
        let got = store.get_for_path("src/a.rs", &key).unwrap();
        assert_eq!(got.bytes, text.to_vec());
        assert!(got.redacted_lines.is_empty());
        assert_eq!(
            store.get_for_path("src/other.rs", &key),
            None,
            "a revision recorded for one file is not another file's basis"
        );
    }

    /// Writing the same revision twice is one snapshot, not a rewrite: the
    /// name is the hash of what the second write would have said.
    #[test]
    fn a_revision_snapshotted_twice_is_one_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BasisStore::for_family(tmp.path());
        let text = b"one\ntwo\n";
        let sealed = through_the_gate(text);
        let key = rift_recall_substrate::worktree::sha256_hex(text);
        let m = meta(&key, "a.rs", sealed.as_slice(), &[]);
        store.put(&sealed, &m).unwrap();
        store.put(&sealed, &m).unwrap();
        assert_eq!(store.survey().snapshots, 1);
    }

    /// A key that is not a content address never becomes a path component,
    /// and neither does an unordered range list become a fact a reader
    /// believes.
    #[test]
    fn nothing_that_is_not_a_content_address_is_written_or_read() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BasisStore::for_family(tmp.path());
        let sealed = through_the_gate(b"x\n");
        let mut m = meta(&"ab".repeat(32), "a.rs", sealed.as_slice(), &[]);
        m.content_sha256 = "../evil".to_owned();
        assert!(store.put(&sealed, &m).is_err());
        assert_eq!(store.survey().snapshots, 0);

        let key = rift_recall_substrate::worktree::sha256_hex(b"x\n");
        let overlapping = meta(&key, "a.rs", sealed.as_slice(), &[(1, 3), (2, 4)]);
        store.put(&sealed, &overlapping).unwrap();
        assert_eq!(
            store.get(&key),
            None,
            "an overlapping range list is a document this reader does not believe"
        );
    }

    /// A meta whose line count is not the file's is a torn or edited pair, and
    /// the answer is absence rather than a set of line numbers about the wrong
    /// bytes.
    #[test]
    fn a_meta_that_disagrees_with_its_bytes_is_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BasisStore::for_family(tmp.path());
        let text = b"a\nb\nc\n";
        let sealed = through_the_gate(text);
        let key = rift_recall_substrate::worktree::sha256_hex(text);
        store
            .put(&sealed, &meta(&key, "a.rs", sealed.as_slice(), &[]))
            .unwrap();
        // Somebody truncated the bytes behind an intact meta.
        std::fs::write(store.bytes_path(&key), b"a\n").unwrap();
        assert_eq!(store.get(&key), None);
    }

    /// A mask that moved a line number is refused at the write: a basis whose
    /// numbering is not the revision's can only produce wrong line numbers.
    #[test]
    fn a_mask_that_moved_a_line_number_is_never_written() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BasisStore::for_family(tmp.path());
        let sealed = through_the_gate(b"a\nb\n");
        let key = rift_recall_substrate::worktree::sha256_hex(b"a\nb\nc\n");
        let mut m = meta(&key, "a.rs", sealed.as_slice(), &[]);
        m.line_count = 3; // the raw revision had three lines; the seal has two
        assert!(store.put(&sealed, &m).is_err());
    }

    /// The sweep collects by age, takes the meta with the bytes, and removes a
    /// meta nothing backs whatever its age.
    #[test]
    fn the_sweep_collects_an_expired_pair_and_an_orphan_meta() {
        let tmp = tempfile::tempdir().unwrap();
        let store = BasisStore::for_family(tmp.path());
        let text = b"a\nb\n";
        let sealed = through_the_gate(text);
        let key = rift_recall_substrate::worktree::sha256_hex(text);
        store
            .put(&sealed, &meta(&key, "a.rs", sealed.as_slice(), &[]))
            .unwrap();
        // Nothing is old yet: a TTL measured from an instant before the write.
        assert_eq!(store.sweep(0, 0, false), SweepReport::default());
        // Far enough in the future that everything is past the TTL.
        let report = store.sweep(i64::MAX / 2, 0, false);
        assert_eq!(report.expired, 1);
        assert_eq!(store.survey().snapshots, 0);
        assert!(!store.meta_path(&key).is_file(), "the meta went with it");

        // A meta with nothing behind it goes whatever its age.
        std::fs::write(store.meta_path(&key), b"{}").unwrap();
        assert_eq!(store.sweep(0, i64::MAX / 2, false).orphan_metas, 1);
    }

    /// The store is the family's, reached from the common directory every
    /// member shares — never from the checkout that happened to be open.
    #[test]
    fn the_store_is_the_familys_and_not_a_checkouts() {
        let store = BasisStore::for_family(Path::new("/repo/.git"));
        assert_eq!(store.dir(), Path::new("/repo/.git/rift-family/basis"));
    }
}
