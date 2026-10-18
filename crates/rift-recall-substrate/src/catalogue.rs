//! Read-only checkpoint catalogue and explicit-target resolution.
//!
//! This module is the shared authority used by inspection surfaces. It unions
//! duplicate ids without losing physical origins, reads metadata leniently,
//! records only factual chain positions, and gives checkpoint identity
//! precedence over Git revision parsing. It never creates refs, objects,
//! databases, checkouts, or network requests.

use std::collections::{BTreeMap, BTreeSet};

use rift_core::Result;
use rift_recall_types::checkpoint::CheckpointMetadata;

use crate::parse::{self, IssueKind, Locator};
use crate::{CheckpointOrigin, SubstrateReader};

/// Maximum target candidates retained for a notice. Resolution remains
/// deterministic when a malicious prefix or squash names more: `truncated`
/// says the list is incomplete.
pub const MAX_TARGET_CANDIDATES: usize = 256;
const MAX_TRAILER_LINES: usize = 4096;
const MAX_TRAILER_CLAIMS: usize = 256;

/// Provenance of the effective catalogue timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum TimestampProvenance {
    /// Recorded in checkpoint metadata.
    Recorded,
    /// Derived from the checkpoint's projected session rows.
    SessionDerived,
    /// The anchor commit's committer timestamp was the only factual time.
    AnchorCommit,
    /// No factual timestamp is available.
    Unknown,
}

/// A non-fatal problem attached to a checkpoint row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CatalogueIssue {
    /// Artifact or catalogue component affected.
    pub component: String,
    /// Safe category/detail; never artifact bytes.
    pub detail: String,
}

/// One logical checkpoint, unioned across every physical storage origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CheckpointCatalogueEntry {
    /// Durable checkpoint id.
    pub checkpoint_id: String,
    /// Every actual shared-branch or per-checkpoint-ref origin.
    pub origins: Vec<CheckpointOrigin>,
    /// Zero-based position in `refs/rift/chain/v1`, when present there.
    pub chain_position: Option<u64>,
    /// Lenient metadata, even when some fields are absent.
    pub metadata: CheckpointMetadata,
    /// Whether `metadata.json` existed at any readable origin.
    pub metadata_present: bool,
    /// Parsed recorded time, if valid.
    pub recorded_at_ms: Option<i64>,
    /// Captured/derived anchor commit oid, if available.
    pub anchor_commit: Option<String>,
    /// Anchor committer time, if the commit is locally readable.
    pub anchor_committed_ms: Option<i64>,
    /// Missing/corrupt/unsupported facts. The row remains selectable.
    pub issues: Vec<CatalogueIssue>,
}

impl CheckpointCatalogueEntry {
    /// Apply the one effective-time precedence used by all viewer sorts.
    /// `-1` is the projection's unknown sentinel and is never admitted as a
    /// date. Negative values from any source are treated as unavailable.
    #[must_use]
    pub fn effective_timestamp(
        &self,
        session_derived_ms: Option<i64>,
    ) -> (Option<i64>, TimestampProvenance) {
        if let Some(ms) = self.recorded_at_ms.filter(|ms| *ms >= 0) {
            return (Some(ms), TimestampProvenance::Recorded);
        }
        if let Some(ms) = session_derived_ms.filter(|ms| *ms >= 0) {
            return (Some(ms), TimestampProvenance::SessionDerived);
        }
        if let Some(ms) = self.anchor_committed_ms.filter(|ms| *ms >= 0) {
            return (Some(ms), TimestampProvenance::AnchorCommit);
        }
        (None, TimestampProvenance::Unknown)
    }
}

/// How an explicit target was interpreted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionKind {
    /// Exact durable checkpoint id.
    ExactCheckpoint,
    /// Unique durable checkpoint prefix.
    CheckpointPrefix,
    /// More than one durable checkpoint has the prefix.
    AmbiguousCheckpointPrefix,
    /// A local Git commit-ish matched metadata anchors and/or trailers.
    GitCommit,
    /// Neither checkpoint identity nor a local Git commit resolved.
    Missing,
}

