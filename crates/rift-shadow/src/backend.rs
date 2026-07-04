//! The strangler-fig seam for the Rust rewrite path (P7 S2).
//!
//! All of `rift-shadow`'s write-side capability is expressed as one trait,
//! [`ShadowBackend`], so callers (condense/executor) are **backend-agnostic**.
//! Both the wrapper ([`EntireBackend`]) and the native rewrite ([`NativeBackend`])
//! implement it identically; [`RoutedBackend`] routes **each capability**
//! individually (`Wrapper | DualRun | Native`, default `Wrapper`) so the rewrite
//! proceeds subsystem-by-subsystem.
//!
//! Under `DualRun` both backends execute, the **wrapper's** (canonical) result is
//! returned, and any divergence is **recorded but not committed** — so a native bug
//! during validation can never corrupt the ledger. A global kill-switch forces
//! every capability back to the wrapper. Promotion `DualRun → Native` is gated on
//! fixture parity (P7 S1) plus zero field divergence over a soak (operational).
//!
//! In this codebase hashing/attribution/cost/redaction are *already* native (they
//! never had an Entire equivalent); only the **git write** (checkpoint archival)
//! is wrapped. So the routable capabilities here are the checkpoint root (BYTE) and
//! the archive (git write) — the rest is shared native code in both backends.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_core::{CanonicalEvent, RedactedBytes, Result, RiftError, Sha256Id};
use rift_git::{
    GitFileMode, GitIdent, MemoryGitWrite, NativeGitWrite, OrphanCommit, TreeWriteEntry,
};
use rift_shadow_entire::{CaptureIntent, EntireRunner, SubstrateError};
use rift_store::{
    record_verification_divergence, record_verification_run, CanonicalStore,
    VerificationDivergenceRow, VerificationRunRow,
};

use crate::sha256_hex;

/// The fully-qualified orphan checkpoint branch the native archive writes
/// (`Dialect::Rift.checkpoint_ref()` on the read side — the LAYOUT contract).
pub const NATIVE_CHECKPOINT_REF: &str = "refs/heads/rift/checkpoints/v1";

/// The redaction/canonicalization version the native archive stamps into every
/// `metadata.json` (`canon_version`). This is the P7 redaction→leaf migration
/// decision (story 18, ADR-0018): rather than reproduce Entire's exact redacted
/// bytes for pre-rewrite checkpoints, the canonicalization is **versioned**. A
/// native checkpoint carries `canon_version = 1` (Rift-native redaction + leaf
/// derivation); a pre-rewrite (Entire) checkpoint has no such field and is read as
/// `canon_version = 0`. `rift verify` selects the leaf canonicalization by the
/// checkpoint's version, so promoting native redaction never breaks `verify` on
/// old checkpoints — the two epochs coexist instead of one rewriting the other.
pub const NATIVE_CANON_VERSION: u32 = 1;

/// One archived canonical event, carrying exactly the fields a `full.jsonl` line
/// records (content addresses, never re-derived). Mirrors
/// [`rift_recall_types::checkpoint::FullEventRecord`] on the write side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedEvent {
    /// The agent's native session id.
    pub session: String,
    /// Turn ordinal within the session.
    pub turn_ordinal: u32,
    /// Event ordinal within the turn.
    pub event_ordinal: u32,
    /// The canonical event-kind token (`session.start`, …).
    pub kind: String,
    /// The native hook verb that produced the event (idempotency-key input).
    pub verb: String,
    /// The agent token (`claude-code` | …).
    pub agent: String,
    /// Transaction time (epoch ms).
    pub occurred_at_ms: i64,
    /// Content address of the redacted payload (hex).
    pub payload_sha256: String,
    /// The **already-redacted** payload, as it will be archived into `full.jsonl`.
    /// INVARIANT: this must have passed the redaction gate before it reaches here —
    /// `full.jsonl` is written verbatim via `write_structural_blob` (the entropy
    /// redactor would corrupt its hex `payload_sha256`), so a caller that populates
    /// `events` must redact each payload first. In v0.1 no per-turn stream is
    /// threaded (`events` is always empty), so no un-redacted byte can reach here yet.
    pub payload: serde_json::Value,
}

