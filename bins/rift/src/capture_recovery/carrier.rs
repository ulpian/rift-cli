//! Carrier choice: which one queued session-end condense may carry a gap
//! artifact, decided purely over facts a loader already read and checked.
//!
//! The selected road (carrier-baseline review, root selection 2026-10-15) is
//! a genuine queued Condense whose present cache is the same identity and was
//! OBSERVED `ending`, whose native session has exactly one capture identity,
//! exactly one stored `session_end` event and no prior completion. Its use is
//! audit-only assembly: choosing it claims no ending episode, finishes no
//! lifecycle and consumes nothing. Every doubt is a typed refusal that keeps
//! the gap owed and visible — never a choice by time, by order or by a
//! synthesized event, because a fabricated end would be historical
//! attribution the evidence does not carry.
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;

use super::archive::Carrier;
use super::eligibility::CacheProjection;

/// The cache phase the first road admits, as the session machine spells it.
const ENDING: &str = "ending";
const ENDED: &str = "ended";

/// Why one queued condense cannot carry, as a static class.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Reason {
    RepoMismatch,
    TicketInvalid,
    CacheAbsent,
    CacheIdentity,
    CacheNotEnding,
    /// A completion marker, an `ended` cache: the carrier is finished and a
    /// gap is never grafted onto it.
    Completed,
    CompletionUnknown,
    IdentityMissing,
    IdentityMismatch,
    /// One native session under more than one agent: refused, never resolved
    /// by choosing one hash.
    IdentityAmbiguous,
    EndMissing,
    /// Two stored end rows, even one key twice: the bounded read cannot name
    /// one event, and the latest is not the current episode.
    EndAmbiguous,
    EndMalformed,
}

/// Why no carrier was chosen; the gap stays owed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CarrierOwed {
    NoCondense,
    /// Every queued condense refused; the count per reason is what a report
    /// names.
    NoneAdmissible(BTreeMap<Reason, u32>),
    /// More than one condense could carry; none is picked.
    Ambiguous {
        admissible: u32,
    },
}

/// Prior completion evidence for one condense delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Completion {
    None,
    Completed,
    Unknown,
}

/// One queued Condense ticket's facts and the digests of its delivery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct QueuedCondense {
    pub(crate) agent: String,
    pub(crate) session_id: String,
    pub(crate) repo_id: String,
    pub(crate) context_sha256: String,
    pub(crate) ticket_sha256: String,
}

/// One stored `session_end` row of the bounded (`LIMIT 2`) native read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EndEvent {
    pub(crate) idempotency_key: String,
    pub(crate) payload_sha256: String,
}

/// Everything read for one queued condense; the loader decides nothing.
pub(crate) struct CarrierFacts<'a> {
    pub(crate) condense: &'a QueuedCondense,
    /// The checked current cache; `None` only for a true absence.
    pub(crate) cache: Option<&'a CacheProjection>,
    pub(crate) completion: Completion,
    /// The agents `capture_session_identity` binds this native session to.
    pub(crate) identity_agents: &'a [String],
    pub(crate) session_end: &'a [EndEvent],
}

/// The chosen carrier and the private evidence it was admitted on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Admitted {
    pub(crate) carrier: Carrier,
    pub(crate) cache_sha256: String,
    pub(crate) end_event_key: String,
    pub(crate) end_payload_sha256: String,
}

/// Exactly one admissible condense of this member, or why the gap stays owed.
pub(crate) fn choose_carrier(
    repo_id: &str,
    facts: &[CarrierFacts<'_>],
) -> Result<Admitted, CarrierOwed> {
    if facts.is_empty() {
        return Err(CarrierOwed::NoCondense);
    }
    let mut refused = BTreeMap::new();
    let mut admitted = Vec::new();
    for one in facts {
        match admit(repo_id, one) {
            Ok(carrier) => admitted.push(carrier),
            Err(reason) => *refused.entry(reason).or_insert(0) += 1,
        }
    }
    match admitted.len() {
        0 => Err(CarrierOwed::NoneAdmissible(refused)),
        1 => admitted.pop().ok_or(CarrierOwed::NoCondense),
        n => Err(CarrierOwed::Ambiguous {
            admissible: u32::try_from(n).unwrap_or(u32::MAX),
        }),
    }
}

/// One condense through every check, cheapest and most decisive first.
fn admit(repo_id: &str, facts: &CarrierFacts<'_>) -> Result<Admitted, Reason> {
    let condense = facts.condense;
    if condense.repo_id.is_empty() || condense.repo_id != repo_id {
        return Err(Reason::RepoMismatch);
    }
    let carrier = Carrier::new(
        &condense.agent,
        &condense.session_id,
        &condense.context_sha256,
        &condense.ticket_sha256,
    )
    .map_err(|_| Reason::TicketInvalid)?;
    check_completion(facts.completion)?;
    let cache_sha256 = check_cache(condense, facts.cache)?;
    check_identity(&condense.agent, facts.identity_agents)?;
    let end = unique_end(facts.session_end)?;
    Ok(Admitted {
        carrier,
        cache_sha256,
        end_event_key: end.idempotency_key.clone(),
        end_payload_sha256: end.payload_sha256.clone(),
    })
}

fn check_completion(completion: Completion) -> Result<(), Reason> {
    match completion {
        Completion::None => Ok(()),
        Completion::Completed => Err(Reason::Completed),
        Completion::Unknown => Err(Reason::CompletionUnknown),
    }
}

/// Present, the same session and agent, and observed `ending`; its digest.
fn check_cache(
    condense: &QueuedCondense,
    cache: Option<&CacheProjection>,
) -> Result<String, Reason> {
    let Some(cache) = cache else {
        return Err(Reason::CacheAbsent);
    };
    if cache.session_id != condense.session_id || cache.agent != condense.agent {
        return Err(Reason::CacheIdentity);
    }
    match cache.state.as_str() {
        ENDING => Ok(cache.cache_sha256.clone()),
        ENDED => Err(Reason::Completed),
        _ => Err(Reason::CacheNotEnding),
    }
}

fn check_identity(agent: &str, agents: &[String]) -> Result<(), Reason> {
    match agents {
        [] => Err(Reason::IdentityMissing),
        [only] if only == agent => Ok(()),
        [_] => Err(Reason::IdentityMismatch),
        _ => Err(Reason::IdentityAmbiguous),
    }
}

fn unique_end(rows: &[EndEvent]) -> Result<&EndEvent, Reason> {
    let [end] = rows else {
        return Err(if rows.is_empty() {
            Reason::EndMissing
        } else {
            Reason::EndAmbiguous
        });
    };
    if !is_hex64(&end.idempotency_key) || !is_hex64(&end.payload_sha256) {
        return Err(Reason::EndMalformed);
    }
    Ok(end)
}

fn is_hex64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