/// Why one checkpoint is a candidate for a Git target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TargetCandidate {
    /// Candidate checkpoint id.
    pub checkpoint_id: String,
    /// Its metadata anchor equals the resolved commit.
    pub metadata_anchor: bool,
    /// A valid checkpoint trailer in the resolved commit named it.
    pub trailer: bool,
}

/// Bounded result of resolving an explicit viewer/explain target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTarget {
    /// Interpretation taken after checkpoint-first precedence.
    pub kind: ResolutionKind,
    /// Candidate checkpoints. The caller applies its timestamp selection rule.
    pub candidates: Vec<TargetCandidate>,
    /// Resolved local commit oid for [`ResolutionKind::GitCommit`].
    pub commit_oid: Option<String>,
    /// Invalid checkpoint-trailer lines encountered; values are never retained.
    pub rejected_trailers: usize,
    /// Candidate/trailer bounds cut the result short.
    pub truncated: bool,
}

impl ResolvedTarget {
    fn checkpoint(kind: ResolutionKind, ids: impl IntoIterator<Item = String>) -> Self {
        let mut truncated = false;
        let candidates = ids
            .into_iter()
            .take(MAX_TARGET_CANDIDATES + 1)
            .enumerate()
            .filter_map(|(index, checkpoint_id)| {
                if index == MAX_TARGET_CANDIDATES {
                    truncated = true;
                    None
                } else {
                    Some(TargetCandidate {
                        checkpoint_id,
                        metadata_anchor: false,
                        trailer: false,
                    })
                }
            })
            .collect();
        Self {
            kind,
            candidates,
            commit_oid: None,
            rejected_trailers: 0,
            truncated,
        }
    }

    fn missing() -> Self {
        Self::checkpoint(ResolutionKind::Missing, std::iter::empty())
    }
}

/// Build the storage-truth checkpoint catalogue. Corrupt and partial entries
/// remain in the returned vector with precise issues.
///
/// # Errors
/// Only repository-level enumeration/read errors are fatal.
pub fn build_checkpoint_catalogue(
    reader: &impl SubstrateReader,
) -> Result<Vec<CheckpointCatalogueEntry>> {
    let mut by_id: BTreeMap<String, Vec<CheckpointOrigin>> = BTreeMap::new();
    for origin in reader.checkpoint_origins()? {
        by_id
            .entry(origin.checkpoint_id.clone())
            .or_default()
            .push(origin);
    }
    // A custom/older reader may enumerate shards but not origins despite the
    // default contract. Keep such shards visible rather than dropping them.
    for shard in reader.checkpoints()? {
        by_id.entry(shard.checkpoint_id.clone()).or_default();
    }

    let mut chain_positions = BTreeMap::new();
    for (position, id) in reader.checkpoint_chain()?.into_iter().enumerate() {
        chain_positions.entry(id).or_insert(position as u64);
    }

    let mut out = Vec::with_capacity(by_id.len());
    for (checkpoint_id, mut origins) in by_id {
        origins.sort();
        origins.dedup();
        let mut issues = Vec::new();
        let metadata_artifact = reader.read_artifact(&checkpoint_id, "metadata.json")?;
        let (metadata, metadata_present) = if let Some(bytes) = metadata_artifact {
            let parsed = parse::parse_metadata(&bytes, &checkpoint_id);
            for issue in parsed.issues {
                issues.push(CatalogueIssue {
                    component: issue.artifact.to_owned(),
                    detail: format_parse_issue(&issue.locator, issue.kind, &issue.detail),
                });
            }
            (parsed.value, true)
        } else {
            issues.push(CatalogueIssue {
                component: "metadata.json".to_owned(),
                detail: "missing".to_owned(),
            });
            (CheckpointMetadata::default(), false)
        };
        let recorded_at_ms = metadata.created_at.as_deref().and_then(parse_rfc3339_ms);
        if metadata.created_at.is_some() && recorded_at_ms.is_none() {
            issues.push(CatalogueIssue {
                component: "metadata.created_at".to_owned(),
                detail: "invalid RFC3339 UTC timestamp".to_owned(),
            });
        }
        let anchor_commit = metadata
            .commit
            .clone()
            .or(reader.checkpoint_commit(&checkpoint_id)?);
        let anchor_committed_ms = match anchor_commit.as_deref() {
            Some(oid) => reader.commit_meta(oid)?.map(|meta| meta.committed_ms),
            None => None,
        };
        if origins.is_empty() {
            issues.push(CatalogueIssue {
                component: "storage origin".to_owned(),
                detail: "enumerated without a readable ref origin".to_owned(),
            });
        }
        out.push(CheckpointCatalogueEntry {
            checkpoint_id: checkpoint_id.clone(),
            origins,
            chain_position: chain_positions.get(&checkpoint_id).copied(),
            metadata,
            metadata_present,
            recorded_at_ms,
            anchor_commit,
            anchor_committed_ms,
            issues,
        });
    }
    Ok(out)
}

