//! The move of a member's run state out of its git dir: first move, crash and
//! re-run, an older binary writing after the move, concurrent movers.

use std::path::{Path, PathBuf};

use rift_shadow::hooks::run_state::{RunRoot, MOVED_MARKER, MOVE_LOCK};
use rift_shadow::hooks::session_cache::{self, SessionCache};

use super::{adopt, copy_across, MoveCtx, MoveReport};

#[test]
fn simultaneous_os_lock_contenders_hold_at_most_one_move_owner() {
    let trees = Trees::new();
    let barrier = std::sync::Barrier::new(3);
    let held = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            barrier.wait();
            super::MoveLock::take(&trees.run_root, 601_000).unwrap()
        });
        let b = scope.spawn(|| {
            barrier.wait();
            super::MoveLock::take(&trees.run_root, 601_000).unwrap()
        });
        barrier.wait();
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(held.iter().filter(|owner| owner.is_some()).count(), 1);
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum MigrationPausePoint {
    TargetAbsent,
    QueuePublished,
    SessionPublished,
    BeforeMarker,
}

type MigrationPause = (MigrationPausePoint, Box<dyn FnOnce()>);

thread_local! {
    static MIGRATION_PAUSE: std::cell::RefCell<Option<MigrationPause>> = const {
        std::cell::RefCell::new(None)
    };
}

struct MigrationPauseGuard(Option<MigrationPause>);

impl Drop for MigrationPauseGuard {
    fn drop(&mut self) {
        MIGRATION_PAUSE.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

fn install_migration_pause(
    point: MigrationPausePoint,
    callback: impl FnOnce() + 'static,
) -> MigrationPauseGuard {
    MigrationPauseGuard(
        MIGRATION_PAUSE.with(|slot| slot.replace(Some((point, Box::new(callback))))),
    )
}

pub(super) fn pause_migration(point: MigrationPausePoint) {
    let callback = MIGRATION_PAUSE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_some_and(|(wanted, _)| *wanted == point) {
            slot.take()
        } else {
            None
        }
    });
    if let Some((_, callback)) = callback {
        callback();
    }
}

fn migration_ticket(work_tree: &str, queued_at_ms: i64) -> crate::agent_queue::AgentJob {
    crate::agent_queue::AgentJob {
        schema_version: 1,
        work_tree: PathBuf::from(work_tree),
        queued_at_ms,
        attempts: 0,
        kind: crate::agent_queue::JobKind::Condense {
            session_id: "same-session".into(),
            agent: "claude-code".into(),
            repo_id: "fixture-repo".into(),
        },
    }
}

fn assert_migration_preserves_a_new_publication(point: MigrationPausePoint) {
    let trees = Trees::new();
    let first = migration_ticket("/original-worktree", 1_000);
    let second = migration_ticket("/replacement-worktree", 2_000);
    let old = crate::agent_queue::enqueue_in(&trees.old(RunRoot::AgentQueue), &first).unwrap();
    let target_dir = trees.new_tree(RunRoot::AgentQueue);
    let target = target_dir.join(old.file_name().unwrap());
    if point == MigrationPausePoint::QueuePublished {
        crate::agent_queue::enqueue_in(&target_dir, &first).unwrap();
    }
    let published = std::rc::Rc::new(std::cell::RefCell::new(None));
    let received = published.clone();
    let expected = second.clone();
    let callback = move || {
        let path = crate::agent_queue::enqueue_in(&target_dir, &second).unwrap();
        assert_eq!(
            serde_json::from_slice::<crate::agent_queue::AgentJob>(&std::fs::read(&path).unwrap())
                .unwrap(),
            second,
            "the replacement publication completed inside the critical window"
        );
        received.replace(Some(path));
    };
    let _publication;
    let _migration;
    if point == MigrationPausePoint::TargetAbsent {
        _publication = Some(crate::capture_queue::delivery::pause_publication(callback));
        _migration = None;
    } else {
        _publication = None;
        _migration = Some(install_migration_pause(point, callback));
    }
    let outcome = adopt(&trees.ctx(NOW));
    let new_path = published
        .borrow()
        .clone()
        .expect("the production pause was reached");
    let decode = |path: &Path| {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<crate::agent_queue::AgentJob>(&bytes).ok())
    };
    let first_owed = [&old, &target]
        .into_iter()
        .any(|path| decode(path) == Some(first.clone()));
    let second_owed = decode(&new_path) == Some(expected);
    let marked = trees.member.join(MOVED_MARKER).exists();
    assert!(
        first_owed && second_owed,
        "migration must retain both contexts: outcome={outcome:?}, A={first_owed}, B={second_owed}, marked={marked}"
    );
}

