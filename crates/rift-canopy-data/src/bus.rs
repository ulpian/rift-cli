//! The live data bus (P8): an off-thread reader that turns the dashboard's data
//! requests into scoped, **read-only** projection queries and feeds the results
//! back to the synchronous render loop as [`Event::ProjectData`].
//!
//! The bus runs on a dedicated OS thread with its own current-thread async
//! runtime (the `rift-store` seam is async-over-sync rusqlite). It keeps the pure
//! render loop synchronous — the loop drains the bus's updates through a merged
//! event source ([`rift_canopy::app::live`]). The bus has **no write path**: it
//! only reads each project's own per-repo projection (MVCC via SQLite WAL,
//! concurrent with capture). Any failure degrades to an `Error` status — never a
//! panic — so the UI stays live even if a projection is missing or locked.
//!
//! **Change observation (P8 S2 / ADR-0010).** Beyond serving requests, the
//! worker polls a [`WatermarkPoller`]: each tracked project's
//! `{cursor, epoch}` [`Watermark`] — the append cursor
//! ([`rift_store::CanonicalStore::read_watermark`]) plus the rebuild epoch
//! ([`rift_store::CanonicalStore::read_rebuild_epoch`]) — is re-read on an
//! adaptive cadence (fast right after a change, slow when idle). A moved
//! **epoch** means the projection was rebuilt from git
//! ([`ChangeKind::Rebuilt`]), which re-bootstraps every tracked project — a
//! rebuild that lands *more* rows than before is detected exactly like any
//! other (the old cursor-regression heuristic is retired; it was blind to
//! that case). A moved cursor at the same epoch is new capture activity
//! ([`ChangeKind::Ingested`]); a watermark that vanishes is
//! [`ChangeKind::Unavailable`]. The observer is **non-authoritative** — it
//! only decides *when* to re-read (git stays canonical), so a missed poll
//! only delays and a duplicate only repeats a harmless read. The reference
//! engine is [`rift_store::NotifyCap::PollOnly`]; a native-notify engine
//! would drive the same path without polling.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::Duration;

use rift_canopy::{
    derive_status, CaptureSummary, DataRequest, DetailView, EntryKind, Event, ModelUsage,
    PhaseRead, ProjectData, ProjectStatus, ProjectionState, SessionPhase, SettingsSummary,
    StatusInputs,
};
use rift_git::repo_id_for;
use rift_store::{projection_db_under, resolve_cache_root, CanonicalStore, SqliteStore};

use crate::store::runtime;

/// Handles to a running data bus: the request sender (loop → bus) and the update
/// receiver (bus → loop). Dropping either end disconnects the worker, which then
/// exits — so the bus is torn down simply by the dashboard returning.
pub struct Bus {
    /// The reducer's data requests are sent here (loop → bus).
    pub req_tx: Sender<DataRequest>,
    /// The loop drains bus updates from here (bus → loop).
    pub evt_rx: Receiver<Event>,
}

/// Spawn the data-bus worker, seeded to refresh every currently-registered
/// project so their statuses populate at startup. The seed updates queue until
/// the first main-loop poll drains them. The cache root is resolved from the
/// process environment ([`resolve_cache_root`]).
#[must_use]
pub fn spawn(initial_paths: Vec<String>) -> Bus {
    spawn_with_root(resolve_cache_root(|k| std::env::var(k).ok()), initial_paths)
}

/// Spawn the bus against an explicit cache `root` — the seam tests drive with a
/// tempdir, so they never touch the process environment.
#[must_use]
pub fn spawn_with_root(root: PathBuf, initial_paths: Vec<String>) -> Bus {
    let (req_tx, req_rx) = channel::<DataRequest>();
    let (evt_tx, evt_rx) = channel::<Event>();
    for p in initial_paths {
        let _ = req_tx.send(DataRequest::Refresh(p));
    }
    thread::spawn(move || worker(&req_rx, &evt_tx, &root));
    Bus { req_tx, evt_rx }
}

