//! Column 3 — the six live-graph sections: view-model types, the stack
//! solver, and the section renderer (M4-10).
//!
//! The **fixed display order** is registration order in [`SECTIONS`] — call
//! sites cannot reorder it. Two source orderings disagreed (PRD-M4 Solution
//! prose vs build-plan P6 S1); the PRD is the binding spec, so the order is:
//! token usage · input|read|output · nic-gauge · recall savings · recall MCP
//! calls · threads (decision recorded in the M4 map).
//!
//! Every section is **honest about absence** (user stories 5/9): a data-less
//! section still occupies its slot with its placeholder — "no token activity
//! yet", the recall enable-prompt vs the visibly distinct enabled-but-quiet
//! state, the unknown-model indicator — never a fake zero line. Under height
//! pressure the solver collapses low-priority sections to one-line summaries
//! that keep the headline number (fidelity is lost, information is not —
//! user story 20).
//!
//! The types here are **filled by the data plane** (`rift-canopy-data`, which
//! imports this crate — never the reverse, ADR-0009) and rendered pure over
//! the [`crate::state::ViewModel`].

use crate::layout::Rect;
use crate::plot;
use crate::render::frame::{Frame, Style};
use crate::render::RenderBackend;

/// The six sections, in their fixed display order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionId {
    /// Token spend over time (rate + cumulative).
    TokenUsage,
    /// The input | read | output class breakdown (+ cache-write band).
    InputReadOutput,
    /// The nic-gauge (context fullness, named bands, thread badge).
    NicGauge,
    /// Recall savings (rate + cumulative tokens avoided).
    RecallSavings,
    /// Recall MCP calls per bucket.
    RecallCalls,
    /// Distinct active sessions in the recent window.
    Threads,
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

