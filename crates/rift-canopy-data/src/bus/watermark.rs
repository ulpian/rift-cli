//! The change observer: each tracked project's `{cursor, epoch}` watermark,
//! re-read on an adaptive cadence, and the diffs it reports.
//!
//! A separate module because the observer is non-authoritative by design — it
//! only decides *when* the worker re-reads — and its rules (a moved epoch is a
//! rebuild, a moved cursor is ingest, cursor direction is not a signal) are
//! tested against a real store on their own.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rift_store::{
    projection_db_for_family_under, projection_file_stamp, CanonicalStore, SqliteStore,
};

use crate::feeds::project_family;
use crate::store::runtime;

/// A projection's change watermark (the rebuild-epoch rule): the append `cursor` moves on
/// ingest; only a rebuild moves the `epoch`. The pair is complete — there is
/// no projection change the pair cannot distinguish from quiet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watermark {
    /// The `captured_event` append cursor.
    pub cursor: i64,
    /// The rebuild epoch (0 for a never-rebuilt projection).
    pub epoch: i64,
}

/// How a tracked project's watermark moved between polls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ChangeKind {
    /// New capture activity — the cursor moved at the same epoch.
    Ingested,
    /// The projection was rebuilt from git — the epoch moved.
    Rebuilt,
    /// The projection is (now) missing/unreadable.
    Unavailable,
}

/// One observed change for one tracked project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChangeEvent {
    /// The canonical project path whose projection changed.
    pub(super) project: String,
    /// Which way its cursor moved.
    pub(super) kind: ChangeKind,
}

/// Fast polling cadence, used for a short window right after a detected change.
const ACTIVE_INTERVAL: Duration = Duration::from_millis(250);

/// Idle polling cadence, used once activity has decayed.
const IDLE_INTERVAL: Duration = Duration::from_secs(2);

/// How many quiet polls it takes to decay from the fast cadence back to idle.
const ACTIVE_TICKS_AFTER_CHANGE: u8 = 8;

/// The baseline change observer for the `PollOnly` reference engine: it
/// re-reads each tracked project's `{cursor, epoch}` watermark on an adaptive
/// interval and reports the diffs. A `Native`-capable engine (Turso subscribe)
/// would push the same [`ChangeEvent`]s without polling.
pub(super) struct WatermarkPoller {
    /// Cache root the projections live under.
    root: PathBuf,
    /// project path → last-seen watermark (`None` once observed as unavailable).
    seen: BTreeMap<String, Option<Watermark>>,
    /// How many more fast-interval ticks remain before decaying to idle cadence.
    active_ticks: u8,
    /// project path → its projection file, once its family has resolved. A
    /// family is minted once and never re-minted automatically, so the
    /// resolution (a git discovery plus a file read) need not run per tick.
    dbs: BTreeMap<String, PathBuf>,
    /// project path → the projection's stat stamp and the watermark read
    /// under it. An unchanged stamp answers the tick without opening SQLite.
    stamped: BTreeMap<String, (String, Option<Watermark>)>,
}

