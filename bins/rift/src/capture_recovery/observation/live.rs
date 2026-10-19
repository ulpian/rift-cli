//! The production collector: the reserved boundary goes through the same
//! boundary capture the worker runs for any queued boundary, stamped by the
//! real collection clock, so the checkpoint it leaves is a current
//! observation and never a historical one.
use rift_shadow::Boundary;
use rift_store::{BoundedBoundaryObservation, SqliteStore};

use super::{checkpoint_id, Collection, Collector};
use crate::capture_recovery::selection::ReservedObservation;

/// Rows the read-back checks before it calls the audit limited. Larger than
/// the doctor's spot check because this one answers for a whole working set.
const ROW_LIMIT: usize = 4096;

pub(crate) struct BoundaryCollector<'a> {
    pub(crate) rt: &'a tokio::runtime::Runtime,
    pub(crate) store: &'a SqliteStore,
    pub(crate) repo: &'a rift_git::RepoPaths,
    pub(crate) reader: &'a rift_git::GixBlobReader,
}

impl Collector for BoundaryCollector<'_> {
    fn collect(&mut self, observation: &ReservedObservation) -> Collection {
        let known = &observation.expectation.known_boundary;
        let Some(boundary) = edge(&known.boundary) else {
            return Collection::Failed;
        };
        // An unborn branch at the event stays unborn only if the reader still
        // says so; otherwise the diffs start from the commit the tree is on.
        let head = match &known.head_oid {
            Some(oid) => Some(oid.clone()),
            None => match self.reader.head_hex() {
                Ok(head) => head,
                Err(_) => return Collection::Failed,
            },
        };
        let collected = self.rt.block_on(rift_shadow::capture_boundary_with_clock(
            self.store,
            self.repo,
            self.reader,
            head.as_deref(),
            &known.agent,
            &known.native_session,
            known.turn_ordinal,
            boundary,
            known.captured_at,
            rift_shadow::boundary_collection_time,
        ));
        match collected {
            Ok(()) => self.read_back(observation),
            Err(_) => Collection::Failed,
        }
    }

    fn read_back(&mut self, observation: &ReservedObservation) -> Collection {
        let id = checkpoint_id(observation);
        let found = self
            .rt
            .block_on(rift_store::bounded_turn_boundary_observation(
                self.store, &id, ROW_LIMIT,
            ));
        match found {
            Ok(BoundedBoundaryObservation::Observed(_)) => Collection::Observed,
            Ok(BoundedBoundaryObservation::Missing) => Collection::Missing,
            Ok(BoundedBoundaryObservation::Limited) => Collection::Limited,
            Err(_) => Collection::Failed,
        }
    }
}

fn edge(spelled: &str) -> Option<Boundary> {
    match spelled {
        "start" => Some(Boundary::Start),
        "end" => Some(Boundary::End),
        _ => None,
    }
}
