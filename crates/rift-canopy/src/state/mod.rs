//! Unidirectional state: a pure reducer + an immutable render snapshot.
//!
//! The [event loop](crate::app) feeds [`Event`]s into the **pure** [`update`]
//! reducer, which mutates [`AppState`] and returns the side effects to perform
//! ([`Effects`]) — never doing I/O itself. The renderer never reads `AppState`;
//! the loop derives an immutable [`ViewModel`] snapshot per dirty frame, so
//! [`crate::view::draw`] is a pure function of state (the foundation every
//! headless/snapshot test relies on).
//!
//! The dashboard model and the key handling are reused wholesale: [`AppState`]
//! wraps the existing [`App`], and [`update`] hands each event family to its
//! reducer in `reduce`, where a key's [`crate::input::on_key`] action folds
//! into effects.

mod actions;
mod anim;
mod click;
mod effects;
mod events;
mod mouse;
mod openers;
mod reduce;
mod verbs;
mod view_model;
mod viewer;

use std::sync::Arc;

use crate::col2::DetailView;
use crate::dispatch::Overlay;
use crate::hit::Target;
use crate::render::frame::Size;
use crate::{App, Panel};

use anim::Col3Anim;
use mouse::on_mouse;
use reduce::{
    apply_feed, on_dispatch_done, on_health, on_key, on_project_data, on_resize, on_tick,
    on_watch_mode,
};
use viewer::apply_viewer_event;

pub use effects::{DataRequest, Effects, Flow, PersistOp};
pub use events::{
    fold_health, Event, FeedPayload, FeedUpdate, HealthEvent, ProjectData, WatchMode,
};
pub use view_model::{ProjectRow, ViewModel};

/// Which panel currently holds keyboard focus (the spec's `Focus`; identical to
/// the existing [`Panel`]).
pub type Focus = Panel;

