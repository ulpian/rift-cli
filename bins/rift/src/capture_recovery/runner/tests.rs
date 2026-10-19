use rift_recall_substrate::GixSubstrate;
use rift_shadow::backend::NativeBackend;

use super::*;
use crate::capture_recovery::eligibility::fixtures::Rig;
use crate::capture_recovery::inventory::tests::queue_dir;

fn native(rig: &Rig) -> NativeBackend<rift_git::PlumbingGitWrite> {
    let repo = rift_git::discover(&rig.fixture.work_tree).unwrap();
    NativeBackend::new(rift_git::PlumbingGitWrite::new(repo))
}

fn recover_once(rig: &Rig, hold: &OwnHold<'_>) -> String {
    let repo = rift_git::discover(&rig.fixture.work_tree).unwrap();
    let reader = GixSubstrate::open(&rig.fixture.work_tree).unwrap();
    let backend = native(rig);
    let cx = Context {
        rt: &rig.rt,
        store: rig.store.inner(),
        registry: &rig.registry,
        repo: &repo,
        backend: &backend,
        reader: &reader,
        now: 1_760_000_000_000,
    };
    recover(cx, hold)
}

#[test]
fn an_empty_backlog_publishes_its_selection_and_reports_nothing_to_recover() {
    let rig = Rig::new();
    let hold = rig.admit();
    queue_dir(&rig.fixture);
    assert_eq!(
        recover_once(&rig, &hold),
        "capture recovery: nothing to recover"
    );
    assert!(rig.fixture.evidence.join(INTENT_NAME).exists());
    assert!(!rig.fixture.evidence.join(ARCHIVE_INTENT_NAME).exists());
}

#[test]
fn a_second_walk_over_the_same_records_resumes_and_reports_the_same_words() {
    let rig = Rig::new();
    let hold = rig.admit();
    queue_dir(&rig.fixture);
    let first = recover_once(&rig, &hold);
    let intent = std::fs::read(rig.fixture.evidence.join(INTENT_NAME)).unwrap();
    assert_eq!(recover_once(&rig, &hold), first);
    assert_eq!(
        std::fs::read(rig.fixture.evidence.join(INTENT_NAME)).unwrap(),
        intent
    );
}

#[test]
fn an_unreadable_selection_intent_stops_the_walk_at_the_selection() {
    let rig = Rig::new();
    let hold = rig.admit();
    queue_dir(&rig.fixture);
    crate::capture_recovery::test_support::write_record(
        &rig.fixture.evidence.join(INTENT_NAME),
        b"not a record",
    );
    let words = recover_once(&rig, &hold);
    assert!(words.contains("owed"), "{words}");
    assert!(!rig.fixture.evidence.join(ARCHIVE_INTENT_NAME).exists());
}
