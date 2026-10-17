//! Checked repository recovery-audit facts, independent of SQL and archives.
//! These facts describe missing observations, never historical authorship or
//! permission to retire work. A carrier digest needs archive validation above
//! this pure layer; subject digests do not create native session membership.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::{RepoId, Result, RiftError, Sha256Id};

/// The only supported gap schema.
pub const SCHEMA_VERSION: u32 = 1;
/// Maximum encoded or decoded artifact size, including JSONL framing.
pub const MAX_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;
/// Maximum exact physical ticket membership in one batch.
pub const MAX_TICKETS: usize = 8192;

fn invalid() -> RiftError {
    RiftError::validity("boundary gap: unsupported or inconsistent format")
}

macro_rules! digest_type {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $name(Sha256Id);
        impl Ord for $name {
            fn cmp(&self, other: &Self) -> std::cmp::Ordering {
                self.0.as_bytes().cmp(other.0.as_bytes())
            }
        }
        impl PartialOrd for $name {
            fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
                Some(self.cmp(other))
            }
        }
        impl $name {
            /// Parse exactly 64 lowercase hexadecimal characters.
            ///
            /// # Errors
            /// Refuses every other spelling without retaining rejected input.
            pub fn parse_hex(value: &str) -> Result<Self> {
                if value.len() != 64
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(invalid());
                }
                Sha256Id::parse_hex(value).map(Self).map_err(|_| invalid())
            }
            /// The canonical lowercase hexadecimal spelling.
            #[must_use]
            pub fn to_hex(self) -> String {
                self.0.to_hex()
            }
        }
    };
}

digest_type!(
    GapId,
    "Content identity of a carrier-independent gap batch."
);
digest_type!(
    SourceKey,
    "Commitment to one original physical source identity."
);
digest_type!(
    ContextDigest,
    "SHA256 commitment to the complete attempts-excluded context."
);
digest_type!(
    TicketDigest,
    "SHA256 commitment to original exact ticket bytes."
);
digest_type!(
    EvidenceDigest,
    "SHA256 commitment to the private preservation manifest."
);
digest_type!(
    SelectionDigest,
    "SHA256 commitment to sorted exact safe membership."
);
digest_type!(
    MemberKey,
    "Commitment to the original canonical member identity."
);
digest_type!(
    SessionKey,
    "Checked capture session-row digest, not a native session name."
);

/// The boundary event whose bytes were not captured at event time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BoundaryEdge {
    /// Beginning of a turn.
    Start,
    /// End of a turn.
    End,
}

impl BoundaryEdge {
    /// Closed canonical and storage spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::End => "end",
        }
    }
    /// Read a closed boundary spelling.
    ///
    /// # Errors
    /// Refuses unsupported spellings without retaining input.
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "start" => Ok(Self::Start),
            "end" => Ok(Self::End),
            _ => Err(invalid()),
        }
    }
}

/// The administrative reason for this missing-observation assertion.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum GapReason {
    /// Detached backlog supplies no immutable historical observation proof.
    #[serde(rename = "uncaptured-backlog")]
    UncapturedBacklog,
}

impl GapReason {
    /// Closed canonical and storage spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        "uncaptured-backlog"
    }
}

/// Named inputs to the single checked ticket constructor.
#[derive(Clone, Debug)]
pub struct GapTicketFields {
    /// Original physical source commitment.
    pub source_key: SourceKey,
    /// Attempts-excluded complete context commitment.
    pub context_digest: ContextDigest,
    /// Original exact ticket-byte commitment.
    pub ticket_digest: TicketDigest,
    /// Subject capture session-row digest.
    pub session_key: SessionKey,
    /// Journaled turn ordinal.
    pub turn_ordinal: u32,
    /// Journaled boundary edge.
    pub boundary: BoundaryEdge,
    /// Optional canonical lowercase Git object ID.
    pub head_oid: Option<String>,
    /// Journaled event time, checked nonnegative by the constructor.
    pub captured_at: i64,
    /// Original enqueue time, checked nonnegative by the constructor.
    pub queued_at: i64,
    /// Preserved original retry count.
    pub attempts: u32,
}

/// One checked physical ticket; all fields are structural identifiers/counts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GapTicket {
    source_key: SourceKey,
    context_digest: ContextDigest,
    ticket_digest: TicketDigest,
    session_key: SessionKey,
    turn_ordinal: u32,
    boundary: BoundaryEdge,
    head_oid: Option<String>,
    captured_at: i64,
    queued_at: i64,
    attempts: u32,
}