/// The content a checkpoint archive writes — everything both backends need. The
/// wrapper reads only `repo`/`commit` (it captures its own content); the native
/// archive writes the full shard from the structural fields + `content`.
///
/// The redaction gate is honored two ways: `content` is [`RedactedBytes`] (proof
/// the raw capture passed the gate — written durably via
/// [`NativeGitWrite::write_durable_blob`]), and the structural artifacts are
/// Rift-authored from counts/ids/content-addresses that never touched a secret.
///
/// No `Debug` derive: `content` is [`RedactedBytes`], which withholds `Debug` on
/// purpose so redacted bytes never reach a log line.
#[derive(Clone)]
pub struct CheckpointArchive {
    /// The repo root (the wrapper's cwd; validated hex commit for its argv).
    pub repo: PathBuf,
    /// The anchoring working-tree commit oid (hex).
    pub commit: String,
    /// The content-addressed checkpoint id (64-hex).
    pub checkpoint_id: String,
    /// The native session ids this checkpoint covers.
    pub sessions: Vec<String>,
    /// The archival strategy token (`manual-commit` | …).
    pub strategy: String,
    /// The prior checkpoint id in the chain (hex), `None` at genesis.
    pub prev_hash: Option<String>,
    /// The Merkle root over the checkpoint's ordered event leaves.
    pub merkle_root: Sha256Id,
    /// The archived canonical events (empty ⇒ no `full.jsonl`).
    pub events: Vec<ArchivedEvent>,
    /// The redacted raw capture (durable-blob proof of the gate).
    pub content: RedactedBytes,
    /// The redaction report for `redaction_report.json` (must be valid).
    pub report: RedactionReportView,
    /// Creation time (epoch ms) — the commit date is derived from it.
    pub created_at_ms: i64,
}

/// The counts-only redaction facts the native archive freezes into
/// `redaction_report.json`. A structural mirror of
/// [`rift_shadow_redact::RedactionReport`] carrying only what the strict bridge
/// gate needs — it is `is_valid()` iff a rule pack ran and the input address is set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedactionReportView {
    /// Total spans masked.
    pub total_spans: u32,
    /// Aggregate masked byte length.
    pub bytes_masked: u64,
    /// Spans deliberately kept via the allowlist.
    pub allowlisted: u32,
    /// Exactly which rule packs/versions ran.
    pub rule_pack_versions: Vec<String>,
    /// SHA-256 (hex) of the raw input.
    pub input_sha256: String,
}

impl RedactionReportView {
    /// Project a redaction-pipeline report into the frozen archive view.
    #[must_use]
    pub fn from_redact(report: &rift_shadow_redact::RedactionReport) -> Self {
        Self {
            total_spans: report.total_spans,
            bytes_masked: report.bytes_masked,
            allowlisted: report.allowlisted,
            rule_pack_versions: report.rule_pack_versions.clone(),
            input_sha256: report.input_sha256.to_hex(),
        }
    }
}

impl CheckpointArchive {
    /// The `<id[:2]>/<id[2:]>` shard path this checkpoint lands at (LAYOUT).
    #[must_use]
    pub fn shard_path(&self) -> String {
        let id = &self.checkpoint_id;
        match (id.get(..2), id.get(2..)) {
            (Some(hi), Some(rest)) => format!("{hi}/{rest}"),
            _ => id.clone(),
        }
    }

