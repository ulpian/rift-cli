//! The System panel: the context-pressure gauge (a donut on sub-cell backends,
//! a bar elsewhere), the active-session badge and the four domain metric rows.

use crate::layout::Rect;
use crate::plot::{self, Paint};
use crate::render::frame::{Frame, Style};
use crate::render::{Capabilities, RenderBackend};
use crate::theme::{self, Icon, Section, Theme};

use super::format::{f64_of, fmt_km};
use super::{body, panel_frame, Col3};

/// The nic-gauge section.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum NicView {
    /// No model calls in scope yet.
    #[default]
    NoData,
    /// The model's window is unknown — an explicit indicator, never a
    /// defaulted percentage.
    UnknownModel {
        /// The model id whose window is missing from the reference table.
        model: String,
        /// Distinct active top-level capture sessions.
        active_sessions: u16,
    },
    /// A measured reading.
    Reading(NicReading),
}

/// A measured context reading for the freshest thread.
#[derive(Clone, Debug, PartialEq)]
pub struct NicReading {
    /// Percent of the window in use (0..=100).
    pub pct: u8,
    /// Neutral pressure band (Clear/Moderate/High/Very High/Critical).
    pub band: &'static str,
    /// The model the reading is for.
    pub model: String,
    /// Distinct active top-level capture sessions (the same query as the
    /// active-session badge by construction).
    pub active_sessions: u16,
}

/// Distinct active top-level capture sessions.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ActiveSessionsView {
    /// The session cache shows nothing live.
    #[default]
    NoSessions,
    /// Live count.
    Count(u16),
}

/// The auditable facts carried by the atomic System-panel feed.
///
/// Recent activity is event-anchored over five minutes, context usage comes
/// from a separate main-chain observation, and storage is the capture + Recall
/// projection size. Missing authorities remain `None`; no value is fabricated.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelMetrics {
    /// Captured canonical events in the event-anchored five-minute window.
    pub events_5m: Option<u64>,
    /// Whole-turn token spend in the same five-minute window.
    pub tokens_5m: Option<u64>,
    /// Recall serves (hook-injection + skill-CLI, hook-and-skill serving) in the same
    /// window, from `serve_call_event`.
    pub serve_calls_5m: Option<u64>,
    /// Projection size — on-disk bytes of the capture + recall projections
    /// (`None` = no projection on disk).
    pub projection_bytes: Option<u64>,
    /// Tokens in the selected main-chain context observation.
    pub context_used: Option<u64>,
    /// Model window for that observation (`None` for an unknown model).
    pub context_window: Option<u64>,
    /// Whether the context observation belongs to current or previous work.
    pub context_scope: SystemScope,
    /// Which session set the five-minute activity facts describe.
    pub activity_scope: SystemScope,
}

/// Explicit scope for System-panel facts; no silent fallback exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SystemScope {
    /// Aggregated across all currently live top-level capture sessions.
    CurrentSessions,
    /// Held from the most recently completed session.
    LastCompletedSession,
    /// Identity or data needed to establish scope is unavailable.
    #[default]
    Unavailable,
}

/// The collapsed System head: context + sessions, plus the wake-mode suffix
/// when the bus is poll-only.
fn system_head(col3: &Col3, active_label: &str) -> String {
    let mut head = match &col3.nic {
        NicView::Reading(r) => format!("{}% {}  ·  {active_label}", r.pct, r.band),
        NicView::UnknownModel { .. } => format!("context ?  ·  {active_label}"),
        NicView::NoData => format!("context unavailable  ·  {active_label}"),
    };
    if col3.watch == crate::state::WatchMode::PollOnly {
        head.push_str("  ·  poll-only");
    }
    head
}

/// The wake-mode row: drawn only when the bus is poll-only — a
/// native watcher is the expectation and draws nothing.
fn draw_watch_row(theme: Theme, unicode: bool, inner: Rect, dy: u16, frame: &mut Frame) {
    bullet_row(
        theme,
        unicode,
        inner,
        dy,
        " watch: poll-only",
        theme.dim(),
        frame,
    );
}

