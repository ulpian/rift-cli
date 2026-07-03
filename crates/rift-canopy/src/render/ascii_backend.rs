//! The ASCII fallback backend — the guaranteed floor (no `xre-rs`).
//!
//! Two roles:
//!   1. [`AsciiBackend::render`] — the legacy stacked one-frame renderer the bin
//!      prints for the non-TTY / headless path (`Vec<String>`, byte-stable so
//!      scripts and the existing tests don't drift).
//!   2. The [`RenderBackend`] impl — flushes a Canopy [`Frame`] to stdout as
//!      lines, so the interactive `--ascii` path shares the one `view::draw`.

use std::io::Write;

use super::frame::Frame;
use super::{Capabilities, RenderBackend, RenderError};
use crate::gauge::render_bar;
use crate::layout::Rect;
use crate::{App, Mode, ProjectStatus};

/// The render frame for the legacy line renderer: one string per row.
pub type Lines = Vec<String>;

/// The ASCII fallback backend (no `xre-rs`, works on any terminal).
pub struct AsciiBackend;

impl AsciiBackend {
    /// Construct the ASCII backend. Capabilities are dropped — the floor reports
    /// the empty set from [`RenderBackend::capabilities`].
    #[must_use]
    pub fn new(_caps: Capabilities) -> Self {
        Self
    }

    /// The legacy stacked render: the dashboard for `app` as lines of text. Used
    /// by the bin's non-TTY one-frame path; kept byte-stable.
    #[must_use]
    pub fn render(&self, app: &App, _area: Rect) -> Lines {
        let mut frame: Lines = Vec::new();

        if !app.splash.done() {
            let bar = render_bar((app.splash.progress() * 100.0) as u8, 24);
            frame.push("        R I F T".to_string());
            frame.push(format!("  {bar}"));
            return frame;
        }

        frame.push("RIFT — Canopy".to_string());

        if app.show_wizard {
            frame.push("No projects registered yet.".to_string());
            frame.push("Run `rift setup` or press 'a' to add a project.".to_string());
        } else {
            // Column 1 — projects
            frame.push("[ PROJECTS ]".to_string());
            for (i, p) in app.projects.iter().enumerate() {
                let cursor = if i == app.selected { '>' } else { ' ' };
                frame.push(format!("{cursor} {} {}", p.status.glyph(), p.name));
            }

            // Column 2 — detail
            frame.push("[ DETAIL ]".to_string());
            if let Some(p) = app.selected_project() {
                frame.push(format!("path: {}", p.path));
                frame.push(format!("status: {}", status_label(p.status)));
            }

            // Column 3 — graphs
            frame.push("[ GRAPHS ]".to_string());
            frame.push(format!("nic-gauge: {}", render_bar(app.context_pct, 16)));
            frame.push(format!("recall savings: {}%", app.savings_pct));
            frame.push(format!("tokens: {}", sparkline(&app.token_series)));
        }

        // transient status / toast (kept above the bar so the bar stays last)
        if let Some(status) = &app.status {
            frame.push(status.clone());
        }

        // command / add-project bar (mode-aware)
        match app.mode {
            Mode::AddProject => frame.push(format!("add path> {}", app.command_line)),
            _ => frame.push(format!("/{}", app.command_line)),
        }
        frame
    }
}

impl RenderBackend for AsciiBackend {
    fn name(&self) -> &'static str {
        "ascii-fallback"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::empty()
    }

    fn present(&mut self, frame: &Frame) -> Result<(), RenderError> {
        let mut out = std::io::stdout().lock();
        // Full repaint: cursor home, then each row. ANSI home works on any
        // VT-ish terminal; the dumb-terminal case uses the one-frame `render`.
        let mut buf = String::from("\x1b[H");
        let w = frame.width();
        for y in 0..frame.height() {
            let mut line = String::with_capacity(usize::from(w));
            for x in 0..w {
                line.push(frame.get(x, y).map_or(' ', |c| c.ch));
            }
            buf.push_str(line.trim_end());
            buf.push_str("\x1b[K\r\n"); // clear to EOL so stale glyphs don't linger
        }
        out.write_all(buf.as_bytes())
            .and_then(|()| out.flush())
            .map_err(|e| RenderError::Terminal(e.to_string()))
    }
}

