//! Import-only reader for the pre-Rift capture tool's on-disk checkpoint
//! formats — read-compat only.
//!
//! **Read-only, and untrusting.** Nothing here writes. The bytes this module
//! hands back are [`SecretBytes`] — exactly as untrusted as bytes off a live
//! hook — so the only route from an imported shard to durable storage is the
//! ordinary [`rift_shadow_redact::redact`] gate that every captured byte takes.
//! This module never mints [`rift_core::RedactedBytes`] and must never gain a
//! way to: one `seal` site, no second constructor.
//!
//! **No legacy naming on any public surface.** Every public item here is named
//! for what it *is* (`ShardedBranch`, `PerCheckpointRefs`), never for the tool
//! that wrote it, and [`ImportReport::render`] is property-tested brand-free.
//! The ref-name *literals* in [`source_ref_prefixes`] are a different thing: a
//! fact about repositories Rift does not own. Renaming those would break
//! read-compat, so they are values, never identifiers.
//!
//! # The two layouts, and where each fact came from
//!
//! Nothing in this module was derived from a network fetch. Each legacy fact is
//! cited to the in-tree declaration that already carried it:
//!
//! * [`Layout::ShardedBranch`] — one orphan branch holding many sharded
//!   checkpoints, ids **12 hex**. Ref name per
//!   `rift_recall_substrate::Dialect::checkpoint_ref`; shard path
//!   `<id[:2]>/<id[2:]>` per `crate::backend::CheckpointArchive::shard_path`
//!   (`crates/rift-shadow/src/backend/artifacts.rs`); the 12-hex id fact per the doc comment on
//!   `MIN_ID_LEN` in `crates/rift-shadow-attribution/src/provenance.rs:66`
//!   ("legacy ids are 12 hex; Rift's are 64").
//! * [`Layout::PerCheckpointRefs`] — one ref per checkpoint, id **ULID**, the
//!   same shard tree inside each ref's commit. **This layout is the gap:**
//!   `rift_recall_substrate::Dialect` models only the branch one, so a
//!   repository that migrated to per-checkpoint refs is unreadable without
//!   this module.
//! * Artifact names and field spellings — `rift_recall_types::checkpoint`,
//!   whose serde aliases are the legacy spellings (`session_ids`,
//!   `commit_oid`/`head`, `id`, `name`/`tool`, `file_path`, `sha256`,
//!   `session_id`/`turn`/`event`/`occurred_at`).
//! * Epoch — a shard with no `canon_version` is epoch **0**
//!   (`crate::backend`): the pre-rewrite canonicalization.

//! # Which trust stage imported data enters at
//!
//! **The lowest one, and it can never be promoted from here.** An imported shard
//! claims a `merkle_root` computed by another tool. [`rederive_root`] recomputes
//! one from the shard's own `full.jsonl` with Rift's canonicalization and
//! [`root_check`] compares them — the re-derive-before-trusting posture the
//! bundle-import gate condition requires. What that comparison can and cannot
//! conclude is the honest part:
//!
//! * **It agreeing proves self-consistency, not provenance.** The events and the
//!   header came out of the same file; nothing outside the shard was consulted.
//!   The events enter as [`rift_core::Trust::GitFact`], the same class a shard
//!   read off Rift's own branch enters at — and no higher.
//! * **It disagreeing does not prove tampering.** A pre-rewrite shard is epoch 0,
//!   whose leaf canonicalization is *deliberately different* from Rift's, and
//!   epoch-0 anchors are deliberately "not a target the native encoder must
//!   byte-match". So a mismatch is a tampered shard **or** a
//!   legitimately differently-canonicalized one, and this module cannot tell
//!   which.
//!
//! Therefore: **an imported legacy checkpoint is never chain-verified.** It is
//! recorded as a claim with its root check attached, and `rift verify` must not
//! treat it as an anchor. Anything that later wants to promote imported data
//! needs an epoch-0 canonicalizer, which does not exist in this tree — building
//! one is a decision, not a patch.

use std::collections::BTreeMap;

use rift_core::{CanonicalEvent, EventKind, SecretBytes, Sha256Id, Trust};
use rift_recall_types::checkpoint::{CheckpointMetadata, FullEventRecord, RedactionReportFacts};

/// The checkpoint ref namespaces an import walks, most specific first.
///
/// Values, not names: these are the on-disk spellings of repositories written
/// by another tool, and read-compat forbids changing them.
const REF_PREFIX_PER_CHECKPOINT: &str = concat!("refs/", "ent", "ire", "/checkpoints/");
const REF_BRANCH_V1: &str = concat!("refs/heads/", "ent", "ire", "/checkpoints/v1");

/// The artifact file names an imported shard may carry, in a stable order.
/// A shard is not required to have all of them; absence is a fact, not an error.
pub const SHARD_ARTIFACTS: &[&str] = &[
    "metadata.json",
    "full.jsonl",
    "content_hash.txt",
    "redaction_report.json",
    "tool_calls.jsonl",
    "attribution.json",
    "token_estimates.json",
    "test_results.json",
    "cost_estimate.json",
];