    /// The canonical `metadata.json` bytes (deterministic key order). Carries the
    /// `canon_version` epoch ([`NATIVE_CANON_VERSION`]) so the redaction→leaf
    /// migration is explicit: old (Entire) checkpoints lack it and read as epoch 0.
    #[must_use]
    pub fn metadata_json(&self) -> Vec<u8> {
        let mut obj = serde_json::Map::new();
        obj.insert("canon_version".into(), NATIVE_CANON_VERSION.into());
        obj.insert("strategy".into(), self.strategy.clone().into());
        obj.insert("merkle_root".into(), self.merkle_root.to_hex().into());
        if let Some(prev) = &self.prev_hash {
            obj.insert("prev_hash".into(), prev.clone().into());
        }
        obj.insert(
            "sessions".into(),
            serde_json::Value::from(self.sessions.clone()),
        );
        obj.insert("commit".into(), self.commit.clone().into());
        canonical_json(&serde_json::Value::Object(obj))
    }

    /// The canonical `redaction_report.json` bytes (valid for the strict bridge gate).
    #[must_use]
    pub fn redaction_report_json(&self) -> Vec<u8> {
        let r = &self.report;
        let obj = serde_json::json!({
            "total_spans": r.total_spans,
            "bytes_masked": r.bytes_masked,
            "allowlisted": r.allowlisted,
            "rule_pack_versions": r.rule_pack_versions,
            "input_sha256": r.input_sha256,
        });
        canonical_json(&obj)
    }

    /// The `full.jsonl` bytes (one canonical event per line), or `None` if empty.
    #[must_use]
    pub fn full_jsonl(&self) -> Option<Vec<u8>> {
        if self.events.is_empty() {
            return None;
        }
        let mut out = Vec::new();
        for (i, e) in self.events.iter().enumerate() {
            if i > 0 {
                out.push(b'\n');
            }
            let line = serde_json::json!({
                "session": e.session,
                "turn": e.turn_ordinal,
                "event": e.event_ordinal,
                "kind": e.kind,
                "verb": e.verb,
                "agent": e.agent,
                "occurred_at_ms": e.occurred_at_ms,
                "payload_sha256": e.payload_sha256,
                "payload": e.payload,
            });
            out.extend_from_slice(&canonical_json(&line));
        }
        Some(out)
    }

    /// The `content_hash.txt` bytes: the shard's content address over its ordered
    /// artifacts (metadata ‖ report ‖ full), lowercase hex + trailing newline.
    #[must_use]
    pub fn content_hash_txt(&self) -> Vec<u8> {
        let mut parts: Vec<Vec<u8>> = vec![self.metadata_json(), self.redaction_report_json()];
        if let Some(full) = self.full_jsonl() {
            parts.push(full);
        }
        let refs: Vec<&[u8]> = parts.iter().map(Vec::as_slice).collect();
        let mut hex = sha256_hex(&refs);
        hex.push('\n');
        hex.into_bytes()
    }
}

/// Serialize a JSON value to canonical bytes: `serde_json`'s default `Map` is a
/// `BTreeMap` (keys sorted), so this is deterministic byte-for-byte.
fn canonical_json(v: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

/// Class-preserving map of a seam error into `RiftError`, so callers (and the
/// CLI's exit contract) can distinguish an integrity refusal from a transient
/// failure from a policy block without string-sniffing.
fn map_archive_err(e: SubstrateError) -> RiftError {
    match e {
        SubstrateError::Integrity => {
            RiftError::Integrity("capture backend failed its pinned checksum".into())
        }
        SubstrateError::Unavailable | SubstrateError::Timeout => {
            RiftError::Substrate(format!("capture backend archive failed (transient): {e}"))
        }
        SubstrateError::Blocked => {
            RiftError::PolicyDenied("capture backend invocation refused (local-only)".into())
        }
        other => RiftError::storage(format!("capture backend archive failed: {other}")),
    }
}

/// One write-side capability that can be routed independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Capability {
    /// The checkpoint Merkle root (BYTE-critical; validated first).
    CheckpointRoot,
    /// Checkpoint archival (the git write orchestration).
    Archive,
}

impl Capability {
    /// The capability's token in `.rift/rewrite.toml` and divergence logs.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckpointRoot => "hashing",
            Self::Archive => "git_write",
        }
    }
}

