//! End-to-end headless dashboard: a seeded tempdir projection feeds
//! the real bus; the real event loop reduces the updates; the real renderer
//! draws the token and model sections live — no binary in the loop, no
//! terminal, no wall-clock assertions.
//!
//! A wait for what a request produces ends on a barrier: a refresh of a path
//! no project uses, which the worker answers after everything asked before
//! it. A wait for what only the observer's poll can see keeps a delivery
//! deadline, because the public bus reports no cycle a test could count.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use rift_canopy::app::{event_loop, EventSource};
use rift_canopy::render::frame::{Frame, Size};
use rift_canopy::render::{AsciiBackend, Capabilities, RenderBackend};
use rift_canopy::{
    AppState, BreakdownView, CanopyKey, DataRequest, EntryKind, Event, PersistOp, Project,
    ProjectStatus, TokensView, ViewModel,
};
use rift_canopy_data::{runtime, spawn_poll_only_with_root};
use rift_core::Result as RiftResult;
use rift_git::repo_id_for;
use rift_store::{projection_db_under, CanonicalStore, SqliteStore, Value};

fn make_project(root: &Path, name: &str) -> String {
    let proj = root.join(name);
    std::fs::create_dir_all(&proj).unwrap();
    // A repository: the bus and the feeds resolve every store through the
    // project's family (family identity), and a main work tree's family adopts
    // exactly the `repo_id_for` id this fixture seeds under.
    rift_git::test_support::fake_repo(&proj).unwrap();
    let repo = repo_id_for(&proj).unwrap();
    let db = projection_db_under(root, &repo);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = SqliteStore::open(&db).unwrap();
    store.create_canonical_schema().unwrap();
    proj.to_string_lossy().into_owned()
}

fn seed_token_fact(root: &Path, path: &str, call_id: &str, t: i64, classes: (i64, i64, i64, i64)) {
    seed_token_fact_for_model(root, path, call_id, "claude-fable-5", t, classes);
}

fn seed_token_fact_for_model(
    root: &Path,
    path: &str,
    call_id: &str,
    model: &str,
    t: i64,
    classes: (i64, i64, i64, i64),
) {
    let repo = repo_id_for(Path::new(path)).unwrap();
    let store = SqliteStore::open(projection_db_under(root, &repo)).unwrap();
    let rt = runtime().unwrap();
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
            Value::text(call_id),
            Value::text("t1"),
            Value::text(model),
            Value::Int(t),
            Value::Int(1),
        ],
    ))
    .unwrap();
    rt.block_on(store.insert(
        "token_usage",
        &[
            "call_id",
            "uncached_input",
            "cached_input",
            "cache_creation",
            "output",
            "counter_method",
            "schema_version",
        ],
        &[
            Value::text(call_id),
            Value::Int(classes.0),
            Value::Int(classes.1),
            Value::Int(classes.2),
            Value::Int(classes.3),
            Value::text("provider_reported"),
            Value::Int(1),
        ],
    ))
    .unwrap();
}

/// Seed a `session` row (the event-anchored panel metrics anchor).
fn seed_session_row(root: &Path, path: &str, id: &str, started_at: i64) {
    let repo = repo_id_for(Path::new(path)).unwrap();
    let store = SqliteStore::open(projection_db_under(root, &repo)).unwrap();
    let rt = runtime().unwrap();
    let canonical = rift_core::capture_session_row_id("claude-code", id);
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
            Value::text(canonical.clone()),
            Value::text("claude-code"),
            Value::text("m"),
            Value::text("r"),
            Value::Int(started_at),
            Value::Int(1),
        ],
    ))
    .unwrap();
    rt.block_on(store.insert(
        "capture_session_identity",
        &["session_id", "agent", "native_session_id", "schema_version"],
        &[
            Value::text(canonical.clone()),
            Value::text("claude-code"),
            Value::text(id),
            Value::Int(1),
        ],
    ))
    .unwrap();
    rt.block_on(store.insert(
        "session_event",
        &[
            "event_id",
            "session_id",
            "kind",
            "occurred_at",
            "schema_version",
        ],
        &[
            Value::text(format!("end-{id}")),
            Value::text(canonical),
            Value::text("end"),
            Value::Int(started_at),
            Value::Int(1),
        ],
    ))
    .unwrap();
}

