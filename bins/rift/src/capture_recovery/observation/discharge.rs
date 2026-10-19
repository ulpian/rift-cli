//! The reserved boundary's own queue ticket, discharged once its baseline is
//! durable. The ordinary drain's idempotency check never matches it, because
//! the baseline carries the time it was actually collected rather than the
//! time the old ticket would have written. So the observation stage removes
//! it itself, through the same guarded removal retirement uses: the queued
//! file goes only while its inode and bytes are still the pinned original.
use serde::{Deserialize, Serialize};

use crate::capture_recovery::archive::retire::parked;
use crate::capture_recovery::files::Namespace;
use crate::capture_recovery::hold::{OwnHold, ReadyQueue};
use crate::capture_recovery::pins::{self, Discharge};
use crate::capture_recovery::selection::Expectation;

/// What became of the reserved ticket after its baseline was collected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum ReservedTicket {
    /// The queued file was still the pinned original and is gone.
    Retired,
    /// Another file sits under the ticket's name; it is never removed.
    NameReused,
    /// The pin is absent or no longer proves the original's bytes, so
    /// nothing was removed.
    PinUnverified,
    /// The queue could not be read or changed cleanly; nothing is claimed.
    Unavailable,
}

/// Discharge the reserved ticket the expectation pins. Rerunning after any
/// step finds the ticket already gone and answers `Retired` again.
pub(crate) fn discharge_reserved(
    hold: &OwnHold<'_>,
    queue: &ReadyQueue<'_>,
    private: &Namespace,
    expectation: &Expectation,
) -> ReservedTicket {
    let owner = hold.owner();
    let Ok(scope) = pins::shared_scope(owner, &queue.queue().path, &private.path) else {
        return ReservedTicket::Unavailable;
    };
    let Ok(live) = pins::reopen_expected(owner, &scope, expectation) else {
        return ReservedTicket::PinUnverified;
    };
    match pins::discharge_original(owner, &live, &parked(&expectation.pin_name)) {
        Ok(Discharge::Retired) => ReservedTicket::Retired,
        Ok(Discharge::Different) => ReservedTicket::NameReused,
        Err(_) => ReservedTicket::Unavailable,
    }
}