#[test]
fn atomicity_a_target_enqueued_after_absence_is_not_overwritten_by_migration() {
    assert_migration_preserves_a_new_publication(MigrationPausePoint::TargetAbsent);
}

#[test]
fn atomicity_a_target_replaced_after_equality_does_not_erase_the_old_context() {
    assert_migration_preserves_a_new_publication(MigrationPausePoint::QueuePublished);
}

#[test]
fn atomicity_a_new_session_cache_published_after_absence_stands_over_old_state() {
    let trees = Trees::new();
    let old_dir = trees.old(RunRoot::Sessions);
    session_cache::write_cache(&old_dir, &cache("session", "ending", 1_000)).unwrap();
    let old_path = old_dir.join("session.json");
    let old_bytes = std::fs::read(&old_path).unwrap();
    let new_dir = trees.new_tree(RunRoot::Sessions);
    let published = new_dir.clone();
    let _pause = install_migration_pause(MigrationPausePoint::TargetAbsent, move || {
        session_cache::write_cache(&published, &cache("session", "active", 2_000)).unwrap();
        assert_eq!(
            session_cache::read_cache(&published, "session")
                .unwrap()
                .unwrap()
                .updated_at_ms,
            2_000
        );
    });
    let outcome = adopt(&trees.ctx(NOW));
    let current = session_cache::read_cache(&new_dir, "session")
        .unwrap()
        .unwrap();
    let late = new_dir.join(rift_shadow::hooks::run_state::late_name(
        "session.json",
        NOW,
    ));
    let old_preserved = [&old_path, &late]
        .into_iter()
        .any(|path| std::fs::read(path).ok() == Some(old_bytes.clone()));
    assert!(
        current.updated_at_ms == 2_000 && current.state == "active" && old_preserved,
        "new session state must stand: outcome={outcome:?}, updated={}, old_preserved={old_preserved}",
        current.updated_at_ms
    );
}

/// A fixed clock: every age these tests read is measured from it.
const NOW: i64 = 1_800_000_000_000;

/// A git dir with its old tree, and a run root beside it.
struct Trees {
    _tmp: tempfile::TempDir,
    git_dir: PathBuf,
    run_root: PathBuf,
    member: PathBuf,
}

impl Trees {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join("repo").join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let run_root = tmp.path().join("cache").join("run");
        let member = run_root.join("0123456789ab");
        Self {
            _tmp: tmp,
            git_dir,
            run_root,
            member,
        }
    }

    fn ctx(&self, now_ms: i64) -> MoveCtx<'_> {
        MoveCtx {
            git_dir: &self.git_dir,
            run_root: &self.run_root,
            member: &self.member,
            now_ms,
        }
    }

    fn old(&self, root: RunRoot) -> PathBuf {
        root.legacy_under(&self.git_dir)
    }

    fn new_tree(&self, root: RunRoot) -> PathBuf {
        root.under(&self.member)
    }
}

#[test]
fn ownership_a_live_mover_is_not_stolen_after_ten_minutes() {
    let trees = Trees::new();
    let owner = super::MoveLock::take(&trees.run_root, 1_000)
        .unwrap()
        .unwrap();
    let contender = super::MoveLock::take(&trees.run_root, 601_000).unwrap();
    assert!(
        contender.is_none(),
        "elapsed time does not release a live mover"
    );
    drop(owner);
}

