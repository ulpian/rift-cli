//! Column 3 — the three live panels (M55-11/12/13, ADR-0024 §4): the M4 six
//! sections regroup into **token usage** (the four-class categorical chart + Σ
//! total + read fraction + five class-coloured stat tiles), **system** (the
//! dumbometer donut + four domain metrics), and **recall** (savings % + tokens
//! avoided, MCP calls, threads). None of the M4 data is dropped — the token
//! time-series moves into the burn-rate metric, the breakdown becomes the
//! chart+tiles, nic becomes the donut, savings/calls/threads compose recall.
//!
//! Each panel is an individually-bordered themed card, sized by the shared
//! [`crate::stack`] solver and **honest about absence** (user stories 5/9): a
//! data-less panel still occupies its slot with its placeholder — "no token
//! activity yet", the recall enable-prompt vs the visibly distinct
//! enabled-but-quiet state, the unknown-model indicator — never a fake zero.
//! Under height pressure a panel collapses to its one-line headline (fidelity
//! is lost, information is not). The donut rasterizes via the hi-res sub-cell
//! tier on SUBCELL-capable backends and degrades to the shipped horizontal
//! gauge bar elsewhere; its formula/bands stay owned by `rift-nic-gauge`
//! (ADR-0012).
//!
//! The types here are **filled by the data plane** (`rift-canopy-data`, which
//! imports this crate — never the reverse, ADR-0009) and rendered pure over
//! the [`crate::state::ViewModel`].

use crate::layout::Rect;
use crate::plot::{self, Paint};
use crate::render::frame::{Color, Frame, Style};
use crate::render::{Capabilities, RenderBackend};
use crate::theme::{self, Icon, Section, Theme};

/// The three column-3 panels, in their fixed display order (M55-11/12/13,
/// ADR-0024 §4 — the M4 six sections regroup here, none of the data dropped).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionId {
    /// Token usage: the four-class categorical chart + Σ total + the read
    /// fraction + five class-coloured stat tiles.
    TokenUsage,
    /// System: the dumbometer donut + the four domain metrics.
    System,
    /// Recall: savings % + tokens avoided, MCP calls, threads.
    Recall,
}

/// How reluctantly a section collapses under height pressure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Collapses first.
    Low,
    /// Collapses after every Low section.
    Medium,
    /// Collapses last.
    High,
}

/// One section's registration: id, height weight, expanded minimum, priority.
#[derive(Clone, Copy, Debug)]
pub struct SectionSpec {
    /// Which section.
    pub id: SectionId,
    /// Relative share of surplus height (all sections at their minimums first).
    pub weight: u16,
    /// The smallest useful expanded height (title + content).
    pub min_rows: u16,
    /// Collapse order under pressure.
    pub priority: Priority,
}

impl SectionSpec {
    /// This section's sizing as a generic [`crate::stack::Spec`].
    fn to_stack(self) -> crate::stack::Spec {
        crate::stack::Spec {
            weight: self.weight,
            min_rows: self.min_rows,
            priority: match self.priority {
                Priority::Low => crate::stack::Priority::Low,
                Priority::Medium => crate::stack::Priority::Medium,
                Priority::High => crate::stack::Priority::High,
            },
        }
    }
}

/// The registration table — the **single** place the panel order, weights, and
/// collapse priorities are defined (M55-11/12/13): token usage (the chart +
/// tiles) and system (the donut) hold out longest; recall folds first.
pub const SECTIONS: [SectionSpec; 3] = [
    SectionSpec {
        id: SectionId::TokenUsage,
        weight: 3,
        min_rows: 7,
        priority: Priority::High,
    },
    SectionSpec {
        id: SectionId::System,
        weight: 3,
        min_rows: 8,
        priority: Priority::High,
    },
    SectionSpec {
        id: SectionId::Recall,
        weight: 2,
        min_rows: 5,
        priority: Priority::Medium,
    },
];

/// One solved slot: where a section renders and whether it collapsed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionSlot {
    /// Which section.
    pub id: SectionId,
    /// Row offset within the column body.
    pub y: u16,
    /// Rows granted (1 when collapsed).
    pub rows: u16,
    /// Collapsed to a one-line summary.
    pub collapsed: bool,
}

/// Distribute `height` rows across the six sections via the shared
/// [`crate::stack`] solver (the same contract the column-2 card stack uses,
/// M55-08): everything expanded when the minimums fit; under pressure Low then
/// Medium then High collapse to one-line headlines; below six rows the first
/// `height` render collapsed and the tail drops. Maps the generic index slots
/// back onto the fixed [`SectionId`] order.
#[must_use]
pub fn solve_stack(height: u16) -> Vec<SectionSlot> {
    let specs: Vec<crate::stack::Spec> = SECTIONS.iter().map(|s| s.to_stack()).collect();
    crate::stack::solve(&specs, height)
        .into_iter()
        .filter_map(|slot| {
            SECTIONS.get(slot.index).map(|spec| SectionSlot {
                id: spec.id,
                y: slot.y,
                rows: slot.rows,
                collapsed: slot.collapsed,
            })
        })
        .collect()
}

/// The panel stack's **fully-expanded** height (every panel at its `min_rows`,
/// none collapsed) — the content length the graphs viewport scrolls over
/// (ADR-0031). When the column is shorter than this, `scroll.graphs` moves the
/// window through the expanded panels instead of collapsing them.
#[must_use]
pub fn content_rows() -> u16 {
    SECTIONS
        .iter()
        .map(|s| s.min_rows)
        .fold(0, u16::saturating_add)
}

