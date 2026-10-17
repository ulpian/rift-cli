//! Checked cache and canonical evidence in, a closed classification out; no
//! execution authority.
//!
//! One pass over the queued boundary jobs of an admitted member: the current
//! session cache is read under the recovery read cap and projected to its
//! semantic facts; for every candidate the cache says is current, at most
//! eleven guarded point queries fetch the identity, session, turn, witness
//! event, receipt and snapshot rows; the classifier is pure over what was read.
//! Any cap, I/O or budget failure makes the WHOLE pass unavailable — never a
//! partial or truncated answer — and nothing here selects, reserves, pins,
//! repairs or writes. Until the recovery controller lands, tests are the only
//! callers.

#[cfg(unix)]
mod cache;
#[cfg(all(test, unix))]
pub(in crate::capture_recovery) mod fixtures;
#[cfg(all(test, unix))]
mod query_tests;
mod rows;
#[cfg(all(test, unix))]
mod tests;

#[cfg(unix)]
use std::collections::BTreeMap;
use std::collections::BTreeSet;

use rift_core::event::{EventKind, Trust};
use rift_core::RepoId;
use rift_shadow::hooks::{AdapterRegistry, AgentId};
use rift_shadow::ids;
#[cfg(unix)]
use rift_store::Value;
use rift_store::{CanonicalStore, Row};

use super::hold::ReadySessions;
use super::plan;
use super::WireResult;
use crate::agent_queue::{BoundaryEdge, BoundarySnapshot};

/// Distinct `(owner, native)` cache keys one pass may read.
pub(crate) const MAX_CACHE_KEYS: usize = 256;
/// Distinct semantically-current candidate keys one pass may query for.
pub(crate) const MAX_CURRENT_CANDIDATES: usize = 128;
pub(crate) const MAX_CALLS: u32 = 2048;
pub(crate) const MAX_ROWS: u32 = 8192;
pub(crate) const MAX_CELL_BYTES: u64 = 32 * 1024 * 1024;
pub(crate) const MAX_CACHE_BYTES: u64 = 32 * 1024 * 1024;
/// Identity, session, turn, witness, receipt, target, later, same-turn and
/// three footprint probes: the whole worst case per current key.
pub(crate) const CALLS_PER_CANDIDATE: u32 = 11;
/// Three PK rows, two witnesses, one receipt, one target, one later, three
/// same-turn and three footprint answers.
pub(crate) const ROWS_PER_CANDIDATE: u32 = 14;
/// The declared-verb list a receipt preflight accepts.
pub(crate) const MAX_VERBS: usize = 64;
/// The owners this first road certifies, exactly.
const OWNER_TOKENS: [&str; 3] = ["claude-code", "codex", "antigravity"];

/// Why a classification is not eligible, as a static identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Reason {
    #[cfg_attr(unix, expect(dead_code, reason = "the non-Unix loader's one answer"))]
    PlatformUnsupported,
    SessionsUnavailable,
    MemberMismatch,
    StoreUnavailable,
    CacheKeysOverflow,
    CandidatesOverflow,
    BudgetExceeded,
    ContextInvalid,
    NativeUnsafe,
    CacheAbsent,
    CacheNotRegular,
    CacheOversize,
    CacheUnreadable,
    CacheMalformed,
    CacheSchema,
    CacheSession,
    CacheAgent,
    CacheWorkTree,
    CacheWorkTreeUnknown,
    CachePhase,
    CacheNotCurrent,
    CacheEdge,
    ContractUnsupported,
    EvidenceIncomplete,
    CellInvalid,
    IdentityMissing,
    IdentityMismatch,
    SessionMissing,
    SessionMismatch,
    TurnMissing,
    TurnMismatch,
    EventMissing,
    EventAmbiguous,
    EventContract,
    EventKeyMismatch,
    AdapterVerbs,
    ReceiptMissing,
    ReceiptMismatch,
    ReceiptOutcome,
    TargetPresent,
    LaterBoundary,
    SnapshotMalformed,
    EndBlocksStart,
    OrphanFootprint,
}

/// The classification of one queued boundary: fresh collection is admissible,
/// the evidence says it is not, or the evidence could not be read whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Eligible(Eligible),
    Unsupported(Reason),
    Unavailable(Reason),
}

/// A verdict's refusing half, two bytes wide: what every check returns on its
/// error path so the eligible facts never ride an `Err`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Refusal {
    Unsupported(Reason),
    Unavailable(Reason),
}

impl From<Refusal> for Verdict {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Unsupported(reason) => Self::Unsupported(reason),
            Refusal::Unavailable(reason) => Self::Unavailable(reason),
        }
    }
}

