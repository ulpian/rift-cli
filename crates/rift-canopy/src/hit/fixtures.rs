//! The fixtures the hit-test modules share: ready states past the splash at
//! a chosen size, a detail-bearing project, a wired profile plane, a browsing
//! explorer and a populated Recall Viewer.
//!
//! One module so every surface's tests click the same states the goldens
//! draw, built one way.

use crate::layout::Rect;
use crate::render::frame::Size;
use crate::state::{AppState, ViewModel};
use crate::{EntryKind, Project, ProjectStatus};

pub(super) fn project(name: &str) -> Project {
    Project {
        name: name.into(),
        path: format!("/r/{name}"),
        status: ProjectStatus::Idle,
        kind: EntryKind::Repo,
    }
}

/// A ready dashboard view-model at a full-grid size.
pub(super) fn vm(w: u16, h: u16) -> ViewModel {
    let mut s = AppState::new(
        vec![project("a"), project("b"), project("c")],
        Size::new(w, h),
    );
    s.app.splash.skip();
    s.snapshot()
}

/// A ready view-model in panel-editor mode at a full-grid size.
pub(super) fn editor_vm(w: u16, h: u16) -> ViewModel {
    let mut s = AppState::new(
        vec![project("a"), project("b"), project("c")],
        Size::new(w, h),
    );
    s.app.splash.skip();
    crate::input::enter_panel_editor(&mut s.app);
    s.snapshot()
}

pub(super) fn state_with_detail(w: u16, h: u16) -> AppState {
    let mut s = AppState::new(vec![project("a")], Size::new(w, h));
    s.app.splash.skip();
    let mut detail = crate::DetailView::thin("/r/a".into(), "a".into(), ProjectStatus::Idle);
    detail.project_settings = Box::new(crate::ProjectSettingsView {
        permissions: Vec::new(),
        path: "/r/a".into(),
        project_name: "a".into(),
        initialization: crate::ProjectInitialization::Initialized,
        settings: crate::ProjectSettingId::ALL
            .iter()
            .map(|id| crate::ProjectSettingRow {
                id: *id,
                enabled: false,
                actionable: true,
                detail: None,
            })
            .collect(),
        policy: crate::ProjectPolicySummary::default(),
        integrations: Vec::new(),
    });
    crate::state::update(
        &mut s,
        crate::Event::ProjectData(crate::ProjectData {
            project: "/r/a".into(),
            status: ProjectStatus::Idle,
            detail: Some(detail),
        }),
    );
    s
}

/// A ready state with the profile plane wired (active = `pf0`).
pub(super) fn profiled_state(w: u16, h: u16) -> AppState {
    let mut s = AppState::new(vec![project("a")], Size::new(w, h));
    s.app.splash.skip();
    s.version = "0.1.0".into();
    s.app.set_profiles(
        ["default", "work"]
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

pub(super) fn dir(name: &str) -> crate::explorer::ExplorerEntry {
    crate::explorer::ExplorerEntry {
        name: name.into(),
        is_dir: true,
        tag: None,
    }
}

/// A state browsing a listing of `n` directories at a full-grid size.
pub(super) fn explorer_state(n: usize) -> AppState {
    let mut s = AppState::new(vec![project("a")], Size::new(120, 40));
    s.app.splash.skip();
    s.app.home_dir = Some("/home/dev".into());
    crate::input::enter_explorer(&mut s.app);
    s.app.explorer.apply(crate::explorer::DirListing {
        path: std::path::PathBuf::from("/home/dev"),
        entries: (0..n).map(|i| dir(&format!("d{i:03}"))).collect(),
        omitted: 0,
        error: None,
    });
    s
}

pub(super) fn populated_viewer_state(w: u16, h: u16) -> AppState {
    use crate::recall_viewer::{
        ViewerCheckpointRow, ViewerDetail, ViewerFilterProgress, ViewerLine, ViewerLineKind,
        ViewerTimelineItem, ViewerTimelineSegment, ViewerTimestampProvenance,
    };

    let mut state = state_with_detail(w, h);
    let _ = state.open_recall_viewer(None);
    let rows = (0_u32..80)
        .map(|index| ViewerCheckpointRow {
            checkpoint_id: format!("cp-{index:03}"),
            effective_at_ms: Some(i64::from(index)),
            timestamp_provenance: ViewerTimestampProvenance::Recorded,
            compact_utc: Some("2026-08-28 12:00Z".to_owned()),
            excerpt: Some(format!("checkpoint {index}")),
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
        .collect::<Vec<_>>();
    state.app.viewer.catalogue = std::sync::Arc::new(rows);
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
                })
                .collect(),
        }],
        notice: None,
    });
    state
}

/// The body under the header at `size` — the rect the dispatcher hands every
/// tier, so a test solves the same grid the hit-test does.
pub(super) fn body_of(size: Size) -> Rect {
    Rect {
        x: 0,
        y: 1,
        w: size.w,
        h: size.h.saturating_sub(1),
    }
}
