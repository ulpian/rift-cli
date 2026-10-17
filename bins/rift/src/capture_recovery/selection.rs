//! Deterministic selection over a complete inventory: ONE eligibility pass,
//! one reserved current boundary chosen by the closed lexical key, every other
//! understood boundary historical in slot order, and the durable
//! SelectionIntent published no-clobber before anything is pinned.
//!
//! Nothing here pins, cuts, archives, retires, releases or writes a cache: the
//! result is the intent, its digest and the retained original descriptors the
//! pin slice consumes. Any refusal before publication leaves nothing behind.
mod intent;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::io::Write as _;

use cap_fs_ext::{FollowSymlinks, OpenOptionsFollowExt as _};
use cap_std::fs::{File, OpenOptions, OpenOptionsExt as _};
use rift_core::boundary_gap::MemberKey;
use rift_shadow::hooks::AdapterRegistry;
use rift_shadow::ids;
use rift_store::CanonicalStore;
use sha2::{Digest as _, Sha256};

use super::eligibility::{self, Candidate, Classified, Eligible, Member, Reason, Verdict};
use super::files::{DirectoryIdentity, Namespace};
use super::hold::OwnHold;
use super::inventory::{self, Counts, Inventory, Source};
use super::plan::Plan;
use super::{codec, WireError};
pub(crate) use intent::{
    Expectation, Identity, KnownBoundary, ReservedObservation, Scope, SelectionIntent,
};

pub(crate) const POLICY: &str = "one-eligible-explicit-current-boundary@1";
pub(crate) const INTENT_NAME: &str = "selection-intent.json";

/// Why a selection is owed rather than made, as a static identifier.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Owed {
    HoldUnavailable,
    QueueUnavailable,
    SessionsUnavailable,
    PolicyMismatch,
    Inventory,
    PassUnavailable(Reason),
    CandidateUnavailable(BTreeMap<Reason, u32>),
    ReservationTie,
    SelectedOverflow,
    IntentOversize,
    IntentConflict,
    PublishFailed,
    Resource,
}

/// The whole attempt owed: the reason and whatever the inventory counted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Deferred {
    pub(crate) owed: Owed,
    pub(crate) counts: Counts,
}

/// Whether the published intent names historical members or is the tagged
/// NoHistoricalCut of a complete inventory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cut {
    Historical,
    NoHistoricalCut,
}

/// Whether this attempt wrote the intent or found its exact bytes already there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Publication {
    Fresh,
    Resumed,
}

/// One retained original descriptor and the expectation frozen from it.
#[derive(Debug)]
pub(crate) struct RetainedOriginal {
    pub(crate) slot: u32,
    pub(crate) file: File,
    pub(crate) expectation: Expectation,
}

#[derive(Debug)]
pub(crate) struct Reserved {
    pub(crate) original: RetainedOriginal,
    pub(crate) observation: ReservedObservation,
}

#[derive(Debug)]
pub(crate) struct Selected {
    pub(crate) intent: SelectionIntent,
    pub(crate) intent_sha256: String,
    pub(crate) retained: Vec<RetainedOriginal>,
    pub(crate) reserved: Option<Reserved>,
    pub(crate) cut: Cut,
    pub(crate) publication: Publication,
}

/// The seam a test observes, in the order the attempt reaches it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    BeforeRetain(u32),
    Inventoried,
    Classified,
    BeforePublish,
    Written,
    Synced,
}

/// The closed reservation order: AFTER every check, never newest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct LexKey {
    session_row: String,
    ordinal: u32,
    boundary: &'static str,
    context_sha256: String,
    name: String,
}

impl LexKey {
    fn of(source: &Source) -> Self {
        Self {
            session_row: ids::session_row_id(&source.snapshot.agent, &source.snapshot.session_id),
            ordinal: source.snapshot.ordinal,
            boundary: source.snapshot.boundary.boundary().as_str(),
            context_sha256: source.context_sha256.clone(),
            name: source.name.clone(),
        }
    }
}