/// Seed one captured event for a session (advances the event anchor).
fn seed_session_event(root: &Path, path: &str, key: &str, session: &str, t: i64) {
    let repo = repo_id_for(Path::new(path)).unwrap();
    let store = SqliteStore::open(projection_db_under(root, &repo)).unwrap();
    let rt = runtime().unwrap();
    rt.block_on(store.insert(
        "captured_event",
        &[
            "idempotency_key",
            "session_id",
            "turn_ordinal",
            "event_ordinal",
            "kind",
            "trust",
            "payload_sha256",
            "occurred_at_ms",
            "schema_version",
        ],
        &[
            Value::text(key),
            Value::text(session),
            Value::Int(0),
            Value::Int(0),
            Value::text("start"),
            Value::text("trusted"),
            Value::text("sha"),
            Value::Int(t),
            Value::Int(1),
        ],
    ))
    .unwrap();
}

fn seed_checkpoint(root: &Path, path: &str, id: &str, t: i64) {
    let repo = repo_id_for(Path::new(path)).unwrap();
    let store = SqliteStore::open(projection_db_under(root, &repo)).unwrap();
    let rt = runtime().unwrap();
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
            Value::Int(t),
            Value::text("root"),
            Value::Null,
            Value::Int(1),
        ],
    ))
    .unwrap();
}

/// An event source bridging the bus into the loop: each poll hands the loop
/// every update the worker emitted before answering a fresh barrier, and it
/// quits once the given predicate over the drained history holds (or after
/// `polls_left` barriers, whichever comes first) — so the bound is the
/// worker's answers, not the wall clock.
struct BusSource {
    evt_rx: Receiver<Event>,
    /// The bus's request side, for the barriers: the loop's own requests
    /// travel the same channel, so a barrier sent from a poll follows them.
    req_tx: Sender<DataRequest>,
    /// The root the barrier paths are made under.
    root: std::path::PathBuf,
    polls_left: u32,
    done: Box<dyn FnMut(&Event) -> bool>,
    satisfied: bool,
}

impl EventSource for BusSource {
    fn poll(&mut self, _timeout: Duration) -> RiftResult<Vec<Event>> {
        if self.satisfied || self.polls_left == 0 {
            return Ok(vec![Event::Key(CanopyKey::Quit)]);
        }
        self.polls_left -= 1;
        let barrier = barrier_path(&self.root, self.polls_left);
        // A dead worker ends the run with what it sent; the assertions after
        // the loop then say which feed never arrived.
        let Some(out) = events_until_barrier(&self.req_tx, &self.evt_rx, &barrier) else {
            return Ok(vec![Event::Key(CanopyKey::Quit)]);
        };
        for ev in &out {
            if (self.done)(ev) {
                self.satisfied = true;
            }
        }
        Ok(out)
    }
}

/// A present-counting mock backend (plot inherits the block default).
struct MockBackend;
impl RenderBackend for MockBackend {
    fn name(&self) -> &'static str {
        "mock"
    }
    fn capabilities(&self) -> Capabilities {
        Capabilities::empty()
    }
    fn present(&mut self, _frame: &Frame) -> Result<(), rift_canopy::render::RenderError> {
        Ok(())
    }
}

fn frame_text(frame: &Frame) -> String {
    (0..frame.height())
        .map(|y| {
            (0..frame.width())
                .map(|x| frame.get(x, y).map_or(' ', |cell| cell.ch))
                .collect::<String>()
                + "\n"
        })
        .collect()
}

fn seed_two_model_usage(root: &Path, path: &str) {
    seed_token_fact_for_model(
        root,
        path,
        "c1",
        "claude-fable-5",
        1_000,
        (500, 2_000, 100, 700),
    );
    seed_token_fact_for_model(
        root,
        path,
        "c2",
        "claude-opus-5",
        8_000,
        (300, 4_000, 0, 500),
    );
}

#[test]
fn token_and_model_sections_render_live_from_a_seeded_projection() {
    let root = tempfile::tempdir().unwrap();
    let path = make_project(root.path(), "seeded");
    seed_two_model_usage(root.path(), &path);

    let bus = spawn_poll_only_with_root(root.path().to_path_buf(), vec![path.clone()]);
    // NO subscribe is sent by this test: the loop's own startup prime
    // must go live on its own — req_sink is wired to the real bus, exactly as
    // the interactive launch wires it.

    let mut state = fixture_dashboard_selecting(&path);
    let mut src = bus_source_until_tokens_and_models(root.path(), bus.req_tx.clone(), bus.evt_rx);
    run_the_interactive_loop(&mut state, &mut src, bus.req_tx.clone());

    // The reducer applied the live feed…
    let vm = state.snapshot();
    assert_the_reducer_applied_both_feeds(&vm);
    // …and the real renderer draws both live sections, placeholders gone.
    assert_both_live_sections_render(&vm);
    // The read fraction (read/(input+read) = 6000/6800 ≈ 88%) also remains an
    // exact reducer fact, independent of responsive geometry.
    match &vm.col3.breakdown {
        BreakdownView::Data(d) => assert!(
            (d.read_fraction - 0.882).abs() < 0.01,
            "live read fraction: {}",
            d.read_fraction
        ),
        BreakdownView::NoActivity => panic!("the breakdown feed never arrived"),
    }
}