/// The route for a capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Use the trusted wrapper (the safe default + rollback target).
    Wrapper,
    /// Run both, return the wrapper's result, record divergences (validating).
    DualRun,
    /// Use the promoted native implementation.
    Native,
}

impl Route {
    fn parse(s: &str) -> Option<Self> {
        match s.trim().trim_matches('"') {
            "wrapper" => Some(Self::Wrapper),
            "dual_run" => Some(Self::DualRun),
            "native" => Some(Self::Native),
            _ => None,
        }
    }
}

/// The per-capability rollout dial (machine-local `.rift/rewrite.toml`). Default
/// route is `Wrapper`; a global `kill_switch` forces every capability to `Wrapper`.
#[derive(Debug, Clone, Default)]
pub struct RouteTable {
    routes: BTreeMap<Capability, Route>,
    kill_switch: bool,
}

impl RouteTable {
    /// An all-wrapper table (behaviorally identical to today's wrapped path).
    #[must_use]
    pub fn all_wrapper() -> Self {
        Self::default()
    }

    /// Set a capability's route.
    #[must_use]
    pub fn with(mut self, cap: Capability, route: Route) -> Self {
        self.routes.insert(cap, route);
        self
    }

    /// Trip the global kill-switch (every capability reverts to the wrapper).
    pub fn trip_kill_switch(&mut self) {
        self.kill_switch = true;
    }

    /// The effective route for a capability (kill-switch forces `Wrapper`).
    #[must_use]
    pub fn route(&self, cap: Capability) -> Route {
        if self.kill_switch {
            return Route::Wrapper;
        }
        self.routes.get(&cap).copied().unwrap_or(Route::Wrapper)
    }

    /// Parse a `[routes]` table from `.rift/rewrite.toml` (minimal: `key = "route"`).
    /// Unknown keys/values are ignored (the default route is `Wrapper`).
    #[must_use]
    pub fn from_toml_str(s: &str) -> Self {
        let mut table = Self::default();
        for line in s.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with('[') {
                continue;
            }
            if let Some((key, val)) = line.split_once('=') {
                let cap = match key.trim() {
                    "hashing" => Some(Capability::CheckpointRoot),
                    "git_write" | "archive" => Some(Capability::Archive),
                    _ => None,
                };
                if let (Some(cap), Some(route)) = (cap, Route::parse(val)) {
                    table.routes.insert(cap, route);
                }
            }
        }
        table
    }

    /// Load the route table from `<rift_dir>/rewrite.toml` (e.g. `.rift/rewrite.toml`).
    /// A missing or unreadable file is the **secure default** ([`all_wrapper`](RouteTable::all_wrapper)) —
    /// promotion is opt-in, never inferred, and a config problem never advances a
    /// capability past the trusted wrapper. A `kill_switch = true` line trips the
    /// global kill-switch regardless of the per-capability routes.
    #[must_use]
    pub fn load(rift_dir: &Path) -> Self {
        let path = rift_dir.join("rewrite.toml");
        let Ok(src) = std::fs::read_to_string(&path) else {
            return Self::all_wrapper();
        };
        let mut table = Self::from_toml_str(&src);
        if src.lines().any(|l| {
            let l = l.trim();
            l.starts_with("kill_switch")
                && l.split_once('=')
                    .is_some_and(|(_, v)| v.trim().trim_matches('"') == "true")
        }) {
            table.trip_kill_switch();
        }
        table
    }
}

/// A recorded parity divergence (wrapper vs native) seen during `DualRun`. These
/// accumulate but are **never committed** to the ledger; they gate promotion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Divergence {
    /// The capability that diverged.
    pub capability: Capability,
    /// The wrapper's (canonical) result, rendered (hex / message).
    pub wrapper: String,
    /// The native result, rendered (hex / message).
    pub native: String,
}

