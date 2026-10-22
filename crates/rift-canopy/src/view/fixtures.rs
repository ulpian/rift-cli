//! The fixtures the view test modules share: populated states past the
//! splash, a frame rendered through the root `draw` and read back as text,
//! and the capability-bearing backend a glyph test needs.
//!
//! One module so a state is built one way and a test in any surface's module
//! renders the same frame the goldens do.

use crate::render::frame::{Frame, Size};
use crate::state::{update, AppState, Event, ProjectData, ViewModel};
use crate::{CaptureSummary, DetailView, EntryKind, ModelUsage, Project, ProjectStatus};

use super::{draw, modal_rect};

pub(super) fn vm() -> ViewModel {
    let mut s = AppState::new(
        vec![
            Project {
                name: "rift-cli".into(),
                path: "/r/rift-cli".into(),
                status: ProjectStatus::Running {
                    since_ms: 0,
                    active_sessions: 1,
                },
                kind: EntryKind::Repo,
            },
            Project {
                name: "rift-site".into(),
                path: "/r/rift-site".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            },
        ],
        Size::new(120, 40),
    );
    s.app.splash.skip();
    s.snapshot()
}

pub(super) fn render_to_text(vm: &ViewModel) -> String {
    let mut f = Frame::new(vm.size);
    let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
    draw(vm, &mut b, &mut f);
    let mut out = String::new();
    for y in 0..f.height() {
        for x in 0..f.width() {
            out.push(f.get(x, y).map_or(' ', |c| c.ch));
        }
        out.push('\n');
    }
    out
}

/// A view-model in a picker/menu-capable state (one live profile).
pub(super) fn profiled_state() -> AppState {
    let mut s = AppState::new(
        vec![Project {
            name: "rift-cli".into(),
            path: "/r/rift-cli".into(),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }],
        Size::new(120, 40),
    );
    s.app.splash.skip();
    s.app.set_profiles(
        vec![crate::profile::ProfileEntry {
            profile_id: "pf0".into(),
            name: "default".into(),
            created_at: 0,
        }],
        "pf0".into(),
    );
    s
}

pub(super) fn dir(name: &str) -> crate::explorer::ExplorerEntry {
    crate::explorer::ExplorerEntry {
        name: name.to_owned(),
        is_dir: true,
        tag: None,
    }
}

pub(super) fn file(name: &str) -> crate::explorer::ExplorerEntry {
    crate::explorer::ExplorerEntry {
        name: name.to_owned(),
        is_dir: false,
        tag: None,
    }
}

/// A capability-bearing backend, so a glyph test can exercise BOTH sides of
/// the one Unicode switch (`AsciiBackend` always reports the empty set).
pub(super) struct CapsBackend {
    pub(super) caps: crate::render::Capabilities,
}

impl crate::render::RenderBackend for CapsBackend {
    fn name(&self) -> &'static str {
        "caps"
    }
    fn capabilities(&self) -> crate::render::Capabilities {
        self.caps
    }
    fn present(&mut self, _frame: &Frame) -> Result<(), crate::render::RenderError> {
        Ok(())
    }
}

/// The drawn text of one row of the explorer's modal — so a status-line
/// assertion reads the modal itself, not the dashboard's hint bar beneath.
pub(super) fn explorer_row_text(s: &mut AppState, dy_from_inner_bottom: u16) -> String {
    let vm = s.snapshot();
    let mut f = Frame::new(vm.size);
    let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
    draw(&vm, &mut b, &mut f);
    let modal = modal_rect(vm.size);
    let y = modal.y + modal.h - 1 - dy_from_inner_bottom;
    (modal.x..modal.x + modal.w)
        .map(|x| f.get(x, y).map_or(' ', |c| c.ch))
        .collect()
}

/// A state browsing `/home/dev/codebase` with two dirs and a file listed.
pub(super) fn explorer_state() -> AppState {
    let mut s = profiled_state();
    s.app.home_dir = Some("/home/dev/codebase".into());
    crate::input::enter_explorer(&mut s.app);
    s.app.explorer.apply(crate::explorer::DirListing {
        path: std::path::PathBuf::from("/home/dev/codebase"),
        entries: vec![dir("alpha"), dir("beta"), file("notes.md")],
        omitted: 0,
        error: None,
    });
    s
}

/// A populated state at the given size, past the splash.
pub(super) fn sized(w: u16, h: u16) -> AppState {
    let mut s = AppState::new(
        vec![
            Project {
                name: "alpha".into(),
                path: "/r/alpha".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            },
            Project {
                name: "beta".into(),
                path: "/r/beta".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            },
        ],
        Size::new(w, h),
    );
    s.app.splash.skip();
    s
}

/// A single-project state whose selected project's rich detail carries `model`.
pub(super) fn state_with_detail(status: ProjectStatus, model: &str) -> AppState {
    let mut s = AppState::new(
        vec![Project {
            name: "p".into(),
            path: "/r/p".into(),
            status,
            kind: EntryKind::Repo,
        }],
        Size::new(120, 40),
    );
    s.app.splash.skip();
    update(
        &mut s,
        Event::ProjectData(ProjectData {
            project: "/r/p".into(),
            status,
            detail: Some(DetailView {
                git_summary: None,
                running_members: Vec::new(),
                path: "/r/p".into(),
                label: "p".into(),
                kind: EntryKind::Repo,
                status,
                models: vec![ModelUsage {
                    model: model.into(),
                    calls: 3,
                    source: crate::col2::ModelProvenance::Runtime,
                }],
                capture: CaptureSummary::default(),
                project_settings: Box::new(crate::ProjectSettingsView::thin(
                    "/r/p".into(),
                    "p".into(),
                )),
            }),
        }),
    );
    s
}

/// Render one immutable snapshot with the capability-bearing backend and
/// return a tiny observation so the compiler cannot discard the draw.
pub(super) fn render_observation(vm: &ViewModel, caps: crate::render::Capabilities) -> usize {
    let mut frame = Frame::new(vm.size);
    let mut backend = CapsBackend { caps };
    draw(vm, &mut backend, &mut frame);
    (0..frame.height())
        .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
        .filter(|(x, y)| frame.get(*x, *y).is_some_and(|cell| cell.ch != ' '))
        .count()
}

pub(super) fn project(name: &str) -> Project {
    Project {
        name: name.to_owned(),
        path: format!("/r/{name}"),
        status: ProjectStatus::Idle,
        kind: EntryKind::Repo,
    }
}
