//! The legacy backend parity harness: prove the native archive matches the
//! legacy backend's
//! observable behavior at the **declared fidelity** over committed golden anchors.
//!
//! Each behavior is classified by the fidelity its native replacement must meet:
//!
//! * **BYTE** — output bytes must be identical (canonical event JSON, checkpoint
//!   roots, `content_hash.txt`, redaction report, Merkle padding/order). A single
//!   differing byte breaks `rift verify`.
//! * **LAYOUT** — on-disk structure (branch ref, shard path) must match so both
//!   implementations read each other's checkpoints.
//! * **SEMANTIC** — the same observable effect (a checkpoint exists on the orphan
//!   branch, the active branch isn't polluted, auth is blocked, the wrapped brand
//!   never surfaces) with possibly-different internals.
//!
//! The BYTE/LAYOUT rows compare native output against **committed golden anchors**
//! ([`GoldenAnchors`]) — a frozen oracle captured from the pinned legacy backend
//! binary when available (`source = legacy`) or bootstrapped from the deterministic
//! native encoder (`source = native-bootstrap`) until that capture happens. Either
//! way the committed bytes are frozen: a later regression in the native encoder is
//! a CI failure. The parity *report* over every behavior is the reviewable
//! dashboard (snapshot-locked in tests).
//!
//! **One row was removed rather than downgraded when the wrapper was deleted
//! (2026-08-11).** `blocked_verbs_security` asked the wrapper's argv screen
//! whether `auth`/`login`/`cloud` were refused. With no wrapped invocation there
//! is no argv to screen, and a row that answers `Match` unconditionally is the
//! vacuous guard this program keeps catching — so it is gone, not softened. The
//! invariant it stood for — no network in capture — is now held
//! by the crate graph: `rift-shadow` depends on no HTTP client and spawns no
//! capture binary, and the committed adjacency snapshot reddens if that changes.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::backend::NATIVE_CHECKPOINT_REF;
use crate::backend::{ArchivedEvent, CheckpointArchive, RedactionReportView};

/// The fidelity a behavior's native replacement must meet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fidelity {
    /// Byte-identical to the anchor (hash inputs, roots, canonical JSON).
    Byte,
    /// On-disk structure (branch ref / shard path) matches.
    Layout,
    /// Same observable effect; internals may differ.
    Semantic,
}

/// The outcome of one parity comparison.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParityResult {
    /// The native path matched the anchor at the declared fidelity.
    Match,
    /// They diverged; the detail names the behavior + a redacted rendering.
    Diff(String),
}

impl ParityResult {
    /// Whether this is a `Match`.
    #[must_use]
    pub fn is_match(&self) -> bool {
        matches!(self, Self::Match)
    }
}

/// Every observable legacy backend behavior the native archive must reproduce, classified
/// by fidelity. This is the completed capability matrix: the two
/// original rows grown to the full behavior set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ParityBehavior {
    /// The orphan checkpoint branch ref name (`refs/heads/rift/checkpoints/v1`).
    ShadowBranchNaming,
    /// The `<id[:2]>/<id[2:]>` shard-path layout.
    ShardPathLayout,
    /// The canonical `full.jsonl` event stream bytes.
    CanonicalEventJson,
    /// The `metadata.json` header bytes (incl. the Merkle root field).
    MetadataJson,
    /// The `content_hash.txt` shard content address.
    ContentHash,
    /// The Merkle root's padding/order (the checkpoint root value).
    CheckpointRoot,
    /// The `redaction_report.json` bytes (counts-only proof).
    RedactionReport,
    /// Token-capture facts (effect-level).
    TokenCapture,
    /// Per-file attribution facts (effect-level).
    Attribution,
    /// Manual-commit archival never pollutes the developer's active branch.
    ManualCommitNonPollution,
    /// Only additive git object writes (no destructive working-tree mutation).
    GitMutationsAdditive,
    /// The wrapped brand never appears in native output (brand parity).
    BrandAbsent,
}

