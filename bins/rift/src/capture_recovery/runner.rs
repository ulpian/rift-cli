//! The production stage runner: each controller stage bound to the entry
//! point that already owns it. Nothing here decides a stage's outcome; it
//! only hands over what the previous stage produced in this process, or, when
//! a crash left it behind, rebuilds that from the stage's own durable record.
use std::collections::BTreeSet;

use rift_recall_substrate::SubstrateReader;
use rift_shadow::backend::ShadowBackend;
use rift_shadow::hooks::strategy::{ArchiveDecision, ArchiveReason, StrategyKind};
use rift_shadow::hooks::AdapterRegistry;
use rift_store::SqliteStore;
use sha2::{Digest, Sha256};

use super::archive::resume::resume_intent;
use super::archive::retire::{retire, write_retire_intent};
use super::archive::run::{archive_recovered, RecoveryRun};
use super::archive::{Delegation, RecoveryBackend, INTENT_NAME as ARCHIVE_INTENT_NAME};
use super::carrier::load::{admit_carrier, Sources};
use super::controller::{drive, owed_words, HoldProgress, Recovery};
use super::controller::{Refused, Stage, Stages};
use super::cut::{pin_all, resume_pins, CutOutcome, Pinned, MAX_CUT_BYTES};
use super::gap::{publish_gap, GapOutcome};
use super::hold::OwnHold;
use super::inventory::enumerate;
use super::observation::{attempt_observation, BoundaryCollector, ObservationOwed, Reservation};
use super::record::{self, RecordError};
use super::selection::intent::SelectionIntent;
use super::selection::{select, Selected, INTENT_NAME};
use rift_core::boundary_gap::GapBatch;

use crate::hooks::Historical;

/// A complete inventory with no historical member: nothing is owed.
const NOTHING_TO_RECOVER: &str = "nothing-to-recover";
const ABSENT: &str = "record-absent";

/// What every stage reads from outside the hold: one runtime, the open
/// projection, the adapters, the native backend and the native tree reader.
pub(crate) struct Context<'a, B, R> {
    pub(crate) rt: &'a tokio::runtime::Runtime,
    pub(crate) store: &'a SqliteStore,
    pub(crate) registry: &'a AdapterRegistry,
    pub(crate) repo: &'a rift_git::RepoPaths,
    pub(crate) backend: &'a B,
    pub(crate) reader: &'a R,
    pub(crate) now: i64,
}

/// The stages of one admitted hold, carrying each stage's result forward
/// within this process only.
pub(crate) struct LiveStages<'a, B, R> {
    cx: Context<'a, B, R>,
    hold: &'a OwnHold<'a>,
    selected: Option<Selected>,
    pinned: Option<Pinned>,
    /// The ticket names the selection classified historical, once a stage
    /// has read them from this run's selection or the durable intent.
    historical: Option<BTreeSet<String>>,
}

impl<'a, B: ShadowBackend, R: SubstrateReader> LiveStages<'a, B, R> {
    pub(crate) fn new(cx: Context<'a, B, R>, hold: &'a OwnHold<'a>) -> Self {
        Self {
            cx,
            hold,
            selected: None,
            pinned: None,
            historical: None,
        }
    }

    fn note_historical(&mut self, intent: &SelectionIntent) {
        let names = intent.historical.iter().map(|e| e.source_name.clone());
        self.historical = Some(names.collect());
    }

    fn select(&mut self) -> Result<(), Refused> {
        let cx = &self.cx;
        let selected = cx
            .rt
            .block_on(select(cx.store, self.hold, cx.registry))
            .map_err(|deferred| Refused(deferred.reason()))?;
        self.note_historical(&selected.intent);
        self.selected = Some(selected);
        Ok(())
    }

    /// The pins: from this run's selection when there is one, else resumed
    /// from the durable selection intent and validated again.
    fn pin(&mut self) -> Result<&Pinned, Refused> {
        if self.pinned.is_none() {
            let pinned = if let Some(selected) = self.selected.take() {
                pin_all(self.hold, selected)
            } else {
                let intent = self.selection_intent()?;
                self.note_historical(&intent);
                resume_pins(self.hold, &intent)
            };
            self.pinned = Some(pinned.map_err(|_| Refused("cut-owed"))?);
        }
        self.pinned.as_ref().ok_or(Refused("cut-owed"))
    }