#[cfg(unix)]
#[test]
fn ownership_dropping_a_move_predecessor_preserves_its_successor() {
    use std::os::unix::fs::MetadataExt as _;
    let trees = Trees::new();
    let predecessor = super::MoveLock::take(&trees.run_root, 1_000)
        .unwrap()
        .unwrap();
    let marker = trees.run_root.join(".move-owner.lock");
    std::fs::rename(&marker, trees.run_root.join("replaced-owner")).unwrap();
    assert!(predecessor.check_current().is_err());
    let successor = super::MoveLock::take(&trees.run_root, 2_000)
        .unwrap()
        .unwrap();
    let identity = std::fs::metadata(&marker).unwrap().ino();
    assert!(super::MoveLock::take(&trees.run_root, 2_001)
        .unwrap()
        .is_none());
    drop(predecessor);
    assert_eq!(std::fs::metadata(&marker).unwrap().ino(), identity);
    assert!(super::MoveLock::take(&trees.run_root, 2_002)
        .unwrap()
        .is_none());
    drop(successor);
}

#[test]
fn ownership_an_empty_legacy_move_marker_is_not_proof_of_abandonment() {
    let trees = Trees::new();
    let marker = trees.run_root.join(MOVE_LOCK);
    put(&marker, b"");
    let contender = super::MoveLock::take(&trees.run_root, 601_000).unwrap();
    assert!(
        contender.is_none(),
        "an unknown legacy owner is not known dead"
    );
    assert_eq!(std::fs::read(marker).unwrap(), b"");
}

/// A serial control, not a deterministic simultaneous stale-read schedule.
#[test]
fn ownership_two_serial_move_contenders_preserve_unknown_legacy_debt() {
    let trees = Trees::new();
    let marker = trees.run_root.join(MOVE_LOCK);
    put(&marker, b"0");
    assert!(super::MoveLock::take(&trees.run_root, 601_000)
        .unwrap()
        .is_none());
    assert!(super::MoveLock::take(&trees.run_root, 601_001)
        .unwrap()
        .is_none());
    assert_eq!(std::fs::read(marker).unwrap(), b"0");
}

#[test]
fn ownership_a_conflicting_migration_retains_both_executable_capture_tickets() {
    let trees = Trees::new();
    let oid = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    let ticket = |session: &str| crate::capture_queue::QueueEntry {
        schema_version: 1,
        commit: oid.into(),
        work_tree: PathBuf::from("/fixture-worktree"),
        session_id: Some(session.into()),
        agent: Some("claude-code".into()),
        session_state: Some("active".into()),
        queued_at_ms: 1_000,
        attempts: 0,
    };
    let old_bytes = serde_json::to_vec(&ticket("legacy-session")).unwrap();
    let new_bytes = serde_json::to_vec(&ticket("current-session")).unwrap();
    let old = trees.old(RunRoot::CaptureQueue).join(format!("{oid}.json"));
    let new = trees
        .new_tree(RunRoot::CaptureQueue)
        .join(format!("{oid}.json"));
    put(&old, &old_bytes);
    put(&new, &new_bytes);
    let outcome = adopt(&trees.ctx(NOW));
    let refused = outcome.as_ref().map_or(true, |report| report.busy);
    let old_retained = std::fs::read(&old).ok() == Some(old_bytes);
    let new_retained = std::fs::read(&new).ok() == Some(new_bytes);
    let marked = trees.member.join(MOVED_MARKER).exists();
    assert!(
        refused && old_retained && new_retained && !marked,
        "conflict must stay executable: outcome={outcome:?}, old={old_retained}, new={new_retained}, marked={marked}"
    );
}

fn cache(id: &str, state: &str, updated_at_ms: i64) -> SessionCache {
    SessionCache::new(id, "claude-code", state, updated_at_ms)
}