/// The one eligible index the closed order chooses, or none; two candidates
/// equal on all five components refuse rather than invent a tie-break.
fn choose_reservation(keys: &[(usize, LexKey)]) -> Result<Option<usize>, Owed> {
    let mut sorted: Vec<&(usize, LexKey)> = keys.iter().collect();
    sorted.sort_by(|a, b| a.1.cmp(&b.1));
    match sorted.as_slice() {
        [] => Ok(None),
        [first, second, ..] if first.1 == second.1 => Err(Owed::ReservationTie),
        [first, ..] => Ok(Some(first.0)),
    }
}

/// `sha256("rift-capture-recovery-source@1\0" ‖ J({member_key, operation_id,
/// pin_slot, queue_root:"agent", source_name}))`: the one source identity
/// formula, reusing the Plan's canonical encoder.
fn source_key(
    member: MemberKey,
    operation: &str,
    slot: u32,
    name: &str,
) -> Result<String, WireError> {
    let bytes = codec::wire_encode(
        &serde_json::json!({
            "member_key": member.to_hex(),
            "operation_id": operation,
            "pin_slot": slot,
            "queue_root": "agent",
            "source_name": name,
        }),
        codec::MAX_BYTES,
    )?;
    let mut hash = Sha256::new();
    hash.update(b"rift-capture-recovery-source@1\0");
    hash.update(bytes);
    Ok(format!("{:x}", hash.finalize()))
}

fn pin_name(slot: u32) -> String {
    format!("pin-{slot:05}")
}

fn expectation(plan: &Plan, slot: u32, source: &Source) -> Result<Expectation, WireError> {
    Ok(Expectation {
        pin_slot: slot,
        source_key: source_key(plan.member(), plan.operation(), slot, &source.name)?,
        source_name: source.name.clone(),
        pin_name: pin_name(slot),
        dev: source.identity.dev,
        ino: source.identity.ino,
        mode: source.identity.permission_bits,
        raw_length: source.raw_length,
        ticket_sha256: source.ticket_sha256.clone(),
        context_sha256: source.context_sha256.clone(),
        attempts: source.attempts,
        known_boundary: KnownBoundary {
            agent: source.snapshot.agent.clone(),
            native_session: source.snapshot.session_id.clone(),
            session_key: ids::session_row_id(&source.snapshot.agent, &source.snapshot.session_id),
            turn_ordinal: source.snapshot.ordinal,
            boundary: source.snapshot.boundary.boundary().as_str().to_owned(),
            head_oid: source.snapshot.head_oid.clone(),
            captured_at: source.snapshot.captured_at_ms,
            queued_at: source.queued_at_ms,
        },
    })
}

fn identity(value: DirectoryIdentity) -> Identity {
    Identity {
        dev: value.dev,
        ino: value.ino,
    }
}

fn scope(plan: &Plan, queue: &Namespace, private: &Namespace) -> Result<Scope, Owed> {
    let path = |path: &std::path::Path| {
        path.to_str()
            .map(str::to_owned)
            .ok_or(Owed::QueueUnavailable)
    };
    Ok(Scope {
        member_key: plan.member().to_hex(),
        repo_id: plan.repo_id().as_str().to_owned(),
        canonical_git_dir: path(plan.git_dir())?,
        canonical_work_tree: path(plan.work_tree())?,
        source_queue_dir: path(&queue.path)?,
        private_dir: path(&private.path)?,
        source_dir_identity: identity(queue.identity().map_err(|_| Owed::QueueUnavailable)?),
        private_dir_identity: identity(private.identity().map_err(|_| Owed::HoldUnavailable)?),
        source_ancestry: queue.ancestry.iter().copied().map(identity).collect(),
        private_ancestry: private.ancestry.iter().copied().map(identity).collect(),
    })
}

/// How the pass's verdicts partition the inventory under the ruling: eligible
/// candidates compete for the reservation; `Unsupported(_)` and a session with
/// no cache in the current tree are historical; any other unavailability on
/// any candidate defers the whole selection.
struct Partition {
    eligible: Vec<(usize, Eligible)>,
    historical: Vec<usize>,
}