/// Identifiers and digests only: never a payload, a path or a clock.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Eligible {
    /// SHA-256 of the exact cache bytes this verdict was decided on: audit
    /// evidence for a reservation, never a freshness condition.
    pub(crate) cache_sha256: String,
    pub(crate) session_row: String,
    pub(crate) turn_id: String,
    pub(crate) snapshot_id: String,
    pub(crate) event_key: String,
    pub(crate) payload_sha256: String,
    pub(crate) receipt_id: String,
    pub(crate) verb: &'static str,
}

/// The member the caller asserts its candidates belong to; a pass refuses
/// before any read unless it is the admitted hold's own.
pub(crate) struct Member {
    repo_id: RepoId,
    git_dir: String,
    work_tree: String,
}

/// One queued boundary job and the context identity of the delivery it rode.
pub(crate) struct Candidate<'a> {
    pub(crate) snapshot: &'a BoundarySnapshot,
    pub(crate) context_id: &'a str,
}

/// What two deliveries share when they ask for the same boundary.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct CandidateKey {
    pub(crate) owner: String,
    pub(crate) native: String,
    pub(crate) ordinal: u32,
    pub(crate) end: bool,
}

/// One classification per supplied candidate, with its full context identity:
/// the memo reuses checked facts, never a delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Classified {
    pub(crate) key: CandidateKey,
    pub(crate) context_id: String,
    pub(crate) verdict: Verdict,
}

/// A whole pass and what it actually cost.
#[derive(Debug)]
pub(crate) struct Pass {
    pub(crate) classified: Vec<Classified>,
    pub(crate) calls: u32,
    pub(crate) rows: u32,
    pub(crate) cells: u64,
    pub(crate) cache_bytes: u64,
}

/// The whole-pass accounting; every overflow fails the pass, never one key.
#[derive(Debug, Default)]
pub(crate) struct Budget {
    pub(crate) calls: u32,
    pub(crate) rows: u32,
    pub(crate) cells: u64,
    pub(crate) cache_bytes: u64,
}

/// The semantic facts of one session cache; everything else is discarded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CacheProjection {
    pub(crate) cache_sha256: String,
    pub(crate) schema: u32,
    pub(crate) session_id: String,
    pub(crate) agent: String,
    pub(crate) state: String,
    pub(crate) ordinal: u32,
    pub(crate) turn_open: bool,
    pub(crate) work_tree: Option<String>,
}

/// The closed verb/kind/trust row the first road admits for one owner and edge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Contract {
    pub(crate) verb: &'static str,
    kind: &'static str,
    trust: &'static str,
}

/// What the evidence of one candidate key is compared against.
pub(crate) struct Expectation<'a> {
    owner: &'a str,
    native: &'a str,
    ordinal: u32,
    edge: BoundaryEdge,
    repo_id: &'a str,
    work_tree: &'a str,
    contract: Contract,
    session_row: String,
    turn_id: String,
    snapshot_id: String,
}

/// The rows one key's point queries returned, undecoded: the classifier
/// decodes them, so a malformed cell is its finding and not the loader's.
pub(crate) struct Canonical {
    identity: Vec<Row>,
    session: Vec<Row>,
    turn: Vec<Row>,
    events: Vec<Row>,
    /// `None` when the preflight failed or no single witness addressed one.
    receipt: Option<Vec<Row>>,
    verbs: Option<Reason>,
    target: Vec<Row>,
    later: Vec<Row>,
    same_turn: Vec<Row>,
    footprint: [Vec<Row>; 3],
}