fn put(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Seed one file of every kind the old tree holds.
fn write_agent(dir: &Path, id: usize, legacy: bool) -> PathBuf {
    let job = crate::agent_queue::AgentJob {
        schema_version: 1,
        work_tree: "/fixture-worktree".into(),
        queued_at_ms: 5,
        attempts: 0,
        kind: crate::agent_queue::JobKind::ReadTokens {
            tool_call_id: format!("{id:064x}"),
            rel_path: "a.txt".into(),
            content_sha256: "b".repeat(64),
            blob_oid: "c".repeat(40),
            lines: None,
            schema_version: 1,
        },
    };
    let path = crate::agent_queue::enqueue_in(dir, &job).unwrap();
    if !legacy {
        return path;
    }
    let plain = dir.join(format!(
        "{}.json",
        crate::agent_queue::logical_stem(&job).unwrap()
    ));
    std::fs::rename(path, &plain).unwrap();
    plain
}

fn write_capture(dir: &Path, id: u32) -> PathBuf {
    let job = crate::capture_queue::QueueEntry {
        schema_version: 1,
        commit: format!("{id:040x}"),
        work_tree: "/fixture-worktree".into(),
        session_id: None,
        agent: None,
        session_state: None,
        queued_at_ms: 5,
        attempts: 0,
    };
    let path = crate::capture_queue::delivery::publish(
        dir,
        &job.commit,
        &serde_json::to_value(&job).unwrap(),
    )
    .unwrap();
    let plain = dir.join(format!("{}.json", job.commit));
    std::fs::rename(path, &plain).unwrap();
    plain
}

fn agent_at(dir: &Path, id: usize) -> PathBuf {
    std::fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().path()).find(|path| {
        crate::agent_queue::read_job(path).is_ok_and(|(_, job)| matches!(job.kind,
            crate::agent_queue::JobKind::ReadTokens { tool_call_id, .. } if tool_call_id == format!("{id:064x}")))
    }).unwrap()
}

fn agent_values(dir: &Path) -> std::collections::BTreeMap<String, serde_json::Value> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let (value, job) = crate::agent_queue::read_job(&entry.unwrap().path()).unwrap();
            (crate::agent_queue::logical_stem(&job).unwrap(), value)
        })
        .collect()
}

