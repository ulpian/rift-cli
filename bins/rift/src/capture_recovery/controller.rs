//! The recovery controller: every stage in order, each skipped once its own
//! durable record proves it finished. The records are the only progress the
//! controller trusts, so a crash between any two stages resumes at the next
//! and a finished stage is never run again; a stage's return value alone
//! never advances it. A refusal stops the walk where it is, the gap owed and
//! named, and nothing after it runs.
use crate::capture_recovery::archive::resume::RECEIPT_NAME;
use crate::capture_recovery::archive::retire::{retirement_report, SlotState, RETIRE_NAME};
use crate::capture_recovery::cut::{CUT_NAME, MAX_CUT_BYTES};
use crate::capture_recovery::gap::GAP_NAME;
use crate::capture_recovery::hold::OwnHold;
use crate::capture_recovery::record::{self, RecordError};
use crate::capture_recovery::selection::INTENT_NAME;

/// The durable stages, in the only order they may run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Selection,
    Cut,
    Gap,
    Archive,
    RetireIntent,
    Retirement,
}

pub(crate) const ORDER: [Stage; 6] = [
    Stage::Selection,
    Stage::Cut,
    Stage::Gap,
    Stage::Archive,
    Stage::RetireIntent,
    Stage::Retirement,
];

/// A stage's record is there but cannot be read as one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Unreadable;

/// A stage refused, with the static reason its own owed class names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Refused(pub(crate) &'static str);

/// What the durable records say about each stage; never writes.
pub(crate) trait Progress {
    fn done(&mut self, stage: Stage) -> Result<bool, Unreadable>;
}

/// Each stage's entry point; every one resumes from its own record.
pub(crate) trait Stages {
    fn run(&mut self, stage: Stage) -> Result<(), Refused>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Recovery {
    Complete,
    Owed { stage: Stage, reason: &'static str },
}

/// The records under the hold's private directory, read-only.
pub(crate) struct HoldProgress<'a, 'h> {
    pub(crate) hold: &'a OwnHold<'h>,
}

impl Stage {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Stage::Selection => "selection",
            Stage::Cut => "cut",
            Stage::Gap => "gap",
            Stage::Archive => "archive",
            Stage::RetireIntent => "retire-intent",
            Stage::Retirement => "retirement",
        }
    }

    fn record(self) -> Option<&'static str> {
        match self {
            Stage::Selection => Some(INTENT_NAME),
            Stage::Cut => Some(CUT_NAME),
            Stage::Gap => Some(GAP_NAME),
            Stage::Archive => Some(RECEIPT_NAME),
            Stage::RetireIntent => Some(RETIRE_NAME),
            Stage::Retirement => None,
        }
    }
}

impl Progress for HoldProgress<'_, '_> {
    fn done(&mut self, stage: Stage) -> Result<bool, Unreadable> {
        let Some(name) = stage.record() else {
            return self.all_retired();
        };
        let queue = self.hold.agent_queue_namespace().map_err(|_| Unreadable)?;
        let private = queue.private().map_err(|_| Unreadable)?;
        // Every record here is bounded by the Cut's own ceiling, the largest.
        match record::read_private_record(private, name, MAX_CUT_BYTES) {
            Ok(_) => Ok(true),
            Err(RecordError::Absent) => Ok(false),
            Err(_) => Err(Unreadable),
        }
    }
}

impl HoldProgress<'_, '_> {
    /// Retirement has no record of its own: it is done when every slot of
    /// the durable intent reads retired.
    fn all_retired(&self) -> Result<bool, Unreadable> {
        let report = retirement_report(self.hold).map_err(|_| Unreadable)?;
        Ok(report.iter().all(|slot| slot.state == SlotState::Retired))
    }
}

/// Run every stage its record does not yet prove, in order, and stop at the
/// first that refuses or leaves no record behind.
pub(crate) fn drive(progress: &mut impl Progress, stages: &mut impl Stages) -> Recovery {
    for stage in ORDER {
        match progress.done(stage) {
            Ok(true) => continue,
            Ok(false) => {}
            Err(Unreadable) => return owed(stage, "record-unreadable"),
        }
        if let Err(Refused(reason)) = stages.run(stage) {
            return owed(stage, reason);
        }
        // The record, not the return, is the proof the stage finished.
        if progress.done(stage) != Ok(true) {
            return owed(stage, "not-durable");
        }
    }
    Recovery::Complete
}

fn owed(stage: Stage, reason: &'static str) -> Recovery {
    Recovery::Owed { stage, reason }
}

/// The report-only words for `rift doctor`: it reads the records and never
/// runs, publishes or renames anything.
pub(crate) fn status_words(progress: &mut impl Progress) -> String {
    let mut finished = 0;
    for stage in ORDER {
        match progress.done(stage) {
            Ok(true) => finished += 1,
            Ok(false) => break,
            Err(Unreadable) => {
                return format!(
                    "capture recovery owed: the {} record is unreadable",
                    stage.name()
                );
            }
        }
    }
    match ORDER.get(finished) {
        None => "capture recovery complete: every selected ticket retired".to_owned(),
        Some(_) if finished == 0 => "capture recovery: none in progress".to_owned(),
        Some(next) => format!(
            "capture recovery in progress: {finished} of {} stages durable, next {}",
            ORDER.len(),
            next.name()
        ),
    }
}

/// The words for a walk that stopped: the stage, its reason, and that the
/// gap stays owed until a rerun finishes it.
pub(crate) fn owed_words(recovery: Recovery) -> String {
    match recovery {
        Recovery::Complete => "capture recovery complete".to_owned(),
        Recovery::Owed { stage, reason } => {
            format!(
                "capture recovery owed at {}: {reason}; the gap stays owed",
                stage.name()
            )
        }
    }
}

#[cfg(test)]
mod tests;
