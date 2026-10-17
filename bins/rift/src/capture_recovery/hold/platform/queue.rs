//! The admitted member's CURRENT agent-queue journal, held by descriptor: the
//! sibling of the sessions capability, derived the same way and rechecked
//! under the same discipline, so the inventory never lists a directory a
//! resolver could have minted, adopted or moved.
use rift_shadow::hooks::run_state::RunRoot;

use super::sessions::BoundRoot;
use super::EvidenceResult;
use crate::capture_recovery::files::Namespace;
use crate::capture_recovery::hold::OwnHold;
use crate::capture_recovery::plan::Plan;

/// The capability: the hold it was derived from, the exact bound member, the
/// checked moved marker and the agent-queue child opened under it.
pub(crate) struct ReadyQueue<'h>(BoundRoot<'h>);

impl<'h> ReadyQueue<'h> {
    pub(in crate::capture_recovery::hold) fn admit(hold: &'h OwnHold<'h>) -> EvidenceResult<Self> {
        BoundRoot::admit(hold, RunRoot::AgentQueue).map(Self)
    }

    /// Every inventory pass rechecks the hold, the member, the marker and the
    /// queue directory before trusting an entry listed under them.
    pub(crate) fn recheck(&self) -> EvidenceResult<()> {
        self.0.recheck()
    }

    pub(in crate::capture_recovery) fn queue(&self) -> &Namespace {
        self.0.dir()
    }

    /// The Plan the hold was admitted on: the one authority for the member,
    /// the budgets and the private directory this inventory serves.
    pub(in crate::capture_recovery) fn plan(&self) -> &Plan {
        self.0.plan()
    }

    /// The hold's private operation directory, rechecked, for the one record
    /// a selection publishes.
    pub(in crate::capture_recovery) fn private(&self) -> EvidenceResult<&Namespace> {
        self.0.private()
    }
}

impl std::fmt::Debug for ReadyQueue<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReadyQueue")
    }
}
