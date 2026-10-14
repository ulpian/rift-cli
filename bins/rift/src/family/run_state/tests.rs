//! The move of a member's run state out of its git dir: first move, crash and
//! re-run, an older binary writing after the move, concurrent movers.

use std::path::{Path, PathBuf};

use rift_shadow::hooks::run_state::{RunRoot, MOVED_MARKER, MOVE_LOCK};
use rift_shadow::hooks::session_cache::{self, SessionCache};

use super::{adopt, copy_across, MoveCtx, MoveReport};

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

fn cache(id: &str, state: &str, updated_at_ms: i64) -> SessionCache {
    SessionCache::new(id, "claude-code", state, updated_at_ms)
}

fn put(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, bytes).unwrap();
}

/// Seed one file of every kind the old tree holds.
fn seed_every_root(trees: &Trees) {
    let sessions = trees.old(RunRoot::Sessions);
    session_cache::write_cache(&sessions, &cache("s1", "active", 5)).unwrap();
    put(&sessions.join("s1.usage"), b"{}");
    put(&sessions.join("s1.agent"), b"claude-code");
    put(&sessions.join("s1.subagents").join("a1.json"), b"{}");
    put(
        &trees.old(RunRoot::AgentQueue).join("condense-s1.json"),
        b"{}",
    );
    put(&trees.old(RunRoot::CaptureQueue).join("abc.json"), b"{}");
    put(&trees.old(RunRoot::CaptureQueue).join("abc.error"), b"note");
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
    let report = adopt(&trees.ctx(NOW)).unwrap();
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
    assert!(trees
        .new_tree(RunRoot::AgentQueue)
        .join("condense-s1.json")
        .is_file());
    assert!(trees
        .new_tree(RunRoot::CaptureQueue)
        .join("abc.error")
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
    let job = trees.old(RunRoot::AgentQueue).join("condense-s1.json");
    put(&job, b"{}");
    set_mode(&job, 0o600);
    adopt(&trees.ctx(NOW)).unwrap();
    let moved = trees.new_tree(RunRoot::AgentQueue);
    assert_eq!(mode(&moved.join("condense-s1.json")), 0o600);
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
    copy_across(&source, &target).unwrap();
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
fn a_crash_mid_move_is_finished_by_the_next_run_with_the_newer_cache_kept() {
    let trees = Trees::new();
    // A mover died after moving some files: both trees hold run state, its
    // lock is still on disk and no marker was written.
    let old = trees.old(RunRoot::Sessions);
    let new = trees.new_tree(RunRoot::Sessions);
    session_cache::write_cache(&old, &cache("newer-in-old", "ending", 9)).unwrap();
    session_cache::write_cache(&new, &cache("newer-in-old", "active", 4)).unwrap();
    session_cache::write_cache(&old, &cache("newer-in-new", "active", 2)).unwrap();
    session_cache::write_cache(&new, &cache("newer-in-new", "ending", 8)).unwrap();
    put(&trees.old(RunRoot::AgentQueue).join("tokens-a.json"), b"{}");
    put(
        &trees.new_tree(RunRoot::AgentQueue).join("tokens-b.json"),
        b"{}",
    );
    put(
        &trees.run_root.join(MOVE_LOCK),
        (NOW - 60_000).to_string().as_bytes(),
    );

    let report = adopt(&trees.ctx(NOW)).unwrap();
    assert!(!report.busy, "a dead mover's lock is taken over");
    let state = |id| session_cache::read_cache(&new, id).unwrap().unwrap().state;
    assert_eq!(state("newer-in-old"), "ending");
    assert_eq!(state("newer-in-new"), "ending");
    let queue = trees.new_tree(RunRoot::AgentQueue);
    assert!(queue.join("tokens-a.json").is_file() && queue.join("tokens-b.json").is_file());
    assert!(!old.exists());
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
fn an_older_binary_writing_after_the_move_is_moved_on_the_next_resolution() {
    let trees = Trees::new();
    seed_every_root(&trees);
    adopt(&trees.ctx(NOW)).unwrap();
    // The installed hook is the old binary: it writes the old tree again,
    // reading no cache there, so it believes the session is new.
    let old = trees.old(RunRoot::Sessions);
    session_cache::write_cache(&old, &cache("s1", "active", 1)).unwrap();
    session_cache::write_cache(&old, &cache("s2", "active", 7)).unwrap();
    put(&trees.old(RunRoot::CaptureQueue).join("def.json"), b"{}");

    let report = adopt(&trees.ctx(NOW)).unwrap();
    assert_eq!((report.moved, report.superseded), (2, 1), "{report:?}");
    let new = trees.new_tree(RunRoot::Sessions);
    assert_eq!(
        session_cache::read_cache(&new, "s1")
            .unwrap()
            .unwrap()
            .updated_at_ms,
        5,
        "the older write does not replace the newer cache"
    );
    assert!(session_cache::read_cache(&new, "s2").unwrap().is_some());
    assert!(trees
        .new_tree(RunRoot::CaptureQueue)
        .join("def.json")
        .is_file());
    assert!(!old.exists());
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
fn a_write_in_flight_stays_and_a_dead_writers_temp_is_removed() {
    let trees = Trees::new();
    let queue = trees.old(RunRoot::CaptureQueue);
    put(&queue.join("abc.json.tmp"), b"{");
    put(&queue.join(".lock"), b"");
    // Seen from the moment it was written: in flight, left where it is.
    adopt(&trees.ctx(super::modified_ms(&queue.join(".lock")).unwrap())).unwrap();
    assert!(queue.join("abc.json.tmp").is_file() && queue.join(".lock").is_file());
    assert!(!trees.new_tree(RunRoot::CaptureQueue).join(".lock").exists());
    // Seen a day later: a dead writer's, removed, and the old tree with it.
    adopt(&trees.ctx(NOW + 86_400_000)).unwrap();
    assert!(!queue.exists());
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
        put(&queue.join(format!("tokens-{n}.json")), b"{}");
    }
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
}

#[test]
fn concurrent_movers_past_a_stale_lock_still_lose_no_file() {
    let trees = Trees::new();
    let queue = trees.old(RunRoot::AgentQueue);
    for n in 0..64 {
        put(&queue.join(format!("tokens-{n}.json")), b"{}");
    }
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
    let _ = std::fs::remove_file(trees.run_root.join(MOVE_LOCK));
    adopt(&trees.ctx(NOW)).unwrap();
    let landed = std::fs::read_dir(trees.new_tree(RunRoot::AgentQueue))
        .unwrap()
        .count();
    assert_eq!(landed, 64, "every job landed exactly once");
}

#[test]
#[ignore = "measurement instrument: what the old-tree check costs a resolution once the member is moved"]
fn measure_the_old_tree_check_on_a_moved_member() {
    const ROUNDS: u32 = 10_000;
    let trees = Trees::new();
    seed_every_root(&trees);
    adopt(&trees.ctx(NOW)).unwrap();
    let started = std::time::Instant::now();
    for _ in 0..ROUNDS {
        adopt(&trees.ctx(NOW)).unwrap();
    }
    let check_ns = started.elapsed().as_nanos() / u128::from(ROUNDS);

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
    eprintln!("old_tree_check_ns={check_ns} resolution_ns={resolution_ns}");
}
