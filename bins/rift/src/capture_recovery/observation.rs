//! The reserved observation: the one current boundary the selection set
//! aside is collected at most once per operation. The attempt is made durable
//! before the collector runs, so a crash after it can only read back what
//! that collection left; it never collects the same boundary a second time
//! and never relabels a later collection as the reserved one.
use serde::{Deserialize, Serialize};

use crate::capture_recovery::files::Namespace;
use crate::capture_recovery::hold::OwnHold;
use crate::capture_recovery::record::{self, Publication, RecordError};
use crate::capture_recovery::selection::ReservedObservation;

pub(crate) const ATTEMPT_NAME: &str = "observation-attempt.json";
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
    let Some(reservation) = reservation else {
        return Ok(ObservationOutcome::NoReservation);
    };
    let observation = reservation.observation;
    hold.check_current()
        .map_err(|_| ObservationOwed::HoldUnavailable)?;
    let queue = hold
        .agent_queue_namespace()
        .map_err(|_| ObservationOwed::HoldUnavailable)?;
    let private = queue
        .private()
        .map_err(|_| ObservationOwed::HoldUnavailable)?;
    let attempt = ObservationAttempt::of(reservation);
    match publish(private, &attempt)? {
        Publication::Fresh => settle(collector.collect(observation), ObservationOutcome::Observed),
        Publication::Resumed => settle(
            collector.read_back(observation),
            ObservationOutcome::Recognized,
        ),
    }
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
    attempt: &ObservationAttempt,
) -> Result<Publication, ObservationOwed> {
    let bytes = serde_json::to_vec(attempt).map_err(|_| ObservationOwed::PublishFailed)?;
    record::publish_private_record(
        private,
        ATTEMPT_NAME,
        MAX_ATTEMPT_BYTES,
        &bytes,
        &mut |_| Ok(()),
    )
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
