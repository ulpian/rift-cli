//! Column 1 — the projects rail: the count badge in the title, per-row kind
//! icon + label + trailing status dot, the selection as a filled pill, and
//! the bottom action bar whose chips the hit-test shares. The Recall Viewer
//! draws the same rail without its write-capable action row.

use crate::col2::sanitize_for_terminal;
use crate::layout::Rect;
use crate::render::frame::{Frame, Style};
use crate::state::ViewModel;
use crate::{EntryKind, Panel};

use super::grid::draw_overflow_bar;
use super::text::{clip_line, elide};

/// Which surface the rail is drawn for. The dashboard's rail follows the
/// projects focus and carries the add/remove/reorder action row; the Recall
/// Viewer's follows the viewer's pane and is read-only — the same rows, so a
/// project reads the same on both surfaces.
#[derive(Clone, Copy)]
enum RailRole {
    Dashboard,
    Viewer,
}

impl RailRole {
    /// Whether the rail's border takes the focus accent on this surface.
    fn focused(self, vm: &ViewModel) -> bool {
        match self {
            Self::Dashboard => vm.focus == Panel::Projects,
            Self::Viewer => vm.viewer.pane == crate::recall_viewer::ViewerPane::Projects,
        }
    }

    /// The list area and the action chips: the viewer spares no row for chips.
    fn layout(self, vm: &ViewModel, inner: Rect) -> (Rect, Vec<(Rect, crate::hit::Target)>) {
        match self {
            Self::Dashboard => rail_action_layout(inner, !vm.projects.is_empty()),
            Self::Viewer => (inner, Vec::new()),
        }
    }

    /// The one-line placeholder when no project is listed.
    fn empty_note(self) -> &'static str {
        match self {
            Self::Dashboard => "No projects yet — press 'a' or [+]",
            Self::Viewer => "No readable project selected",
        }
    }
}

/// The rail's bottom action bar: the list area (rows above the bar) and
/// the clickable `[glyph]` buttons — `+` add always, and `↑`/`↓`/`✕` (reorder
/// up/down, remove selected) when a project is selected. One shared source for
/// `draw_projects` and the hit-test, so the chips and their click targets are
/// the same cells. Returns `(list_area, buttons)`; no buttons when the rail is
/// too short to spare a row.
pub(crate) fn rail_action_layout(
    inner: Rect,
    has_selection: bool,
) -> (Rect, Vec<(Rect, crate::hit::Target)>) {
    if inner.h < 2 {
        return (inner, Vec::new());
    }
    let bar_y = inner.y + inner.h - 1;
    let list = Rect {
        h: inner.h - 1,
        ..inner
    };
    let mut targets = vec![crate::hit::Target::RailAdd];
    if has_selection {
        targets.push(crate::hit::Target::RailReorderUp);
        targets.push(crate::hit::Target::RailReorderDown);
        targets.push(crate::hit::Target::RailRemove);
    }
    // Chips abut (`[+][↑][↓][✕]`) so all four fit even in a ~14-cell rail; a
    // gap would clip the rightmost (remove) at narrow widths.
    let chip = 3u16;
    let right = inner.x + inner.w;
    let mut x = inner.x;
    let mut buttons = Vec::with_capacity(targets.len());
    for t in targets {
        if x + chip > right {
            break;
        }
        buttons.push((
            Rect {
                x,
                y: bar_y,
                w: chip,
                h: 1,
            },
            t,
        ));
        x += chip;
    }
    (list, buttons)
}

/// Draw the same Projects rail without its write-capable add/remove/reorder
/// action row. The Recall Viewer calls this read-only variant.
pub(crate) fn draw_projects_read_only(
    vm: &ViewModel,
    frame: &mut Frame,
    rect: Rect,
    unicode: bool,
) {
    draw_rail(vm, frame, rect, unicode, RailRole::Viewer);
}

/// The visible `[start, end)` slice of a `len`-item list of `height` rows that
/// keeps `selected` in view (scroll-to-selection). Deterministic in its inputs.
/// Shared with [`crate::hit`] so a click resolves to the same project row the
/// renderer drew.
pub(crate) fn visible_window(selected: usize, len: usize, height: usize) -> (usize, usize) {
    if height == 0 || len == 0 {
        return (0, 0);
    }
    let h = height.min(len);
    // Pin so `selected` stays within the window; clamp the top to the last page.
    let start = selected.saturating_sub(h - 1).min(len - h);
    (start, start + h)
}

/// Draw the dashboard's projects rail, with its write-capable action row.
pub(super) fn draw_projects(vm: &ViewModel, frame: &mut Frame, rect: Rect, unicode: bool) {
    draw_rail(vm, frame, rect, unicode, RailRole::Dashboard);
}