pub(super) fn draw_system_panel(
    col3: &Col3,
    theme: Theme,
    focused: bool,
    unicode: bool,
    collapsed: bool,
    rect: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let inner = panel_frame(theme, Section::System, focused, "system", rect, frame);
    let active = match col3.active_sessions {
        ActiveSessionsView::Count(count) => count,
        ActiveSessionsView::NoSessions => 0,
    };
    if collapsed {
        let head = system_head(col3, &active_sessions_label(active));
        body(frame, inner, 0, &head, theme.body());
        return;
    }
    // The gauge occupies the top; the domain metrics the bottom rows — plus,
    // where the panel is tall enough, one hint row naming the rates' scope:
    // current-session facts are labelled directly; completed-session facts
    // hold dimmed without ever being called idle.
    let hint = inner.h >= 8;
    let poll_only = col3.watch == crate::state::WatchMode::PollOnly;
    let metric_rows = 4u16 + u16::from(hint) + u16::from(poll_only);
    let gauge_rect = Rect {
        h: inner.h.saturating_sub(metric_rows),
        ..inner
    };
    draw_context_pressure(
        &col3.nic,
        &col3.metrics,
        active,
        theme,
        unicode,
        gauge_rect,
        backend,
        frame,
    );
    let mut dy = gauge_rect.h;
    if hint {
        draw_scope_hint(
            theme,
            unicode,
            col3.metrics.activity_scope,
            inner,
            dy,
            frame,
        );
        dy += 1;
    }
    draw_metric_rows(theme, unicode, &col3.metrics, inner, dy, frame);
    if poll_only {
        draw_watch_row(theme, unicode, inner, dy + 4, frame);
    }
}

/// The scope hint row: which session set the five-minute rates describe.
fn draw_scope_hint(
    theme: Theme,
    unicode: bool,
    scope: SystemScope,
    inner: Rect,
    dy: u16,
    frame: &mut Frame,
) {
    let sep = if unicode { "·" } else { "-" };
    let (word, style) = match scope {
        SystemScope::CurrentSessions => ("current sessions", theme.body()),
        SystemScope::LastCompletedSession => ("last completed session", theme.dim()),
        SystemScope::Unavailable => ("session unavailable", theme.dim()),
    };
    body(
        frame,
        indented(inner),
        dy,
        &format!("scope {sep} {word}"),
        style,
    );
}

/// The four metric rows from `dy` down. The three session rates render dimmed
/// when no session is live (held from the last one — a stale marker, not an
/// absence); the projection size is instantaneous and always body.
fn draw_metric_rows(
    theme: Theme,
    unicode: bool,
    metrics: &PanelMetrics,
    inner: Rect,
    dy: u16,
    frame: &mut Frame,
) {
    let held = metrics.activity_scope != SystemScope::CurrentSessions;
    let rows = [
        (
            "events · 5m",
            metrics.events_5m.map(|value| value.to_string()),
            held,
        ),
        (
            "tokens · 5m",
            metrics.tokens_5m.map(|value| fmt_km(f64_of(value))),
            held,
        ),
        (
            "serves · 5m",
            metrics.serve_calls_5m.map(|value| value.to_string()),
            held,
        ),
        (
            "projection storage",
            metrics.projection_bytes.map(fmt_bytes),
            false,
        ),
    ];
    for (i, (label, value, dimmed)) in rows.into_iter().enumerate() {
        let Ok(offset) = u16::try_from(i) else { break };
        let text = value.map_or_else(|| format!(" {label}: —"), |v| format!(" {label}: {v}"));
        let style = if value_present(&text) && !dimmed {
            theme.body()
        } else {
            theme.dim()
        };
        bullet_row(theme, unicode, inner, dy + offset, &text, style, frame);
    }
}

/// One bulleted row: the dot in the panel's first cell, the text after it.
fn bullet_row(
    theme: Theme,
    unicode: bool,
    inner: Rect,
    dy: u16,
    text: &str,
    style: Style,
    frame: &mut Frame,
) {
    frame.set(
        inner.x,
        inner.y + dy,
        Icon::Bullet.glyph(unicode),
        theme.dim(),
    );
    body(frame, indented(inner), dy, text, style);
}

/// The inner rect one cell in — where a row's text sits after its bullet.
fn indented(inner: Rect) -> Rect {
    Rect {
        x: inner.x + 1,
        w: inner.w.saturating_sub(1),
        ..inner
    }
}

