//! The carrier loader: every fact `choose_carrier` decides over, read whole
//! and bounded through the existing readers, and the admission it yields
//! rechecked against the same reads before anything uses it.
//!
//! The store is the caller's already-open handle: nothing here opens one, so
//! the loader adds no store open to a hook decision path. A fact that cannot
//! be read refuses the whole load — the gap stays owed and visible — rather
//! than quietly narrowing the condenses a carrier is chosen among. The
//! admission is held in memory; its durable form is the archive intent, which
//! already records the carrier key, context and ticket digests.
#[cfg(test)]
mod tests;

use std::ffi::OsStr;

use rift_store::{CanonicalStore, Row, Value};
use sha2::{Digest as _, Sha256};

use super::{choose_carrier, Admitted, CarrierFacts, CarrierOwed, Completion, EndEvent};
use crate::capture_recovery::codec::MAX_BYTES;
use crate::capture_recovery::eligibility::{
    cache, rows, Budget, CacheProjection, Reason as ReadReason, MAX_CURRENT_CANDIDATES,
};
use crate::capture_recovery::files::{self, Namespace};
use crate::capture_recovery::inventory::CondenseTicket;

/// The agents one native session is captured under, guarded like every point
/// query; two rows are enough to refuse an ambiguous identity.
const IDENTITY_AGENTS: &str = "SELECT CASE WHEN typeof(agent)='text' AND \
     length(CAST(agent AS BLOB))<=11 THEN agent ELSE NULL END \
     FROM capture_session_identity WHERE native_session_id=?1 LIMIT 2";
/// The stored `session_end` rows of one native session, bounded at two: one
/// names the event, a second refuses.
const SESSION_END: &str = "SELECT CASE WHEN typeof(idempotency_key)='text' AND \
     length(CAST(idempotency_key AS BLOB))<=64 THEN idempotency_key ELSE NULL END, \
     CASE WHEN typeof(payload_sha256)='text' AND \
     length(CAST(payload_sha256 AS BLOB))<=64 THEN payload_sha256 ELSE NULL END \
     FROM captured_event WHERE session_id=?1 AND kind='session_end' LIMIT 2";

/// Where the loader reads: the member's admitted queue and sessions trees.
pub(crate) struct Sources<'a> {
    pub(crate) queue: &'a Namespace,
    pub(crate) sessions: &'a Namespace,
}

/// Why no admission was made; the gap stays owed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum LoadOwed {
    /// More queued condenses than one bounded pass reads.
    Overflow,
    /// A fact could not be read whole; no carrier is chosen around it.
    Evidence(ReadReason),
    Carrier(CarrierOwed),
}

/// What changed between the admission and its use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Changed {
    /// The ticket is gone or its bytes differ.
    Ticket,
    Evidence(ReadReason),
    /// The facts no longer admit this exact carrier.
    Facts,
}

/// The chosen carrier, with the ticket it was admitted from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Admission {
    pub(crate) ticket: CondenseTicket,
    pub(crate) admitted: Admitted,
}

/// The facts read for one condense, owned.
struct Loaded {
    cache: Option<CacheProjection>,
    completion: Completion,
    agents: Vec<String>,
    ends: Vec<EndEvent>,
}

impl Admission {
    /// Read every fact again and require the same carrier on the same
    /// evidence; any difference refuses and the gap stays owed.
    pub(crate) async fn recheck(
        &self,
        store: &impl CanonicalStore,
        sources: &Sources<'_>,
        repo_id: &str,
    ) -> Result<(), Changed> {
        let read = files::read_regular(sources.queue, OsStr::new(&self.ticket.name), MAX_BYTES)
            .map_err(|_| Changed::Ticket)?;
        if format!("{:x}", Sha256::digest(&read.bytes)) != self.ticket.job.ticket_sha256 {
            return Err(Changed::Ticket);
        }
        let mut budget = Budget::default();
        let loaded = load_one(store, sources, &self.ticket, &mut budget)
            .await
            .map_err(Changed::Evidence)?;
        match choose_carrier(repo_id, &[loaded.facts(&self.ticket)]) {
            Ok(again) if again == self.admitted => Ok(()),
            _ => Err(Changed::Facts),
        }
    }
}

