use std::time::Duration;

use super::{begin, held_words, release, Member, HOLD_NAME};
use crate::capture_recovery::hold::admit_own_hold;
use crate::capture_recovery::plan::Plan;
use crate::capture_recovery::test_support::{write_record, Fixture};

/// A fixture member with its authored hold and plan taken away, so the job
/// starts from a member no recovery has touched.
fn untouched() -> (Fixture, rift_core::RepoId) {
    let fixture = Fixture::new();
    std::fs::remove_file(fixture.hold_path()).unwrap();
    std::fs::remove_file(fixture.evidence.join("plan.json")).unwrap();
    let repo_id = Plan::decode(&fixture.plan_bytes).unwrap().repo_id().clone();
    (fixture, repo_id)
}

fn member<'a>(fixture: &'a Fixture, repo_id: &'a rift_core::RepoId) -> Member<'a> {
    Member {
        repo_id,
        work_tree: &fixture.work_tree,
        family_dir: fixture.run_root.parent().unwrap(),
    }
}

#[test]
fn a_fresh_job_writes_a_plan_and_a_hold_that_the_admission_accepts() {
    let (f, repo_id) = untouched();
    let job = begin(&f.owner, &member(&f, &repo_id), "fix-1").unwrap();
    assert!(!job.resumed);
    assert!(job
        .evidence
        .starts_with(f.run_root.parent().unwrap().canonicalize().unwrap()));
    assert!(job.evidence.join("plan.json").is_file());
    assert!(f.hold_path().is_file());
    assert!(admit_own_hold(&f.owner, &job.evidence, "fix-1").is_ok());
}

#[test]
fn an_existing_hold_is_resumed_and_never_replaced() {
    let (f, repo_id) = untouched();
    let first = begin(&f.owner, &member(&f, &repo_id), "fix-1").unwrap();
    let hold = std::fs::read(f.hold_path()).unwrap();
    let again = begin(&f.owner, &member(&f, &repo_id), "fix-2").unwrap();
    assert!(again.resumed);
    assert_eq!(again.operation, "fix-1");
    assert_eq!(again.evidence, first.evidence);
    assert_eq!(std::fs::read(f.hold_path()).unwrap(), hold);
    assert!(!again.evidence.with_file_name("fix-2").exists());
}

#[test]
fn release_removes_only_the_hold_this_job_holds_and_keeps_the_plan() {
    let (f, repo_id) = untouched();
    let job = begin(&f.owner, &member(&f, &repo_id), "fix-1").unwrap();
    release(&f.owner, &job).unwrap();
    assert!(!f.hold_path().exists());
    assert!(job.evidence.join("plan.json").is_file());
    release(&f.owner, &job).unwrap();
    write_record(
        &f.owner.git_dir().join(HOLD_NAME),
        b"{\"someone\":\"else\"}",
    );
    assert!(release(&f.owner, &job).is_err());
    assert!(f.hold_path().exists());
}

#[test]
fn the_report_words_name_the_held_operation_and_its_age() {
    let (f, repo_id) = untouched();
    begin(&f.owner, &member(&f, &repo_id), "fix-1").unwrap();
    let written = f
        .hold_path()
        .symlink_metadata()
        .unwrap()
        .modified()
        .unwrap();
    let words = held_words(f.owner.git_dir(), written + Duration::from_secs(90)).unwrap();
    assert!(words.contains("operation fix-1"), "{words}");
    assert!(words.contains("held for 90s"), "{words}");
}

#[test]
fn the_report_words_say_nothing_when_no_hold_is_there() {
    let (f, _) = untouched();
    assert_eq!(
        held_words(f.owner.git_dir(), std::time::SystemTime::now()),
        None
    );
}