/// The whole dashboard state: the existing [`App`] model plus loop metadata.
#[derive(Clone, Debug)]
pub struct AppState {
    /// The dashboard model (projects, selection, mode, command buffer, …).
    pub app: App,
    /// The current terminal size.
    pub size: Size,
    /// Set when something observable changed; gates redraws.
    pub dirty: bool,
    /// Honor OS/user reduced-motion: skip col-3 tweens, snap to each new value.
    pub reduced_motion: bool,
    /// The binary's version string, injected at launch (the render crate stays
    /// env-free); empty hides the header's version segment.
    pub version: String,
    /// The data-plane health the header dot shows (folded from bus reports).
    pub health: crate::theme::Health,
    /// The active profile's surface theme: Rift dark plus the
    /// profile's slot overrides. Swapped (dirtying) on profile change and on a
    /// live picker apply; rides the snapshot so the renderer stays pure.
    theme: crate::theme::Theme,
    /// Idle-tick counter driving the background re-stat cadence (never dirties).
    tick: u32,
    /// How the bus is woken, as it last reported (`Native` until told
    /// otherwise — the pre-report cadence is the watched one, exactly as
    /// before the report existed).
    watch_mode: WatchMode,
    /// The bus-computed rich detail for the active project (keyed by its path);
    /// the snapshot shares it when it matches the selection, else a thin fallback.
    detail: Option<Arc<DetailView>>,
    /// The last snapshot's project rows, shared with every [`ViewModel`] until the
    /// underlying projects change (checked by cheap comparison in [`Self::snapshot`],
    /// so a stale row can never be served).
    rows_cache: Arc<Vec<ProjectRow>>,
    /// The last snapshot's layout model, shared with every
    /// [`ViewModel`] until `app.layout` changes — pointer-equal on quiescent
    /// frames, like `rows_cache`.
    layout_cache: Arc<crate::layout_model::LayoutModel>,
    /// The column-3 sections' view-model, filled by the data plane's feeds
    /// `Default` is every section's honest placeholder. This is always the
    /// **target** value; the displayed value is [`Self::snapshot`]'s interpolation
    /// when a tween is in flight.
    col3: Arc<crate::col3::Col3>,
    /// The in-flight column-3 tween, if any (drives the loop's fast-poll window).
    col3_anim: Option<Col3Anim>,
    /// The selected project's activity-card data, filled by the
    /// Activity feed; `Default` is the honest placeholder. Column-2, so it sits
    /// outside the col3 tween machinery (activity snaps; polish may come later).
    activity: Arc<crate::col3::ActivityView>,
    /// The suggest panel's fade-in progress (0..=1, dt-advanced under the
    /// shared animator contract; snaps to 1 under reduced motion / ASCII).
    suggest_fade: f32,
    /// The one-shot startup sweep progress (0..=1). Rests at 1.0 (done);
    /// armed to 0.0 once per launch by [`Self::arm_startup_sweep`], then advances
    /// to 1.0 and **never re-fires**. Reduced motion / headless stays at 1.0.
    startup_sweep: f32,
    /// A transient selection/hover pulse (1.0 → 0.0 decay). Armed on a
    /// selection change; a brief colour lerp acknowledges the cursor.
    select_pulse: f32,
    /// A transient button press-flash (1.0 → 0.0 decay), keyed by
    /// [`Self::pressed`] so the right button lights. Distinct from
    /// `select_pulse`: this is bound to the activation event, not a selection
    /// change, so re-pressing the same button re-arms it.
    press_pulse: f32,
    /// The button whose press-flash is in flight (keys `press_pulse`).
    pressed: Option<Target>,
    /// The open [`Overlay::Notice`]'s remaining settle (1.0 → 0.0).
    /// Armed to 1.0 when a Notice opens, decayed by `dt` in [`Self::advance_anim`],
    /// which dismisses the Notice when it reaches 0 — a bounded auto-dismiss.
    notice_settle: f32,
    /// A project just registered through the add flow, whose first status
    /// read decides whether Project Settings opens on it. Adding a project is
    /// the moment a user expects Rift to look at the machine and offer the
    /// agents it found — but a repository already set up here needs no
    /// wizard, and the add flow cannot know which case it is until the data
    /// plane reads it, so the decision waits for that read (the
    /// `ProjectData` arm of [`update`]) rather than opening blind.
    pub pending_setup_prompt: Option<String>,
    /// Set at a pinned launch whose resolved profile differs from the store's
    /// last-selected fold: launch writes nothing, so the loop
    /// prepends the synchronizing `SelectProfile` to the session's **first**
    /// persist batch — observation never mutates; the first mutation aligns
    /// the fold with what the session shows.
    pub select_sync_pending: bool,
}

impl AppState {
    /// A fresh state for the given projects and size.
    #[must_use]
    pub fn new(projects: Vec<crate::Project>, size: Size) -> Self {
        Self {
            app: App::new(projects),
            size,
            tick: 0,
            watch_mode: WatchMode::Native,
            dirty: true,
            reduced_motion: false,
            version: String::new(),
            health: crate::theme::Health::Ok,
            theme: crate::theme::Theme::rift_dark(),
            detail: None,
            rows_cache: Arc::new(Vec::new()),
            layout_cache: Arc::new(crate::layout_model::LayoutModel::default()),
            col3: Arc::new(crate::col3::Col3::default()),
            col3_anim: None,
            activity: Arc::new(crate::col3::ActivityView::default()),
            suggest_fade: 1.0,
            startup_sweep: 1.0,
            select_pulse: 0.0,
            press_pulse: 0.0,
            pressed: None,
            notice_settle: 0.0,
            pending_setup_prompt: None,
            select_sync_pending: false,
        }
    }

    /// Open a blocking [`Overlay::Alert`] (an error / warning message)
    /// — the one door error sites route through. Dirties so the modal presents.
    pub fn open_alert(&mut self, message: crate::dispatch::Message) {
        self.app.open_overlay(Overlay::Alert(message));
        self.notice_settle = 0.0;
        self.dirty = true;
    }