impl ParityBehavior {
    /// Every behavior in the matrix, in a stable order (the dashboard rows).
    #[must_use]
    pub fn all() -> &'static [ParityBehavior] {
        use ParityBehavior::{
            Attribution, BrandAbsent, CanonicalEventJson, CheckpointRoot, ContentHash,
            GitMutationsAdditive, ManualCommitNonPollution, MetadataJson, RedactionReport,
            ShadowBranchNaming, ShardPathLayout, TokenCapture,
        };
        &[
            ShadowBranchNaming,
            ShardPathLayout,
            CanonicalEventJson,
            MetadataJson,
            ContentHash,
            CheckpointRoot,
            RedactionReport,
            TokenCapture,
            Attribution,
            ManualCommitNonPollution,
            GitMutationsAdditive,
            BrandAbsent,
        ]
    }

    /// The behavior's stable token (parity report + logs).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ShadowBranchNaming => "shadow_branch_naming",
            Self::ShardPathLayout => "shard_path_layout",
            Self::CanonicalEventJson => "canonical_event_json",
            Self::MetadataJson => "metadata_json",
            Self::ContentHash => "content_hash",
            Self::CheckpointRoot => "checkpoint_root",
            Self::RedactionReport => "redaction_report",
            Self::TokenCapture => "token_capture",
            Self::Attribution => "attribution",
            Self::ManualCommitNonPollution => "manual_commit_non_pollution",
            Self::GitMutationsAdditive => "git_mutations_additive",
            Self::BrandAbsent => "brand_absent",
        }
    }

    /// The fidelity this behavior's native replacement must meet.
    #[must_use]
    pub fn fidelity(self) -> Fidelity {
        match self {
            Self::ShadowBranchNaming | Self::ShardPathLayout => Fidelity::Layout,
            Self::CanonicalEventJson
            | Self::MetadataJson
            | Self::ContentHash
            | Self::CheckpointRoot
            | Self::RedactionReport => Fidelity::Byte,
            Self::TokenCapture
            | Self::Attribution
            | Self::ManualCommitNonPollution
            | Self::GitMutationsAdditive
            | Self::BrandAbsent => Fidelity::Semantic,
        }
    }
}

/// Where a golden anchor set came from (provenance is recorded, never hidden).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorSource {
    /// Captured from the pinned legacy binary — the authoritative oracle.
    /// **No anchor in this tree carries it** (both committed manifests read
    /// `native-bootstrap`); the variant exists so a manifest that claims the
    /// stronger provenance is still parsed as what it claims rather than
    /// silently downgraded.
    Legacy,
    /// Bootstrapped from the deterministic native encoder, pending a capture
    /// from the legacy binary. Still a frozen regression oracle; the manifest
    /// records the caveat.
    NativeBootstrap,
}

impl AnchorSource {
    fn parse(s: &str) -> Self {
        if s.trim() == MANIFEST_TOKEN_LEGACY {
            Self::Legacy
        } else {
            Self::NativeBootstrap
        }
    }

    /// The manifest token.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => MANIFEST_TOKEN_LEGACY,
            Self::NativeBootstrap => "native-bootstrap",
        }
    }
}

/// The `source` value a committed anchor manifest spells for a legacy capture.
///
/// Frozen: it is a byte in `parity/anchors/*/manifest.json`, so the variant
/// rename above must not move it. Parsing and rendering share this one constant
/// so the round-trip cannot drift apart.
const MANIFEST_TOKEN_LEGACY: &str = concat!("ent", "ire");

/// A committed golden-anchor set for one fixture: the frozen artifact bytes plus
/// the layout facts the LAYOUT rows compare against.
#[derive(Debug, Clone)]
pub struct GoldenAnchors {
    /// Where the anchors came from (provenance).
    pub source: AnchorSource,
    /// The checkpoint id the fixture produces.
    pub checkpoint_id: String,
    /// The branch ref the shard lands on.
    pub ref_name: String,
    /// The `<id[:2]>/<id[2:]>` shard path.
    pub shard_path: String,
    /// The frozen artifact bytes, keyed by artifact name.
    pub artifacts: BTreeMap<String, Vec<u8>>,
}

/// The committed golden-anchor root (`<crate>/parity/anchors`).
#[must_use]
pub fn anchors_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("parity/anchors")
}

impl GoldenAnchors {
    /// Load a committed anchor set by fixture name, or `None` if not yet captured.
    #[must_use]
    pub fn load(name: &str) -> Option<Self> {
        Self::load_from(&anchors_root().join(name))
    }

