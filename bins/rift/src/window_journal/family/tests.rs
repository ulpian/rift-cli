//! Liveness, members and bounds, over synthetic checkouts with an injected
//! clock: every modification time a test depends on is set, never read off
//! the wall.

use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use rift_agent::window_journal::{Interval, Line};

use super::super::tests::hash;
use super::*;

/// The decision's moment and the family's floor, for every test.
const NOW: u64 = 1_800_000_000_000;
const FLOOR: u64 = 300_000;

/// A window key no journal in these tests is written under.
const NOBODY: &str = "0000000000000000000000000000000000000000000000000000000000000000";

/// A family on disk: the main checkout, and its common dir.
struct Family {
    _tmp: tempfile::TempDir,
    main: PathBuf,
    common: PathBuf,
}

impl Family {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(tmp.path()).unwrap();
        let main = root.join("main");
        let common = main.join(".git");
        std::fs::create_dir_all(common.join("objects")).unwrap();
        std::fs::write(common.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        Self {
            _tmp: tmp,
            main,
            common,
        }
    }

    /// A linked worktree `name`, from the files git itself leaves: its admin
    /// dir under the common dir, and the `.git` file in its checkout.
    fn linked(&self, name: &str) -> Checkout {
        let work_tree = self.main.parent().unwrap().join(name);
        let admin = self.common.join("worktrees").join(name);
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::create_dir_all(&work_tree).unwrap();
        std::fs::write(admin.join("HEAD"), "ref: refs/heads/side\n").unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        let dot_git = work_tree.join(".git");
        std::fs::write(admin.join("gitdir"), format!("{}\n", dot_git.display())).unwrap();
        std::fs::write(&dot_git, format!("gitdir: {}\n", admin.display())).unwrap();
        Checkout {
            work_tree,
            git_dir: admin,
        }
    }

    fn main_checkout(&self) -> Checkout {
        Checkout {
            work_tree: self.main.clone(),
            git_dir: self.common.clone(),
        }
    }

    fn query<'a>(&'a self, own_key: &'a str) -> FamilyQuery<'a> {
        FamilyQuery {
            work_tree: &self.main,
            git_dir: &self.common,
            own_key,
            now_ms: NOW,
            floor_ms: FLOOR,
        }
    }
}

/// A window's journal holding one read of `path`, last written at
/// `written_ms`, with the marker its opening leaves.
fn window(at: &Checkout, session: &str, agent: Option<&str>, path: &str, written_ms: u64) {
    let journal = WindowJournal::open(&at.work_tree, session, agent);
    journal.append(&read_of(path)).unwrap();
    set_written(&journal.live_path(), written_ms);
    let markers = at.work_tree.join(".rift/state/injection");
    std::fs::create_dir_all(&markers).unwrap();
    let marker = serde_json::json!({
        "schema_version": 1,
        "session": session,
        "agent_id": agent,
        "decided_at_ms": 1,
    });
    std::fs::write(
        markers.join(format!("{}.json", key(session, agent))),
        marker.to_string(),
    )
    .unwrap();
}

fn read_of(path: &str) -> Line {
    Line::Read {
        turn: 1,
        path: path.to_owned(),
        hash: hash(7),
        range: Interval::new(1, 40).unwrap(),
        total_lines: 400,
        at_ms: 1,
    }
}

fn set_written(file: &Path, written_ms: u64) {
    let handle = std::fs::OpenOptions::new().append(true).open(file).unwrap();
    handle
        .set_modified(UNIX_EPOCH + Duration::from_millis(written_ms))
        .unwrap();
}

/// A session's cache in one checkout's git dir, in `state`, last written at
/// `updated_ms`.
fn session(at: &Checkout, session: &str, state: &str, updated_ms: u64) {
    let dir = crate::family::run_state::sessions_dir(&at.git_dir);
    std::fs::create_dir_all(&dir).unwrap();
    let cache = serde_json::json!({
        "schema": 1,
        "session_id": session,
        "state": state,
        "agent": "claude-code",
        "current_turn_ordinal": 3,
        "compaction_boundary": null,
        "updated_at_ms": updated_ms,
    });
    std::fs::write(dir.join(format!("{session}.json")), cache.to_string()).unwrap();
}