    /// Open a brief [`Overlay::Notice`] (a success/info message) and arm
    /// its `dt`-timed auto-dismiss settle. Dirties so the toast presents; the
    /// loop's fast-poll window carries the settle to its bounded close.
    pub fn open_notice(&mut self, message: crate::dispatch::Message) {
        self.app.open_overlay(Overlay::Notice(message));
        self.notice_settle = 1.0;
        self.dirty = true;
    }

    /// Swap the active surface theme: Rift dark plus a profile's
    /// slot overrides, dirtying only on a real change — the set-theme path the
    /// picker's live apply and the profile switch both ride.
    pub fn set_theme(&mut self, overrides: crate::theme::ThemeOverrides) {
        let next = crate::theme::Theme::with_overrides(overrides);
        if next != self.theme {
            self.theme = next;
            self.dirty = true;
        }
    }

    /// The active surface theme (the snapshot copies it each frame).
    #[must_use]
    pub fn theme(&self) -> crate::theme::Theme {
        self.theme
    }

    /// Apply a project's saved dashboard arrangement — the loop's path
    /// after loading the layout for the newly-active project. Dirties (so the new
    /// grid presents) only on a real change.
    pub fn set_layout(&mut self, layout: crate::layout_model::LayoutModel) {
        if self.app.layout != layout {
            self.app.layout = layout;
            self.dirty = true;
        }
    }

    /// The focused panel.
    #[must_use]
    pub fn focus(&self) -> Focus {
        self.app.focus
    }

    /// Apply a per-project update from the bus: set the matching project's live
    /// status (matched by canonical path) and store the rich detail when the bus
    /// computed it (only for the active project). Returns `true` if anything
    /// changed, so the loop can gate the redraw (a no-op update must not redraw).
    pub(super) fn apply_project_data(&mut self, pd: &ProjectData) -> bool {
        let mut changed = false;
        for p in &mut self.app.projects {
            if p.path == pd.project && p.status != pd.status {
                p.status = pd.status;
                changed = true;
            }
        }
        if let Some(detail) = &pd.detail {
            if self.detail.as_deref() != Some(detail) {
                self.detail = Some(Arc::new(detail.clone()));
                changed = true;
            }
        }
        changed
    }
}

/// The **pure** reducer: events in, next state + [`Effects`] out. No I/O — every
/// side effect is returned, never performed here (the loop performs them).
/// One arm per event family; each family's reducer lives in `reduce`.
pub fn update(s: &mut AppState, ev: Event) -> Flow {
    match ev {
        Event::Resize(size) => on_resize(s, size),
        Event::Tick => on_tick(s),
        Event::ProjectData(pd) => on_project_data(s, &pd),
        Event::Feed(update) => {
            apply_feed(s, update);
            Flow::Continue(Effects::default())
        }
        Event::Viewer(event) => Flow::Continue(apply_viewer_event(s, event)),
        Event::Key(key) => on_key(s, key),
        Event::Mouse(m) => on_mouse(s, m),
        Event::Health(ev) => on_health(s, ev),
        Event::WatchMode(mode) => on_watch_mode(s, mode),
        Event::DispatchDone { request, outcome } => on_dispatch_done(s, &request, &outcome),
    }
}

#[cfg(test)]
mod fixtures;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::fixtures::*;

    #[test]
    fn an_empty_registry_raises_the_wizard_modal() {
        // Construction raises it; `ready` dismisses it, so build directly.
        let s = AppState::new(vec![], Size::new(120, 40));
        assert!(s.app.showing_wizard(), "empty startup raises the wizard");
    }

    #[test]
    fn removing_the_last_project_re_raises_the_wizard() {
        let mut app = App::new(vec![project("a")]);
        app.overlay = None;
        app.set_projects(vec![]); // the registry emptied
        assert!(
            app.showing_wizard(),
            "emptying the registry re-raises the wizard"
        );
    }
}