/// Whether a metric row carries a real value (not the `—` placeholder).
fn value_present(text: &str) -> bool {
    !text.ends_with('—')
}

/// Draw context pressure: a radial donut on SUBCELL-capable backends, degrading
/// to the shipped horizontal gauge bar elsewhere; the unknown-model indicator
/// and the no-data placeholder render explicitly (never a percentage; the gauge's single owner).
fn draw_context_pressure(
    nic: &NicView,
    metrics: &PanelMetrics,
    active_sessions: u16,
    theme: Theme,
    unicode: bool,
    rect: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    if rect.h == 0 || rect.w == 0 {
        return;
    }
    match nic {
        NicView::NoData => {
            body(frame, rect, rect.h / 2, "context unavailable", theme.dim());
            body(
                frame,
                rect,
                rect.h / 2 + 1,
                &format!("[{}]", active_sessions_label(active_sessions)),
                theme.body(),
            );
        }
        NicView::UnknownModel { model, .. } => {
            let q = Icon::CheckPending.glyph(unicode);
            body(frame, rect, 0, &format!("{q} {model}"), theme.dim());
            body(
                frame,
                rect,
                1,
                &format!("{} / unknown", context_used(metrics)),
                theme.dim(),
            );
            body(
                frame,
                rect,
                2,
                context_scope_label(metrics.context_scope),
                theme.dim(),
            );
            body(
                frame,
                rect,
                3,
                &format!("[{}]", active_sessions_label(active_sessions)),
                theme.body(),
            );
        }
        NicView::Reading(r) => {
            if backend.capabilities().contains(Capabilities::SUBCELL) && rect.h >= 5 {
                draw_context_radial(r, metrics, active_sessions, theme, rect, backend, frame);
            } else {
                draw_context_bar(r, metrics, active_sessions, theme, rect, backend, frame);
            }
        }
    }
}

fn draw_context_radial(
    reading: &NicReading,
    metrics: &PanelMetrics,
    active_sessions: u16,
    theme: Theme,
    rect: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let colour = theme::band_color(reading.band);
    let samples = rect
        .w
        .saturating_mul(rect.h)
        .saturating_mul(2)
        .clamp(16, 720);
    let pts = plot::donut_points(reading.pct, samples);
    backend.plot(&pts, rect, &Paint::Solid(Style::fg(colour)), frame);
    center(
        frame,
        rect,
        0,
        &format!(
            "{} · {}/{}",
            reading.model,
            context_used(metrics),
            metrics
                .context_window
                .map_or_else(|| "?".to_string(), |v| fmt_km(v as f64))
        ),
        if metrics.context_scope == SystemScope::CurrentSessions {
            theme.body()
        } else {
            theme.dim()
        },
    );
    center(
        frame,
        rect,
        1,
        context_scope_label(metrics.context_scope),
        theme.dim(),
    );
    center(
        frame,
        rect,
        rect.h / 2,
        &format!("{}%", reading.pct),
        theme.tile_value(colour),
    );
    center(frame, rect, rect.h / 2 + 1, reading.band, theme.dim());
    center(
        frame,
        rect,
        rect.h.saturating_sub(1),
        &format!("[{}]", active_sessions_label(active_sessions)),
        theme.body(),
    );
}

fn draw_context_bar(
    reading: &NicReading,
    metrics: &PanelMetrics,
    active_sessions: u16,
    theme: Theme,
    rect: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    body(
        frame,
        rect,
        0,
        &format!("{}% {} · {}", reading.pct, reading.band, reading.model),
        theme.body(),
    );
    if rect.h > 1 {
        body(
            frame,
            rect,
            1,
            &format!(
                "{}/{} · {}",
                context_used(metrics),
                metrics
                    .context_window
                    .map_or_else(|| "?".to_string(), |v| fmt_km(v as f64)),
                context_scope_label(metrics.context_scope)
            ),
            if metrics.context_scope == SystemScope::CurrentSessions {
                theme.body()
            } else {
                theme.dim()
            },
        );
    }
    if rect.h > 2 {
        body(
            frame,
            rect,
            2,
            &format!("[{}]", active_sessions_label(active_sessions)),
            theme.body(),
        );
    }
    if rect.h > 3 {
        let bar = Rect {
            x: rect.x,
            y: rect.y + 3,
            w: rect.w,
            h: rect.h - 3,
        };
        let pts = plot::gauge_points(
            f32::from(reading.pct) / 100.0,
            bar.w.saturating_mul(2).max(2),
            bar.h.saturating_mul(4).clamp(1, 4),
        );
        backend.plot(&pts, bar, &Paint::HRamp(&theme::GAUGE_RAMP), frame);
    }
}