/// The worker loop. It waits for a request **or** the observer's poll interval,
/// whichever comes first:
/// * a request → coalesce the burst (latest-per-project), read each distinct
///   project's status, and emit a [`ProjectData`];
/// * a timeout → poll every tracked project's cursor and re-read the ones that
///   changed (a rebuild additionally re-bootstraps *all* tracked projects).
///
/// Exits when either channel disconnects (the dashboard quit).
fn worker(req_rx: &Receiver<DataRequest>, evt_tx: &Sender<Event>, root: &Path) {
    let mut observer = WatermarkPoller::new(root.to_path_buf());
    // The foreground project — the only one for which the expensive rich detail is
    // computed (status is cheap and read for every project).
    let mut active: Option<String> = None;
    loop {
        match req_rx.recv_timeout(observer.interval()) {
            Ok(first) => {
                // Coalesce a burst so a storm of requests collapses to one read per
                // project (backpressure by set-union, not queue growth).
                let mut to_read = BTreeSet::new();
                collect(first, &mut to_read, &mut active);
                while let Ok(req) = req_rx.try_recv() {
                    collect(req, &mut to_read, &mut active);
                }
                for path in to_read {
                    if read_emit(&mut observer, evt_tx, root, path, active.as_deref()).is_err() {
                        return; // the loop is gone; end the worker
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                let mut to_read = BTreeSet::new();
                for change in observer.poll_once() {
                    // Every observed change re-reads that project; a rebuild
                    // replaced the projection wholesale, so re-bootstrap every
                    // tracked project rather than trusting any cached sibling.
                    if change.kind == ChangeKind::Rebuilt {
                        to_read.extend(observer.tracked());
                    }
                    to_read.insert(change.project);
                }
                for path in to_read {
                    if read_emit(&mut observer, evt_tx, root, path, active.as_deref()).is_err() {
                        return;
                    }
                }
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Read a project's status + watermark once, record the watermark as the
/// observer's polling baseline (so the next poll only fires on genuinely newer
/// changes), and emit the update. The rich detail is attached only when `path`
/// is the `active` (foreground) project — building it reads several tables, so
/// it stays scoped to the one project the user is looking at. Returns
/// `Err(())` if the loop end is gone.
fn read_emit(
    observer: &mut WatermarkPoller,
    evt_tx: &Sender<Event>,
    root: &Path,
    path: String,
    active: Option<&str>,
) -> Result<(), ()> {
    let (status, mark) = read_project(root, &path);
    observer.sync(&path, mark);
    let detail = if active == Some(path.as_str()) {
        Some(build_detail(root, &path, status))
    } else {
        None
    };
    let update = Event::ProjectData(ProjectData {
        project: path,
        status,
        detail,
    });
    evt_tx.send(update).map_err(|_| ())
}

/// Fold a request into the set of projects to (re-)read, updating the `active`
/// (foreground) project. `SetActive(None)` clears the active project.
fn collect(req: DataRequest, out: &mut BTreeSet<String>, active: &mut Option<String>) {
    match req {
        DataRequest::Refresh(p) => {
            out.insert(p);
        }
        DataRequest::SetActive(Some(p)) => {
            *active = Some(p.clone());
            out.insert(p);
        }
        DataRequest::SetActive(None) => *active = None,
    }
}

/// Read a project's live status **and** its `{cursor, epoch}` watermark from
/// its own per-repo projection in a single open — read-only, scoped by the
/// project's `repo_id`. The gathered facts (repo presence, projection
/// readiness, the session phase file, the most-recent event's age) are fed to
/// the pure [`derive_status`] truth table (P4 S2), so the classification is
/// testable without a store; any failure degrades to an
/// `Error`/`NotInitialized` status, never a panic.
///
/// The watermark is `Some` iff the projection is open and readable, and `None`
/// iff it is absent/unreadable — the same `None` the change observer treats as
/// [`ChangeKind::Unavailable`].
fn read_project(root: &Path, path: &str) -> (ProjectStatus, Option<Watermark>) {
    let dir = PathBuf::from(path);
    let now_ms = now_ms();

    // A registered path that no longer resolves → the repo is gone.
    let Ok(repo) = repo_id_for(&dir) else {
        return (
            status_of(
                false,
                ProjectionState::Uninitialized,
                PhaseRead::Ok(None),
                None,
                now_ms,
            ),
            None,
        );
    };
    let phase = read_phase(&dir);
    let db = projection_db_under(root, &repo);
    if !db.exists() {
        // No projection: initialized (`.rift/` present) → Building, else uninit.
        let projection = if dir.join(".rift").is_dir() {
            ProjectionState::Building
        } else {
            ProjectionState::Uninitialized
        };
        return (status_of(true, projection, phase, None, now_ms), None);
    }
    // The projection exists: open once, read the watermark + freshest event.
    let Some((mark, last_ts)) = open_and_read(&db) else {
        return (
            status_of(true, ProjectionState::Unavailable, phase, None, now_ms),
            None,
        );
    };
    let age = last_ts.map(|ts| now_ms.saturating_sub(ts));
    (
        status_of(true, ProjectionState::Ready, phase, age, now_ms),
        Some(mark),
    )
}

/// Assemble the [`StatusInputs`] and run the pure truth table.
fn status_of(
    repo_present: bool,
    projection: ProjectionState,
    phase: PhaseRead,
    last_event_age_ms: Option<i64>,
    now_ms: i64,
) -> ProjectStatus {
    derive_status(&StatusInputs {
        repo_present,
        projection,
        phase,
        last_event_age_ms,
        now_ms,
    })
}

/// Open the projection read-only and return `(watermark, most-recent
/// occurred_at)`, or `None` if it cannot be opened/read
/// (→ [`ProjectionState::Unavailable`]).
fn open_and_read(db: &Path) -> Option<(Watermark, Option<i64>)> {
    let store = SqliteStore::open(db).ok()?;
    let rt = runtime().ok()?;
    let cursor = rt.block_on(store.read_watermark("captured_event")).ok()?;
    let epoch = rt.block_on(store.read_rebuild_epoch()).ok()?;
    // The freshest event's transaction time (0 rows → None, not an error).
    let last_ts = rt
        .block_on(store.query_opt_i64(
            "SELECT occurred_at_ms FROM captured_event ORDER BY occurred_at_ms DESC LIMIT 1",
            &[],
        ))
        .ok()?;
    Some((Watermark { cursor, epoch }, last_ts))
}

/// Read a project's session phase file (`<dir>/.rift/session.phase`). Absent →
/// `Ok(None)` (no live session); present but unreadable/garbled → `Corrupt` (so the
/// status becomes a distinct `CorruptPhaseFile` error rather than silently "quiet").
/// The format is two whitespace-separated integers: `<active_sessions> <since_ms>`.
fn read_phase(dir: &Path) -> PhaseRead {
    let f = dir.join(".rift").join("session.phase");
    if !f.exists() {
        return PhaseRead::Ok(None);
    }
    match std::fs::read(&f) {
        Ok(bytes) => match parse_phase(&bytes) {
            Some(p) => PhaseRead::Ok(Some(p)),
            None => PhaseRead::Corrupt,
        },
        Err(_) => PhaseRead::Corrupt, // exists but unreadable
    }
}

/// Parse a phase file's bytes into a [`SessionPhase`], or `None` if malformed —
/// total over arbitrary bytes (non-UTF8, extra tokens, bad numbers all fail
/// cleanly), so the reader never panics on a corrupt file.
fn parse_phase(bytes: &[u8]) -> Option<SessionPhase> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut it = text.split_whitespace();
    let active_sessions: u16 = it.next()?.parse().ok()?;
    let since_ms: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None; // trailing junk → corrupt
    }
    Some(SessionPhase {
        active_sessions,
        since_ms,
    })
}

/// Build the rich column-2 [`DetailView`] for the active project: its effective
/// settings (from `.rift/config.toml`) plus the models and capture counts from its
/// own projection. Any unreadable part degrades to empty/default — never a panic.
/// The `status` is the already-derived one, so the detail and the list agree.
fn build_detail(root: &Path, path: &str, status: ProjectStatus) -> DetailView {
    let dir = PathBuf::from(path);
    let label = dir
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned());
    let (models, capture) = read_projection_detail(root, &dir).unwrap_or_default();
    // Toggles live in the machine-global ui-state store (not the per-repo
    // projection); a read-only open folds them for this project.
    let toggles = crate::store::CanopyStore::open(root)
        .and_then(|cs| cs.toggles(path))
        .unwrap_or_default();
    DetailView {
        path: path.to_string(),
        label,
        kind: EntryKind::Repo,
        status,
        models,
        settings: read_settings(&dir),
        capture,
        toggles,
    }
}

/// Summarize a project's effective policy **safely** — counts and booleans only,
/// never the raw sensitive-path globs (surfacing those would leak the patterns the
/// policy protects). A missing/invalid config falls back to secure defaults.
fn read_settings(dir: &Path) -> SettingsSummary {
    let cfg = rift_config::EffectiveConfig::load(&dir.join(".rift"))
        .or_else(|_| rift_config::EffectiveConfig::secure_default())
        .ok();
    cfg.map_or_else(SettingsSummary::default, |c| SettingsSummary {
        sensitive_path_count: c.sensitive_paths().len(),
        raw_transcripts: c.raw_transcripts_enabled(),
        captures_tool_calls: c.captures_tool_calls(),
        captures_token_usage: c.captures_token_usage(),
    })
}

/// Read a project's models + capture counts from its own projection (read-only,
/// scoped by `repo_id`). `None` if the projection is missing/unreadable.
fn read_projection_detail(root: &Path, dir: &Path) -> Option<(Vec<ModelUsage>, CaptureSummary)> {
    let repo = repo_id_for(dir).ok()?;
    let db = projection_db_under(root, &repo);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;

    // Models: call counts from model_call, unioned with each session's headline
    // model (which may have no calls yet). Sorted most-used first.
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    if let Ok(rows) =
        rt.block_on(store.query("SELECT model, COUNT(*) FROM model_call GROUP BY model", &[]))
    {
        for r in rows {
            if let (Ok(model), Ok(n)) = (r.get_text(0), r.get_i64(1)) {
                counts.insert(model, u64::try_from(n).unwrap_or(0));
            }
        }
    }
    if let Ok(rows) = rt.block_on(store.query("SELECT DISTINCT model_primary FROM session", &[])) {
        for r in rows {
            if let Ok(model) = r.get_text(0) {
                counts.entry(model).or_insert(0);
            }
        }
    }
    let mut models: Vec<ModelUsage> = counts
        .into_iter()
        .map(|(model, calls)| ModelUsage { model, calls })
        .collect();
    models.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.model.cmp(&b.model)));

    let count = |t: &str| u64::try_from(rt.block_on(store.count(t)).unwrap_or(0)).unwrap_or(0);
    let capture = CaptureSummary {
        events: count("captured_event"),
        sessions: count("session"),
        checkpoints: count("checkpoint"),
        last_checkpoint_at_ms: rt
            .block_on(store.query_opt_i64(
                "SELECT created_at FROM checkpoint ORDER BY created_at DESC LIMIT 1",
                &[],
            ))
            .ok()
            .flatten(),
    };
    Some((models, capture))
}

/// Read a project's `{cursor, epoch}` watermark, or `None` if the projection
/// is missing/unreadable. The single tiny read the change observer polls;
/// agrees with [`read_project`] on `None` (iff the projection is
/// absent/unreadable).
fn read_watermark_pair(root: &Path, path: &str) -> Option<Watermark> {
    let dir = PathBuf::from(path);
    let repo = repo_id_for(&dir).ok()?;
    let db = projection_db_under(root, &repo);
    if !db.exists() {
        return None;
    }
    let store = SqliteStore::open(&db).ok()?;
    let rt = runtime().ok()?;
    let cursor = rt.block_on(store.read_watermark("captured_event")).ok()?;
    let epoch = rt.block_on(store.read_rebuild_epoch()).ok()?;
    Some(Watermark { cursor, epoch })
}

/// A projection's change watermark (ADR-0010): the append `cursor` moves on
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
enum ChangeKind {
    /// New capture activity — the cursor moved at the same epoch.
    Ingested,
    /// The projection was rebuilt from git — the epoch moved.
    Rebuilt,
    /// The projection is (now) missing/unreadable.
    Unavailable,
}

/// One observed change for one tracked project.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChangeEvent {
    /// The canonical project path whose projection changed.
    project: String,
    /// Which way its cursor moved.
    kind: ChangeKind,
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
struct WatermarkPoller {
    /// Cache root the projections live under.
    root: PathBuf,
    /// project path → last-seen watermark (`None` once observed as unavailable).
    seen: BTreeMap<String, Option<Watermark>>,
    /// How many more fast-interval ticks remain before decaying to idle cadence.
    active_ticks: u8,
}

impl WatermarkPoller {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            seen: BTreeMap::new(),
            active_ticks: 0,
        }
    }

    /// The current poll interval — fast while recently active, else idle.
    fn interval(&self) -> Duration {
        if self.active_ticks > 0 {
            ACTIVE_INTERVAL
        } else {
            IDLE_INTERVAL
        }
    }

    /// Every project the observer is tracking (for rebuild re-bootstrap).
    fn tracked(&self) -> impl Iterator<Item = String> + '_ {
        self.seen.keys().cloned()
    }

    /// Record a project's freshly-observed watermark as the polling baseline
    /// (and begin tracking it). Called after every request/observed read so the
    /// next poll only fires on genuinely newer changes.
    fn sync(&mut self, project: &str, mark: Option<Watermark>) {
        self.seen.insert(project.to_string(), mark);
    }

    /// Re-read every tracked project's watermark and report the diffs. A moved
    /// **epoch** is [`ChangeKind::Rebuilt`] (however the cursor moved — more
    /// rows, fewer rows, or the same count); a moved cursor at the same epoch
    /// is [`ChangeKind::Ingested`]; a vanished watermark is
    /// [`ChangeKind::Unavailable`]. Cursor direction is deliberately **not** a
    /// signal (ADR-0010 retired the regression heuristic). A quiet poll decays
    /// the cadence toward idle.
    fn poll_once(&mut self) -> Vec<ChangeEvent> {
        let mut out = Vec::new();
        let paths: Vec<String> = self.seen.keys().cloned().collect();
        for path in paths {
            let prev = self.seen.get(&path).copied().flatten();
            let now = read_watermark_pair(&self.root, &path);
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
}

/// Wall-clock epoch-ms — the bus's only clock (status ages against it). The
/// pure truth table stays clock-injected; only this worker samples the wall.
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_canopy::StatusError;
    use rift_store::Value;

    const EVENT_COLS: [&str; 9] = [
        "idempotency_key",
        "session_id",
        "turn_ordinal",
        "event_ordinal",
        "kind",
        "trust",
        "payload_sha256",
        "occurred_at_ms",
        "schema_version",
    ];

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
    }

    /// Insert captured-event rows `start..start+n` into an open projection.
    fn insert_events(store: &SqliteStore, rt: &tokio::runtime::Runtime, start: usize, n: usize) {
        for i in start..start + n {
            rt.block_on(store.insert(
                "captured_event",
                &EVENT_COLS,
                &[
                    Value::text(format!("k{i}")),
                    Value::text("sess"),
                    Value::Int(0),
                    Value::Int(i64::try_from(i).unwrap()),
                    Value::text("start"),
                    Value::text("trusted"),
                    Value::text("sha"),
                    Value::Int(1),
                    Value::Int(1),
                ],
            ))
            .unwrap();
        }
    }

    /// Build a project directory + its per-repo projection under `root`, seeded
    /// with `events` captured-event rows. Returns the project path.
    fn make_project(root: &Path, name: &str, events: usize) -> PathBuf {
        let proj = root.join(name);
        std::fs::create_dir_all(&proj).unwrap();
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root, &repo);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = SqliteStore::open(&db).unwrap();
        store.create_canonical_schema().unwrap();
        insert_events(&store, &runtime(), 0, events);
        proj
    }

    /// Append `n` more captured-event rows (keys `start..`) to an existing project.
    fn append_events(root: &Path, proj: &Path, start: usize, n: usize) {
        let repo = repo_id_for(proj).unwrap();
        let db = projection_db_under(root, &repo);
        let store = SqliteStore::open(&db).unwrap();
        insert_events(&store, &runtime(), start, n);
    }

    /// Open an existing project's projection store.
    fn open_proj(root: &Path, proj: &Path) -> SqliteStore {
        let repo = repo_id_for(proj).unwrap();
        SqliteStore::open(projection_db_under(root, &repo)).unwrap()
    }

    /// Seed a `session` row (headline model).
    fn seed_session(store: &SqliteStore, rt: &tokio::runtime::Runtime, id: &str, model: &str) {
        rt.block_on(store.insert(
            "session",
            &[
                "session_id",
                "agent",
                "model_primary",
                "repo_id",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("claude-code"),
                Value::text(model),
                Value::text("r"),
                Value::Int(1),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed a `model_call` row.
    fn seed_model_call(store: &SqliteStore, rt: &tokio::runtime::Runtime, id: &str, model: &str) {
        rt.block_on(store.insert(
            "model_call",
            &[
                "call_id",
                "turn_id",
                "model",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("t"),
                Value::text(model),
                Value::Int(1),
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    /// Seed a `checkpoint` row at `created_at`.
    fn seed_checkpoint(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        id: &str,
        created_at: i64,
    ) {
        rt.block_on(store.insert(
            "checkpoint",
            &[
                "checkpoint_id",
                "strategy",
                "created_at",
                "merkle_root",
                "prev_hash",
                "schema_version",
            ],
            &[
                Value::text(id),
                Value::text("manual"),
                Value::Int(created_at),
                Value::text("root"),
                Value::Null,
                Value::Int(1),
            ],
        ))
        .unwrap();
    }

    fn path_of(proj: &Path) -> String {
        proj.to_string_lossy().into_owned()
    }

    /// The status half of [`read_project`] — the P8 S1 read surface under test.
    fn status(root: &Path, path: &str) -> ProjectStatus {
        read_project(root, path).0
    }

    #[test]
    fn read_status_missing_repo_for_nonexistent_path() {
        let root = tempfile::tempdir().unwrap();
        let s = status(root.path(), "/definitely/not/a/real/path");
        assert_eq!(s, ProjectStatus::Error(StatusError::RepoMissing));
    }

    #[test]
    fn read_status_not_initialized_without_projection() {
        let root = tempfile::tempdir().unwrap();
        let proj = root.path().join("plain");
        std::fs::create_dir_all(&proj).unwrap();
        let s = status(root.path(), &proj.to_string_lossy());
        assert_eq!(s, ProjectStatus::NotInitialized);
    }

    #[test]
    fn read_status_idle_when_projection_has_capture_data() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "captured", 3);
        let s = status(root.path(), &proj.to_string_lossy());
        assert_eq!(s, ProjectStatus::Idle);
    }

    #[test]
    fn read_is_read_only_leaves_row_count_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "ro", 2);
        let repo = repo_id_for(&proj).unwrap();
        let db = projection_db_under(root.path(), &repo);

        let _ = status(root.path(), &proj.to_string_lossy());

        // The bus never writes: the row count is exactly what we seeded.
        let store = SqliteStore::open(&db).unwrap();
        assert_eq!(
            runtime().block_on(store.count("captured_event")).unwrap(),
            2
        );
    }

    #[test]
    fn scope_is_per_project_projection() {
        // Two projects have independent projections; reading one never reflects
        // the other's capture data.
        let root = tempfile::tempdir().unwrap();
        let a = make_project(root.path(), "a", 5);
        let b = root.path().join("b");
        std::fs::create_dir_all(&b).unwrap();
        assert_eq!(
            status(root.path(), &a.to_string_lossy()),
            ProjectStatus::Idle
        );
        assert_eq!(
            status(root.path(), &b.to_string_lossy()),
            ProjectStatus::NotInitialized
        );
    }

    #[test]
    fn spawn_refreshes_initial_projects_and_delivers_updates() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        // Inject the cache root (no process-env mutation → no test races).
        let bus = spawn_with_root(root.path().to_path_buf(), vec![path_of(&proj)]);

        // The seeded refresh yields one ProjectData update for the project.
        let ev = bus
            .evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a ProjectData update");
        match ev {
            Event::ProjectData(pd) => {
                assert_eq!(pd.project, path_of(&proj));
                assert_eq!(pd.status, ProjectStatus::Idle);
            }
            other => panic!("expected ProjectData, got {other:?}"),
        }
    }

    // --- P4 S2: status derivation from the phase file -----------------------

    /// Write `<proj>/.rift/session.phase` with `contents`.
    fn write_phase(proj: &Path, contents: &[u8]) {
        let rift = proj.join(".rift");
        std::fs::create_dir_all(&rift).unwrap();
        std::fs::write(rift.join("session.phase"), contents).unwrap();
    }

    #[test]
    fn live_session_phase_makes_status_running() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "live", 1);
        write_phase(&proj, b"2 12345"); // 2 active sessions, since_ms 12345
        assert!(matches!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Running {
                active_sessions: 2,
                since_ms: 12345
            }
        ));
    }

    #[test]
    fn corrupt_phase_file_is_a_distinct_error() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "bad", 1);
        write_phase(&proj, b"not a valid phase file");
        assert_eq!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Error(StatusError::CorruptPhaseFile)
        );
    }

    #[test]
    fn absent_phase_file_leaves_status_unaffected() {
        // No phase file → no live session → old capture data reads as Idle.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "quiet", 1);
        assert_eq!(
            status(root.path(), &proj.to_string_lossy()),
            ProjectStatus::Idle
        );
    }

    #[test]
    fn phase_parser_is_total_over_arbitrary_bytes() {
        // Fuzz-lite: every pattern parses to Some or None, and read_phase never
        // panics — a garbled phase file degrades to Corrupt, never a crash.
        let patterns: &[&[u8]] = &[
            b"",
            b"   ",
            b"1",
            b"1 2",
            b"1 2 3",
            b"-1 5",
            b"99999999999999999999 0",
            b"1 notanumber",
            b"\xff\xfe\x00\x01",
            b"nan nan",
            b"0 0",
            b"65535 9223372036854775807",
        ];
        for p in patterns {
            // parse_phase is total: Some(valid) or None, never a panic.
            let _ = parse_phase(p);
        }
        // "1 2" is the one well-formed pattern.
        assert_eq!(
            parse_phase(b"1 2"),
            Some(SessionPhase {
                active_sessions: 1,
                since_ms: 2
            })
        );
        assert_eq!(parse_phase(b"1 2 3"), None, "trailing junk → corrupt");
        assert_eq!(parse_phase(b"\xff\xfe"), None, "non-utf8 → corrupt");

        // And read_phase over a real file with garbage yields Corrupt, not a panic.
        let dir = tempfile::tempdir().unwrap();
        write_phase(dir.path(), b"\xff\xfe garbage \x00");
        assert_eq!(read_phase(dir.path()), PhaseRead::Corrupt);
    }

    // --- P5 S1: the detail view ---------------------------------------------

    #[test]
    fn aggregates_distinct_models_with_counts() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "m", 1);
        let store = open_proj(root.path(), &proj);
        let rt = runtime();
        seed_model_call(&store, &rt, "c1", "A");
        seed_model_call(&store, &rt, "c2", "A");
        seed_model_call(&store, &rt, "c3", "B");
        seed_session(&store, &rt, "s1", "A"); // already seen
        seed_session(&store, &rt, "s2", "C"); // headline-only, 0 calls

        let d = build_detail(root.path(), &path_of(&proj), ProjectStatus::Idle);
        // Most-used first; the headline-only model appears with 0 calls.
        let models: Vec<(&str, u64)> = d
            .models
            .iter()
            .map(|m| (m.model.as_str(), m.calls))
            .collect();
        assert_eq!(models, vec![("A", 2), ("B", 1), ("C", 0)]);
    }

    #[test]
    fn capture_summary_reflects_projection() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "cap", 4); // 4 captured events
        let store = open_proj(root.path(), &proj);
        let rt = runtime();
        seed_session(&store, &rt, "s1", "A");
        seed_checkpoint(&store, &rt, "k1", 100);
        seed_checkpoint(&store, &rt, "k2", 200);

        let d = build_detail(root.path(), &path_of(&proj), ProjectStatus::Idle);
        assert_eq!(d.capture.events, 4);
        assert_eq!(d.capture.sessions, 1);
        assert_eq!(d.capture.checkpoints, 2);
        assert_eq!(d.capture.last_checkpoint_at_ms, Some(200));
    }

    #[test]
    fn settings_summary_from_config() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "cfg", 1);
        let rift = proj.join(".rift");
        std::fs::create_dir_all(&rift).unwrap();
        std::fs::write(
            rift.join("config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n\
             [paths]\nsensitive_paths = [\"my-secret-glob/**\"]\n",
        )
        .unwrap();

        let builtin = rift_config::EffectiveConfig::secure_default()
            .unwrap()
            .sensitive_paths()
            .len();
        let d = build_detail(root.path(), &path_of(&proj), ProjectStatus::Idle);
        assert!(
            d.settings.sensitive_path_count > builtin,
            "the custom glob is counted on top of the built-ins"
        );
        assert!(!d.settings.raw_transcripts, "raw transcripts default off");
    }

    #[test]
    fn sensitive_paths_shown_as_count_not_pattern() {
        // The dashboard must never surface the sensitive globs themselves — only a
        // count — or it would leak the very patterns the policy protects.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "leak", 1);
        let rift = proj.join(".rift");
        std::fs::create_dir_all(&rift).unwrap();
        std::fs::write(
            rift.join("config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n\
             [paths]\nsensitive_paths = [\"TOPSECRET-PATTERN/**\"]\n",
        )
        .unwrap();

        let d = build_detail(root.path(), &path_of(&proj), ProjectStatus::Idle);
        assert!(d.settings.sensitive_path_count >= 1);
        // The pattern text appears nowhere in any string field of the detail.
        let haystack = format!(
            "{} {} {:?}",
            d.path,
            d.label,
            d.models.iter().map(|m| &m.model).collect::<Vec<_>>()
        );
        assert!(
            !haystack.contains("TOPSECRET-PATTERN"),
            "the raw sensitive glob must not leak into the detail view"
        );
    }

    #[test]
    fn detail_is_scope_filtered() {
        // Two projects with independent projections: one's models never bleed into
        // the other's detail.
        let root = tempfile::tempdir().unwrap();
        let a = make_project(root.path(), "a", 1);
        let b = make_project(root.path(), "b", 1);
        let sa = open_proj(root.path(), &a);
        seed_model_call(&sa, &runtime(), "c1", "OnlyInA");

        let da = build_detail(root.path(), &path_of(&a), ProjectStatus::Idle);
        let db = build_detail(root.path(), &path_of(&b), ProjectStatus::Idle);
        assert!(da.models.iter().any(|m| m.model == "OnlyInA"));
        assert!(
            db.models.iter().all(|m| m.model != "OnlyInA"),
            "project b's detail must not reflect project a's models"
        );
    }

    #[test]
    fn active_project_gets_detail_others_do_not() {
        // The bus attaches rich detail only for the SetActive (foreground) project.
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let bus = spawn_with_root(root.path().to_path_buf(), vec![]);
        bus.req_tx
            .send(DataRequest::SetActive(Some(path_of(&proj))))
            .unwrap();
        let ev = bus
            .evt_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("a ProjectData update");
        match ev {
            Event::ProjectData(pd) => {
                assert_eq!(pd.project, path_of(&proj));
                assert!(pd.detail.is_some(), "the active project carries detail");
            }
            other => panic!("expected ProjectData, got {other:?}"),
        }
    }

    // --- P8 S2: the change observer -----------------------------------------

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
        // The ADR-0010 blind spot, closed: a rebuild that lands MORE rows than
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
        /// The watermark never goes backwards without an epoch bump, over an
        /// arbitrary interleaving of appends and rebuilds against a real store
        /// (PRD-M4 testing decision).
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
    fn bus_delivers_live_update_via_poll() {
        let root = tempfile::tempdir().unwrap();
        let proj = make_project(root.path(), "seed", 1);
        let bus = spawn_with_root(root.path().to_path_buf(), vec![path_of(&proj)]);

        // Drain the initial request-driven update.
        let first = bus.evt_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(matches!(first, Event::ProjectData(_)));

        // A concurrent append must surface via the observer's poll — with no
        // further request sent, any subsequent update is poll-driven.
        append_events(root.path(), &proj, 1, 2);
        let second = bus
            .evt_rx
            .recv_timeout(Duration::from_secs(8))
            .expect("a poll-driven update");
        match second {
            Event::ProjectData(pd) => {
                assert_eq!(pd.project, path_of(&proj));
                assert_eq!(pd.status, ProjectStatus::Idle);
            }
            other => panic!("expected ProjectData, got {other:?}"),
        }
    }
}