impl GapTicket {
    /// Check one member without making any event-time or ownership inference.
    ///
    /// # Errors
    /// Refuses negative times or a noncanonical optional Git object ID.
    pub fn new(fields: GapTicketFields) -> Result<Self> {
        let GapTicketFields {
            source_key,
            context_digest,
            ticket_digest,
            session_key,
            turn_ordinal,
            boundary,
            head_oid,
            captured_at,
            queued_at,
            attempts,
        } = fields;
        if captured_at < 0
            || queued_at < 0
            || head_oid.as_deref().is_some_and(|head| {
                head.len() != 40
                    || !head
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            })
        {
            return Err(invalid());
        }
        Ok(Self {
            source_key,
            context_digest,
            ticket_digest,
            session_key,
            turn_ordinal,
            boundary,
            head_oid,
            captured_at,
            queued_at,
            attempts,
        })
    }
    /// Original physical source commitment.
    #[must_use]
    pub const fn source_key(&self) -> SourceKey {
        self.source_key
    }
    /// Attempts-excluded full context commitment.
    #[must_use]
    pub const fn context_digest(&self) -> ContextDigest {
        self.context_digest
    }
    /// Exact preserved bytes commitment.
    #[must_use]
    pub const fn ticket_digest(&self) -> TicketDigest {
        self.ticket_digest
    }
    /// Subject session-row digest; not carrier membership.
    #[must_use]
    pub const fn session_key(&self) -> SessionKey {
        self.session_key
    }
    /// Event ordinal.
    #[must_use]
    pub const fn turn_ordinal(&self) -> u32 {
        self.turn_ordinal
    }
    /// Event boundary.
    #[must_use]
    pub const fn boundary(&self) -> BoundaryEdge {
        self.boundary
    }
    /// Optional canonical event HEAD.
    #[must_use]
    pub fn head_oid(&self) -> Option<&str> {
        self.head_oid.as_deref()
    }
    /// Journaled event time, not collection time.
    #[must_use]
    pub const fn captured_at(&self) -> i64 {
        self.captured_at
    }
    /// Original enqueue time.
    #[must_use]
    pub const fn queued_at(&self) -> i64 {
        self.queued_at
    }
    /// Preserved retry count, part of exact member equality.
    #[must_use]
    pub const fn attempts(&self) -> u32 {
        self.attempts
    }
}

/// Complete checked batch; its identity excludes carrier and recorded time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GapBatch {
    gap_id: GapId,
    repo_id: RepoId,
    member_key: MemberKey,
    selection_digest: SelectionDigest,
    evidence_digest: EvidenceDigest,
    recorded_at: i64,
    tickets: Vec<GapTicket>,
    ticket_count: u32,
}

impl GapBatch {
    /// Sort and check exact membership, then derive its two identities.
    ///
    /// # Errors
    /// Refuses empty/over-bound/duplicate membership or negative recorded time.
    pub fn new(
        repo_id: RepoId,
        member_key: MemberKey,
        evidence_digest: EvidenceDigest,
        recorded_at: i64,
        mut tickets: Vec<GapTicket>,
    ) -> Result<Self> {
        if recorded_at < 0 || tickets.is_empty() || tickets.len() > MAX_TICKETS {
            return Err(invalid());
        }
        let ticket_count = u32::try_from(tickets.len()).map_err(|_| invalid())?;
        tickets.sort_by_key(GapTicket::source_key);
        if tickets
            .windows(2)
            .any(|pair| matches!(pair, [a, b] if a.source_key == b.source_key))
        {
            return Err(invalid());
        }
        let wire: Vec<_> = tickets.iter().map(TicketWire::from).collect();
        let bytes = serde_json::to_vec(&wire).map_err(|_| invalid())?;
        let selection_digest = SelectionDigest(hash(b"rift-boundary-gap-selection@1\0", &bytes));
        let identity = IdentityWire {
            evidence_sha256: evidence_digest.to_hex(),
            member_key: member_key.to_hex(),
            reason: GapReason::UncapturedBacklog,
            repo_id: repo_id.as_str(),
            schema_version: SCHEMA_VERSION,
            selection_sha256: selection_digest.to_hex(),
        };
        let gap_id = GapId(hash(
            b"rift-boundary-gap@1\0",
            &serde_json::to_vec(&identity).map_err(|_| invalid())?,
        ));
        Ok(Self {
            gap_id,
            repo_id,
            member_key,
            selection_digest,
            evidence_digest,
            recorded_at,
            tickets,
            ticket_count,
        })
    }
    /// Verify separately stored identity/count against the checked full set.
    ///
    /// # Errors
    /// Refuses any inconsistent commitment or count.
    pub fn verify_stored(&self, id: GapId, selection: SelectionDigest, count: u32) -> Result<()> {
        if self.gap_id != id || self.selection_digest != selection || self.ticket_count() != count {
            return Err(invalid());
        }
        Ok(())
    }
    /// Carrier-independent batch identity.
    #[must_use]
    pub const fn gap_id(&self) -> GapId {
        self.gap_id
    }
    /// Original repository identity.
    #[must_use]
    pub const fn repo_id(&self) -> &RepoId {
        &self.repo_id
    }
    /// Original member commitment, never a destination executable path.
    #[must_use]
    pub const fn member_key(&self) -> MemberKey {
        self.member_key
    }
    /// Exact sorted membership commitment.
    #[must_use]
    pub const fn selection_digest(&self) -> SelectionDigest {
        self.selection_digest
    }
    /// Private evidence manifest commitment, not possession proof.
    #[must_use]
    pub const fn evidence_digest(&self) -> EvidenceDigest {
        self.evidence_digest
    }
    /// Original administrative decision time; replay must retain it.
    #[must_use]
    pub const fn recorded_at(&self) -> i64 {
        self.recorded_at
    }
    /// The sole supported administrative reason.
    #[must_use]
    pub const fn reason(&self) -> GapReason {
        GapReason::UncapturedBacklog
    }
    /// Checked count, bounded by MAX_TICKETS before construction.
    #[must_use]
    pub fn ticket_count(&self) -> u32 {
        self.ticket_count
    }
    /// Exact safe members in source-key order.
    #[must_use]
    pub fn tickets(&self) -> &[GapTicket] {
        &self.tickets
    }
}