impl Reason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PlatformUnsupported => "platform-unsupported",
            Self::SessionsUnavailable => "sessions-unavailable",
            Self::MemberMismatch => "member-mismatch",
            Self::StoreUnavailable => "store-unavailable",
            Self::CacheKeysOverflow => "cache-keys-overflow",
            Self::CandidatesOverflow => "candidates-overflow",
            Self::BudgetExceeded => "budget-exceeded",
            Self::ContextInvalid => "context-invalid",
            Self::NativeUnsafe => "native-unsafe",
            Self::CacheAbsent => "cache-absent",
            Self::CacheNotRegular => "cache-not-regular",
            Self::CacheOversize => "cache-oversize",
            Self::CacheUnreadable => "cache-unreadable",
            Self::CacheMalformed => "cache-malformed",
            Self::CacheSchema => "cache-schema",
            Self::CacheSession => "cache-session",
            Self::CacheAgent => "cache-agent",
            Self::CacheWorkTree => "cache-work-tree",
            Self::CacheWorkTreeUnknown => "cache-work-tree-unknown",
            Self::CachePhase => "cache-phase",
            Self::CacheNotCurrent => "cache-not-current",
            Self::CacheEdge => "cache-edge",
            Self::ContractUnsupported => "contract-unsupported",
            Self::EvidenceIncomplete => "evidence-incomplete",
            Self::CellInvalid => "cell-invalid",
            Self::IdentityMissing => "identity-missing",
            Self::IdentityMismatch => "identity-mismatch",
            Self::SessionMissing => "session-missing",
            Self::SessionMismatch => "session-mismatch",
            Self::TurnMissing => "turn-missing",
            Self::TurnMismatch => "turn-mismatch",
            Self::EventMissing => "event-missing",
            Self::EventAmbiguous => "event-ambiguous",
            Self::EventContract => "event-contract",
            Self::EventKeyMismatch => "event-key-mismatch",
            Self::AdapterVerbs => "adapter-verbs",
            Self::ReceiptMissing => "receipt-missing",
            Self::ReceiptMismatch => "receipt-mismatch",
            Self::ReceiptOutcome => "receipt-outcome",
            Self::TargetPresent => "target-present",
            Self::LaterBoundary => "later-boundary",
            Self::SnapshotMalformed => "snapshot-malformed",
            Self::EndBlocksStart => "end-blocks-start",
            Self::OrphanFootprint => "orphan-footprint",
        }
    }
}

impl Member {
    pub(crate) fn new(repo_id: RepoId, git_dir: &str, work_tree: &str) -> WireResult<Self> {
        plan::lexical_path(git_dir)?;
        plan::lexical_path(work_tree)?;
        Ok(Self {
            repo_id,
            git_dir: git_dir.to_owned(),
            work_tree: work_tree.to_owned(),
        })
    }

    #[cfg(unix)]
    fn matches(&self, sessions: &ReadySessions<'_>) -> bool {
        sessions.repo_id() == &self.repo_id
            && sessions.git_dir() == std::path::Path::new(&self.git_dir)
            && sessions.work_tree() == std::path::Path::new(&self.work_tree)
    }
}

impl CandidateKey {
    fn of(snapshot: &BoundarySnapshot) -> Self {
        Self {
            owner: snapshot.agent.clone(),
            native: snapshot.session_id.clone(),
            ordinal: snapshot.ordinal,
            end: matches!(snapshot.boundary, BoundaryEdge::End),
        }
    }

    fn edge(&self) -> BoundaryEdge {
        if self.end {
            BoundaryEdge::End
        } else {
            BoundaryEdge::Start
        }
    }

    fn cache_key(&self) -> (String, String) {
        (self.owner.clone(), self.native.clone())
    }

    /// What refuses a key before its cache is read: an owner/edge outside the
    /// closed contract, or a native id that may not name a file.
    fn pre_verdict(&self) -> Option<Verdict> {
        if contract_for(&self.owner, self.edge()).is_none() {
            return Some(Verdict::Unsupported(Reason::ContractUnsupported));
        }
        if !rift_shadow::hooks::is_safe_session_id(&self.native) {
            return Some(Verdict::Unavailable(Reason::NativeUnsafe));
        }
        None
    }
}

impl<'a> Expectation<'a> {
    fn new(key: &'a CandidateKey, member: &'a Member, contract: Contract) -> Self {
        let session_row = ids::session_row_id(&key.owner, &key.native);
        let edge = key.edge();
        Self {
            owner: &key.owner,
            native: &key.native,
            ordinal: key.ordinal,
            edge,
            repo_id: member.repo_id.as_str(),
            work_tree: &member.work_tree,
            contract,
            turn_id: ids::turn_row_id(&session_row, key.ordinal),
            snapshot_id: ids::turn_boundary_snapshot_id(
                &session_row,
                key.ordinal,
                edge.boundary().as_str(),
            ),
            session_row,
        }
    }

    fn receipt_id(&self, payload_sha256: &str) -> String {
        rift_shadow::bridge::hook_receipt_id(
            self.owner,
            self.contract.verb,
            self.native,
            &self.ordinal.to_string(),
            payload_sha256,
        )
    }
}

impl Budget {
    /// The worst case of a whole pass, checked before its first query.
    pub(crate) fn preflight(current: usize) -> Result<(), Reason> {
        if current > MAX_CURRENT_CANDIDATES {
            return Err(Reason::CandidatesOverflow);
        }
        let keys = u32::try_from(current).map_err(|_| Reason::CandidatesOverflow)?;
        let calls = keys.checked_mul(CALLS_PER_CANDIDATE);
        let rows = keys.checked_mul(ROWS_PER_CANDIDATE);
        if calls.is_none_or(|calls| calls > MAX_CALLS) || rows.is_none_or(|rows| rows > MAX_ROWS) {
            return Err(Reason::BudgetExceeded);
        }
        Ok(())
    }