/// The ref prefixes an import enumerates. Callers walk their repository's refs
/// and offer each name to [`classify_ref`]; this list exists so a caller can
/// narrow the walk first.
#[must_use]
pub fn source_ref_prefixes() -> &'static [&'static str] {
    &[REF_PREFIX_PER_CHECKPOINT, REF_BRANCH_V1]
}

/// The on-disk checkpoint layout a source ref is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Layout {
    /// One orphan branch holding every checkpoint, sharded `<id[:2]>/<id[2:]>`.
    /// Ids are short hex. Serializes concurrent writers — the reason the other
    /// layout exists.
    ShardedBranch,
    /// One ref per checkpoint, the id carried in the ref name. Ids are ULID
    /// (time-sortable), the shard tree lives inside each ref's own commit.
    PerCheckpointRefs,
}

impl Layout {
    /// The layout's stable, brand-free token (reports, logs, `--json`).
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::ShardedBranch => "sharded-branch",
            Self::PerCheckpointRefs => "per-checkpoint-refs",
        }
    }
}

/// The id spellings the two layouts use. Id kind is how a reader routes a bare
/// id to its layout when no ref name is at hand.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    /// Short lowercase hex (the sharded-branch layout).
    ShortHex,
    /// A 26-character Crockford-base32 ULID (the per-checkpoint-refs layout).
    Ulid,
}

/// Crockford base32, the ULID alphabet: no `I`, `L`, `O`, `U`.
const ULID_ALPHABET: &[u8] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
/// A ULID is exactly 26 symbols.
const ULID_LEN: usize = 26;
/// The shortest admissible sharded-branch id (the legacy floor).
const MIN_SHORT_HEX: usize = 12;

/// Classify a bare checkpoint id, or `None` if it is neither spelling.
///
/// Hex is checked first and ULID second: they are disjoint at these lengths
/// because a 26-symbol string cannot be a valid 12-or-more *even*-length hex id
/// that also avoids the ULID alphabet's excluded letters — but the order is
/// fixed here rather than left to luck.
#[must_use]
pub fn id_kind(id: &str) -> Option<IdKind> {
    let b = id.as_bytes();
    if b.len() >= MIN_SHORT_HEX
        && b.len().is_multiple_of(2)
        && b.iter()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
    {
        return Some(IdKind::ShortHex);
    }
    if b.len() == ULID_LEN && b.iter().all(|c| ULID_ALPHABET.contains(c)) {
        return Some(IdKind::Ulid);
    }
    None
}

/// What a source ref denotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    /// A branch holding many sharded checkpoints; the ids are inside it.
    Branch,
    /// Exactly one checkpoint, whose id the ref name carries.
    Checkpoint(String),
}

/// Classify one ref name. `None` means "not an importable source" — the safe
/// answer, and the one every unknown ref gets.
///
/// Fail-closed on a malformed per-checkpoint ref: a ref under the checkpoint
/// namespace whose trailing component is not a well-formed id is **not** a
/// source. An importer that guessed here would import an attacker-chosen name.
#[must_use]
pub fn classify_ref(ref_name: &str) -> Option<(Layout, Source)> {
    if ref_name == REF_BRANCH_V1 {
        return Some((Layout::ShardedBranch, Source::Branch));
    }
    let rest = ref_name.strip_prefix(REF_PREFIX_PER_CHECKPOINT)?;
    // `<shard>/<id>`: the shard segment is a redundant prefix of the id, so the
    // id is the last component and the shard is verified against it.
    let (shard, id) = rest.rsplit_once('/')?;
    if shard.is_empty() || id_kind(id) != Some(IdKind::Ulid) || !id.starts_with(shard) {
        return None;
    }
    Some((Layout::PerCheckpointRefs, Source::Checkpoint(id.to_owned())))
}

/// The layout of the ref that carries a shard, or `None` if the ref is not a
/// legacy checkpoint source.
///
/// The importer's entry point, and the reason it exists separately from
/// [`classify_ref`]: a caller that has a shard and the ref it came from wants
/// the LAYOUT and nothing else. The first version of the import verb had no
/// such call — it passed [`Layout::ShardedBranch`] as a literal — so every
/// refs-layout shard it might have found would have been mislabelled on top of
/// not being found at all.
#[must_use]
pub fn layout_of_shard_ref(ref_name: &str) -> Option<Layout> {
    classify_ref(ref_name).map(|(layout, _)| layout)
}

/// The `<id[:2]>/<id[2:]>` shard path for a checkpoint id, or `None` if the id
/// is too short to shard.
#[must_use]
pub fn shard_path(checkpoint_id: &str) -> Option<String> {
    if !checkpoint_id.is_char_boundary(2) || checkpoint_id.len() < 3 {
        return None;
    }
    let (hi, rest) = checkpoint_id.split_at(2);
    Some(format!("{hi}/{rest}"))
}