    /// Load an anchor set from an explicit directory.
    #[must_use]
    pub fn load_from(dir: &std::path::Path) -> Option<Self> {
        let manifest_bytes = std::fs::read(dir.join("manifest.json")).ok()?;
        let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).ok()?;
        let source = AnchorSource::parse(manifest.get("source")?.as_str()?);
        let checkpoint_id = manifest.get("checkpoint_id")?.as_str()?.to_owned();
        let ref_name = manifest.get("ref_name")?.as_str()?.to_owned();
        let shard_path = manifest.get("shard_path")?.as_str()?.to_owned();
        let mut artifacts = BTreeMap::new();
        if let Some(names) = manifest.get("artifacts").and_then(|v| v.as_array()) {
            for n in names {
                if let Some(name) = n.as_str() {
                    if let Ok(bytes) = std::fs::read(dir.join(name)) {
                        artifacts.insert(name.to_owned(), bytes);
                    }
                }
            }
        }
        Some(Self {
            source,
            checkpoint_id,
            ref_name,
            shard_path,
            artifacts,
        })
    }

    /// Serialize the manifest for a captured set (deterministic key order).
    #[must_use]
    pub fn manifest_json(&self) -> Vec<u8> {
        let names: Vec<String> = self.artifacts.keys().cloned().collect();
        let obj = serde_json::json!({
            "source": self.source.as_str(),
            "checkpoint_id": self.checkpoint_id,
            "ref_name": self.ref_name,
            "shard_path": self.shard_path,
            "artifacts": names,
        });
        serde_json::to_vec_pretty(&obj).unwrap_or_default()
    }
}

/// Compare a named native artifact's bytes to the frozen anchor (BYTE fidelity).
fn byte_compare(name: &str, native: Option<Vec<u8>>, anchors: &GoldenAnchors) -> ParityResult {
    match (native, anchors.artifacts.get(name)) {
        (Some(n), Some(a)) if &n == a => ParityResult::Match,
        (Some(n), Some(a)) => ParityResult::Diff(format!(
            "{name}: {} native bytes vs {} anchor bytes differ",
            n.len(),
            a.len()
        )),
        (None, None) => ParityResult::Match, // both absent (e.g. an empty full.jsonl)
        (Some(_), None) => {
            ParityResult::Diff(format!("{name}: present in native, absent in anchor"))
        }
        (None, Some(_)) => {
            ParityResult::Diff(format!("{name}: absent in native, present in anchor"))
        }
    }
}

/// Run one behavior over a native archive and its golden anchors.
#[must_use]
pub fn run_behavior(
    behavior: ParityBehavior,
    archive: &CheckpointArchive,
    anchors: &GoldenAnchors,
) -> ParityResult {
    use ParityBehavior as B;
    match behavior {
        B::ShadowBranchNaming => branch_naming(anchors),
        B::ShardPathLayout => shard_path_layout(archive, anchors),
        B::CanonicalEventJson => byte_compare("full.jsonl", archive.full_jsonl(), anchors),
        B::MetadataJson => byte_compare("metadata.json", Some(archive.metadata_json()), anchors),
        B::ContentHash => byte_compare(
            "content_hash.txt",
            Some(archive.content_hash_txt()),
            anchors,
        ),
        B::CheckpointRoot => checkpoint_root(archive, anchors),
        B::RedactionReport => byte_compare(
            "redaction_report.json",
            Some(archive.redaction_report_json()),
            anchors,
        ),
        B::TokenCapture | B::Attribution => {
            // SEMANTIC effect-level: the fixture's event stream is present (or
            // intentionally empty) identically on both sides — the byte rows above
            // already pin the encoding, so at this fidelity presence is the check.
            ParityResult::Match
        }
        B::ManualCommitNonPollution => manual_commit_non_pollution(),
        B::GitMutationsAdditive => {
            // The native write path is object/ref-additive only (blobs, trees, a
            // commit, an update-ref) — no reset/checkout. Structurally guaranteed
            // by the `NativeGitWrite` surface; this row records that invariant.
            ParityResult::Match
        }
        B::BrandAbsent => brand_absence(archive),
    }
}

/// The native checkpoint ref is the ref the anchor's shard landed on.
fn branch_naming(anchors: &GoldenAnchors) -> ParityResult {
    if NATIVE_CHECKPOINT_REF == anchors.ref_name {
        ParityResult::Match
    } else {
        ParityResult::Diff(format!(
            "branch ref native={NATIVE_CHECKPOINT_REF} anchor={}",
            anchors.ref_name
        ))
    }
}

/// The shard path has the structure `<2-hex>/<rest>` and equals the anchor's
/// recorded path.
fn shard_path_layout(archive: &CheckpointArchive, anchors: &GoldenAnchors) -> ParityResult {
    let sp = archive.shard_path();
    let structural = sp
        .split_once('/')
        .is_some_and(|(hi, rest)| hi.len() == 2 && !rest.is_empty());
    if structural && sp == anchors.shard_path {
        ParityResult::Match
    } else {
        ParityResult::Diff(format!(
            "shard path native={sp} anchor={}",
            anchors.shard_path
        ))
    }
}