/// The seam the strangler grows along. Both backends implement it identically.
pub trait ShadowBackend {
    /// Archive a checkpoint shard (the git-write orchestration). The wrapper reads
    /// only `repo`/`commit`; the native archive writes the full shard.
    ///
    /// # Errors
    /// [`RiftError`] on an archive failure.
    fn archive_checkpoint(&self, archive: &CheckpointArchive) -> Result<()>;

    /// The checkpoint Merkle root over its ordered event leaves (BYTE-critical).
    /// Both backends share the native implementation (no Entire equivalent), so
    /// this defaults to it.
    fn checkpoint_root(&self, events: &[CanonicalEvent]) -> Sha256Id {
        crate::checkpoint_merkle_root(events)
    }
}

/// The wrapper backend: archival shells out to the pinned capture binary via the
/// [`EntireRunner`] seam. Borrows the runner so callers/tests retain it.
pub struct EntireBackend<'a, R: EntireRunner> {
    runner: &'a R,
}

impl<'a, R: EntireRunner> EntireBackend<'a, R> {
    /// Wrap a runner.
    pub fn new(runner: &'a R) -> Self {
        Self { runner }
    }
}

impl<R: EntireRunner> ShadowBackend for EntireBackend<'_, R> {
    fn archive_checkpoint(&self, archive: &CheckpointArchive) -> Result<()> {
        // The wrapper captures its own shard content: it needs only the repo + the
        // anchoring commit, translated to its screened argv (invalid oids blocked).
        let intent = CaptureIntent::ArchiveCheckpoint {
            repo: archive.repo.clone(),
            commit: archive.commit.clone(),
        };
        self.runner.run(&intent).map_err(map_archive_err)?;
        Ok(())
    }
}

/// The native backend (P7 S2/S3): writes a byte/layout-compatible checkpoint shard
/// onto the orphan `rift/checkpoints/v1` branch through screened git plumbing
/// ([`NativeGitWrite`], ADR-0017). Generic over the writer so unit tests use
/// [`MemoryGitWrite`] and production uses `PlumbingGitWrite`. Hashing/attribution/
/// cost/redaction are already native (the default trait methods); this closes the
/// last wrapped capability.
pub struct NativeBackend<W: NativeGitWrite = MemoryGitWrite> {
    writer: W,
    ident: GitIdent,
}

impl NativeBackend<MemoryGitWrite> {
    /// A native backend over the in-memory writer double (tests / dry-run).
    #[must_use]
    pub fn in_memory() -> Self {
        Self {
            writer: MemoryGitWrite::default(),
            ident: GitIdent::default(),
        }
    }

    /// The in-memory writer, for post-archive assertions.
    #[must_use]
    pub fn writer(&self) -> &MemoryGitWrite {
        &self.writer
    }
}

impl<W: NativeGitWrite> NativeBackend<W> {
    /// A native backend over an explicit writer (production: `PlumbingGitWrite`).
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            ident: GitIdent::default(),
        }
    }

    /// Write the checkpoint shard: structural artifacts + the durable content blob,
    /// assembled into a leaf tree grafted onto the orphan branch. Idempotent (the
    /// underlying `commit_on_orphan` no-ops an already-present shard).
    fn write_shard(&self, archive: &CheckpointArchive) -> Result<()> {
        // The one content path through the redaction wall: the redacted capture.
        let content_oid = self.writer.write_durable_blob(&archive.content)?;

        // Structural artifacts (Rift-authored; secret-free by construction).
        let mut entries = vec![
            TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "content.redacted".into(),
                oid: content_oid,
            },
            TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "content_hash.txt".into(),
                oid: self
                    .writer
                    .write_structural_blob(&archive.content_hash_txt())?,
            },
            TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "metadata.json".into(),
                oid: self
                    .writer
                    .write_structural_blob(&archive.metadata_json())?,
            },
            TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "redaction_report.json".into(),
                oid: self
                    .writer
                    .write_structural_blob(&archive.redaction_report_json())?,
            },
        ];
        if let Some(full) = archive.full_jsonl() {
            entries.push(TreeWriteEntry {
                mode: GitFileMode::Blob,
                name: "full.jsonl".into(),
                oid: self.writer.write_structural_blob(&full)?,
            });
        }
        // Canonical git tree order (bytewise by name) so the leaf tree is stable.
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        let leaf = self.writer.write_tree(&entries)?;

        let shard_path = archive.shard_path();
        // Epoch seconds for the commit date (derived from content, never wall clock).
        let date_epoch = archive.created_at_ms.div_euclid(1000);
        self.writer.commit_on_orphan(&OrphanCommit {
            branch: NATIVE_CHECKPOINT_REF,
            shard_path: &shard_path,
            tree_oid: &leaf,
            message: "rift: checkpoint",
            ident: &self.ident,
            date_epoch,
        })?;
        Ok(())
    }
}

