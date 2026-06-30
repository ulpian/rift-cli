//! The live xRenderEngine backend: a raw-mode event loop + xre-tui rendering.
//!
//! This is the only part of Canopy that touches a real terminal. It is a thin
//! shell around the pure pieces: [`crate::input::on_key`] owns all behavior, the
//! `App` owns all state, and this module merely (1) translates xre's event
//! stream into [`CanopyKey`]s, (2) draws the current `App` into a `CellBuffer`
//! with xre-tui widgets, and (3) drives the present/poll loop. Everything here
//! is `#[cfg(feature = "xre")]`; without the feature the dashboard still builds
//! and renders through the always-available ASCII fallback.

use std::time::Duration;

use xre_core::{Cell, Color, Rect, Style};
use xre_term::{
    Capabilities, Event, EventQueue, KeyCode, KeyState, Modifiers, Presenter, TermError,
    TerminalGuard,
};
use xre_tui::{
    BorderSet, Constraint, Frame, Gauge, Layout, List, ListState, Panel, Sparkline, Widget,
};

use crate::gauge::GaugeState;
use crate::input::{on_key, Action, CanopyKey, Mode};
use crate::{App, Command, ProjStatus, Project};

/// Run the interactive Canopy dashboard on the real terminal until the user
/// quits.
///
/// `on_add` is invoked when a project is registered (via the `a` key or the
/// `/setup` palette command); it persists the project and returns the refreshed
/// project list, which becomes the new dashboard state. A persistence failure is
/// surfaced as a transient status line rather than aborting the session.
///
/// The [`TerminalGuard`] restores the terminal (cooked mode, main screen, cursor
/// shown) on return *and* on panic.
///
/// # Errors
/// [`rift_core::RiftError`] if entering raw mode, polling input, or presenting a
/// frame fails (an underlying terminal I/O error).
pub fn run_tui<F>(initial: Vec<Project>, mut on_add: F) -> rift_core::Result<()>
where
    F: FnMut(&str) -> rift_core::Result<Vec<Project>>,
{
    let _guard = TerminalGuard::enter().map_err(term_err)?;
    let caps = Capabilities::probe();
    let mut presenter = Presenter::stdout(&caps);
    let mut buf = xre_core::CellBuffer::new(caps.size);
    let mut events = EventQueue::new();
    let mut app = App::new(initial);

    loop {
        render(&app, &mut buf);
        presenter.present(&buf).map_err(term_err)?;
        if app.quit {
            break;
        }

        // Block on input, but wake often enough to animate the splash.
        let timeout = if app.splash.done() {
            Duration::from_millis(200)
        } else {
            Duration::from_millis(33)
        };
        events.pump(timeout).map_err(term_err)?;
        let drained: Vec<Event> = events.drain().collect();
        for ev in &drained {
            if let Event::Resize(size) = ev {
                buf.resize(*size);
                presenter.resize(*size);
                continue;
            }
            if let Some(key) = to_canopy(ev) {
                match on_key(&mut app, key) {
                    Action::Quit => app.quit = true,
                    Action::AddProject(path) => apply_add(&mut app, &mut on_add, &path),
                    Action::RunCommand(cmd) => handle_command(&mut app, &cmd, &mut on_add),
                    Action::None => {}
                }
            }
        }

        if !app.splash.done() {
            app.splash.tick();
        }
    }
    Ok(())
}

/// Map an xre terminal error onto the shared Rift error type without leaking
/// anything beyond a reason string.
fn term_err(e: TermError) -> rift_core::RiftError {
    match e {
        TermError::Io(io) => rift_core::RiftError::Io(io),
        other => rift_core::RiftError::Other(other.to_string()),
    }
}