fn seed_every_root(trees: &Trees) {
    let sessions = trees.old(RunRoot::Sessions);
    session_cache::write_cache(&sessions, &cache("s1", "active", 5)).unwrap();
    put(&sessions.join("s1.usage"), b"{}");
    put(&sessions.join("s1.agent"), b"claude-code");
    put(&sessions.join("s1.subagents").join("a1.json"), b"{}");
    write_agent(&trees.old(RunRoot::AgentQueue), 0, true);
    write_capture(&trees.old(RunRoot::CaptureQueue), 0);
    put(
        &trees
            .old(RunRoot::CaptureQueue)
            .join(format!("{:040x}.error", 0)),
        b"note",
    );
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[test]
fn a_first_move_takes_every_root_into_the_member_and_marks_it_last() {
    let trees = Trees::new();
    seed_every_root(&trees);
    let agent_before = crate::agent_queue::read_job(&agent_at(&trees.old(RunRoot::AgentQueue), 0))
        .unwrap()
        .0;
    let capture_before = crate::capture_queue::read_entry(
        &trees
            .old(RunRoot::CaptureQueue)
            .join(format!("{:040x}.json", 0)),
    )
    .unwrap()
    .0;
    let report = adopt(&trees.ctx(NOW)).unwrap();
    let agent_after =
        crate::agent_queue::read_job(&agent_at(&trees.new_tree(RunRoot::AgentQueue), 0))
            .unwrap()
            .0;
    let captures: Vec<_> = std::fs::read_dir(trees.new_tree(RunRoot::CaptureQueue))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter_map(|path| crate::capture_queue::read_entry(&path).ok())
        .collect();
    assert_eq!(captures.len(), 1);
    let capture_after = &captures[0].0;
    assert_eq!(agent_after, agent_before);
    assert_eq!(capture_after, &capture_before);
    assert_eq!(capture_after["attempts"], 0);
    assert_eq!(report.moved, 7, "{report:?}");
    for root in RunRoot::ALL {
        assert!(!trees.old(root).exists(), "{root:?} left the git dir");
    }
    let sessions = trees.new_tree(RunRoot::Sessions);
    assert_eq!(
        session_cache::read_cache(&sessions, "s1")
            .unwrap()
            .unwrap()
            .updated_at_ms,
        5
    );
    assert!(sessions.join("s1.subagents").join("a1.json").is_file());
    assert!(agent_at(&trees.new_tree(RunRoot::AgentQueue), 0).is_file());
    assert!(trees
        .new_tree(RunRoot::CaptureQueue)
        .join(format!("{:040x}.error", 0))
        .is_file());
    assert!(trees.member.join(MOVED_MARKER).is_file());
    assert!(
        !trees.run_root.join(MOVE_LOCK).exists(),
        "the lock is released"
    );
}

#[cfg(unix)]
#[test]
fn a_moved_file_keeps_its_mode_and_new_directories_are_owner_only() {
    let trees = Trees::new();
    let job = write_agent(&trees.old(RunRoot::AgentQueue), 0, true);
    set_mode(&job, 0o600);
    adopt(&trees.ctx(NOW)).unwrap();
    let moved = trees.new_tree(RunRoot::AgentQueue);
    assert_eq!(mode(&agent_at(&moved, 0)), 0o600);
    assert_eq!(mode(&moved), 0o700);
    assert_eq!(mode(&trees.member), 0o700);
}

#[cfg(unix)]
#[test]
fn a_copy_across_volumes_keeps_the_mode_and_unlinks_the_source() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("job.json");
    put(&source, b"{\"a\":1}");
    set_mode(&source, 0o600);
    let target = tmp.path().join("elsewhere").join("job.json");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let owner = crate::capture_queue::acquire_owner(tmp.path()).unwrap();
    copy_across(&source, &target, &|| {
        super::Admission::Ordinary(&owner).check_io()
    })
    .unwrap();
    assert!(!source.exists());
    assert_eq!(std::fs::read(&target).unwrap(), b"{\"a\":1}");
    assert_eq!(mode(&target), 0o600);
    let staged: Vec<_> = std::fs::read_dir(target.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .collect();
    assert!(staged.is_empty(), "the staging copy was renamed into place");
}

#[test]
fn a_crash_mid_move_is_finished_by_the_next_run_and_the_new_trees_copy_stands() {
    let trees = Trees::new();
    // A mover died after moving some files, and hooks went on writing the new
    // tree meanwhile: both trees hold run state, the dead mover's lock is
    // still on disk and no marker was written.
    let old = trees.old(RunRoot::Sessions);
    let new = trees.new_tree(RunRoot::Sessions);
    session_cache::write_cache(&old, &cache("both", "ending", 9)).unwrap();
    session_cache::write_cache(&new, &cache("both", "active", 4)).unwrap();
    session_cache::write_cache(&old, &cache("old-only", "active", 2)).unwrap();
    write_agent(&trees.old(RunRoot::AgentQueue), 0, true);
    write_agent(&trees.new_tree(RunRoot::AgentQueue), 0, false);
    write_agent(&trees.new_tree(RunRoot::AgentQueue), 1, false);
    put(
        &trees.run_root.join(".move-owner.lock"),
        (NOW - 60_000).to_string().as_bytes(),
    );

    let report = adopt(&trees.ctx(NOW)).unwrap();
    assert!(
        !report.busy,
        "an unheld persistent file is not an active owner"
    );
    assert_eq!(
        (report.moved, report.preserved, report.superseded),
        (1, 1, 1),
        "{report:?}"
    );
    let state = |id| session_cache::read_cache(&new, id).unwrap().unwrap().state;
    assert_eq!(state("both"), "active", "the new tree's copy stands");
    assert_eq!(state("old-only"), "active");
    assert!(new
        .join(rift_shadow::hooks::run_state::late_name("both.json", NOW))
        .is_file());
    let queue = trees.new_tree(RunRoot::AgentQueue);
    assert!(agent_at(&queue, 0).is_file() && agent_at(&queue, 1).is_file());
    assert!(!old.exists() && !trees.old(RunRoot::AgentQueue).exists());
    assert!(trees.member.join(MOVED_MARKER).is_file());
}

#[test]
fn a_re_run_after_the_move_finds_nothing_to_do() {
    let trees = Trees::new();
    seed_every_root(&trees);
    adopt(&trees.ctx(NOW)).unwrap();
    let marker = std::fs::read(trees.member.join(MOVED_MARKER)).unwrap();
    assert_eq!(adopt(&trees.ctx(NOW + 1)).unwrap(), MoveReport::default());
    assert_eq!(
        std::fs::read(trees.member.join(MOVED_MARKER)).unwrap(),
        marker,
        "the marker is written once"
    );
}

#[test]
fn an_older_binarys_late_write_is_preserved_beside_the_new_state_never_applied() {
    let trees = Trees::new();
    seed_every_root(&trees);
    adopt(&trees.ctx(NOW)).unwrap();
    // The installed hook is the old binary. It writes the old tree NOW —
    // later than anything the new tree holds — and, reading no cache there,
    // it believes the session new: a fresher and poorer copy.
    let late_at = NOW + 60_000;
    let old = trees.old(RunRoot::Sessions);
    session_cache::write_cache(&old, &cache("s1", "active", late_at)).unwrap();
    put(&old.join("s1.usage"), b"{\"byte_offset\":0}");
    session_cache::write_cache(&old, &cache("s2", "active", late_at)).unwrap();
    write_capture(&trees.old(RunRoot::CaptureQueue), 1);

    let report = adopt(&trees.ctx(late_at)).unwrap();
    assert_eq!((report.moved, report.preserved), (2, 2), "{report:?}");
    let new = trees.new_tree(RunRoot::Sessions);
    let kept = session_cache::read_cache(&new, "s1").unwrap().unwrap();
    assert_eq!(kept.updated_at_ms, 5, "the new tree's cache stands");
    assert_eq!(
        std::fs::read(new.join("s1.usage")).unwrap(),
        b"{}",
        "and its progress"
    );
    let late = |name| new.join(rift_shadow::hooks::run_state::late_name(name, late_at));
    assert!(
        late("s1.json").is_file() && late("s1.usage").is_file(),
        "preserved"
    );
    assert_eq!(
        session_cache::live_sessions(&new).unwrap().len(),
        2,
        "no reader takes a late file for a session"
    );
    assert!(session_cache::read_cache(&new, "s2").unwrap().is_some());
    assert!(std::fs::read_dir(trees.new_tree(RunRoot::CaptureQueue))
        .unwrap()
        .any(|entry| {
            crate::capture_queue::read_entry(&entry.unwrap().path())
                .is_ok_and(|(_, row)| row.commit == format!("{:040x}", 1))
        }));
    assert!(!old.exists());
}

#[test]
fn a_copy_the_new_tree_already_holds_byte_for_byte_is_simply_removed() {
    let trees = Trees::new();
    write_agent(&trees.old(RunRoot::AgentQueue), 0, true);
    write_agent(&trees.new_tree(RunRoot::AgentQueue), 0, false);
    let report = adopt(&trees.ctx(NOW)).unwrap();
    assert_eq!((report.superseded, report.preserved), (1, 0));
    let names = std::fs::read_dir(trees.new_tree(RunRoot::AgentQueue))
        .unwrap()
        .count();
    assert_eq!(names, 1);
}

/// A mover meeting another mover's lock waits in its bounded steps — 25 ms
/// in all, through the sleep it is handed — and leaves the move to the holder
/// when no marker appears; when one does, it takes what arrived since.
#[test]
fn a_mover_waits_at_most_twenty_five_ms_for_the_holder_and_never_more() {
    let trees = Trees::new();
    seed_every_root(&trees);
    put(&trees.run_root.join(MOVE_LOCK), NOW.to_string().as_bytes());
    let mut waited = Vec::new();
    let report = super::settle(&trees.ctx(NOW + 1), &mut |d| waited.push(d)).unwrap();
    assert!(report.busy);
    let total: u128 = waited.iter().map(std::time::Duration::as_millis).sum();
    assert_eq!(total, 25, "{waited:?}");
    assert!(
        trees.old(RunRoot::Sessions).exists(),
        "the holder's to move"
    );

    // The holder marks the member during the wait and releases its lock.
    let mut waited = 0;
    let report = super::settle(&trees.ctx(NOW + 1), &mut |_| {
        waited += 1;
        if waited == 2 {
            std::fs::create_dir_all(&trees.member).unwrap();
            std::fs::write(trees.member.join(MOVED_MARKER), b"").unwrap();
            std::fs::remove_file(trees.run_root.join(MOVE_LOCK)).unwrap();
        }
    })
    .unwrap();
    assert_eq!(waited, 2);
    assert!(!report.busy && report.moved > 0, "{report:?}");
}

#[test]
fn a_claim_already_standing_in_the_new_tree_is_never_replaced() {
    let trees = Trees::new();
    put(&trees.old(RunRoot::Sessions).join("s.agent"), b"cursor");
    put(
        &trees.new_tree(RunRoot::Sessions).join("s.agent"),
        b"claude-code",
    );
    adopt(&trees.ctx(NOW)).unwrap();
    assert_eq!(
        std::fs::read(trees.new_tree(RunRoot::Sessions).join("s.agent")).unwrap(),
        b"claude-code"
    );
}

#[test]
fn a_queue_publication_and_legacy_marker_stay_until_explicit_handoff() {
    let trees = Trees::new();
    let queue = trees.old(RunRoot::CaptureQueue);
    put(&queue.join("abc.json.tmp"), b"{");
    put(&queue.join(".lock"), b"");
    // Seen from the moment it was written: in flight, left where it is.
    adopt(&trees.ctx(super::modified_ms(&queue.join(".lock")).unwrap())).unwrap();
    assert!(queue.join("abc.json.tmp").is_file() && queue.join(".lock").is_file());
    assert!(!trees.new_tree(RunRoot::CaptureQueue).join(".lock").exists());
    // The same queue publication remains evidence a day later.
    adopt(&trees.ctx(NOW + 86_400_000)).unwrap();
    assert!(
        queue.exists(),
        "age is not proof that the legacy writer died"
    );
    assert!(queue.join("abc.json.tmp").is_file() && queue.join(".lock").is_file());
    assert!(!trees.member.join(MOVED_MARKER).exists());
}

#[test]
fn a_live_movers_lock_defers_the_move_to_a_later_resolution() {
    let trees = Trees::new();
    seed_every_root(&trees);
    put(&trees.run_root.join(MOVE_LOCK), NOW.to_string().as_bytes());
    let report = adopt(&trees.ctx(NOW + 1)).unwrap();
    assert!(report.busy);
    assert!(trees.old(RunRoot::Sessions).exists());
    assert!(!trees.member.join(MOVED_MARKER).exists());
}

#[test]
fn concurrent_movers_lose_no_file_and_move_each_one_once() {
    let trees = Trees::new();
    let queue = trees.old(RunRoot::AgentQueue);
    for n in 0..64 {
        write_agent(&queue, n, true);
    }
    let expected = agent_values(&queue);
    let moved: usize = std::thread::scope(|scope| {
        let movers: Vec<_> = (0..8)
            .map(|_| scope.spawn(|| adopt(&trees.ctx(NOW)).unwrap().moved))
            .collect();
        movers.into_iter().map(|m| m.join().unwrap()).sum()
    });
    // A mover that found the lock held moved nothing; a later resolution
    // finishes whatever is left.
    let finished = moved + adopt(&trees.ctx(NOW)).unwrap().moved;
    assert_eq!(finished, 64);
    let landed = std::fs::read_dir(trees.new_tree(RunRoot::AgentQueue))
        .unwrap()
        .count();
    assert_eq!(landed, 64);
    assert!(!queue.exists());
    assert!(!trees.run_root.join(MOVE_LOCK).exists());
    assert_eq!(agent_values(&trees.new_tree(RunRoot::AgentQueue)), expected);
}

#[test]
fn concurrent_legacy_barriers_preserve_jobs_until_explicit_quiescent_handoff() {
    let trees = Trees::new();
    let queue = trees.old(RunRoot::AgentQueue);
    for n in 0..64 {
        write_agent(&queue, n, true);
    }
    let expected = agent_values(&queue);
    let stale = NOW - 60_000;
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                put(
                    &trees.run_root.join(MOVE_LOCK),
                    stale.to_string().as_bytes(),
                );
                adopt(&trees.ctx(NOW)).unwrap();
            });
        }
    });
    assert_eq!(
        std::fs::read_dir(&queue).unwrap().count(),
        64,
        "all debt survives the legacy barrier"
    );
    assert!(!trees.member.join(MOVED_MARKER).exists());
    // Fixture-only handoff: all simulated legacy participants have joined.
    std::fs::remove_file(trees.run_root.join(MOVE_LOCK)).unwrap();
    adopt(&trees.ctx(NOW)).unwrap();
    let landed = std::fs::read_dir(trees.new_tree(RunRoot::AgentQueue))
        .unwrap()
        .count();
    assert_eq!(landed, 64, "every job landed exactly once");
    assert_eq!(agent_values(&trees.new_tree(RunRoot::AgentQueue)), expected);
}