fn format_parse_issue(locator: &Locator, kind: IssueKind, detail: &str) -> String {
    let at = match locator {
        Locator::WholeFile => "whole artifact".to_owned(),
        Locator::Line(line) => format!("line {line}"),
    };
    let category = match kind {
        IssueKind::Corrupt => "corrupt",
        IssueKind::OversizeSkipped => "oversized",
        IssueKind::TokenUnresolved => "token unresolved",
    };
    format!("{category} at {at}: {detail}")
}

/// Resolve a user-supplied target with checkpoint identity before local Git
/// revision semantics. The function returns all Git candidates; the viewer and
/// explain choose among them using their own documented timestamp behavior.
///
/// # Errors
/// Propagates reader failures. Invalid/missing targets are data, not errors.
pub fn resolve_explicit_target(
    reader: &impl SubstrateReader,
    catalogue: &[CheckpointCatalogueEntry],
    target: &str,
) -> Result<ResolvedTarget> {
    if target.is_empty() {
        return Ok(ResolvedTarget::missing());
    }
    if let Some(resolved) = checkpoint_identity_target(catalogue, target) {
        return Ok(resolved);
    }
    if target.starts_with('-') {
        return Ok(ResolvedTarget::missing());
    }
    let Some(commit_oid) = reader.resolve_commitish(target)? else {
        return Ok(ResolvedTarget::missing());
    };
    let claims = match reader.commit_message(&commit_oid)? {
        Some(message) => checkpoint_trailers(&message),
        None => TrailerScan::default(),
    };
    Ok(git_commit_target(catalogue, commit_oid, claims))
}

/// The checkpoint-first half of resolution: an exact durable id wins, then a
/// unique prefix, then an ambiguous one. `None` hands the target on to Git
/// revision parsing, which a checkpoint id must always shadow.
fn checkpoint_identity_target(
    catalogue: &[CheckpointCatalogueEntry],
    target: &str,
) -> Option<ResolvedTarget> {
    if let Some(exact) = catalogue.iter().find(|entry| entry.checkpoint_id == target) {
        return Some(ResolvedTarget::checkpoint(
            ResolutionKind::ExactCheckpoint,
            [exact.checkpoint_id.clone()],
        ));
    }
    let prefixes: Vec<String> = catalogue
        .iter()
        .filter(|entry| entry.checkpoint_id.starts_with(target))
        .map(|entry| entry.checkpoint_id.clone())
        .collect();
    match prefixes.as_slice() {
        [one] => Some(ResolvedTarget::checkpoint(
            ResolutionKind::CheckpointPrefix,
            [one.clone()],
        )),
        [_, _, ..] => Some(ResolvedTarget::checkpoint(
            ResolutionKind::AmbiguousCheckpointPrefix,
            prefixes,
        )),
        [] => None,
    }
}