/// One imported checkpoint, parsed leniently and judged nowhere.
///
/// Holds no raw bytes: `Shard::content` re-wraps them on demand as
/// [`SecretBytes`], so the struct itself can never be the thing that walks a
/// raw transcript into a log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shard {
    /// The checkpoint id, as spelled on disk.
    pub checkpoint_id: String,
    /// Which layout it was read from.
    pub layout: Layout,
    /// The `metadata.json` header (legacy field spellings accepted).
    pub metadata: CheckpointMetadata,
    /// The canonicalization epoch: `canon_version` when present, else **0**.
    pub epoch: u32,
    /// The `full.jsonl` records, in file order. Unparseable lines are counted
    /// in [`Shard::issues`], never silently dropped and never fatal.
    pub events: Vec<FullEventRecord>,
    /// The `redaction_report.json` facts (counts only, never spans).
    pub report: RedactionReportFacts,
    /// Artifact names present in the shard, sorted.
    pub artifacts: Vec<String>,
    /// Lenient-parse notes: `<artifact>: <category>`, never input bytes.
    pub issues: Vec<String>,
}

/// Wrap an imported shard's raw artifact bytes as **untrusted** input.
///
/// The return type is the whole point, and it is the reason this is the only
/// named entry point from an import to the rest of the system: an importer that
/// wants these bytes durable must hand them to [`rift_shadow_redact::redact`]
/// like any captured byte. There is no other path, here or anywhere — a shard
/// that was redacted by another tool is still, to Rift, raw.
#[must_use]
pub fn untrusted_content(raw_artifact_bytes: Vec<u8>) -> SecretBytes {
    SecretBytes::new(raw_artifact_bytes)
}

impl Shard {
    /// Whether the shard carries a structurally valid redaction report — the
    /// same precondition [`crate::artifact_bridge`] enforces on native shards.
    /// An import must refuse the rest exactly as the bridge does.
    #[must_use]
    pub fn report_is_valid(&self) -> bool {
        self.report.is_valid()
    }
}

/// Parse one shard from its artifact bytes. Total: never panics, never errors —
/// every problem lands in [`Shard::issues`], because a half-readable legacy
/// shard is evidence, not a crash.
#[must_use]
pub fn parse_shard(
    checkpoint_id: &str,
    layout: Layout,
    artifacts: &BTreeMap<String, Vec<u8>>,
) -> Shard {
    let mut issues = Vec::new();
    let metadata = if let Some(b) = artifacts.get("metadata.json") {
        serde_json::from_slice::<CheckpointMetadata>(b).unwrap_or_else(|_| {
            issues.push("metadata.json: unparseable".to_owned());
            CheckpointMetadata::default()
        })
    } else {
        issues.push("metadata.json: absent".to_owned());
        CheckpointMetadata::default()
    };
    // No `canon_version` field means the pre-rewrite epoch, 0.
    let epoch = metadata.canon_version.unwrap_or(0);

    let mut events = Vec::new();
    if let Some(b) = artifacts.get("full.jsonl") {
        let mut bad = 0usize;
        for line in b.split(|c| *c == b'\n') {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_slice::<FullEventRecord>(line) {
                Ok(e) => events.push(e),
                Err(_) => bad += 1,
            }
        }
        if bad > 0 {
            issues.push(format!("full.jsonl: {bad} unparseable line(s)"));
        }
    }

    let report = if let Some(b) = artifacts.get("redaction_report.json") {
        serde_json::from_slice::<RedactionReportFacts>(b).unwrap_or_else(|_| {
            issues.push("redaction_report.json: unparseable".to_owned());
            RedactionReportFacts::default()
        })
    } else {
        issues.push("redaction_report.json: absent".to_owned());
        RedactionReportFacts::default()
    };

    Shard {
        checkpoint_id: checkpoint_id.to_owned(),
        layout,
        metadata,
        epoch,
        events,
        report,
        artifacts: artifacts.keys().cloned().collect(),
        issues,
    }
}

/// The trust class every imported event enters at.
///
/// A named constant rather than a literal at the call site, because the value is
/// the whole claim: imported bytes are treated exactly like bytes read off
/// Rift's own checkpoint branch, and **never** better.
pub const IMPORT_TRUST: Trust = Trust::GitFact;

/// Re-derive a shard's checkpoint root from its own event stream, with Rift's
/// canonicalization. `None` when the shard carries no parseable events.
///
/// This is the *re-derivation*, not the *verification*: see the module header
/// for why agreement is weaker evidence than it looks and disagreement is
/// weaker evidence than it looks.
#[must_use]
pub fn rederive_root(shard: &Shard) -> Option<Sha256Id> {
    let events = canonical_events(shard);
    if events.is_empty() {
        return None;
    }
    Some(crate::checkpoint_merkle_root(&events))
}

