//! End-to-end headless dashboard (M4-12): a seeded tempdir projection feeds
//! the real bus; the real event loop reduces the updates; the real renderer
//! draws the token sections live — no binary in the loop (PRD-M4 Testing
//! Decisions), no terminal, no wall-clock assertions beyond generous
//! delivery timeouts.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use rift_canopy::app::{event_loop, EventSource};
use rift_canopy::render::frame::{Frame, Size};
use rift_canopy::render::{AsciiBackend, Capabilities, RenderBackend};
use rift_canopy::{
    AppState, BreakdownView, CanopyKey, DataRequest, EntryKind, Event, NullFeatureManager,
    PersistOp, Project, ProjectStatus, TokensView,
};
use rift_canopy_data::{runtime, spawn_with_root};
use rift_core::Result as RiftResult;
use rift_git::repo_id_for;
use rift_store::{projection_db_under, CanonicalStore, SqliteStore, Value};

fn make_project(root: &Path, name: &str) -> String {
    let proj = root.join(name);
    std::fs::create_dir_all(&proj).unwrap();
    let repo = repo_id_for(&proj).unwrap();
    let db = projection_db_under(root, &repo);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = SqliteStore::open(&db).unwrap();
    store.create_canonical_schema().unwrap();
    proj.to_string_lossy().into_owned()
}

fn seed_token_fact(root: &Path, path: &str, call_id: &str, t: i64, classes: (i64, i64, i64, i64)) {
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
            Value::text("claude-fable-5"),
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

/// An event source bridging the bus into the loop: drains bus updates each
/// poll, and quits once the given predicate over the drained history holds
/// (or after `max_polls`, whichever comes first).
struct BusSource {
    evt_rx: Receiver<Event>,
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
        let mut out = Vec::new();
        // Block briefly for the first update, then drain the burst.
        if let Ok(ev) = self.evt_rx.recv_timeout(Duration::from_millis(400)) {
            out.push(ev);
        }
        while let Ok(ev) = self.evt_rx.try_recv() {
            out.push(ev);
        }
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

#[test]
fn token_sections_render_live_from_a_seeded_projection() {
    let root = tempfile::tempdir().unwrap();
    let path = make_project(root.path(), "seeded");
    seed_token_fact(root.path(), &path, "c1", 1_000, (500, 2_000, 100, 700));
    seed_token_fact(root.path(), &path, "c2", 8_000, (300, 4_000, 0, 500));

    let bus = spawn_with_root(root.path().to_path_buf(), vec![path.clone()]);
    // NO subscribe is sent by this test (M5-02): the loop's own startup prime
    // must go live on its own — req_sink is wired to the real bus, exactly as
    // the interactive launch wires it.

    let mut state = AppState::new(
        vec![Project {
            name: "seeded".into(),
            path: path.clone(),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }],
        Size::new(100, 40),
    );
    state.app.splash.skip();

    let mut src = BusSource {
        evt_rx: bus.evt_rx,
        polls_left: 50,
        done: Box::new(|ev| {
            matches!(ev, Event::Feed(f) if matches!(
                f.payload,
                rift_canopy::FeedPayload::Tokens { ref tokens, .. }
                    if matches!(tokens, TokensView::Data(_))
            ))
        }),
        satisfied: false,
    };
    let mut backend = MockBackend;
    let req_tx = bus.req_tx.clone();
    event_loop(
        &mut backend,
        &mut src,
        &mut state,
        &mut |_op: &PersistOp| Ok(vec![]),
        &mut move |req: DataRequest| {
            let _ = req_tx.send(req);
        },
        &mut NullFeatureManager,
        &mut rift_canopy::NullDispatcher,
        &mut rift_canopy::NullEditorLauncher,
        &mut || Ok(vec![]),
    )
    .unwrap();

    // The reducer applied the live feed…
    let vm = state.snapshot();
    match &vm.col3.tokens {
        TokensView::Data(d) => assert!((d.total - 8_100.0).abs() < f64::EPSILON),
        TokensView::NoActivity => panic!("the Tokens feed never arrived"),
    }
    // …and the real renderer draws both live sections, placeholders gone.
    let mut frame = Frame::new(vm.size);
    let mut ascii = AsciiBackend::new(Capabilities::empty());
    rift_canopy::view::draw(&vm, &mut ascii, &mut frame);
    let text: String = (0..frame.height())
        .map(|y| {
            (0..frame.width())
                .map(|x| frame.get(x, y).map_or(' ', |c| c.ch))
                .collect::<String>()
                + "\n"
        })
        .collect();
    assert!(text.contains("token usage  Σ 8.1K"), "live total:\n{text}");
    assert!(!text.contains("no token activity yet"));
    // The read fraction (read/(input+read) = 6000/6800 ≈ 88%) flows through the
    // live feed. It surfaces in the token panel's read tile, which the narrow
    // col3 column lays out vertically and can push below the fold (ADR-0030 axis /
    // ADR-0031 scroll), so verify the *data* the render draws from — the source of
    // truth — not a layout-fragile substring.
    match &vm.col3.breakdown {
        BreakdownView::Data(d) => assert!(
            (d.read_fraction - 0.882).abs() < 0.01,
            "live read fraction: {}",
            d.read_fraction
        ),
        BreakdownView::NoActivity => panic!("the breakdown feed never arrived"),
    }
}

#[test]
fn a_mid_run_rebuild_drives_a_full_re_read() {
    let root = tempfile::tempdir().unwrap();
    let path = make_project(root.path(), "reb");
    seed_token_fact(root.path(), &path, "c1", 1_000, (100, 0, 0, 0));

    let bus = spawn_with_root(root.path().to_path_buf(), vec![path.clone()]);
    bus.req_tx
        .send(DataRequest::SetActive(Some(path.clone())))
        .unwrap();

    // First live value arrives (total 100).
    let mut saw_first = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if let Ok(Event::Feed(f)) = bus.evt_rx.recv_timeout(Duration::from_millis(300)) {
            if let rift_canopy::FeedPayload::Tokens {
                tokens: TokensView::Data(d),
                ..
            } = &f.payload
            {
                if (d.total - 100.0).abs() < f64::EPSILON {
                    saw_first = true;
                    break;
                }
            }
        }
    }
    assert!(saw_first, "the first Tokens feed never arrived");

    // Rebuild the projection mid-run (epoch bump) and land MORE facts than
    // before — the poller re-bootstraps and the feed reflects the fresh read.
    let repo = repo_id_for(Path::new(&path)).unwrap();
    let db = projection_db_under(root.path(), &repo);
    rift_store::rebuild(&db).unwrap();
    seed_token_fact(root.path(), &path, "n1", 2_000, (300, 0, 0, 0));
    seed_token_fact(root.path(), &path, "n2", 3_000, (400, 0, 0, 0));

    let mut saw_rebuilt = false;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        if let Ok(Event::Feed(f)) = bus.evt_rx.recv_timeout(Duration::from_millis(300)) {
            if let rift_canopy::FeedPayload::Tokens {
                tokens: TokensView::Data(d),
                ..
            } = &f.payload
            {
                if (d.total - 700.0).abs() < f64::EPSILON {
                    saw_rebuilt = true;
                    break;
                }
            }
        }
    }
    assert!(
        saw_rebuilt,
        "the post-rebuild feed never reflected the fresh scoped read"
    );
}