impl WatermarkPoller {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            root,
            seen: BTreeMap::new(),
            active_ticks: 0,
            dbs: BTreeMap::new(),
            stamped: BTreeMap::new(),
        }
    }

    /// The current poll interval — fast while recently active, else idle.
    pub(super) fn interval(&self) -> Duration {
        if self.active_ticks > 0 {
            ACTIVE_INTERVAL
        } else {
            IDLE_INTERVAL
        }
    }

    /// Every project the observer is tracking (for rebuild re-bootstrap).
    pub(super) fn tracked(&self) -> impl Iterator<Item = String> + '_ {
        self.seen.keys().cloned()
    }

    /// Record a project's freshly-observed watermark as the polling baseline
    /// (and begin tracking it). Called after every request/observed read so the
    /// next poll only fires on genuinely newer changes.
    pub(super) fn sync(&mut self, project: &str, mark: Option<Watermark>) {
        self.seen.insert(project.to_string(), mark);
    }

    /// Re-read every tracked project's watermark and report the diffs. A moved
    /// **epoch** is [`ChangeKind::Rebuilt`] (however the cursor moved — more
    /// rows, fewer rows, or the same count); a moved cursor at the same epoch
    /// is [`ChangeKind::Ingested`]; a vanished watermark is
    /// [`ChangeKind::Unavailable`]. Cursor direction is deliberately **not** a
    /// signal (the rebuild-epoch rule retired the regression heuristic). A quiet poll decays
    /// the cadence toward idle.
    pub(super) fn poll_once(&mut self) -> Vec<ChangeEvent> {
        let mut out = Vec::new();
        let paths: Vec<String> = self.seen.keys().cloned().collect();
        for path in paths {
            let prev = self.seen.get(&path).copied().flatten();
            let now = self.read_stamped(&path);
            let kind = match (prev, now) {
                (Some(p), Some(n)) if n.epoch != p.epoch => Some(ChangeKind::Rebuilt),
                (Some(p), Some(n)) if n.cursor != p.cursor => Some(ChangeKind::Ingested),
                (None, Some(_)) => Some(ChangeKind::Ingested),
                (Some(_), None) => Some(ChangeKind::Unavailable),
                // watermark unchanged, or still-unavailable → nothing to report.
                (Some(_), Some(_)) | (None, None) => None,
            };
            self.seen.insert(path.clone(), now);
            if let Some(kind) = kind {
                out.push(ChangeEvent {
                    project: path,
                    kind,
                });
            }
        }
        if out.is_empty() {
            self.active_ticks = self.active_ticks.saturating_sub(1);
        } else {
            self.active_ticks = ACTIVE_TICKS_AFTER_CHANGE;
        }
        out
    }

    /// [`read_watermark_at`] behind the projection's stat stamp.
    ///
    /// The stamp is taken *before* the read, so a commit that lands while
    /// SQLite is open leaves the cached stamp stale and the next tick reads
    /// again: a change can be read twice, never missed. Only an exact match
    /// skips the open.
    fn read_stamped(&mut self, path: &str) -> Option<Watermark> {
        let db = self.projection_db(path)?;
        let stamp = projection_file_stamp(&db);
        if let Some((seen, mark)) = self.stamped.get(path) {
            if *seen == stamp {
                return *mark;
            }
        }
        let mark = read_watermark_at(&db);
        self.stamped.insert(path.to_owned(), (stamp, mark));
        mark
    }

    /// The project's projection file, resolving its family once. An
    /// unresolved family is not remembered: it may be minted later.
    fn projection_db(&mut self, path: &str) -> Option<PathBuf> {
        if let Some(db) = self.dbs.get(path) {
            return Some(db.clone());
        }
        let db = projection_db_of(&self.root, path)?;
        self.dbs.insert(path.to_owned(), db.clone());
        Some(db)
    }
}

/// Read a project's `{cursor, epoch}` watermark, or `None` if the projection
/// is missing/unreadable, with no stamp and no cache: what the poller's
/// cached read must always agree with. Agrees with [`read_project`] on
/// `None` (iff the projection is absent/unreadable).
#[cfg(test)]
pub(super) fn read_watermark_pair(root: &Path, path: &str) -> Option<Watermark> {
    read_watermark_at(&projection_db_of(root, path)?)
}

/// Where a project's projection lives, or `None` while its family is
/// unresolved.
fn projection_db_of(root: &Path, path: &str) -> Option<PathBuf> {
    #[cfg(test)]
    tests::FAMILY_RESOLUTIONS.with(|n| n.set(n.get() + 1));
    let family = project_family(Path::new(path))?;
    Some(projection_db_for_family_under(root, &family))
}