/// The Git half of resolution, over facts the reader already produced: every
/// checkpoint anchored at the commit, joined with every known checkpoint its
/// trailers claim, cut to the candidate bound. Kept free of the reader so the
/// only repository reads stay in [`resolve_explicit_target`].
fn git_commit_target(
    catalogue: &[CheckpointCatalogueEntry],
    commit_oid: String,
    claims: TrailerScan,
) -> ResolvedTarget {
    let mut matches = anchor_matches(catalogue, &commit_oid);
    add_trailer_matches(&mut matches, catalogue, claims.ids);
    let mut truncated = claims.truncated;
    let mut candidates: Vec<TargetCandidate> = matches.into_values().collect();
    if candidates.len() > MAX_TARGET_CANDIDATES {
        candidates.truncate(MAX_TARGET_CANDIDATES);
        truncated = true;
    }
    ResolvedTarget {
        kind: ResolutionKind::GitCommit,
        candidates,
        commit_oid: Some(commit_oid),
        rejected_trailers: claims.rejected,
        truncated,
    }
}

/// Checkpoints whose metadata anchor is the resolved commit, keyed by id so
/// trailer claims can join them.
fn anchor_matches(
    catalogue: &[CheckpointCatalogueEntry],
    commit_oid: &str,
) -> BTreeMap<String, TargetCandidate> {
    let mut matches: BTreeMap<String, TargetCandidate> = BTreeMap::new();
    for entry in catalogue {
        if entry.anchor_commit.as_deref() == Some(commit_oid) {
            matches.insert(
                entry.checkpoint_id.clone(),
                TargetCandidate {
                    checkpoint_id: entry.checkpoint_id.clone(),
                    metadata_anchor: true,
                    trailer: false,
                },
            );
        }
    }
    matches
}

/// Join the commit's trailer claims into the anchor matches. A claim naming a
/// checkpoint the catalogue does not hold is dropped: a commit message cannot
/// conjure a checkpoint into existence.
fn add_trailer_matches(
    matches: &mut BTreeMap<String, TargetCandidate>,
    catalogue: &[CheckpointCatalogueEntry],
    claimed_ids: Vec<String>,
) {
    let known: BTreeSet<&str> = catalogue
        .iter()
        .map(|entry| entry.checkpoint_id.as_str())
        .collect();
    for id in claimed_ids {
        if !known.contains(id.as_str()) {
            continue;
        }
        matches
            .entry(id.clone())
            .and_modify(|candidate| candidate.trailer = true)
            .or_insert(TargetCandidate {
                checkpoint_id: id,
                metadata_anchor: false,
                trailer: true,
            });
    }
}

#[derive(Default)]
struct TrailerScan {
    ids: Vec<String>,
    rejected: usize,
    truncated: bool,
}

fn checkpoint_trailers(message: &str) -> TrailerScan {
    let mut ids = Vec::new();
    let mut rejected = 0usize;
    let mut truncated = false;
    for (line_index, line) in message.lines().enumerate() {
        if line_index >= MAX_TRAILER_LINES {
            truncated = true;
            break;
        }
        let line = line.trim_start();
        let rest = ["Rift-Checkpoint:", concat!("Ent", "ire-Checkpoint:")]
            .into_iter()
            .find_map(|prefix| line.strip_prefix(prefix));
        let Some(rest) = rest else {
            continue;
        };
        let value = rest.split_whitespace().next().unwrap_or("");
        if !(7..=64).contains(&value.len())
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            rejected += 1;
            continue;
        }
        if ids.iter().any(|id| id == value) {
            continue;
        }
        if ids.len() >= MAX_TRAILER_CLAIMS {
            truncated = true;
            break;
        }
        ids.push(value.to_owned());
    }
    TrailerScan {
        ids,
        rejected,
        truncated,
    }
}