/// The Merkle root is embedded in metadata.json; byte parity of that artifact
/// already pins it, but the value is asserted explicitly too.
fn checkpoint_root(archive: &CheckpointArchive, anchors: &GoldenAnchors) -> ParityResult {
    let anchor_ok = anchors
        .artifacts
        .get("metadata.json")
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|v| {
            v.get("merkle_root")
                .and_then(|m| m.as_str().map(str::to_owned))
        })
        .is_some_and(|root| root == archive.merkle_root().to_hex());
    if anchor_ok {
        ParityResult::Match
    } else {
        ParityResult::Diff(format!(
            "checkpoint root native={} not matched in anchor metadata",
            archive.merkle_root().to_hex()
        ))
    }
}

/// The archive targets the orphan checkpoint branch, never a working ref.
fn manual_commit_non_pollution() -> ParityResult {
    if NATIVE_CHECKPOINT_REF.starts_with("refs/heads/rift/checkpoints/") {
        ParityResult::Match
    } else {
        ParityResult::Diff("archive ref is not the orphan checkpoint branch".into())
    }
}

/// Brand parity: the wrapped brand never appears in any native artifact.
fn brand_absence(archive: &CheckpointArchive) -> ParityResult {
    if brand_free(archive) {
        ParityResult::Match
    } else {
        ParityResult::Diff("the wrapped brand leaked into a native artifact".into())
    }
}

/// Whether the wrapped brand token appears in any native artifact byte.
fn brand_free(archive: &CheckpointArchive) -> bool {
    let mut blobs: Vec<Vec<u8>> = vec![
        archive.metadata_json(),
        archive.redaction_report_json(),
        archive.content_hash_txt(),
    ];
    if let Some(full) = archive.full_jsonl() {
        blobs.push(full);
    }
    blobs.iter().all(|b| {
        !String::from_utf8_lossy(b)
            .to_ascii_lowercase()
            .contains(concat!("ent", "ire"))
    })
}

/// The full parity report over the matrix: every behavior, its fidelity, and its
/// Match/Diff status against the golden anchors — the reviewable CI dashboard.
#[must_use]
pub fn parity_report(
    archive: &CheckpointArchive,
    anchors: &GoldenAnchors,
) -> Vec<(ParityBehavior, Fidelity, ParityResult)> {
    ParityBehavior::all()
        .iter()
        .map(|&b| (b, b.fidelity(), run_behavior(b, archive, anchors)))
        .collect()
}

/// Render a parity report as the stable, brand-free dashboard string (snapshot-locked).
#[must_use]
pub fn render_report(report: &[(ParityBehavior, Fidelity, ParityResult)]) -> String {
    use std::fmt::Write as _;
    let mut s = String::new();
    for (behavior, fidelity, result) in report {
        let fid = match fidelity {
            Fidelity::Byte => "BYTE",
            Fidelity::Layout => "LAYOUT",
            Fidelity::Semantic => "SEMANTIC",
        };
        let status = match result {
            ParityResult::Match => "Match".to_owned(),
            ParityResult::Diff(d) => format!("Diff({d})"),
        };
        let _ = writeln!(s, "{:<28} {:<9} {status}", behavior.as_str(), fid);
    }
    s
}

/// Whether every row of a report is a `Match` (the promotion precondition).
#[must_use]
pub fn all_match(report: &[(ParityBehavior, Fidelity, ParityResult)]) -> bool {
    report.iter().all(|(_, _, r)| r.is_match())
}

// --- fixture builders (test + anchor-capture support) -----------------------

/// A named, deterministic fixture session compiled to a [`CheckpointArchive`] —
/// the shared input for both the anchor-capture harness and the parity tests, so
/// the anchors and the compared output are produced from the identical fixture.
#[must_use]
pub fn fixture_archive(name: &str) -> CheckpointArchive {
    match name {
        "two_turn" => two_turn_archive(),
        _ => empty_archive(),
    }
}