/// Structural audit carrier; this type alone does not prove archive membership.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GapEnvelope {
    carrier_session_key: SessionKey,
    batch: GapBatch,
}

impl GapEnvelope {
    /// Associate checked facts with a carrier digest. Archive validation is
    /// required above this layer before publication or import.
    #[must_use]
    pub const fn new(carrier_session_key: SessionKey, batch: GapBatch) -> Self {
        Self {
            carrier_session_key,
            batch,
        }
    }
    /// The proposed genuine archive carrier, not the subject sessions.
    #[must_use]
    pub const fn carrier_session_key(&self) -> SessionKey {
        self.carrier_session_key
    }
    /// Carrier-independent checked facts.
    #[must_use]
    pub const fn batch(&self) -> &GapBatch {
        &self.batch
    }
}

fn hash(domain: &[u8], bytes: &[u8]) -> Sha256Id {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(bytes);
    Sha256Id::from_bytes(hash.finalize().into())
}

// Field declarations deliberately follow lexicographic key order. Struct
// serialization does not depend on serde_json map feature unification.
#[derive(Serialize)]
struct IdentityWire<'a> {
    evidence_sha256: String,
    member_key: String,
    reason: GapReason,
    repo_id: &'a str,
    schema_version: u32,
    selection_sha256: String,
}

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct RequiredHead(Option<String>);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketWire {
    attempts: u32,
    boundary: BoundaryEdge,
    captured_at: i64,
    context_sha256: String,
    head_oid: RequiredHead,
    queued_at: i64,
    schema_version: u32,
    session_key: String,
    source_key: String,
    ticket_sha256: String,
    turn_ordinal: u32,
}

impl From<&GapTicket> for TicketWire {
    fn from(ticket: &GapTicket) -> Self {
        Self {
            attempts: ticket.attempts,
            boundary: ticket.boundary,
            captured_at: ticket.captured_at,
            context_sha256: ticket.context_digest.to_hex(),
            head_oid: RequiredHead(ticket.head_oid.clone()),
            queued_at: ticket.queued_at,
            schema_version: SCHEMA_VERSION,
            session_key: ticket.session_key.to_hex(),
            source_key: ticket.source_key.to_hex(),
            ticket_sha256: ticket.ticket_digest.to_hex(),
            turn_ordinal: ticket.turn_ordinal,
        }
    }
}

impl TicketWire {
    fn checked(self) -> Result<GapTicket> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid());
        }
        GapTicket::new(GapTicketFields {
            source_key: SourceKey::parse_hex(&self.source_key)?,
            context_digest: ContextDigest::parse_hex(&self.context_sha256)?,
            ticket_digest: TicketDigest::parse_hex(&self.ticket_sha256)?,
            session_key: SessionKey::parse_hex(&self.session_key)?,
            turn_ordinal: self.turn_ordinal,
            boundary: self.boundary,
            head_oid: self.head_oid.0,
            captured_at: self.captured_at,
            queued_at: self.queued_at,
            attempts: self.attempts,
        })
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchWire {
    evidence_sha256: String,
    gap_id: String,
    member_key: String,
    reason: GapReason,
    recorded_at: i64,
    repo_id: String,
    schema_version: u32,
    selection_sha256: String,
    ticket_count: u32,
    tickets: Vec<TicketWire>,
}