/// Draw the rail for `role`: the bordered panel with its count badge, then
/// the placeholder or the windowed rows, the action chips, and the overflow
/// scrollbar that tracks the selection window when more projects exist than
/// the list has rows — the visual half of the silent paging `visible_window`
/// already does.
fn draw_rail(vm: &ViewModel, frame: &mut Frame, rect: Rect, unicode: bool, role: RailRole) {
    use crate::theme::Section;
    let inner = crate::chrome::panel(
        frame,
        rect,
        "projects",
        vm.theme.border(Section::Projects, role.focused(vm)),
        vm.theme.title(Section::Projects),
    );
    // The registered-project count as a badge on the title row, after the title
    // (the count is a fact you see, not a scroll you take).
    if rect.w >= 16 && rect.h >= 2 {
        let bx = rect.x + 12; // just past ` projects ` (title starts at x+3)
        crate::chrome::badge(
            frame,
            bx,
            rect.y,
            &vm.projects.len().to_string(),
            vm.theme.badge(Section::Projects.accent()),
        );
    }
    let (list, buttons) = role.layout(vm, inner);
    if vm.projects.is_empty() {
        clip_line(frame, list, 0, role.empty_note(), vm.theme.body());
        draw_rail_actions(vm, frame, &buttons, unicode);
        return;
    }
    let (start, end) = visible_window(vm.selected, vm.projects.len(), usize::from(list.h));
    for (row, p) in vm.projects.iter().enumerate().take(end).skip(start) {
        let Ok(dy) = u16::try_from(row.saturating_sub(start)) else {
            break;
        };
        draw_rail_row(
            vm,
            frame,
            unicode,
            inner,
            inner.y + dy,
            p,
            row == vm.selected,
        );
    }
    draw_rail_actions(vm, frame, &buttons, unicode);
    let bar_area = Rect {
        x: rect.x,
        y: list.y,
        w: rect.w,
        h: list.h,
    };
    draw_overflow_bar(frame, bar_area, start, vm.projects.len(), vm.theme, unicode);
}

/// One rail row — `> ▣ label … ●`: cursor, kind icon, label, trailing status
/// dot. The selected row is the selection pill (BOLD keeps it visible on the
/// colourless floors, beside the `>` marker); a linked worktree renders as an
/// indented child of its repository; the label is untrusted (a directory
/// name), so it is sanitized before it is elided.
fn draw_rail_row(
    vm: &ViewModel,
    frame: &mut Frame,
    unicode: bool,
    inner: Rect,
    y: u16,
    p: &crate::state::ProjectRow,
    selected: bool,
) {
    use crate::theme::Icon;
    let (row_style, marker) = if selected {
        let pill = selection_pill(vm);
        let row = Rect {
            x: inner.x,
            y,
            w: inner.w,
            h: 1,
        };
        crate::chrome::fill(frame, row, pill);
        (pill, '>')
    } else {
        (vm.theme.body(), ' ')
    };
    let indent = match p.kind {
        EntryKind::Repo => "",
        EntryKind::Worktree => "  ",
    };
    let label_w = usize::from(inner.w).saturating_sub(6);
    let label = elide(
        &sanitize_for_terminal(&p.name),
        label_w.saturating_sub(indent.len()),
    );
    frame.set(inner.x, y, marker, row_style);
    let kind_style = if selected { row_style } else { vm.theme.dim() };
    let mut x = frame.put_str(inner.x + 1, y, indent, row_style);
    frame.set(x, y, Icon::for_kind(p.kind).glyph(unicode), kind_style);
    x = x.saturating_add(1);
    frame.put_str(x + 1, y, &label, row_style);
    // The trailing status dot, pinned to the right edge in status colour.
    if inner.w >= 2 {
        let dot_x = inner.x + inner.w - 1;
        let mut dot = vm.theme.status_dot(p.status);
        if selected {
            dot = dot.with_bg(crate::theme::SELECT_BG);
        }
        frame.set(dot_x, y, Icon::for_status(p.status).glyph(unicode), dot);
    }
}

/// The selection pill's style: on a cursor move it flashes toward the accent
/// (the selection pulse), then decays to the resting selection wash.
fn selection_pill(vm: &ViewModel) -> Style {
    if vm.select_pulse <= 0.0 {
        return vm.theme.selection();
    }
    let bg = crate::theme::lerp_color(
        crate::theme::ACCENT,
        crate::theme::SELECT_BG,
        1.0 - vm.select_pulse.clamp(0.0, 1.0),
    );
    vm.theme.selection().with_bg(bg)
}

/// The glyph inside a rail action chip; the ASCII floor substitutes.
fn rail_action_glyph(target: &crate::hit::Target, unicode: bool) -> char {
    use crate::hit::Target;
    match target {
        Target::RailAdd => '+',
        Target::RailReorderUp => {
            if unicode {
                '↑'
            } else {
                '^'
            }
        }
        Target::RailReorderDown => {
            if unicode {
                '↓'
            } else {
                'v'
            }
        }
        Target::RailRemove => {
            if unicode {
                '✕'
            } else {
                'x'
            }
        }
        _ => '?',
    }
}

