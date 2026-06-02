//! Top-level draw: the three-region layout.
//!
//! ```text
//! ┌──────────┬───────────────────────────────┐
//! │ options  │ metrics                        │
//! │  (20%)   │  (80%)                         │
//! │          │                                │
//! ├──────────┴───────────────────────────────┤
//! │ command bar (height 3)                    │
//! └───────────────────────────────────────────┘
//! ```

use ratatui::layout::{Constraint, Layout};
use ratatui::Frame;

use crate::app::AppState;
use crate::command_bar;
use crate::widgets::{metrics, option_list};

/// Draw the entire UI for one frame.
pub fn draw(frame: &mut Frame, app: &mut AppState) {
    let [body, command] =
        Layout::vertical([Constraint::Fill(1), Constraint::Length(3)]).areas(frame.area());

    let [options, metrics_area] =
        Layout::horizontal([Constraint::Percentage(20), Constraint::Percentage(80)]).areas(body);

    option_list::render(frame, options, &app.options, &mut app.list_state, app.focus);
    metrics::render(frame, metrics_area, &app.metrics);
    command_bar::render(frame, command, app);
}