impl<W: NativeGitWrite> ShadowBackend for NativeBackend<W> {
    fn archive_checkpoint(&self, archive: &CheckpointArchive) -> Result<()> {
        self.write_shard(archive)
    }
}

/// The composite backend that routes each capability per the [`RouteTable`].
pub struct RoutedBackend<E: ShadowBackend, N: ShadowBackend> {
    entire: E,
    native: N,
    routes: RouteTable,
    divergences: RefCell<Vec<Divergence>>,
}

impl<E: ShadowBackend, N: ShadowBackend> RoutedBackend<E, N> {
    /// Compose a wrapper + native backend under a route table.
    pub fn new(entire: E, native: N, routes: RouteTable) -> Self {
        Self {
            entire,
            native,
            routes,
            divergences: RefCell::new(Vec::new()),
        }
    }

    /// Drain the recorded field-parity divergences (for promotion gating / logging).
    pub fn take_divergences(&self) -> Vec<Divergence> {
        self.divergences.borrow_mut().drain(..).collect()
    }
}

impl<E: ShadowBackend, N: ShadowBackend> ShadowBackend for RoutedBackend<E, N> {
    fn archive_checkpoint(&self, archive: &CheckpointArchive) -> Result<()> {
        match self.routes.route(Capability::Archive) {
            Route::Wrapper => self.entire.archive_checkpoint(archive),
            Route::Native => self.native.archive_checkpoint(archive),
            Route::DualRun => {
                // The wrapper result is canonical and is what the ledger receives.
                let w = self.entire.archive_checkpoint(archive);
                // The native path is exercised for validation only; an error is
                // recorded as a divergence and never affects the wrapper result.
                if let Err(e) = self.native.archive_checkpoint(archive) {
                    self.divergences.borrow_mut().push(Divergence {
                        capability: Capability::Archive,
                        wrapper: "ok".into(),
                        native: format!("error: {e}"),
                    });
                }
                w
            }
        }
    }

    fn checkpoint_root(&self, events: &[CanonicalEvent]) -> Sha256Id {
        match self.routes.route(Capability::CheckpointRoot) {
            Route::Wrapper => self.entire.checkpoint_root(events),
            Route::Native => self.native.checkpoint_root(events),
            Route::DualRun => {
                let w = self.entire.checkpoint_root(events);
                let n = self.native.checkpoint_root(events);
                if w != n {
                    self.divergences.borrow_mut().push(Divergence {
                        capability: Capability::CheckpointRoot,
                        wrapper: w.to_hex(),
                        native: n.to_hex(),
                    });
                }
                w // the trusted wrapper result is always returned during DualRun
            }
        }
    }
}

