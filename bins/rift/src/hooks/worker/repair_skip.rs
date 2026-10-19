//! The boundary tickets one `rift doctor --fix` run leaves to capture
//! recovery. Recovery runs first in that run; the tickets its eligibility
//! classified as historical are discharged by its gap, or stay owed and
//! visible when it could not finish. Replaying them one by one through a
//! repair drain is the sequential historical replay recovery exists to
//! replace, so every repair drain of the run passes them by.
//!
//! The set lives in a process-wide slot for the length of the run rather than
//! being threaded through every leg, because unrelated legs (the queue repair,
//! the session sweep's rescue) reach the same repair drain through their own
//! callers. The hook worker runs in its own process and never sees the slot,
//! so ordinary capture is unchanged.
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;

use crate::agent_queue::{AgentJob, JobKind};

/// What a recovery run left for the repair drains to pass by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Historical {
    /// The ticket file names recovery's eligibility classified historical.
    Named(BTreeSet<String>),
    /// Recovery stopped before it classified anything: no boundary ticket is
    /// proven ordinary, so none is replayed by this run.
    Unclassified,
}

static LEFT_TO_RECOVERY: Mutex<Option<Historical>> = Mutex::new(None);

/// Empties the slot when dropped, so a run's set never outlives the run.
#[must_use]
pub(crate) struct Leave(());

impl Drop for Leave {
    fn drop(&mut self) {
        set(None);
    }
}

/// Hold `historical` back from every repair drain until the guard drops.
pub(crate) fn leave_to_recovery(historical: Historical) -> Leave {
    set(Some(historical));
    Leave(())
}

fn set(value: Option<Historical>) {
    if let Ok(mut slot) = LEFT_TO_RECOVERY.lock() {
        *slot = value;
    }
}

/// Whether a repair drain must pass this delivery by. Only boundary tickets
/// are ever left; every other job drains as before.
pub(super) fn left_to_recovery(path: &Path, job: &AgentJob) -> bool {
    if !matches!(job.kind, JobKind::BoundarySnapshot(_)) {
        return false;
    }
    let Ok(slot) = LEFT_TO_RECOVERY.lock() else {
        return false;
    };
    match &*slot {
        None => false,
        Some(Historical::Unclassified) => true,
        Some(Historical::Named(names)) => path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| names.contains(name)),
    }
}