fn partition(classified: &[Classified]) -> Result<Partition, Owed> {
    let mut eligible = Vec::new();
    let mut historical = Vec::new();
    let mut deferred: BTreeMap<Reason, u32> = BTreeMap::new();
    for (index, item) in classified.iter().enumerate() {
        match &item.verdict {
            Verdict::Eligible(facts) => eligible.push((index, facts.clone())),
            Verdict::Unsupported(_) | Verdict::Unavailable(Reason::CacheAbsent) => {
                historical.push(index);
            }
            Verdict::Unavailable(reason) => *deferred.entry(*reason).or_insert(0) += 1,
        }
    }
    if !deferred.is_empty() {
        return Err(Owed::CandidateUnavailable(deferred));
    }
    Ok(Partition {
        eligible,
        historical,
    })
}

struct Attempt<'a> {
    plan: &'a Plan,
    inventory: Inventory,
}

impl Attempt<'_> {
    /// Candidates from the boundary tickets, ONE pass under the same hold's
    /// sessions capability, then the ruling's partition.
    async fn classify_sources(
        &self,
        store: &impl CanonicalStore,
        hold: &OwnHold<'_>,
        registry: &AdapterRegistry,
    ) -> Result<Partition, Owed> {
        if self.inventory.sources.is_empty() {
            return Ok(Partition {
                eligible: Vec::new(),
                historical: Vec::new(),
            });
        }
        let sessions = hold
            .sessions_namespace()
            .map_err(|_| Owed::SessionsUnavailable)?;
        let member = member_of(self.plan)?;
        let candidates: Vec<Candidate<'_>> = self
            .inventory
            .sources
            .iter()
            .map(|source| Candidate {
                snapshot: &source.snapshot,
                context_id: &source.context_id,
            })
            .collect();
        let pass = eligibility::load_evidence(store, &sessions, registry, &member, &candidates)
            .await
            .map_err(Owed::PassUnavailable)?;
        if pass.classified.len() != candidates.len() {
            return Err(Owed::PassUnavailable(Reason::EvidenceIncomplete));
        }
        partition(&pass.classified)
    }

    /// Reserve by the closed order, slot the historical members in that same
    /// order, and freeze every expectation; the sources move into the
    /// retained originals, one descriptor each.
    fn freeze(self, part: Partition) -> Result<Frozen, Owed> {
        let keys: Vec<(usize, LexKey)> = part
            .eligible
            .iter()
            .filter_map(|(index, _)| {
                self.inventory
                    .sources
                    .get(*index)
                    .map(|s| (*index, LexKey::of(s)))
            })
            .collect();
        let reserved_index = choose_reservation(&keys)?;
        let mut historical: Vec<(LexKey, usize)> = part
            .historical
            .iter()
            .chain(part.eligible.iter().map(|(index, _)| index))
            .filter(|index| Some(**index) != reserved_index)
            .filter_map(|index| {
                self.inventory
                    .sources
                    .get(*index)
                    .map(|s| (LexKey::of(s), *index))
            })
            .collect();
        historical.sort();
        let budgets = self.plan.budgets();
        if historical.len()
            > usize::try_from(budgets.max_selected_tickets).map_err(|_| Owed::SelectedOverflow)?
        {
            return Err(Owed::SelectedOverflow);
        }
        let mut sources: Vec<Option<Source>> =
            self.inventory.sources.into_iter().map(Some).collect();
        let mut retained = Vec::with_capacity(historical.len());
        for (slot, (_, index)) in historical.into_iter().enumerate() {
            let slot = u32::try_from(slot).map_err(|_| Owed::SelectedOverflow)?;
            let source = sources
                .get_mut(index)
                .and_then(Option::take)
                .ok_or(Owed::SelectedOverflow)?;
            retained.push(RetainedOriginal {
                slot,
                expectation: expectation(self.plan, slot, &source)
                    .map_err(|_| Owed::IntentOversize)?,
                file: source.file,
            });
        }
        let reserved = match reserved_index {
            None => None,
            Some(index) => {
                let slot = u32::try_from(retained.len()).map_err(|_| Owed::SelectedOverflow)?;
                let source = sources
                    .get_mut(index)
                    .and_then(Option::take)
                    .ok_or(Owed::SelectedOverflow)?;
                let facts = part
                    .eligible
                    .into_iter()
                    .find(|(i, _)| *i == index)
                    .map(|(_, facts)| facts)
                    .ok_or(Owed::SelectedOverflow)?;
                Some(reserve(self.plan, slot, source, facts)?)
            }
        };
        Ok(Frozen {
            counts: self.inventory.counts,
            retained,
            reserved,
        })
    }
}