#[test]
#[ignore = "measurement instrument: what the old-tree check costs a hook, and what the first move costs the worker"]
fn measure_the_old_tree_check_and_the_first_move() {
    for files in [126, 2_000] {
        let trees = Trees::new();
        let sessions = files * 106 / 126;
        for n in 0..sessions {
            session_cache::write_cache(
                &trees.old(RunRoot::Sessions),
                &cache(&format!("s{n}"), "active", 5),
            )
            .unwrap();
        }
        for n in sessions..files {
            write_agent(&trees.old(RunRoot::AgentQueue), n, true);
        }
        let started = std::time::Instant::now();
        let report = adopt(&trees.ctx(NOW)).unwrap();
        let first_move_us = started.elapsed().as_micros();
        assert_eq!(report.moved, files);
        eprintln!("first_move_files={files} first_move_us={first_move_us}");
    }

    const ROUNDS: u32 = 10_000;
    let checkout = tempfile::tempdir().unwrap();
    let initialized = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .current_dir(checkout.path())
        .status()
        .unwrap();
    assert!(initialized.success());
    let git_dir = checkout.path().join(".git");
    super::sessions_dir(&git_dir);
    let started = std::time::Instant::now();
    for _ in 0..ROUNDS {
        super::sessions_dir(&git_dir);
    }
    let resolution_ns = started.elapsed().as_nanos() / u128::from(ROUNDS);
    eprintln!("hook_resolution_ns={resolution_ns}");
}