    pub(crate) fn charge(&mut self, rows: &[Row]) -> Result<(), Reason> {
        let count = u32::try_from(rows.len()).map_err(|_| Reason::BudgetExceeded)?;
        self.calls = self
            .calls
            .checked_add(1)
            .filter(|calls| *calls <= MAX_CALLS)
            .ok_or(Reason::BudgetExceeded)?;
        self.rows = self
            .rows
            .checked_add(count)
            .filter(|rows| *rows <= MAX_ROWS)
            .ok_or(Reason::BudgetExceeded)?;
        self.cells = self
            .cells
            .checked_add(rows::cell_bytes(rows))
            .filter(|cells| *cells <= MAX_CELL_BYTES)
            .ok_or(Reason::BudgetExceeded)?;
        Ok(())
    }

    pub(crate) fn charge_cache(&mut self, bytes: u64) -> Result<(), Reason> {
        self.cache_bytes = self
            .cache_bytes
            .checked_add(bytes)
            .filter(|total| *total <= MAX_CACHE_BYTES)
            .ok_or(Reason::BudgetExceeded)?;
        Ok(())
    }
}

/// The closed first-road contract, source-backed on the three adapters' verb
/// tables: Claude Code and Codex map `UserPromptSubmit` to exactly one
/// `prompt_submit` of transcript trust, all three map `Stop` to exactly one
/// `turn_end` of git-fact trust, and Antigravity has no explicit Start.
pub(crate) fn contract_for(owner: &str, edge: BoundaryEdge) -> Option<Contract> {
    match (owner, edge) {
        ("claude-code" | "codex", BoundaryEdge::Start) => Some(Contract {
            verb: "UserPromptSubmit",
            kind: EventKind::PromptSubmit.as_str(),
            trust: Trust::TranscriptUntrusted.as_str(),
        }),
        ("claude-code" | "codex" | "antigravity", BoundaryEdge::End) => Some(Contract {
            verb: "Stop",
            kind: EventKind::TurnEnd.as_str(),
            trust: Trust::GitFact.as_str(),
        }),
        _ => None,
    }
}

/// Pure: the cache's semantic facts and whatever canonical rows were read, to
/// one verdict. `None` canonical evidence for a current cache is incomplete
/// I/O, never a judgement.
pub(crate) fn classify_boundary(
    expect: &Expectation<'_>,
    cache: &CacheProjection,
    canonical: Option<&Canonical>,
) -> Verdict {
    if let Err(refusal) = check_cache(expect, cache) {
        return refusal.into();
    }
    let Some(canonical) = canonical else {
        return Verdict::Unavailable(Reason::EvidenceIncomplete);
    };
    match classify_canonical(expect, canonical) {
        Ok(mut eligible) => {
            eligible.cache_sha256.clone_from(&cache.cache_sha256);
            Verdict::Eligible(eligible)
        }
        Err(refusal) => refusal.into(),
    }
}

/// Present, schema 1, the exact native session, the exact owner among the
/// closed tokens, the admitted work tree (absent is UNKNOWN evidence, so
/// unavailable; a different tree was read whole and is unsupported), Active, the queued ordinal above zero, and the edge's own open/closed
/// relation. Nothing about memos, hashes or clocks.
fn check_cache(expect: &Expectation<'_>, cache: &CacheProjection) -> Result<(), Refusal> {
    if cache.schema != 1 {
        return Err(Refusal::Unavailable(Reason::CacheSchema));
    }
    if cache.session_id != expect.native {
        return Err(Refusal::Unsupported(Reason::CacheSession));
    }
    if cache.agent != expect.owner || !OWNER_TOKENS.contains(&cache.agent.as_str()) {
        return Err(Refusal::Unsupported(Reason::CacheAgent));
    }
    match cache.work_tree.as_deref() {
        None => return Err(Refusal::Unavailable(Reason::CacheWorkTreeUnknown)),
        Some(tree) if tree != expect.work_tree => {
            return Err(Refusal::Unsupported(Reason::CacheWorkTree))
        }
        Some(_) => (),
    }
    if cache.state != "active" {
        return Err(Refusal::Unsupported(Reason::CachePhase));
    }
    if cache.ordinal == 0 || cache.ordinal != expect.ordinal {
        return Err(Refusal::Unsupported(Reason::CacheNotCurrent));
    }
    let relation_holds = match expect.edge {
        BoundaryEdge::Start => cache.turn_open,
        BoundaryEdge::End => !cache.turn_open,
    };
    if !relation_holds {
        return Err(Refusal::Unsupported(Reason::CacheEdge));
    }
    Ok(())
}