/// Parse the strict UTC RFC3339 subset written by checkpoint producers:
/// `YYYY-MM-DDTHH:MM:SS[.fraction]Z`. Fractional precision beyond milliseconds
/// is truncated, never rounded. Invalid calendar dates are refused.
#[must_use]
pub fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || bytes.get(10) != Some(&b'T')
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
        || !text.ends_with('Z')
    {
        return None;
    }
    let number = |start: usize, end: usize| text.get(start..end)?.parse::<i64>().ok();
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    if !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [
        31i64,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let max_day = month_days.get(usize::try_from(month - 1).ok()?).copied()?;
    if day < 1 || day > max_day {
        return None;
    }
    let fraction = text.get(19..text.len().saturating_sub(1))?;
    let millis = if fraction.is_empty() {
        0
    } else {
        let digits = fraction.strip_prefix('.')?;
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        let mut value = 0i64;
        for byte in digits.bytes().take(3) {
            value = value * 10 + i64::from(byte - b'0');
        }
        for _ in digits.len().min(3)..3 {
            value *= 10;
        }
        value
    };

    // Days from civil (Howard Hinnant), proleptic Gregorian calendar.
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = if shifted >= 0 { shifted } else { shifted - 399 } / 400;
    let year_of_era = shifted - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 1000 + millis)
}

#[cfg(test)]
mod tests {
    use rift_recall_types::checkpoint::CheckpointMetadata;

    use super::*;
    use crate::{
        CheckpointOrigin, CheckpointShard, CheckpointStorageKind, CommitMeta, Dialect,
        MemorySubstrate,
    };

