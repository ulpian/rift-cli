//! Column 3 — the three live panels (M55-11/12/13, ADR-0024 §4): the M4 six
//! sections regroup into **token usage** (the four-class categorical chart + Σ
//! total + read fraction + five class-coloured stat tiles), **system** (context
//! pressure + recent activity + projection storage), and **recall** (savings %
//! + tokens avoided and process-scoped MCP call threads).
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
    /// System: context pressure + recent activity + projection storage.
    System,
    /// Recall: savings % + tokens avoided and process-scoped MCP call threads.
    Recall,
}

impl SectionId {
    /// The panels in their fixed default display order (mirrors [`SECTIONS`]) —
    /// the default [`crate::layout_model::LayoutModel`]'s column-3 units.
    pub const ALL: [SectionId; 3] = [SectionId::TokenUsage, SectionId::System, SectionId::Recall];
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
        min_rows: 10,
        priority: Priority::Medium,
    },
];

/// A section's stack sizing as a generic [`crate::stack::Spec`] — the single
/// source, so a data-driven column (ADR-0033) sizes each panel exactly as the
/// fixed stack did.
#[must_use]
pub fn section_spec(id: SectionId) -> crate::stack::Spec {
    SECTIONS.iter().find(|s| s.id == id).map_or(
        crate::stack::Spec {
            weight: 1,
            min_rows: 3,
            priority: crate::stack::Priority::Medium,
        },
        |s| s.to_stack(),
    )
}

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
pub fn solve_stack(ids: &[SectionId], height: u16) -> Vec<SectionSlot> {
    let specs: Vec<crate::stack::Spec> = ids.iter().map(|id| section_spec(*id)).collect();
    crate::stack::solve(&specs, height)
        .into_iter()
        .filter_map(|slot| {
            ids.get(slot.index).map(|id| SectionSlot {
                id: *id,
                y: slot.y,
                rows: slot.rows,
                collapsed: slot.collapsed,
            })
        })
        .collect()
}

/// The panel stack's **fully-expanded** height for `ids` (every panel at its
/// `min_rows`, none collapsed) — the content length the graphs viewport scrolls
/// over (ADR-0031).
#[must_use]
pub fn content_rows(ids: &[SectionId]) -> u16 {
    ids.iter()
        .map(|id| section_spec(*id).min_rows)
        .fold(0, u16::saturating_add)
}