    fn cut(&mut self) -> Result<(), Refused> {
        match self.pin()?.cut {
            CutOutcome::NoHistoricalCut => Err(Refused(NOTHING_TO_RECOVER)),
            CutOutcome::Published { .. } | CutOutcome::Resumed { .. } => Ok(()),
        }
    }

    /// The gap record publishes once; a later call rebuilds the same batch
    /// from it with its first recorded time.
    fn batch(&mut self) -> Result<GapBatch, Refused> {
        let now = self.cx.now;
        let hold = self.hold;
        match publish_gap(hold, self.pin()?, || now) {
            Ok(GapOutcome::Published(batch) | GapOutcome::Resumed(batch)) => Ok(batch),
            Ok(GapOutcome::NoHistoricalCut) => Err(Refused(NOTHING_TO_RECOVER)),
            Err(_) => Err(Refused("gap-owed")),
        }
    }

    /// Archive once: an intent already on disk means the archive call was
    /// made, so only the receipt is resumed against the native tree.
    fn archive(&mut self) -> Result<(), Refused> {
        if !self.private_record(ARCHIVE_INTENT_NAME)? {
            self.archive_carrier()?;
        }
        resume_intent(self.hold, self.cx.reader)
            .map(|_| ())
            .map_err(|_| Refused("receipt-owed"))
    }

    fn archive_carrier(&mut self) -> Result<(), Refused> {
        let batch = self.batch()?;
        let unavailable = |_| Refused("archive-evidence-unavailable");
        let queue = self.hold.agent_queue_namespace().map_err(unavailable)?;
        let sessions = self.hold.sessions_namespace().map_err(unavailable)?;
        let inventory = enumerate(&queue).map_err(|_| Refused("inventory-owed"))?;
        let sources = Sources {
            queue: queue.queue(),
            sessions: sessions.sessions(),
        };
        let cx = &self.cx;
        let repo_id = sessions.repo_id().as_str();
        let admission = cx
            .rt
            .block_on(admit_carrier(
                cx.store,
                &sources,
                repo_id,
                &inventory.condenses,
            ))
            .map_err(|_| Refused("carrier-owed"))?;
        let backend =
            RecoveryBackend::new(cx.backend, self.hold, admission.admitted.carrier, batch)
                .map_err(|_| Refused("artifact-invalid"))?;
        let run = RecoveryRun {
            rt: cx.rt,
            store: cx.store,
            repo: cx.repo,
            commit: None,
            now: cx.now,
        };
        archive_recovered(&run, &backend, recovery_decision())
            .map_err(|_| Refused("archive-failed"))?;
        match backend.delegation() {
            Some(Delegation::Delegated { .. }) => Ok(()),
            _ => Err(Refused("archive-refused")),
        }
    }

    /// Collect the reserved boundary the durable selection intent names, as
    /// the hold's last act; with no reservation only the completion is written.
    fn observe(&mut self) -> Result<(), Refused> {
        let bytes = self.read_private(INTENT_NAME)?;
        let intent =
            SelectionIntent::decode(&bytes).map_err(|_| Refused("selection-intent-invalid"))?;
        let intent_sha256 = format!("{:x}", Sha256::digest(&bytes));
        let reservation = intent
            .reserved_observation
            .as_ref()
            .map(|observation| Reservation {
                operation_id: &intent.operation_id,
                plan_sha256: &intent.plan_sha256,
                intent_sha256: &intent_sha256,
                observation,
            });
        let cx = &self.cx;
        let reader = rift_git::GixBlobReader::open(&cx.repo.work_tree)
            .map_err(|_| Refused("observation-reader-unavailable"))?;
        let mut collector = BoundaryCollector {
            rt: cx.rt,
            store: cx.store,
            repo: cx.repo,
            reader: &reader,
        };
        attempt_observation(self.hold, reservation, &mut collector)
            .map(|_| ())
            .map_err(|owed| Refused(observation_reason(owed)))
    }

