//! Committed golden frames (insta) guarding visual drift in the deterministic
//! renderers: the ASCII dashboard, the static figlet banner, and the 2D splash
//! keyframes. Every input is fixed (seed/size/state), so these are stable.

use rift_canopy::render::frame::{Frame, Size};
use rift_canopy::render::Capabilities;
use rift_canopy::splash::director::Tier;
use rift_canopy::splash::render2d::draw_tier;
use rift_canopy::state::AppState;
use rift_canopy::{AsciiBackend, Project, ProjectStatus};

/// Render a frame to a trailing-trimmed text block (stable across runs).
fn frame_text(frame: &Frame) -> String {
    let mut lines = Vec::new();
    for y in 0..frame.height() {
        let mut line = String::new();
        for x in 0..frame.width() {
            line.push(frame.get(x, y).map_or(' ', |c| c.ch));
        }
        lines.push(line.trim_end().to_string());
    }
    // Drop trailing blank lines for a compact golden.
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

fn dashboard_state() -> AppState {
    let mut s = AppState::new(
        vec![
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
        ],
        Size::new(80, 18),
    );
    s.app.splash.skip();
    s.app.context_pct = 50;
    s.app.savings_pct = 18;
    s.app.token_series = vec![1, 4, 2, 8, 5, 3];
    s
}

#[test]
fn golden_dashboard_frame() {
    let mut s = dashboard_state();
    let mut f = Frame::new(s.size);
    rift_canopy::view::draw(&s.snapshot(), &mut f);
    insta::assert_snapshot!("dashboard", frame_text(&f));
}

#[test]
fn golden_dashboard_frame_wide() {
    // A wider terminal guards the 20/40/40 tiling at a second size (P3 S1).
    let mut s = dashboard_state();
    s.size = Size::new(120, 40);
    let mut f = Frame::new(s.size);
    rift_canopy::view::draw(&s.snapshot(), &mut f);
    insta::assert_snapshot!("dashboard_wide", frame_text(&f));
}

/// Render `dashboard_state()` at `size` and return the golden text (P3 S2 modes).
fn render_at(w: u16, h: u16) -> String {
    let mut s = dashboard_state();
    s.size = Size::new(w, h);
    let mut f = Frame::new(s.size);
    rift_canopy::view::draw(&s.snapshot(), &mut f);
    frame_text(&f)
}

#[test]
fn golden_layout_two_col() {
    // 48 ≤ w < 76 → the two-column degraded grid.
    insta::assert_snapshot!("layout_two_col", render_at(60, 20));
}

#[test]
fn golden_layout_single_panel() {
    // w < 48 → one panel + a tab strip.
    insta::assert_snapshot!("layout_single_panel", render_at(36, 16));
}

#[test]
fn golden_layout_too_small() {
    // Below the floor → the centered notice.
    insta::assert_snapshot!("layout_too_small", render_at(22, 5));
}

// --- graph widgets (M4-09): fixed series, block (floor) tier ----------------

/// Render `points` into a bordered plot box and return the golden text.
fn widget_text(points: &[rift_canopy::plot::PlotPoint]) -> String {
    use rift_canopy::render::frame::Style;
    let mut f = Frame::new(Size::new(26, 8));
    let inner = f.border(f.rect(), "plot", Style::DEFAULT);
    rift_canopy::plot::draw_blocks(&mut f, inner, points, Style::DEFAULT);
    frame_text(&f)
}

const WIDGET_SERIES: [f64; 12] = [1.0, 3.0, 2.0, 6.0, 4.0, 9.0, 7.0, 5.0, 8.0, 2.0, 4.0, 6.0];

#[test]
fn golden_widget_sparkline() {
    let max = rift_canopy::plot::autoscale_max(&WIDGET_SERIES);
    insta::assert_snapshot!(
        "widget_sparkline",
        widget_text(&rift_canopy::plot::sparkline_points(&WIDGET_SERIES, max))
    );
}

#[test]
fn golden_widget_bars() {
    let max = rift_canopy::plot::autoscale_max(&WIDGET_SERIES);
    insta::assert_snapshot!(
        "widget_bars",
        widget_text(&rift_canopy::plot::bar_points(&WIDGET_SERIES, max, 24))
    );
}

#[test]
fn golden_widget_line() {
    let max = rift_canopy::plot::autoscale_max(&WIDGET_SERIES);
    insta::assert_snapshot!(
        "widget_line",
        widget_text(&rift_canopy::plot::line_points(&WIDGET_SERIES, max, 48))
    );
}

#[test]
fn golden_widget_gauge() {
    insta::assert_snapshot!(
        "widget_gauge",
        widget_text(&rift_canopy::plot::gauge_points(0.65, 48, 4))
    );
}

#[test]
fn golden_widget_braille_line() {
    // The sub-cell tier over the same fixed series (cell-aligned with the
    // block tier by the one-contract property; this pins the dot pattern).
    use rift_canopy::render::frame::Style;
    let max = rift_canopy::plot::autoscale_max(&WIDGET_SERIES);
    let pts = rift_canopy::plot::line_points(&WIDGET_SERIES, max, 48);
    let mut f = Frame::new(Size::new(26, 8));
    let inner = f.border(f.rect(), "plot", Style::DEFAULT);
    rift_canopy::plot::draw_braille(&mut f, inner, &pts, Style::DEFAULT);
    insta::assert_snapshot!("widget_braille_line", frame_text(&f));
}

#[test]
fn golden_static_banner() {
    let mut b = AsciiBackend;
    let mut f = Frame::new(Size::new(34, 7));
    draw_tier(
        Tier::StaticAscii,
        0.0,
        1,
        &mut b,
        &mut f,
        Capabilities::empty(),
    );
    insta::assert_snapshot!("splash_static_banner", frame_text(&f));
}

#[test]
fn golden_reduced2d_final_frame() {
    let mut b = AsciiBackend;
    let mut f = Frame::new(Size::new(48, 16));
    draw_tier(
        Tier::Reduced2d,
        1.0,
        42,
        &mut b,
        &mut f,
        Capabilities::empty(),
    );
    insta::assert_snapshot!("splash_reduced2d_final", frame_text(&f));
}

#[test]
fn golden_reduced2d_midframe() {
    let mut b = AsciiBackend;
    let mut f = Frame::new(Size::new(48, 16));
    draw_tier(
        Tier::Reduced2d,
        0.5,
        7,
        &mut b,
        &mut f,
        Capabilities::empty(),
    );
    insta::assert_snapshot!("splash_reduced2d_mid", frame_text(&f));
}