/// The shard's events as canonical events. Records whose kind or payload hash
/// this build cannot read are dropped — which is itself a reason a re-derived
/// root may differ, and why [`RootCheck::Mismatch`] is not an accusation.
///
/// **The kind token is read by [`EventKind::from_wire_token`], the one reader of
/// the archived dialect.** This module carried a private `read_kind` that was
/// byte-identical to it — written in the same commit that argued a wire dialect
/// must have exactly one reader — so the argument and the code disagreed. Two
/// writers of one rule is this program's most expensive recurring defect, and
/// the cheapest moment to kill an instance of it is while it is still a
/// duplicate rather than a divergence: nothing would have reddened today if only
/// one of the two had learned a new spelling.
fn canonical_events(shard: &Shard) -> Vec<CanonicalEvent> {
    shard
        .events
        .iter()
        .filter_map(|e| {
            Some(CanonicalEvent {
                schema_version: 1,
                session: e.session.clone(),
                turn_ordinal: e.turn_ordinal,
                event_ordinal: e.event_ordinal,
                kind: EventKind::from_wire_token(&e.kind)?,
                trust: IMPORT_TRUST,
                occurred_at_ms: e.occurred_at_ms,
                payload_sha256: Sha256Id::parse_hex(e.payload_sha256.as_deref()?).ok()?,
            })
        })
        .collect()
}

/// What comparing a shard's claimed root against a re-derived one established.
///
/// Deliberately **not** named `Valid`/`Invalid`. None of these outcomes promotes
/// a shard past [`IMPORT_TRUST`]; they record what was checked so a later reader
/// is not left guessing whether anyone looked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootCheck {
    /// Claimed and re-derived roots agree — the shard is self-consistent.
    /// Not provenance: nothing outside the shard was consulted.
    SelfConsistent,
    /// They disagree. Tampering **or** an epoch-0 canonicalization difference,
    /// and this module cannot distinguish them (module header).
    Mismatch,
    /// The header claims no root, so there was nothing to check against.
    Unclaimed,
    /// The shard has no re-derivable events, so nothing could be re-derived.
    NotDerivable,
}

impl RootCheck {
    /// The stable, brand-free token for reports and `--json`.
    #[must_use]
    pub fn token(self) -> &'static str {
        match self {
            Self::SelfConsistent => "self-consistent",
            Self::Mismatch => "mismatch",
            Self::Unclaimed => "unclaimed",
            Self::NotDerivable => "not-derivable",
        }
    }
}

/// Compare a shard's claimed root against one re-derived from its own events.
#[must_use]
pub fn root_check(shard: &Shard) -> RootCheck {
    let Some(derived) = rederive_root(shard) else {
        return RootCheck::NotDerivable;
    };
    let Some(claimed) = shard.metadata.merkle_root.as_deref() else {
        return RootCheck::Unclaimed;
    };
    if claimed.eq_ignore_ascii_case(&derived.to_hex()) {
        RootCheck::SelfConsistent
    } else {
        RootCheck::Mismatch
    }
}

/// The durable `imported_checkpoint` row for one shard: a home
/// for the claim, never a chain position. Pure — no I/O, no clock read;
/// `imported_at` is the caller's own timestamp (the injected-clock rule: this
/// module never reads the wall clock).
///
/// Callers decide WHEN to record this (this crate's own `report_is_valid`
/// gate, mirroring [`ImportReport::observe`]'s importable/refused split, is
/// the caller's business, not this function's — a pure builder should not
/// also be a policy).
#[must_use]
pub fn imported_checkpoint_record(
    shard: &Shard,
    imported_at: i64,
) -> rift_store::ImportedCheckpointRecord {
    rift_store::ImportedCheckpointRecord {
        import_id: crate::ids::imported_checkpoint_id(&shard.checkpoint_id, shard.layout.token()),
        source_id: shard.checkpoint_id.clone(),
        source_layout: shard.layout.token().to_owned(),
        source_root_claim: shard.metadata.merkle_root.clone(),
        root_check: root_check(shard).token().to_owned(),
        imported_at,
        schema_version: 1,
    }
}

/// What one import run found. Counts and categories only — never an id from a
/// foreign repository, never a ref name, never a byte of payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Shards seen, per layout.
    pub seen: BTreeMap<&'static str, usize>,
    /// Shards whose redaction report is structurally valid (importable).
    pub importable: usize,
    /// Shards refused for want of a valid redaction report.
    pub refused: usize,
    /// Canonical event records read across every importable shard.
    pub events: usize,
    /// Root-check outcomes, by token. Every importable shard contributes exactly
    /// one, so a reader can never mistake "nobody looked" for "nothing wrong".
    pub root_checks: BTreeMap<&'static str, usize>,
    /// Lenient-parse note categories, deduplicated and sorted.
    pub notes: Vec<String>,
}

impl ImportReport {
    /// Fold one parsed shard into the report.
    pub fn observe(&mut self, shard: &Shard) {
        *self.seen.entry(shard.layout.token()).or_default() += 1;
        if shard.report_is_valid() {
            self.importable += 1;
            self.events += shard.events.len();
            *self
                .root_checks
                .entry(root_check(shard).token())
                .or_default() += 1;
        } else {
            self.refused += 1;
        }
        for note in &shard.issues {
            if !self.notes.contains(note) {
                self.notes.push(note.clone());
            }
        }
        self.notes.sort();
    }