/// A dashboard past its splash whose one project is `path`, so the loop's
/// startup prime selects it and asks the bus for its feeds.
fn fixture_dashboard_selecting(path: &str) -> AppState {
    let mut state = AppState::new(
        vec![Project {
            name: "seeded".into(),
            path: path.to_owned(),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }],
        Size::new(100, 40),
    );
    state.app.splash.skip();
    state
}

/// The loop quits once both live facts have crossed the bus: a Tokens feed
/// carrying data, and project detail naming both seeded models once each.
fn bus_source_until_tokens_and_models(
    root: &Path,
    req_tx: Sender<DataRequest>,
    evt_rx: Receiver<Event>,
) -> BusSource {
    let mut saw_tokens = false;
    let mut saw_models = false;
    BusSource {
        evt_rx,
        req_tx,
        root: root.to_path_buf(),
        polls_left: 50,
        done: Box::new(move |ev| {
            if matches!(ev, Event::Feed(f) if matches!(
                    f.payload,
                    rift_canopy::FeedPayload::Tokens { ref tokens, .. }
                        if matches!(tokens, TokensView::Data(_))))
            {
                saw_tokens = true;
            }
            if matches!(ev, Event::ProjectData(pd) if pd.detail.as_ref().is_some_and(
                    |detail| detail.models.len() == 2
                        && detail.models.iter().all(|model| model.calls == 1)))
            {
                saw_models = true;
            }
            saw_tokens && saw_models
        }),
        satisfied: false,
    }
}

/// Run the real event loop with its data requests wired to the real bus and
/// every other seam nulled, exactly as the interactive launch wires `req_sink`.
fn run_the_interactive_loop(
    state: &mut AppState,
    src: &mut BusSource,
    req_tx: Sender<DataRequest>,
) {
    let mut backend = MockBackend;
    event_loop(
        &mut backend,
        src,
        state,
        &mut |_op: &PersistOp| Ok(vec![]),
        &mut move |req: DataRequest| {
            let _ = req_tx.send(req);
        },
        &mut rift_canopy::NullProjectSettingsManager,
        &mut rift_canopy::NullDispatcher,
        &mut || Ok(vec![]),
        &mut rift_canopy::NullProfileLoader,
        // The lister seam is unused here — this loop never opens the explorer.
        &mut rift_canopy_data::list_dir,
    )
    .unwrap();
}

/// The reducer holds the live Tokens total (800 input + 6.0k read + 1.2k
/// output + 100 cache-write) and both models, in seeded order, once each.
fn assert_the_reducer_applied_both_feeds(vm: &ViewModel) {
    match &vm.col3.tokens {
        TokensView::Data(d) => assert!((d.total - 8_100.0).abs() < f64::EPSILON),
        TokensView::NoActivity => panic!("the Tokens feed never arrived"),
    }
    let detail = vm.detail.as_ref().expect("selected project detail");
    assert_eq!(detail.models.len(), 2);
    assert_eq!(detail.models[0].model, "claude-fable-5");
    assert_eq!(detail.models[0].calls, 1);
    assert_eq!(detail.models[1].model, "claude-opus-5");
    assert_eq!(detail.models[1].calls, 1);
}

/// The ASCII renderer draws the token and model sections from live data:
/// the placeholder is gone and every compact readout carries the seeded sums.
fn assert_both_live_sections_render(vm: &ViewModel) {
    let mut frame = Frame::new(vm.size);
    let mut ascii = AsciiBackend::new(Capabilities::empty());
    rift_canopy::view::draw(vm, &mut ascii, &mut frame);
    let text = frame_text(&frame);
    assert!(text.contains("token usage  Σ 8.1k"), "live total:\n{text}");
    assert!(!text.contains("no token activity yet"));
    assert!(
        text.contains("input 800 read 6.0k"),
        "the treemap footer carries compact live totals:\n{text}"
    );
    assert!(
        text.contains("output 1.2k cache-write 100") && text.contains("read 88%"),
        "the second compact footer row and read fraction render:\n{text}"
    );
    assert!(
        text.contains("models · Σ 2 calls"),
        "the repository-scoped model total renders:\n{text}"
    );
    assert!(
        text.contains("claude-fable-5")
            && text.contains("claude-opus-5")
            && text.matches("×1 · runtime").count() == 2,
        "the model treemap keeps exact keyed readouts for every used model:\n{text}"
    );
}

