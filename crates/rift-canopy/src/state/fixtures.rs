//! The state fixtures every test module under `state/` shares: a ready
//! dashboard, a wired profile plane, a populated viewer, and the click and
//! wheel drivers that resolve a target through the pure hit-test.
//!
//! One module so a fixture is built one way, and so the drivers exercise
//! drawn cells rather than targets a test invented.

use crate::col2::DetailView;
use crate::hit::Target;
use crate::input::{CanopyKey, MouseButton, MouseInput, MouseKind};
use crate::render::frame::Size;
use crate::{EntryKind, Project, ProjectStatus};

use super::mouse::apply_scroll;
use super::{
    update, AppState, Effects, Event, FeedPayload, FeedUpdate, Flow, ProjectData, ViewModel,
};

pub(super) fn project(name: &str) -> Project {
    Project {
        name: name.into(),
        path: format!("/r/{name}"),
        status: ProjectStatus::Idle,
        kind: EntryKind::Repo,
    }
}

pub(super) fn ready(projects: Vec<Project>) -> AppState {
    let mut s = AppState::new(projects, Size::new(120, 40));
    s.app.splash.skip(); // past the splash
    s.app.overlay = None; // dismiss the first-run wizard; drive the dashboard
    s.app.overlay_focus = 0;
    s.dirty = false;
    s
}

pub(super) fn open_drawer(state: &mut AppState) {
    if !state.app.projects_open {
        update(state, Event::Key(CanopyKey::Left));
    }
}

pub(super) fn project_settings_state(initialization: crate::ProjectInitialization) -> AppState {
    let mut s = ready(vec![project("a")]);
    let status = match initialization {
        crate::ProjectInitialization::Initialized => ProjectStatus::Idle,
        crate::ProjectInitialization::NotInitialized => ProjectStatus::NotInitialized,
        crate::ProjectInitialization::NotProvisioned => ProjectStatus::NotProvisioned,
        crate::ProjectInitialization::NotRepository => ProjectStatus::NotRepository,
        crate::ProjectInitialization::SetupIncomplete => ProjectStatus::SetupIncomplete,
    };
    s.app.projects[0].status = status;
    let settings = crate::ProjectSettingId::ALL
        .iter()
        .map(|id| crate::ProjectSettingRow {
            id: *id,
            enabled: false,
            actionable: true,
            detail: None,
        })
        .collect();
    let integrations = (0..9)
        .map(|index| crate::NativeAgentIntegrationRow {
            agent: format!("agent-{index}"),
            display_name: format!("Agent {index}"),
            detection: crate::NativeAgentDetection::NotDetected,
            executable_name: None,
            installations: Vec::new(),
            wiring: crate::NativeAgentWiring::Missing,
            hook_wiring: None,
            alpha_supported: true,
            selected: false,
            remedies: Vec::new(),
        })
        .collect();
    let mut detail = DetailView::thin("/r/a".into(), "a".into(), status);
    detail.project_settings = Box::new(crate::ProjectSettingsView {
        permissions: Vec::new(),
        path: "/r/a".into(),
        project_name: "a".into(),
        initialization,
        settings,
        policy: crate::ProjectPolicySummary::default(),
        integrations,
    });
    update(
        &mut s,
        Event::ProjectData(ProjectData {
            project: "/r/a".into(),
            status,
            detail: Some(detail),
        }),
    );
    s.dirty = false;
    s
}

/// A left mouse event of `kind` at `(col,row)`.
pub(super) fn mouse(kind: MouseKind, col: u16, row: u16) -> Event {
    Event::Mouse(MouseInput { kind, col, row })
}

/// A ready state with three live profiles wired (active = `pf0`).
pub(super) fn ready_with_profiles(projects: Vec<crate::Project>) -> AppState {
    let mut s = ready(projects);
    s.app.set_profiles(
        ["default", "work", "lab"]
            .iter()
            .enumerate()
            .map(|(i, name)| crate::profile::ProfileEntry {
                profile_id: format!("pf{i}"),
                name: (*name).to_string(),
                created_at: i64::try_from(i).unwrap(),
            })
            .collect(),
        "pf0".into(),
    );
    s
}

/// Unwrap a `Flow::Continue`'s effects.
pub(super) fn effects_of(flow: Flow) -> Effects {
    match flow {
        Flow::Continue(e) => e,
        Flow::Quit => panic!("expected Continue"),
    }
}

pub(super) fn dir(name: &str) -> crate::explorer::ExplorerEntry {
    crate::explorer::ExplorerEntry {
        name: name.into(),
        is_dir: true,
        tag: None,
    }
}

pub(super) fn file(name: &str) -> crate::explorer::ExplorerEntry {
    crate::explorer::ExplorerEntry {
        name: name.into(),
        is_dir: false,
        tag: None,
    }
}

/// The explorer's fixture listing of `/home/dev` (never a filesystem read).
pub(super) fn home_listing() -> crate::explorer::DirListing {
    crate::explorer::DirListing {
        path: std::path::PathBuf::from("/home/dev"),
        entries: vec![dir(".config"), dir("alpha"), dir("beta"), file("notes.md")],
        omitted: 0,
        error: None,
    }
}

/// A dashboard browsing `/home/dev` over [`home_listing`].
pub(super) fn browsing() -> AppState {
    let mut s = ready(vec![project("a")]);
    s.app.home_dir = Some("/home/dev".into());
    let e = effects_of(update(&mut s, Event::Key(CanopyKey::Char('a'))));
    assert_eq!(e.list_dir, Some(std::path::PathBuf::from("/home/dev")));
    s.app.explorer.apply(home_listing());
    s
}