impl From<&GapBatch> for BatchWire {
    fn from(batch: &GapBatch) -> Self {
        Self {
            evidence_sha256: batch.evidence_digest.to_hex(),
            gap_id: batch.gap_id.to_hex(),
            member_key: batch.member_key.to_hex(),
            reason: batch.reason(),
            recorded_at: batch.recorded_at,
            repo_id: batch.repo_id.as_str().to_owned(),
            schema_version: SCHEMA_VERSION,
            selection_sha256: batch.selection_digest.to_hex(),
            ticket_count: batch.ticket_count(),
            tickets: batch.tickets.iter().map(TicketWire::from).collect(),
        }
    }
}

impl BatchWire {
    fn checked(self) -> Result<GapBatch> {
        if self
            .tickets
            .windows(2)
            .any(|pair| matches!(pair, [a, b] if a.source_key >= b.source_key))
        {
            return Err(invalid());
        }
        if self.schema_version != SCHEMA_VERSION {
            return Err(invalid());
        }
        let batch = GapBatch::new(
            RepoId::new(self.repo_id).map_err(|_| invalid())?,
            MemberKey::parse_hex(&self.member_key)?,
            EvidenceDigest::parse_hex(&self.evidence_sha256)?,
            self.recorded_at,
            self.tickets
                .into_iter()
                .map(TicketWire::checked)
                .collect::<Result<_>>()?,
        )?;
        batch.verify_stored(
            GapId::parse_hex(&self.gap_id)?,
            SelectionDigest::parse_hex(&self.selection_sha256)?,
            self.ticket_count,
        )?;
        Ok(batch)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeWire {
    batch: BatchWire,
    carrier_session_key: String,
    schema_version: u32,
}

/// Render canonical sorted JSONL from checked facts only.
///
/// # Errors
/// Refuses empty, duplicate-batch or over-bound artifacts.
pub fn encode_canonical(envelopes: &[GapEnvelope]) -> Result<Vec<u8>> {
    if envelopes.is_empty() {
        return Err(invalid());
    }
    let mut ordered: Vec<_> = envelopes.iter().collect();
    ordered.sort_by_key(|envelope| envelope.batch.gap_id);
    let mut ids = BTreeSet::new();
    let mut sources = BTreeSet::new();
    let mut out = Vec::new();
    for envelope in ordered {
        if !ids.insert(envelope.batch.gap_id) {
            return Err(invalid());
        }
        add_sources(&envelope.batch, &mut sources)?;
        let wire = EnvelopeWire {
            batch: BatchWire::from(&envelope.batch),
            carrier_session_key: envelope.carrier_session_key.to_hex(),
            schema_version: SCHEMA_VERSION,
        };
        serde_json::to_writer(&mut out, &wire).map_err(|_| invalid())?;
        out.push(b'\n');
        if out.len() > MAX_ARTIFACT_BYTES {
            return Err(invalid());
        }
    }
    Ok(out)
}

/// Decode complete bounded JSONL with strict recursive structural validation.
/// Unknown fields/duplicates and trailing JSON refuse without retaining input
/// in errors. Absence is represented outside this codec, never an empty file.
///
/// # Errors
/// Refuses unsupported/inconsistent facts, framing, bounds or identities.
pub fn decode_checked(bytes: &[u8]) -> Result<Vec<GapEnvelope>> {
    if bytes.is_empty() || bytes.len() > MAX_ARTIFACT_BYTES {
        return Err(invalid());
    }
    let body = bytes.strip_suffix(b"\n").ok_or_else(invalid)?;
    let mut previous = None;
    let mut sources = BTreeSet::new();
    let mut out = Vec::new();
    for line in body.split(|b| *b == b'\n') {
        let wire: EnvelopeWire = serde_json::from_slice(line).map_err(|_| invalid())?;
        if wire.schema_version != SCHEMA_VERSION {
            return Err(invalid());
        }
        let batch = wire.batch.checked()?;
        if previous.is_some_and(|id| id >= batch.gap_id) {
            return Err(invalid());
        }
        previous = Some(batch.gap_id);
        add_sources(&batch, &mut sources)?;
        out.push(GapEnvelope::new(
            SessionKey::parse_hex(&wire.carrier_session_key)?,
            batch,
        ));
    }
    Ok(out)
}

fn add_sources(batch: &GapBatch, sources: &mut BTreeSet<SourceKey>) -> Result<()> {
    for ticket in &batch.tickets {
        if !sources.insert(ticket.source_key) {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