#[test]
fn migration_retains_both_attempt_values_when_publication_wins_before_install() {
    let trees = Trees::new();
    let first = migration_ticket("/original-worktree", 1_000);
    let mut second = first.clone();
    second.attempts = 1;
    let old = crate::agent_queue::enqueue_in(&trees.old(RunRoot::AgentQueue), &first).unwrap();
    let original = std::fs::read(&old).unwrap();
    let target = trees.new_tree(RunRoot::AgentQueue);
    let observed = std::rc::Rc::new(std::cell::RefCell::new(None));
    let received = observed.clone();
    let expected = second.clone();
    let _pause = crate::capture_queue::delivery::pause_publication(move || {
        let path = crate::agent_queue::enqueue_in(&target, &second).unwrap();
        assert_eq!(crate::agent_queue::read_job(&path).unwrap().1, second);
        received.replace(Some(path));
    });
    assert!(
        adopt(&trees.ctx(NOW)).is_err(),
        "different persisted attempts are a conflict"
    );
    let published = observed
        .borrow()
        .clone()
        .expect("actual no-clobber window executed");
    assert_eq!(std::fs::read(&old).unwrap(), original);
    assert_eq!(
        crate::agent_queue::read_job(&published).unwrap().1,
        expected
    );
    assert!(!trees.member.join(MOVED_MARKER).exists());
}

#[test]
fn session_temporary_files_keep_the_existing_fresh_and_stale_policy() {
    let trees = Trees::new();
    let path = trees.old(RunRoot::Sessions).join("session.json.tmp");
    put(&path, b"unfinished-session");
    let written = NOW;
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_times(
            std::fs::FileTimes::new().set_modified(
                std::time::UNIX_EPOCH + std::time::Duration::from_millis(written as u64),
            ),
        )
        .unwrap();
    adopt(&trees.ctx(written)).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"unfinished-session");
    adopt(&trees.ctx(written + rift_shadow::hooks::STALE_WRITER_MS)).unwrap();
    assert!(!path.exists());
}

mod recovery;