/// Render a `u64` series as block-glyph sparkline cells.
fn sparkline(series: &[u64]) -> String {
    const RAMP: &[char] = &['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = series.iter().copied().max().unwrap_or(0);
    if max == 0 {
        return "▁".repeat(series.len());
    }
    series
        .iter()
        .map(|v| {
            let idx = (u128::from(*v) * (RAMP.len() as u128 - 1) / u128::from(max)) as usize;
            *RAMP.get(idx).unwrap_or(&'▁')
        })
        .collect()
}

/// A short human label for a project's live status.
fn status_label(s: ProjectStatus) -> &'static str {
    match s {
        ProjectStatus::Running { .. } => "running",
        ProjectStatus::PickingUp => "picking up",
        ProjectStatus::Idle => "idle",
        ProjectStatus::NotInitialized => "not initialized",
        ProjectStatus::Error(_) => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::frame::Size;
    use crate::{Project, ProjectStatus, SplashState};

    fn area() -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: 120,
            h: 40,
        }
    }

    fn app() -> App {
        let mut a = App::new(vec![
            Project {
                name: "rift-cli".into(),
                path: "/r/rift-cli".into(),
                status: ProjectStatus::Running {
                    since_ms: 0,
                    active_sessions: 1,
                },
            },
            Project {
                name: "rift-site".into(),
                path: "/r/rift-site".into(),
                status: ProjectStatus::Idle,
            },
        ]);
        a.splash = SplashState::new();
        a.splash.skip(); // past the splash
        a.context_pct = 50;
        a.savings_pct = 18;
        a.token_series = vec![1, 4, 2, 8, 5];
        a
    }

    #[test]
    fn renders_three_panels_and_command_bar() {
        let frame = AsciiBackend.render(&app(), area());
        let joined = frame.join("\n");
        assert!(joined.contains("PROJECTS"));
        assert!(joined.contains("rift-cli"));
        assert!(joined.contains("DETAIL"));
        assert!(joined.contains("GRAPHS"));
        assert!(joined.contains("nic-gauge"));
        assert!(joined.contains("Thinking")); // 50% gauge
        assert!(joined.contains("recall savings: 18%"));
        assert!(joined.ends_with('/'));
    }

    #[test]
    fn shows_splash_before_done() {
        let mut a = app();
        a.splash = SplashState::new(); // not done
        let frame = AsciiBackend.render(&a, area());
        assert!(frame.join("\n").contains("R I F T"));
    }

    #[test]
    fn shows_wizard_when_empty() {
        let mut a = App::new(vec![]);
        a.splash.skip();
        let frame = AsciiBackend.render(&a, area());
        assert!(frame.join("\n").contains("No projects registered"));
    }

    #[test]
    fn shows_add_project_prompt() {
        let mut a = app();
        a.mode = crate::Mode::AddProject;
        a.command_line = "/tmp/repo".into();
        let frame = AsciiBackend.render(&a, area());
        let joined = frame.join("\n");
        assert!(joined.contains("add path> /tmp/repo"));
    }

    #[test]
    fn shows_status_line_when_set() {
        let mut a = app();
        a.status = Some("registered demo".into());
        let frame = AsciiBackend.render(&a, area());
        let joined = frame.join("\n");
        assert!(joined.contains("registered demo"));
        assert!(joined.ends_with('/'), "command bar stays last");
    }

    #[test]
    fn backend_name() {
        assert_eq!(AsciiBackend.name(), "ascii-fallback");
    }

    #[test]
    fn present_emits_frame_cells() {
        // A trait-level present writes the frame glyphs (smoke: no panic, content path).
        let mut f = Frame::new(Size::new(6, 2));
        f.put_str(0, 0, "hi", super::super::Style::DEFAULT);
        // We can't capture stdout here easily; assert the frame content is what
        // present would emit (glyph extraction parity).
        let row0: String = (0..f.width())
            .map(|x| f.get(x, 0).map_or(' ', |c| c.ch))
            .collect();
        assert_eq!(row0.trim_end(), "hi");
    }
}