/// The largest in-range graphs scroll offset for a column of `height` rows: 0
/// when the expanded panels fit, else the overflow. Content-aware clamping reads
/// this so the offset can never scroll past the end.
#[must_use]
pub fn max_scroll(height: u16) -> usize {
    usize::from(content_rows().saturating_sub(height))
}

// --- the section view-models (filled by the data plane) ---------------------

/// Token spend over time.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum TokensView {
    /// No token facts exist in scope — never a fake zero line.
    #[default]
    NoActivity,
    /// Live data.
    Data(TokensData),
}

/// The token-usage section's numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct TokensData {
    /// Tokens per bucket (oldest first), for the sparkline/bars.
    pub rate: Vec<f64>,
    /// Cumulative total across the visible history.
    pub total: f64,
}

/// The input | read | output breakdown ("read" = cache reads — the PRD
/// resolves the label question; cache writes are the secondary band).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum BreakdownView {
    /// No token facts exist in scope.
    #[default]
    NoActivity,
    /// Live data.
    Data(BreakdownData),
}

/// The four-class totals + the read fraction.
#[derive(Clone, Debug, PartialEq)]
pub struct BreakdownData {
    /// Uncached input tokens.
    pub input: f64,
    /// Cached-read input tokens.
    pub read: f64,
    /// Output tokens.
    pub output: f64,
    /// Cache-creation tokens (the secondary band).
    pub cache_write: f64,
    /// read / (input + read), `0/0 ≡ 0` (user story 3).
    pub read_fraction: f64,
}

/// The nic-gauge section.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum NicView {
    /// No model calls in scope yet.
    #[default]
    NoData,
    /// The model's window is unknown — an explicit indicator, never a
    /// defaulted percentage (user story 5).
    UnknownModel {
        /// The model id whose window is missing from the reference table.
        model: String,
        /// Distinct active sessions (the thread badge).
        threads: u16,
    },
    /// A measured reading.
    Reading(NicReading),
}

/// A measured context reading for the freshest thread.
#[derive(Clone, Debug, PartialEq)]
pub struct NicReading {
    /// Percent of the window in use (0..=100).
    pub pct: u8,
    /// The band label (Smart/Capable/Thinking/Strained/Dumb).
    pub band: &'static str,
    /// The model the reading is for.
    pub model: String,
    /// Distinct active sessions (the thread badge; the same query as the
    /// Threads section by construction — user story 6).
    pub threads: u16,
}

/// A recall-derived section: the three honest states (user story 9).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum RecallView<T> {
    /// Recall is not enabled for this project — prompt, never a zero line.
    #[default]
    Disabled,
    /// Enabled but no served events yet — visibly distinct from Disabled.
    Quiet,
    /// Live data.
    Data(T),
}

/// Recall savings numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct SavingsData {
    /// Tokens avoided per bucket (oldest first).
    pub rate: Vec<f64>,
    /// Cumulative tokens avoided.
    pub total: f64,
    /// The savings rate: avoided / (avoided + served), `0/0 ≡ 0` (M55-07). A
    /// headline (never tweened) — recall's value as a rate, not just a counter
    /// (user story 26).
    pub pct: f64,
}

/// Recall MCP call numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct CallsData {
    /// Calls per bucket (oldest first).
    pub rate: Vec<f64>,
    /// Total calls across the visible history.
    pub total: f64,
}

/// The threads section: distinct active sessions in the recent window.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ThreadsView {
    /// The session cache shows nothing live.
    #[default]
    NoSessions,
    /// Live count.
    Count(u16),
}

/// The four **domain** metrics of the system panel (M55-07/12, ADR-0024 §4):
/// each honest about absence — a missing projection or disabled recall renders
/// its placeholder, never a fake zero. Domain facts about *Rift*, not host
/// telemetry.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PanelMetrics {
    /// Capture rate — captured events per minute over the recent window
    /// (`None` = no readable projection).
    pub capture_per_min: Option<f64>,
    /// Tokens burn rate — tokens per minute over the recent window (`None` =
    /// no token facts).
    pub tokens_per_min: Option<f64>,
    /// Projection size — on-disk bytes of the capture + recall projections
    /// (`None` = no projection on disk).
    pub projection_bytes: Option<u64>,
    /// Recall load — MCP serves per hour from the audit trail (`None` = recall
    /// disabled / no recall.db).
    pub recall_per_hour: Option<f64>,
}

/// A single trailing-week day of activity: one grouped bar of the three capture
/// categories, with its weekday label (M55-06/09).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DayBar {
    /// The short weekday label ("Mon".."Sun"), computed from the injected clock.
    pub weekday: &'static str,
    /// Captured events on this calendar day.
    pub events: u64,
    /// Sessions started on this calendar day.
    pub sessions: u64,
    /// Checkpoints created on this calendar day.
    pub checkpoints: u64,
}

/// The activity card's data: the trailing seven calendar days, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActivityData {
    /// Seven days of grouped bars (oldest → today). An absent day is a
    /// zero-count day (a fact), never dropped.
    pub days: Vec<DayBar>,
}

