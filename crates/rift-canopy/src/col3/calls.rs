//! The Recall panel's per-channel serve-call chart: the series the data plane
//! fills and the sparkline the card draws under its bars.

use crate::layout::Rect;
use crate::plot;
use crate::render::frame::{Color, Frame, Style};
use crate::render::RenderBackend;
use crate::theme::Theme;

use super::format::f64_of;

/// Recall serve-call numbers, per serve channel.
#[derive(Clone, Debug, PartialEq)]
pub struct CallsData {
    /// Total attributed serve calls across all `serve_call_event` history.
    pub total: u64,
    /// Exclusive end of the event-anchored 30-minute window.
    pub window_end_ms: i64,
    /// One entry per serve channel active in the visible window, freshest
    /// first. The channel vocabulary is closed and small (the store's
    /// `serve_call_event.channel` CHECK admits `hook_inject` and
    /// `skill_cli`), so every active channel is drawn — nothing is ever
    /// truncated away or aggregated into an "other" bucket.
    pub series: Vec<ChannelCallSeries>,
}

/// One serve channel's calls per minute, oldest bucket first.
#[derive(Clone, Debug, PartialEq)]
pub struct ChannelCallSeries {
    /// The `ServeChannel` token this series aggregates (`hook_inject` /
    /// `skill_cli`), as stored — the view shortens it for the legend.
    pub channel: String,
    /// Terminal timestamp of this channel's freshest visible call.
    pub last_call_at: i64,
    /// Exactly 30 event-anchored one-minute buckets.
    pub rate: Vec<f64>,
}

/// The legend name for a stored `ServeChannel` token. The legend row is the
/// narrowest text in the card, so the two known tokens get short, unambiguous
/// names; an unrecognised token (only reachable if the store's CHECK
/// vocabulary grows) prints its own first cells rather than a guess.
fn channel_label(channel: &str) -> String {
    match channel {
        "hook_inject" => "hook".to_owned(),
        "skill_cli" => "skill".to_owned(),
        other => other.chars().take(5).collect(),
    }
}

pub(super) fn draw_call_chart(
    calls: &CallsData,
    theme: Theme,
    unicode: bool,
    chart: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let y_max = call_y_max(&calls.series);
    let colours = theme.recall_channel_colors();
    let labels: Vec<String> = calls
        .series
        .iter()
        .map(|s| channel_label(&s.channel))
        .collect();
    let legend: Vec<(Color, &str)> = labels
        .iter()
        .enumerate()
        .map(|(i, label)| {
            (
                colours.get(i).copied().unwrap_or(theme.graph_calls()),
                label.as_str(),
            )
        })
        .collect();
    let (past_30, past_15) = if unicode {
        ("−30m", "−15m")
    } else {
        ("-30m", "-15m")
    };
    let x_ticks = [
        crate::axis::XTick {
            position: 0.0,
            label: past_30,
        },
        crate::axis::XTick {
            position: 0.5,
            label: past_15,
        },
        crate::axis::XTick {
            position: 1.0,
            label: "last",
        },
    ];
    let spec = crate::axis::AxisSpec {
        max: y_max,
        humanize: fmt_call_tick,
        x_axis: crate::axis::XAxis::Positions(&x_ticks),
        legend: &legend,
    };
    let plot_rect = crate::axis::draw(frame, theme, chart, &spec, unicode);
    if plot_rect.w == 0 || plot_rect.h == 0 {
        return;
    }
    for (i, series) in calls.series.iter().enumerate() {
        let vertices = plot::sparkline_points(&series.rate, y_max);
        backend.polyline(
            &vertices,
            plot_rect,
            Style::fg(colours.get(i).copied().unwrap_or(theme.graph_calls())),
            frame,
        );
    }
}

fn fmt_call_tick(value: f64) -> String {
    let value = value.round().max(0.0) as u64;
    value.to_string()
}

/// One integral scale shared by every visible call line: zero at the bottom and
/// the smallest even ceiling at least as high as the busiest minute (minimum 2).
fn call_y_max(series: &[ChannelCallSeries]) -> f64 {
    let observed = series
        .iter()
        .flat_map(|item| item.rate.iter().copied())
        .filter(|value| value.is_finite())
        .fold(0.0f64, f64::max)
        .ceil();
    let observed = observed.max(0.0) as u64;
    let ceiling = observed.max(2);
    f64_of(ceiling.saturating_add(ceiling % 2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_lines_share_the_smallest_even_integral_scale() {
        let series = |values: &[f64]| ChannelCallSeries {
            channel: "hook_inject".into(),
            last_call_at: 0,
            rate: values.to_vec(),
        };
        assert_eq!(call_y_max(&[]), 2.0);
        assert_eq!(call_y_max(&[series(&[0.0, 1.0])]), 2.0);
        assert_eq!(call_y_max(&[series(&[2.0]), series(&[3.0])]), 4.0);
        assert_eq!(call_y_max(&[series(&[6.0])]), 6.0);
    }
}