#[test]
fn activity_projection_flows_through_reducer_into_thirty_day_heat_strips() {
    const DAY_MS: i64 = 86_400_000;
    const TODAY: i64 = 20_000;
    const NOW: i64 = TODAY * DAY_MS + 60_000;

    let root = tempfile::tempdir().unwrap();
    let path = make_project(root.path(), "activity");
    seed_session_row(root.path(), &path, "s1", (TODAY - 10) * DAY_MS + 1_000);
    seed_session_event(
        root.path(),
        &path,
        "activity-event",
        "s1",
        (TODAY - 10) * DAY_MS + 2_000,
    );
    seed_checkpoint(
        root.path(),
        &path,
        "activity-checkpoint",
        TODAY * DAY_MS + 3_000,
    );

    let mut state = AppState::new(
        vec![Project {
            name: "activity".into(),
            path: path.clone(),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }],
        Size::new(120, 50),
    );
    state.app.splash.skip();
    let update = rift_canopy_data::feeds::compute_activity(root.path(), &path, NOW);
    let _ = rift_canopy::state::update(&mut state, Event::Feed(update));

    let vm = state.snapshot();
    let rift_canopy::ActivityView::Days(activity) = vm.activity.as_ref() else {
        panic!("the projection did not become Activity data");
    };
    assert_eq!(activity.days.len(), 30);
    assert_eq!(activity.days.first().unwrap().events, 0);
    assert_eq!(activity.days[19].events, 1);
    assert_eq!(activity.days.last().unwrap().checkpoints, 1);

    let mut frame = Frame::new(vm.size);
    let mut ascii = AsciiBackend::new(Capabilities::empty());
    rift_canopy::view::draw(&vm, &mut ascii, &mut frame);
    let text: String = (0..frame.height())
        .map(|y| {
            (0..frame.width())
                .map(|x| frame.get(x, y).map_or(' ', |cell| cell.ch))
                .collect::<String>()
                + "\n"
        })
        .collect();
    assert!(
        text.contains("activity · 30d"),
        "thirty-day card title:\n{text}"
    );
    assert!(text.contains("-29d"), "oldest relative tick:\n{text}");
    assert!(text.contains("today"), "today relative tick:\n{text}");
    assert!(
        text.chars().any(|ch| ".:-=+*#%@".contains(ch)),
        "projection facts produce density-shaded strips:\n{text}"
    );
}

#[test]
fn a_mid_run_rebuild_drives_a_full_re_read() {
    let root = tempfile::tempdir().unwrap();
    let path = make_project(root.path(), "reb");
    seed_token_fact(root.path(), &path, "c1", 1_000, (100, 0, 0, 0));

    let bus = spawn_poll_only_with_root(root.path().to_path_buf(), vec![path.clone()]);
    bus.req_tx
        .send(DataRequest::SetActive(Some(path.clone())))
        .unwrap();

    // First live value arrives (total 100), in the SetActive's cycle.
    let barrier = barrier_path(root.path(), 0);
    let saw_first = events_until_barrier(&bus.req_tx, &bus.evt_rx, &barrier)
        .unwrap_or_default()
        .iter()
        .any(|ev| tokens_total_is(ev, 100.0));
    assert!(saw_first, "the first Tokens feed never arrived");

    // Rebuild the projection mid-run (epoch bump) and land MORE facts than
    // before — the poller re-bootstraps and the feed reflects the fresh read.
    let repo = repo_id_for(Path::new(&path)).unwrap();
    let db = projection_db_under(root.path(), &repo);
    rift_store::rebuild(&db).unwrap();
    seed_token_fact(root.path(), &path, "n1", 2_000, (300, 0, 0, 0));
    seed_token_fact(root.path(), &path, "n2", 3_000, (400, 0, 0, 0));

    // Only the observer's poll sees a rebuild, and the public bus reports no
    // cycle to count, so this wait keeps its delivery deadline.
    let mut saw_rebuilt = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if let Ok(ev) = bus.evt_rx.recv_timeout(Duration::from_millis(300)) {
            if tokens_total_is(&ev, 700.0) {
                saw_rebuilt = true;
                break;
            }
        }
    }
    assert!(
        saw_rebuilt,
        "the post-rebuild feed never reflected the fresh scoped read"
    );
}

