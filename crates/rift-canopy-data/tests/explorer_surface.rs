//! The destination explorer end to end across the lister seam (ADR-0052).
//!
//! Every other explorer test lives on one side of the seam: `rift-canopy`'s
//! reducer/draw tests inject hand-built listings, and `dirlist`'s tempdir tests
//! assert on the walker's return value. This one crosses it — a **real**
//! directory walked by the **real** `list_dir`, applied to `ExplorerState`, and
//! drawn through the same `view::draw` the binary flushes — so the two halves
//! are proven to compose, on both backends.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]

use rift_canopy::render::frame::{Frame, Size};
use rift_canopy::render::{Capabilities, RenderBackend, RenderError};
use rift_canopy::state::AppState;
use rift_canopy::{EntryKind, Project, ProjectStatus};

/// A capability-bearing headless backend (the `style_goldens::TierBackend`
/// shape) — `AsciiBackend` always reports the empty set, so the Unicode tier
/// needs its own double.
struct TierBackend {
    caps: Capabilities,
}

impl RenderBackend for TierBackend {
    fn name(&self) -> &'static str {
        "tier"
    }
    fn capabilities(&self) -> Capabilities {
        self.caps
    }
    fn present(&mut self, _frame: &Frame) -> Result<(), RenderError> {
        Ok(())
    }
}

fn render(state: &mut AppState, caps: Capabilities) -> String {
    let vm = state.snapshot();
    let mut frame = Frame::new(vm.size);
    let mut backend = TierBackend { caps };
    rift_canopy::view::draw(&vm, &mut backend, &mut frame);
    let mut out = String::new();
    for y in 0..frame.height() {
        let mut line = String::new();
        for x in 0..frame.width() {
            line.push(frame.get(x, y).map_or(' ', |c| c.ch));
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[test]
fn the_real_walk_lands_on_the_drawn_surface_on_both_backends() {
    // A directory with one of everything the surface distinguishes.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::create_dir(root.join("alpha")).unwrap();
    std::fs::create_dir(root.join("Beta")).unwrap();
    std::fs::create_dir(root.join(".hidden")).unwrap();
    std::fs::write(root.join("notes.md"), b"x").unwrap();

    let mut state = AppState::new(
        vec![Project {
            name: "a".into(),
            path: "/r/a".into(),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }],
        Size::new(100, 30),
    );
    state.app.splash.skip();
    state.app.overlay = None;
    state.app.home_dir = Some(root.to_string_lossy().into_owned());

    // Entry requests the start directory; the loop's job — performed here by
    // the real walker — is to answer it.
    let rift_canopy::Action::ExplorerList(requested) =
        rift_canopy::input::enter_explorer(&mut state.app)
    else {
        panic!("entering the explorer must request a listing");
    };
    assert_eq!(requested, root);
    state
        .app
        .explorer
        .apply(rift_canopy_data::list_dir(&requested));

    for (label, caps) in [
        ("ascii", Capabilities::empty()),
        ("unicode", Capabilities::UNICODE),
    ] {
        let text = render(&mut state, caps);
        println!("\n===== destination explorer ({label}) =====\n{text}");
        assert!(text.contains("add project"), "{label}: framed as the act");
        // Directories the walker found are enterable rows, in ITS order.
        let alpha = text.find("alpha/").expect("alpha renders as a directory");
        let beta = text.find("Beta/").expect("Beta renders as a directory");
        let notes = text.find("notes.md").expect("the file is listed");
        assert!(alpha < beta, "{label}: the one comparator's order survives");
        assert!(beta < notes, "{label}: directories precede files");
        assert!(
            !text.contains(".hidden"),
            "{label}: dot-entries are filtered at read time"
        );
        // Both acts and both toggles are on the surface (ADR-0046).
        assert!(text.contains("[ select destination ]"), "{label}");
        assert!(text.contains("[ input as path ]"), "{label}");
        assert!(text.contains("[.dotfiles]"), "{label}");
        assert!(text.contains("[x]"), "{label}: the shared close X");
    }

    // The dot-toggle reveals what the walker really returned.
    rift_canopy::input::on_key(&mut state.app, rift_canopy::CanopyKey::Char('.'));
    let text = render(&mut state, Capabilities::UNICODE);
    println!("\n===== dot-entries revealed =====\n{text}");
    assert!(
        text.contains(".hidden/"),
        "the toggle reveals, never re-reads"
    );

    // Descending crosses the seam again, and selecting registers what the
    // title names — the typed flow's own action.
    let rift_canopy::Action::ExplorerList(child) =
        rift_canopy::input::on_key(&mut state.app, rift_canopy::CanopyKey::Enter)
    else {
        panic!("Enter on a directory row must request its listing");
    };
    assert_eq!(child, root.join(".hidden"));
    state.app.explorer.apply(rift_canopy_data::list_dir(&child));
    let text = render(&mut state, Capabilities::UNICODE);
    println!("\n===== descended into .hidden =====\n{text}");
    assert!(
        text.contains("(empty directory)"),
        "an honest empty listing"
    );

    assert_eq!(
        rift_canopy::input::on_key(&mut state.app, rift_canopy::CanopyKey::Char('s')),
        rift_canopy::Action::AddProject(child.to_string_lossy().into_owned()),
        "select registers the folder that is OPEN, through the typed flow's action"
    );
}