/// The reserved source: its expectation at the slot after the historical
/// members, plus the event, receipt and cache evidence it was admitted on.
fn reserve(plan: &Plan, slot: u32, source: Source, facts: Eligible) -> Result<Reserved, Owed> {
    let expectation = expectation(plan, slot, &source).map_err(|_| Owed::IntentOversize)?;
    Ok(Reserved {
        observation: ReservedObservation {
            expectation: expectation.clone(),
            event_key: facts.event_key,
            receipt_id: facts.receipt_id,
            cache_evidence_sha256: facts.cache_sha256,
        },
        original: RetainedOriginal {
            slot,
            file: source.file,
            expectation,
        },
    })
}

struct Frozen {
    counts: Counts,
    retained: Vec<RetainedOriginal>,
    reserved: Option<Reserved>,
}

fn member_of(plan: &Plan) -> Result<Member, Owed> {
    Member::new(
        plan.repo_id().clone(),
        utf8(plan.git_dir())?,
        utf8(plan.work_tree())?,
    )
    .map_err(|_| Owed::HoldUnavailable)
}

fn utf8(path: &std::path::Path) -> Result<&str, Owed> {
    path.to_str().ok_or(Owed::HoldUnavailable)
}

/// The record from the frozen selection: the Plan's identity, the shared
/// scope once, the counts, the expectations in slot order, the reservation.
fn assemble(
    plan: &Plan,
    queue: &Namespace,
    private: &Namespace,
    frozen: &Frozen,
) -> Result<SelectionIntent, Owed> {
    Ok(SelectionIntent {
        schema_version: 1,
        operation_id: plan.operation().to_owned(),
        plan_sha256: plan
            .sha256()
            .map_err(|_| Owed::HoldUnavailable)?
            .as_str()
            .to_owned(),
        policy: POLICY.to_owned(),
        scope: scope(plan, queue, private)?,
        inventory: counts_of(&frozen.counts),
        historical: frozen
            .retained
            .iter()
            .map(|original| original.expectation.clone())
            .collect(),
        reserved_observation: frozen
            .reserved
            .as_ref()
            .map(|reserved| reserved.observation.clone()),
    })
}

fn counts_of(counts: &Counts) -> intent::InventoryCounts {
    intent::InventoryCounts {
        entries_counted: counts.entries_counted,
        tickets: counts.tickets,
        understood_other: counts.understood_other,
        ancillary: counts.ancillary,
        refused_by_reason: counts
            .refused
            .iter()
            .map(|(reason, count)| (reason.as_str().to_owned(), *count))
            .collect(),
    }
}

/// Create the record no-clobber, 0600, write it whole and sync it and its
/// directory. An existing record holding exactly these bytes is the resume
/// input; anything else there refuses — never overwritten.
fn publish(
    private: &Namespace,
    bytes: &[u8],
    observe: &mut impl FnMut(Phase) -> Result<(), ()>,
) -> Result<Publication, Owed> {
    private.recheck().map_err(|_| Owed::HoldUnavailable)?;
    let mut options = OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No)
        .mode(0o600);
    let mut file = match private.dir.open_with(INTENT_NAME, &options) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return resume(private, bytes);
        }
        Err(_) => return Err(Owed::PublishFailed),
    };
    file.write_all(bytes).map_err(|_| Owed::PublishFailed)?;
    observe(Phase::Written).map_err(|()| Owed::Resource)?;
    file.sync_all().map_err(|_| Owed::PublishFailed)?;
    private
        .dir
        .try_clone()
        .map_err(|_| Owed::PublishFailed)?
        .into_std_file()
        .sync_all()
        .map_err(|_| Owed::PublishFailed)?;
    observe(Phase::Synced).map_err(|()| Owed::Resource)?;
    if read_record(private)? != bytes {
        return Err(Owed::PublishFailed);
    }
    Ok(Publication::Fresh)
}

fn resume(private: &Namespace, bytes: &[u8]) -> Result<Publication, Owed> {
    let existing = read_record(private)?;
    if existing != bytes {
        return Err(Owed::IntentConflict);
    }
    SelectionIntent::decode(&existing).map_err(|_| Owed::IntentConflict)?;
    Ok(Publication::Resumed)
}

