//! The reserved observation: the one current boundary the selection set
//! aside is collected at most once per operation, as the last stage of the
//! hold, so normal capture resumes from a truthful fresh baseline. The attempt
//! is made durable before the collector runs and its completion, naming the
//! checkpoint, after; a rerun that finds the completion does nothing, and one
//! that finds only the attempt looks the checkpoint up by its boundary before
//! it collects again, so the boundary is never collected twice. Once the
//! baseline is durable the reserved ticket itself is discharged, and the
//! completion records what became of it.
use serde::{Deserialize, Serialize};

use crate::capture_recovery::files::Namespace;
use crate::capture_recovery::hold::OwnHold;
use crate::capture_recovery::record::{self, Publication, RecordError};
use crate::capture_recovery::selection::ReservedObservation;

mod discharge;
mod live;

pub(crate) use discharge::ReservedTicket;
pub(crate) use live::BoundaryCollector;

pub(crate) const ATTEMPT_NAME: &str = "observation-attempt.json";
/// The attempt's completion: written once, after the collection is durable.
pub(crate) const COLLECTED_NAME: &str = "observation-collected.json";
/// Five digests and two short names; anything larger is not this record.
const MAX_ATTEMPT_BYTES: usize = 4096;

/// What one bounded collection or read-back found for the reserved key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Collection {
    /// The boundary and its complete collection audit are durable.
    Observed,
    /// Nothing durable under the key.
    Missing,
    /// The bounded reader hit its row ceiling before the audit closed.
    Limited,
    /// The collection or the read itself failed.
    Failed,
}

/// The seam over the boundary collector. Both calls bound every read; only
/// `collect` may scan the working tree.
pub(crate) trait Collector {
    fn collect(&mut self, observation: &ReservedObservation) -> Collection;
    fn read_back(&mut self, observation: &ReservedObservation) -> Collection;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationOutcome {
    /// The selection reserved nothing, so there is nothing to collect.
    NoReservation,
    /// This attempt collected the reserved boundary.
    Observed,
    /// An earlier attempt of this operation collected it; read back, not redone.
    Recognized,
}

/// Why the reserved observation is still owed, as a static class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ObservationOwed {
    HoldUnavailable,
    /// Another attempt record is already there; it is never overwritten.
    AttemptConflict,
    PublishFailed,
    /// The one attempt is spent and found this; it is not retried.
    Spent(Collection),
}

/// The durable promise that the reserved boundary is being collected now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObservationAttempt {
    pub(crate) schema_version: u32,
    pub(crate) operation_id: String,
    pub(crate) plan_sha256: String,
    pub(crate) intent_sha256: String,
    pub(crate) event_key: String,
    pub(crate) receipt_id: String,
    pub(crate) ticket_sha256: String,
}

/// The attempt completed: the checkpoint the reserved boundary became, or
/// `None` where the selection reserved nothing. A separate no-clobber record
/// rather than an edit, because the attempt itself is never rewritten.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ObservationCollected {
    pub(crate) schema_version: u32,
    pub(crate) checkpoint_id: Option<String>,
    /// The reserved ticket's discharge; absent where nothing was reserved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reserved_ticket: Option<ReservedTicket>,
}

/// The reserved boundary with the operation and the intent that set it
/// aside, each borrowed from the published selection intent.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reservation<'a> {
    pub(crate) operation_id: &'a str,
    pub(crate) plan_sha256: &'a str,
    pub(crate) intent_sha256: &'a str,
    pub(crate) observation: &'a ReservedObservation,
}

impl ObservationAttempt {
    fn of(reservation: Reservation<'_>) -> Self {
        let observation = reservation.observation;
        Self {
            schema_version: 1,
            operation_id: reservation.operation_id.to_owned(),
            plan_sha256: reservation.plan_sha256.to_owned(),
            intent_sha256: reservation.intent_sha256.to_owned(),
            event_key: observation.event_key.clone(),
            receipt_id: observation.receipt_id.clone(),
            ticket_sha256: observation.expectation.ticket_sha256.clone(),
        }
    }
}