/// The activity card view-model (M55-06). Honest about absence: a **missing
/// projection** is the placeholder (never seven zeros); a readable projection
/// with quiet days is `Days` with zero-count bars.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ActivityView {
    /// No readable projection — the placeholder (distinct from a quiet week).
    #[default]
    NoProjection,
    /// The trailing week's grouped daily bars.
    Days(ActivityData),
}

/// The whole column-3 view-model the data plane fills (Arc-shared through the
/// snapshot; `Default` is every section's honest placeholder).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Col3 {
    /// Token usage.
    pub tokens: TokensView,
    /// Input | read | output.
    pub breakdown: BreakdownView,
    /// The nic-gauge.
    pub nic: NicView,
    /// Recall savings.
    pub savings: RecallView<SavingsData>,
    /// Recall MCP calls.
    pub calls: RecallView<CallsData>,
    /// Threads.
    pub threads: ThreadsView,
    /// The system panel's four domain metrics (M55-07). Headlines — taken from
    /// the target immediately, never tweened.
    pub metrics: PanelMetrics,
}

// --- animation: tween the plot series on a data change -----------------------
//
// Only the *plottable series* interpolate (bar/line heights); every scalar
// headline and enum variant is taken from the target immediately, so a mid-tween
// read still shows the true total/band/count and a fresh headless render (which
// never animates) is always the settled target. A section whose variant changed
// (placeholder↔Data) snaps rather than tweens.
//
// The **nic-gauge does not tween**: its `pct` is *both* the gauge fill and the
// title headline (`"{pct}% {band}"`), so easing it would desync the number from
// its band (e.g. show "80% Smart" mid-transition). The meter snaps to each
// reading — consistent, and standard for a small horizontal gauge.