/// Translate an xre input event into a terminal-agnostic [`CanopyKey`], or
/// `None` for events the dashboard ignores (mouse, focus, key releases, …).
fn to_canopy(ev: &Event) -> Option<CanopyKey> {
    let Event::Key(key) = ev else {
        return None;
    };
    if key.state == KeyState::Release {
        return None;
    }
    if key.mods.contains(Modifiers::CTRL) {
        if let KeyCode::Char('c') = key.code {
            return Some(CanopyKey::Quit);
        }
    }
    match key.code {
        KeyCode::Char(c) => Some(CanopyKey::Char(c)),
        KeyCode::Enter => Some(CanopyKey::Enter),
        KeyCode::Esc => Some(CanopyKey::Esc),
        KeyCode::Backspace => Some(CanopyKey::Backspace),
        KeyCode::Up => Some(CanopyKey::Up),
        KeyCode::Down => Some(CanopyKey::Down),
        _ => None,
    }
}

/// Register a project and reflect the result (refreshed list, or an error
/// status) back into the dashboard.
fn apply_add<F>(app: &mut App, on_add: &mut F, path: &str)
where
    F: FnMut(&str) -> rift_core::Result<Vec<Project>>,
{
    match on_add(path) {
        Ok(projects) => {
            app.set_projects(projects);
            app.status = Some(format!("registered {path}"));
        }
        Err(e) => app.status = Some(e.to_string()),
    }
}

/// Handle a slash command issued from the palette. Only the dashboard-relevant
/// verbs are wired; the rest report that they are CLI-only for now.
fn handle_command<F>(app: &mut App, cmd: &Command, on_add: &mut F)
where
    F: FnMut(&str) -> rift_core::Result<Vec<Project>>,
{
    match cmd.name.as_str() {
        "setup" => match std::env::current_dir() {
            Ok(dir) => apply_add(app, on_add, &dir.to_string_lossy()),
            Err(e) => app.status = Some(format!("setup failed: {e}")),
        },
        "quit" => app.quit = true,
        other => app.status = Some(format!("/{other} is only available from the CLI for now")),
    }
}

/// The Rift accent color (used for the title + splash).
fn accent() -> Style {
    Style::fg(Color::Rgb(0x6c, 0xe0, 0x9a))
}

/// Draw the whole dashboard for `app` into `buf`.
fn render(app: &App, buf: &mut xre_core::CellBuffer) {
    let mut frame = Frame::root(buf);
    frame.fill(Cell::new(' '));
    let area = frame.area();
    if area.width() == 0 || area.height() == 0 {
        return;
    }

    if !app.splash.done() {
        render_splash(app, &mut frame, area);
        return;
    }

    let rows =
        Layout::vertical([Constraint::Len(1), Constraint::Fill(1), Constraint::Len(3)]).split(area);
    let title = rows[0];
    let body = rows[1];
    let footer = rows[2];

    let mut header = String::from("RIFT — Canopy");
    if let Some(status) = &app.status {
        header.push_str("   ");
        header.push_str(status);
    }
    print_clipped(&mut frame, title, 0, &header, accent());

    if app.show_wizard {
        let inner = Panel::new()
            .title("projects")
            .border(Some(BorderSet::ROUNDED))
            .render(body, &mut frame);
        print_clipped(
            &mut frame,
            inner,
            0,
            "No projects registered yet.",
            Style::DEFAULT,
        );
        print_clipped(
            &mut frame,
            inner,
            1,
            "Press 'a' to add a project, or run `rift setup`.",
            Style::DEFAULT,
        );
    } else {
        render_body(app, &mut frame, body);
    }

    let bar_title = match app.mode {
        Mode::AddProject => "add project (path)",
        _ => "command",
    };
    let inner = Panel::new()
        .title(bar_title)
        .border(Some(BorderSet::ROUNDED))
        .render(footer, &mut frame);
    let prefix = if matches!(app.mode, Mode::AddProject) {
        ""
    } else {
        "/"
    };
    let line = format!("{prefix}{}\u{2588}", app.command_line);
    print_clipped(&mut frame, inner, 0, &line, Style::DEFAULT);
}

