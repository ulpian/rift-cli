//! Left-hand 20% panel: a stateful list of projects/files.

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, List, ListItem, ListState};
use ratatui::Frame;

use crate::app::{Focus, ProjectItem};

/// Render the option list into `area`, highlighting the current selection.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    options: &[ProjectItem],
    state: &mut ListState,
    focus: Focus,
) {
    let items: Vec<ListItem> = options
        .iter()
        .map(|p| ListItem::new(p.name.clone()))
        .collect();

    let title = if focus == Focus::Options {
        " Projects ◂ "
    } else {
        " Projects "
    };

    let list = List::new(items)
        .block(Block::bordered().title(title))
        .highlight_symbol("› ")
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    frame.render_stateful_widget(list, area, state);
}