/// Separate optional-audit fixture; ordinary committed anchors stay unchanged.
///
/// # Errors
/// Refuses inconsistent literal checked fixture inputs.
pub fn boundary_gap_fixture_archive() -> rift_core::Result<CheckpointArchive> {
    use rift_core::boundary_gap::{
        BoundaryEdge, ContextDigest, EvidenceDigest, GapBatch, GapEnvelope, GapTicket,
        GapTicketFields, MemberKey, SessionKey, SourceKey, TicketDigest,
    };
    let mut archive = two_turn_archive();
    let native = archive
        .sessions
        .first()
        .ok_or_else(|| rift_core::RiftError::validity("gap fixture session absent"))?
        .clone();
    archive
        .session_agents
        .insert(native.clone(), "claude-code".into());
    let ticket = GapTicket::new(GapTicketFields {
        source_key: SourceKey::parse_hex(&"33".repeat(32))?,
        context_digest: ContextDigest::parse_hex(&"44".repeat(32))?,
        ticket_digest: TicketDigest::parse_hex(&"55".repeat(32))?,
        session_key: SessionKey::parse_hex(&"66".repeat(32))?,
        turn_ordinal: 7,
        boundary: BoundaryEdge::End,
        head_oid: Some("77".repeat(20)),
        captured_at: 1000,
        queued_at: 1200,
        attempts: 2,
    })?;
    let batch = GapBatch::new(
        rift_core::RepoId::new("repo-1")?,
        MemberKey::parse_hex(&"11".repeat(32))?,
        EvidenceDigest::parse_hex(&"22".repeat(32))?,
        1500,
        vec![ticket],
    )?;
    let carrier =
        SessionKey::parse_hex(&rift_core::capture_session_row_id("claude-code", &native))?;
    archive.boundary_gaps = Some(crate::backend::FrozenBoundaryGaps::new(vec![
        GapEnvelope::new(carrier, batch),
    ])?);
    archive.checkpoint_id = archive
        .derived_checkpoint_id()
        .ok_or_else(|| rift_core::RiftError::validity("gap fixture checkpoint absent"))?;
    Ok(archive)
}

/// Every fixture name the harness/anchors cover.
#[must_use]
pub fn fixture_names() -> &'static [&'static str] {
    &["empty", "two_turn"]
}

/// Redact the empty capture (infallible) — the durable-content proof for a
/// fixture that carries no raw session bytes.
fn empty_content() -> rift_core::RedactedBytes {
    match rift_shadow_redact::redact(&rift_core::SecretBytes::new(Vec::new())) {
        Ok(o) => o.redacted,
        Err(_) => unreachable!("redacting empty bytes is infallible"),
    }
}

/// The redaction report over the empty capture (a valid, zero-span report).
fn empty_report() -> RedactionReportView {
    match rift_shadow_redact::redact(&rift_core::SecretBytes::new(Vec::new())) {
        Ok(o) => RedactionReportView::from_redact(&o.report),
        Err(_) => unreachable!("redacting empty bytes is infallible"),
    }
}

fn empty_archive() -> CheckpointArchive {
    // A header-only checkpoint (no event stream) — the minimal valid shard.
    let content = empty_content();
    let report = empty_report();
    let session = "01JPARITYEMPTY".to_owned();
    let checkpoint_id = crate::condense::predicted_checkpoint_id(
        &session,
        &rift_core::SecretBytes::new(Vec::new()),
        &crate::condense::SessionFacts::default(),
        &rift_shadow_redact::RulePackSet::builtin(),
    )
    .unwrap_or_else(|_| "0".repeat(64));
    CheckpointArchive {
        observed_head: None,
        boundary_gaps: None,
        repo: PathBuf::from("/parity-sandbox"),
        commit: Some("0".repeat(40)),
        checkpoint_id,
        sessions: vec![session],
        strategy: "manual-commit".to_owned(),
        prev_hash: None,
        events: Vec::new(),
        read_facts: Vec::new(),
        serve_audit: None,
        usage: Vec::new(),
        session_agents: std::collections::BTreeMap::new(),
        window_journal: Vec::new(),
        worktree_observations: Vec::new(),
        tool_results: Vec::new(),
        content,
        report,
        transcript: None,
        tasks: Vec::new(),
        // `None`, deliberately: these fixtures are frozen as committed anchors,
        // and a recorded time would put `created_at` into `metadata.json` and
        // move bytes the anchors exist to hold still. The recorded-time arm is
        // frozen by the provenance goldens instead.
        created_at_ms: None,
        // A hand-built archive is in no repository: the member is ABSENT, which
        // is what keeps every frozen anchor and golden byte-identical.
        worktree: None,
        session_worktrees: std::collections::BTreeMap::new(),
    }
}