fn context_used(metrics: &PanelMetrics) -> String {
    metrics
        .context_used
        .map_or_else(|| "?".to_string(), |value| fmt_km(value as f64))
}

pub(super) fn active_sessions_label(count: u16) -> String {
    if count == 1 {
        "1 active session".to_string()
    } else {
        format!("{count} active sessions")
    }
}

fn context_scope_label(scope: SystemScope) -> &'static str {
    match scope {
        SystemScope::CurrentSessions => "current sessions",
        SystemScope::LastCompletedSession => "previous session",
        SystemScope::Unavailable => "session unavailable",
    }
}

/// Center `text` on row `dy` of `rect`.
fn center(frame: &mut Frame, rect: Rect, dy: u16, text: &str, style: Style) {
    if dy >= rect.h {
        return;
    }
    let w = u16::try_from(text.chars().count()).unwrap_or(0);
    let x = rect.x + rect.w.saturating_sub(w) / 2;
    let clipped: String = text.chars().take(usize::from(rect.w)).collect();
    frame.put_str(x, rect.y + dy, &clipped, style);
}

/// Human-readable on-disk size (`B` / `KB` / `MB`).
fn fmt_bytes(bytes: u64) -> String {
    let b = bytes as f64;
    if b >= 1_048_576.0 {
        format!("{:.1}MB", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.0}KB", b / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use super::super::fixtures::{draw_at, draw_to, panel_text, text_of};
    use super::super::{NicReading, SectionId};

    /// **The System panel says when wakes are poll-only**:
    /// `PollOnly` adds the dim "watch: poll-only" row (and the collapsed-head
    /// suffix); `Native` draws nothing, so every earlier golden is unchanged.
    #[test]
    fn the_system_panel_says_when_wakes_are_poll_only() {
        let native = Col3::default();
        let t = panel_text(&native, SectionId::System, 40, 12, false);
        assert!(!t.contains("poll-only"), "{t}");
        let t = panel_text(&native, SectionId::System, 60, 3, true);
        assert!(!t.contains("poll-only"), "{t}");

        let degraded = Col3 {
            watch: crate::state::WatchMode::PollOnly,
            ..Col3::default()
        };
        let t = panel_text(&degraded, SectionId::System, 40, 12, false);
        assert!(t.contains("watch: poll-only"), "{t}");
        // The collapsed head carries the short suffix (a 56-column frame: the
        // head is one line and the suffix rides after the sessions count).
        let t = panel_text(&degraded, SectionId::System, 60, 3, true);
        assert!(t.contains("·  poll-only"), "{t}");
    }

    #[test]
    fn system_panel_donut_bands_and_unknown_indicator() {
        // The donut arc coverage is monotonic in pct, and the band colour
        // changes across the gauge crate's boundaries (49/50, 94/95).
        assert!(plot::donut_points(50, 200).len() > plot::donut_points(20, 200).len());
        assert!(plot::donut_points(95, 200).len() >= plot::donut_points(94, 200).len());
        // The band colour steps at the gauge crate's boundaries (49→Clear /
        // 50→Moderate, 94→Very High / 95→Critical); the correspondence to
        // `Band::from_pct` is proven in the gauge crate's own tests.
        assert_ne!(theme::band_color("Clear"), theme::band_color("Moderate"));
        assert_ne!(
            theme::band_color("Very High"),
            theme::band_color("Critical")
        );
        // The unknown-model indicator renders explicitly, never a percentage.
        let col3 = Col3 {
            nic: NicView::UnknownModel {
                model: "mystery".into(),
                active_sessions: 1,
            },
            ..Col3::default()
        };
        let text = text_of(&draw_to(&col3, 40, 24));
        assert!(
            text.contains("unknown"),
            "the unknown indicator shows: \n{text}"
        );
        assert!(
            !text.contains('%'),
            "no invented percentage for an unknown model"
        );
    }

    #[test]
    fn system_metrics_render_placeholders_for_absent_sources() {
        let col3 = Col3 {
            metrics: PanelMetrics {
                events_5m: Some(3),
                tokens_5m: None,
                projection_bytes: Some(4096),
                serve_calls_5m: None,
                activity_scope: SystemScope::CurrentSessions,
                ..PanelMetrics::default()
            },
            ..Col3::default()
        };
        let text = text_of(&draw_to(&col3, 40, 24));
        assert!(text.contains("events · 5m: 3"));
        assert!(
            text.contains("tokens · 5m: —"),
            "an absent metric is an honest dash"
        );
        assert!(text.contains("projection storage: 4KB"));
        assert!(text.contains("serves · 5m: —"));
    }

    #[test]
    fn system_panel_hints_the_activity_scope() {
        let live = Col3 {
            metrics: PanelMetrics {
                events_5m: Some(3),
                activity_scope: SystemScope::CurrentSessions,
                ..PanelMetrics::default()
            },
            ..Col3::default()
        };
        // 52, not 33. The hint is the System panel's LAST row, so this test
        // needs the panel at the 10 rows a 33-row three-panel column gave it:
        // seating raised the stack minimum by 15 and re-weighted the surplus
        // five ways, and 52 is the height that restores those two rows.
        let text = text_of(&draw_to(&live, 40, 52));
        assert!(
            text.contains("scope - current sessions"),
            "the current hint shows:\n{text}"
        );

        let held = Col3 {
            metrics: PanelMetrics {
                events_5m: Some(3),
                activity_scope: SystemScope::LastCompletedSession,
                ..PanelMetrics::default()
            },
            ..Col3::default()
        };
        let text = text_of(&draw_to(&held, 40, 52));
        assert!(
            text.contains("scope - last completed session"),
            "the held hint shows:\n{text}"
        );
        // The held rates themselves still show (a stale marker, not absence).
        assert!(text.contains("events · 5m: 3"));
    }

    #[test]
    fn held_context_is_dimmed_and_labelled_previous_session() {
        let col3 = Col3 {
            nic: NicView::Reading(NicReading {
                pct: 25,
                band: "Clear",
                model: "claude-fable-5".into(),
                active_sessions: 0,
            }),
            metrics: PanelMetrics {
                context_used: Some(50_000),
                context_window: Some(200_000),
                context_scope: SystemScope::LastCompletedSession,
                ..PanelMetrics::default()
            },
            ..Col3::default()
        };
        // 48, not 30: seating raised the stack's expanded minimum by 15 rows
        // (7 + 8) and the Recall panel's own content by 3 more, and this test
        // is about the SYSTEM panel's last row, not about what a short column
        // sheds.
        let text = text_of(&draw_to(&col3, 48, 48));
        assert!(text.contains("previous session"), "held label:\n{text}");
        assert!(text.contains("0 active sessions"));
    }

    #[test]
    fn active_session_badge_pluralizes_honestly() {
        assert_eq!(active_sessions_label(1), "1 active session");
        assert_eq!(active_sessions_label(2), "2 active sessions");
    }

    #[test]
    fn system_panel_too_short_for_the_hint_degrades_to_rows_only() {
        // A short panel drops the hint row, never a metric row.
        let col3 = Col3 {
            metrics: PanelMetrics {
                events_5m: Some(3),
                activity_scope: SystemScope::CurrentSessions,
                ..PanelMetrics::default()
            },
            ..Col3::default()
        };
        // Window the fully-expanded stack directly onto the System panel. Its
        // eight-row minimum leaves six inner rows: enough for all four metric
        // rows, deliberately too short for the optional scope hint.
        let text = text_of(&draw_at(&col3, 40, 8, 9));
        assert!(text.contains("events · 5m: 3"), "rows survive:\n{text}");
        assert!(
            !text.contains("scope -"),
            "the optional hint is absent:\n{text}"
        );
    }
}