/// The paths the journals read hold, in the order they were read.
fn held_paths(read: &SiblingJournals) -> Vec<String> {
    read.journals
        .iter()
        .flat_map(|journal| journal.lines().iter())
        .filter_map(|line| match line {
            Line::Read { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

/// ★ A sibling window of a working session, written within the floor, is
/// read — a subagent of the caller's own session included — and the caller's
/// own window never is.
#[test]
fn a_live_sibling_window_is_read_and_the_callers_own_is_not() {
    let family = Family::new();
    let main = family.main_checkout();
    window(&main, "s-own", None, "own.rs", NOW - 10);
    window(&main, "s-own", Some("agent-a"), "child.rs", NOW - 20);
    window(&main, "s-1", None, "other.rs", NOW - 30);
    session(&main, "s-own", "active", NOW - 5);
    session(&main, "s-1", "compacting", NOW - FLOOR);
    let own = key("s-own", None);
    let read = live_sibling_journals(&family.query(&own));
    assert_eq!(held_paths(&read), ["child.rs", "other.rs"]);
    assert!(read.bytes > 0);
}

/// A window whose session ended, went quiet past the floor or has no cache,
/// whose own journal is older than the floor, or whose marker is missing or
/// names another window, is not live.
#[test]
fn a_window_is_not_live_when_its_session_or_its_journal_is_not() {
    let family = Family::new();
    let main = family.main_checkout();
    window(&main, "s-ended", None, "ended.rs", NOW);
    session(&main, "s-ended", "ended", NOW);
    window(&main, "s-quiet", None, "quiet.rs", NOW);
    session(&main, "s-quiet", "active", NOW - FLOOR - 1);
    window(&main, "s-uncached", None, "uncached.rs", NOW);
    window(
        &main,
        "s-live",
        Some("finished"),
        "finished.rs",
        NOW - FLOOR - 1,
    );
    window(&main, "s-live", Some("unmarked"), "unmarked.rs", NOW);
    session(&main, "s-live", "active", NOW);
    let markers = main.work_tree.join(".rift/state/injection");
    std::fs::remove_file(markers.join(format!("{}.json", key("s-live", Some("unmarked")))))
        .unwrap();
    // A marker an agent rewrote to claim a live session for another window.
    window(&main, "s-ended", Some("forged"), "forged.rs", NOW);
    std::fs::write(
        markers.join(format!("{}.json", key("s-ended", Some("forged")))),
        r#"{"session":"s-live","agent_id":"someone-else"}"#,
    )
    .unwrap();
    let read = live_sibling_journals(&family.query(NOBODY));
    assert!(read.journals.is_empty(), "{:?}", held_paths(&read));
    assert_eq!(read.bytes, 0);
}

/// A cache stamped after the decision's own moment reads as age zero rather
/// than as a session that ended long ago.
#[test]
fn a_session_stamped_in_the_future_is_live() {
    let family = Family::new();
    let main = family.main_checkout();
    window(&main, "s-1", None, "a.rs", NOW + 50_000);
    session(&main, "s-1", "active", NOW + 60_000);
    let read = live_sibling_journals(&family.query(NOBODY));
    assert_eq!(held_paths(&read), ["a.rs"]);
}

/// ★ Every checkout of the family is a member, found from git's own files:
/// a linked worktree's windows are read from its checkout, and its sessions
/// from its own admin dir — a cache of the same name in another member's git
/// dir says nothing about it.
#[test]
fn a_linked_worktrees_windows_are_read_with_its_own_sessions() {
    let family = Family::new();
    let main = family.main_checkout();
    let linked = family.linked("wt");
    let members = family_checkouts(&family.main, &family.common);
    assert_eq!(members, [main.clone(), linked.clone()]);
    window(&linked, "s-wt", None, "wt.rs", NOW - 1);
    session(&main, "s-wt", "active", NOW);
    let query = family.query(NOBODY);
    assert!(live_sibling_journals(&query).journals.is_empty());
    session(&linked, "s-wt", "active", NOW);
    assert_eq!(held_paths(&live_sibling_journals(&query)), ["wt.rs"]);
    // Asked from the linked member, the main checkout is a member too.
    window(&main, "s-main", None, "main.rs", NOW - 2);
    session(&main, "s-main", "active", NOW);
    let from_linked = FamilyQuery {
        work_tree: &linked.work_tree,
        git_dir: &linked.git_dir,
        ..family.query(NOBODY)
    };
    assert_eq!(
        held_paths(&live_sibling_journals(&from_linked)),
        ["wt.rs", "main.rs"]
    );
}

/// A checkout that is no repository at all is its own only member.
#[test]
fn outside_a_repository_the_caller_is_its_own_only_member() {
    let tmp = tempfile::tempdir().unwrap();
    let members = family_checkouts(tmp.path(), &tmp.path().join(".git"));
    assert_eq!(members.len(), 1);
    assert_eq!(members[0].work_tree, tmp.path());
}

/// ★ At most sixteen journals are read, the most recently written first, and
/// never past a mebibyte of journal bytes: the hook's bound holds whatever
/// the directory holds.
#[test]
fn at_most_sixteen_journals_and_a_mebibyte_are_read_newest_first() {
    let family = Family::new();
    let main = family.main_checkout();
    for n in 0..24u64 {
        let session_id = format!("s-{n:02}");
        window(
            &main,
            &session_id,
            None,
            &format!("f{n:02}.rs"),
            NOW - 1_000 * n,
        );
        session(&main, &session_id, "active", NOW);
    }
    let read = live_sibling_journals(&family.query(NOBODY));
    let expected: Vec<String> = (0..16).map(|n| format!("f{n:02}.rs")).collect();
    assert_eq!(held_paths(&read), expected);

    let heavy = Family::new();
    let main = heavy.main_checkout();
    for n in 0..5u64 {
        let session_id = format!("h-{n}");
        window(&main, &session_id, None, &format!("h{n}.rs"), NOW - n);
        session(&main, &session_id, "active", NOW);
        let live = WindowJournal::open(&main.work_tree, &session_id, None).live_path();
        // Rows no binary wrote still cost their bytes to read.
        let mut text = std::fs::read_to_string(&live).unwrap();
        text.push_str(&"x".repeat(300 * 1024));
        std::fs::write(&live, text).unwrap();
        set_written(&live, NOW - n);
    }
    let read = live_sibling_journals(&heavy.query(NOBODY));
    assert_eq!(held_paths(&read), ["h0.rs", "h1.rs", "h2.rs"]);
    assert!(read.bytes <= MAX_SIBLING_BYTES, "{}", read.bytes);
}

/// The hook's cost at the bound: sixteen live journals filling the
/// mebibyte, at the row density a folded journal has, read in a fresh pass
/// each time. Prints the median and the 95th percentile; the budget is the
/// shipped binary's, so it is run in a release build.
#[test]
#[ignore = "measurement instrument: times the sibling read at its bound on this machine"]
fn the_sibling_read_at_its_bound_stays_inside_the_hook_budget() {
    let family = Family::new();
    let main = family.main_checkout();
    let rows_per_journal = 380;
    for n in 0..16u64 {
        let session_id = format!("p-{n:02}");
        window(&main, &session_id, None, "seed.rs", NOW - n);
        session(&main, &session_id, "active", NOW);
        let journal = WindowJournal::open(&main.work_tree, &session_id, None);
        for row in 0..rows_per_journal {
            journal
                .append(&read_of(&format!("src/m{row:04}/file.rs")))
                .unwrap();
        }
        set_written(&journal.live_path(), NOW - n);
    }
    let query = family.query(NOBODY);
    let mut times: Vec<Duration> = (0..40)
        .map(|_| {
            let started = std::time::Instant::now();
            let read = live_sibling_journals(&query);
            assert_eq!(read.journals.len(), 16);
            started.elapsed()
        })
        .collect();
    times.sort();
    let bytes = live_sibling_journals(&query).bytes;
    eprintln!(
        "sibling read: 16 journals, {bytes} bytes: p50 {:?}, p95 {:?}",
        times[19], times[37]
    );
    assert!(times[37] < Duration::from_millis(100));
}