fn classify_canonical(expect: &Expectation<'_>, c: &Canonical) -> Result<Eligible, Refusal> {
    check_identity(expect, &c.identity)?;
    check_session(expect, &c.session)?;
    check_turn(expect, &c.turn)?;
    let event = check_event(expect, &c.events)?;
    let receipt = check_receipt(expect, c, &event)?;
    check_snapshots(expect, c)?;
    Ok(Eligible {
        cache_sha256: String::new(),
        session_row: expect.session_row.clone(),
        turn_id: expect.turn_id.clone(),
        snapshot_id: expect.snapshot_id.clone(),
        event_key: event.key,
        payload_sha256: event.payload_sha256,
        receipt_id: receipt.id,
        verb: expect.contract.verb,
    })
}

/// A primary-key answer: no row is a missing fact, one row decodes or is an
/// invalid cell, and more than one is a cell the engine should not have given.
fn single(rows: &[Row], missing: Reason) -> Result<&Row, Refusal> {
    match rows {
        [] => Err(Refusal::Unavailable(missing)),
        [row] => Ok(row),
        _ => Err(Refusal::Unavailable(Reason::CellInvalid)),
    }
}

fn check_identity(expect: &Expectation<'_>, rows: &[Row]) -> Result<(), Refusal> {
    let fact =
        rows::identity(single(rows, Reason::IdentityMissing)?).map_err(Refusal::Unavailable)?;
    if fact.session_id != expect.session_row
        || fact.agent != expect.owner
        || fact.native != expect.native
    {
        return Err(Refusal::Unsupported(Reason::IdentityMismatch));
    }
    Ok(())
}

fn check_session(expect: &Expectation<'_>, rows: &[Row]) -> Result<(), Refusal> {
    let fact =
        rows::session(single(rows, Reason::SessionMissing)?).map_err(Refusal::Unavailable)?;
    if fact.session_id != expect.session_row
        || fact.agent != expect.owner
        || fact.repo_id != expect.repo_id
    {
        return Err(Refusal::Unsupported(Reason::SessionMismatch));
    }
    Ok(())
}

fn check_turn(expect: &Expectation<'_>, rows: &[Row]) -> Result<(), Refusal> {
    let fact = rows::turn(single(rows, Reason::TurnMissing)?).map_err(Refusal::Unavailable)?;
    if fact.turn_id != expect.turn_id
        || fact.session_id != expect.session_row
        || fact.ordinal != expect.ordinal
    {
        return Err(Refusal::Unsupported(Reason::TurnMismatch));
    }
    Ok(())
}

/// Exactly one witness, of the contract's kind and trust at event ordinal
/// zero; two keys at one slot are an ambiguity, never a choice.
fn check_event(expect: &Expectation<'_>, rows: &[Row]) -> Result<rows::EventFact, Refusal> {
    let row = match rows {
        [] => return Err(Refusal::Unavailable(Reason::EventMissing)),
        [row] => row,
        _ => return Err(Refusal::Unsupported(Reason::EventAmbiguous)),
    };
    let fact = rows::event(row).map_err(Refusal::Unavailable)?;
    if fact.kind != expect.contract.kind
        || fact.trust != expect.contract.trust
        || fact.event_ordinal != 0
    {
        return Err(Refusal::Unsupported(Reason::EventContract));
    }
    Ok(fact)
}

/// The owner's recorded receipt for the one kind-specific verb, then the
/// event key recomputed from the STORED verb: the key omits kind, trust and
/// time, so this is what binds the witness to the hook that wrote it.
fn check_receipt(
    expect: &Expectation<'_>,
    c: &Canonical,
    event: &rows::EventFact,
) -> Result<rows::ReceiptFact, Refusal> {
    if let Some(reason) = c.verbs {
        return Err(Refusal::Unavailable(reason));
    }
    let Some(rows) = &c.receipt else {
        return Err(Refusal::Unavailable(Reason::ReceiptMissing));
    };
    let fact =
        rows::receipt(single(rows, Reason::ReceiptMissing)?).map_err(Refusal::Unavailable)?;
    if fact.id != expect.receipt_id(&event.payload_sha256)
        || fact.agent != expect.owner
        || fact.verb != expect.contract.verb
        || fact.native != expect.native
    {
        return Err(Refusal::Unsupported(Reason::ReceiptMismatch));
    }
    if fact.outcome != rift_shadow::HookReceipt::Recorded.as_str() {
        return Err(Refusal::Unsupported(Reason::ReceiptOutcome));
    }
    let sha = rift_core::Sha256Id::parse_hex(&event.payload_sha256)
        .map_err(|_| Refusal::Unavailable(Reason::CellInvalid))?;
    let recomputed = rift_agent::idempotency_key(
        &AgentId::new(expect.owner),
        expect.native,
        &fact.verb,
        expect.ordinal,
        event.event_ordinal,
        &sha,
    );
    if recomputed.to_hex() != event.key {
        return Err(Refusal::Unsupported(Reason::EventKeyMismatch));
    }
    Ok(fact)
}