/// Draw the three-column body (projects · detail · graphs).
fn render_body(app: &App, frame: &mut Frame<'_>, body: Rect) {
    let cols = Layout::horizontal([
        Constraint::Pct(20),
        Constraint::Pct(40),
        Constraint::Pct(40),
    ])
    .split(body);

    // Column 1 — projects.
    let projects = Panel::new()
        .title("projects")
        .border(Some(BorderSet::ROUNDED))
        .render(cols[0], frame);
    let items: Vec<String> = app
        .projects
        .iter()
        .map(|p| format!("{} {}", p.status.glyph(), p.name))
        .collect();
    let mut state = ListState::new();
    state.set_len(items.len());
    state.select(app.selected);
    List::new(&items).render_stateful(projects, frame, &mut state);

    // Column 2 — detail + nic-gauge.
    let detail = Panel::new()
        .title("detail")
        .border(Some(BorderSet::ROUNDED))
        .render(cols[1], frame);
    if let Some(p) = app.selected_project() {
        print_clipped(
            frame,
            detail,
            0,
            &format!("path: {}", p.path),
            Style::DEFAULT,
        );
        print_clipped(
            frame,
            detail,
            1,
            &format!("status: {}", status_label(p.status)),
            Style::DEFAULT,
        );
    } else {
        print_clipped(frame, detail, 0, "no project selected", Style::DEFAULT);
    }
    let gauge_state = GaugeState::from_pct(app.context_pct);
    print_clipped(
        frame,
        detail,
        3,
        &format!("context — {}", gauge_state.label()),
        Style::DEFAULT,
    );
    Gauge::new(f32::from(app.context_pct) / 100.0)
        .label(format!("{}%", app.context_pct))
        .render(one_row(detail, 4), frame);

    // Column 3 — graphs.
    let graphs = Panel::new()
        .title("graphs")
        .border(Some(BorderSet::ROUNDED))
        .render(cols[2], frame);
    print_clipped(
        frame,
        graphs,
        0,
        &format!("recall savings: {}%", app.savings_pct),
        Style::DEFAULT,
    );
    Gauge::new(f32::from(app.savings_pct) / 100.0).render(one_row(graphs, 1), frame);
    print_clipped(frame, graphs, 3, "tokens", Style::DEFAULT);
    let data: Vec<f32> = app.token_series.iter().map(|&v| v as f32).collect();
    Sparkline::new(data).render(one_row(graphs, 4), frame);
}

/// Draw the startup splash: a centered "R I F T" wordmark over a progress gauge.
fn render_splash(app: &App, frame: &mut Frame<'_>, area: Rect) {
    let mid_y = area.top() + area.height() / 2;
    let title = "R I F T";
    let title_w = title.chars().count() as u32;
    let cx = area.left() + area.width().saturating_sub(title_w) / 2;
    print_clipped_at(frame, cx, mid_y.saturating_sub(1), area, title, accent());

    let gw = area.width().min(24);
    let gx = area.left() + (area.width().saturating_sub(gw)) / 2;
    if mid_y + 1 < area.bottom() {
        Gauge::new(app.splash.progress()).render(Rect::new(gx, mid_y + 1, gw, 1), frame);
    }
}

/// A one-cell-high rect `dy` rows down inside `area` (empty if out of bounds).
fn one_row(area: Rect, dy: u32) -> Rect {
    let y = area.top() + dy;
    if y >= area.bottom() {
        return Rect::new(area.left(), area.top(), 0, 0);
    }
    Rect::new(area.left(), y, area.width(), 1)
}

/// Print `text` on row `dy` of `area`, truncated to the area's width and clipped
/// vertically.
fn print_clipped(frame: &mut Frame<'_>, area: Rect, dy: u32, text: &str, style: Style) {
    print_clipped_at(frame, area.left(), area.top() + dy, area, text, style);
}

/// Print `text` at an absolute `(x, y)` but truncated to `area`'s right edge and
/// suppressed if it falls below `area`.
fn print_clipped_at(frame: &mut Frame<'_>, x: u32, y: u32, area: Rect, text: &str, style: Style) {
    if y >= area.bottom() || x >= area.right() {
        return;
    }
    let max = (area.right() - x) as usize;
    let clipped: String = text.chars().take(max).collect();
    frame.print(x, y, &clipped, style);
}

/// A short human label for a project's live status.
fn status_label(status: ProjStatus) -> &'static str {
    match status {
        ProjStatus::Active => "active",
        ProjStatus::Idle => "idle",
        ProjStatus::Error => "error",
    }
}
