//! The events that enter the reducer: input, the bus's per-project and
//! per-feed updates, and the worker's health and wake-mode reports.
//!
//! Its own module because these types are the crate's seam to
//! `rift-canopy-data` and the `rift` binary: they name what the outside world
//! can tell the dashboard, and nothing in here decides anything.

use crate::col2::DetailView;
use crate::dispatch::{DispatchOutcome, DispatchRequest};
use crate::input::{CanopyKey, MouseInput};
use crate::render::frame::Size;
use crate::ProjectStatus;

/// An event entering the reducer. Canopy-native (the loop translates terminal
/// events into these), so the state layer never names an engine type.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A normalized key press.
    Key(CanopyKey),
    /// A normalized mouse event (click / scroll) at a cell coordinate.
    Mouse(MouseInput),
    /// The terminal was resized.
    Resize(Size),
    /// A fixed-cadence animation/sampling tick.
    Tick,
    /// A per-project update from the data bus: live status for one project.
    ProjectData(ProjectData),
    /// A column-3 feed update from the data bus (the data-plane split's Tokens/Nic/Recall
    /// trio): full section state for one project, applied when it is the
    /// selected one.
    Feed(FeedUpdate),
    /// Read-only Recall Viewer worker response.
    Viewer(crate::recall_viewer::ViewerEvent),
    /// A delegated dispatch completed (the loop ran it synchronously); the
    /// reducer renders the outcome into the results overlay.
    DispatchDone {
        /// The request that ran.
        request: DispatchRequest,
        /// What it produced.
        outcome: DispatchOutcome,
    },
    /// A data-plane health report from the bus worker (the dashboard information architecture).
    /// Emitted on change only; the header dot renders the folded state.
    Health(HealthEvent),
    /// How the bus worker is woken: sent ONCE, first,
    /// right after the worker attached (or failed to attach) its filesystem
    /// watcher. `PollOnly` is the silent degrade that used to be invisible;
    /// the heartbeat tightens on it and the System panel says so.
    WatchMode(WatchMode),
}

/// How the data bus learns that something changed: a native filesystem
/// watcher, or the timeout poll alone (the watcher failed to attach, or the
/// platform has none). The dashboard adapts its idle re-stat cadence to this
/// (`refresh_every_ticks`) instead of degrading silently.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WatchMode {
    /// A native watcher wakes the worker promptly; the heartbeat is slow.
    #[default]
    Native,
    /// No watcher — the poll timeout is the only wake; the heartbeat is fast.
    PollOnly,
}

/// What the bus worker observed about its own data plane in one read cycle.
/// The header dot folds these through [`fold_health`]: a degraded state holds
/// until a healthy cycle restores it (never a timer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HealthEvent {
    /// Reads completed with the worker infrastructure and canopy store sound.
    Healthy,
    /// The worker hit an internal failure (e.g. its async runtime).
    WorkerError,
    /// The machine-global canopy store could not be opened.
    StoreUnreachable,
}

/// One feed's full section state for one project. Each update carries the
/// **complete** recomputed view (never a delta), so dropped or duplicated
/// delivery converges on the fresh-read truth by construction — the
/// delivery-robustness property this design owes.
#[derive(Clone, Debug, PartialEq)]
pub struct FeedUpdate {
    /// The project (canonical path) this feed update is for.
    pub project: String,
    /// Which sections it fills.
    pub payload: FeedPayload,
}

/// The feed kinds (the data-plane split), each patching its dashboard surface.
#[derive(Clone, Debug, PartialEq)]
pub enum FeedPayload {
    /// Token spend + the input|read|output breakdown.
    Tokens {
        /// The token-usage section.
        tokens: crate::col3::TokensView,
        /// The breakdown section.
        breakdown: crate::col3::BreakdownView,
    },
    /// One atomic System-panel snapshot: context, recent activity/storage, and
    /// the shared active-session count always advance together.
    System {
        /// Main-chain context pressure.
        context: crate::col3::NicView,
        /// Five-minute activity, storage, and explicit scopes.
        metrics: crate::col3::PanelMetrics,
        /// Top-level capture sessions currently live.
        active_sessions: crate::col3::ActiveSessionsView,
    },
    /// The Recall savings and per-channel serve-call chart.
    Recall {
        /// Recall savings.
        savings: crate::col3::RecallView<crate::col3::SavingsData>,
        /// Recall serve calls.
        calls: crate::col3::RecallView<crate::col3::CallsData>,
    },
    /// The activity card's trailing-month heat strips. Patches
    /// the column-2 activity card, not column 3.
    Activity(crate::col3::ActivityView),
    /// `rift dispatch`'s window facts as a card.
    ///
    /// Its own feed, not a field of another one, for the reason the four above
    /// are separate: a feed carries **full section state** and coalesces under
    /// its own key, so a dropped or duplicated delivery converges on the
    /// fresh-read truth for this panel alone.
    Dispatch(crate::col3::DispatchView),
    /// `rift recap`'s window facts as a card.
    Recap(crate::col3::RecapView),
}

/// A per-project update from the live data bus. Carries already-scoped,
/// read-only values for one project, keyed by its canonical path. The bus
/// computes the status off-thread from the projection; the reducer just stores it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectData {
    /// The project key (its canonical path) this update is for.
    pub project: String,
    /// The freshly-derived live status.
    pub status: ProjectStatus,
    /// The rich column-2 detail — `Some` only for the **active** project (the bus
    /// computes it just for the foreground selection); `None` for status-only
    /// updates of the other projects.
    pub detail: Option<DetailView>,
}

/// Fold one health event into the shown `Health`: the latest report wins,
/// and only a healthy cycle restores green (the recover-on-healthy-poll rule).
#[must_use]
pub fn fold_health(ev: HealthEvent) -> crate::theme::Health {
    match ev {
        HealthEvent::Healthy => crate::theme::Health::Ok,
        HealthEvent::WorkerError => crate::theme::Health::Warn,
        HealthEvent::StoreUnreachable => crate::theme::Health::Down,
    }
}