/// Draw the rail action chips, each `[glyph]` — muted at rest, flashing toward
/// the accent when pressed (the press-flash rule).
fn draw_rail_actions(
    vm: &ViewModel,
    frame: &mut Frame,
    buttons: &[(Rect, crate::hit::Target)],
    unicode: bool,
) {
    for (r, target) in buttons {
        let g = rail_action_glyph(target, unicode);
        let style = match crate::chrome::press_flash_bg(vm.theme, vm.button_flash(target)) {
            Some(bg) => vm.theme.dim().with_bg(bg),
            None => vm.theme.dim(),
        };
        frame.put_str(r.x, r.y, &format!("[{g}]"), style);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;
    use crate::state::AppState;
    use crate::view::draw;
    use crate::view::fixtures::{render_to_text, sized};
    use crate::{Project, ProjectStatus};

    #[test]
    fn a_long_rail_shows_a_scrollbar_a_short_one_does_not() {
        use crate::render::frame::Size;
        // More projects than rows → the rail carries an overflow scrollbar (its
        // track/thumb glyph on the right edge); a fitting rail carries none.
        let many: Vec<Project> = (0..30)
            .map(|i| Project {
                name: format!("proj-{i}"),
                path: format!("/r/{i}"),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            })
            .collect();
        let mut s = AppState::new(many, Size::new(90, 20));
        s.app.splash.skip();
        let text = render_to_text(&s.snapshot());
        let has_bar = |t: &str| t.contains('█') || t.contains('│') || t.contains('#');
        assert!(
            has_bar(&text),
            "the overflowing rail shows a scrollbar:\n{text}"
        );

        let mut few = AppState::new(
            vec![Project {
                name: "solo".into(),
                path: "/r/solo".into(),
                status: ProjectStatus::Idle,
                kind: EntryKind::Repo,
            }],
            Size::new(90, 20),
        );
        few.app.splash.skip();
        // The projects rail itself carries no thumb glyph when a single row fits
        // (the rail is column 1; a `█` there would be the bar). Scan column-1 rows.
        let vm = few.snapshot();
        let mut f = Frame::new(vm.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&vm, &mut b, &mut f);
        let col1 = crate::layout::solve(Rect {
            x: 0,
            y: 1,
            w: 90,
            h: 19,
        })
        .col1;
        let edge = col1.x + col1.w - 1;
        for y in col1.y..(col1.y + col1.h) {
            assert_ne!(
                f.get(edge, y).map(|c| c.ch),
                Some('#'),
                "a fitting rail draws no scrollbar thumb"
            );
        }
    }

    #[test]
    fn rail_shows_count_badge_status_dots_and_selection_pill() {
        use crate::theme::{status_color, SELECT_BG};
        let mut s = AppState::new(
            vec![
                Project {
                    name: "alpha".into(),
                    path: "/r/alpha".into(),
                    status: ProjectStatus::Running {
                        since_ms: 0,
                        active_sessions: 1,
                    },
                    kind: EntryKind::Repo,
                },
                Project {
                    name: "beta".into(),
                    path: "/r/beta".into(),
                    status: ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
            ],
            Size::new(120, 40),
        );
        s.app.splash.skip();
        s.app.projects_open = true;
        let mut f = Frame::new(s.size);
        let mut b = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        draw(&s.snapshot(), &mut b, &mut f);

        // The count badge shows "2" on the title row (col1 spans x=0..24).
        let drawer = crate::dashboard::drawer_rect(s.size);
        let title: String = (drawer.x..drawer.x + drawer.w)
            .map(|x| f.get(x, drawer.y).unwrap().ch)
            .collect();
        let row = drawer.y + 1;
        assert!(title.contains('2'), "the project count badge: {title}");

        // Row 0 of the rail (inner y=2) is the selected `alpha` — its trailing
        // status dot is running-green, and the pill wash tints the row.
        let dot_x = drawer.x + drawer.w - 2; // col1 inner right edge (width 24 → inner 1..23)
        assert_eq!(
            f.get(dot_x, row).unwrap().style.fg,
            status_color(ProjectStatus::Running {
                since_ms: 0,
                active_sessions: 1
            }),
            "the running dot is green"
        );
        assert_eq!(
            f.get(2, row).unwrap().style.bg,
            SELECT_BG,
            "the selected row carries the pill wash"
        );
        // Row 1 (`beta`, unselected) has no pill wash — just the surface the
        // dashboard paints for itself (the background slot's canvas).
        assert_eq!(
            f.get(2, row + 1).unwrap().style.bg,
            crate::theme::SURFACE_BG
        );
        // …and its idle dot is the idle slate.
        assert_eq!(
            f.get(dot_x, row + 1).unwrap().style.fg,
            status_color(ProjectStatus::Idle)
        );
    }

    #[test]
    fn long_names_truncate_with_ellipsis() {
        let mut s = sized(120, 40);
        s.app.projects_open = true;
        s.app.projects = vec![Project {
            name: "a-really-long-project-name-that-cannot-possibly-fit".into(),
            path: "/r/x".into(),
            status: ProjectStatus::Idle,
            kind: EntryKind::Repo,
        }];
        let text = render_to_text(&s.snapshot());
        assert!(text.contains('…'), "an over-long label is ellipsized");
    }
}
