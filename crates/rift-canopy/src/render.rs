//! The render seam + an ASCII fallback backend.
//!
//! [`RenderBackend`] is the swappable seam: the production backend is `xre-rs`
//! (3D splash, braille graphs), and [`AsciiBackend`] is the always-available
//! degraded fallback (plain cells, block sparklines). The dashboard is rendered
//! to a [`Frame`] (lines of text), which makes headless render tests trivial.

use crate::gauge::render_bar;
use crate::layout::Rect;
use crate::{App, Mode, ProjStatus};

/// A rendered frame: one string per row.
pub type Frame = Vec<String>;

/// The swappable render backend.
pub trait RenderBackend {
    /// A human-readable backend name.
    fn name(&self) -> &str;
    /// Render the dashboard for `app` within `area`.
    fn render(&self, app: &App, area: Rect) -> Frame;
}

/// The ASCII fallback backend (no `xre-rs`, works on any terminal).
pub struct AsciiBackend;

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

impl RenderBackend for AsciiBackend {
    fn name(&self) -> &'static str {
        "ascii-fallback"
    }

    fn render(&self, app: &App, _area: Rect) -> Frame {
        let mut frame: Frame = Vec::new();

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

fn status_label(s: ProjStatus) -> &'static str {
    match s {
        ProjStatus::Active => "active",
        ProjStatus::Idle => "idle",
        ProjStatus::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ProjStatus, Project, SplashState};

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
                status: ProjStatus::Active,
            },
            Project {
                name: "rift-site".into(),
                path: "/r/rift-site".into(),
                status: ProjStatus::Idle,
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
}