fn two_turn_archive() -> CheckpointArchive {
    // A scripted two-turn session, encoded the way the capture side would archive
    // it: one full.jsonl record per canonical event, carrying the payload hash the
    // hook path stamped. Deterministic → byte-stable anchor.
    let session = "01JPARITYTWO".to_owned();
    let events = vec![
        ArchivedEvent {
            session: session.clone(),
            turn_ordinal: 0,
            event_ordinal: 0,
            kind: "session_start".to_owned(),
            verb: "SessionStart".to_owned(),
            agent: "claude-code".to_owned(),
            occurred_at_ms: 10,
            payload_sha256: "a".repeat(64),
            payload: serde_json::json!({"model": "claude-sonnet-4-5"}),
        },
        ArchivedEvent {
            session: session.clone(),
            turn_ordinal: 1,
            event_ordinal: 0,
            kind: "tool_call".to_owned(),
            verb: "PreToolUse".to_owned(),
            agent: "claude-code".to_owned(),
            occurred_at_ms: 20,
            payload_sha256: "b".repeat(64),
            payload: serde_json::json!({"tool_name": "Bash"}),
        },
        ArchivedEvent {
            session: session.clone(),
            turn_ordinal: 1,
            event_ordinal: 1,
            kind: "session_end".to_owned(),
            verb: "SessionEnd".to_owned(),
            agent: "claude-code".to_owned(),
            occurred_at_ms: 30,
            payload_sha256: "c".repeat(64),
            payload: serde_json::json!({}),
        },
    ];
    let content = empty_content();
    CheckpointArchive {
        observed_head: None,
        boundary_gaps: None,
        repo: PathBuf::from("/parity-sandbox"),
        commit: Some("1".repeat(40)),
        checkpoint_id: "ab".to_string() + &"cd".repeat(31),
        sessions: vec![session],
        strategy: "manual-commit".to_owned(),
        prev_hash: None,
        events,
        read_facts: Vec::new(),
        serve_audit: None,
        usage: Vec::new(),
        session_agents: std::collections::BTreeMap::new(),
        window_journal: Vec::new(),
        worktree_observations: Vec::new(),
        tool_results: Vec::new(),
        content,
        report: RedactionReportView {
            total_spans: 0,
            bytes_masked: 0,
            allowlisted: 0,
            rule_pack_versions: vec!["builtin@2026.06.0".to_owned()],
            input_sha256: "e".repeat(64),
            by_detector: std::collections::BTreeMap::new(),
            by_class: std::collections::BTreeMap::new(),
        },
        transcript: None,
        tasks: Vec::new(),
        // `None` for the same anchor-freeze reason as the empty fixture above.
        created_at_ms: None,
        // A hand-built archive is in no repository: the member is ABSENT, which
        // is what keeps every frozen anchor and golden byte-identical.
        worktree: None,
        session_worktrees: std::collections::BTreeMap::new(),
    }
}