    fn shard(id: &str, ordinal: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.to_owned(),
            dialect: Dialect::Rift,
            prev: None,
            ordinal,
        }
    }

    #[test]
    fn catalogue_unions_origins_and_keeps_partial_rows() {
        let id = "a".repeat(64);
        let reader = MemorySubstrate::local()
            .with_shard(shard(&id, 1))
            .with_origin(CheckpointOrigin {
                checkpoint_id: id.clone(),
                dialect: Dialect::Rift,
                storage_ref: Dialect::Rift.checkpoint_ref().to_owned(),
                kind: CheckpointStorageKind::Branch,
                tip_oid: Some("1".repeat(40)),
            })
            .with_origin(CheckpointOrigin {
                checkpoint_id: id.clone(),
                dialect: Dialect::Legacy,
                storage_ref: concat!("refs/", "ent", "ire", "/checkpoints/aa/id").to_owned(),
                kind: CheckpointStorageKind::PerCheckpointRef,
                tip_oid: Some("2".repeat(40)),
            })
            .with_chain(&[&id]);
        let catalogue = build_checkpoint_catalogue(&reader).unwrap();
        assert_eq!(catalogue.len(), 1);
        assert_eq!(catalogue[0].origins.len(), 2);
        assert_eq!(catalogue[0].chain_position, Some(0));
        assert!(!catalogue[0].metadata_present);
        assert!(catalogue[0]
            .issues
            .iter()
            .any(|issue| issue.detail == "missing"));
    }

    /// A shard whose metadata names no `commit` — minted on an unborn HEAD —
    /// has no anchor and no anchor time, and its effective time is the one
    /// every native shard records (`created_at`), with `Recorded` provenance;
    /// the absence raises no catalogue issue.
    #[test]
    fn an_anchorless_shard_dates_by_its_recorded_time_with_no_issue() {
        let id = "b".repeat(64);
        let reader = MemorySubstrate::local()
            .with_shard(shard(&id, 1))
            .with_artifact(
                &id,
                "metadata.json",
                br#"{"canon_version":3,"strategy":"native","sessions":["s1"],"created_at":"2026-10-17T12:00:00.000Z"}"#,
            )
            .with_chain(&[&id]);
        let catalogue = build_checkpoint_catalogue(&reader).unwrap();
        assert_eq!(catalogue.len(), 1);
        let entry = &catalogue[0];
        assert_eq!(entry.anchor_commit, None);
        assert_eq!(entry.anchor_committed_ms, None);
        assert!(entry.issues.is_empty(), "{:?}", entry.issues);
        let (at, provenance) = entry.effective_timestamp(None);
        assert_eq!(provenance, TimestampProvenance::Recorded);
        assert_eq!(at, parse_rfc3339_ms("2026-10-17T12:00:00.000Z"));
    }

    #[test]
    fn timestamp_precedence_never_exposes_unknown_sentinel() {
        let mut entry = CheckpointCatalogueEntry {
            checkpoint_id: "c".into(),
            origins: Vec::new(),
            chain_position: None,
            metadata: CheckpointMetadata::default(),
            metadata_present: true,
            recorded_at_ms: Some(-1),
            anchor_commit: None,
            anchor_committed_ms: Some(30),
            issues: Vec::new(),
        };
        assert_eq!(
            entry.effective_timestamp(Some(20)),
            (Some(20), TimestampProvenance::SessionDerived)
        );
        entry.recorded_at_ms = Some(10);
        assert_eq!(
            entry.effective_timestamp(Some(20)),
            (Some(10), TimestampProvenance::Recorded)
        );
    }

    #[test]
    fn checkpoint_identity_wins_and_ambiguous_prefix_is_explicit() {
        let ids = [
            format!("abcdef0{}", "1".repeat(56)),
            format!("abcdef0{}", "2".repeat(56)),
        ];
        let reader = MemorySubstrate::local()
            .with_shard(shard(&ids[0], 1))
            .with_shard(shard(&ids[1], 2))
            .with_revision("abcdef0", "f".repeat(40).as_str());
        let catalogue = build_checkpoint_catalogue(&reader).unwrap();
        let resolved = resolve_explicit_target(&reader, &catalogue, "abcdef0").unwrap();
        assert_eq!(resolved.kind, ResolutionKind::AmbiguousCheckpointPrefix);
        assert_eq!(resolved.candidates.len(), 2);
    }

    #[test]
    fn git_target_unions_metadata_and_valid_trailers() {
        let anchor_id = "a".repeat(64);
        let trailer_id = "b".repeat(64);
        let oid = "c".repeat(40);
        let reader = MemorySubstrate::local()
            .with_shard(shard(&anchor_id, 1))
            .with_shard(shard(&trailer_id, 2))
            .with_artifact(
                &anchor_id,
                "metadata.json",
                format!(r#"{{"commit":"{oid}"}}"#).as_bytes(),
            )
            .with_revision("topic", &oid)
            .with_commit(CommitMeta {
                oid: oid.clone(),
                parents: Vec::new(),
                committed_ms: 10,
                summary: "subject".into(),
            })
            .with_commit_message(
                &oid,
                &format!("subject\n\nRift-Checkpoint: {trailer_id}\nRift-Checkpoint: BAD"),
            );
        let catalogue = build_checkpoint_catalogue(&reader).unwrap();
        let resolved = resolve_explicit_target(&reader, &catalogue, "topic").unwrap();
        assert_eq!(resolved.kind, ResolutionKind::GitCommit);
        assert_eq!(resolved.candidates.len(), 2);
        assert_eq!(resolved.rejected_trailers, 1);
        assert!(resolved.candidates.iter().any(|candidate| {
            candidate.checkpoint_id == anchor_id && candidate.metadata_anchor
        }));
        assert!(resolved
            .candidates
            .iter()
            .any(|candidate| candidate.checkpoint_id == trailer_id && candidate.trailer));
    }

    #[test]
    fn rfc3339_parser_checks_calendar_and_fraction() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00.1259Z"), Some(125));
        assert_eq!(parse_rfc3339_ms("2025-02-29T00:00:00Z"), None);
        assert!(parse_rfc3339_ms("2024-02-29T00:00:00Z").is_some());
        assert_eq!(parse_rfc3339_ms("2024-01-01T00:00:00+01:00"), None);
    }
}