/// A left click on the drawn cell of `target` (resolved via the pure
/// hit-test so the test exercises pixels == targets).
pub(super) fn click_on(target: Target, s: &mut AppState) -> crate::input::MouseInput {
    let vm = s.snapshot();
    let (w, h) = (vm.size.w, vm.size.h);
    for row in 0..h {
        for col in 0..w {
            if crate::hit::hit_test(&vm, col, row).as_ref() == Some(&target) {
                return crate::input::MouseInput {
                    kind: crate::input::MouseKind::Down(crate::input::MouseButton::Left),
                    col,
                    row,
                };
            }
        }
    }
    panic!("target {target:?} has no drawn cell");
}

/// Build a Tokens feed for `/r/a` with the given series/total.
pub(super) fn tokens_feed(rate: Vec<f64>, total: f64) -> Event {
    use crate::col3::{TokensData, TokensView};
    Event::Feed(FeedUpdate {
        project: "/r/a".into(),
        payload: FeedPayload::Tokens {
            tokens: TokensView::Data(TokensData {
                rate,
                total,
                by_worktree: crate::col3::WorktreeAxis::default(),
            }),
            breakdown: crate::col3::BreakdownView::NoActivity,
        },
    })
}

pub(super) fn tokens_of(v: &ViewModel) -> (Vec<f64>, f64) {
    match &v.col3.tokens {
        crate::col3::TokensView::Data(d) => (d.rate.clone(), d.total),
        crate::col3::TokensView::NoActivity => (vec![], -1.0),
    }
}

/// A Recall feed whose **call chart** carries `rate`.
///
/// The chart is the card's one series over time and therefore the only
/// part of it that animates: the savings figures are headlines over one
/// window and snap whole.
pub(super) fn recall_feed(rate: Vec<f64>) -> Event {
    use crate::col3::{CallsData, ChannelCallSeries, RecallView, SavingsData};
    Event::Feed(FeedUpdate {
        project: "/r/a".into(),
        payload: FeedPayload::Recall {
            savings: RecallView::Data(SavingsData {
                ..SavingsData::default()
            }),
            calls: RecallView::Data(CallsData {
                total: 3,
                window_end_ms: 1_800_000,
                series: vec![ChannelCallSeries {
                    channel: "hook_inject".into(),
                    last_call_at: 1_799_000,
                    rate,
                }],
            }),
        },
    })
}

pub(super) fn click(col: u16, row: u16) -> Event {
    Event::Mouse(MouseInput {
        kind: MouseKind::Down(MouseButton::Left),
        col,
        row,
    })
}

pub(super) fn exercise_wheel_targets(state: &mut AppState) -> Vec<Target> {
    let view = state.snapshot();
    let mut targets = Vec::new();
    for row in 0..view.size.h {
        for col in 0..view.size.w {
            let Some(target) = crate::hit::hit_test(&view, col, row) else {
                continue;
            };
            if targets.contains(&target) {
                continue;
            }
            targets.push(target);
            for down in [false, true] {
                let effects = effects_of(apply_scroll(state, &view, col, row, down));
                assert!(
                    effects.persist.is_empty(),
                    "wheel routing over {:?} must stay read-only",
                    targets.last()
                );
            }
        }
    }
    targets
}

pub(super) fn populated_viewer_for_wheel(w: u16, h: u16) -> AppState {
    use crate::recall_viewer::{
        ViewerCheckpointRow, ViewerDetail, ViewerFilterProgress, ViewerLine, ViewerLineKind,
        ViewerTimelineItem, ViewerTimelineSegment, ViewerTimestampProvenance,
    };

    let mut state = ready(vec![project("a"), project("b")]);
    state.size = Size::new(w, h);
    let _ = state.open_recall_viewer(None);
    state.app.viewer.catalogue = std::sync::Arc::new(
        (0_u32..80)
            .map(|index| ViewerCheckpointRow {
                checkpoint_id: format!("cp-{index:03}"),
                effective_at_ms: Some(i64::from(index)),
                timestamp_provenance: ViewerTimestampProvenance::Recorded,
                compact_utc: Some("2026-08-28 12:00Z".to_owned()),
                excerpt: None,
                excerpt_loading: false,
                anchor_commit: Some(format!("oid-{index:03}")),
                origins: Vec::new(),
                chain_position: Some(u64::from(index)),
                local_branches: vec!["main".to_owned()],
                strategy: Some("native".to_owned()),
                projection: "projected".to_owned(),
                issues: Vec::new(),
                metadata_search: format!("cp-{index:03}"),
                worktree: None,
            })
            .collect(),
    );
    state.app.viewer.visible = (0..80).collect();
    state.app.viewer.new_count = 2;
    state.app.viewer.filter_progress = Some(ViewerFilterProgress {
        scanned: 5,
        total: 80,
        complete: false,
    });
    state.app.viewer.detail = std::sync::Arc::new(ViewerDetail {
        checkpoint_id: "cp-000".to_owned(),
        segments: vec![ViewerTimelineSegment {
            title: "captured".to_owned(),
            boundary: Some("ordered by recorded time".to_owned()),
            ordered_by_time: true,
            items: (0..80)
                .map(|index| ViewerTimelineItem {
                    id: format!("item-{index}"),
                    title: format!("event {index}"),
                    timestamp: Some("12:00:00Z".to_owned()),
                    summary: vec![ViewerLine::plain(
                        ViewerLineKind::Heading,
                        format!("summary {index}"),
                    )],
                    body: vec![ViewerLine::plain(ViewerLineKind::Text, "body")],
                    expanded_body: None,
                    collapsed: false,
                    full_session_only: false,
                    tone: crate::recall_viewer::ViewerItemTone::Neutral,
                })
                .collect(),
        }],
        notice: None,
    });
    state
}