    /// Render the operator-facing summary. Stable, deterministic, and — the
    /// property below is the guard — free of the legacy brand for **every**
    /// input, because the only strings it emits are its own.
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        for (layout, n) in &self.seen {
            let _ = writeln!(s, "layout {layout}: {n} checkpoint(s)");
        }
        let _ = writeln!(
            s,
            "importable {} · refused {} · events {}",
            self.importable, self.refused, self.events
        );
        for (check, n) in &self.root_checks {
            let _ = writeln!(s, "root-check {check}: {n}");
        }
        // Stated on every run, not only when something looks wrong: a reader who
        // sees "self-consistent" must not read it as "verified".
        if self.importable > 0 {
            let _ = writeln!(
                s,
                "imported checkpoints are claims, not chain anchors — \
                 a pre-rewrite root cannot be re-derived by this build"
            );
        }
        for note in &self.notes {
            let _ = writeln!(s, "note: {note}");
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RETIRED_TOKEN: &str = concat!("ent", "ire");

    /// Brand tokens that must never reach a user through this module. Ported
    /// from the wrapper's `rebrand_never_leaks_brand`, which guarded the same
    /// property on the other side of the same boundary.
    const FORBIDDEN_BRAND_TOKENS: &[&str] = &[RETIRED_TOKEN, concat!("ent", "ireio")];

    fn artifacts(pairs: &[(&str, &str)]) -> BTreeMap<String, Vec<u8>> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.as_bytes().to_vec()))
            .collect()
    }

    #[test]
    fn both_layouts_classify_from_their_ref_names() {
        assert_eq!(
            classify_ref(concat!("refs/heads/", "ent", "ire", "/checkpoints/v1")),
            Some((Layout::ShardedBranch, Source::Branch))
        );
        let ulid = "01JQZ8T9WQKX3M7VN2P4RB6CDE";
        let r = format!("refs/{RETIRED_TOKEN}/checkpoints/01/{ulid}");
        assert_eq!(
            classify_ref(&r),
            Some((
                Layout::PerCheckpointRefs,
                Source::Checkpoint(ulid.to_owned())
            ))
        );
    }

    #[test]
    fn a_ref_outside_both_namespaces_is_never_a_source() {
        for r in [
            "refs/heads/main",
            "refs/heads/rift/checkpoints/v1",
            concat!("refs/tags/", "ent", "ire", "/sessions/s1"),
            concat!("refs/", "ent", "ire", "/checkpoints/"),
        ] {
            assert_eq!(classify_ref(r), None, "{r}");
        }
    }

    #[test]
    fn a_malformed_per_checkpoint_ref_fails_closed() {
        // Shard segment disagreeing with the id; no shard segment at all; and —
        // the case that discriminates the id check from the shard check — a ref
        // whose shard segment IS a correct prefix of a trailing component that
        // is nevertheless not an id. The first two versions of this test had only
        // the first two rows, and deleting the `id_kind` clause left them green:
        // `"not-a-ulid".starts_with("ab")` is already false, so the shard check
        // was doing all the work and the id check was unguarded.
        for r in [
            concat!(
                "refs/",
                "ent",
                "ire",
                "/checkpoints/zz/01JQZ8T9WQKX3M7VN2P4RB6CDE"
            ),
            concat!(
                "refs/",
                "ent",
                "ire",
                "/checkpoints/01JQZ8T9WQKX3M7VN2P4RB6CDE"
            ),
            concat!("refs/", "ent", "ire", "/checkpoints/ab/abcdef"),
            concat!(
                "refs/",
                "ent",
                "ire",
                "/checkpoints/01/01JQZ8T9WQKX3M7VN2P4RB6CD"
            ),
        ] {
            assert_eq!(classify_ref(r), None, "{r}");
        }
    }

    #[test]
    fn id_kinds_route_the_two_layouts_apart() {
        assert_eq!(id_kind("ab34cd56ef78"), Some(IdKind::ShortHex));
        assert_eq!(id_kind(&"a".repeat(64)), Some(IdKind::ShortHex));
        assert_eq!(id_kind("01JQZ8T9WQKX3M7VN2P4RB6CDE"), Some(IdKind::Ulid));
        // Too short, odd length, uppercase hex, and a ULID with an excluded letter.
        assert_eq!(id_kind("abc"), None);
        assert_eq!(id_kind("abc34cd56ef78"), None);
        assert_eq!(id_kind("AB34CD56EF78"), None);
        assert_eq!(id_kind("01JQZ8T9WQKX3M7VN2P4RB6CDI"), None);
    }

    #[test]
    fn shard_path_matches_the_two_hex_prefix_layout() {
        assert_eq!(shard_path("ab34cd56ef78").as_deref(), Some("ab/34cd56ef78"));
        assert_eq!(shard_path("ab"), None);
    }

    /// The legacy field spellings, every one of them, in one shard: this is the
    /// read-compat promise, expressed as a test.
    #[test]
    fn a_legacy_shard_parses_through_its_own_field_spellings() {
        let a = artifacts(&[
            (
                "metadata.json",
                r#"{"strategy":"manual-commit","session_ids":["s1","s2"],
                    "commit_oid":"1111111111111111111111111111111111111111",
                    "developer_id":"dev-hash","future_field":42}"#,
            ),
            (
                "full.jsonl",
                "{\"session_id\":\"s1\",\"turn\":1,\"event\":0,\"kind\":\"tool.call\",\"verb\":\"PreToolUse\",\"occurred_at\":20}\n",
            ),
            (
                "redaction_report.json",
                &format!(
                    r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
                    "a".repeat(64)
                ),
            ),
        ]);
        let s = parse_shard("ab34cd56ef78", Layout::ShardedBranch, &a);
        assert_eq!(s.metadata.sessions, vec!["s1".to_owned(), "s2".to_owned()]);
        assert_eq!(s.metadata.developer.as_deref(), Some("dev-hash"));
        assert!(s.metadata.commit.is_some());
        // No `canon_version` ⇒ the pre-rewrite epoch, not a default of 1.
        assert_eq!(s.epoch, 0);
        assert_eq!(s.events.len(), 1);
        assert_eq!(s.events[0].session, "s1");
        assert_eq!(s.events[0].turn_ordinal, 1);
        assert_eq!(s.events[0].occurred_at_ms, 20);
        assert!(s.report_is_valid());
        assert!(s.issues.is_empty(), "{:?}", s.issues);
    }

    #[test]
    fn a_native_epoch_shard_keeps_its_declared_epoch() {
        let a = artifacts(&[("metadata.json", r#"{"canon_version":1}"#)]);
        assert_eq!(
            parse_shard("ab34cd56ef78", Layout::ShardedBranch, &a).epoch,
            1
        );
    }

    #[test]
    fn a_half_readable_shard_reports_rather_than_panics() {
        let a = artifacts(&[
            ("metadata.json", "{not json"),
            ("full.jsonl", "{\"session_id\":\"s\"}\n{oops\n\n"),
        ]);
        let s = parse_shard("ab34cd56ef78", Layout::ShardedBranch, &a);
        assert_eq!(s.events.len(), 1);
        assert!(!s.report_is_valid(), "no report ⇒ not importable");
        assert!(s.issues.iter().any(|i| i.starts_with("metadata.json")));
        assert!(s.issues.iter().any(|i| i.contains("1 unparseable line")));
        assert!(s.issues.iter().any(|i| i.starts_with("redaction_report")));
    }

    #[test]
    fn imported_bytes_are_secret_so_the_redaction_gate_is_the_only_way_out() {
        let raw = untrusted_content(b"anything at all".to_vec());
        // The type is the proof: `SecretBytes` has no durable-surface API, and
        // `redact` is the sole producer of the type that does.
        let out = rift_shadow_redact::redact(&raw).expect("redacting is total");
        assert_eq!(out.redacted.as_slice(), b"anything at all");
    }

    #[test]
    fn a_report_counts_both_layouts_and_separates_refusals() {
        let good = artifacts(&[(
            "redaction_report.json",
            &format!(
                r#"{{"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
                "a".repeat(64)
            ),
        )]);
        let mut r = ImportReport::default();
        r.observe(&parse_shard("ab34cd56ef78", Layout::ShardedBranch, &good));
        r.observe(&parse_shard(
            "01JQZ8T9WQKX3M7VN2P4RB6CDE",
            Layout::PerCheckpointRefs,
            &good,
        ));
        r.observe(&parse_shard(
            "cd34cd56ef78",
            Layout::ShardedBranch,
            &BTreeMap::new(),
        ));
        assert_eq!(r.importable, 2);
        assert_eq!(r.refused, 1);
        assert_eq!(r.seen.get("sharded-branch"), Some(&2));
        assert_eq!(r.seen.get("per-checkpoint-refs"), Some(&1));
        let text = r.render();
        assert!(text.contains("sharded-branch: 2"));
        assert!(text.contains("per-checkpoint-refs: 1"));
        assert!(text.contains("importable 2 · refused 1"));
    }

    /// Both event-kind spellings in this repository read **through the importer**,
    /// because the repository asserts both and a reader that picks one drops
    /// every shard that used the other. Found by a test that failed: the first
    /// version of the trust test wrote `session.start` (the spelling
    /// `rift_recall_types` documents and tests) and derived NOTHING, because
    /// `EventKind::from_token` only knows `session_start`.
    ///
    /// It is written through `rederive_root` rather than against a helper on
    /// purpose. The helper it used to call was a private copy of
    /// [`EventKind::from_wire_token`]; a test aimed at the copy passes whether or
    /// not the importer still uses one, which is exactly how a duplicate becomes
    /// a divergence unnoticed. Aimed at the import path, this reds the moment the
    /// importer reads kinds by any rule that is not the wire reader's.
    #[test]
    fn both_event_kind_spellings_in_this_repo_are_readable() {
        let report = format!(
            r#"{{"rule_pack_versions":["p@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        let shard_with_kind = |kind: &str| {
            parse_shard(
                "ab34cd56ef78",
                Layout::ShardedBranch,
                &artifacts(&[
                    ("redaction_report.json", &report),
                    (
                        "full.jsonl",
                        &format!(
                            r#"{{"session":"s1","turn_ordinal":0,"event_ordinal":0,"kind":"{kind}","verb":"V","occurred_at_ms":1,"payload_sha256":"{}"}}"#,
                            "b".repeat(64)
                        ),
                    ),
                ]),
            )
        };
        for (dotted, under) in [
            ("session.start", "session_start"),
            ("tool.call", "tool_call"),
            ("session.end", "session_end"),
        ] {
            let d = rederive_root(&shard_with_kind(dotted));
            assert!(
                d.is_some(),
                "{dotted} derived nothing — the dialect dropped"
            );
            assert_eq!(
                d,
                rederive_root(&shard_with_kind(under)),
                "{dotted} and {under} are one kind, so they derive one root"
            );
        }
        // Not a blanket alias: an unknown token still derives nothing.
        assert!(rederive_root(&shard_with_kind("not.a.kind")).is_none());
    }

    /// The re-derive-before-trusting posture, exercised on all four outcomes.
    #[test]
    fn a_root_is_re_derived_before_it_is_trusted_and_never_promoted() {
        let report = format!(
            r#"{{"rule_pack_versions":["p@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        let event = |sess: &str| {
            format!(
                r#"{{"session":"{sess}","turn_ordinal":0,"event_ordinal":0,"kind":"session.start","verb":"SessionStart","occurred_at_ms":1,"payload_sha256":"{}"}}"#,
                "b".repeat(64)
            )
        };

        // No events at all → nothing could be re-derived.
        let none = parse_shard(
            "ab34cd56ef78",
            Layout::ShardedBranch,
            &artifacts(&[("redaction_report.json", &report)]),
        );
        assert_eq!(root_check(&none), RootCheck::NotDerivable);
        assert!(rederive_root(&none).is_none());

        // Events but no claimed root → nothing to check against.
        let unclaimed = parse_shard(
            "ab34cd56ef78",
            Layout::ShardedBranch,
            &artifacts(&[
                ("redaction_report.json", &report),
                ("full.jsonl", &event("s1")),
            ]),
        );
        assert_eq!(root_check(&unclaimed), RootCheck::Unclaimed);
        let derived = rederive_root(&unclaimed).expect("one event derives a root");

        // Claiming exactly what the events derive → self-consistent.
        let meta_ok = format!(r#"{{"merkle_root":"{}"}}"#, derived.to_hex());
        let ok = parse_shard(
            "ab34cd56ef78",
            Layout::ShardedBranch,
            &artifacts(&[
                ("redaction_report.json", &report),
                ("full.jsonl", &event("s1")),
                ("metadata.json", &meta_ok),
            ]),
        );
        assert_eq!(root_check(&ok), RootCheck::SelfConsistent);

        // Same header, ONE event byte changed → the claim no longer derives.
        let tampered = parse_shard(
            "ab34cd56ef78",
            Layout::ShardedBranch,
            &artifacts(&[
                ("redaction_report.json", &report),
                ("full.jsonl", &event("s2")),
                ("metadata.json", &meta_ok),
            ]),
        );
        assert_eq!(root_check(&tampered), RootCheck::Mismatch);

        // And self-consistency buys NOTHING beyond the ordinary import class.
        assert_eq!(IMPORT_TRUST, Trust::GitFact);
    }

    /// The durable-row builder carries the shard's own facts
    /// verbatim — layout, claimed root, and the re-derivation outcome — and is
    /// deterministic in the caller's clock, never its own.
    #[test]
    fn imported_checkpoint_record_carries_the_shards_own_facts() {
        let report = format!(
            r#"{{"rule_pack_versions":["p@1"],"input_sha256":"{}"}}"#,
            "a".repeat(64)
        );
        let event = format!(
            r#"{{"session":"s1","turn_ordinal":0,"event_ordinal":0,"kind":"session.start","verb":"SessionStart","occurred_at_ms":1,"payload_sha256":"{}"}}"#,
            "b".repeat(64)
        );
        let unclaimed = parse_shard(
            "ab34cd56ef78",
            Layout::ShardedBranch,
            &artifacts(&[("redaction_report.json", &report), ("full.jsonl", &event)]),
        );
        let r = imported_checkpoint_record(&unclaimed, 1_700_000_000);
        assert_eq!(r.source_id, "ab34cd56ef78");
        assert_eq!(r.source_layout, "sharded-branch");
        assert_eq!(r.source_root_claim, None);
        assert_eq!(r.root_check, "unclaimed");
        assert_eq!(r.imported_at, 1_700_000_000);
        assert_eq!(r.schema_version, 1);
        // Deterministic: same shard, same clock reading, same import_id.
        assert_eq!(
            r.import_id,
            imported_checkpoint_record(&unclaimed, 1_700_000_000).import_id
        );
        // The claimed root, when present, rides through untouched.
        let claimed = parse_shard(
            "ab34cd56ef78",
            Layout::PerCheckpointRefs,
            &artifacts(&[
                ("redaction_report.json", &report),
                ("full.jsonl", &event),
                (
                    "metadata.json",
                    &format!(r#"{{"merkle_root":"{}"}}"#, "c".repeat(64)),
                ),
            ]),
        );
        let claimed_row = imported_checkpoint_record(&claimed, 1);
        assert_eq!(claimed_row.source_layout, "per-checkpoint-refs");
        assert_eq!(
            claimed_row.source_root_claim.as_deref(),
            Some("c".repeat(64)).as_deref()
        );
        // A different layout for the SAME source id is a different import_id
        // (layout is part of identity — see `ids::imported_checkpoint_id`).
        assert_ne!(r.import_id, claimed_row.import_id);
    }

    /// The report says out loud, on every run that imports anything, that an
    /// imported checkpoint is not a chain anchor. A number without that sentence
    /// is the misreading this whole trust layer exists to prevent.
    #[test]
    fn the_report_refuses_to_let_self_consistent_read_as_verified() {
        let good = artifacts(&[(
            "redaction_report.json",
            &format!(
                r#"{{"rule_pack_versions":["p@1"],"input_sha256":"{}"}}"#,
                "a".repeat(64)
            ),
        )]);
        let mut r = ImportReport::default();
        r.observe(&parse_shard("ab34cd56ef78", Layout::ShardedBranch, &good));
        let text = r.render();
        assert!(text.contains("root-check not-derivable: 1"), "{text}");
        assert!(text.contains("claims, not chain anchors"), "{text}");

        // A run that imported nothing makes no claim either way.
        let empty = ImportReport::default().render();
        assert!(!empty.contains("chain anchors"), "{empty}");
    }

    proptest::proptest! {
        /// The ported brand guard. The wrapper protected the boundary by
        /// rewriting the child's bytes; a reader protects it by never letting a
        /// foreign string reach the report at all — so the property is stronger
        /// here: for arbitrary shard input the rendered report is brand-free.
        ///
        /// `(?s)` and the explicit `\n` in the alphabet are deliberate: a
        /// generator that cannot produce a newline cannot reach a multi-line
        /// report, which is the whole shape under test (ci/proptest-newline-baseline.txt).
        #[test]
        fn a_rendered_report_never_leaks_the_brand(
            id in "(?s)[\\x20-\\x7e\\n]{0,40}",
            meta in "(?s).{0,120}",
            full in "(?s).{0,120}",
        ) {
            let a: BTreeMap<String, Vec<u8>> = [
                ("metadata.json".to_owned(), meta.as_bytes().to_vec()),
                ("full.jsonl".to_owned(), full.as_bytes().to_vec()),
            ]
            .into_iter()
            .collect();
            let mut r = ImportReport::default();
            for layout in [Layout::ShardedBranch, Layout::PerCheckpointRefs] {
                r.observe(&parse_shard(&id, layout, &a));
            }
            let lower = r.render().to_ascii_lowercase();
            for t in FORBIDDEN_BRAND_TOKENS {
                proptest::prop_assert!(!lower.contains(t), "brand leaked: {}", lower);
            }
        }

        /// Classification is total and fail-closed: an arbitrary ref name either
        /// classifies to a well-formed source or to nothing, and a classified
        /// per-checkpoint source always carries a valid ULID.
        ///
        /// The generator is **shaped**, not free. A uniform `[\x20-\x7e/]{0,80}`
        /// draw never assembles the retired per-checkpoint prefix in any
        /// realistic number of cases, so it reaches the classifier's interesting
        /// branch never — spelling is not assembly. This draw emits the real
        /// prefix and varies only the two segments that decide the answer, so
        /// deleting either clause of the guard is reachable in a handful of cases.
        #[test]
        fn ref_classification_is_total_and_fail_closed(
            shard in "(?s)[0-9A-Z]{0,4}",
            tail in "(?s)[0-9A-Z-]{0,28}",
            junk in "(?s)[\\x20-\\x7e/\\n]{0,40}",
        ) {
            for r in [
                format!("refs/{RETIRED_TOKEN}/checkpoints/{shard}/{tail}"),
                format!("refs/{RETIRED_TOKEN}/checkpoints/{tail}/{tail}"),
                format!("refs/{RETIRED_TOKEN}/checkpoints/{tail}"),
                junk.clone(),
            ] {
                match classify_ref(&r) {
                    None => {}
                    Some((Layout::ShardedBranch, s)) => {
                        proptest::prop_assert_eq!(s, Source::Branch);
                    }
                    Some((Layout::PerCheckpointRefs, Source::Checkpoint(got))) => {
                        proptest::prop_assert_eq!(id_kind(&got), Some(IdKind::Ulid), "{}", r);
                    }
                    Some(other) => proptest::prop_assert!(false, "impossible pair: {:?}", other),
                }
            }
        }
    }
}
