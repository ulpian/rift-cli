//! Full-screen repository/worktree picker. Geometry is shared by drawing and clicks.

use crate::hit::{contains, Target};
use crate::layout::Rect;
use crate::render::{Color, Frame, Mods, Style};
use crate::state::ViewModel;
use crate::view::text::clip_at;
use crate::{EntryKind, ProjectStatus};

pub(crate) const FILTERS: [Option<EntryKind>; 3] =
    [None, Some(EntryKind::Repo), Some(EntryKind::Worktree)];

pub(crate) fn accepts(filter: Option<EntryKind>, kind: EntryKind) -> bool {
    filter.is_none_or(|wanted| wanted == kind)
}

fn filter_tabs(vm: &ViewModel) -> Vec<(Option<EntryKind>, Rect, String)> {
    let mut x = 1;
    FILTERS
        .into_iter()
        .zip(["All", "Repositories", "Worktrees"])
        .map(|(filter, label)| {
            let count = vm
                .projects
                .iter()
                .filter(|p| accepts(filter, p.kind))
                .count();
            let text = if filter == vm.project_filter {
                format!("[{label} {count}]")
            } else {
                format!(" {label} {count} ")
            };
            let width = u16::try_from(text.chars().count())
                .unwrap_or(0)
                .min(vm.size.w.saturating_sub(x + 1));
            let rect = Rect {
                x,
                y: 1,
                w: width,
                h: 1,
            };
            x += width + 1;
            (filter, rect, text)
        })
        .collect()
}

enum Row {
    Heading(String),
    Project(usize),
    Gap,
}

fn rows(vm: &ViewModel) -> Vec<Row> {
    let mut rows = Vec::new();
    let mut heading = None;
    for (i, project) in vm.projects.iter().enumerate() {
        if project.kind == EntryKind::Repo {
            heading = Some(
                vm.home_dir
                    .as_deref()
                    .and_then(|home| std::path::Path::new(&project.path).strip_prefix(home).ok())
                    .map_or_else(
                        || project.path.clone(),
                        |rest| format!("~/{}", rest.display()),
                    ),
            );
        }
        if !accepts(vm.project_filter, project.kind) {
            continue;
        }
        if let Some(path) = heading.take() {
            if !rows.is_empty() {
                rows.push(Row::Gap);
            }
            rows.push(Row::Heading(path));
        }
        rows.push(Row::Project(i));
    }
    rows
}

fn list_area(vm: &ViewModel) -> Rect {
    Rect {
        x: 1,
        y: 4,
        w: if vm.size.w >= 90 {
            vm.size.w * 2 / 3 - 2
        } else {
            vm.size.w.saturating_sub(2)
        },
        h: vm.size.h.saturating_sub(6),
    }
}

fn visible_rows(vm: &ViewModel) -> Vec<(u16, Row)> {
    let area = list_area(vm);
    let rows = rows(vm);
    let selected = rows
        .iter()
        .position(|row| matches!(row, Row::Project(i) if *i == vm.project_cursor))
        .unwrap_or(0);
    let start = selected.saturating_sub(usize::from(area.h).saturating_sub(1));
    rows.into_iter()
        .skip(start)
        .take(usize::from(area.h))
        .enumerate()
        .map(|(offset, row)| (area.y + u16::try_from(offset).unwrap_or(0), row))
        .collect()
}

pub(crate) fn draw(vm: &ViewModel, frame: &mut Frame, unicode: bool) {
    let strong = vm.theme.body().with_mods(Mods::BOLD);
    clip_at(frame, 1, 0, vm.size.w.saturating_sub(2), "Projects", strong);
    for (filter, rect, text) in filter_tabs(vm) {
        let style = if filter == vm.project_filter {
            vm.theme.key_hint()
        } else {
            vm.theme.dim()
        };
        clip_at(frame, rect.x, rect.y, rect.w, &text, style);
    }
    let rule = if unicode { '─' } else { '-' };
    for x in 1..vm.size.w.saturating_sub(1) {
        frame.set(x, 2, rule, vm.theme.dim());
    }
    let area = list_area(vm);
    clip_at(
        frame,
        area.x + 3,
        3,
        area.w.saturating_sub(3),
        "Repositories / worktrees",
        vm.theme.dim(),
    );
    if area.w > 38 {
        clip_at(frame, area.x + area.w - 18, 3, 18, "Status", vm.theme.dim());
    }
    for (y, row) in visible_rows(vm) {
        match row {
            Row::Heading(path) => clip_at(
                frame,
                area.x,
                y,
                area.w,
                &crate::col2::sanitize_for_terminal(&path),
                vm.theme.dim(),
            ),
            Row::Project(i) => draw_project(vm, frame, area, y, i, unicode),
            Row::Gap => {}
        }
    }
    if !vm
        .projects
        .iter()
        .any(|p| accepts(vm.project_filter, p.kind))
    {
        clip_at(
            frame,
            area.x,
            area.y,
            area.w,
            "No matching projects. Press a to add one.",
            vm.theme.body(),
        );
    }
    if vm.size.w >= 90 {
        draw_preview(vm, frame, unicode);
    }
    for (x, width, label, _) in footer(vm) {
        clip_at(
            frame,
            x,
            vm.size.h.saturating_sub(1),
            width,
            label,
            vm.theme.dim(),
        );
    }
}

