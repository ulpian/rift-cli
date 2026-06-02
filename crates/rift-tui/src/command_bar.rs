//! Bottom command bar: a single-line input that dispatches `rift`/`entire`
//! commands and internal verbs (`:q`, `:help`).

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::app::{AppState, Mode};

/// Render the command bar into `area`.
pub fn render(frame: &mut Frame, area: Rect, app: &AppState) {
    let (line, style, title) = match app.mode {
        Mode::Insert => (
            format!(":{}\u{2588}", app.command_input), // trailing block = cursor
            Style::default().fg(Color::Yellow),
            " Command ",
        ),
        Mode::Normal => (
            app.status.clone(),
            Style::default().add_modifier(Modifier::DIM),
            " Status ",
        ),
    };

    let widget = Paragraph::new(line)
        .style(style)
        .block(Block::bordered().title(title));
    frame.render_widget(widget, area);
}