/// Linear interpolation of two `f64`s at `t ∈ [0, 1]`.
fn lerp_f64(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

/// Interpolate two equal-length series element-wise; `None` when the lengths
/// differ (the caller snaps to the target rather than guess an alignment).
fn lerp_series(a: &[f64], b: &[f64], t: f64) -> Option<Vec<f64>> {
    if a.len() != b.len() {
        return None;
    }
    Some(a.iter().zip(b).map(|(x, y)| lerp_f64(*x, *y, t)).collect())
}

/// Interpolate a `RecallView<T>` only when both sides carry data; otherwise the
/// target wins (the placeholder↔data transition snaps).
fn interp_recall<T: Clone>(
    from: &RecallView<T>,
    to: &RecallView<T>,
    blend: impl Fn(&T, &T) -> T,
) -> RecallView<T> {
    match (from, to) {
        (RecallView::Data(a), RecallView::Data(b)) => RecallView::Data(blend(a, b)),
        _ => to.clone(),
    }
}

/// Would tweening `from → to` show any motion? True iff at least one section is a
/// same-variant `Data` change with an interpolable (equal-length) series or a
/// changed value — so a pure placeholder↔data update snaps instead.
#[must_use]
pub(crate) fn should_animate(from: &Col3, to: &Col3) -> bool {
    let series_changed = |a: &[f64], b: &[f64]| a.len() == b.len() && a != b;
    // Nic is deliberately excluded — it snaps (its pct is a headline; see above).
    matches!((&from.tokens, &to.tokens),
        (TokensView::Data(a), TokensView::Data(b)) if series_changed(&a.rate, &b.rate))
        || matches!((&from.breakdown, &to.breakdown),
            (BreakdownView::Data(a), BreakdownView::Data(b)) if a != b)
        || matches!((&from.savings, &to.savings),
            (RecallView::Data(a), RecallView::Data(b)) if series_changed(&a.rate, &b.rate))
        || matches!((&from.calls, &to.calls),
            (RecallView::Data(a), RecallView::Data(b)) if series_changed(&a.rate, &b.rate))
}

/// The displayed `Col3` at tween progress `t ∈ [0, 1]`: `from`'s series eased
/// toward `to`'s, with all headlines/variants taken from `to`.
#[must_use]
pub(crate) fn interpolate(from: &Col3, to: &Col3, t: f32) -> Col3 {
    let t = f64::from(t.clamp(0.0, 1.0));
    Col3 {
        tokens: match (&from.tokens, &to.tokens) {
            (TokensView::Data(a), TokensView::Data(b)) => TokensView::Data(TokensData {
                rate: lerp_series(&a.rate, &b.rate, t).unwrap_or_else(|| b.rate.clone()),
                total: b.total,
            }),
            _ => to.tokens.clone(),
        },
        breakdown: match (&from.breakdown, &to.breakdown) {
            (BreakdownView::Data(a), BreakdownView::Data(b)) => {
                BreakdownView::Data(BreakdownData {
                    input: lerp_f64(a.input, b.input, t),
                    read: lerp_f64(a.read, b.read, t),
                    output: lerp_f64(a.output, b.output, t),
                    cache_write: lerp_f64(a.cache_write, b.cache_write, t),
                    read_fraction: b.read_fraction,
                })
            }
            _ => to.breakdown.clone(),
        },
        // The nic-gauge snaps: its pct is a headline (title + fill share it).
        nic: to.nic.clone(),
        savings: interp_recall(&from.savings, &to.savings, |a, b| SavingsData {
            rate: lerp_series(&a.rate, &b.rate, t).unwrap_or_else(|| b.rate.clone()),
            total: b.total,
            pct: b.pct, // a headline: the savings % is taken from the target
        }),
        calls: interp_recall(&from.calls, &to.calls, |a, b| CallsData {
            rate: lerp_series(&a.rate, &b.rate, t).unwrap_or_else(|| b.rate.clone()),
            total: b.total,
        }),
        threads: to.threads.clone(),
        // Domain metrics are headlines — taken from the target immediately.
        metrics: to.metrics.clone(),
    }
}

// --- rendering ---------------------------------------------------------------

/// Compact K/M formatting for token-scale numbers (PRD "numbers format with
/// K/M suffixes").
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn fmt_km(v: f64) -> String {
    let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 10_000.0 {
        format!("{:.0}K", v / 1_000.0)
    } else if v >= 1_000.0 {
        format!("{:.1}K", v / 1_000.0)
    } else {
        format!("{}", v.round() as u64)
    }
}

/// Render the three column-3 panels into `area` through the plot seam (M55-11/
/// 12/13). Each panel is an individually-bordered themed card, sized by the
/// shared stack solver; `focused` accents the borders when the graphs column
/// holds focus; `unicode` drives glyph substitution. Pure over the view-model
/// and safe at any size.
pub fn draw(
    col3: &Col3,
    focused: bool,
    unicode: bool,
    area: Rect,
    offset: usize,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let content_h = content_rows();
    if content_h <= area.h || area.w == 0 || area.h == 0 {
        // The expanded panels fit: solve at the column height (surplus spreads),
        // draw straight in. `scroll.graphs` clamps to 0 — nothing to scroll.
        draw_stack(col3, focused, unicode, area, backend, frame);
        return;
    }
    // Overflow (ADR-0031): render the fully-expanded stack into a scratch frame,
    // then blit the [offset, offset+viewport) window — so "j/k scroll" moves the
    // panels instead of collapsing them, with the scrollbar (M57-06) as its sign.
    let mut scratch = Frame::new(crate::render::frame::Size::new(area.w, content_h));
    draw_stack(
        col3,
        focused,
        unicode,
        scratch.rect(),
        backend,
        &mut scratch,
    );
    let off = offset.min(usize::from(content_h.saturating_sub(area.h)));
    frame.blit_window(&scratch, area, off);
}

/// Draw the three panels straight into `area` at the solver's sizing (the
/// non-scrolled path; also the scratch-frame body for the scrolled path). Solving
/// at `area.h` = [`content_rows`] expands every panel to its `min_rows`.
fn draw_stack(
    col3: &Col3,
    focused: bool,
    unicode: bool,
    area: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let theme = Theme::rift_dark();
    for slot in solve_stack(area.h) {
        let rect = Rect {
            x: area.x,
            y: area.y + slot.y,
            w: area.w,
            h: slot.rows,
        };
        if rect.h == 0 || rect.w == 0 {
            continue;
        }
        match slot.id {
            SectionId::TokenUsage => {
                draw_token_panel(
                    col3,
                    theme,
                    focused,
                    unicode,
                    slot.collapsed,
                    rect,
                    backend,
                    frame,
                );
            }
            SectionId::System => {
                draw_system_panel(
                    col3,
                    theme,
                    focused,
                    unicode,
                    slot.collapsed,
                    rect,
                    backend,
                    frame,
                );
            }
            SectionId::Recall => {
                draw_recall_panel(col3, theme, focused, slot.collapsed, rect, frame);
            }
        }
    }
}

/// A panel border + title (with the Σ / headline appended), returning the inner
/// content rect.
fn panel_frame(
    theme: Theme,
    section: Section,
    focused: bool,
    title: &str,
    rect: Rect,
    frame: &mut Frame,
) -> Rect {
    crate::chrome::panel(
        frame,
        rect,
        title,
        theme.border(section, focused),
        theme.title(section),
    )
}

/// One body line inside a panel's inner rect (dropped when below it).
fn body(frame: &mut Frame, inner: Rect, dy: u16, text: &str, style: Style) {
    if dy >= inner.h {
        return;
    }
    let clipped: String = text.chars().take(usize::from(inner.w)).collect();
    frame.put_str(inner.x, inner.y + dy, &clipped, style);
}

// --- token-usage panel (M55-11) ----------------------------------------------

fn draw_token_panel(
    col3: &Col3,
    theme: Theme,
    focused: bool,
    unicode: bool,
    collapsed: bool,
    rect: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    // Σ total in the title (from the token totals); the chart + tiles from the
    // four-class breakdown. They arrive together, so the breakdown gates data.
    let total = match &col3.tokens {
        TokensView::Data(d) => Some(d.total),
        TokensView::NoActivity => None,
    };
    let title = total.map_or_else(
        || "token usage".to_string(),
        |t| format!("token usage  Σ {}", fmt_km(t)),
    );
    let inner = panel_frame(theme, Section::TokenUsage, focused, &title, rect, frame);
    let BreakdownView::Data(d) = &col3.breakdown else {
        body(frame, inner, 0, "(no token activity yet)", theme.dim());
        return;
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let read_pct = (d.read_fraction * 100.0).round() as u8;
    if collapsed {
        body(
            frame,
            inner,
            0,
            &format!(
                "in {} rd {} out {} · read {read_pct}%",
                fmt_km(d.input),
                fmt_km(d.read),
                fmt_km(d.output)
            ),
            theme.body(),
        );
        return;
    }
    // The categorical class chart in the upper portion.
    let vals = [d.input, d.read, d.output, d.cache_write];
    let tiles_rows = 3u16;
    if inner.h > tiles_rows + 1 {
        let chart = Rect {
            x: inner.x,
            y: inner.y,
            w: inner.w,
            h: inner.h - tiles_rows,
        };
        let max = plot::autoscale_max(&vals);
        // The axis frame (ADR-0030): a humanized Y-token gutter, the four class
        // labels on X, and the class-colour legend — single-sourced with the
        // tiles via `theme::BREAKDOWN`. The legend drops first under vertical
        // pressure (the tiles below already name the classes), then the axis.
        let x_labels = ["in".to_string(), "rd".into(), "out".into(), "ca".into()];
        let legend_full = [
            (theme::BREAKDOWN[0], "input"),
            (theme::BREAKDOWN[1], "read"),
            (theme::BREAKDOWN[2], "output"),
            (theme::BREAKDOWN[3], "cache"),
        ];
        let legend: &[(Color, &str)] = if chart.h >= 4 { &legend_full } else { &[] };
        let plot_rect = if chart.h >= 3 {
            let spec = crate::axis::AxisSpec {
                max,
                humanize: fmt_km,
                x_labels: &x_labels,
                legend,
            };
            crate::axis::draw(frame, theme, chart, &spec, unicode)
        } else {
            chart
        };
        if plot_rect.h > 0 && plot_rect.w > 0 {
            let pts = plot::bar_points(&vals, max, plot_rect.h.saturating_mul(4).max(4));
            backend.plot(
                &pts,
                plot_rect,
                &Paint::ByCategory(&theme::BREAKDOWN),
                frame,
            );
        }
    }
    // Five stat tiles on the bottom rows: the four classes + the read fraction,
    // each bordered in its class colour (the read tile shows the 0/0 = 0 case).
    let tiles_top = inner.h.saturating_sub(tiles_rows);
    draw_class_tiles(
        theme,
        d,
        read_pct,
        Rect {
            y: inner.y + tiles_top,
            h: tiles_rows,
            ..inner
        },
        frame,
    );
}

/// The five class-coloured stat tiles (headline form): input · read · output ·
/// read-% · cache. Drawn as one-line labelled values so five fit a narrow
/// column (the tile primitive's collapsed form).
fn draw_class_tiles(theme: Theme, d: &BreakdownData, read_pct: u8, rect: Rect, frame: &mut Frame) {
    let cats = &theme::BREAKDOWN;
    let cell = |i: usize| cats.get(i).copied().unwrap_or(theme::ACCENT);
    let tiles: [(&str, String, Color); 5] = [
        ("in", fmt_km(d.input), cell(0)),
        ("rd", fmt_km(d.read), cell(1)),
        ("out", fmt_km(d.output), cell(2)),
        ("read%", format!("{read_pct}"), cell(1)),
        ("cache", fmt_km(d.cache_write), cell(3)),
    ];
    // Lay the five tiles across the available width, one row each if the column
    // is narrow, else side by side.
    let per_row = if rect.w >= 40 { 5 } else { 1 };
    let tw = rect.w / u16::try_from(per_row).unwrap_or(1).max(1);
    for (i, (label, value, colour)) in tiles.into_iter().enumerate() {
        let Ok(iu) = u16::try_from(i) else { break };
        let (cx, cy) = if per_row == 5 {
            (rect.x + iu * tw, rect.y)
        } else {
            (rect.x, rect.y + iu)
        };
        if cy >= rect.y + rect.h {
            break;
        }
        crate::chrome::tile_headline(
            frame,
            cx,
            cy,
            tw,
            label,
            &value,
            theme.dim(),
            theme.tile_value(colour),
        );
    }
}

// --- system panel: the donut + the domain metrics (M55-12) -------------------

fn draw_system_panel(
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
    if collapsed {
        let head = match &col3.nic {
            NicView::Reading(r) => format!("{}% {}  ·  {} threads", r.pct, r.band, r.threads),
            NicView::UnknownModel { threads, .. } => format!("? unknown  ·  {threads} threads"),
            NicView::NoData => "(no model calls yet)".to_string(),
        };
        body(frame, inner, 0, &head, theme.body());
        return;
    }
    // The donut occupies the top; the four domain metrics the bottom four rows.
    let metric_rows = 4u16;
    let gauge_h = inner.h.saturating_sub(metric_rows);
    let gauge_rect = Rect {
        x: inner.x,
        y: inner.y,
        w: inner.w,
        h: gauge_h,
    };
    draw_dumbometer(&col3.nic, theme, unicode, gauge_rect, backend, frame);
    // The metric rows below.
    let m = &col3.metrics;
    let rows = [
        ("capture", m.capture_per_min.map(|v| format!("{v:.1}/min"))),
        (
            "burn",
            m.tokens_per_min.map(|v| format!("{}/min", fmt_km(v))),
        ),
        ("projection", m.projection_bytes.map(fmt_bytes)),
        ("recall", m.recall_per_hour.map(|v| format!("{v:.1}/hr"))),
    ];
    let base = inner.h.saturating_sub(metric_rows);
    for (i, (label, value)) in rows.into_iter().enumerate() {
        let Ok(iu) = u16::try_from(i) else { break };
        let dy = base + iu;
        let dot = Icon::Bullet.glyph(unicode);
        frame.set(inner.x, inner.y + dy, dot, theme.dim());
        let text = value.map_or_else(|| format!(" {label}: —"), |v| format!(" {label}: {v}"));
        let style = if value_present(&text) {
            theme.body()
        } else {
            theme.dim()
        };
        body(
            frame,
            Rect {
                x: inner.x + 1,
                ..inner
            },
            dy,
            &text,
            style,
        );
    }
}

/// Whether a metric row carries a real value (not the `—` placeholder).
fn value_present(text: &str) -> bool {
    !text.ends_with('—')
}

/// Draw the dumbometer: a radial donut on SUBCELL-capable backends, degrading
/// to the shipped horizontal gauge bar elsewhere; the unknown-model indicator
/// and the no-data placeholder render explicitly (never a percentage; ADR-0012).
fn draw_dumbometer(
    nic: &NicView,
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
        NicView::NoData => body(frame, rect, rect.h / 2, "(no model calls yet)", theme.dim()),
        NicView::UnknownModel { model, threads } => {
            let q = Icon::CheckPending.glyph(unicode);
            body(
                frame,
                rect,
                rect.h / 2,
                &format!("{q} unknown window: {model}"),
                theme.dim(),
            );
            body(
                frame,
                rect,
                rect.h / 2 + 1,
                &format!("[{threads} threads]"),
                theme.body(),
            );
        }
        NicView::Reading(r) => {
            let colour = theme::band_color(r.band);
            let subcell = backend.capabilities().contains(Capabilities::SUBCELL);
            if subcell && rect.h >= 5 {
                // The radial arc via the plot seam, centered.
                let samples = rect
                    .w
                    .saturating_mul(rect.h)
                    .saturating_mul(2)
                    .clamp(16, 720);
                let pts = plot::donut_points(r.pct, samples);
                backend.plot(&pts, rect, &Paint::Solid(Style::fg(colour)), frame);
                // Centered percent + band; the thread badge below.
                center(
                    frame,
                    rect,
                    rect.h / 2,
                    &format!("{}%", r.pct),
                    theme.tile_value(colour),
                );
                center(frame, rect, rect.h / 2 + 1, r.band, theme.dim());
                center(
                    frame,
                    rect,
                    rect.h.saturating_sub(1),
                    &format!("[{} threads]", r.threads),
                    theme.body(),
                );
            } else {
                // Degrade: the shipped horizontal gauge bar.
                body(
                    frame,
                    rect,
                    0,
                    &format!("{}% {}  [{} threads]", r.pct, r.band, r.threads),
                    theme.body(),
                );
                if rect.h > 1 {
                    let bar = Rect {
                        x: rect.x,
                        y: rect.y + 1,
                        w: rect.w,
                        h: rect.h - 1,
                    };
                    let pts = plot::gauge_points(
                        f32::from(r.pct) / 100.0,
                        bar.w.saturating_mul(2).max(2),
                        bar.h.saturating_mul(4).clamp(1, 4),
                    );
                    backend.plot(&pts, bar, &Paint::HRamp(&theme::GAUGE_RAMP), frame);
                }
            }
        }
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
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    if b >= 1_048_576.0 {
        format!("{:.1}MB", b / 1_048_576.0)
    } else if b >= 1024.0 {
        format!("{:.0}KB", b / 1024.0)
    } else {
        format!("{bytes}B")
    }
}

// --- recall panel (M55-13) ---------------------------------------------------

fn draw_recall_panel(
    col3: &Col3,
    theme: Theme,
    focused: bool,
    collapsed: bool,
    rect: Rect,
    frame: &mut Frame,
) {
    let inner = panel_frame(theme, Section::Recall, focused, "recall", rect, frame);
    match &col3.savings {
        RecallView::Disabled => {
            body(
                frame,
                inner,
                0,
                "recall off — /toggle total-recall",
                theme.dim(),
            );
            return;
        }
        RecallView::Quiet => {
            body(
                frame,
                inner,
                0,
                "enabled, no serves yet — see `rift doctor`",
                theme.dim(),
            );
            return;
        }
        RecallView::Data(sd) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let pct = (sd.pct * 100.0).round() as u8;
            // Savings % beside the absolute tokens avoided.
            body(
                frame,
                inner,
                0,
                &format!("saved {pct}%  ({} tokens)", fmt_km(sd.total)),
                theme.tile_value(theme::ACCENT),
            );
        }
    }
    if collapsed {
        return;
    }
    // MCP calls + threads as dot-labelled rows.
    let calls = match &col3.calls {
        RecallView::Data(c) => format!("{} total", fmt_km(c.total)),
        _ => "—".to_string(),
    };
    body(
        frame,
        inner,
        1,
        &format!("· mcp calls: {calls}"),
        theme.body(),
    );
    let threads = match &col3.threads {
        ThreadsView::Count(n) => format!("{n} active"),
        ThreadsView::NoSessions => "none".to_string(),
    };
    body(
        frame,
        inner,
        2,
        &format!("· threads: {threads}"),
        theme.body(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities};

    /// Render the three panels into a `w`×`h` area (no outer border).
    fn draw_to(col3: &Col3, w: u16, h: u16) -> Frame {
        let mut f = Frame::new(Size::new(w, h));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw(col3, false, false, f.rect(), 0, &mut b, &mut f);
        f
    }

    fn draw_at(col3: &Col3, w: u16, h: u16, offset: usize) -> Frame {
        let mut f = Frame::new(Size::new(w, h));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw(col3, false, false, f.rect(), offset, &mut b, &mut f);
        f
    }

    fn text_of(f: &Frame) -> String {
        (0..f.height())
            .map(|y| {
                (0..f.width())
                    .map(|x| f.get(x, y).map_or(' ', |c| c.ch))
                    .collect::<String>()
                    + "\n"
            })
            .collect()
    }

    #[test]
    fn a_short_column_scrolls_the_expanded_panels() {
        // The scroll offset moves content (ADR-0031): below the expanded content
        // height (20), a different offset windows a different slice; a tall column
        // fits, so the offset is inert (the old code's no-op, now the fit path).
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 5.0],
                total: 22.0,
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 200.0,
                read: 800.0,
                output: 300.0,
                cache_write: 50.0,
                read_fraction: 0.8,
            }),
            ..Col3::default()
        };
        assert!(
            content_rows() > 12,
            "the fixture stack overflows a 12-row column"
        );
        let top = text_of(&draw_at(&col3, 34, 12, 0));
        let scrolled = text_of(&draw_at(&col3, 34, 12, 5));
        assert_ne!(top, scrolled, "the offset scrolls the panels");
        // A column taller than the expanded content ignores the offset.
        let fit_a = text_of(&draw_at(&col3, 34, 24, 0));
        let fit_b = text_of(&draw_at(&col3, 34, 24, 5));
        assert_eq!(fit_a, fit_b, "a fitting column has nothing to scroll");
        assert_eq!(max_scroll(24), 0);
        assert!(max_scroll(12) > 0);
    }

    #[test]
    fn order_is_the_three_panels_by_registration() {
        let ids: Vec<SectionId> = SECTIONS.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![SectionId::TokenUsage, SectionId::System, SectionId::Recall]
        );
    }

    #[test]
    fn tall_column_expands_all_three_panels() {
        let slots = solve_stack(40);
        assert_eq!(slots.len(), 3);
        assert!(slots.iter().all(|s| !s.collapsed));
        let mut y = 0;
        for s in &slots {
            assert_eq!(s.y, y);
            y += s.rows;
        }
        assert!(y <= 40);
    }

    #[test]
    fn short_column_collapses_recall_first_then_none_disappears() {
        // Recall is Medium (folds before the two High panels).
        let slots = solve_stack(18);
        assert_eq!(slots.len(), 3, "every panel keeps a slot");
        let recall = slots.iter().find(|s| s.id == SectionId::Recall).unwrap();
        assert!(recall.collapsed, "recall folds first under pressure");
        assert!(
            slots.iter().all(|s| s.rows >= 1),
            "a collapsed panel keeps its headline, never disappears"
        );
    }

    #[test]
    fn zero_height_is_empty() {
        assert!(solve_stack(0).is_empty());
    }

    #[test]
    fn token_panel_charts_classes_and_tiles_in_class_colours() {
        // The four-class categorical chart leaves RGB-coloured bar cells, and
        // the read tile shows the honest 0/0 = 0 empty case.
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 5.0],
                total: 22.0,
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 200.0,
                read: 800.0,
                output: 300.0,
                cache_write: 50.0,
                read_fraction: 0.8,
            }),
            ..Col3::default()
        };
        let f = draw_to(&col3, 40, 24);
        // The Σ total renders in the panel title.
        let text = text_of(&f);
        assert!(text.contains("Σ"), "the Σ total is in the title: \n{text}");
        // At least one categorical bar cell is RGB-coloured.
        let mut coloured = 0;
        for y in 0..f.height() {
            for x in 0..f.width() {
                if let Some(c) = f.get(x, y) {
                    if matches!(c.ch, '▀' | '▄' | '█') && matches!(c.style.fg, Color::Rgb(_, _, _))
                    {
                        coloured += 1;
                    }
                }
            }
        }
        assert!(coloured > 0, "the class chart drew no coloured bar cells");
    }

    #[test]
    fn read_fraction_zero_over_zero_renders_as_zero_not_error() {
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0],
                total: 5.0,
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 0.0,
                read: 0.0,
                output: 5.0,
                cache_write: 0.0,
                read_fraction: 0.0,
            }),
            ..Col3::default()
        };
        let text = text_of(&draw_to(&col3, 44, 24));
        assert!(
            text.contains("read% "),
            "the read tile shows a defined 0, not a dash"
        );
    }

    #[test]
    fn system_panel_donut_bands_and_unknown_indicator() {
        // The donut arc coverage is monotonic in pct, and the band colour
        // changes across the gauge crate's boundaries (49/50, 94/95).
        assert!(plot::donut_points(50, 200).len() > plot::donut_points(20, 200).len());
        assert!(plot::donut_points(95, 200).len() >= plot::donut_points(94, 200).len());
        // The band colour steps at the gauge crate's boundaries (49→Smart /
        // 50→Capable, 94→Strained / 95→Dumb); the correspondence to
        // `Band::from_pct` is proven in the data-plane gate leg (nic-gauge dep).
        assert_ne!(theme::band_color("Smart"), theme::band_color("Capable"));
        assert_ne!(theme::band_color("Strained"), theme::band_color("Dumb"));
        // The unknown-model indicator renders explicitly, never a percentage.
        let col3 = Col3 {
            nic: NicView::UnknownModel {
                model: "mystery".into(),
                threads: 1,
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
                capture_per_min: Some(2.5),
                tokens_per_min: None,
                projection_bytes: Some(4096),
                recall_per_hour: None,
            },
            ..Col3::default()
        };
        let text = text_of(&draw_to(&col3, 40, 24));
        assert!(text.contains("capture: 2.5/min"));
        assert!(
            text.contains("burn: —"),
            "an absent metric is an honest dash"
        );
        assert!(text.contains("projection: 4KB"));
        assert!(text.contains("recall: —"));
    }

    #[test]
    fn recall_panel_states_render_honestly() {
        // Disabled / Quiet / Data (savings % + calls + threads).
        let disabled = text_of(&draw_to(&Col3::default(), 40, 24));
        assert!(disabled.contains("recall off"));

        let data = Col3 {
            savings: RecallView::Data(SavingsData {
                rate: vec![1.0],
                total: 6000.0,
                pct: 0.42,
            }),
            calls: RecallView::Data(CallsData {
                rate: vec![1.0],
                total: 17.0,
            }),
            threads: ThreadsView::Count(3),
            ..Col3::default()
        };
        let text = text_of(&draw_to(&data, 40, 24));
        assert!(text.contains("saved 42%"), "savings % + tokens: \n{text}");
        assert!(text.contains("mcp calls: 17"));
        assert!(text.contains("threads: 3 active"));
    }

    #[test]
    fn interpolate_tweens_series_but_takes_headline_from_target() {
        let from = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![0.0, 0.0],
                total: 100.0,
            }),
            ..Col3::default()
        };
        let to = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![10.0, 20.0],
                total: 700.0,
            }),
            ..Col3::default()
        };
        let mid = interpolate(&from, &to, 0.5);
        match mid.tokens {
            TokensView::Data(d) => {
                assert_eq!(d.rate, vec![5.0, 10.0], "series is half-way");
                assert!(
                    (d.total - 700.0).abs() < f64::EPSILON,
                    "the headline total is taken from the target immediately"
                );
            }
            TokensView::NoActivity => panic!("expected Data"),
        }
    }

    #[test]
    fn interpolate_endpoints_are_exact() {
        let from = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 2.0],
                total: 3.0,
            }),
            ..Col3::default()
        };
        let to = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![9.0, 8.0],
                total: 17.0,
            }),
            ..Col3::default()
        };
        // At t=0 the *series* is the old shape but the *headline* is already the
        // target (totals never tween); at t=1 everything equals the target.
        match interpolate(&from, &to, 0.0).tokens {
            TokensView::Data(d) => {
                assert_eq!(d.rate, vec![1.0, 2.0]);
                assert!((d.total - 17.0).abs() < f64::EPSILON);
            }
            TokensView::NoActivity => panic!("expected Data"),
        }
        assert_eq!(interpolate(&from, &to, 1.0).tokens, to.tokens);
    }

    #[test]
    fn placeholder_to_data_snaps_not_tweens() {
        let from = Col3::default(); // NoActivity everywhere
        let to = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![5.0],
                total: 5.0,
            }),
            ..Col3::default()
        };
        assert!(
            !should_animate(&from, &to),
            "a placeholder→data update must snap"
        );
        // And interpolate at t=0 already shows the target (no blank tween frame).
        assert_eq!(interpolate(&from, &to, 0.0).tokens, to.tokens);
    }

    #[test]
    fn data_to_data_series_change_animates() {
        let from = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 2.0],
                total: 3.0,
            }),
            ..Col3::default()
        };
        let to = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![4.0, 5.0],
                total: 9.0,
            }),
            ..Col3::default()
        };
        assert!(should_animate(&from, &to));
        // A length change is not interpolable → snap that section.
        let to_len = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![4.0, 5.0, 6.0],
                total: 15.0,
            }),
            ..Col3::default()
        };
        assert!(!should_animate(&from, &to_len));
        assert_eq!(interpolate(&from, &to_len, 0.5).tokens, to_len.tokens);
    }

    #[test]
    fn nic_reading_change_snaps_and_never_tweens() {
        // The nic pct is a headline (title + gauge share it), so a reading change
        // must snap — never show an eased pct beside the target band.
        let reading = |pct, band| Col3 {
            nic: NicView::Reading(NicReading {
                pct,
                band,
                model: "m".into(),
                threads: 1,
            }),
            ..Col3::default()
        };
        let from = reading(80, "Strained");
        let to = reading(20, "Smart");
        assert!(
            !should_animate(&from, &to),
            "a nic reading change must not arm a tween"
        );
        // interpolate at any progress shows the TARGET reading verbatim — pct and
        // band always agree, never "80% Smart".
        for t in [0.0, 0.5, 1.0] {
            assert_eq!(interpolate(&from, &to, t).nic, to.nic);
        }
    }

    #[test]
    fn fmt_km_table() {
        assert_eq!(fmt_km(0.0), "0");
        assert_eq!(fmt_km(999.0), "999");
        assert_eq!(fmt_km(1_500.0), "1.5K");
        assert_eq!(fmt_km(25_000.0), "25K");
        assert_eq!(fmt_km(1_250_000.0), "1.2M"); // {:.1} rounds half-to-even
        assert_eq!(fmt_km(f64::NAN), "0");
        assert_eq!(fmt_km(-5.0), "0");
    }
}
