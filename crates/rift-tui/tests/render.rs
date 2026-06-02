//! Headless render test: draw one frame with ratatui's `TestBackend` and assert
//! the three regions appear. Proves the layout wires up without a real TTY.

use std::path::Path;

use ratatui::backend::TestBackend;
use ratatui::Terminal;
use rift_tui::app::{AppState, ProjectItem};
use rift_tui::metrics::MetricsSnapshot;
use rift_tui::ui;

#[test]
fn renders_all_three_regions() {
    let options = vec![ProjectItem {
        name: "demo-repo".into(),
        path: "/repo".into(),
    }];
    let mut app = AppState::new(Path::new("/repo"), options, MetricsSnapshot::placeholder());

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app)).unwrap();

    let buffer = terminal.backend().buffer();
    let text: String = buffer
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();

    // Left options panel, right metrics panels, bottom bar.
    assert!(text.contains("Projects"), "options panel title missing");
    assert!(text.contains("demo-repo"), "project entry missing");
    assert!(text.contains("Usage"), "metrics headline missing");
    assert!(text.contains("Token usage"), "sparkline panel missing");
    assert!(text.contains("File changes"), "bar chart panel missing");
    assert!(text.contains("Status"), "command bar missing");
}