/// The watermark of the projection at `db`, or `None` if it is
/// missing/unreadable.
fn read_watermark_at(db: &Path) -> Option<Watermark> {
    if !db.exists() {
        return None;
    }
    #[cfg(test)]
    tests::OPENS.with(|n| n.set(n.get() + 1));
    let store = SqliteStore::open(db).ok()?;
    let rt = runtime().ok()?;
    let cursor = rt.block_on(store.read_watermark("captured_event")).ok()?;
    let epoch = rt.block_on(store.read_rebuild_epoch()).ok()?;
    Some(Watermark { cursor, epoch })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use rift_git::repo_id_for;
    use rift_store::projection_db_under;

    use super::super::fixtures::{append_events, make_project, path_of};
    use super::*;

    thread_local! {
        /// SQLite opens the watermark read made on this thread.
        pub(super) static OPENS: Cell<u32> = const { Cell::new(0) };
        /// Family resolutions made on this thread.
        pub(super) static FAMILY_RESOLUTIONS: Cell<u32> = const { Cell::new(0) };
    }

    fn counted(counter: &'static std::thread::LocalKey<Cell<u32>>) -> u32 {
        counter.with(Cell::get)
    }

    // --- the change observer -------------------------------------------------

    #[test]
    fn watermark_advance_triggers_ingested() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 2);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 2,
                epoch: 0,
            }),
        ); // baseline

        append_events(root.path(), &proj, 2, 3); // now 5

        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Ingested,
            }]
        );
        // Baseline caught up: a subsequent quiet poll reports nothing.
        assert!(poller.poll_once().is_empty());
    }

    #[test]
    fn epoch_bump_triggers_rebuilt_and_rebootstrap() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 5);
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root.path(), &repo);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 5,
                epoch: 0,
            }),
        ); // baseline

        // A rebuild bumps the epoch (0 → 1); the cursor also resets, but the
        // epoch alone is the signal.
        rift_store::rebuild(&db).unwrap();
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Rebuilt,
            }]
        );

        // Capture re-populates (cursor 0 → 3 at the same epoch = Ingested),
        // then a second rebuild is a fresh Rebuilt — re-detected, not swallowed.
        append_events(root.path(), &proj, 0, 3);
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Ingested,
            }]
        );
        rift_store::rebuild(&db).unwrap();
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Rebuilt,
            }]
        );
    }

    #[test]
    fn rebuild_landing_more_rows_is_still_detected() {
        // The rebuild-epoch rule blind spot, closed: a rebuild that lands MORE rows than
        // the projection previously held is indistinguishable from ingest by
        // cursor alone — the epoch is what detects it.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 2);
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root.path(), &repo);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 2,
                epoch: 0,
            }),
        );

        // Rebuild, then land MORE rows (5 > 2) before the next poll.
        rift_store::rebuild(&db).unwrap();
        append_events(root.path(), &proj, 0, 5);
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Rebuilt,
            }],
            "a rebuild with a net-larger projection must read as Rebuilt, not Ingested"
        );
    }

    #[test]
    fn cursor_regression_alone_is_not_rebuilt() {
        // Cursor direction is retired as a signal: a baseline that is somehow
        // AHEAD of the store (e.g. a stale in-memory baseline) re-reads as
        // Ingested — only the epoch says Rebuilt.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 3);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 9,
                epoch: 0,
            }),
        );
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Ingested,
            }],
            "a backward cursor at the same epoch re-reads, but never as Rebuilt"
        );
    }

    proptest::proptest! {
        // Each case drives a real store through rebuilds — cap the case count
        // so the suite's wall-clock stays test-farm friendly.
        #![proptest_config(proptest::prelude::ProptestConfig::with_cases(24))]

        /// The watermark never goes backwards without an epoch bump, over an
        /// arbitrary interleaving of appends and rebuilds against a real store.
        #[test]
        fn watermark_never_backward_without_epoch_bump(
            ops in proptest::collection::vec(
                proptest::prop_oneof![
                    proptest::prelude::Strategy::prop_map(1usize..4, OpKind::Append),
                    proptest::strategy::Just(OpKind::Rebuild),
                ],
                1..8,
            ),
        ) {
            let root = tempfile::tempdir().unwrap();
            let proj = make_project(root.path(), "p", 1);
            let repo = repo_id_for(&proj).unwrap();
            let db = projection_db_under(root.path(), &repo);

            let mut prev = read_watermark_pair(root.path(), &path_of(&proj)).unwrap();
            let mut appended = 1usize;
            for op in ops {
                match op {
                    OpKind::Append(n) => {
                        append_events(root.path(), &proj, appended, n);
                        appended += n;
                    }
                    OpKind::Rebuild => {
                        rift_store::rebuild(&db).unwrap();
                        appended = 0;
                    }
                }
                let now = read_watermark_pair(root.path(), &path_of(&proj)).unwrap();
                proptest::prop_assert!(
                    now.cursor >= prev.cursor || now.epoch > prev.epoch,
                    "cursor went backward ({} -> {}) without an epoch bump ({} -> {})",
                    prev.cursor, now.cursor, prev.epoch, now.epoch
                );
                prev = now;
            }
        }
    }

    /// One projection mutation for the watermark property test.
    #[derive(Clone, Copy, Debug)]
    enum OpKind {
        Append(usize),
        Rebuild,
    }

    #[test]
    fn missing_projection_is_unavailable_not_panic() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 2);
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root.path(), &repo);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 2,
                epoch: 0,
            }),
        ); // baseline

        // The projection file disappears out from under the observer.
        std::fs::remove_file(&db).unwrap();
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Unavailable,
            }]
        );
        // Polling again does not panic and reports nothing new (still gone).
        assert!(poller.poll_once().is_empty());
    }

    #[test]
    fn observer_is_non_authoritative() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 1);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 1,
                epoch: 0,
            }),
        );

        // No underlying change: polling twice is idempotent (no spurious events).
        assert!(poller.poll_once().is_empty());
        assert!(poller.poll_once().is_empty());

        // A burst of appends between polls collapses to a single Ingested — a
        // "missed" intermediate state only delays, never loses, the update.
        append_events(root.path(), &proj, 1, 4); // 1 → 5 in one gap
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Ingested,
            }]
        );
        // Caught up again.
        assert!(poller.poll_once().is_empty());
    }

    #[test]
    fn interval_is_fast_after_change_then_decays_to_idle() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 1);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(
            &path_of(&proj),
            Some(Watermark {
                cursor: 1,
                epoch: 0,
            }),
        );
        assert_eq!(poller.interval(), IDLE_INTERVAL); // no activity yet

        append_events(root.path(), &proj, 1, 1); // a change
        let _ = poller.poll_once();
        assert_eq!(poller.interval(), ACTIVE_INTERVAL); // fast right after

        // Quiet polls decay the cadence back to idle.
        for _ in 0..ACTIVE_TICKS_AFTER_CHANGE {
            assert!(poller.poll_once().is_empty());
        }
        assert_eq!(poller.interval(), IDLE_INTERVAL);
    }

    #[test]
    fn a_quiet_projection_is_answered_from_its_stamp_without_opening_sqlite() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 2);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(&path_of(&proj), None);
        assert_eq!(poller.poll_once().len(), 1, "the first read is real");
        // The poller's own close may fold the log back once; let it settle.
        assert!(poller.poll_once().is_empty());
        let opens = counted(&OPENS);
        for _ in 0..5 {
            assert!(poller.poll_once().is_empty());
        }
        assert_eq!(counted(&OPENS), opens, "no tick opened a quiet projection");

        append_events(root.path(), &proj, 2, 1);
        assert_eq!(
            poller.poll_once(),
            vec![ChangeEvent {
                project: path_of(&proj),
                kind: ChangeKind::Ingested,
            }]
        );
        assert!(counted(&OPENS) > opens, "a moved stamp is read");
    }

    #[test]
    fn a_projects_family_is_resolved_once_across_ticks() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "p", 1);
        let mut poller = WatermarkPoller::new(root.path().to_path_buf());
        poller.sync(&path_of(&proj), None);
        let before = counted(&FAMILY_RESOLUTIONS);
        for _ in 0..4 {
            let _ = poller.poll_once();
        }
        assert_eq!(counted(&FAMILY_RESOLUTIONS), before + 1);
    }
}