/// Collect the reserved boundary once: the attempt record first,
/// then the collector. An identical record already there means the attempt
/// was spent, so only the bounded read-back runs.
pub(crate) fn attempt_observation(
    hold: &OwnHold<'_>,
    reservation: Option<Reservation<'_>>,
    collector: &mut impl Collector,
) -> Result<ObservationOutcome, ObservationOwed> {
    hold.check_current()
        .map_err(|_| ObservationOwed::HoldUnavailable)?;
    let queue = hold
        .agent_queue_namespace()
        .map_err(|_| ObservationOwed::HoldUnavailable)?;
    let private = queue
        .private()
        .map_err(|_| ObservationOwed::HoldUnavailable)?;
    let Some(reservation) = reservation else {
        complete(private, None, None)?;
        return Ok(ObservationOutcome::NoReservation);
    };
    let observation = reservation.observation;
    let attempt = ObservationAttempt::of(reservation);
    let outcome = match publish(private, ATTEMPT_NAME, &attempt)? {
        Publication::Fresh => settle(collector.collect(observation), ObservationOutcome::Observed)?,
        Publication::Resumed if completed(private)? => return Ok(ObservationOutcome::Recognized),
        Publication::Resumed => resume(collector, observation)?,
    };
    let ticket = discharge::discharge_reserved(hold, &queue, private, &observation.expectation);
    complete(private, Some(checkpoint_id(observation)), Some(ticket))?;
    Ok(outcome)
}

/// Collect the reserved boundary ahead of the archive, so the recovered gap
/// can ride on it when no condense can: the same attempt record first, and a
/// rerun reads the collected checkpoint back instead of collecting twice. The
/// completion and the reserved ticket's discharge stay with the last stage.
pub(crate) fn collect_baseline(
    hold: &OwnHold<'_>,
    reservation: Reservation<'_>,
    collector: &mut impl Collector,
) -> Result<String, ObservationOwed> {
    hold.check_current()
        .map_err(|_| ObservationOwed::HoldUnavailable)?;
    let queue = hold
        .agent_queue_namespace()
        .map_err(|_| ObservationOwed::HoldUnavailable)?;
    let private = queue
        .private()
        .map_err(|_| ObservationOwed::HoldUnavailable)?;
    let observation = reservation.observation;
    let attempt = ObservationAttempt::of(reservation);
    match publish(private, ATTEMPT_NAME, &attempt)? {
        Publication::Fresh => settle(collector.collect(observation), ObservationOutcome::Observed)?,
        Publication::Resumed => resume(collector, observation)?,
    };
    Ok(checkpoint_id(observation))
}

/// A spent attempt without its completion: the boundary is looked up first,
/// and collected only when nothing durable sits under it.
fn resume(
    collector: &mut impl Collector,
    observation: &ReservedObservation,
) -> Result<ObservationOutcome, ObservationOwed> {
    match collector.read_back(observation) {
        Collection::Missing => settle(collector.collect(observation), ObservationOutcome::Observed),
        found => settle(found, ObservationOutcome::Recognized),
    }
}

/// The checkpoint the reserved boundary is stored under: the same id the
/// normal boundary capture derives from the session, turn and edge.
pub(crate) fn checkpoint_id(observation: &ReservedObservation) -> String {
    let known = &observation.expectation.known_boundary;
    let session_row = rift_shadow::ids::session_row_id(&known.agent, &known.native_session);
    rift_shadow::ids::turn_boundary_snapshot_id(&session_row, known.turn_ordinal, &known.boundary)
}

fn completed(private: &Namespace) -> Result<bool, ObservationOwed> {
    match record::read_private_record(private, COLLECTED_NAME, MAX_ATTEMPT_BYTES) {
        Ok(_) => Ok(true),
        Err(RecordError::Absent) => Ok(false),
        Err(_) => Err(ObservationOwed::PublishFailed),
    }
}

fn complete(
    private: &Namespace,
    checkpoint_id: Option<String>,
    reserved_ticket: Option<ReservedTicket>,
) -> Result<(), ObservationOwed> {
    let done = ObservationCollected {
        schema_version: 1,
        checkpoint_id,
        reserved_ticket,
    };
    publish(private, COLLECTED_NAME, &done).map(|_| ())
}

fn settle(
    found: Collection,
    done: ObservationOutcome,
) -> Result<ObservationOutcome, ObservationOwed> {
    match found {
        Collection::Observed => Ok(done),
        other => Err(ObservationOwed::Spent(other)),
    }
}

fn publish(
    private: &Namespace,
    name: &str,
    value: &impl Serialize,
) -> Result<Publication, ObservationOwed> {
    let bytes = serde_json::to_vec(value).map_err(|_| ObservationOwed::PublishFailed)?;
    record::publish_private_record(private, name, MAX_ATTEMPT_BYTES, &bytes, &mut |_| Ok(()))
        .map_err(|error| match error {
            RecordError::Unavailable => ObservationOwed::HoldUnavailable,
            RecordError::Conflict => ObservationOwed::AttemptConflict,
            RecordError::Failed | RecordError::Resource | RecordError::Absent => {
                ObservationOwed::PublishFailed
            }
        })
}

#[cfg(test)]
mod tests;