fn draw_project(vm: &ViewModel, frame: &mut Frame, area: Rect, y: u16, i: usize, unicode: bool) {
    let Some(project) = vm.projects.get(i) else {
        return;
    };
    let selected = i == vm.project_cursor;
    let style = if selected {
        Style::fg(Color::Rgb(0, 0, 0))
            .with_bg(Color::Rgb(100, 170, 255))
            .with_mods(Mods::BOLD)
    } else {
        vm.theme.body()
    };
    if selected {
        crate::chrome::fill(
            frame,
            Rect {
                x: area.x,
                y,
                w: area.w,
                h: 1,
            },
            style,
        );
    }
    let marker = if selected { ">" } else { " " };
    let indent = if project.kind == EntryKind::Worktree {
        if unicode {
            "  ↳ "
        } else {
            "  + "
        }
    } else {
        ""
    };
    let label = format!(
        "{marker} {indent}{}",
        crate::col2::sanitize_for_terminal(&project.name)
    );
    let label_width = if area.w > 38 { area.w - 19 } else { area.w };
    clip_at(frame, area.x, y, label_width, &label, style);
    if area.w > 38 {
        clip_at(
            frame,
            area.x + area.w - 18,
            y,
            18,
            status(project.status),
            style,
        );
    }
}

fn draw_preview(vm: &ViewModel, frame: &mut Frame, unicode: bool) {
    let x = vm.size.w * 2 / 3;
    let width = vm.size.w.saturating_sub(x + 3);
    for y in 3..vm.size.h.saturating_sub(2) {
        frame.set(x, y, if unicode { '│' } else { '|' }, vm.theme.dim());
    }
    let Some(project) = vm
        .projects
        .get(vm.project_cursor)
        .filter(|p| accepts(vm.project_filter, p.kind))
    else {
        return;
    };
    let text = |frame: &mut Frame, y, label: &str, style| {
        clip_at(
            frame,
            x + 2,
            y,
            width,
            &crate::col2::sanitize_for_terminal(label),
            style,
        );
    };
    text(
        frame,
        3,
        "Project details",
        vm.theme.body().with_mods(Mods::BOLD),
    );
    text(frame, 5, &project.name, vm.theme.key_hint());
    text(frame, 6, status(project.status), vm.theme.body());
    text(
        frame,
        9,
        if project.kind == EntryKind::Worktree {
            "Worktree"
        } else {
            "Repository"
        },
        vm.theme.dim(),
    );
    for (i, chunk) in project
        .path
        .chars()
        .collect::<Vec<_>>()
        .chunks(usize::from(width.max(1)))
        .take(4)
        .enumerate()
    {
        text(
            frame,
            10 + u16::try_from(i).unwrap_or(0),
            &chunk.iter().collect::<String>(),
            vm.theme.body(),
        );
    }
    if let Some(detail) = vm
        .detail
        .as_deref()
        .filter(|detail| detail.path == project.path)
    {
        if let Some(git) = &detail.git_summary {
            text(
                frame,
                16,
                &format!("Branch: {}", git.branch),
                vm.theme.dim(),
            );
        }
    }
}

fn status(status: ProjectStatus) -> &'static str {
    match status {
        ProjectStatus::Running { .. } => "Working",
        ProjectStatus::Idle => "Ready",
        ProjectStatus::PickingUp => "Loading",
        ProjectStatus::NotInitialized | ProjectStatus::NotProvisioned => "Setup required",
        ProjectStatus::SetupIncomplete => "Setup incomplete",
        ProjectStatus::NotRepository => "Not a repository",
        ProjectStatus::NotAdopted => "Not adopted",
        ProjectStatus::Error(_) => "Needs attention",
    }
}

fn footer(vm: &ViewModel) -> Vec<(u16, u16, &'static str, Option<Target>)> {
    let open = vm
        .projects
        .get(vm.project_cursor)
        .filter(|p| accepts(vm.project_filter, p.kind))
        .map(|_| Target::ProjectRow(vm.project_cursor));
    if vm.size.w < 64 {
        return vec![
            (1, 8, "Esc back", Some(Target::ProjectDrawer)),
            (10, 10, "Enter open", open),
            (22, 6, "a Add", Some(Target::RailAdd)),
            (30, 8, "↑/↓ move", None),
        ];
    }
    [
        (1, 10, "Esc back", Some(Target::ProjectDrawer)),
        (12, 12, "↑/↓ move", None),
        (25, 12, "Enter open", open),
        (39, 14, "a Add project", Some(Target::RailAdd)),
        (55, 8, "? Help", Some(Target::HeaderHelp)),
    ]
    .into_iter()
    .filter(|(x, w, _, _)| x + w <= vm.size.w)
    .collect()
}