/// The largest in-range graphs scroll offset for a column of `height` rows: 0
/// when the expanded panels fit, else the overflow. Content-aware clamping reads
/// this so the offset can never scroll past the end.
#[must_use]
pub fn max_scroll(ids: &[SectionId], height: u16) -> usize {
    usize::from(content_rows(ids).saturating_sub(height))
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

/// One per-model row composing the USD savings estimate (M5.15 / ADR-0051):
/// an attributed slice of the avoided tokens priced at that model's
/// uncached-input rate. The rows are what the breakdown modal renders, so the
/// headline is explained by exactly the numbers that produced it.
///
/// Labels are pre-rendered by the one money authority (`rift-shadow-cost`,
/// via the data plane) — the render layer draws strings and never does money
/// math or formatting of its own.
#[derive(Clone, Debug, PartialEq)]
pub struct UsdRow {
    /// The model id (e.g. `claude-opus-4-8`).
    pub model: String,
    /// Avoided tokens attributed to this model (a largest-remainder slice;
    /// all rows' tokens sum to exactly the avoided total).
    pub tokens: u64,
    /// The uncached-input rate applied, micro-USD per MTok. `None` = no
    /// seeded price: the row contributes no dollars, honestly.
    pub price_per_mtok_micros: Option<i64>,
    /// `tokens` valued at the rate, micro-USD. `None` iff unpriced.
    pub subtotal_micros: Option<i64>,
    /// Catalog rate label (`"$5.00/MTok"`, no tilde — a recorded fact).
    pub rate_label: Option<String>,
    /// Estimate label (`"~$39.31"`, tilde mandatory). `None` iff unpriced.
    pub subtotal_label: Option<String>,
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
    /// Estimated USD saved (ADR-0051), integer micro-USD: the sum of the
    /// priced [`UsdRow`] subtotals. `None` is honest absence — no resolvable
    /// pricing basis — and draws no chip. A headline like `pct`: copied from
    /// the target frame, never tweened (money must not animate).
    pub usd_saved_micros: Option<i64>,
    /// The chip label, pre-rendered by the money authority (`"~$105.90"`).
    /// `Some` exactly when [`Self::usd_saved_micros`] is.
    pub usd_label: Option<String>,
    /// The per-model rows whose priced subtotals compose the headline
    /// (empty exactly when [`Self::usd_saved_micros`] is `None`).
    pub usd_rows: Vec<UsdRow>,
}

/// Recall MCP call numbers.
#[derive(Clone, Debug, PartialEq)]
pub struct CallsData {
    /// Total attributed calls across all schema-v2 history.
    pub total: u64,
    /// Exclusive end of the event-anchored 30-minute window.
    pub window_end_ms: i64,
    /// The four most recently active MCP connections in the visible window.
    pub series: Vec<ThreadCallSeries>,
    /// Additional real connections omitted from the chart; never aggregated.
    pub hidden_threads: usize,
}

/// One MCP process/connection's calls per minute, oldest bucket first.
#[derive(Clone, Debug, PartialEq)]
pub struct ThreadCallSeries {
    /// Opaque process/connection identity (not a capture-session id).
    pub thread_id: String,
    /// Terminal timestamp of this connection's freshest visible call.
    pub last_call_at: i64,
    /// Exactly 30 event-anchored one-minute buckets.
    pub rate: Vec<f64>,
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
    /// Terminal MCP invocations in the same window.
    pub mcp_calls_5m: Option<u64>,
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
    /// Top-level capture sessions currently live.
    pub active_sessions: ActiveSessionsView,
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
// its band (e.g. show "80% Clear" mid-transition). The meter snaps to each
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
            (RecallView::Data(a), RecallView::Data(b)) if calls_can_animate(a, b)
                && a.series.iter().zip(&b.series).any(|(x, y)| x.rate != y.rate))
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
            // Money never animates (ADR-0051 §7): the estimate and its rows
            // are headlines, copied straight from the target like `pct`.
            usd_saved_micros: b.usd_saved_micros,
            usd_label: b.usd_label.clone(),
            usd_rows: b.usd_rows.clone(),
        }),
        calls: interp_recall(&from.calls, &to.calls, |a, b| {
            if !calls_can_animate(a, b) {
                return b.clone();
            }
            CallsData {
                total: b.total,
                window_end_ms: b.window_end_ms,
                series: a
                    .series
                    .iter()
                    .zip(&b.series)
                    .map(|(x, y)| ThreadCallSeries {
                        thread_id: y.thread_id.clone(),
                        last_call_at: y.last_call_at,
                        rate: lerp_series(&x.rate, &y.rate, t).unwrap_or_else(|| y.rate.clone()),
                    })
                    .collect(),
                hidden_threads: b.hidden_threads,
            }
        }),
        active_sessions: to.active_sessions.clone(),
        // Domain metrics are headlines — taken from the target immediately.
        metrics: to.metrics.clone(),
    }
}

/// Calls may ease only when every sample still denotes the same time bucket
/// and MCP connection. A shifted window or reordered thread set snaps.
fn calls_can_animate(a: &CallsData, b: &CallsData) -> bool {
    a.window_end_ms == b.window_end_ms
        && a.series.len() == b.series.len()
        && a.series
            .iter()
            .zip(&b.series)
            .all(|(x, y)| x.thread_id == y.thread_id && x.rate.len() == y.rate.len())
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
    ids: &[SectionId],
    focused: bool,
    unicode: bool,
    area: Rect,
    offset: usize,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let content_h = content_rows(ids);
    if content_h <= area.h || area.w == 0 || area.h == 0 {
        // The expanded panels fit: solve at the column height (surplus spreads),
        // draw straight in. `scroll.graphs` clamps to 0 — nothing to scroll.
        draw_stack(col3, ids, focused, unicode, area, backend, frame);
        return;
    }
    // Overflow (ADR-0031): render the fully-expanded stack into a scratch frame,
    // then blit the [offset, offset+viewport) window — so "j/k scroll" moves the
    // panels instead of collapsing them, with the scrollbar (M57-06) as its sign.
    let mut scratch = Frame::new(crate::render::frame::Size::new(area.w, content_h));
    draw_stack(
        col3,
        ids,
        focused,
        unicode,
        scratch.rect(),
        backend,
        &mut scratch,
    );
    let off = offset.min(usize::from(content_h.saturating_sub(area.h)));
    frame.blit_window(&scratch, area, off);
}