/// The exact target, any later ordinal, a same-ordinal row that is malformed
/// or an End under a Start, and any footprint at the target id all block a
/// fresh collection; an earlier Start alone does not block an End.
fn check_snapshots(expect: &Expectation<'_>, c: &Canonical) -> Result<(), Refusal> {
    if let Some(row) = c.target.first() {
        let fact = rows::snapshot(row).map_err(Refusal::Unavailable)?;
        if fact.snapshot_id != expect.snapshot_id
            || fact.session_id != expect.session_row
            || fact.ordinal != expect.ordinal
            || fact.boundary != expect.edge.boundary().as_str()
        {
            return Err(Refusal::Unsupported(Reason::SnapshotMalformed));
        }
        return Err(Refusal::Unsupported(Reason::TargetPresent));
    }
    if rows::exists(&c.later).map_err(Refusal::Unavailable)? {
        return Err(Refusal::Unsupported(Reason::LaterBoundary));
    }
    if c.same_turn.len() > 2 {
        return Err(Refusal::Unsupported(Reason::SnapshotMalformed));
    }
    let mut end_present = false;
    for row in &c.same_turn {
        let fact = rows::same_turn(row).map_err(Refusal::Unavailable)?;
        let expected_id =
            ids::turn_boundary_snapshot_id(&expect.session_row, expect.ordinal, &fact.boundary);
        if !matches!(fact.boundary.as_str(), "start" | "end") || fact.snapshot_id != expected_id {
            return Err(Refusal::Unsupported(Reason::SnapshotMalformed));
        }
        if fact.boundary == expect.edge.boundary().as_str() {
            return Err(Refusal::Unsupported(Reason::TargetPresent));
        }
        end_present |= fact.boundary == "end";
    }
    if end_present && matches!(expect.edge, BoundaryEdge::Start) {
        return Err(Refusal::Unsupported(Reason::EndBlocksStart));
    }
    for rows in &c.footprint {
        if rows::exists(rows).map_err(Refusal::Unavailable)? {
            return Err(Refusal::Unsupported(Reason::OrphanFootprint));
        }
    }
    Ok(())
}

/// Whether the firing agent's complete declared verb list is within its caps,
/// free of duplicates and declares the one verb the contract probes. The
/// table is a precondition for the probe, never evidence of history.
fn verbs_preflight(registry: &AdapterRegistry, owner: &str, verb: &str) -> Option<Reason> {
    let Some(adapter) = registry.get(&AgentId::new(owner)) else {
        return Some(Reason::AdapterVerbs);
    };
    let verbs = adapter.verbs();
    let distinct: BTreeSet<&&str> = verbs.iter().collect();
    if verbs.len() > MAX_VERBS
        || distinct.len() != verbs.len()
        || verbs
            .iter()
            .any(|declared| declared.len() > rows::MAX_VERB_BYTES)
        || !verbs.contains(&verb)
    {
        return Some(Reason::AdapterVerbs);
    }
    None
}

#[cfg(unix)]
struct PassState<'m> {
    member: &'m Member,
    budget: Budget,
    caches: BTreeMap<(String, String), Result<CacheProjection, Reason>>,
    verbs: BTreeMap<(String, &'static str), Option<Reason>>,
    verdicts: BTreeMap<CandidateKey, Verdict>,
}