/// The record read whole under its cap plus one, descriptor-relative and
/// without following: a symlink, a directory or an oversize file is not it.
fn read_record(private: &Namespace) -> Result<Vec<u8>, Owed> {
    use std::io::Read as _;
    private.recheck().map_err(|_| Owed::HoldUnavailable)?;
    let before = private
        .dir
        .symlink_metadata(INTENT_NAME)
        .map_err(|_| Owed::IntentConflict)?;
    if !before.is_file() || before.len() > intent::MAX_INTENT_BYTES as u64 {
        return Err(Owed::IntentConflict);
    }
    let mut options = OpenOptions::new();
    options.read(true).follow(FollowSymlinks::No);
    let file = private
        .dir
        .open_with(INTENT_NAME, &options)
        .map_err(|_| Owed::IntentConflict)?;
    let mut bytes = Vec::new();
    (&file)
        .take(intent::MAX_INTENT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Owed::IntentConflict)?;
    if bytes.len() > intent::MAX_INTENT_BYTES {
        return Err(Owed::IntentConflict);
    }
    Ok(bytes)
}

/// The whole selection, under an admitted hold: the policy checked, the
/// inventory complete and actionable, one pass, the reservation and the
/// historical slots frozen, the intent measured under its caps, every
/// original descriptor already retained, then the one durable publication.
/// `Err` is the whole attempt owed: nothing published, every descriptor
/// dropped with the inventory.
pub(crate) async fn select(
    store: &impl CanonicalStore,
    hold: &OwnHold<'_>,
    registry: &AdapterRegistry,
) -> Result<Selected, Deferred> {
    select_with(store, hold, registry, &mut |_| Ok(())).await
}

pub(crate) async fn select_with(
    store: &impl CanonicalStore,
    hold: &OwnHold<'_>,
    registry: &AdapterRegistry,
    observe: &mut impl FnMut(Phase) -> Result<(), ()>,
) -> Result<Selected, Deferred> {
    let owed = |owed: Owed| Deferred {
        owed,
        counts: Counts::default(),
    };
    hold.check_current()
        .map_err(|_| owed(Owed::HoldUnavailable))?;
    let queue = hold
        .agent_queue_namespace()
        .map_err(|_| owed(Owed::QueueUnavailable))?;
    let plan = queue.plan();
    if plan.reservation() != POLICY {
        return Err(owed(Owed::PolicyMismatch));
    }
    let inventory = inventory::enumerate_with(&queue, &mut |point| match point {
        inventory::Phase::BeforeRetain(slot) => observe(Phase::BeforeRetain(slot)),
    })
    .map_err(|counts| Deferred {
        owed: Owed::Inventory,
        counts,
    })?;
    observe(Phase::Inventoried).map_err(|()| owed(Owed::Resource))?;
    let attempt = Attempt { plan, inventory };
    let counts = attempt.inventory.counts.clone();
    let deferred = |owed: Owed| Deferred {
        owed,
        counts: counts.clone(),
    };
    let part = attempt
        .classify_sources(store, hold, registry)
        .await
        .map_err(deferred)?;
    observe(Phase::Classified).map_err(|()| deferred(Owed::Resource))?;
    let frozen = attempt.freeze(part).map_err(deferred)?;
    let private = queue
        .private()
        .map_err(|_| deferred(Owed::HoldUnavailable))?;
    let intent = assemble(plan, queue.queue(), private, &frozen).map_err(deferred)?;
    let bytes = intent
        .encode()
        .map_err(|_| deferred(Owed::IntentOversize))?;
    queue
        .recheck()
        .map_err(|_| deferred(Owed::QueueUnavailable))?;
    observe(Phase::BeforePublish).map_err(|()| deferred(Owed::Resource))?;
    let publication = publish(private, &bytes, observe).map_err(deferred)?;
    let cut = if frozen.retained.is_empty() {
        Cut::NoHistoricalCut
    } else {
        Cut::Historical
    };
    Ok(Selected {
        intent,
        intent_sha256: format!("{:x}", Sha256::digest(&bytes)),
        retained: frozen.retained,
        reserved: frozen.reserved,
        cut,
        publication,
    })
}