/// Draw the `ids` panels straight into `area` at the solver's sizing (the
/// non-scrolled path; also the scratch-frame body for the scrolled path).
fn draw_stack(
    col3: &Col3,
    ids: &[SectionId],
    focused: bool,
    unicode: bool,
    area: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    for slot in solve_stack(ids, area.h) {
        let rect = Rect {
            x: area.x,
            y: area.y + slot.y,
            w: area.w,
            h: slot.rows,
        };
        draw_one(
            col3,
            slot.id,
            focused,
            unicode,
            slot.collapsed,
            rect,
            backend,
            frame,
        );
    }
}

/// Draw one panel into `rect` — the per-unit dispatch a mixed column (a panel
/// among cards, ADR-0034) shares with the homogeneous panel stack.
pub fn draw_one(
    col3: &Col3,
    id: SectionId,
    focused: bool,
    unicode: bool,
    collapsed: bool,
    rect: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    if rect.h == 0 || rect.w == 0 {
        return;
    }
    let theme = Theme::rift_dark();
    match id {
        SectionId::TokenUsage => {
            draw_token_panel(
                col3, theme, focused, unicode, collapsed, rect, backend, frame,
            );
        }
        SectionId::System => {
            draw_system_panel(
                col3, theme, focused, unicode, collapsed, rect, backend, frame,
            );
        }
        SectionId::Recall => draw_recall_panel(
            col3, theme, focused, unicode, collapsed, rect, backend, frame,
        ),
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
                x_axis: crate::axis::XAxis::Categories(&x_labels),
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
    let cell = |i: usize| cats.get(i).copied().unwrap_or(theme.graph_accent());
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
    let active = match col3.active_sessions {
        ActiveSessionsView::Count(count) => count,
        ActiveSessionsView::NoSessions => 0,
    };
    let active_label = active_sessions_label(active);
    if collapsed {
        let head = match &col3.nic {
            NicView::Reading(r) => format!("{}% {}  ·  {active_label}", r.pct, r.band),
            NicView::UnknownModel { .. } => format!("context ?  ·  {active_label}"),
            NicView::NoData => format!("context unavailable  ·  {active_label}"),
        };
        body(frame, inner, 0, &head, theme.body());
        return;
    }
    // The donut occupies the top; the domain metrics the bottom rows — plus,
    // where the panel is tall enough, one hint row naming the rates' scope
    // current-session facts are labelled directly; completed-session facts
    // hold dimmed without ever being called idle.
    let m = &col3.metrics;
    let hint = inner.h >= 8;
    let metric_rows = 4u16 + u16::from(hint);
    let gauge_h = inner.h.saturating_sub(metric_rows);
    let gauge_rect = Rect {
        x: inner.x,
        y: inner.y,
        w: inner.w,
        h: gauge_h,
    };
    draw_context_pressure(
        &col3.nic, m, active, theme, unicode, gauge_rect, backend, frame,
    );
    let base = inner.h.saturating_sub(metric_rows);
    if hint {
        let sep = if unicode { "·" } else { "-" };
        let (word, style) = match m.activity_scope {
            SystemScope::CurrentSessions => ("current sessions", theme.body()),
            SystemScope::LastCompletedSession => ("last completed session", theme.dim()),
            SystemScope::Unavailable => ("session unavailable", theme.dim()),
        };
        body(
            frame,
            Rect {
                x: inner.x + 1,
                w: inner.w.saturating_sub(1),
                ..inner
            },
            base,
            &format!("scope {sep} {word}"),
            style,
        );
    }
    // The metric rows below the hint. The three session rates render dimmed
    // when no session is live (held from the last one — a stale-marker, not
    // an absence); the projection size is instantaneous and always body.
    let rows = [
        (
            "events · 5m",
            m.events_5m.map(|value| value.to_string()),
            m.activity_scope != SystemScope::CurrentSessions,
        ),
        (
            "tokens · 5m",
            m.tokens_5m.map(|value| fmt_km(value as f64)),
            m.activity_scope != SystemScope::CurrentSessions,
        ),
        (
            "MCP calls · 5m",
            m.mcp_calls_5m.map(|value| value.to_string()),
            m.activity_scope != SystemScope::CurrentSessions,
        ),
        (
            "projection storage",
            m.projection_bytes.map(fmt_bytes),
            false,
        ),
    ];
    let rows_base = base + u16::from(hint);
    for (i, (label, value, dimmed)) in rows.into_iter().enumerate() {
        let Ok(iu) = u16::try_from(i) else { break };
        let dy = rows_base + iu;
        let dot = Icon::Bullet.glyph(unicode);
        frame.set(inner.x, inner.y + dy, dot, theme.dim());
        let text = value.map_or_else(|| format!(" {label}: —"), |v| format!(" {label}: {v}"));
        let style = if value_present(&text) && !dimmed {
            theme.body()
        } else {
            theme.dim()
        };
        body(
            frame,
            Rect {
                x: inner.x + 1,
                w: inner.w.saturating_sub(1),
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

/// Draw context pressure: a radial donut on SUBCELL-capable backends, degrading
/// to the shipped horizontal gauge bar elsewhere; the unknown-model indicator
/// and the no-data placeholder render explicitly (never a percentage; ADR-0012).
#[allow(clippy::too_many_arguments)]
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

#[allow(clippy::too_many_arguments)]
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

#[allow(clippy::too_many_arguments)]
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

fn active_sessions_label(count: u16) -> String {
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
    unicode: bool,
    collapsed: bool,
    rect: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let inner = panel_frame(theme, Section::Recall, focused, "recall", rect, frame);
    if draw_recall_placeholder(col3, theme, inner, frame) {
        return;
    }
    let (savings, call_total, hidden, sessions) = recall_headlines(col3);
    let hidden_text = if hidden > 0 {
        format!(" · +{hidden} hidden")
    } else {
        String::new()
    };
    if collapsed || inner.w < 28 || inner.h < 7 {
        body(frame, inner, 0, &savings, theme.body());
        if inner.w >= 30 {
            body(
                frame,
                inner,
                1,
                &format!("{call_total} calls · {sessions}"),
                theme.body(),
            );
        } else {
            body(
                frame,
                inner,
                1,
                &format!("{call_total} calls"),
                theme.body(),
            );
            body(frame, inner, 2, &sessions, theme.body());
        }
        return;
    }
    body(
        frame,
        inner,
        0,
        &savings,
        theme.tile_value(theme.graph_accent()),
    );
    // The USD estimate chip (ADR-0051): drawn from the same solver the
    // hit-test reads, so pixels == target. Absent label → no chip at all
    // (honest absence, never a fabricated figure). Flashless by design:
    // col3 carries no press-pulse plumbing (recorded M5.15 decision).
    if let Some(chip) = recall_cost_rect(col3, collapsed, inner) {
        if let RecallView::Data(sd) = &col3.savings {
            if let Some(label) = sd.usd_label.as_deref() {
                frame.put_str(chip.x, chip.y, &format!("[{label}]"), theme.key_hint());
            }
        }
    }
    body(
        frame,
        inner,
        1,
        &format!("{call_total} MCP calls · {sessions}{hidden_text}"),
        theme.body(),
    );

    let RecallView::Data(calls) = &col3.calls else {
        body(
            frame,
            inner,
            3,
            "(no attributed MCP calls yet)",
            theme.dim(),
        );
        return;
    };
    if calls.series.is_empty() {
        body(frame, inner, 3, "(no calls in window)", theme.dim());
        return;
    }

    let chart = Rect {
        x: inner.x,
        y: inner.y + 2,
        w: inner.w,
        h: inner.h.saturating_sub(2),
    };
    draw_call_chart(calls, theme, unicode, chart, backend, frame);
}

fn draw_recall_placeholder(col3: &Col3, theme: Theme, inner: Rect, frame: &mut Frame) -> bool {
    if matches!(col3.savings, RecallView::Disabled) && matches!(col3.calls, RecallView::Disabled) {
        body(
            frame,
            inner,
            0,
            "recall off — /toggle total-recall",
            theme.dim(),
        );
        return true;
    }
    if matches!(col3.savings, RecallView::Quiet) && matches!(col3.calls, RecallView::Quiet) {
        body(
            frame,
            inner,
            0,
            "enabled, no serves yet — see `rift doctor`",
            theme.dim(),
        );
        return true;
    }
    false
}

/// The clickable USD-savings chip on the Recall headline row (ADR-0051 §7):
/// `[~$105.90]`, two cells after the `saved …` text. **One solver** feeds the
/// draw and the hit-test, so the glyphs and the click target are always the
/// same cells (`hit_test` recomputes geometry; a second solver would put
/// clicks on the wrong cells).
///
/// `None` — and therefore no chip anywhere — when there is no honest figure
/// (`usd_label` absent, ADR-0051 §5 rung 4), in the collapsed/narrow path
/// (the panel is already shedding text), or when the whole chip cannot fit
/// on the headline row.
#[must_use]
pub fn recall_cost_rect(col3: &Col3, collapsed: bool, inner: Rect) -> Option<Rect> {
    if collapsed || inner.w < 28 || inner.h < 7 {
        return None;
    }
    let RecallView::Data(sd) = &col3.savings else {
        return None;
    };
    let label = sd.usd_label.as_deref()?;
    let (savings, _, _, _) = recall_headlines(col3);
    let headline = u16::try_from(savings.chars().count()).ok()?;
    let w = u16::try_from(label.chars().count().checked_add(2)?).ok()?;
    let x = inner.x.checked_add(headline)?.checked_add(2)?;
    (u32::from(x) + u32::from(w) <= u32::from(inner.x) + u32::from(inner.w)).then_some(Rect {
        x,
        y: inner.y,
        w,
        h: 1,
    })
}

/// Thousands-grouped token count (`7862400` → `"7,862,400"`) for the
/// breakdown modal: its rows must *visibly* sum to the headline, so no lossy
/// K/M humanizing there. Tokens are counts, not money — money strings only
/// ever come pre-rendered from the money authority.
#[must_use]
pub fn fmt_count(n: u64) -> String {
    let digits = n.to_string();
    let len = digits.chars().count();
    let mut out = String::with_capacity(len + len / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// The breakdown modal's body (ADR-0051 §3) — the full disclosed basis:
/// net avoided tokens, the class assumption, one row per model (attributed
/// slice × catalog rate = subtotal), the composing total, the unpriced
/// coverage when any, and the estimate-of-an-estimate statement. Pure over
/// the savings view-model; every money string was pre-rendered by the one
/// money authority, so this function formats counts and assembles lines —
/// it never computes or formats money.
#[must_use]
pub fn usd_breakdown_lines(sd: &SavingsData) -> Vec<String> {
    let Some(total_label) = sd.usd_label.as_deref() else {
        // Defensive: the chip never draws without a figure, so this modal is
        // unreachable in practice — but if reached, it stays honest.
        return vec![
            "no dollar figure: no seeded price applies to this".to_owned(),
            "project's models — the token headline stands alone".to_owned(),
        ];
    };
    let avoided: u64 = sd.usd_rows.iter().map(|r| r.tokens).sum();
    let mut lines = vec![
        format!("net tokens avoided   {}", fmt_count(avoided)),
        "priced as uncached input, at the current seeded".to_owned(),
        "rates; split by each model's share of this project's".to_owned(),
        "observed input tokens".to_owned(),
        String::new(),
    ];
    let name_w = sd
        .usd_rows
        .iter()
        .map(|r| r.model.chars().count())
        .max()
        .unwrap_or(0);
    let tok_w = sd
        .usd_rows
        .iter()
        .map(|r| fmt_count(r.tokens).chars().count())
        .max()
        .unwrap_or(0);
    let mut unpriced_tokens: u64 = 0;
    for r in &sd.usd_rows {
        let tokens = fmt_count(r.tokens);
        if let (Some(rate), Some(sub)) = (&r.rate_label, &r.subtotal_label) {
            lines.push(format!(
                "{model:<name_w$}  {tokens:>tok_w$} tok x {rate} = {sub}",
                model = r.model,
            ));
        } else {
            unpriced_tokens = unpriced_tokens.saturating_add(r.tokens);
            lines.push(format!(
                "{model:<name_w$}  {tokens:>tok_w$} tok — no seeded price",
                model = r.model,
            ));
        }
    }
    lines.push(format!("total = {total_label}"));
    if unpriced_tokens > 0 {
        lines.push(String::new());
        lines.push(format!(
            "{} tok carry no seeded price and add nothing to",
            fmt_count(unpriced_tokens)
        ));
        lines.push("the total — the figure covers the priced share only".to_owned());
    }
    lines.push(String::new());
    lines.push("~ marks an estimate, not a bill: token counts are".to_owned());
    lines.push("cl100k_base (not the provider's billing tokenizer),".to_owned());
    lines.push("and the per-model split is attributed from observed".to_owned());
    lines.push("usage — the serve trail records no per-serve model".to_owned());
    lines.push("(ADR-0051).".to_owned());
    lines
}

fn recall_headlines(col3: &Col3) -> (String, u64, usize, String) {
    let savings = match &col3.savings {
        RecallView::Data(sd) => {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
            let pct = (sd.pct * 100.0).round() as u8;
            format!("saved {pct}% ({})", fmt_km(sd.total))
        }
        RecallView::Quiet | RecallView::Disabled => "saved —".to_string(),
    };
    let (call_total, hidden) = match &col3.calls {
        RecallView::Data(calls) => (calls.total, calls.hidden_threads),
        RecallView::Quiet | RecallView::Disabled => (0, 0),
    };
    let sessions = match &col3.active_sessions {
        ActiveSessionsView::Count(count) => active_sessions_label(*count),
        ActiveSessionsView::NoSessions => active_sessions_label(0),
    };
    (savings, call_total, hidden, sessions)
}

fn draw_call_chart(
    calls: &CallsData,
    theme: Theme,
    unicode: bool,
    chart: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    let y_max = call_y_max(&calls.series);
    let colours = theme.recall_thread_colors();
    let labels: Vec<String> = calls
        .series
        .iter()
        .map(|s| format!("t-{}", s.thread_id.chars().take(4).collect::<String>()))
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
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let value = value.round().max(0.0) as u64;
    value.to_string()
}

/// One integral scale shared by every visible call line: zero at the bottom and
/// the smallest even ceiling at least as high as the busiest minute (minimum 2).
fn call_y_max(series: &[ThreadCallSeries]) -> f64 {
    let observed = series
        .iter()
        .flat_map(|item| item.rate.iter().copied())
        .filter(|value| value.is_finite())
        .fold(0.0f64, f64::max)
        .ceil();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let observed = observed.max(0.0) as u64;
    let ceiling = observed.max(2);
    #[allow(clippy::cast_precision_loss)]
    let ceiling = ceiling.saturating_add(ceiling % 2) as f64;
    ceiling
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
        draw(
            col3,
            &SectionId::ALL,
            false,
            false,
            f.rect(),
            0,
            &mut b,
            &mut f,
        );
        f
    }

    fn draw_at(col3: &Col3, w: u16, h: u16, offset: usize) -> Frame {
        let mut f = Frame::new(Size::new(w, h));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw(
            col3,
            &SectionId::ALL,
            false,
            false,
            f.rect(),
            offset,
            &mut b,
            &mut f,
        );
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
        // height, a different offset windows a different slice; a tall column
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
            content_rows(&SectionId::ALL) > 12,
            "the fixture stack overflows a 12-row column"
        );
        let top = text_of(&draw_at(&col3, 34, 12, 0));
        let scrolled = text_of(&draw_at(&col3, 34, 12, 5));
        assert_ne!(top, scrolled, "the offset scrolls the panels");
        // A column taller than the expanded content ignores the offset.
        let fit_h = content_rows(&SectionId::ALL);
        let fit_a = text_of(&draw_at(&col3, 34, fit_h, 0));
        let fit_b = text_of(&draw_at(&col3, 34, fit_h, 5));
        assert_eq!(fit_a, fit_b, "a fitting column has nothing to scroll");
        assert_eq!(max_scroll(&SectionId::ALL, fit_h), 0);
        assert!(max_scroll(&SectionId::ALL, 12) > 0);
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
        let slots = solve_stack(&SectionId::ALL, 40);
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
        let slots = solve_stack(&SectionId::ALL, 18);
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
        assert!(solve_stack(&SectionId::ALL, 0).is_empty());
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
        // The band colour steps at the gauge crate's boundaries (49→Clear /
        // 50→Moderate, 94→Very High / 95→Critical); the correspondence to
        // `Band::from_pct` is proven in the data-plane gate leg (nic-gauge dep).
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
                mcp_calls_5m: None,
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
        assert!(text.contains("MCP calls · 5m: —"));
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
        let text = text_of(&draw_to(&live, 40, 30));
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
        let text = text_of(&draw_to(&held, 40, 30));
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
        let text = text_of(&draw_to(&col3, 48, 30));
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
        let text = text_of(&draw_to(&col3, 40, 12));
        assert!(text.contains("events · 5m: 3"), "rows survive:\n{text}");
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
                usd_saved_micros: None,
                usd_label: None,
                usd_rows: vec![],
            }),
            calls: RecallView::Data(CallsData {
                total: 17,
                window_end_ms: 1_800_000,
                series: vec![ThreadCallSeries {
                    thread_id: "abcd1234".into(),
                    last_call_at: 1_799_000,
                    rate: vec![1.0; 30],
                }],
                hidden_threads: 0,
            }),
            active_sessions: ActiveSessionsView::Count(3),
            ..Col3::default()
        };
        let text = text_of(&draw_to(&data, 40, 24));
        assert!(text.contains("saved 42%"), "savings % + tokens: \n{text}");
        assert!(text.contains("17 MCP calls"));
        assert!(text.contains("3 active sessions"));
    }

    /// A Col3 whose savings carry the USD estimate (ADR-0051 fixtures).
    fn usd_col3(label: Option<&str>) -> Col3 {
        Col3 {
            savings: RecallView::Data(SavingsData {
                rate: vec![1.0],
                total: 6000.0,
                pct: 0.42,
                usd_saved_micros: label.map(|_| 6_250_000),
                usd_label: label.map(str::to_owned),
                usd_rows: vec![],
            }),
            ..Col3::default()
        }
    }

    /// ADR-0051 §7: the chip's glyphs land exactly on the cells the shared
    /// solver reports — pixels == target, one geometry for draw and hit-test.
    #[test]
    fn usd_chip_draws_exactly_where_the_shared_solver_says() {
        let col3 = usd_col3(Some("~$6.25"));
        let rect = Rect {
            x: 0,
            y: 0,
            w: 40,
            h: 10,
        };
        let mut f = Frame::new(Size::new(40, 10));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_one(
            &col3,
            SectionId::Recall,
            false,
            false,
            false,
            rect,
            &mut b,
            &mut f,
        );
        // Recompute the inner rect through the same chrome solver the panel
        // used (geometry is theme-independent).
        let mut scratch = Frame::new(Size::new(40, 10));
        let inner = panel_frame(
            Theme::rift_dark(),
            Section::Recall,
            false,
            "recall",
            rect,
            &mut scratch,
        );
        let chip = recall_cost_rect(&col3, false, inner).expect("an honest figure draws a chip");
        // The drawn text at the chip's cells is exactly `[~$6.25]`.
        let drawn: String = (chip.x..chip.x + chip.w)
            .map(|x| f.get(x, chip.y).map_or(' ', |c| c.ch))
            .collect();
        assert_eq!(drawn, "[~$6.25]");
        assert!(drawn.contains('~'), "the estimate marker is mandatory");
        // And nowhere else: the row before the chip is the headline text.
        let row: String = (inner.x..inner.x + inner.w)
            .map(|x| f.get(x, chip.y).map_or(' ', |c| c.ch))
            .collect();
        assert_eq!(row.matches('[').count(), 1, "one chip only: {row}");
    }

    /// Honest absence and degraded layouts draw no chip at all (never a
    /// fabricated figure, never a truncated one).
    #[test]
    fn usd_chip_absent_without_an_honest_figure_or_room() {
        // No figure → no chip anywhere in the drawn panel.
        let none = usd_col3(None);
        let text = text_of(&draw_to(&none, 40, 24));
        assert!(!text.contains('['), "no chip without a figure: {text}");

        let some = usd_col3(Some("~$6.25"));
        let inner = Rect {
            x: 1,
            y: 1,
            w: 38,
            h: 8,
        };
        // Collapsed path: the panel is shedding text; the chip goes first.
        assert_eq!(recall_cost_rect(&some, true, inner), None);
        // Narrow / short inners: below the wide-path floor, no chip.
        assert_eq!(
            recall_cost_rect(
                &some,
                false,
                Rect {
                    x: 1,
                    y: 1,
                    w: 27,
                    h: 8
                }
            ),
            None
        );
        assert_eq!(
            recall_cost_rect(
                &some,
                false,
                Rect {
                    x: 1,
                    y: 1,
                    w: 38,
                    h: 6
                }
            ),
            None
        );
        // A row too tight for the whole chip refuses rather than truncates:
        // headline "saved 42% (6.0K)" = 16 cells + 2 gap + 8 chip = 26 < 28
        // fits at w=28... shrink until it cannot: chip needs x+w ≤ inner end.
        let tight = Rect {
            x: 1,
            y: 1,
            w: 28,
            h: 8,
        };
        // 16 + 2 = 18 offset, +8 = 26 ≤ 28 → fits; w=25 would fail the wide
        // floor first, so prove the fit guard with a longer label instead.
        let long = usd_col3(Some("~$1,234,567.89"));
        assert_eq!(
            recall_cost_rect(&long, false, tight),
            None,
            "a chip that cannot fit whole is not drawn at all"
        );
    }

    /// ADR-0051 §3: the modal discloses the whole basis — rows that visibly
    /// sum, the class assumption, unpriced coverage, and the
    /// estimate-of-an-estimate statement. Every money string is pre-rendered
    /// (this function never formats money).
    #[test]
    fn breakdown_lines_disclose_the_whole_basis() {
        let sd = SavingsData {
            rate: vec![1.0],
            total: 1_000_500.0,
            pct: 0.9,
            usd_saved_micros: Some(2_500_000),
            usd_label: Some("~$2.50".to_owned()),
            usd_rows: vec![
                UsdRow {
                    model: "claude-opus-4-8".to_owned(),
                    tokens: 500_000,
                    price_per_mtok_micros: Some(5_000_000),
                    subtotal_micros: Some(2_500_000),
                    rate_label: Some("$5.00/MTok".to_owned()),
                    subtotal_label: Some("~$2.50".to_owned()),
                },
                UsdRow {
                    model: "mystery-model".to_owned(),
                    tokens: 500_500,
                    price_per_mtok_micros: None,
                    subtotal_micros: None,
                    rate_label: None,
                    subtotal_label: None,
                },
            ],
        };
        let body = usd_breakdown_lines(&sd).join("\n");
        // The net total is the rows' exact sum, thousands-grouped.
        assert!(body.contains("net tokens avoided   1,000,500"), "{body}");
        assert!(
            body.contains("uncached input"),
            "the class is named: {body}"
        );
        // Priced row: slice × catalog rate = tilde'd subtotal.
        assert!(body.contains("500,000 tok x $5.00/MTok = ~$2.50"), "{body}");
        // Unpriced row: slice kept, marked, no dollars anywhere on it.
        assert!(body.contains("mystery-model"), "{body}");
        assert!(body.contains("no seeded price"), "{body}");
        // The composing total and the unpriced-coverage disclosure.
        assert!(body.contains("total = ~$2.50"), "{body}");
        assert!(
            body.contains("500,500 tok carry no seeded price"),
            "coverage is disclosed: {body}"
        );
        // The honesty statement, verbatim anchors.
        assert!(body.contains("estimate, not a bill"), "{body}");
        assert!(body.contains("cl100k_base"), "{body}");
        assert!(body.contains("attributed from observed"), "{body}");
    }

    #[test]
    fn fmt_count_groups_thousands() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_000), "1,000");
        assert_eq!(fmt_count(14_594_095), "14,594,095");
    }

    /// ADR-0051 §7: money never animates — the estimate and its rows are
    /// headlines, copied from the target frame exactly like `pct`.
    #[test]
    fn money_is_a_headline_never_tweened() {
        let mut from = usd_col3(Some("~$1.00"));
        if let RecallView::Data(sd) = &mut from.savings {
            sd.usd_saved_micros = Some(1_000_000);
        }
        let to = usd_col3(Some("~$6.25"));
        let mid = interpolate(&from, &to, 0.5);
        let RecallView::Data(sd) = mid.savings else {
            panic!("expected Data");
        };
        assert_eq!(
            sd.usd_saved_micros,
            Some(6_250_000),
            "target's figure, not a blend"
        );
        assert_eq!(sd.usd_label.as_deref(), Some("~$6.25"));
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
    fn recall_lines_snap_when_the_window_or_thread_identity_changes() {
        let calls = |window_end_ms, thread_id: &str, value| CallsData {
            total: 1,
            window_end_ms,
            series: vec![ThreadCallSeries {
                thread_id: thread_id.into(),
                last_call_at: window_end_ms - 1,
                rate: vec![value; 30],
            }],
            hidden_threads: 0,
        };
        let base = calls(1_800_000, "thread-a", 1.0);
        assert!(calls_can_animate(&base, &calls(1_800_000, "thread-a", 2.0)));
        assert!(!calls_can_animate(
            &base,
            &calls(1_860_000, "thread-a", 2.0)
        ));
        assert!(!calls_can_animate(
            &base,
            &calls(1_800_000, "thread-b", 2.0)
        ));
    }

    #[test]
    fn recall_lines_share_the_smallest_even_integral_scale() {
        let series = |values: &[f64]| ThreadCallSeries {
            thread_id: "thread".into(),
            last_call_at: 0,
            rate: values.to_vec(),
        };
        assert_eq!(call_y_max(&[]), 2.0);
        assert_eq!(call_y_max(&[series(&[0.0, 1.0])]), 2.0);
        assert_eq!(call_y_max(&[series(&[2.0]), series(&[3.0])]), 4.0);
        assert_eq!(call_y_max(&[series(&[6.0])]), 6.0);
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
                active_sessions: 1,
            }),
            ..Col3::default()
        };
        let from = reading(80, "High");
        let to = reading(20, "Clear");
        assert!(
            !should_animate(&from, &to),
            "a nic reading change must not arm a tween"
        );
        // interpolate at any progress shows the TARGET reading verbatim — pct and
        // band always agree, never "80% Clear".
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
