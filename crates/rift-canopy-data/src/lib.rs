//! rift-canopy-data — the dashboard's data plane.
//!
//! Owns every dashboard read and every UI-fact persist: the live bus and its
//! worker, the watermark poller and change observation, the append-only
//! UI-facts store over the canopy registry tables, the status/detail builders,
//! and the graph feeds. `rift-canopy` (the render/reducer side) never imports
//! this crate — the edge points one way, so the dashboard stays storage-free
//! and this plane is testable against tempdir projections without the binary.
//!
//! # `checkpoint.created_at` is not always a time — an open gap
//!
//! The capture projection's `checkpoint.created_at` carries an explicit
//! unknown-time sentinel (`rift_shadow::CREATED_AT_UNKNOWN`) whenever the shard
//! records no time of its own, which is every native shard. Measured
//! 2026-08-18 over the 574 projections on this machine: **336 checkpoint rows
//! carry the sentinel and 364 carry a recorded time**, so a reader may neither
//! assume it is always a time nor assume it never is.
//!
//! `rift dispatch` resolves it — recorded time wins, else the tightest lower
//! bound derivable from the sessions the shard archives, else *unresolved*
//! (excluded from bounded windows, included in the unbounded one, and counted
//! in every report).
//!
//! **That derivation used to live in `bins/rift/src/workflows.rs`, inside a
//! bin-only package this crate could not reach, and this paragraph used to say
//! so. It moved on 2026-08-19.** It is
//! [`rift_store::window::CHECKPOINT_TIME_CTE`] and the sentinel is
//! [`rift_store::CREATED_AT_UNKNOWN`]; `rift-canopy-data` has depended on
//! `rift-store` all along, so linking them added **no crate-graph edge** and
//! rewrote no reviewable crate-graph snapshot. The reason for the enumeration
//! therefore changed: it is no longer a record of sites stuck re-deriving a
//! rule they cannot import, it is a record of **which arm each site takes and
//! whether it takes it through the shared derivation or around it**.
//!
//! The four sites are still **enumerated, not summarized**:
//!
//! 1. `bus::read_projection_detail` fills
//!    `CaptureSummary::last_checkpoint_at_ms` with
//!    `SELECT created_at FROM checkpoint ORDER BY created_at DESC LIMIT 1`.
//!    **STILL OPEN, and its stated blocker is now gone.** On the projection
//!    for this repository that query returns `-1`, so the field holds a
//!    sentinel typed as an epoch-millisecond instant. Nothing renders the
//!    field today (checked workspace-wide), which is the only reason a
//!    sentinel is not on screen. This entry used to end *"not fixed here:
//!    classifying `-1` requires the constant"* — the constant is reachable
//!    now, so that excuse has been deleted rather than left to age. It stays
//!    open because it is a different site with a different question (whether
//!    `last_checkpoint_at_ms` becomes an `Option` across the crate boundary),
//!    not because anything prevents it.
//! 2. [`feeds::compute_activity`]'s thirty-day strips read the same column
//!    **through [`rift_store::window::CHECKPOINT_TIME_CTE`]**, so a shard that
//!    records no time of its own is placed at `MAX(session.started_at)` over
//!    the sessions it archives rather than dropped. Until 2026-08-19 they
//!    bucketed the raw column, which put the `-1` sentinel in 1969 and placed
//!    **none** of it: on a repository of native shards the checkpoint row was
//!    structurally zero. **CLOSED for the sentinel, CLOSED for silence, and
//!    still OPEN for cause** — a row the derivation cannot resolve at all is
//!    counted into [`rift_canopy::OffStrip`] and shown, but that count still
//!    cannot say whether such a row is unresolvable or merely older than the
//!    window.
//! 3. [`viewer::ViewerWorker`]'s checkpoint catalogue also uses
//!    [`rift_store::window::CHECKPOINT_TIME_CTE`]. A non-sentinel time stored in
//!    checkpoint metadata outranks the CTE result; otherwise its effective time
//!    is labelled session-derived, then falls back to anchor-commit time, then
//!    unknown. The sentinel is filtered before it can reach the display model.
//!    The same builder also selects `FROM checkpoint_worktree`
//!    for the catalogue's worktree badge (worktree provenance). That is a **different**
//!    v14 relation which merely shares the prefix the enumeration matches on;
//!    it reads no time column, so it takes no arm of the sentinel rule, and
//!    its own absence arm is provenance's own — no row badges `unknown`, never
//!    `main`. It
//!    is enumerated anyway rather than exempted: a guard taught to skip prefix
//!    matches stops seeing the readers it exists for.
//!
//! 4. [`workflows::compute_workflow_cards`] reads the table through
//!    [`rift_store::window::gather`] — the whole checkpoint-anchored cascade,
//!    the verbs' own function — so the dispatch/recap cards take exactly the
//!    arms `rift dispatch` takes: recorded time wins, a shard without one is
//!    placed by the sessions it archives and **labelled derived on the card**,
//!    and a shard with neither is unresolved and shown as its own figure so
//!    "excluded" never reads as "absent". **CLOSED for the sentinel.** It is
//!    also the site that made the enumeration grow a FOURTH spelling: it names
//!    neither the table, nor the CTE, nor any SQL, so the three older spellings
//!    could not see it.
//!
//! Nothing else in this crate reads that table; the other `created_at` columns
//! here belong to Canopy's **own** append-only registry tables, whose times
//! this crate mints itself and which carry no sentinel. That sentence is not
//! taken on trust: `tests/checkpoint_time_readers.rs` pins the list, so a fifth
//! reader of `checkpoint` reddens rather than quietly joining a summary. What
//! the guard covers, and the assembly shape it would not notice, are stated in
//! its own module docs.
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

pub mod bus;
pub mod dirlist;
pub mod feeds;
pub mod loader;
pub mod series;
pub mod session;
pub mod store;
pub mod viewer;
pub mod workflows;

pub use bus::{
    project_settings_view, spawn, spawn_poll_only_with_root, spawn_read_only_viewer,
    spawn_with_root, Bus,
};
pub use dirlist::list_dir;
pub use loader::StoreProfileLoader;
pub use series::{saturating_sum, Sample, Series, Unit};
pub use session::{active_sessions, ACTIVE_WINDOW_MS};
pub use store::{runtime, CanopyError, CanopyStore};
pub use viewer::ViewerWorker;
pub use workflows::compute_workflow_cards;
