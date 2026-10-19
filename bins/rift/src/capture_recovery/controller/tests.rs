//! The walk over durable records: each stage runs only while its record is
//! absent, a crash after any stage resumes at the next, and a refusal stops
//! the walk with the gap owed and named.
use std::cell::RefCell;
use std::rc::Rc;

use super::*;
use crate::agent_queue::BoundaryEdge::Start;
use crate::capture_recovery::eligibility::fixtures::Rig;
use crate::capture_recovery::inventory::tests::enqueue;
use crate::capture_recovery::test_support::write_record;

/// The stages whose records are durable, shared by the probe and the runner.
#[derive(Default)]
struct Disk(Rc<RefCell<Vec<Stage>>>);

impl Progress for Disk {
    fn done(&mut self, stage: Stage) -> Result<bool, Unreadable> {
        Ok(self.0.borrow().contains(&stage))
    }
}

/// Runs a stage by making its record durable, unless told to stop there.
struct Runner {
    disk: Rc<RefCell<Vec<Stage>>>,
    runs: Vec<Stage>,
    crash_after: Option<Stage>,
    refuse: Option<Stage>,
}

impl Runner {
    fn over(disk: &Disk) -> Self {
        Self {
            disk: Rc::clone(&disk.0),
            runs: Vec::new(),
            crash_after: None,
            refuse: None,
        }
    }
}

impl Stages for Runner {
    fn run(&mut self, stage: Stage) -> Result<(), Refused> {
        self.runs.push(stage);
        if self.refuse == Some(stage) {
            return Err(Refused("fixture-refusal"));
        }
        self.disk.borrow_mut().push(stage);
        if self.crash_after == Some(stage) {
            return Err(Refused("crashed"));
        }
        Ok(())
    }
}

#[test]
fn capture_recovery_controller_runs_every_stage_once_in_order() {
    let mut disk = Disk::default();
    let mut runner = Runner::over(&disk);
    assert_eq!(drive(&mut disk, &mut runner), Recovery::Complete);
    assert_eq!(runner.runs, ORDER.to_vec());
}

#[test]
fn capture_recovery_controller_resumes_at_the_next_stage_after_a_crash_between_any_two() {
    for crashed in ORDER {
        let mut disk = Disk::default();
        let mut first = Runner::over(&disk);
        first.crash_after = Some(crashed);
        assert!(
            matches!(drive(&mut disk, &mut first), Recovery::Owed { stage, .. } if stage == crashed)
        );
        let mut second = Runner::over(&disk);
        assert_eq!(drive(&mut disk, &mut second), Recovery::Complete);
        let mut all = first.runs;
        all.extend(second.runs);
        assert_eq!(
            all,
            ORDER.to_vec(),
            "crash after {crashed:?} ran a stage twice"
        );
    }
}

#[test]
fn capture_recovery_controller_stops_at_a_refusal_and_runs_nothing_after_it() {
    let mut disk = Disk::default();
    let mut runner = Runner::over(&disk);
    runner.refuse = Some(Stage::Archive);
    let stopped = drive(&mut disk, &mut runner);
    assert_eq!(
        stopped,
        Recovery::Owed {
            stage: Stage::Archive,
            reason: "fixture-refusal"
        }
    );
    assert_eq!(runner.runs.last(), Some(&Stage::Archive));
    assert_eq!(
        owed_words(stopped),
        "capture recovery owed at archive: fixture-refusal; the gap stays owed"
    );
    assert_eq!(
        status_words(&mut disk),
        "capture recovery in progress: 3 of 6 stages durable, next archive"
    );
}

/// A stage that returns success but leaves no record has not finished.
struct Forgetful(Vec<Stage>);

impl Stages for Forgetful {
    fn run(&mut self, stage: Stage) -> Result<(), Refused> {
        self.0.push(stage);
        Ok(())
    }
}

#[test]
fn capture_recovery_controller_never_advances_on_a_return_value_alone() {
    let mut disk = Disk::default();
    let mut runner = Forgetful(Vec::new());
    assert_eq!(
        drive(&mut disk, &mut runner),
        Recovery::Owed {
            stage: Stage::Selection,
            reason: "not-durable"
        }
    );
    assert_eq!(runner.0, vec![Stage::Selection]);
}

#[test]
fn capture_recovery_doctor_words_read_the_hold_records_without_writing_any() {
    let rig = Rig::new();
    // The hold reads its private directory through the live queue root.
    enqueue(&rig.fixture, "old-a", 1, Start);
    let hold = rig.admit();
    let before = std::fs::read_dir(&rig.fixture.evidence).unwrap().count();
    let mut progress = HoldProgress { hold: &hold };
    assert_eq!(
        status_words(&mut progress),
        "capture recovery: none in progress"
    );
    write_record(&rig.fixture.evidence.join(INTENT_NAME), b"{}");
    write_record(&rig.fixture.evidence.join(CUT_NAME), b"{}");
    assert_eq!(
        status_words(&mut progress),
        "capture recovery in progress: 2 of 6 stages durable, next gap"
    );
    std::os::unix::fs::symlink("elsewhere", rig.fixture.evidence.join(GAP_NAME)).unwrap();
    assert_eq!(
        status_words(&mut progress),
        "capture recovery owed: the gap record is unreadable"
    );
    let after = std::fs::read_dir(&rig.fixture.evidence).unwrap().count();
    // The three records this test wrote, and nothing the probe wrote.
    assert_eq!(after, before + 3);
}