impl Loaded {
    fn facts<'a>(&'a self, ticket: &'a CondenseTicket) -> CarrierFacts<'a> {
        CarrierFacts {
            condense: &ticket.job,
            cache: self.cache.as_ref(),
            completion: self.completion,
            identity_agents: &self.agents,
            session_end: &self.ends,
        }
    }
}

/// Load every queued condense's facts and choose exactly one carrier.
pub(crate) async fn admit_carrier(
    store: &impl CanonicalStore,
    sources: &Sources<'_>,
    repo_id: &str,
    tickets: &[CondenseTicket],
) -> Result<Admission, LoadOwed> {
    if tickets.len() > MAX_CURRENT_CANDIDATES {
        return Err(LoadOwed::Overflow);
    }
    let mut budget = Budget::default();
    let mut loaded = Vec::with_capacity(tickets.len());
    for ticket in tickets {
        let facts = load_one(store, sources, ticket, &mut budget).await;
        loaded.push(facts.map_err(LoadOwed::Evidence)?);
    }
    let facts: Vec<CarrierFacts<'_>> = tickets
        .iter()
        .zip(&loaded)
        .map(|(ticket, loaded)| loaded.facts(ticket))
        .collect();
    let admitted = choose_carrier(repo_id, &facts).map_err(LoadOwed::Carrier)?;
    // The one admissible ticket is the one whose own facts admit alone the
    // same carrier: found by the rule, never by position.
    let ticket = tickets
        .iter()
        .zip(&loaded)
        .find(|(ticket, loaded)| {
            choose_carrier(repo_id, &[loaded.facts(ticket)]).as_ref() == Ok(&admitted)
        })
        .map(|(ticket, _)| ticket.clone())
        .ok_or(LoadOwed::Evidence(ReadReason::EvidenceIncomplete))?;
    Ok(Admission { ticket, admitted })
}

async fn load_one(
    store: &impl CanonicalStore,
    sources: &Sources<'_>,
    ticket: &CondenseTicket,
    budget: &mut Budget,
) -> Result<Loaded, ReadReason> {
    let native = &ticket.job.session_id;
    let cache = match cache::read_projection(sources.sessions, native, budget) {
        Ok(cache) => Some(cache),
        Err(ReadReason::CacheAbsent) => None,
        Err(reason) => return Err(reason),
    };
    let completion = completion(sources.queue, &ticket.context_id);
    let param = [Value::text(native.clone())];
    let agents = query(store, IDENTITY_AGENTS, &param, budget).await?;
    let agents = agents
        .iter()
        .map(|row| rows::text(row, 0, rows::MAX_AGENT_BYTES))
        .collect::<Result<_, _>>()?;
    let ends = query(store, SESSION_END, &param, budget).await?;
    let ends = ends.iter().map(end_event).collect::<Result<_, _>>()?;
    Ok(Loaded {
        cache,
        completion,
        agents,
        ends,
    })
}

/// The delivery's completion marker in the queue: only a true absence is no
/// completion, and any other answer is unknown, never "not completed".
fn completion(queue: &Namespace, context_id: &str) -> Completion {
    let name = format!("completed-{context_id}.done");
    if queue.recheck().is_err() {
        return Completion::Unknown;
    }
    match queue.dir.symlink_metadata(&name) {
        Ok(_) => Completion::Completed,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Completion::None,
        Err(_) => Completion::Unknown,
    }
}

async fn query(
    store: &impl CanonicalStore,
    sql: &str,
    params: &[Value],
    budget: &mut Budget,
) -> Result<Vec<Row>, ReadReason> {
    let rows = store
        .query(sql, params)
        .await
        .map_err(|_| ReadReason::StoreUnavailable)?;
    budget.charge(&rows)?;
    Ok(rows)
}

/// One end row; a guard sentinel is malformed evidence, never an absent row.
fn end_event(row: &Row) -> Result<EndEvent, ReadReason> {
    Ok(EndEvent {
        idempotency_key: rows::hex64(row, 0)?,
        payload_sha256: rows::hex64(row, 1)?,
    })
}