    fn selection_intent(&self) -> Result<SelectionIntent, Refused> {
        let bytes = self.read_private(INTENT_NAME)?;
        SelectionIntent::decode(&bytes).map_err(|_| Refused("selection-intent-invalid"))
    }

    fn private_record(&self, name: &str) -> Result<bool, Refused> {
        match self.read_private(name) {
            Ok(_) => Ok(true),
            Err(Refused(ABSENT)) => Ok(false),
            Err(refused) => Err(refused),
        }
    }

    fn read_private(&self, name: &str) -> Result<Vec<u8>, Refused> {
        let queue = self
            .hold
            .agent_queue_namespace()
            .map_err(|_| Refused("hold-unavailable"))?;
        let private = queue.private().map_err(|_| Refused("hold-unavailable"))?;
        match record::read_private_record(private, name, MAX_CUT_BYTES) {
            Ok(bytes) => Ok(bytes),
            Err(RecordError::Absent) => Err(Refused(ABSENT)),
            Err(_) => Err(Refused("record-unreadable")),
        }
    }
}

impl<B: ShadowBackend, R: SubstrateReader> Stages for LiveStages<'_, B, R> {
    fn run(&mut self, stage: Stage) -> Result<(), Refused> {
        match stage {
            Stage::Selection => self.select(),
            Stage::Cut => self.cut(),
            Stage::Gap => self.batch().map(|_| ()),
            Stage::Archive => self.archive(),
            Stage::RetireIntent => write_retire_intent(self.hold)
                .map(|_| ())
                .map_err(|_| Refused("retire-intent-owed")),
            Stage::Retirement => retire(self.hold)
                .map(|_| ())
                .map_err(|_| Refused("retirement-owed")),
            Stage::Observation => self.observe(),
        }
    }
}

/// Walk every stage the hold's records do not yet prove and say, in plain
/// words, how the walk ended. The caller releases the hold either way.
pub(crate) fn recover<B: ShadowBackend, R: SubstrateReader>(
    cx: Context<'_, B, R>,
    hold: &OwnHold<'_>,
) -> String {
    recover_naming_historical(cx, hold).words
}

/// How one recovery walk ended, and the boundary tickets the rest of a
/// `--fix` run must leave to it.
pub(crate) struct Recovered {
    pub(crate) words: String,
    pub(crate) historical: Historical,
}

/// [`recover`], also naming what its selection classified historical. A walk
/// that owed before any selection was read names nothing as proven ordinary.
pub(crate) fn recover_naming_historical<B: ShadowBackend, R: SubstrateReader>(
    cx: Context<'_, B, R>,
    hold: &OwnHold<'_>,
) -> Recovered {
    let mut stages = LiveStages::new(cx, hold);
    let recovery = drive(&mut HoldProgress { hold }, &mut stages);
    let historical = match (&recovery, stages.historical) {
        (_, Some(names)) => Historical::Named(names),
        (
            Recovery::Owed {
                reason: NOTHING_TO_RECOVER,
                ..
            },
            None,
        ) => Historical::Named(BTreeSet::new()),
        (_, None) => Historical::Unclassified,
    };
    Recovered {
        words: recovery_words(recovery),
        historical,
    }
}

fn recovery_words(recovery: Recovery) -> String {
    match recovery {
        Recovery::Complete => {
            "capture recovery complete: the backlog is archived, its queue files retired and the current boundary collected"
                .to_owned()
        }
        Recovery::Owed {
            reason: NOTHING_TO_RECOVER,
            ..
        } => "capture recovery: nothing to recover".to_owned(),
        owed => owed_words(owed),
    }
}

fn observation_reason(owed: ObservationOwed) -> &'static str {
    match owed {
        ObservationOwed::HoldUnavailable => "hold-unavailable",
        ObservationOwed::AttemptConflict => "observation-attempt-conflict",
        ObservationOwed::PublishFailed => "observation-unwritten",
        ObservationOwed::Spent(_) => "observation-spent",
    }
}

/// The recovery archives as a manual commit would: the carrier is a finished
/// condense, never a live turn.
fn recovery_decision() -> ArchiveDecision {
    ArchiveDecision {
        archive: true,
        reason: ArchiveReason::Commit,
        strategy: StrategyKind::ManualCommit,
    }
}

#[cfg(test)]
mod tests;