/// Persist a soak window's divergences as an append-only `verification_run` + its
/// `verification_divergence` rows (the CI/rollout dashboard). A run with zero
/// divergences records the run with `exit_code = 0` and no divergence rows.
///
/// # Errors
/// [`RiftError::Storage`] on an insert failure.
pub async fn persist_divergences(
    store: &impl CanonicalStore,
    run_id: &str,
    divergences: &[Divergence],
    now_ms: i64,
) -> Result<()> {
    let exit_code = i64::from(!divergences.is_empty());
    let outcome = sha256_hex(&[run_id.as_bytes(), b"\0", &exit_code.to_le_bytes()]);
    record_verification_run(
        store,
        &VerificationRunRow {
            run_id: run_id.to_owned(),
            branch: "rift/checkpoints/v1".to_owned(),
            exit_code,
            checked_at: now_ms,
            outcome_sha256: outcome,
            schema_version: 1,
        },
    )
    .await?;
    for (i, d) in divergences.iter().enumerate() {
        record_verification_divergence(
            store,
            &VerificationDivergenceRow {
                run_id: run_id.to_owned(),
                ordinal: i64::try_from(i).unwrap_or(0),
                checkpoint_id: String::new(),
                layer: d.capability.as_str().to_owned(),
                site: "DualRunWrapperVsNative".to_owned(),
                detail: format!("wrapper={} native={}", d.wrapper, d.native),
                schema_version: 1,
            },
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::SecretBytes;
    use rift_shadow_entire::MockRunner;

    /// A backend whose checkpoint_root deliberately diverges (for dual-run tests).
    struct DivergentNative;
    impl ShadowBackend for DivergentNative {
        fn archive_checkpoint(&self, _archive: &CheckpointArchive) -> Result<()> {
            Ok(())
        }
        fn checkpoint_root(&self, _events: &[CanonicalEvent]) -> Sha256Id {
            Sha256Id::from_bytes([0xAB; 32]) // never matches the real root
        }
    }

    /// A redacted (empty) capture minted through the sole sanctioned gate.
    fn redacted() -> RedactedBytes {
        rift_shadow_redact::redact(&SecretBytes::new(Vec::new()))
            .unwrap()
            .redacted
    }

    /// A minimal, valid checkpoint archive over a real 64-hex id.
    fn archive() -> CheckpointArchive {
        CheckpointArchive {
            repo: std::path::PathBuf::from("/tmp/r"),
            commit: "abc123".to_owned(),
            checkpoint_id: "ab".to_string() + &"cd".repeat(31),
            sessions: vec!["native-1".to_owned()],
            strategy: "manual-commit".to_owned(),
            prev_hash: None,
            merkle_root: crate::checkpoint_merkle_root(&[]),
            events: Vec::new(),
            content: redacted(),
            report: RedactionReportView {
                total_spans: 0,
                bytes_masked: 0,
                allowlisted: 0,
                rule_pack_versions: vec!["builtin@2026.06.0".to_owned()],
                input_sha256: "c".repeat(64),
            },
            created_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn router_defaults_to_wrapper() {
        let t = RouteTable::all_wrapper();
        assert_eq!(t.route(Capability::Archive), Route::Wrapper);
        assert_eq!(t.route(Capability::CheckpointRoot), Route::Wrapper);
    }

    #[test]
    fn wrapper_route_invokes_the_runner() {
        let runner = MockRunner::default();
        let rb = RoutedBackend::new(
            EntireBackend::new(&runner),
            NativeBackend::in_memory(),
            RouteTable::all_wrapper(),
        );
        rb.archive_checkpoint(&archive()).unwrap();
        assert_eq!(
            runner.calls.borrow().len(),
            1,
            "wrapper archived via the runner"
        );
    }

    #[test]
    fn native_route_writes_a_shard_not_the_runner() {
        let runner = MockRunner::default();
        let routes = RouteTable::all_wrapper().with(Capability::Archive, Route::Native);
        let rb = RoutedBackend::new(
            EntireBackend::new(&runner),
            NativeBackend::in_memory(),
            routes,
        );
        rb.archive_checkpoint(&archive()).unwrap();
        // The wrapper was never invoked; the native writer produced the shard.
        assert!(
            runner.calls.borrow().is_empty(),
            "native route skips the wrapper"
        );
        let w = rb.native.writer();
        assert_eq!(w.durable_count(), 1, "the redacted content blob (the wall)");
        // content_hash.txt + metadata.json + redaction_report.json (no full.jsonl).
        assert_eq!(w.structural_count(), 3);
        assert_eq!(w.shard_paths().len(), 1, "one shard grafted");
        assert!(w.branch_tip().is_some(), "the branch advanced");
    }

    #[test]
    fn native_archive_is_idempotent() {
        let n = NativeBackend::in_memory();
        n.archive_checkpoint(&archive()).unwrap();
        let tip1 = n.writer().branch_tip();
        // Re-archiving the identical checkpoint is a no-op (same shard path).
        n.archive_checkpoint(&archive()).unwrap();
        assert_eq!(n.writer().branch_tip(), tip1, "idempotent re-archive");
        assert_eq!(n.writer().shard_paths().len(), 1);
    }

    #[test]
    fn metadata_and_report_are_canonical_and_valid() {
        use rift_recall_types::checkpoint::{CheckpointMetadata, RedactionReportFacts};
        let a = archive();
        let meta: CheckpointMetadata = serde_json::from_slice(&a.metadata_json()).unwrap();
        assert_eq!(
            meta.merkle_root.as_deref(),
            Some(a.merkle_root.to_hex()).as_deref()
        );
        assert_eq!(meta.sessions, a.sessions);
        assert_eq!(meta.commit.as_deref(), Some(a.commit.as_str()));
        let report: RedactionReportFacts =
            serde_json::from_slice(&a.redaction_report_json()).unwrap();
        assert!(
            report.is_valid(),
            "the archived report passes the strict bridge gate"
        );
        // Canonical (byte-stable) serialization: identical archive → identical bytes.
        assert_eq!(archive().metadata_json(), a.metadata_json());
    }

    #[test]
    fn dual_run_returns_wrapper_and_records_divergence() {
        let runner = MockRunner::default();
        let routes = RouteTable::all_wrapper().with(Capability::CheckpointRoot, Route::DualRun);
        let rb = RoutedBackend::new(EntireBackend::new(&runner), DivergentNative, routes);
        // The wrapper (real native impl) result is returned, but native diverges →
        // exactly one divergence is recorded (and the wrapper result is canonical).
        let _ = rb.checkpoint_root(&[]);
        let div = rb.take_divergences();
        assert_eq!(div.len(), 1);
        assert_eq!(div[0].capability, Capability::CheckpointRoot);
    }

    #[test]
    fn kill_switch_forces_wrapper() {
        let mut routes = RouteTable::all_wrapper().with(Capability::Archive, Route::Native);
        assert_eq!(routes.route(Capability::Archive), Route::Native);
        routes.trip_kill_switch();
        assert_eq!(routes.route(Capability::Archive), Route::Wrapper);
    }

    #[test]
    fn rewrite_toml_parses_routes() {
        let toml = "[routes]\nhashing = \"native\"\ngit_write = \"dual_run\"\n";
        let t = RouteTable::from_toml_str(toml);
        assert_eq!(t.route(Capability::CheckpointRoot), Route::Native);
        assert_eq!(t.route(Capability::Archive), Route::DualRun);
    }

    #[test]
    fn load_missing_rewrite_toml_is_all_wrapper() {
        let dir = tempfile::tempdir().unwrap();
        let t = RouteTable::load(dir.path());
        assert_eq!(t.route(Capability::Archive), Route::Wrapper);
        assert_eq!(t.route(Capability::CheckpointRoot), Route::Wrapper);
    }

    #[test]
    fn load_reads_routes_and_kill_switch() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("rewrite.toml"),
            "[routes]\ngit_write = \"native\"\nkill_switch = true\n",
        )
        .unwrap();
        // The kill-switch wins over the per-capability native route.
        let t = RouteTable::load(dir.path());
        assert_eq!(t.route(Capability::Archive), Route::Wrapper);
    }
}