/// The registration table — the **single** place the order, weights, and
/// collapse priorities are defined (M4-10 decisions, recorded in the map):
/// the token and nic sections hold out longest; recall-calls and threads
/// fold first (threads is nearly one line anyway).
pub const SECTIONS: [SectionSpec; 6] = [
    SectionSpec {
        id: SectionId::TokenUsage,
        weight: 3,
        min_rows: 4,
        priority: Priority::High,
    },
    SectionSpec {
        id: SectionId::InputReadOutput,
        weight: 3,
        min_rows: 4,
        priority: Priority::High,
    },
    SectionSpec {
        id: SectionId::NicGauge,
        weight: 2,
        min_rows: 3,
        priority: Priority::High,
    },
    SectionSpec {
        id: SectionId::RecallSavings,
        weight: 3,
        min_rows: 4,
        priority: Priority::Medium,
    },
    SectionSpec {
        id: SectionId::RecallCalls,
        weight: 2,
        min_rows: 3,
        priority: Priority::Low,
    },
    SectionSpec {
        id: SectionId::Threads,
        weight: 1,
        min_rows: 2,
        priority: Priority::Low,
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

/// Distribute `height` rows across the six sections. Deterministic and total:
/// * everything expanded when the minimums fit, surplus split by weight;
/// * under pressure, Low sections collapse to one line first, then Medium,
///   then High — a collapsed section keeps its headline (never omitted);
/// * when even six single lines do not fit, the **first** `height` sections
///   render collapsed and the rest are dropped (the only lossy tier, below
///   6 rows).
#[must_use]
pub fn solve_stack(height: u16) -> Vec<SectionSlot> {
    let n = u16::try_from(SECTIONS.len()).unwrap_or(6);
    if height == 0 {
        return Vec::new();
    }
    // Choose which priorities stay expanded: strongest set that fits.
    let tiers: [&[Priority]; 4] = [
        &[Priority::High, Priority::Medium, Priority::Low], // all expanded
        &[Priority::High, Priority::Medium],
        &[Priority::High],
        &[],
    ];
    let mut chosen: &[Priority] = &[];
    for tier in tiers {
        let need: u16 = SECTIONS
            .iter()
            .map(|s| {
                if tier.contains(&s.priority) {
                    s.min_rows
                } else {
                    1
                }
            })
            .sum();
        if need <= height {
            chosen = tier;
            break;
        }
    }
    // Below six rows even the all-collapsed tier fails: first `height` only.
    let all_collapsed_fit = height >= n;
    if chosen.is_empty() && !all_collapsed_fit {
        return SECTIONS
            .iter()
            .take(usize::from(height))
            .enumerate()
            .map(|(i, s)| SectionSlot {
                id: s.id,
                y: u16::try_from(i).unwrap_or(0),
                rows: 1,
                collapsed: true,
            })
            .collect();
    }

    // Base allocation: minimums for expanded, one line for collapsed.
    let mut rows: Vec<u16> = SECTIONS
        .iter()
        .map(|s| {
            if chosen.contains(&s.priority) {
                s.min_rows
            } else {
                1
            }
        })
        .collect();
    let used: u16 = rows.iter().sum();
    let mut surplus = height.saturating_sub(used);
    // Spread surplus over the expanded sections by weight, round-robin so the
    // split is exact (no fractional rows, no gap at the bottom).
    let weights: Vec<u16> = SECTIONS
        .iter()
        .map(|s| {
            if chosen.contains(&s.priority) {
                s.weight
            } else {
                0
            }
        })
        .collect();
    let total_weight: u16 = weights.iter().sum();
    if total_weight > 0 {
        // Whole shares first…
        for (r, w) in rows.iter_mut().zip(&weights) {
            let share = u32::from(surplus) * u32::from(*w) / u32::from(total_weight);
            *r = r.saturating_add(u16::try_from(share).unwrap_or(0));
        }
        // …then the remainder, one row at a time, heaviest-first order.
        let mut leftover = height.saturating_sub(rows.iter().sum::<u16>());
        let mut order: Vec<usize> = (0..rows.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(weights.get(i).copied().unwrap_or(0)));
        for i in order {
            if leftover == 0 {
                break;
            }
            if weights.get(i).copied().unwrap_or(0) > 0 {
                if let Some(r) = rows.get_mut(i) {
                    *r = r.saturating_add(1);
                    leftover -= 1;
                }
            }
        }
        surplus = leftover;
    }
    let _ = surplus; // fully collapsed columns leave the tail blank

    let mut out = Vec::with_capacity(SECTIONS.len());
    let mut y = 0u16;
    for (spec, r) in SECTIONS.iter().zip(rows) {
        if y >= height {
            break; // all-collapsed tier taller than the column: drop the tail
        }
        let rows = r.min(height - y);
        out.push(SectionSlot {
            id: spec.id,
            y,
            rows,
            collapsed: !chosen.contains(&spec.priority),
        });
        y = y.saturating_add(rows);
    }
    out
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

/// Render the six sections into `area` through the plot seam. Pure over the
/// view-model; safe at any size (zero-height slots simply do not render).
pub fn draw(col3: &Col3, area: Rect, backend: &mut dyn RenderBackend, frame: &mut Frame) {
    for slot in solve_stack(area.h) {
        let rect = Rect {
            x: area.x,
            y: area.y + slot.y,
            w: area.w,
            h: slot.rows,
        };
        draw_section(col3, slot, rect, backend, frame);
    }
}

/// The section title row: `· token usage` etc.
fn title_line(frame: &mut Frame, rect: Rect, title: &str, headline: &str) {
    let mut line = String::from("· ");
    line.push_str(title);
    if !headline.is_empty() {
        line.push_str("  ");
        line.push_str(headline);
    }
    let clipped: String = line.chars().take(usize::from(rect.w)).collect();
    frame.put_str(rect.x, rect.y, &clipped, Style::DEFAULT);
}

/// One plain content line under the title.
fn body_line(frame: &mut Frame, rect: Rect, dy: u16, text: &str) {
    if dy >= rect.h {
        return;
    }
    let clipped: String = text.chars().take(usize::from(rect.w)).collect();
    frame.put_str(rect.x, rect.y + dy, &clipped, Style::DEFAULT);
}

/// The plot area under a section's title (rows 1.., full width).
fn plot_rect(rect: Rect) -> Rect {
    Rect {
        x: rect.x,
        y: rect.y + 1,
        w: rect.w,
        h: rect.h.saturating_sub(1),
    }
}

#[allow(clippy::too_many_lines)] // one match arm per section — splitting hides the shape
fn draw_section(
    col3: &Col3,
    slot: SectionSlot,
    rect: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) {
    if rect.h == 0 || rect.w == 0 {
        return;
    }
    match slot.id {
        SectionId::TokenUsage => match &col3.tokens {
            TokensView::NoActivity => {
                title_line(frame, rect, "token usage", "(no token activity yet)")
            }
            TokensView::Data(d) => {
                title_line(
                    frame,
                    rect,
                    "token usage",
                    &format!("Σ {}", fmt_km(d.total)),
                );
                if !slot.collapsed {
                    let pr = plot_rect(rect);
                    let max = plot::autoscale_max(&d.rate);
                    let pts = plot::bar_points(&d.rate, max, pr.h.saturating_mul(4).max(4));
                    backend.plot(&pts, pr, Style::DEFAULT, frame);
                }
            }
        },
        SectionId::InputReadOutput => match &col3.breakdown {
            BreakdownView::NoActivity => {
                title_line(
                    frame,
                    rect,
                    "input | read | output",
                    "(no token activity yet)",
                );
            }
            BreakdownView::Data(d) => {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let read_pct = (d.read_fraction * 100.0).round() as u8;
                title_line(
                    frame,
                    rect,
                    "input | read | output",
                    &format!("read {read_pct}%"),
                );
                if !slot.collapsed {
                    body_line(
                        frame,
                        rect,
                        1,
                        &format!(
                            "in {}  rd {}  out {}",
                            fmt_km(d.input),
                            fmt_km(d.read),
                            fmt_km(d.output)
                        ),
                    );
                    // The cache-write band is visibly secondary: one dim line.
                    body_line(
                        frame,
                        rect,
                        2,
                        &format!("cache-write {}", fmt_km(d.cache_write)),
                    );
                    if rect.h > 3 {
                        let pr = Rect {
                            x: rect.x,
                            y: rect.y + 3,
                            w: rect.w,
                            h: rect.h - 3,
                        };
                        let vals = [d.input, d.read, d.output, d.cache_write];
                        let max = plot::autoscale_max(&vals);
                        let pts = plot::bar_points(&vals, max, pr.h.saturating_mul(4).max(4));
                        backend.plot(&pts, pr, Style::DEFAULT, frame);
                    }
                }
            }
        },
        SectionId::NicGauge => match &col3.nic {
            NicView::NoData => title_line(frame, rect, "nic-gauge", "(no model calls yet)"),
            NicView::UnknownModel { model, threads } => {
                title_line(frame, rect, "nic-gauge", &format!("[{threads} thread]"));
                if !slot.collapsed {
                    // The explicit indicator — never a defaulted percentage.
                    body_line(
                        frame,
                        rect,
                        1,
                        &format!("? unknown context window: {model}"),
                    );
                }
            }
            NicView::Reading(r) => {
                title_line(
                    frame,
                    rect,
                    "nic-gauge",
                    &format!("{}% {}  [{} thread]", r.pct, r.band, r.threads),
                );
                if !slot.collapsed {
                    let pr = plot_rect(rect);
                    let pts = plot::gauge_points(
                        f32::from(r.pct) / 100.0,
                        pr.w.saturating_mul(2).max(2),
                        pr.h.saturating_mul(4).clamp(1, 4),
                    );
                    backend.plot(&pts, pr, Style::DEFAULT, frame);
                }
            }
        },
        SectionId::RecallSavings => match &col3.savings {
            RecallView::Disabled => {
                title_line(
                    frame,
                    rect,
                    "recall savings",
                    "(recall off — `rift enable`)",
                );
            }
            RecallView::Quiet => title_line(frame, rect, "recall savings", "(no serves yet)"),
            RecallView::Data(d) => {
                title_line(
                    frame,
                    rect,
                    "recall savings",
                    &format!("Σ {} saved", fmt_km(d.total)),
                );
                if !slot.collapsed {
                    let pr = plot_rect(rect);
                    let max = plot::autoscale_max(&d.rate);
                    let pts = plot::line_points(&d.rate, max, pr.w.saturating_mul(2).max(2));
                    backend.plot(&pts, pr, Style::DEFAULT, frame);
                }
            }
        },
        SectionId::RecallCalls => match &col3.calls {
            RecallView::Disabled => {
                title_line(
                    frame,
                    rect,
                    "recall mcp calls",
                    "(recall off — `rift enable`)",
                );
            }
            RecallView::Quiet => title_line(frame, rect, "recall mcp calls", "(no serves yet)"),
            RecallView::Data(d) => {
                title_line(
                    frame,
                    rect,
                    "recall mcp calls",
                    &format!("Σ {}", fmt_km(d.total)),
                );
                if !slot.collapsed {
                    let pr = plot_rect(rect);
                    let max = plot::autoscale_max(&d.rate);
                    let pts = plot::bar_points(&d.rate, max, pr.h.saturating_mul(4).max(4));
                    backend.plot(&pts, pr, Style::DEFAULT, frame);
                }
            }
        },
        SectionId::Threads => match &col3.threads {
            ThreadsView::NoSessions => title_line(frame, rect, "threads", "(no active sessions)"),
            ThreadsView::Count(n) => {
                title_line(frame, rect, "threads", &format!("{n} active"));
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_is_the_prd_order_by_registration() {
        let ids: Vec<SectionId> = SECTIONS.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![
                SectionId::TokenUsage,
                SectionId::InputReadOutput,
                SectionId::NicGauge,
                SectionId::RecallSavings,
                SectionId::RecallCalls,
                SectionId::Threads,
            ]
        );
    }

    #[test]
    fn tall_column_expands_everything() {
        let slots = solve_stack(40);
        assert_eq!(slots.len(), 6);
        assert!(slots.iter().all(|s| !s.collapsed));
        // Exact tiling: contiguous, inside the column.
        let mut y = 0;
        for s in &slots {
            assert_eq!(s.y, y);
            y += s.rows;
        }
        assert!(y <= 40);
    }

    #[test]
    fn short_column_collapses_low_priority_first() {
        // 16 rows: too tight for all six expanded (min sum = 20), enough for
        // High+Medium expanded with Low collapsed (min sum = 17? -> next tier).
        let slots = solve_stack(17);
        assert_eq!(slots.len(), 6, "every section still occupies a slot");
        let collapsed: Vec<SectionId> =
            slots.iter().filter(|s| s.collapsed).map(|s| s.id).collect();
        assert_eq!(
            collapsed,
            vec![SectionId::RecallCalls, SectionId::Threads],
            "the Low sections fold first"
        );
    }

    #[test]
    fn tighter_column_collapses_medium_too() {
        // High mins (4+4+3) + three collapsed lines = 14.
        let slots = solve_stack(14);
        let expanded: Vec<SectionId> = slots
            .iter()
            .filter(|s| !s.collapsed)
            .map(|s| s.id)
            .collect();
        assert_eq!(
            expanded,
            vec![
                SectionId::TokenUsage,
                SectionId::InputReadOutput,
                SectionId::NicGauge
            ],
            "only High sections stay expanded"
        );
        // One row tighter: even the High tier folds.
        assert!(solve_stack(13).iter().all(|s| s.collapsed));
    }

    #[test]
    fn six_rows_renders_all_six_collapsed() {
        let slots = solve_stack(6);
        assert_eq!(slots.len(), 6);
        assert!(slots.iter().all(|s| s.collapsed && s.rows == 1));
    }

    #[test]
    fn below_six_rows_drops_from_the_tail() {
        let slots = solve_stack(3);
        assert_eq!(slots.len(), 3);
        assert_eq!(slots[0].id, SectionId::TokenUsage);
        assert_eq!(slots[2].id, SectionId::NicGauge);
    }

    #[test]
    fn zero_height_is_empty() {
        assert!(solve_stack(0).is_empty());
    }

    proptest::proptest! {
        /// The solver tiles within the column at every height: slots are
        /// contiguous from the top, never exceed the height, and keep the
        /// fixed order.
        #[test]
        fn solver_tiles_exactly(h in 0u16..200) {
            let slots = solve_stack(h);
            let mut y = 0u16;
            for (i, s) in slots.iter().enumerate() {
                proptest::prop_assert_eq!(s.y, y);
                proptest::prop_assert!(s.rows >= 1);
                y = y.saturating_add(s.rows);
                // Order preserved by registration.
                proptest::prop_assert_eq!(s.id, SECTIONS[i].id);
            }
            proptest::prop_assert!(y <= h);
            // At 6+ rows every section is present (information, not fidelity).
            if h >= 6 {
                proptest::prop_assert_eq!(slots.len(), 6);
            }
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