#[cfg(unix)]
impl PassState<'_> {
    fn is_current(&self, key: &CandidateKey) -> bool {
        if key.pre_verdict().is_some() {
            return false;
        }
        let Some(Ok(cache)) = self.caches.get(&key.cache_key()) else {
            return false;
        };
        let Some(contract) = contract_for(&key.owner, key.edge()) else {
            return false;
        };
        check_cache(&Expectation::new(key, self.member, contract), cache).is_ok()
    }

    /// The verdict of a key no query was spent on: its pre-cache refusal, its
    /// cache's failure, or the cache's own disagreement.
    fn cold_verdict(&self, key: &CandidateKey) -> Verdict {
        if let Some(verdict) = key.pre_verdict() {
            return verdict;
        }
        let contract = contract_for(&key.owner, key.edge()).unwrap_or_else(unreachable_contract);
        match self.caches.get(&key.cache_key()) {
            Some(Ok(cache)) => {
                classify_boundary(&Expectation::new(key, self.member, contract), cache, None)
            }
            Some(Err(reason)) => Verdict::Unavailable(*reason),
            None => Verdict::Unavailable(Reason::EvidenceIncomplete),
        }
    }

    async fn query(
        &mut self,
        store: &impl CanonicalStore,
        sql: &str,
        params: &[Value],
    ) -> Result<Vec<Row>, Reason> {
        let rows = store
            .query(sql, params)
            .await
            .map_err(|_| Reason::StoreUnavailable)?;
        self.budget.charge(&rows)?;
        Ok(rows)
    }

    fn verbs_for(
        &mut self,
        registry: &AdapterRegistry,
        owner: &str,
        verb: &'static str,
    ) -> Option<Reason> {
        *self
            .verbs
            .entry((owner.to_owned(), verb))
            .or_insert_with(|| verbs_preflight(registry, owner, verb))
    }

    /// At most eleven point queries for one current key, in the fixed order
    /// the budget formula counts; the receipt is probed only after a single
    /// witness addressed it and the verb list passed.
    async fn load_canonical(
        &mut self,
        store: &impl CanonicalStore,
        registry: &AdapterRegistry,
        expect: &Expectation<'_>,
    ) -> Result<Canonical, Reason> {
        let session_row = Value::text(expect.session_row.clone());
        let ordinal = Value::Int(i64::from(expect.ordinal));
        let snapshot_id = Value::text(expect.snapshot_id.clone());
        let witness = match expect.edge {
            BoundaryEdge::Start => rows::START_WITNESS,
            BoundaryEdge::End => rows::END_WITNESS,
        };
        let identity = self
            .query(
                store,
                rows::IDENTITY_BY_PK,
                std::slice::from_ref(&session_row),
            )
            .await?;
        let session = self
            .query(
                store,
                rows::SESSION_BY_PK,
                std::slice::from_ref(&session_row),
            )
            .await?;
        let turn_id = Value::text(expect.turn_id.clone());
        let turn = self.query(store, rows::TURN_BY_PK, &[turn_id]).await?;
        let native = Value::text(expect.native);
        let events = self
            .query(store, witness, &[native, ordinal.clone()])
            .await?;
        let verbs = self.verbs_for(registry, expect.owner, expect.contract.verb);
        let receipt = match (verbs, events.as_slice()) {
            (None, [row]) => self.probe_receipt(store, expect, row).await?,
            _ => None,
        };
        let target = self
            .query(
                store,
                rows::TARGET_BY_PK,
                std::slice::from_ref(&snapshot_id),
            )
            .await?;
        let later = self
            .query(
                store,
                rows::LATER_EXISTS,
                &[session_row.clone(), ordinal.clone()],
            )
            .await?;
        let same_turn = self
            .query(store, rows::SAME_TURN, &[session_row, ordinal])
            .await?;
        let footprint = self.footprint(store, &snapshot_id).await?;
        Ok(Canonical {
            identity,
            session,
            turn,
            events,
            receipt,
            verbs,
            target,
            later,
            same_turn,
            footprint,
        })
    }

    /// The one kind-specific receipt, addressed by the witness's payload hash;
    /// a witness whose cells do not decode addresses nothing.
    async fn probe_receipt(
        &mut self,
        store: &impl CanonicalStore,
        expect: &Expectation<'_>,
        witness: &Row,
    ) -> Result<Option<Vec<Row>>, Reason> {
        let Ok(event) = rows::event(witness) else {
            return Ok(None);
        };
        let id = Value::text(expect.receipt_id(&event.payload_sha256));
        Ok(Some(self.query(store, rows::RECEIPT_BY_PK, &[id]).await?))
    }

    /// The three existence probes at the target id: observation, file, kind.
    async fn footprint(
        &mut self,
        store: &impl CanonicalStore,
        snapshot_id: &Value,
    ) -> Result<[Vec<Row>; 3], Reason> {
        let mut footprint: [Vec<Row>; 3] = Default::default();
        for (slot, sql) in footprint.iter_mut().zip([
            rows::OBSERVATION_EXISTS,
            rows::FILE_EXISTS,
            rows::FILE_KIND_EXISTS,
        ]) {
            *slot = self
                .query(store, sql, std::slice::from_ref(snapshot_id))
                .await?;
        }
        Ok(footprint)
    }

    async fn classify_current(
        &mut self,
        store: &impl CanonicalStore,
        registry: &AdapterRegistry,
        key: &CandidateKey,
    ) -> Result<(), Reason> {
        let Some(contract) = contract_for(&key.owner, key.edge()) else {
            return Ok(());
        };
        let expect = Expectation::new(key, self.member, contract);
        let canonical = self.load_canonical(store, registry, &expect).await?;
        let Some(Ok(cache)) = self.caches.get(&key.cache_key()) else {
            return Ok(());
        };
        let verdict = classify_boundary(&expect, cache, Some(&canonical));
        self.verdicts.insert(key.clone(), verdict);
        Ok(())
    }

    fn finish(self, candidates: &[Candidate<'_>], keys: Vec<CandidateKey>) -> Pass {
        let classified = candidates
            .iter()
            .zip(keys)
            .map(|(candidate, key)| {
                let verdict = if is_context_id(candidate.context_id) {
                    self.verdicts
                        .get(&key)
                        .cloned()
                        .unwrap_or_else(|| self.cold_verdict(&key))
                } else {
                    Verdict::Unavailable(Reason::ContextInvalid)
                };
                Classified {
                    key,
                    context_id: candidate.context_id.to_owned(),
                    verdict,
                }
            })
            .collect();
        Pass {
            classified,
            calls: self.budget.calls,
            rows: self.budget.rows,
            cells: self.budget.cells,
            cache_bytes: self.budget.cache_bytes,
        }
    }
}