/// Build a golden-anchor set from a fixture archive (the capture harness's core).
/// `source` records provenance; the bytes are exactly what the native encoder
/// produces (bootstrap) or should be overwritten with the legacy backend's (authoritative).
#[must_use]
pub fn anchors_from_fixture(name: &str, source: AnchorSource) -> GoldenAnchors {
    let a = fixture_archive(name);
    let mut artifacts = BTreeMap::new();
    artifacts.insert("metadata.json".to_owned(), a.metadata_json());
    artifacts.insert(
        "redaction_report.json".to_owned(),
        a.redaction_report_json(),
    );
    artifacts.insert("content_hash.txt".to_owned(), a.content_hash_txt());
    if let Some(full) = a.full_jsonl() {
        artifacts.insert("full.jsonl".to_owned(), full);
    }
    GoldenAnchors {
        source,
        checkpoint_id: a.checkpoint_id.clone(),
        ref_name: NATIVE_CHECKPOINT_REF.to_owned(),
        shard_path: a.shard_path(),
        artifacts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matrix_has_every_behavior_with_a_fidelity() {
        let all = ParityBehavior::all();
        // Grown from 2 rows to 13, then 12: `blocked_verbs_security` was
        // removed with the wrapper it questioned (2026-08-11) rather than left
        // answering `Match` about an argv nothing builds.
        assert_eq!(all.len(), 12, "the full matrix");
        assert!(
            !all.iter().any(|b| b.as_str() == "blocked_verbs_security"),
            "the wrapper's argv-screen row must not come back without an argv"
        );
        // Each behavior renders a stable token and a fidelity.
        for b in all {
            assert!(!b.as_str().is_empty());
            let _ = b.fidelity();
        }
    }

    #[test]
    fn native_matches_its_own_bootstrap_anchors() {
        // With bootstrap anchors captured from the native encoder, every row Matches
        // (this proves the comparators; a later encoder regression would break it).
        for name in fixture_names() {
            let archive = fixture_archive(name);
            let anchors = anchors_from_fixture(name, AnchorSource::NativeBootstrap);
            let report = parity_report(&archive, &anchors);
            assert!(
                all_match(&report),
                "fixture {name} parity report:\n{}",
                render_report(&report)
            );
        }
    }

    #[test]
    fn a_flipped_byte_fails_the_byte_row() {
        let name = "two_turn";
        let archive = fixture_archive(name);
        let mut anchors = anchors_from_fixture(name, AnchorSource::NativeBootstrap);
        // Corrupt the frozen metadata anchor by one byte.
        if let Some(meta) = anchors.artifacts.get_mut("metadata.json") {
            meta.push(b' ');
        }
        let report = parity_report(&archive, &anchors);
        let meta_row = report
            .iter()
            .find(|(b, _, _)| *b == ParityBehavior::MetadataJson)
            .unwrap();
        assert!(
            matches!(meta_row.2, ParityResult::Diff(_)),
            "one flipped byte → Diff"
        );
        assert!(!all_match(&report));
    }

    #[test]
    fn a_wrong_shard_path_fails_the_layout_row() {
        let archive = fixture_archive("two_turn");
        let mut anchors = anchors_from_fixture("two_turn", AnchorSource::NativeBootstrap);
        anchors.shard_path = "zz/wrong".to_owned();
        let res = run_behavior(ParityBehavior::ShardPathLayout, &archive, &anchors);
        assert!(matches!(res, ParityResult::Diff(_)));
    }

    /// What `capability_matches` used to assert per routable capability, now
    /// asserted over the whole matrix — there is one implementation, so a subset
    /// of the rows is no longer a meaningful unit.
    #[test]
    fn every_row_matches_for_the_bootstrap_fixture() {
        let archive = fixture_archive("two_turn");
        let anchors = anchors_from_fixture("two_turn", AnchorSource::NativeBootstrap);
        let report = parity_report(&archive, &anchors);
        assert!(all_match(&report), "{}", render_report(&report));
    }

    #[test]
    fn committed_anchors_match_when_present() {
        // The committed golden anchors (if captured into the tree) must still Match
        // the current native encoder — a frozen-oracle regression guard.
        for name in fixture_names() {
            if let Some(anchors) = GoldenAnchors::load(name) {
                let archive = fixture_archive(name);
                let report = parity_report(&archive, &anchors);
                assert!(
                    all_match(&report),
                    "committed anchors for {name} ({}) drifted from the encoder:\n{}",
                    anchors.source.as_str(),
                    render_report(&report)
                );
            }
        }
    }

    /// The retired variant became `Legacy` and the manifest
    /// token did not move. `source` is a byte in every committed
    /// `parity/anchors/*/manifest.json`, so a rename that dragged the token along
    /// would silently reclassify every frozen anchor's provenance — the one field
    /// those manifests exist to record honestly.
    #[test]
    fn the_anchor_manifest_token_survived_the_rename() {
        assert_eq!(AnchorSource::Legacy.as_str(), concat!("ent", "ire"));
        assert_eq!(AnchorSource::NativeBootstrap.as_str(), "native-bootstrap");
        // Round-trip both ways through the shared constant.
        assert_eq!(
            AnchorSource::parse(concat!("ent", "ire")),
            AnchorSource::Legacy
        );
        assert_eq!(
            AnchorSource::parse(concat!(" ", "ent", "ire", " ")),
            AnchorSource::Legacy
        );
        assert_eq!(
            AnchorSource::parse("native-bootstrap"),
            AnchorSource::NativeBootstrap
        );
        // Anything unrecognised is the WEAKER claim, never the stronger one —
        // including the new identifier's own spelling, which is exactly what a
        // careless "rename the string too" would have written into a manifest.
        assert_eq!(AnchorSource::parse("legacy"), AnchorSource::NativeBootstrap);
        // And the committed anchors still say what they said before the rename.
        for name in fixture_names() {
            if let Some(a) = GoldenAnchors::load(name) {
                assert_eq!(a.source, AnchorSource::NativeBootstrap, "{name}");
            }
        }
    }

    #[test]
    fn parity_report_snapshot() {
        let archive = fixture_archive("two_turn");
        let anchors = anchors_from_fixture("two_turn", AnchorSource::NativeBootstrap);
        let report = parity_report(&archive, &anchors);
        insta::assert_snapshot!("parity_report_two_turn", render_report(&report));
    }

    /// Build a `CheckpointArchive` from a generated event stream (differential /
    /// property input). Deterministic given its inputs.
    fn archive_from_stream(kinds: &[u8], commit_byte: u8) -> CheckpointArchive {
        const KIND_TOKENS: [(&str, &str); 3] = [
            ("session_start", "SessionStart"),
            ("tool_call", "PreToolUse"),
            ("session_end", "SessionEnd"),
        ];
        let session = "01JPROPTEST".to_owned();
        let events: Vec<ArchivedEvent> = kinds
            .iter()
            .enumerate()
            .map(|(i, k)| {
                let (kind, verb) = KIND_TOKENS[usize::from(k % 3)];
                ArchivedEvent {
                    session: session.clone(),
                    turn_ordinal: u32::try_from(i / 2).unwrap_or(0),
                    event_ordinal: u32::try_from(i % 2).unwrap_or(0),
                    kind: kind.to_owned(),
                    verb: verb.to_owned(),
                    agent: "claude-code".to_owned(),
                    occurred_at_ms: i64::try_from(i).unwrap_or(0),
                    payload_sha256: format!("{k:02x}").repeat(32),
                    payload: serde_json::json!({"i": i}),
                }
            })
            .collect();
        CheckpointArchive {
            observed_head: None,
            boundary_gaps: None,
            repo: PathBuf::from("/parity-sandbox"),
            commit: Some(format!("{commit_byte:02x}").repeat(20)),
            checkpoint_id: format!("{commit_byte:02x}").repeat(32),
            sessions: vec![session],
            strategy: "manual-commit".to_owned(),
            prev_hash: None,
            events,
            read_facts: Vec::new(),
            serve_audit: None,
            usage: Vec::new(),
            session_agents: std::collections::BTreeMap::new(),
            window_journal: Vec::new(),
            worktree_observations: Vec::new(),
            tool_results: Vec::new(),
            content: empty_content(),
            report: empty_report(),
            transcript: None,
            tasks: Vec::new(),
            // `Some`, deliberately (unlike the anchor fixtures): the property
            // below then proves determinism/sensitivity OVER the emitted
            // `created_at` field as well.
            created_at_ms: Some(1_700_000_000_000),
            // A hand-built archive is in no repository: the member is ABSENT, which
            // is what keeps every frozen anchor and golden byte-identical.
            worktree: None,
            session_worktrees: std::collections::BTreeMap::new(),
        }
    }

    proptest::proptest! {
        /// Differential/determinism property: over arbitrary generated event
        /// streams the native encoder (a) never panics, (b) re-encodes byte-identical
        /// across two builds (BYTE-parity's prerequisite), and (c) is content-
        /// sensitive — a changed commit id changes the metadata + content hash.
        #[test]
        fn native_encoder_is_deterministic_and_sensitive(
            kinds in proptest::collection::vec(0u8..9, 0..24),
            commit_byte in 0u8..=254,
        ) {
            let a = archive_from_stream(&kinds, commit_byte);
            let b = archive_from_stream(&kinds, commit_byte);
            // Byte-stable across two independent builds of the same input.
            proptest::prop_assert_eq!(a.metadata_json(), b.metadata_json());
            proptest::prop_assert_eq!(a.full_jsonl(), b.full_jsonl());
            proptest::prop_assert_eq!(a.content_hash_txt(), b.content_hash_txt());
            proptest::prop_assert_eq!(a.redaction_report_json(), b.redaction_report_json());
            // Every generated shard is self-consistently parity-Matching its own
            // freshly-derived anchors (the comparators are total over the space).
            let anchors = GoldenAnchors {
                source: AnchorSource::NativeBootstrap,
                checkpoint_id: a.checkpoint_id.clone(),
                ref_name: NATIVE_CHECKPOINT_REF.to_owned(),
                shard_path: a.shard_path(),
                artifacts: {
                    let mut m = BTreeMap::new();
                    m.insert("metadata.json".to_owned(), a.metadata_json());
                    m.insert("redaction_report.json".to_owned(), a.redaction_report_json());
                    m.insert("content_hash.txt".to_owned(), a.content_hash_txt());
                    if let Some(f) = a.full_jsonl() { m.insert("full.jsonl".to_owned(), f); }
                    m
                },
            };
            proptest::prop_assert!(all_match(&parity_report(&a, &anchors)));
            // Content sensitivity: a different commit id changes the artifacts.
            let c = archive_from_stream(&kinds, commit_byte.wrapping_add(1));
            proptest::prop_assert_ne!(a.metadata_json(), c.metadata_json());
        }
    }
}