pub(crate) fn hit_test(vm: &ViewModel, col: u16, row: u16) -> Option<Target> {
    if let Some((filter, _, _)) = filter_tabs(vm)
        .into_iter()
        .find(|(_, rect, _)| contains(*rect, col, row))
    {
        return Some(Target::ProjectFilter(filter));
    }
    if row == vm.size.h.saturating_sub(1) {
        return footer(vm)
            .into_iter()
            .find(|(x, w, _, _)| col >= *x && col < x + w)
            .and_then(|(_, _, _, target)| target);
    }
    let area = list_area(vm);
    if !contains(area, col, row) {
        return None;
    }
    visible_rows(vm)
        .into_iter()
        .find_map(|(y, item)| match item {
            Row::Project(i) if y == row => Some(Target::ProjectRow(i)),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Size;
    use crate::{AppState, Project};

    fn state(width: u16) -> AppState {
        let mut state = AppState::new(
            vec![
                Project {
                    name: "repo".into(),
                    path: "/demo/repo".into(),
                    status: ProjectStatus::Idle,
                    kind: EntryKind::Repo,
                },
                Project {
                    name: "feature".into(),
                    path: "/demo/feature".into(),
                    status: ProjectStatus::NotAdopted,
                    kind: EntryKind::Worktree,
                },
            ],
            Size::new(width, 24),
        );
        state.app.splash.skip();
        state.app.projects_open = true;
        state.app.project_cursor = 1;
        state
    }

    #[test]
    fn groups_worktrees_under_repository_and_clicks_match_visible_rows() {
        let mut state = state(120);
        let vm = state.snapshot();
        let rows = visible_rows(&vm);
        assert!(matches!(rows.first().unwrap().1, Row::Heading(ref path) if path == "/demo/repo"));
        for (y, row) in rows {
            if let Row::Project(i) = row {
                assert_eq!(hit_test(&vm, 3, y), Some(Target::ProjectRow(i)));
            }
        }
    }

    #[test]
    fn picker_replaces_the_dashboard_and_preserves_terminal_background() {
        let mut state = state(120);
        let vm = state.snapshot();
        let mut frame = Frame::new(vm.size);
        let mut backend = crate::render::AsciiBackend::new(crate::render::Capabilities::empty());
        crate::view::draw(&vm, &mut backend, &mut frame);
        let text: String = (0..vm.size.h)
            .flat_map(|y| (0..vm.size.w).map(move |x| (x, y)))
            .map(|(x, y)| frame.get(x, y).unwrap().ch)
            .collect();
        assert!(text.contains("Projects"));
        assert!(text.contains("Project details"));
        assert!(!text.contains("[Home]"));
        assert!(!text.contains("╭"));
        assert_eq!(frame.get(0, 0).unwrap().style.bg, Color::Default);
    }

    #[test]
    fn narrow_picker_keeps_open_add_and_back_clickable() {
        let mut state = state(40);
        let vm = state.snapshot();
        let y = vm.size.h - 1;
        assert_eq!(hit_test(&vm, 2, y), Some(Target::ProjectDrawer));
        assert_eq!(hit_test(&vm, 12, y), Some(Target::ProjectRow(1)));
        assert_eq!(hit_test(&vm, 23, y), Some(Target::RailAdd));
    }
    #[test]
    fn filter_chips_and_filtered_rows_share_the_click_geometry() {
        let mut state = state(80);
        for filter in FILTERS {
            let vm = state.snapshot();
            let (_, rect, _) = filter_tabs(&vm)
                .into_iter()
                .find(|(f, _, _)| *f == filter)
                .unwrap();
            assert_eq!(
                hit_test(&vm, rect.x, rect.y),
                Some(Target::ProjectFilter(filter))
            );
            crate::state::update(
                &mut state,
                crate::state::Event::Mouse(crate::MouseInput {
                    kind: crate::MouseKind::Down(crate::MouseButton::Left),
                    col: rect.x,
                    row: rect.y,
                }),
            );
            assert_eq!(state.app.project_filter, filter);
            let vm = state.snapshot();
            for (_, row) in visible_rows(&vm) {
                if let Row::Project(i) = row {
                    assert!(accepts(filter, vm.projects.get(i).unwrap().kind));
                }
            }
        }
    }
}