#[test]
fn system_counts_hold_while_idle_and_move_on_capture() {
    let root = tempfile::tempdir().unwrap();
    let path = make_project(root.path(), "sess");
    const S0: i64 = 1_000_000;
    seed_session_row(root.path(), &path, "s1", S0);
    seed_session_event(root.path(), &path, "e1", "s1", S0 + 60_000);

    let bus = spawn_poll_only_with_root(root.path().to_path_buf(), vec![path.clone()]);
    bus.req_tx
        .send(DataRequest::SetActive(Some(path.clone())))
        .unwrap();

    let barrier = barrier_path(root.path(), 0);
    let held = events_until_barrier(&bus.req_tx, &bus.evt_rx, &barrier)
        .unwrap_or_default()
        .into_iter()
        .find_map(|ev| system_metrics_with_count(ev, 1))
        .expect("the first System feed never arrived");

    // IDLE: nothing captured. Wall-clock advances across several poll
    // cycles — any Metrics that arrives (idempotent recomputes are allowed)
    // must carry the IDENTICAL held value; a drifted value is the old
    // trailing-window bug.
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    while std::time::Instant::now() < deadline {
        if let Ok(Event::Feed(f)) = bus.evt_rx.recv_timeout(Duration::from_millis(250)) {
            if let rift_canopy::FeedPayload::System { metrics: m, .. } = &f.payload {
                assert_eq!(m, &held, "an idle recompute must hold the value exactly");
            }
        }
    }

    // Only the observer's poll sees the capture, so this wait keeps its
    // delivery deadline.
    seed_session_event(root.path(), &path, "e2", "s1", S0 + 4 * 60_000);
    let moved = wait_for_event_count(&bus.evt_rx, 2, Duration::from_secs(15))
        .expect("the re-anchored System feed never arrived");
    assert!(
        matches!(
            moved.activity_scope,
            rift_canopy::SystemScope::LastCompletedSession
        ),
        "no cache means an explicitly labelled completed-session hold"
    );
}

/// Drain the bus until a System feed with the given event count arrives.
fn wait_for_event_count(
    rx: &Receiver<Event>,
    count: u64,
    total: Duration,
) -> Option<rift_canopy::PanelMetrics> {
    let deadline = std::time::Instant::now() + total;
    while std::time::Instant::now() < deadline {
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(250)) {
            if let Some(metrics) = system_metrics_with_count(ev, count) {
                return Some(metrics);
            }
        }
    }
    None
}

/// The System panel's metrics, when `ev` is a System feed with `count`
/// events in its window.
fn system_metrics_with_count(ev: Event, count: u64) -> Option<rift_canopy::PanelMetrics> {
    let Event::Feed(f) = ev else {
        return None;
    };
    match f.payload {
        rift_canopy::FeedPayload::System { metrics, .. } if metrics.events_5m == Some(count) => {
            Some(metrics)
        }
        _ => None,
    }
}

/// Whether `ev` is a Tokens feed carrying data that totals `total`.
fn tokens_total_is(ev: &Event, total: f64) -> bool {
    let Event::Feed(f) = ev else {
        return false;
    };
    matches!(
        &f.payload,
        rift_canopy::FeedPayload::Tokens { tokens: TokensView::Data(d), .. }
            if (d.total - total).abs() < f64::EPSILON
    )
}

/// A barrier path: a child of the test's root that no project uses, whose
/// `~` sorts after every fixture project's name. The worker flushes one
/// cycle's updates sorted by project path, so the barrier's answer is the
/// last thing its cycle sends. Unique per `n`: a rebuild re-reads every
/// tracked path, and a reused barrier could then answer early.
fn barrier_path(root: &Path, n: u32) -> String {
    root.join(format!("~barrier-{n}"))
        .to_string_lossy()
        .into_owned()
}

/// Send a refresh of `barrier` and return every update the worker emitted
/// before answering it — `None` when the worker stopped first. Requests
/// reach the worker in order, so all asked before the barrier has been read
/// by then; a live worker always answers a refresh, so no clock is needed.
fn events_until_barrier(
    req_tx: &Sender<DataRequest>,
    evt_rx: &Receiver<Event>,
    barrier: &str,
) -> Option<Vec<Event>> {
    req_tx.send(DataRequest::Refresh(barrier.to_owned())).ok()?;
    let mut seen = Vec::new();
    loop {
        match evt_rx.recv().ok()? {
            Event::ProjectData(data) if data.project == barrier => return Some(seen),
            ev => seen.push(ev),
        }
    }
}