/// `cold_verdict` is reached only for keys whose contract exists; a key with
/// none already answered through `pre_verdict`.
#[cfg(unix)]
fn unreachable_contract() -> Contract {
    Contract {
        verb: "",
        kind: "",
        trust: "",
    }
}

/// A delivery's context identity is the queue's blake3 hex.
#[cfg(unix)]
fn is_context_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// One whole pass: the admitted sessions tree rechecked, the asserted member
/// compared against the hold's own, every cache key preflighted and read, the
/// current set and its worst-case cost preflighted, then the point queries.
/// `Err` is the whole pass unavailable; a per-candidate answer is in the `Pass`.
#[cfg(unix)]
pub(crate) async fn load_evidence(
    store: &impl CanonicalStore,
    sessions: &ReadySessions<'_>,
    registry: &AdapterRegistry,
    member: &Member,
    candidates: &[Candidate<'_>],
) -> Result<Pass, Reason> {
    sessions
        .recheck()
        .map_err(|_| Reason::SessionsUnavailable)?;
    if !member.matches(sessions) {
        return Err(Reason::MemberMismatch);
    }
    let keys: Vec<CandidateKey> = candidates
        .iter()
        .map(|candidate| CandidateKey::of(candidate.snapshot))
        .collect();
    // A candidate whose context identity is not the queue's spends nothing:
    // it is refused before its cache is opened or a query is addressed.
    let valid: Vec<&CandidateKey> = keys
        .iter()
        .zip(candidates)
        .filter(|(_, candidate)| is_context_id(candidate.context_id))
        .map(|(key, _)| key)
        .collect();
    let cache_keys: BTreeSet<(String, String)> = valid
        .iter()
        .filter(|key| key.pre_verdict().is_none())
        .map(|key| key.cache_key())
        .collect();
    if cache_keys.len() > MAX_CACHE_KEYS {
        return Err(Reason::CacheKeysOverflow);
    }
    let mut state = PassState {
        member,
        budget: Budget::default(),
        caches: BTreeMap::new(),
        verbs: BTreeMap::new(),
        verdicts: BTreeMap::new(),
    };
    for key in cache_keys {
        let read = cache::read_projection(sessions.sessions(), &key.1, &mut state.budget);
        if read == Err(Reason::BudgetExceeded) {
            return Err(Reason::BudgetExceeded);
        }
        state.caches.insert(key, read);
    }
    let current: BTreeSet<&CandidateKey> = valid
        .iter()
        .copied()
        .filter(|key| state.is_current(key))
        .collect();
    Budget::preflight(current.len())?;
    sessions
        .recheck()
        .map_err(|_| Reason::SessionsUnavailable)?;
    for key in current {
        state.classify_current(store, registry, key).await?;
    }
    Ok(state.finish(candidates, keys))
}

/// Non-Unix builds hold no descriptors: the one closed answer.
#[cfg(not(unix))]
pub(crate) async fn load_evidence(
    _store: &impl CanonicalStore,
    _sessions: &ReadySessions<'_>,
    _registry: &AdapterRegistry,
    _member: &Member,
    _candidates: &[Candidate<'_>],
) -> Result<Pass, Reason> {
    Err(Reason::PlatformUnsupported)
}
