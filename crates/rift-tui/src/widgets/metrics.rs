//! Right-hand 80% panel: graphical usage metrics.
//!
//! Stacked panels — a headline summary, a token-usage sparkline, and a
//! per-checkpoint file-change bar chart.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::widgets::{BarChart, Block, Paragraph, Sparkline};
use ratatui::Frame;

use crate::metrics::MetricsSnapshot;

/// Render the metrics panels into `area`.
pub fn render(frame: &mut Frame, area: Rect, metrics: &MetricsSnapshot) {
    let [headline, tokens, files] = Layout::vertical([
        Constraint::Length(4),
        Constraint::Fill(1),
        Constraint::Fill(1),
    ])
    .areas(area);

    render_headline(frame, headline, metrics);
    render_tokens(frame, tokens, metrics);
    render_files(frame, files, metrics);
}

fn render_headline(frame: &mut Frame, area: Rect, metrics: &MetricsSnapshot) {
    let mut body = format!(
        "checkpoints: {}    input tokens: {}    output tokens: {}",
        metrics.checkpoints, metrics.total_input_tokens, metrics.total_output_tokens,
    );
    if let Some(note) = &metrics.note {
        body.push('\n');
        body.push_str(note);
    }
    let widget = Paragraph::new(body).block(Block::bordered().title(" Usage "));
    frame.render_widget(widget, area);
}

fn render_tokens(frame: &mut Frame, area: Rect, metrics: &MetricsSnapshot) {
    let sparkline = Sparkline::default()
        .block(Block::bordered().title(" Token usage "))
        .data(metrics.token_series.iter().copied())
        .style(Style::default().fg(Color::Cyan));
    frame.render_widget(sparkline, area);
}

fn render_files(frame: &mut Frame, area: Rect, metrics: &MetricsSnapshot) {
    let bars: Vec<(&str, u64)> = metrics
        .file_changes
        .iter()
        .map(|(label, value)| (label.as_str(), *value))
        .collect();

    let chart = BarChart::default()
        .block(Block::bordered().title(" File changes per checkpoint "))
        .bar_width(7)
        .bar_gap(2)
        .data(bars.as_slice())
        .bar_style(Style::default().fg(Color::Green));
    frame.render_widget(chart, area);
}
