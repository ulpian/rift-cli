//! Column 3 — the live panels (the dashboard information architecture): the
//! earlier six sections regroup into **token usage** (the four-class
//! proportional treemap + Σ total + compact footer), **system** (context
//! pressure + recent activity + projection storage), and **recall** (savings %,
//! tokens avoided, and per-channel serve-call rates); the two workflow panels
//! join them.
//!
//! Each panel is an individually-bordered themed card, sized by the shared
//! [`crate::stack`] solver and **honest about absence**: a
//! data-less panel still occupies its slot with its placeholder — "no token
//! activity yet", the recall enable-prompt vs the visibly distinct
//! enabled-but-quiet state, the unknown-model indicator — never a fake zero.
//! Under height pressure a panel collapses to its one-line headline (fidelity
//! is lost, information is not). The donut rasterizes via the hi-res sub-cell
//! tier on SUBCELL-capable backends and degrades to the shipped horizontal
//! gauge bar elsewhere; its formula/bands stay owned by `rift-nic-gauge`
//! (the gauge's single owner).
//!
//! The types here are **filled by the data plane** (`rift-canopy-data`, which
//! imports this crate — never the reverse, the data-plane split) and rendered pure over
//! the [`crate::state::ViewModel`].

use crate::layout::Rect;
use crate::plot::{self, Paint};
use crate::render::frame::{Color, Frame, Style};
use crate::render::{Capabilities, CellShaderKind, RenderBackend};
use crate::theme::{self, Icon, Section, Theme};

/// The column-3 panels (the dashboard information architecture — the earlier
/// six sections regroup here, none of the data dropped, and the two workflow
/// panels join them).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionId {
    /// Token usage: the four-class proportional treemap + compact readouts.
    TokenUsage,
    /// System: context pressure + recent activity + projection storage.
    System,
    /// Recall: savings % + tokens avoided and per-channel serve-call rates.
    Recall,
    /// Dispatch: what this repository has captured — checkpoints (and how their
    /// times were known), sessions, the AI/MX/HU split, and the files that
    /// carried it. The card half of `rift dispatch`.
    Dispatch,
    /// Recap: how agents are being used — sessions by agent (or by model),
    /// tokens, cost, and recall's ledger. The card half of `rift recap`.
    Recap,
}

impl SectionId {
    /// The panels the default [`crate::layout_model::LayoutModel`] seats in
    /// column 3, in display order — today, every registered one.
    ///
    /// It is a **separate list from [`SECTIONS`]** and stays one even while the
    /// two agree. Registration is what the stack solver sizes and what a
    /// persisted layout may name; seating is what a fresh install draws; and a
    /// panel can be the first without being the second, which is how the two
    /// workflow panels were built, fed and tested before they moved a single
    /// golden frame.
    ///
    /// **It is not a free list, and that is the point of keeping the two
    /// apart.** Adding an id here rewrites every committed frame that draws the
    /// dashboard, so the seated set is a product decision rather than a
    /// consequence of registering a panel — the workflow pair was seated by an
    /// explicit owner decision, on the argument that a panel a user cannot reach
    /// is a green test over an invisible surface (the panel editor moves units;
    /// it cannot add one).
    ///
    /// **What seating costs the panels already here is not nothing**, and the
    /// two halves of that are worth keeping apart because only one of them is
    /// pinned by a test.
    ///
    /// Pinned: no panel drops below its **registered minimum** before a `Low`
    /// one folds. Both newcomers are [`Priority::Low`] and the solver folds
    /// every `Low` before any `Medium`, so
    /// `the_workflow_panels_fold_before_the_chart_panels` holds TokenUsage,
    /// System and Recall at 9/8/13 rows at height 32.
    ///
    /// Not pinned, and measured instead: the **surplus** above those minimums
    /// is now divided five ways, and the column's expanded content height goes
    /// 27 → 42 — so a column between those heights that used to fit now
    /// SCROLLS ([`crate::col3::max_scroll`], the scrollbar rule's viewport). Read off the
    /// `col3_tall` golden at 30 rows: TokenUsage 11 → 9, System 9 → 8, Recall
    /// 10 → 10, and the recap panel below the fold at offset 0. Two tests in
    /// this file had to grow their frames (33 → 52, 30 → 45) for exactly that
    /// reason: their assertions sat on rows seating took away.
    ///
    /// The Recall panel's own minimum has since gone 10 → 13 for the period
    /// row and the read-vs-saved bars it now carries, taking the expanded
    /// content height 42 → 45 and moving one more panel below the fold on a
    /// short column.
    pub const ALL: [SectionId; 5] = [
        SectionId::TokenUsage,
        SectionId::System,
        SectionId::Recall,
        SectionId::Dispatch,
        SectionId::Recap,
    ];
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
/// collapse priorities are defined: token usage (the chart +
/// tiles) and system (the donut) hold out longest; recall folds first.
///
/// The two workflow panels are `Low`: they are *tables of counts*, so a
/// collapsed one-line headline loses far less than a collapsed chart does, and
/// a column that has to shed rows should shed them where the loss is smallest.
/// The solver collapses every `Low` before any `Medium`, so seating them cannot
/// take a row from the three panels that were here first until the column is
/// too short for all five headlines.
pub const SECTIONS: [SectionSpec; 5] = [
    SectionSpec {
        id: SectionId::TokenUsage,
        weight: 3,
        min_rows: 9,
        priority: Priority::High,
    },
    SectionSpec {
        id: SectionId::System,
        weight: 3,
        min_rows: 8,
        priority: Priority::High,
    },
    // Thirteen, not ten: the panel now carries three content blocks over its
    // two headline rows — the period row, the read-vs-saved bars, and the
    // per-channel call chart. At the old minimum the chart was left three
    // rows, which is a legend and an axis with nothing between them. Raising
    // the *registered* minimum is how a panel that grew says so, rather than
    // taking the rows silently out of a chart that has no way to complain.
    SectionSpec {
        id: SectionId::Recall,
        weight: 2,
        min_rows: 13,
        priority: Priority::Medium,
    },
    SectionSpec {
        id: SectionId::Dispatch,
        weight: 2,
        min_rows: 7,
        priority: Priority::Low,
    },
    SectionSpec {
        id: SectionId::Recap,
        weight: 2,
        min_rows: 8,
        priority: Priority::Low,
    },
];

/// A section's stack sizing as a generic [`crate::stack::Spec`] — the single
/// source, so a data-driven column (the data-driven layout model) sizes each panel exactly as the
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
/// [`crate::stack`] solver (the same contract the column-2 card stack uses):
/// everything expanded when the minimums fit; under pressure Low then
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
/// over (the scrollbar rule).
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

/// One member's slice of a family figure.
///
/// The family total is the default everywhere; this is the row that makes it
/// auditable. The slice is a **count**, not a rate: it is never tweened and
/// never re-derived at draw time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeSlice {
    /// The member's admin name (`main` for the repository's main work tree).
    ///
    /// `None` is **unknown** — a session with no `session_worktree`
    /// observation, which is every session captured before worktree provenance. Surfaces
    /// print `unknown` for it, never `main` and never blank (worktree provenance);
    /// Sealed shards are never rewritten, so the history that would have named
    /// it can never be back-filled.
    pub name: Option<String>,
    /// The slice's tokens.
    pub tokens: u64,
}

/// A family figure decomposed by worktree — the "by worktree" axis
/// (the family view).
///
/// It is carried **inside** the view-model whose total it decomposes, so a
/// surface can never pair one read's total with another read's rows: the two
/// are computed in one pass over one set of facts, and
/// [`Self::total`] is that same total by construction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeAxis {
    /// One row per observed member, plus the `unknown` row when any fact
    /// carries no observation. Ordered largest first; equal rows put a named
    /// member before `unknown` and then sort by name — a total order, so the
    /// disclosure is a golden and not a hash walk.
    pub rows: Vec<WorktreeSlice>,
    /// Tokens that carry no session key **at all**, and so belong to the
    /// family and to no member.
    ///
    /// This is not "unknown": an unknown row is a session whose worktree was
    /// never observed, while this is a fact that has no session to observe.
    /// Recall serves are the live case — no serve relation records a session
    /// (`memd_audit_event`, `serve_call_event`, `serve_session_event` hold
    /// none), so the savings figure is the family's and the modal says so.
    pub family_only: u64,
}

impl WorktreeAxis {
    /// The decomposed total: every row plus the family-only remainder. Equal
    /// to the figure this axis decomposes — the property the feed's
    /// `the_breakdown_sums_to_the_family_total` pins.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.rows
            .iter()
            .map(|r| r.tokens)
            .fold(self.family_only, u64::saturating_add)
    }

    /// Whether there is nothing to disclose (no rows and no family-only
    /// remainder) — the axis of a figure that does not exist yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() && self.family_only == 0
    }
}

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
    /// [`Self::total`] decomposed by worktree (the family view), computed in
    /// the same pass over the same rows.
    pub by_worktree: WorktreeAxis,
}

/// The input | read | output breakdown ("read" = cache reads — the settled
/// answer to the label question; cache writes are the secondary band).
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
    /// read / (input + read), `0/0 ≡ 0`.
    pub read_fraction: f64,
}

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

/// The period the Recall panel's figures cover — a viewer's choice, not a
/// property of the data.
///
/// Every variant is a **cut**, and the cut is the only thing that leaves this
/// type: it is handed to `rift_recall_ledger::savings::compose` exactly the way
/// `rift savings --since` hands one in, so the dashboard runs the same
/// computation over a different window rather than a different computation.
/// Nothing here re-derives a share.
///
/// The three narrow windows are **trailing**, not calendar: a calendar "today"
/// needs a timezone this crate has no business holding, and four windows sitting
/// side by side that are not the same *kind* of measurement is worse than one of
/// them being spelled `24h` instead of `today`.
///
/// [`Self::AllTime`] is not "every row ever" — it is the window the family's
/// boundary marker names, which is what this card, `rift savings` and the status
/// table have always meant by "since the reset". It carries no cut of its own so
/// the marker still decides, and the default therefore renders exactly as it did
/// before the buttons existed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecallWindow {
    /// The trailing 24 hours.
    Today,
    /// The trailing 7 days.
    Last7Days,
    /// The trailing 30 days.
    Last30Days,
    /// The family's boundary marker, or the whole record when it has none.
    #[default]
    AllTime,
}

/// Milliseconds in a day — the trailing windows' unit.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

impl RecallWindow {
    /// The windows in button order, narrowest first.
    pub const ALL: [RecallWindow; 4] = [
        RecallWindow::Today,
        RecallWindow::Last7Days,
        RecallWindow::Last30Days,
        RecallWindow::AllTime,
    ];

    /// The button's glyphs — short, because four of them share one row.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RecallWindow::Today => "24h",
            RecallWindow::Last7Days => "7d",
            RecallWindow::Last30Days => "30d",
            RecallWindow::AllTime => "all",
        }
    }

    /// What this window resolves to against one reading of the clock.
    ///
    /// It answers with three cases rather than an `Option<i64>` because there
    /// are three, and the middle one used to be spelled as the first: a
    /// trailing window the clock cannot place was clamped at the epoch, which
    /// is "every row ever" — so a failed clock drew the whole record under a
    /// button that says `24h`. The cases are now distinguishable by the type,
    /// and the surface draws the honest empty window for the third.
    #[must_use]
    pub fn cut(self, now_ms: i64) -> WindowCut {
        let days = match self {
            RecallWindow::Today => 1,
            RecallWindow::Last7Days => 7,
            RecallWindow::Last30Days => 30,
            RecallWindow::AllTime => return WindowCut::Marker,
        };
        let start = now_ms.saturating_sub(DAY_MS.saturating_mul(days));
        if start < 0 {
            // Before the epoch: either the clock read `0` because the platform
            // refused it, or this machine believes it is January 1970. Neither
            // places a trailing window on the record's timeline.
            return WindowCut::NoClock;
        }
        WindowCut::Since(start)
    }
}

/// What a [`RecallWindow`] resolves to against one clock reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowCut {
    /// No cut of its own: the family's boundary marker decides, which is what
    /// [`RecallWindow::AllTime`] has always meant.
    Marker,
    /// A trailing cut at this instant, to be composed with the marker.
    Since(i64),
    /// The clock could not place this trailing window. **Not "everything"** —
    /// absence, drawn as the empty window it is and labelled as such.
    NoClock,
}

/// The unit the Recall card's bar chart is read in — a viewer's choice, like
/// the period beside it.
///
/// Purely a *rendering* choice: both figures ride the same feed frame, so
/// flipping it costs no read and cannot show two windows at once. It is also
/// why the toggle emits no data request while the period buttons do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecallUnit {
    /// Token counts, the unit both figures are actually measured in.
    #[default]
    Tokens,
    /// The same figures valued in US dollars — an estimate over an estimate,
    /// and marked as one by the mandatory leading tilde the money authority
    /// renders.
    Usd,
}

impl RecallUnit {
    /// The chip's glyphs — what the bars are showing *now*, not what a press
    /// would switch to. A control labelled with its own effect and a control
    /// labelled with its own state are both defensible; showing the state is
    /// the one that still reads correctly when nobody is about to press it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            RecallUnit::Tokens => "tok",
            RecallUnit::Usd => "usd",
        }
    }

    /// The other unit — what a press selects.
    #[must_use]
    pub fn flipped(self) -> RecallUnit {
        match self {
            RecallUnit::Tokens => RecallUnit::Usd,
            RecallUnit::Usd => RecallUnit::Tokens,
        }
    }
}

/// A recall-derived section: the three honest states.
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

/// One per-model row composing the USD savings estimate (the priced estimate):
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

/// One family's row in the savings-share block of the breakdown modal.
///
/// The share pairs the serving relation's avoided tokens with the capture
/// projection's read tokens, joined on the family NAME and nothing else — the
/// modal says so above the rows rather than implying a key that does not
/// exist.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FamilyShareRow {
    /// The family, as the relation that named it spells it.
    pub family: String,
    /// `avoided ÷ (avoided + read)`, `None` when neither side holds a token.
    pub share: Option<f64>,
    /// Avoided minus returned, for this family. Signed: an observe window
    /// that injected and declined nothing is legitimately negative.
    pub honesty: i64,
    /// Sessions of this family that read a file — the `n` behind its share.
    pub sessions_with_reads: u64,
    /// It decided and its reads were never captured, so it has a numerator
    /// and no denominator of its own. Named, never silently dropped.
    pub decided_without_reads: bool,
}

/// The savings share: what fraction of the tokens this repository's agents
/// would have read the recall plane avoided, over the card's window.
///
/// **It is the card's headline**, not a second opinion beside one. Every figure
/// here was computed by `rift_recall_ledger::savings`, which the CLI report,
/// the `savings` verb and the recall status table also call — one computation,
/// so the dashboard and the terminal cannot show the owner two numbers. The
/// card used to publish a second percentage of its own, `avoided ÷ (avoided +
/// returned)` over the audit trail, which is a different question with a
/// different denominator and looked exactly like this one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ShareData {
    /// The window the figures cover, when a boundary marker narrows it to less
    /// than the whole record. `None` is the whole record — the wording the
    /// card has always used — so an absent field reads as it always did.
    pub window: Option<String>,
    /// The headline share, over the audit relation. `None` is unanswerable —
    /// nothing read and nothing avoided — and draws no chip at all.
    pub overall: Option<f64>,
    /// Avoided minus returned over the same relation.
    pub honesty: i64,
    /// The numerator: Σ avoided over the audit relation, in tokens.
    ///
    /// It is carried beside [`Self::read_tokens`] rather than re-derived from
    /// [`Self::overall`] and the read side, because the share is a rounded
    /// percentage and inverting it would put a token count on the card that no
    /// relation ever held. Both come out of one `savings::compose` call, so the
    /// two bars and the chip beside them describe one window.
    pub avoided_tokens: u64,
    /// The read side of the denominator.
    pub read_tokens: u64,
    /// Sessions that read a file, over every family.
    pub sessions_with_reads: u64,
    /// Reads in the window that carry no token count at all.
    ///
    /// They sit in the denominator's row set and weigh nothing in it, so the
    /// share is over fewer measured reads than it looks. Published beside the
    /// figure rather than imputed into it — a count nobody froze is not a
    /// count this dashboard will invent.
    pub reads_unmeasured: u64,
    /// The same share over the family rows alone — the comparable twin of
    /// [`Self::excluding`], summed from the same rows.
    pub by_family: Option<f64>,
    /// [`Self::by_family`] with every family that decided and whose reads were
    /// never captured left out of both sides.
    pub excluding: Option<f64>,
    /// Those families, named so the exclusion is auditable.
    pub excluded: Vec<String>,
    /// The per-family rows the modal draws.
    pub families: Vec<FamilyShareRow>,
}

/// Recall savings numbers.
///
/// Every figure here describes **one** window, resolved once by the data plane
/// (the card's period button composed with the family's boundary marker), so
/// the headline, the bars, the money and the modal cannot cover two periods.
/// Nothing on this struct is tweened: they are headlines, and a measurement
/// mid-animation is a number no window ever held.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SavingsData {
    /// Estimated USD saved, integer micro-USD: the sum of the
    /// priced [`UsdRow`] subtotals. `None` is honest absence — no resolvable
    /// pricing basis — and draws no chip.
    pub usd_saved_micros: Option<i64>,
    /// The chip label, pre-rendered by the money authority (`"~$105.90"`).
    /// `Some` exactly when [`Self::usd_saved_micros`] is.
    pub usd_label: Option<String>,
    /// The per-model rows whose priced subtotals compose the headline
    /// (empty exactly when [`Self::usd_saved_micros`] is `None`).
    pub usd_rows: Vec<UsdRow>,
    /// [`ShareData::read_tokens`] valued through the **same** model mix and
    /// the same uncached-input rates as [`Self::usd_saved_micros`], in
    /// micro-USD. `None` is honest absence, and the bar chart falls back to
    /// tokens rather than drawing one priced bar beside one unpriced one.
    ///
    /// One basis, read twice — never a second price table. The two bars are
    /// comparable in dollars only because their tokens were partitioned
    /// across the same models at the same moment.
    pub usd_read_micros: Option<i64>,
    /// The read bar's label, pre-rendered by the money authority (`"~$12.40"`,
    /// tilde mandatory). `Some` exactly when [`Self::usd_read_micros`] is.
    pub usd_read_label: Option<String>,
    /// The avoided total decomposed by worktree (the family view). Today every
    /// avoided token lands in [`WorktreeAxis::family_only`], because no serve
    /// relation records the session that was served; the breakdown modal
    /// discloses that rather than attributing a serve to a guess.
    pub by_worktree: WorktreeAxis,
    /// The product's stated savings target, as a whole percentage — the card
    /// says what it is aiming at beside what it reached.
    ///
    /// It arrives from `rift_recall_ledger::savings::SAVINGS_TARGET_PCT`,
    /// because this crate depends on `rift-core` and the registry and on
    /// nothing else: a number the product publishes is data, and data reaches
    /// the render layer on the view model like every other figure here.
    /// `None` draws no target at all — a target of zero is not a target.
    pub target_pct: Option<u8>,
    /// The savings share the owner judges this plane by, overall and per
    /// family — **the card's headline figure**. Absent figures draw nothing.
    ///
    /// **Boxed**, like `DetailView::project_settings` and for the same
    /// reason: this is much the largest part of the struct, and
    /// `FeedPayload::Recall` carries the struct through the update channel.
    /// Unboxed it made the Recall variant more than twice the next largest
    /// and every other feed paid for it.
    pub share: Box<ShareData>,
}

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

/// A single trailing-month day of activity for the three heat strips.
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

/// Capture rows a trailing-month strip could **not place on a day**: they fall
/// outside the window, or their time column is not a time this plane can read.
///
/// # Why a strip owes its reader a remainder
///
/// The strips are built by filtering a time column, and a filter drops what it
/// does not match — silently. That is fine while every row carries a time, and
/// it is a lie the moment one does not: `checkpoint.created_at` carries an
/// explicit unknown-time sentinel, so a strip filtering on it renders thirty
/// zeros for a repository that made a hundred checkpoints, three cards away
/// from the capture tile that says a hundred. Measured 2026-08-18 on the
/// owner's projection: **0 of 103** checkpoints placed, beside 15,887 events
/// and 14 sessions placed by the same read.
///
/// So the remainder is counted and shown rather than dropped. It deliberately
/// does **not** claim *why* a row is off the strip — "older than the window"
/// and "undated" are different facts, and this plane can only tell you that a
/// row is not on the strip. Splitting them needs the checkpoint-time derivation
/// that `rift dispatch` owns (see the crate docs' open-gap note); until that is
/// shared, the honest word is the one this type uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OffStrip {
    /// Captured events not placed on any day of the strip.
    pub events: u64,
    /// Sessions not placed on any day of the strip.
    pub sessions: u64,
    /// Checkpoints not placed on any day of the strip.
    pub checkpoints: u64,
}

impl OffStrip {
    /// Whether anything at all was left off the strip.
    #[must_use]
    pub const fn any(self) -> bool {
        self.events > 0 || self.sessions > 0 || self.checkpoints > 0
    }
}

/// The activity card's data: the trailing thirty calendar days, oldest first.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActivityData {
    /// Thirty days of heat-strip values (oldest → today). An absent day is a
    /// zero-count day (a fact), never dropped.
    pub days: Vec<DayBar>,
    /// What the thirty days do **not** account for — see [`OffStrip`]. Zero on
    /// a month whose every row landed on a day, which is why a fully-placed
    /// card renders exactly as it did before this field existed.
    pub off_strip: OffStrip,
}

/// The activity card view-model. Honest about absence: a **missing
/// projection** is the placeholder (never thirty zeros); a readable projection
/// with quiet days is `Days` with zero-count strips.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum ActivityView {
    /// No readable projection — the placeholder (distinct from a quiet month).
    #[default]
    NoProjection,
    /// The trailing month's three heat strips.
    Days(ActivityData),
}

// --- the workflow panels' view-models ---------------------------------------
//
// These two carry `rift dispatch` / `rift recap` facts, and they carry them as
// FINISHED TEXT: every label below has already been through
// [`crate::escape_for_terminal`] in the data plane, and every money figure has
// already been through the cost crate's formatter. A renderer that had to
// escape or to format would be a second place either rule could be got wrong.
//
// **The cards never re-derive a citation ordinal.** `rift dispatch --json` and
// `rift recap --json` replace payload-derived strings with `cite:<n>` and print
// the map on their human surface; a card is a human surface reading the data
// plane directly, so it shows the value itself. What it must never do is print
// or invent an ordinal — a dashboard and a document that disagreed about which
// value `cite:3` names would be worse than either alone.

/// One touched file, as the dispatch panel lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileTouch {
    /// The repository path, already escaped for a terminal.
    pub label: String,
    /// Distinct checkpoints that touched it.
    pub checkpoints: i64,
    /// Whether this repository's policy calls the path sensitive.
    pub sensitive: bool,
}

/// The dispatch panel's numbers, over the window the feed gathered.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DispatchData {
    /// Checkpoints in the window.
    pub checkpoints: u64,
    /// How many of those were placed by the sessions they archive rather than
    /// by a time the shard recorded. A derived time is a lower bound and the
    /// card says so rather than passing it off as a record.
    pub derived: u64,
    /// Checkpoints in this repository whose time cannot be resolved at all —
    /// repository-wide, so "excluded" never reads as "absent".
    pub unresolved: u64,
    /// Sessions in the window.
    pub sessions: u64,
    /// Windowed sessions with no `end` event ever recorded.
    pub open_sessions: u64,
    /// Lines the agent added (AI).
    pub agent_lines: i64,
    /// Agent lines a human then modified (MX).
    pub mixed_lines: i64,
    /// Lines the human added (HU).
    pub human_lines: i64,
    /// Mean per-file attribution confidence in milli-units. **`None` is not a
    /// zero**: no file in the window carries an attribution row, so nothing was
    /// measured.
    pub confidence_milli: Option<i64>,
    /// How many distinct paths the window touched — **all** of them, not the
    /// length of [`DispatchData::files`]. The list is budgeted for what crosses
    /// the bus; this is the fact the card states, so a prefix is never shown as
    /// a total.
    pub files_total: u64,
    /// The top touched files, ranked by touch count then path — the window's
    /// own total order, so the card lists them exactly as the report does.
    pub files: Vec<FileTouch>,
    /// How many touched paths this repository's policy calls sensitive —
    /// `None` when the policy was never ASKED (no readable config, so the
    /// gather left the question open). An unasked answer
    /// renders as "sensitive unasked", never as a zero that claims the window
    /// was checked and found clean.
    pub sensitive: Option<u64>,
}

/// The dispatch panel. Honest about absence: an unreadable
/// projection is the placeholder, a readable one holding nothing is the
/// visibly distinct empty state, and neither is ever a zero line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DispatchView {
    /// No readable capture projection for this repository.
    #[default]
    NoProjection,
    /// A readable projection with no checkpoint and no session in it.
    NoActivity,
    /// The window's facts.
    Data(DispatchData),
}

/// Recall's ledger arithmetic as the recap panel shows it. Absent (`None` on
/// [`RecapData::savings`]) means this repository has no ledger — never a zero.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RecapSavings {
    /// Tokens the estimator says serves avoided. **An estimated upper bound**
    /// and never a claim — the owner's standing rule for this figure.
    pub avoided_upper_bound: u64,
    /// Tokens spent injecting manifests — real spend, never credit.
    pub injected: u64,
}

/// The recap panel's numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecapData {
    /// Sessions in the window.
    pub sessions: u64,
    /// Checkpoints in the window.
    pub checkpoints: u64,
    /// Every token class summed.
    pub tokens: u64,
    /// The window's cost, already formatted by the money authority. `None` when
    /// there is no honest figure — never a fabricated `~$0.00`.
    pub cost_label: Option<String>,
    /// Whether [`RecapData::split`] is keyed by model rather than by agent.
    pub by_model: bool,
    /// How many distinct keys the split has — all of them, for the same reason
    /// [`DispatchData::files_total`] exists.
    pub split_total: u64,
    /// The top `(key, sessions)` pairs in key order — already escaped, and the
    /// store's one `session_split`, so the card and `rift recap` cannot
    /// disagree.
    pub split: Vec<(String, u64)>,
    /// Recall's ledger, or `None` when this repository has none.
    pub savings: Option<RecapSavings>,
}

/// The recap panel, with the same absence rules as
/// [`DispatchView`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RecapView {
    /// No readable capture projection for this repository.
    #[default]
    NoProjection,
    /// A readable projection with no session and no checkpoint in it.
    NoActivity,
    /// The window's facts.
    Data(RecapData),
}

/// The whole column-3 view-model the data plane fills (Arc-shared through the
/// snapshot; `Default` is every section's honest placeholder).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Col3 {
    /// How the data bus is woken: `PollOnly` adds a
    /// dim "watch: poll-only" row to the System panel (and a head suffix when
    /// collapsed); `Native` draws nothing — the default, so every golden that
    /// predates the field is unchanged.
    pub watch: crate::state::WatchMode,
    /// Token usage.
    pub tokens: TokensView,
    /// Input | read | output.
    pub breakdown: BreakdownView,
    /// The nic-gauge.
    pub nic: NicView,
    /// Recall savings.
    pub savings: RecallView<SavingsData>,
    /// Recall serve calls.
    pub calls: RecallView<CallsData>,
    /// The unit the Recall panel's bar chart is read in. A viewer's choice,
    /// set by the reducer; no feed writes it, and flipping it needs no read
    /// because both figures ride the same frame.
    pub recall_unit: RecallUnit,
    /// The period the Recall panel's figures were computed over.
    ///
    /// A **viewer's choice**, so the reducer sets it and no feed ever does:
    /// the panel must be able to say which window it is drawing before the
    /// bus has answered for it, or the buttons would lag a frame behind the
    /// press. The default is [`RecallWindow::AllTime`], which is what the card
    /// showed before there were buttons.
    pub recall_window: RecallWindow,
    /// Top-level capture sessions currently live.
    pub active_sessions: ActiveSessionsView,
    /// The system panel's four domain metrics. Headlines — taken from
    /// the target immediately, never tweened.
    pub metrics: PanelMetrics,
    /// The dispatch panel's window facts.
    pub dispatch: DispatchView,
    /// The recap panel's window facts.
    pub recap: RecapView,
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
    // Token composition, Nic and the recall savings deliberately snap: every
    // proportional area, headline and compact readout must describe the same
    // atomic feed frame. Only the per-channel call chart is a series over time
    // and therefore the only thing on this card with motion to show.
    matches!((&from.calls, &to.calls),
            (RecallView::Data(a), RecallView::Data(b)) if calls_can_animate(a, b)
                && a.series.iter().zip(&b.series).any(|(x, y)| x.rate != y.rate))
}

/// The displayed `Col3` at tween progress `t ∈ [0, 1]`: `from`'s series eased
/// toward `to`'s, with all headlines/variants taken from `to`.
#[must_use]
pub(crate) fn interpolate(from: &Col3, to: &Col3, t: f32) -> Col3 {
    let t = f64::from(t.clamp(0.0, 1.0));
    Col3 {
        // Not tweened: the wake mode is a fact, read from the target.
        watch: to.watch,
        tokens: to.tokens.clone(),
        breakdown: to.breakdown.clone(),
        // The nic-gauge snaps: its pct is a headline (title + fill share it).
        nic: to.nic.clone(),
        // The savings card snaps whole. Every figure on it is a headline over
        // one window — the share, the money and its rows, the axis of counts —
        // and a frame between two measurements is a number no window held.
        savings: to.savings.clone(),
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
                    .map(|(x, y)| ChannelCallSeries {
                        channel: y.channel.clone(),
                        last_call_at: y.last_call_at,
                        rate: lerp_series(&x.rate, &y.rate, t).unwrap_or_else(|| y.rate.clone()),
                    })
                    .collect(),
            }
        }),
        active_sessions: to.active_sessions.clone(),
        // Not tweened: the chosen window and unit are the viewer's, read from
        // the target so the pressed control lights in the frame of the press.
        recall_window: to.recall_window,
        recall_unit: to.recall_unit,
        // Domain metrics are headlines — taken from the target immediately.
        metrics: to.metrics.clone(),
        // The workflow panels carry no plottable series at all: every value in
        // them is a count, a split or a formatted figure, and easing a count
        // would put a number on screen that no window ever yielded. They take
        // the target, like every other headline here.
        dispatch: to.dispatch.clone(),
        recap: to.recap.clone(),
    }
}

/// Calls may ease only when every sample still denotes the same time bucket
/// and serve channel. A shifted window or reordered channel set snaps.
fn calls_can_animate(a: &CallsData, b: &CallsData) -> bool {
    a.window_end_ms == b.window_end_ms
        && a.series.len() == b.series.len()
        && a.series
            .iter()
            .zip(&b.series)
            .all(|(x, y)| x.channel == y.channel && x.rate.len() == y.rate.len())
}

// --- rendering ---------------------------------------------------------------

/// Compact k/M formatting for token-scale numbers.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
pub fn fmt_km(v: f64) -> String {
    let v = if v.is_finite() { v.max(0.0) } else { 0.0 };
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 10_000.0 {
        format!("{:.0}k", v / 1_000.0)
    } else if v >= 1_000.0 {
        format!("{:.1}k", v / 1_000.0)
    } else {
        format!("{}", v.round() as u64)
    }
}

/// Render the column-3 panels into `area` through the plot seam. Each panel
/// is an individually-bordered themed card, sized by the
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
    // Overflow (the scrollbar rule): render the fully-expanded stack into a scratch frame,
    // then blit the [offset, offset+viewport) window — so "j/k scroll" moves the
    // panels instead of collapsing them, with the scrollbar as its sign.
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
/// among cards, panel-editor mode) shares with the homogeneous panel stack.
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
        SectionId::Dispatch => draw_dispatch_panel(col3, theme, focused, collapsed, rect, frame),
        SectionId::Recap => draw_recap_panel(col3, theme, focused, collapsed, rect, frame),
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

// --- token-usage panel ----------------------------------------------

/// The token-usage title's fixed lead, up to and including the two pad cells
/// before the `Σ` total. Load-bearing: [`token_total_rect`] measures the
/// clickable span by subtracting it, so the drawn glyphs and the click cells
/// stay the same cells.
const TOKEN_TITLE_LEAD: &str = "token usage  ";

/// The token-usage panel's title — `token usage` alone with no facts in
/// scope, else with the `Σ` total appended.
fn token_title(col3: &Col3) -> String {
    match &col3.tokens {
        TokensView::Data(d) => format!("{TOKEN_TITLE_LEAD}Σ {}", fmt_km(d.total)),
        TokensView::NoActivity => "token usage".to_string(),
    }
}

/// The cells the token panel's `Σ …` total occupies on its title row — the
/// "by worktree" axis's click affordance (the family view).
///
/// The total **is** the affordance: the axis decomposes exactly that number,
/// so the number is what opens the decomposition. It costs no extra cells,
/// which is why the disclosure survives where a separate `[…]` chip could not
/// — column 3 is 34 cells wide in the goldens, leaving 29 for a title, and
/// `token usage  Σ 7.5k  [by worktree]` needs 34. The `w` key
/// (`keymap::GRAPHS`) is the same act from the keyboard, and it is available
/// whether or not this rect is.
///
/// **One solver** feeds the draw and the hit-test over
/// [`crate::chrome::panel`]'s own geometry (the title sits at `x + 3` on the
/// top border row, clipped to the span between the corners), so a click can
/// never land a cell away from the glyphs. `None` — and therefore no target
/// at all — with no total in scope, on a panel too small to carry a border,
/// or when the title would be clipped (a truncated affordance is not one).
///
/// `rect` is the **section's outer rect**, not its inner: the title lives on
/// the border row.
#[must_use]
pub fn token_total_rect(col3: &Col3, rect: Rect) -> Option<Rect> {
    if rect.w < 2 || rect.h < 2 {
        return None; // `chrome::panel` draws no frame, so there is no title
    }
    if !matches!(col3.tokens, TokensView::Data(_)) {
        return None; // no total, nothing to decompose, no target
    }
    let title = token_title(col3);
    let span_start = rect.x.checked_add(2)?;
    let span_end = rect.x.checked_add(rect.w)?.checked_sub(2)?;
    if span_end <= span_start {
        return None;
    }
    let avail = usize::from(span_end.checked_sub(span_start)?.checked_add(1)?).checked_sub(2)?;
    let len = title.chars().count();
    if len > avail {
        return None; // clipped by `chrome::panel` — never a partial chip
    }
    let lead = TOKEN_TITLE_LEAD.chars().count();
    let w = u16::try_from(len.checked_sub(lead)?).ok()?;
    let x = span_start
        .checked_add(1)?
        .checked_add(u16::try_from(lead).ok()?)?;
    Some(Rect {
        x,
        y: rect.y,
        w,
        h: 1,
    })
}

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
    // Σ total in the title (from the token totals); the treemap + footer from
    // the four-class breakdown. They arrive together, so breakdown gates data.
    let title = token_title(col3);
    let inner = panel_frame(theme, Section::TokenUsage, focused, &title, rect, frame);
    // The Σ total is the "by worktree" axis's affordance (the family view):
    // re-drawn over its own cells from the SAME solver the hit-test reads, in
    // the key-hint style that marks an act everywhere else in the dashboard.
    // Flashless by design, like the USD chip: col3 carries no press-pulse
    // plumbing, by a recorded decision.
    if let Some(chip) = token_total_rect(col3, rect) {
        let sigma: String = title
            .chars()
            .skip(TOKEN_TITLE_LEAD.chars().count())
            .collect();
        frame.put_str(chip.x, chip.y, &sigma, theme.key_hint());
    }
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
    let vals = [d.input, d.read, d.output, d.cache_write];
    let footer_rows = 2u16;
    if inner.h >= footer_rows + 4 && inner.w >= 16 {
        let chart_area = Rect {
            x: inner.x,
            y: inner.y,
            w: inner.w,
            h: inner.h - footer_rows,
        };
        let plot_rect = crate::render::treemap_outline(frame, chart_area, theme.dim(), unicode);
        let leaves = crate::charts::treemap_layout(&vals);
        let samples = crate::charts::treemap_samples(&leaves, plot_rect, &theme::BREAKDOWN);
        backend.shade_cells(&samples, plot_rect, CellShaderKind::Braille, frame);
        let symbols = ['I', 'R', 'O', 'C'];
        for leaf in leaves {
            let label_rect = crate::charts::treemap_cell_rect(leaf, plot_rect);
            if label_rect.w == 0 || label_rect.h == 0 {
                continue;
            }
            let label_style = theme.tile_value(
                theme::BREAKDOWN
                    .get(leaf.index)
                    .copied()
                    .unwrap_or(theme.graph_accent()),
            );
            frame.set(
                label_rect.x,
                label_rect.y,
                symbols.get(leaf.index).copied().unwrap_or('T'),
                label_style,
            );
        }
    }
    draw_token_footer(
        theme,
        d,
        read_pct,
        Rect {
            y: inner.y + inner.h.saturating_sub(footer_rows),
            h: footer_rows.min(inner.h),
            ..inner
        },
        frame,
    );
}

fn draw_token_footer(theme: Theme, d: &BreakdownData, read_pct: u8, rect: Rect, frame: &mut Frame) {
    if rect.h == 0 {
        return;
    }
    let cats = &theme::BREAKDOWN;
    let color = |index: usize| cats.get(index).copied().unwrap_or(theme.graph_accent());
    let values = [
        fmt_km(d.input),
        fmt_km(d.read),
        fmt_km(d.output),
        fmt_km(d.cache_write),
    ];
    let full_labels = ["input", "read", "output", "cache-write"];
    let compact_labels = ["in", "rd", "out", "ca"];
    let use_full_labels = footer_pair_width(full_labels[0], &values[0], full_labels[1], &values[1])
        <= usize::from(rect.w)
        && footer_pair_width(full_labels[2], &values[2], full_labels[3], &values[3])
            <= usize::from(rect.w);
    let labels = if use_full_labels {
        full_labels
    } else {
        compact_labels
    };
    let end = rect.x.saturating_add(rect.w);
    let pct = format!("read {read_pct}%");
    let row_one_width = footer_pair_width(labels[0], &values[0], labels[1], &values[1]);
    let row_two_width = footer_pair_width(labels[2], &values[2], labels[3], &values[3]);
    let pct_width = pct.len();
    let pct_row = if row_one_width + 1 + pct_width <= usize::from(rect.w) {
        Some(0u16)
    } else if rect.h >= 2 && row_two_width + 1 + pct_width <= usize::from(rect.w) {
        Some(1u16)
    } else {
        None
    };
    let row_end = |row: u16| {
        if pct_row == Some(row) {
            end.saturating_sub(u16::try_from(pct_width + 1).unwrap_or(rect.w))
        } else {
            end
        }
    };
    draw_footer_pair(
        frame,
        rect.x,
        row_end(0),
        rect.y,
        [
            (labels[0], values[0].as_str(), color(0)),
            (labels[1], values[1].as_str(), color(1)),
        ],
        theme,
    );
    if rect.h >= 2 {
        draw_footer_pair(
            frame,
            rect.x,
            row_end(1),
            rect.y + 1,
            [
                (labels[2], values[2].as_str(), color(2)),
                (labels[3], values[3].as_str(), color(3)),
            ],
            theme,
        );
    }
    if let Some(row) = pct_row {
        let x = end.saturating_sub(u16::try_from(pct_width).unwrap_or(rect.w));
        put_footer_str(
            frame,
            x,
            end,
            rect.y + row,
            &pct,
            theme.tile_value(color(1)),
        );
    }
}

fn footer_pair_width(label_a: &str, value_a: &str, label_b: &str, value_b: &str) -> usize {
    label_a.len() + value_a.len() + label_b.len() + value_b.len() + 3
}

fn draw_footer_pair(
    frame: &mut Frame,
    x: u16,
    end: u16,
    y: u16,
    items: [(&str, &str, Color); 2],
    theme: Theme,
) {
    let [(first_label, first_value, first_color), (second_label, second_value, second_color)] =
        items;
    let x = footer_item(
        frame,
        x,
        end,
        y,
        first_label,
        first_value,
        first_color,
        theme,
    );
    let x = put_footer_str(frame, x, end, y, " ", theme.dim());
    footer_item(
        frame,
        x,
        end,
        y,
        second_label,
        second_value,
        second_color,
        theme,
    );
}

fn footer_item(
    frame: &mut Frame,
    x: u16,
    end: u16,
    y: u16,
    label: &str,
    value: &str,
    color: Color,
    theme: Theme,
) -> u16 {
    let x = put_footer_str(frame, x, end, y, label, Style::fg(color));
    let x = put_footer_str(frame, x, end, y, " ", theme.dim());
    put_footer_str(frame, x, end, y, value, theme.tile_value(color))
}

fn put_footer_str(frame: &mut Frame, x: u16, end: u16, y: u16, text: &str, style: Style) -> u16 {
    if x >= end {
        return x;
    }
    let clipped: String = text.chars().take(usize::from(end - x)).collect();
    frame.put_str(x, y, &clipped, style).min(end)
}

// --- system panel: the donut + the domain metrics -------------------

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
    let dot = Icon::Bullet.glyph(unicode);
    frame.set(inner.x, inner.y + dy, dot, theme.dim());
    body(
        frame,
        Rect {
            x: inner.x + 1,
            w: inner.w.saturating_sub(1),
            ..inner
        },
        dy,
        " watch: poll-only",
        theme.dim(),
    );
}

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
        body(
            frame,
            inner,
            0,
            &system_head(col3, &active_label),
            theme.body(),
        );
        return;
    }
    // The donut occupies the top; the domain metrics the bottom rows — plus,
    // where the panel is tall enough, one hint row naming the rates' scope
    // current-session facts are labelled directly; completed-session facts
    // hold dimmed without ever being called idle.
    let m = &col3.metrics;
    let hint = inner.h >= 8;
    let poll_only = col3.watch == crate::state::WatchMode::PollOnly;
    let metric_rows = 4u16 + u16::from(hint) + u16::from(poll_only);
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
            "serves · 5m",
            m.serve_calls_5m.map(|value| value.to_string()),
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
    if poll_only {
        draw_watch_row(theme, unicode, inner, rows_base + 4, frame);
    }
}

/// Whether a metric row carries a real value (not the `—` placeholder).
fn value_present(text: &str) -> bool {
    !text.ends_with('—')
}

/// Draw context pressure: a radial donut on SUBCELL-capable backends, degrading
/// to the shipped horizontal gauge bar elsewhere; the unknown-model indicator
/// and the no-data placeholder render explicitly (never a percentage; the gauge's single owner).
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

// --- recall panel ---------------------------------------------------

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
    let inner = panel_frame(
        theme,
        Section::Recall,
        focused,
        recall_title(col3),
        rect,
        frame,
    );
    // The period row is drawn BEFORE the placeholder early-return, and that
    // ordering is load-bearing: a narrow window over a quiet ledger puts the
    // card *into* the placeholder state, and buttons that vanished there would
    // leave the only way back on the keyboard.
    draw_recall_window_row(col3, theme, collapsed, inner, frame);
    if draw_recall_placeholder(col3, theme, inner, frame) {
        return;
    }
    let (savings, call_total, sessions) = recall_headlines(col3);
    if collapsed || inner.w < 28 || inner.h < 7 {
        body(frame, inner, 0, &savings, theme.body());
        if inner.w >= 30 {
            body(
                frame,
                inner,
                1,
                &format!("{call_total} serves · {sessions}"),
                theme.body(),
            );
        } else {
            body(
                frame,
                inner,
                1,
                &format!("{call_total} serves"),
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
    // The USD estimate chip: drawn from the same solver the
    // hit-test reads, so pixels == target. Absent label → no chip at all
    // (honest absence, never a fabricated figure). Flashless by design:
    // col3 carries no press-pulse plumbing, by a recorded decision.
    if let Some(chip) = recall_cost_rect(col3, collapsed, inner) {
        if let RecallView::Data(sd) = &col3.savings {
            if let Some(label) = sd.usd_label.as_deref() {
                frame.put_str(chip.x, chip.y, &format!("[{label}]"), theme.key_hint());
            }
        }
    }
    // The savings-share chip, from its own solver on the same row: the share
    // of would-have-been-read tokens this plane avoided. Absent figure → no
    // chip, like the money.
    if let Some(chip) = recall_share_rect(col3, collapsed, inner) {
        if let RecallView::Data(sd) = &col3.savings {
            if let Some(label) = share_chip_label(sd) {
                frame.put_str(chip.x, chip.y, &label, theme.key_hint());
            }
        }
    }
    body(
        frame,
        inner,
        1,
        &format!("{call_total} serves · {sessions}"),
        theme.body(),
    );
    let bar_rows = draw_recall_bars(col3, theme, unicode, collapsed, inner, backend, frame);
    let chart_dy = RECALL_CHART_DY.saturating_add(bar_rows);

    let RecallView::Data(calls) = &col3.calls else {
        body(
            frame,
            inner,
            chart_dy.saturating_add(1),
            "(no attributed serves yet)",
            theme.dim(),
        );
        return;
    };
    if calls.series.is_empty() {
        body(
            frame,
            inner,
            chart_dy.saturating_add(1),
            "(no calls in window)",
            theme.dim(),
        );
        return;
    }

    let chart = Rect {
        x: inner.x,
        y: inner.y + chart_dy,
        w: inner.w,
        h: inner.h.saturating_sub(chart_dy),
    };
    draw_call_chart(calls, theme, unicode, chart, backend, frame);
}

/// The inner row the period buttons sit on, under the headline and the serve
/// count.
const RECALL_WINDOW_DY: u16 = 2;

/// The first inner row the per-channel call chart may use.
const RECALL_CHART_DY: u16 = RECALL_WINDOW_DY + 1;

/// One period button on the Recall card: which window it selects and the exact
/// cells it occupies.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RecallWindowButton {
    /// The period a press selects.
    pub window: RecallWindow,
    /// Its exact clickable rectangle.
    pub rect: Rect,
}

/// The Recall card's period buttons — `[24h][7d][30d][all]` — solved once for
/// both the draw and the hit-test, so the cells that look pressable are the
/// cells that answer a press.
///
/// Chips **abut**, the rail toolbar's rule: a gap would clip the rightmost
/// button first, and losing "all" — the way back to the widest window — is the
/// worst one to lose.
///
/// Empty, and therefore no buttons anywhere, in the collapsed/narrow tier (the
/// card is already shedding text there, exactly as the two headline chips are)
/// or when the row would not fit. The keys stay bound in both cases; nothing
/// here is the only door to a period.
#[must_use]
pub fn recall_window_buttons(collapsed: bool, inner: Rect) -> Vec<RecallWindowButton> {
    if collapsed || inner.w < 28 || inner.h < 7 {
        return Vec::new();
    }
    let mut x = inner.x;
    let right = u32::from(inner.x) + u32::from(inner.w);
    let mut out = Vec::with_capacity(RecallWindow::ALL.len());
    for window in RecallWindow::ALL {
        let Ok(w) = u16::try_from(window.label().chars().count().saturating_add(2)) else {
            break;
        };
        if u32::from(x) + u32::from(w) > right {
            break;
        }
        out.push(RecallWindowButton {
            window,
            rect: Rect {
                x,
                y: inner.y + RECALL_WINDOW_DY,
                w,
                h: 1,
            },
        });
        x = x.saturating_add(w);
    }
    // All four or none: a half-drawn row of periods reads as "these are the
    // periods", and a viewer would never learn the ones that did not fit.
    if out.len() == RecallWindow::ALL.len() {
        out
    } else {
        Vec::new()
    }
}

/// How many inner rows the read/saved bar chart takes: one per bar.
const RECALL_BAR_ROWS: u16 = 2;

/// The narrowest card that can carry a labelled bar and its value without the
/// bar itself shrinking to nothing: 5 label + 1 gap + 4 bar + 1 gap + the
/// widest figure the value column draws.
const RECALL_BAR_MIN_W: u16 = 24;

/// One bar of the read-vs-saved chart: its row label and the tokens behind it.
struct RecallBar {
    label: &'static str,
    /// The token count — the measured figure, and the bar's proportion when
    /// the card is read in tokens.
    value: u64,
    /// The same figure in micro-USD, when the money authority could value it.
    micros: Option<i64>,
    /// The pre-rendered dollar label (tilde mandatory), `Some` exactly when
    /// [`Self::micros`] is.
    usd_label: Option<String>,
    color: Color,
}

/// The two figures the bar chart compares, or `None` when neither side of the
/// window holds a token.
///
/// Absence draws nothing, the rule the two headline chips already follow: a
/// window with no reads and no avoided tokens has no proportion to show, and
/// two empty rows labelled `read` and `saved` would look like a measured pair
/// of zeroes rather than a question nobody answered. The rows the chart would
/// have taken go back to the call chart.
///
/// **Both figures come from the share**, so the bars stand with the
/// `[N% of reads]` chip rather than with the `saved N% (…)` headline beside
/// it. On a narrow period the two agree by construction — one cut reaches
/// both. At `all` on a repository carrying a boundary marker they can differ,
/// which is the *existing* difference between those two denominators, spelled
/// out in the cost chapter's "every rate names its denominator": the headline
/// asks what share of the context this plane was responsible for it did not
/// send, the share asks what share of the tokens the agents would have read it
/// avoided. The bars answer the second question, and it is the second question
/// the chip beside them answers too.
fn recall_bar_pair(col3: &Col3) -> Option<[RecallBar; 2]> {
    let RecallView::Data(sd) = &col3.savings else {
        return None;
    };
    let (read, saved) = (sd.share.read_tokens, sd.share.avoided_tokens);
    if read == 0 && saved == 0 {
        return None;
    }
    Some([
        RecallBar {
            label: "read",
            value: read,
            micros: sd.usd_read_micros,
            usd_label: sd.usd_read_label.clone(),
            // The token panel's accent: read tokens are spend, and that is
            // the colour spend already wears on this dashboard.
            color: Section::TokenUsage.accent(),
        },
        RecallBar {
            label: "saved",
            value: saved,
            micros: sd.usd_saved_micros,
            usd_label: sd.usd_label.clone(),
            color: Section::Recall.accent(),
        },
    ])
}

/// Whether the dollar view can be honoured: **both** bars valued, so the pair
/// is still comparable.
///
/// One priced bar beside one unpriced one is worse than two token bars — it
/// would draw a proportion nobody computed. So the fallback is all-or-nothing,
/// and the card says why in its title rather than inventing a rate.
fn recall_usd_available(col3: &Col3) -> bool {
    recall_bar_pair(col3).is_some_and(|bars| bars.iter().all(|bar| bar.micros.is_some()))
}

/// The unit the bars are actually drawn in: the viewer's choice, unless the
/// dollar view has no honest basis.
fn recall_effective_unit(col3: &Col3) -> RecallUnit {
    if col3.recall_unit == RecallUnit::Usd && !recall_usd_available(col3) {
        RecallUnit::Tokens
    } else {
        col3.recall_unit
    }
}

/// The Recall panel's title, carrying the reason the dollar view was refused
/// when it was.
///
/// **Byte-identical to the bare title whenever the card is showing what was
/// asked for**, the activity card's off-strip precedent, so every golden that
/// does not exercise the refusal is unchanged. It goes in the title because
/// the card's rows are all spoken for and a reason that only appears when the
/// panel is tall enough is a reason the reader cannot rely on.
fn recall_title(col3: &Col3) -> &'static str {
    if col3.recall_unit == RecallUnit::Usd && !recall_usd_available(col3) {
        "recall · no seeded price"
    } else {
        "recall"
    }
}

/// Draw the read-vs-saved bar chart under the period row, returning the rows
/// it used (`0` when it drew nothing, so the call chart reclaims them).
///
/// **Both bars share one scale**, the larger of the two, because the whole
/// point of putting them on one card is that their lengths are comparable:
/// two independently-normalised bars would draw a repository that avoided 1 %
/// of its reads exactly like one that avoided 99 %.
fn draw_recall_bars(
    col3: &Col3,
    theme: Theme,
    unicode: bool,
    collapsed: bool,
    inner: Rect,
    backend: &mut dyn RenderBackend,
    frame: &mut Frame,
) -> u16 {
    if collapsed || inner.w < RECALL_BAR_MIN_W {
        return 0;
    }
    if inner.h < RECALL_CHART_DY.saturating_add(RECALL_BAR_ROWS) {
        return 0;
    }
    let Some(bars) = recall_bar_pair(col3) else {
        return 0;
    };
    let unit = recall_effective_unit(col3);
    let labels: Vec<String> = bars
        .iter()
        .map(|bar| match (unit, bar.usd_label.as_deref()) {
            (RecallUnit::Usd, Some(label)) => label.to_owned(),
            _ => fmt_km(f64_of(bar.value)),
        })
        .collect();
    let value_w = u16::try_from(labels.iter().map(|v| v.chars().count()).max().unwrap_or(1))
        .unwrap_or(1)
        .min(12);
    // Six: the widest label (`saved`) plus the space that keeps it off the
    // bar's first cell.
    let label_w = 6u16;
    let bar_w = inner
        .w
        .saturating_sub(label_w)
        .saturating_sub(value_w)
        .saturating_sub(1);
    if bar_w == 0 {
        return 0;
    }
    // In dollars the bars are proportioned on the money, not the tokens: a
    // partially-priced mix can value the same token counts differently, and a
    // bar whose length disagreed with the figure printed beside it would be
    // the one thing this chart must never do.
    let amount = |bar: &RecallBar| match (unit, bar.micros) {
        (RecallUnit::Usd, Some(micros)) => f64_of(u64::try_from(micros.max(0)).unwrap_or(0)),
        _ => f64_of(bar.value),
    };
    let scale = bars.iter().map(amount).fold(0.0f64, f64::max);
    let shader = if unicode {
        CellShaderKind::Braille
    } else {
        CellShaderKind::LuminanceRamp
    };
    for (index, (bar, value)) in bars.iter().zip(&labels).enumerate() {
        let Ok(offset) = u16::try_from(index) else {
            break;
        };
        let y = inner.y + RECALL_CHART_DY + offset;
        frame.put_str(inner.x, y, bar.label, theme.dim());
        let plot = Rect {
            x: inner.x + label_w,
            y,
            w: bar_w,
            h: 1,
        };
        let fraction = if scale > 0.0 {
            amount(bar) / scale
        } else {
            0.0
        };
        let samples = crate::charts::hbar_samples(fraction, plot, bar.color);
        backend.shade_cells(&samples, plot, shader, frame);
        let value_x = inner.x
            + inner.w.saturating_sub(
                u16::try_from(value.chars().count())
                    .unwrap_or(value_w)
                    .min(value_w),
            );
        frame.put_str(value_x, y, value, theme.tile_value(bar.color));
    }
    RECALL_BAR_ROWS
}

/// The unit toggle's exact cells: a `[tok]`/`[usd]` chip **right-aligned on
/// the period row**, solved once for the draw and the hit-test.
///
/// It shares the period row rather than taking one of its own because the two
/// choices describe the same chart and the card has no row to spare; right
/// alignment keeps it off the period buttons at every width the row is drawn
/// at, and the gap between them is asserted rather than assumed.
///
/// `None` when the period row itself is not drawn (the collapsed and narrow
/// tiers) or when the chip would touch the last period button. The `$` key is
/// bound in both cases; nothing here is the only door to the unit.
#[must_use]
pub fn recall_unit_rect(col3: &Col3, collapsed: bool, inner: Rect) -> Option<Rect> {
    let buttons = recall_window_buttons(collapsed, inner);
    let last = buttons.last()?;
    let w = u16::try_from(recall_effective_unit(col3).label().chars().count() + 2).ok()?;
    let x = inner.x.checked_add(inner.w)?.checked_sub(w)?;
    // One clear cell between the periods and the unit, so the row never reads
    // as five buttons of one kind.
    (u32::from(x) > u32::from(last.rect.x) + u32::from(last.rect.w)).then_some(Rect {
        x,
        y: inner.y + RECALL_WINDOW_DY,
        w,
        h: 1,
    })
}

/// Draw the period row from [`recall_window_buttons`]. The selected period is
/// drawn in the key-hint style the rest of the dashboard uses for a live
/// affordance; the others are dim, so which window the figures cover is
/// readable without counting brackets.
fn draw_recall_window_row(
    col3: &Col3,
    theme: Theme,
    collapsed: bool,
    inner: Rect,
    frame: &mut Frame,
) {
    for button in recall_window_buttons(collapsed, inner) {
        let selected = button.window == col3.recall_window;
        let style = if selected {
            theme.key_hint()
        } else {
            theme.dim()
        };
        frame.put_str(
            button.rect.x,
            button.rect.y,
            &format!("[{}]", button.window.label()),
            style,
        );
    }
    // The unit toggle, at the row's right edge. It is always a live
    // affordance, so it always wears the key-hint style — unlike the period
    // buttons, exactly one of which is the current selection.
    if let Some(chip) = recall_unit_rect(col3, collapsed, inner) {
        frame.put_str(
            chip.x,
            chip.y,
            &format!("[{}]", recall_effective_unit(col3).label()),
            theme.key_hint(),
        );
    }
}

fn draw_recall_placeholder(col3: &Col3, theme: Theme, inner: Rect, frame: &mut Frame) -> bool {
    if matches!(col3.savings, RecallView::Disabled) && matches!(col3.calls, RecallView::Disabled) {
        body(
            frame,
            inner,
            0,
            "recall off — open Project Settings",
            theme.dim(),
        );
        return true;
    }
    if matches!(col3.savings, RecallView::Quiet) && matches!(col3.calls, RecallView::Quiet) {
        // Which emptiness, said out loud. Over the whole record an empty
        // ledger means the plane has never served here and `doctor` is the
        // next step; over a narrow period it means this period is empty, and
        // sending an operator to a diagnostic for that would be the card
        // reporting a fault where there is none. The narrow sentence also
        // covers the window a broken clock cannot place: nothing is in a
        // window that could not be placed, which is exactly what it says.
        let sentence = if col3.recall_window == RecallWindow::AllTime {
            "enabled, no serves yet — see `rift doctor`"
        } else {
            "no serves in this period — try [all]"
        };
        body(frame, inner, 0, sentence, theme.dim());
        return true;
    }
    false
}

/// The clickable USD-savings chip on the Recall headline row:
/// `[~$105.90]`, two cells after the `saved …` text. **One solver** feeds the
/// draw and the hit-test, so the glyphs and the click target are always the
/// same cells (`hit_test` recomputes geometry; a second solver would put
/// clicks on the wrong cells).
///
/// `None` — and therefore no chip anywhere — when there is no honest figure
/// (`usd_label` absent, the priced savings estimate rung 4), in the collapsed/narrow path
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
    let (savings, _, _) = recall_headlines(col3);
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

/// The target chip on the Recall headline row: `[of 90% target]`, two cells
/// after the USD chip when there is one and after the headline text when there
/// is not.
///
/// It says what the headline beside it is aiming at — the product's stated
/// savings target, from the ledger's one constant — so a reader can size a
/// small share against the goal instead of against nothing. It is a target and
/// never a measurement, which is why it carries no tilde and no `n`.
///
/// **One solver** feeds the draw and the hit-test, exactly as the USD chip
/// does, so the glyphs and the click target are always the same cells. It
/// opens the same breakdown modal the USD chip opens — the same act through a
/// second door, so the card gains no act and no new modal surface. That
/// matters for more than tidiness: the USD chip disappears whenever no seeded
/// price applies, and without this door the per-family share rows would be
/// unreachable by mouse on exactly the repositories that have no price
/// catalogue. Both doors have a key twin (`S`, hinted while the graphs column
/// has focus), so neither direction of the parity rule rests on the other
/// chip being drawn.
///
/// `None` — and therefore no chip — when the card carries no target, in the
/// collapsed/narrow path, or when the whole chip cannot fit on the row.
#[must_use]
pub fn recall_share_rect(col3: &Col3, collapsed: bool, inner: Rect) -> Option<Rect> {
    if collapsed || inner.w < 28 || inner.h < 7 {
        return None;
    }
    let RecallView::Data(sd) = &col3.savings else {
        return None;
    };
    let label = share_chip_label(sd)?;
    let (headline, _, _) = recall_headlines(col3);
    let mut x = u32::from(inner.x) + headline.chars().count() as u32 + 2;
    if let Some(cost) = recall_cost_rect(col3, collapsed, inner) {
        x = u32::from(cost.x) + u32::from(cost.w) + 1;
    }
    let w = u32::try_from(label.chars().count()).ok()?;
    let x = u16::try_from(x).ok()?;
    let w = u16::try_from(w).ok()?;
    (u32::from(x) + u32::from(w) <= u32::from(inner.x) + u32::from(inner.w)).then_some(Rect {
        x,
        y: inner.y,
        w,
        h: 1,
    })
}

/// The target chip's glyphs, or `None` when this card carries no target.
///
/// "of N% target" rather than a bare percentage, because the row already holds
/// one percentage and two of them unlabelled is how a reader ends up quoting
/// the wrong figure.
#[must_use]
pub fn share_chip_label(sd: &SavingsData) -> Option<String> {
    let target = sd.target_pct?;
    Some(format!("[of {target}% target]"))
}

// --- the workflow panels -------------------------------------------------
//
// Both are text tables over already-finished values, which is why neither takes
// a `RenderBackend`: there is no chart, no shader tier, and nothing to degrade.
// Every string they draw came out of the data plane escaped, and every number
// is a count the window yielded — so a hostile filename can occupy a row here
// and still be inert text.

/// How many rows of a budgeted list fit in `room`, and how many of `total` are
/// therefore unshown.
///
/// `carried` is what the data plane put on the bus and `total` is what the
/// window actually holds; they differ when the feed's own row budget already
/// truncated. Both truncations are counted into one number, because a reader
/// does not care which layer dropped a row — only that some were dropped. When
/// anything is hidden, one row of the budget is spent saying so.
fn fit(carried: usize, room: usize, total: u64) -> (usize, u64) {
    let total = total.max(u64::try_from(carried).unwrap_or(u64::MAX));
    if u64::try_from(carried).unwrap_or(u64::MAX) >= total && carried <= room {
        return (carried, 0);
    }
    let shown = carried.min(room.saturating_sub(1));
    let hidden = total.saturating_sub(u64::try_from(shown).unwrap_or(u64::MAX));
    (shown, hidden)
}

/// A percentage rendered from milli-units without floating point (`912` →
/// `"91.2%"`), so the card and the report round the same way.
fn fmt_milli_pct(milli: i64) -> String {
    let m = milli.clamp(0, 100_000);
    format!("{}.{}%", m / 10, m % 10)
}

fn draw_dispatch_panel(
    col3: &Col3,
    theme: Theme,
    focused: bool,
    collapsed: bool,
    rect: Rect,
    frame: &mut Frame,
) {
    let headline = match &col3.dispatch {
        DispatchView::Data(d) => format!("dispatch  {} checkpoints", fmt_count(d.checkpoints)),
        DispatchView::NoProjection | DispatchView::NoActivity => "dispatch".to_string(),
    };
    // A one-row panel has no border and therefore no title — `chrome::panel`
    // returns the area untouched below two rows — so the collapsed line has to
    // name the panel itself, or the surviving row is an unlabelled number.
    if collapsed {
        let tail = match &col3.dispatch {
            DispatchView::Data(d) => format!(" · {} sessions", fmt_count(d.sessions)),
            DispatchView::NoProjection => " · no projection".to_string(),
            DispatchView::NoActivity => " · nothing captured".to_string(),
        };
        body(frame, rect, 0, &format!("{headline}{tail}"), theme.body());
        return;
    }
    let inner = panel_frame(theme, Section::Dispatch, focused, &headline, rect, frame);
    let d = match &col3.dispatch {
        // Two different absences, two different sentences — never one zero row.
        DispatchView::NoProjection => {
            body(frame, inner, 0, "(no capture projection here)", theme.dim());
            return;
        }
        DispatchView::NoActivity => {
            body(frame, inner, 0, "(nothing captured yet)", theme.dim());
            return;
        }
        DispatchView::Data(d) => d,
    };
    body(
        frame,
        inner,
        0,
        &format!(
            "sessions {} ({} open)",
            fmt_count(d.sessions),
            fmt_count(d.open_sessions)
        ),
        theme.body(),
    );
    let conf = d
        .confidence_milli
        .map_or_else(String::new, |c| format!("  conf {}", fmt_milli_pct(c)));
    body(
        frame,
        inner,
        1,
        &format!(
            "AI {}  MX {}  HU {}{conf}",
            d.agent_lines, d.mixed_lines, d.human_lines
        ),
        theme.body(),
    );
    // The time row is the panel's honesty about `checkpoint.created_at`: a
    // shard placed by the sessions it archives is a LOWER BOUND, and a shard
    // that cannot be placed at all is excluded from bounded windows rather than
    // absent. A window where neither happened does not draw the row at all.
    let mut dy = 2u16;
    if d.derived > 0 || d.unresolved > 0 {
        body(
            frame,
            inner,
            dy,
            &format!(
                "{} session-timed · {} undated",
                fmt_count(d.derived),
                fmt_count(d.unresolved)
            ),
            theme.dim(),
        );
        dy += 1;
    }
    match d.sensitive {
        Some(n) => body(
            frame,
            inner,
            dy,
            &format!(
                "files {}  ·  {} sensitive",
                fmt_count(d.files_total),
                fmt_count(n)
            ),
            theme.body(),
        ),
        // Unasked is not zero: the policy never answered, and the row says so
        // in the dim register rather than claiming a clean window.
        None => body(
            frame,
            inner,
            dy,
            &format!("files {}  ·  sensitive unasked", fmt_count(d.files_total)),
            theme.dim(),
        ),
    }
    draw_touched_files(d, theme, inner, dy + 1, frame);
}

/// The dispatch panel's file list, from row `dy` down.
///
/// It fills what is left in the window's own rank order, and what it cannot fit
/// it COUNTS — against `files_total`, so a list the data plane already budgeted
/// is never reported as the whole truth. A silently truncated evidence list is
/// the shape of "the rest is cosmetic"; one row is a cheap price for not doing
/// that.
fn draw_touched_files(d: &DispatchData, theme: Theme, inner: Rect, dy: u16, frame: &mut Frame) {
    let mut dy = dy;
    let room = usize::from(inner.h.saturating_sub(dy));
    let (shown, hidden) = fit(d.files.len(), room, d.files_total);
    for f in d.files.iter().take(shown) {
        let style = if f.sensitive {
            theme.dim()
        } else {
            theme.body()
        };
        let mark = if f.sensitive { "!" } else { " " };
        body(
            frame,
            inner,
            dy,
            &format!("{mark}{:>3}  {}", f.checkpoints, f.label),
            style,
        );
        dy += 1;
    }
    if hidden > 0 {
        body(
            frame,
            inner,
            dy,
            &format!("     +{hidden} more"),
            theme.dim(),
        );
    }
}

fn draw_recap_panel(
    col3: &Col3,
    theme: Theme,
    focused: bool,
    collapsed: bool,
    rect: Rect,
    frame: &mut Frame,
) {
    let headline = match &col3.recap {
        RecapView::Data(d) => format!("recap  {} sessions", fmt_count(d.sessions)),
        RecapView::NoProjection | RecapView::NoActivity => "recap".to_string(),
    };
    // As above: the collapsed row is the only row, so it names its own panel.
    if collapsed {
        let tail = match &col3.recap {
            RecapView::Data(d) => format!(" · {} tokens", fmt_km(f64_of(d.tokens))),
            RecapView::NoProjection => " · no projection".to_string(),
            RecapView::NoActivity => " · no sessions".to_string(),
        };
        body(frame, rect, 0, &format!("{headline}{tail}"), theme.body());
        return;
    }
    let inner = panel_frame(theme, Section::Recap, focused, &headline, rect, frame);
    let d = match &col3.recap {
        RecapView::NoProjection => {
            body(frame, inner, 0, "(no capture projection here)", theme.dim());
            return;
        }
        RecapView::NoActivity => {
            body(frame, inner, 0, "(no agent sessions yet)", theme.dim());
            return;
        }
        RecapView::Data(d) => d,
    };
    let cost = d
        .cost_label
        .as_deref()
        .map_or_else(String::new, |c| format!(" · {c}"));
    body(
        frame,
        inner,
        0,
        &format!("tokens {}{cost}", fmt_km(f64_of(d.tokens))),
        theme.body(),
    );
    body(
        frame,
        inner,
        1,
        if d.by_model { "by model:" } else { "by agent:" },
        theme.dim(),
    );
    // Recall's tail is reserved before the split is drawn, so a repository with
    // many agents cannot push the ledger off the panel — the split is the part
    // that degrades, and it says how much it dropped.
    let tail = if d.savings.is_some() { 2u16 } else { 1 };
    let split_room = usize::from(inner.h.saturating_sub(2 + tail));
    let (shown, hidden) = fit(d.split.len(), split_room, d.split_total);
    let mut dy = 2u16;
    for (key, n) in d.split.iter().take(shown) {
        body(
            frame,
            inner,
            dy,
            &format!("{:>4}  {key}", fmt_count(*n)),
            theme.body(),
        );
        dy += 1;
    }
    if hidden > 0 {
        body(
            frame,
            inner,
            dy,
            &format!("      +{hidden} more"),
            theme.dim(),
        );
        dy += 1;
    }
    // The ledger is last and is never a zero: absent means this repository has
    // no `recall.db`, which is a different fact from a ledger that avoided
    // nothing. The caveat rides in the text and not in a comment — the avoided
    // figure is an ESTIMATED UPPER BOUND, by the owner's standing rule, and no
    // claim rests on it, so the surface that shows it says so.
    match d.savings {
        Some(s) => {
            body(
                frame,
                inner,
                dy,
                &format!(
                    "avoided {} (est. upper bound)",
                    fmt_km(f64_of(s.avoided_upper_bound))
                ),
                theme.dim(),
            );
            body(
                frame,
                inner,
                dy + 1,
                &format!("injected {}", fmt_count(s.injected)),
                theme.dim(),
            );
        }
        None => body(frame, inner, dy, "no recall ledger here", theme.dim()),
    }
}

/// A count as the `f64` [`fmt_km`] takes. Counts here are token sums, which
/// stay well inside `f64`'s exact-integer range; a saturating cast is still
/// used so an absurd value degrades to a large number rather than to a wrong
/// one.
#[allow(clippy::cast_precision_loss)]
fn f64_of(n: u64) -> f64 {
    n as f64
}

/// Thousands-grouped token count (`7862400` → `"7,862,400"`) for the
/// breakdown modal: its rows must *visibly* sum to the headline, so no lossy
/// k/M humanizing there. Tokens are counts, not money — money strings only
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

/// worktree provenance's word for a fact whose worktree was never observed. Every
/// surface prints this, never `main` and never a blank.
pub const UNKNOWN_WORKTREE: &str = "unknown";

/// The label for a fact that carries no session key at all, and so belongs to
/// the family and to no member.
const FAMILY_ONLY: &str = "family-only";

/// The axis's rows as aligned `name  count tok` lines, `unknown` and
/// `family-only` spelled as themselves. Shared by both surfaces that carry
/// the axis, so one table reads the same way in each.
fn worktree_rows(axis: &WorktreeAxis) -> Vec<String> {
    let mut names: Vec<String> = axis
        .rows
        .iter()
        .map(|s| {
            s.name
                .clone()
                .unwrap_or_else(|| UNKNOWN_WORKTREE.to_owned())
        })
        .collect();
    let mut counts: Vec<String> = axis.rows.iter().map(|s| fmt_count(s.tokens)).collect();
    if axis.family_only > 0 {
        names.push(FAMILY_ONLY.to_owned());
        counts.push(fmt_count(axis.family_only));
    }
    let name_w = names.iter().map(|n| n.chars().count()).max().unwrap_or(0);
    let tok_w = counts.iter().map(|c| c.chars().count()).max().unwrap_or(0);
    names
        .iter()
        .zip(&counts)
        .map(|(name, count)| format!("{name:<name_w$}  {count:>tok_w$} tok"))
        .collect()
}

/// The Tokens detail's "by worktree" body — the family
/// total, one row per member, and the rule that put each fact in its row.
///
/// Pure over the token view-model: the rows were computed by the data plane
/// in the same pass as the total they sum to, so this function assembles
/// lines and never divides anything. Honest absence has its own words — a
/// total with no decomposition is said, never drawn as an empty table.
#[must_use]
pub fn worktree_axis_lines(col3: &Col3) -> Vec<String> {
    let TokensView::Data(d) = &col3.tokens else {
        return vec![
            "no token facts in scope yet — there is no total to".to_owned(),
            "decompose.".to_owned(),
        ];
    };
    let axis = &d.by_worktree;
    if axis.is_empty() {
        return vec![
            "no worktree facts in scope: this projection records".to_owned(),
            "no session for the token history it holds, so the".to_owned(),
            "total cannot be split.".to_owned(),
        ];
    }
    let mut lines = vec![
        format!("family total  {} tok", fmt_count(axis.total())),
        String::new(),
    ];
    lines.extend(worktree_rows(axis));
    lines.push(String::new());
    lines.push("each session counts under the LAST worktree it was".to_owned());
    lines.push("observed in; `unknown` is a session with no".to_owned());
    lines.push("observation at all — history captured before".to_owned());
    lines.push("worktree provenance, and sealed history is".to_owned());
    lines.push("never relabelled.".to_owned());
    lines
}

/// The savings modal's "by worktree" block (the family view).
///
/// Every avoided token lands under `family-only` today, and the modal says
/// exactly why: no serve relation records the session that was served
/// (`memd_audit_event`, `serve_call_event` and `serve_session_event` hold no
/// session key), so no serve can be attributed to a member without inventing
/// the attribution. The wording follows the data: if a future relation does
/// carry the key, member rows appear and the family-only sentence goes with
/// the empty remainder.
fn savings_worktree_lines(axis: &WorktreeAxis) -> Vec<String> {
    if axis.is_empty() {
        return Vec::new();
    }
    let mut lines = vec![String::new(), "by worktree".to_owned()];
    lines.extend(worktree_rows(axis));
    if axis.rows.is_empty() {
        lines.push("no serve relation records the session that was".to_owned());
        lines.push("served, so no serve can be attributed to one".to_owned());
        lines.push("member — this figure is the family's.".to_owned());
    } else {
        lines.push("`unknown` is a session with no worktree".to_owned());
        lines.push("observation; `family-only` is a serve with no".to_owned());
        lines.push("session key at all (the family view).".to_owned());
    }
    lines
}

/// The breakdown modal's body (the priced savings estimate) — the full disclosed basis:
/// net avoided tokens, the class assumption, one row per model (attributed
/// slice × catalog rate = subtotal), the composing total, the unpriced
/// coverage when any, and the estimate-of-an-estimate statement. Pure over
/// the savings view-model; every money string was pre-rendered by the one
/// money authority, so this function formats counts and assembles lines —
/// it never computes or formats money.
#[must_use]
pub fn usd_breakdown_lines(sd: &SavingsData) -> Vec<String> {
    let Some(total_label) = sd.usd_label.as_deref() else {
        // Reachable, and not rarely: the share chip opens this modal on every
        // repository whose models carry no seeded price. The dollar half says
        // it has nothing to say, and the share half below still answers.
        let mut lines = vec![
            "no dollar figure: no seeded price applies to this".to_owned(),
            "project's models — the token headline stands alone".to_owned(),
        ];
        lines.extend(savings_share_lines(&sd.share, sd.target_pct));
        return lines;
    };
    // The rows' own slices, which sum to exactly the avoided total this card's
    // window holds — the same figure the headline and the saved bar draw,
    // because the money is priced from the share rather than from a second
    // read of the audit relation over a second window.
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
    lines.extend(savings_worktree_lines(&sd.by_worktree));
    lines.push(String::new());
    lines.push("~ marks an estimate, not a bill: token counts are".to_owned());
    // (the target line is appended with the share block below, where the
    // figure it is a target *for* is printed)
    lines.push("cl100k_base (not the provider's billing tokenizer),".to_owned());
    lines.push("and the per-model split is attributed from observed".to_owned());
    lines.push("usage — the serve trail records no per-serve model".to_owned());
    lines.push("(the priced savings estimate).".to_owned());
    lines.extend(savings_share_lines(&sd.share, sd.target_pct));
    lines
}

/// The savings-share block of the breakdown modal: the headline share, the
/// honesty figure and one row per family (share, honesty, n).
///
/// Every number was computed by `rift_recall_ledger::savings` and arrived on
/// the bus already decided; this function lays out strings. An absent share is
/// drawn as `n/a`, never as `0 %` — the two mean opposite things, and the
/// second is a measurement nobody made.
#[must_use]
pub fn savings_share_lines(share: &ShareData, target_pct: Option<u8>) -> Vec<String> {
    // The window the numbers cover, in the card's own voice: the default is the
    // whole record, and a boundary marker replaces that clause rather than
    // being appended to it — two window sentences on one card is how a reader
    // ends up believing the wrong one.
    let window = share
        .window
        .clone()
        .unwrap_or_else(|| "since the projection was reset".to_owned());
    let Some(overall) = share.overall else {
        return vec![
            String::new(),
            "savings share: unanswerable — nothing has been read".to_owned(),
            format!("and nothing avoided {window}"),
        ];
    };
    let mut lines = vec![
        String::new(),
        "savings share — of the tokens this repository's agents".to_owned(),
        "would have read, the share the recall plane avoided,".to_owned(),
        window,
        match target_pct {
            Some(target) => format!("overall              {overall:.1}% of a {target}% target"),
            None => format!("overall              {overall:.1}%"),
        },
        format!("honesty              {:+} tok avoided net", share.honesty),
        format!(
            "sessions with reads  {} (the n behind the share)",
            share.sessions_with_reads
        ),
    ];
    // The hole in the denominator, named. A read whose token count was never
    // frozen is a row in the denominator's set worth zero tokens, so the share
    // is over fewer measured reads than its `n` suggests. It is counted here
    // rather than imputed into the figure above it.
    if share.reads_unmeasured > 0 {
        lines.push(format!(
            "reads not measured   {} (in the denominator, worth 0)",
            share.reads_unmeasured
        ));
    }
    lines.push(String::new());
    let name_w = share
        .families
        .iter()
        .map(|f| f.family.chars().count())
        .max()
        .unwrap_or(0)
        .max(6);
    lines.push(format!(
        "{family:<name_w$}  {share:>7}  {honesty:>9}  {n:>3}",
        family = "family",
        share = "share",
        honesty = "honesty",
        n = "n"
    ));
    if share.families.is_empty() {
        lines.push("(no family has a decision or a read yet)".to_owned());
    }
    for f in &share.families {
        let pct = f
            .share
            .map_or_else(|| "n/a".to_owned(), |p| format!("{p:.1}%"));
        let flag = if f.decided_without_reads {
            "  decided, no captured reads"
        } else {
            ""
        };
        lines.push(format!(
            "{family:<name_w$}  {pct:>7}  {honesty:>+9}  {n:>3}{flag}",
            family = f.family,
            honesty = f.honesty,
            n = f.sessions_with_reads
        ));
    }
    if let Some(excluding) = share.excluding {
        lines.push(String::new());
        lines.push("excluding families that decided with no captured".to_owned());
        lines.push(format!(
            "reads: {excluding:.1}% (excluded: {})",
            if share.excluded.is_empty() {
                "none".to_owned()
            } else {
                share.excluded.join(", ")
            }
        ));
    }
    lines.push(String::new());
    lines.push("a row is a name one relation used: the serving side's".to_owned());
    lines.push("decisions and the capture projection's reads are joined".to_owned());
    lines.push("by that name alone, so a family with decisions and no".to_owned());
    lines.push("captured read has no read side of its own.".to_owned());
    lines
}

/// The card's headline, its serve count and its active-session phrase.
///
/// The headline is **the shared savings share** and the token count behind it,
/// both out of the one `savings::compose` call the command line reads from. It
/// used to be a second percentage — avoided over avoided plus returned — which
/// is a different question with the same shape, drawn two cells from a chip
/// that answered the other one. Its denominator is named where there is room
/// to name it: the two bars under it are literally `read` and `saved`, and the
/// modal spells the fraction out.
///
/// One decimal, because that is how `rift savings` prints it: a card that
/// rounded to whole percent would disagree with the terminal beside it on a
/// repository sitting at 2.5 %. An unanswerable share is `saved —`, never a
/// zero — nothing read and nothing avoided is a question with no answer.
///
/// It is kept **short** on purpose. Everything else on this row — the dollar
/// estimate, the target — is positioned after it and shed when it will not
/// fit, so a longer headline is paid for by whatever stands to its right at
/// the narrow widths this column is usually drawn at.
fn recall_headlines(col3: &Col3) -> (String, u64, String) {
    let savings = match &col3.savings {
        RecallView::Data(sd) => match sd.share.overall {
            Some(pct) => format!(
                "saved {pct:.1}% ({})",
                fmt_km(f64_of(sd.share.avoided_tokens))
            ),
            None => "saved —".to_string(),
        },
        RecallView::Quiet | RecallView::Disabled => "saved —".to_string(),
    };
    let call_total = match &col3.calls {
        RecallView::Data(calls) => calls.total,
        RecallView::Quiet | RecallView::Disabled => 0,
    };
    let sessions = match &col3.active_sessions {
        ActiveSessionsView::Count(count) => active_sessions_label(*count),
        ActiveSessionsView::NoSessions => active_sessions_label(0),
    };
    (savings, call_total, sessions)
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

fn draw_call_chart(
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
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
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

    #[derive(Default)]
    struct ShaderProbe {
        shader: Option<CellShaderKind>,
    }

    impl RenderBackend for ShaderProbe {
        fn name(&self) -> &'static str {
            "shader-probe"
        }

        fn capabilities(&self) -> Capabilities {
            Capabilities::UNICODE | Capabilities::SUBCELL
        }

        fn present(&mut self, _frame: &Frame) -> Result<(), crate::render::RenderError> {
            Ok(())
        }

        fn shade_cells(
            &mut self,
            _samples: &[crate::render::ShadeSample],
            _area: Rect,
            shader: CellShaderKind,
            _frame: &mut Frame,
        ) {
            self.shader = Some(shader);
        }
    }

    /// Render the panels into a `w`×`h` area (no outer border).
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

    /// The windows are trailing cuts, ordered, and the default is the one
    /// that changes nothing.
    ///
    /// The default matters most: `AllTime` yields **no cut at all**, so the
    /// family's boundary marker still decides the period and a dashboard
    /// nobody has clicked draws exactly what it drew before these buttons
    /// existed.
    #[test]
    fn the_recall_windows_are_ordered_trailing_cuts_over_an_uncut_default() {
        let now = 1_784_505_600_000i64;
        assert_eq!(RecallWindow::default(), RecallWindow::AllTime);
        assert_eq!(RecallWindow::AllTime.cut(now), WindowCut::Marker);

        let cuts: Vec<i64> = [
            RecallWindow::Today,
            RecallWindow::Last7Days,
            RecallWindow::Last30Days,
        ]
        .into_iter()
        .map(|w| match w.cut(now) {
            WindowCut::Since(ms) => ms,
            other => panic!("a narrow window names its own cut: {other:?}"),
        })
        .collect();
        assert_eq!(
            cuts,
            vec![now - DAY_MS, now - 7 * DAY_MS, now - 30 * DAY_MS]
        );
        assert!(
            cuts.windows(2).all(|pair| pair[0] > pair[1]),
            "a wider window must start earlier: {cuts:?}"
        );

        // Button order is narrowest-first and every label is distinct — four
        // chips on one row that shared a glyph would be four doors a viewer
        // cannot tell apart.
        let labels: Vec<&str> = RecallWindow::ALL.iter().map(|w| w.label()).collect();
        assert_eq!(labels, vec!["24h", "7d", "30d", "all"]);
        let mut sorted = labels.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), labels.len());
    }

    /// A clock a trailing window cannot be placed against is **not**
    /// "everything".
    ///
    /// The bus answers `0` when the platform refuses the wall clock, and the
    /// old arithmetic clamped `0 − 24h` to the epoch — the whole record, drawn
    /// under a button that says `24h`, with every figure on the card claiming
    /// to be a day old. The window now says it cannot be placed, and the card
    /// draws the empty window and names the reason.
    #[test]
    fn a_window_the_clock_cannot_place_is_absence_and_never_the_whole_record() {
        for window in [
            RecallWindow::Today,
            RecallWindow::Last7Days,
            RecallWindow::Last30Days,
        ] {
            assert_eq!(window.cut(0), WindowCut::NoClock, "{window:?}");
            assert_eq!(window.cut(1_000), WindowCut::NoClock, "{window:?}");
            assert_eq!(window.cut(i64::MIN), WindowCut::NoClock, "{window:?}");
            // A real clock still places it.
            assert!(matches!(
                window.cut(1_784_505_600_000),
                WindowCut::Since(ms) if ms > 0
            ));
        }
        // `all` never needs a clock: the marker decides it.
        assert_eq!(RecallWindow::AllTime.cut(0), WindowCut::Marker);
    }

    #[test]
    fn a_short_column_scrolls_the_expanded_panels() {
        // The scroll offset moves content: below the expanded content
        // height, a different offset windows a different slice; a tall column
        // fits, so the offset is inert (the old code's no-op, now the fit path).
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 5.0],
                total: 22.0,
                by_worktree: WorktreeAxis::default(),
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
    fn order_is_the_panels_by_registration() {
        let ids: Vec<SectionId> = SECTIONS.iter().map(|s| s.id).collect();
        assert_eq!(
            ids,
            vec![
                SectionId::TokenUsage,
                SectionId::System,
                SectionId::Recall,
                SectionId::Dispatch,
                SectionId::Recap,
            ]
        );
        // Registration is the wider list; `ALL` is what the default layout
        // seats. They are allowed to differ — but every seated panel must be a
        // registered one, or the stack solver sizes it from a fallback.
        for id in SectionId::ALL {
            assert!(
                SECTIONS.iter().any(|s| s.id == id),
                "{id:?} is unregistered"
            );
        }
    }

    /// **Under pressure the two workflow panels fold before anything that was
    /// here first**, so no panel that predates them is pushed below its
    /// registered minimum.
    ///
    /// That is the whole of what this test pins, and it is deliberately less
    /// than "seating costs the chart panels nothing". It looks at height 29,
    /// where the minimums fit exactly and there is no surplus to divide; the
    /// cost of seating lands in the surplus at every taller height, and it is
    /// real (see [`SectionId::ALL`] for the measured figures). A test that
    /// examines the one height with nothing to lose cannot speak for the
    /// heights that have something.
    #[test]
    fn the_workflow_panels_fold_before_the_chart_panels() {
        let ids: Vec<SectionId> = SECTIONS.iter().map(|s| s.id).collect();
        // 9 + 8 + 13 + 1 + 1 = 32: every High/Medium panel expanded, both Low
        // panels at their one-line headline.
        let slots = solve_stack(&ids, 32);
        assert_eq!(slots.len(), 5, "every panel keeps a slot");
        let by = |id: SectionId| *slots.iter().find(|s| s.id == id).expect("slot");
        assert!(!by(SectionId::TokenUsage).collapsed);
        assert!(!by(SectionId::System).collapsed);
        assert!(!by(SectionId::Recall).collapsed);
        assert!(by(SectionId::Dispatch).collapsed);
        assert!(by(SectionId::Recap).collapsed);
        assert_eq!(by(SectionId::TokenUsage).rows, 9, "no row was taken");
        assert_eq!(by(SectionId::System).rows, 8);
        assert_eq!(by(SectionId::Recall).rows, 13);
    }

    // --- the workflow panels ------------------------------------------------

    /// Draw ONE panel into a `w`×`h` frame and read it back as text. The
    /// workflow panels take no backend and no shader tier, so this is the whole
    /// render path for them.
    fn panel_text(col3: &Col3, id: SectionId, w: u16, h: u16, collapsed: bool) -> String {
        let mut f = Frame::new(Size::new(w, h));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_one(col3, id, false, false, collapsed, f.rect(), &mut b, &mut f);
        text_of(&f)
    }

    fn seeded_dispatch() -> DispatchData {
        DispatchData {
            checkpoints: 3,
            derived: 1,
            unresolved: 1,
            sessions: 3,
            open_sessions: 2,
            agent_lines: 35,
            mixed_lines: 3,
            human_lines: 5,
            confidence_milli: Some(700),
            files_total: 2,
            files: vec![
                FileTouch {
                    label: "\"src/a.rs\"".to_owned(),
                    checkpoints: 2,
                    sensitive: false,
                },
                FileTouch {
                    label: "\".env\"".to_owned(),
                    checkpoints: 1,
                    sensitive: true,
                },
            ],
            sensitive: Some(1),
        }
    }

    /// **An unasked sensitivity answer renders as unasked**: `None` is "the policy was never
    /// asked", drawn as "sensitive unasked" in the dim register — never as
    /// "0 sensitive", which would claim the window was checked and clean.
    #[test]
    fn an_unasked_sensitivity_answer_renders_as_unasked() {
        let col3 = Col3 {
            dispatch: DispatchView::Data(DispatchData {
                sensitive: None,
                ..seeded_dispatch()
            }),
            ..Col3::default()
        };
        let t = panel_text(&col3, SectionId::Dispatch, 34, 12, false);
        assert!(t.contains("files 2  ·  sensitive unasked"), "{t}");
        assert!(!t.contains("0 sensitive"), "{t}");
        // The asked-and-clean answer is a different sentence.
        let col3 = Col3 {
            dispatch: DispatchView::Data(DispatchData {
                sensitive: Some(0),
                ..seeded_dispatch()
            }),
            ..Col3::default()
        };
        let t = panel_text(&col3, SectionId::Dispatch, 34, 12, false);
        assert!(t.contains("files 2  ·  0 sensitive"), "{t}");
    }

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
    fn the_dispatch_panel_renders_the_window_it_was_given() {
        let col3 = Col3 {
            dispatch: DispatchView::Data(seeded_dispatch()),
            ..Col3::default()
        };
        let t = panel_text(&col3, SectionId::Dispatch, 34, 12, false);
        assert!(t.contains("dispatch  3 checkpoints"), "{t}");
        assert!(t.contains("sessions 3 (2 open)"), "{t}");
        assert!(t.contains("AI 35  MX 3  HU 5  conf 70.0%"), "{t}");
        // The two honesty rows: a derived time is a lower bound, and a shard
        // with no resolvable time is excluded rather than absent.
        assert!(t.contains("1 session-timed · 1 undated"), "{t}");
        assert!(t.contains("files 2  ·  1 sensitive"), "{t}");
        assert!(t.contains("src/a.rs"), "{t}");
        assert!(t.contains(".env"), "{t}");
    }

    /// **The panel never prints a citation ordinal.** `rift dispatch --json`
    /// replaces payload-derived strings with `cite:<n>`; a card reads the data
    /// plane directly and shows the value. If it also printed an ordinal, the
    /// dashboard and the document would be two authorities on which value
    /// `cite:3` names.
    #[test]
    fn no_workflow_panel_prints_a_citation_ordinal() {
        let col3 = Col3 {
            dispatch: DispatchView::Data(seeded_dispatch()),
            recap: RecapView::Data(seeded_recap()),
            ..Col3::default()
        };
        for id in [SectionId::Dispatch, SectionId::Recap] {
            for collapsed in [false, true] {
                let t = panel_text(&col3, id, 40, 14, collapsed);
                assert!(!t.contains("cite:"), "{id:?} printed an ordinal: {t}");
                assert!(!t.contains("evidence:"), "{id:?} printed one: {t}");
            }
        }
    }

    /// A confidence nobody measured is absent, never `0.0%` — the zero-vs-absent
    /// rule this program has paid for twice.
    #[test]
    fn an_unmeasured_confidence_is_absent_from_the_dispatch_panel() {
        let col3 = Col3 {
            dispatch: DispatchView::Data(DispatchData {
                confidence_milli: None,
                ..seeded_dispatch()
            }),
            ..Col3::default()
        };
        let t = panel_text(&col3, SectionId::Dispatch, 34, 12, false);
        assert!(t.contains("AI 35  MX 3  HU 5"), "{t}");
        assert!(
            !t.contains("conf"),
            "an absent measurement grew a number: {t}"
        );
    }

    #[test]
    fn a_fully_dated_window_says_nothing_about_derivation() {
        let col3 = Col3 {
            dispatch: DispatchView::Data(DispatchData {
                derived: 0,
                unresolved: 0,
                ..seeded_dispatch()
            }),
            ..Col3::default()
        };
        let t = panel_text(&col3, SectionId::Dispatch, 34, 12, false);
        assert!(!t.contains("undated"), "{t}");
        assert!(
            t.contains("files 2"),
            "and the row below still renders: {t}"
        );
    }

    /// The two absences are different sentences, and neither is a zero row.
    #[test]
    fn the_workflow_panels_tell_the_two_absences_apart() {
        let none = Col3::default();
        let t = panel_text(&none, SectionId::Dispatch, 34, 8, false);
        assert!(t.contains("no capture projection here"), "{t}");
        let t = panel_text(&none, SectionId::Recap, 34, 8, false);
        assert!(t.contains("no capture projection here"), "{t}");

        let quiet = Col3 {
            dispatch: DispatchView::NoActivity,
            recap: RecapView::NoActivity,
            ..Col3::default()
        };
        let t = panel_text(&quiet, SectionId::Dispatch, 34, 8, false);
        assert!(t.contains("nothing captured yet"), "{t}");
        assert!(
            !t.contains(" 0 "),
            "an empty state must not draw zeros: {t}"
        );
        let t = panel_text(&quiet, SectionId::Recap, 34, 8, false);
        assert!(t.contains("no agent sessions yet"), "{t}");
        assert!(
            !t.contains(" 0 "),
            "an empty state must not draw zeros: {t}"
        );
    }

    fn seeded_recap() -> RecapData {
        RecapData {
            sessions: 3,
            checkpoints: 2,
            tokens: 414,
            cost_label: Some("~<$0.01".to_owned()),
            by_model: false,
            split_total: 2,
            split: vec![
                ("\"claude-code\"".to_owned(), 2),
                ("\"codex\"".to_owned(), 1),
            ],
            savings: Some(RecapSavings {
                avoided_upper_bound: 1_000,
                injected: 40,
            }),
        }
    }

    #[test]
    fn the_recap_panel_renders_its_axis_and_its_ledger() {
        let col3 = Col3 {
            recap: RecapView::Data(seeded_recap()),
            ..Col3::default()
        };
        let t = panel_text(&col3, SectionId::Recap, 40, 12, false);
        assert!(t.contains("recap  3 sessions"), "{t}");
        assert!(t.contains("tokens 414 · ~<$0.01"), "{t}");
        assert!(t.contains("by agent:"), "{t}");
        assert!(t.contains("claude-code"), "{t}");
        assert!(t.contains("avoided 1.0k (est. upper bound)"), "{t}");
        assert!(t.contains("injected 40"), "{t}");

        let by_model = Col3 {
            recap: RecapView::Data(RecapData {
                by_model: true,
                split_total: 1,
                split: vec![("\"opus\"".to_owned(), 2)],
                ..seeded_recap()
            }),
            ..Col3::default()
        };
        let t = panel_text(&by_model, SectionId::Recap, 40, 12, false);
        assert!(t.contains("by model:"), "the axis it was given: {t}");
    }

    /// A repository with no recall ledger says so. It is a different fact from
    /// a ledger that avoided nothing, and the card must not spell them alike.
    #[test]
    fn an_absent_ledger_is_named_rather_than_shown_as_zero() {
        let col3 = Col3 {
            recap: RecapView::Data(RecapData {
                savings: None,
                ..seeded_recap()
            }),
            ..Col3::default()
        };
        let t = panel_text(&col3, SectionId::Recap, 40, 12, false);
        assert!(t.contains("no recall ledger here"), "{t}");
        assert!(!t.contains("avoided"), "{t}");
    }

    /// No honest cost figure means no figure at all — never a fabricated
    /// `~$0.00` (the priced savings estimate rung 4, as the recall panel already applies it).
    #[test]
    fn an_unpriceable_window_shows_no_money_at_all() {
        let col3 = Col3 {
            recap: RecapView::Data(RecapData {
                cost_label: None,
                ..seeded_recap()
            }),
            ..Col3::default()
        };
        let t = panel_text(&col3, SectionId::Recap, 40, 12, false);
        assert!(t.contains("tokens 414"), "{t}");
        assert!(!t.contains('$'), "a money figure was invented: {t}");
    }

    /// Collapsed keeps the headline and one line of fact — information is lost,
    /// the panel is not.
    #[test]
    fn a_collapsed_workflow_panel_keeps_its_headline() {
        let col3 = Col3 {
            dispatch: DispatchView::Data(seeded_dispatch()),
            recap: RecapView::Data(seeded_recap()),
            ..Col3::default()
        };
        // ONE row — what the solver grants a collapsed panel, and below the two
        // rows `chrome::panel` needs to draw a title at all.
        let t = panel_text(&col3, SectionId::Dispatch, 44, 1, true);
        assert!(t.contains("dispatch  3 checkpoints · 3 sessions"), "{t}");
        assert!(
            !t.contains("AI 35"),
            "a collapsed panel sheds its table: {t}"
        );
        let t = panel_text(&col3, SectionId::Recap, 44, 1, true);
        assert!(t.contains("recap  3 sessions · 414 tokens"), "{t}");
        assert!(!t.contains("by agent"), "{t}");
        // …and a collapsed absence names itself too, rather than surviving as
        // an unlabelled row.
        let none = Col3::default();
        let t = panel_text(&none, SectionId::Dispatch, 44, 1, true);
        assert!(t.contains("dispatch · no projection"), "{t}");
        let t = panel_text(&none, SectionId::Recap, 44, 1, true);
        assert!(t.contains("recap · no projection"), "{t}");
    }

    /// A hostile label is inert text in the cell grid: the escape happened in
    /// the data plane, and the panel neither un-escapes nor re-escapes it.
    #[test]
    fn a_hostile_label_reaches_the_cells_as_inert_text() {
        let col3 = Col3 {
            dispatch: DispatchView::Data(DispatchData {
                files_total: 1,
                files: vec![FileTouch {
                    label: crate::escape_for_terminal("s\u{1b}[31m/\u{202e}a.rs"),
                    checkpoints: 1,
                    sensitive: false,
                }],
                ..seeded_dispatch()
            }),
            ..Col3::default()
        };
        let mut f = Frame::new(Size::new(60, 14));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_one(
            &col3,
            SectionId::Dispatch,
            false,
            false,
            false,
            f.rect(),
            &mut b,
            &mut f,
        );
        for y in 0..f.height() {
            for x in 0..f.width() {
                let ch = f.get(x, y).map_or(' ', |c| c.ch);
                assert!(!ch.is_control(), "a control char reached a cell at {x},{y}");
                assert_ne!(ch, '\u{202e}', "an RTL override reached a cell");
            }
        }
    }

    #[test]
    fn tall_column_expands_every_panel() {
        // 9 + 8 + 13 + 7 + 8 = 45, the seated stack's expanded minimum.
        let slots = solve_stack(&SectionId::ALL, 45);
        assert_eq!(slots.len(), 5);
        assert!(slots.iter().all(|s| !s.collapsed));
        let mut y = 0;
        for s in &slots {
            assert_eq!(s.y, y);
            y += s.rows;
        }
        assert!(y <= 45);
    }

    #[test]
    fn short_column_collapses_recall_first_then_none_disappears() {
        // Recall is Medium and folds before the two High panels — but AFTER the
        // two Low ones, so the height that isolates its fold is the High-only
        // tier: 9 + 8 + 1 + 1 + 1 = 20 fits, 29 (Medium still expanded) does
        // not. 22 sits inside that band and is what makes this test about
        // Recall rather than about everything folding at once.
        let slots = solve_stack(&SectionId::ALL, 22);
        assert_eq!(slots.len(), 5, "every panel keeps a slot");
        let by = |id: SectionId| *slots.iter().find(|s| s.id == id).unwrap();
        assert!(
            by(SectionId::Recall).collapsed,
            "recall folds before the High panels"
        );
        assert!(!by(SectionId::TokenUsage).collapsed);
        assert!(!by(SectionId::System).collapsed);
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
    fn token_panel_draws_proportional_treemap_and_compact_footer() {
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 5.0],
                total: 22.0,
                by_worktree: WorktreeAxis::default(),
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 1234.0,
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
        assert!(
            text.contains("input 1.2k"),
            "input footer is compact: \n{text}"
        );
        assert!(
            text.contains("read 800"),
            "read footer is compact: \n{text}"
        );
        assert!(
            text.contains("output 300"),
            "output footer is compact: \n{text}"
        );
        assert!(
            text.contains("cache-write 50"),
            "cache footer is compact: \n{text}"
        );
        assert!(
            text.contains("read 80%"),
            "read fraction is exact: \n{text}"
        );
        // The ASCII floor remains a coloured density ramp when Braille is not
        // supported by the backend.
        let mut coloured = 0;
        for y in 0..f.height() {
            for x in 0..f.width() {
                if let Some(c) = f.get(x, y) {
                    if ".:-=+*#%@".contains(c.ch) && matches!(c.style.fg, Color::Rgb(_, _, _)) {
                        coloured += 1;
                    }
                }
            }
        }
        assert!(coloured > 0, "the treemap drew no coloured ramp cells");
    }

    #[test]
    fn token_panel_uses_braille_symbols_and_full_footer_names_at_live_panel_width() {
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0],
                total: 37_500_000.0,
                by_worktree: WorktreeAxis::default(),
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 114.0,
                read: 33_664_392.0,
                output: 58_306.0,
                cache_write: 3_767_170.0,
                read_fraction: 1.0,
            }),
            ..Col3::default()
        };
        let mut frame = Frame::new(Size::new(46, 9));
        let mut backend = ShaderProbe::default();
        draw_token_panel(
            &col3,
            Theme::rift_dark(),
            false,
            true,
            false,
            frame.rect(),
            &mut backend,
            &mut frame,
        );
        let text = text_of(&frame);
        assert_eq!(backend.shader, Some(CellShaderKind::Braille));
        assert!(text.contains("input 114"), "input class is named:\n{text}");
        assert!(text.contains("read 33.7M"), "read class is named:\n{text}");
        assert!(
            text.contains("output 58k"),
            "output class is named:\n{text}"
        );
        assert!(
            text.contains("cache-write 3.8M"),
            "cache-write class is named:\n{text}"
        );
        assert!(text.contains("read 100%"), "read fraction remains:\n{text}");
        let chart_text: String = (1..6)
            .flat_map(|y| (0..frame.width()).map(move |x| (x, y)))
            .map(|(x, y)| frame.get(x, y).map_or(' ', |cell| cell.ch))
            .collect();
        assert!(chart_text.contains('R') && chart_text.contains('C'));
        assert!(
            !chart_text.contains("read") && !chart_text.contains("cache"),
            "the plot keeps only class symbols; full names belong in the footer: {chart_text}"
        );
    }

    #[test]
    fn read_fraction_zero_over_zero_renders_as_zero_not_error() {
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0],
                total: 5.0,
                by_worktree: WorktreeAxis::default(),
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
        let frame = draw_to(&col3, 44, 24);
        let text = text_of(&frame);
        assert!(
            text.contains("read 0%"),
            "the footer shows a defined 0, not a dash"
        );
    }

    #[test]
    fn zero_total_keeps_empty_boundary_and_all_compact_footer_values() {
        let col3 = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![0.0],
                total: 0.0,
                by_worktree: WorktreeAxis::default(),
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 0.0,
                read: 0.0,
                output: 0.0,
                cache_write: 0.0,
                read_fraction: 0.0,
            }),
            ..Col3::default()
        };
        let frame = draw_to(&col3, 44, 24);
        let text = text_of(&frame);
        assert!(
            text.contains('+'),
            "the empty outer boundary remains:\n{text}"
        );
        assert!(text.contains("input 0 read 0"), "first footer row:\n{text}");
        assert!(
            text.contains("output 0 cache-write 0") && text.contains("read 0%"),
            "second footer row:\n{text}"
        );
        for y in 2..5 {
            for x in 2..42 {
                assert_eq!(
                    frame.get(x, y).map(|cell| cell.ch),
                    Some(' '),
                    "zero weights do not receive shaded interior cells at ({x},{y})"
                );
            }
        }
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

    #[test]
    fn recall_panel_states_render_honestly() {
        // Disabled / Quiet / Data (savings % + calls + threads).
        let disabled = text_of(&draw_to(&Col3::default(), 40, 24));
        assert!(disabled.contains("recall off"));

        let data = Col3 {
            savings: RecallView::Data(SavingsData {
                target_pct: Some(90),
                share: Box::new(ShareData {
                    overall: Some(42.0),
                    avoided_tokens: 6_000,
                    read_tokens: 8_285,
                    ..ShareData::default()
                }),
                ..SavingsData::default()
            }),
            calls: RecallView::Data(CallsData {
                total: 17,
                window_end_ms: 1_800_000,
                series: vec![ChannelCallSeries {
                    channel: "hook_inject".into(),
                    last_call_at: 1_799_000,
                    rate: vec![1.0; 30],
                }],
            }),
            active_sessions: ActiveSessionsView::Count(3),
            ..Col3::default()
        };
        let text = text_of(&draw_to(&data, 40, 24));
        assert!(
            text.contains("saved 42.0% (6.0k)"),
            "the shared share and the tokens behind it: \n{text}"
        );
        assert!(text.contains("17 serves"));
        assert!(text.contains("hook"), "channel legend name: \n{text}");
        assert!(text.contains("3 active sessions"));
    }

    /// A Col3 whose savings carry the USD estimate (the priced savings estimate fixtures).
    fn usd_col3(label: Option<&str>) -> Col3 {
        Col3 {
            savings: RecallView::Data(SavingsData {
                target_pct: Some(90),
                usd_saved_micros: label.map(|_| 6_250_000),
                usd_label: label.map(str::to_owned),
                usd_rows: vec![],
                usd_read_micros: None,
                usd_read_label: None,
                by_worktree: WorktreeAxis::default(),
                share: Box::new(ShareData {
                    overall: Some(42.0),
                    avoided_tokens: 6_000,
                    read_tokens: 8_285,
                    ..ShareData::default()
                }),
            }),
            ..Col3::default()
        }
    }

    /// the priced savings estimate: the chip's glyphs land exactly on the cells the shared
    /// solver reports — pixels == target, one geometry for draw and hit-test.
    #[test]
    fn usd_chip_draws_exactly_where_the_shared_solver_says() {
        let col3 = usd_col3(Some("~$6.25"));
        let rect = Rect {
            x: 0,
            y: 0,
            w: 64,
            h: 10,
        };
        let mut f = Frame::new(Size::new(64, 10));
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
        let mut scratch = Frame::new(Size::new(64, 10));
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
        // The headline row carries exactly two chips: the money and the
        // target beside it, in that order and never overlapping.
        assert_eq!(row.matches('[').count(), 2, "money and target: {row}");
        let target = recall_share_rect(&col3, false, inner).expect("the target chip");
        assert!(
            target.x >= chip.x + chip.w,
            "the chips abut without overlapping: {row}"
        );
    }

    /// Honest absence and degraded layouts draw no chip at all (never a
    /// fabricated figure, never a truncated one).
    #[test]
    fn usd_chip_absent_without_an_honest_figure_or_room() {
        // No figure → no chip on the headline row, which is the only row
        // either chip is ever drawn on. Scoped to that row rather than to the
        // whole panel: the period buttons own their own row of `[…]` chips,
        // and a panel-wide search for a bracket could no longer tell a
        // fabricated dollar figure from the row that lets a viewer change the
        // window.
        let none = usd_col3(None);
        let text = text_of(&draw_to(&none, 64, 24));
        let headline = text
            .lines()
            .find(|line| line.contains("saved 42.0% (6.0k)"))
            .expect("the recall headline is drawn");
        assert!(
            !headline.contains("~$"),
            "no money without a figure: {headline}"
        );
        assert_eq!(
            headline.matches('[').count(),
            1,
            "the target chip stands alone, and it is the modal's only door \
             on a repository with no seeded price: {headline}"
        );

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
        // headline "saved 42% (6.0k)" = 16 cells + 2 gap + 8 chip = 26 < 28
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

    /// The period buttons' glyphs land exactly on the cells the shared solver
    /// reports, and the selected one is the one drawn as live.
    ///
    /// Pixels == target: `hit_test` recomputes these rects from this same
    /// function, so a drift between them would put a viewer's press on the
    /// wrong period — the failure the one-solver rule exists to make
    /// impossible.
    #[test]
    fn window_button_glyphs_land_on_the_solver_rects() {
        let col3 = Col3 {
            recall_window: RecallWindow::Last7Days,
            ..usd_col3(None)
        };
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
        let mut scratch = Frame::new(Size::new(40, 10));
        let inner = panel_frame(
            Theme::rift_dark(),
            Section::Recall,
            false,
            "recall",
            rect,
            &mut scratch,
        );
        let buttons = recall_window_buttons(false, inner);
        assert_eq!(buttons.len(), 4, "all four periods or none");
        for button in &buttons {
            let drawn: String = (button.rect.x..button.rect.x + button.rect.w)
                .map(|x| f.get(x, button.rect.y).map_or(' ', |c| c.ch))
                .collect();
            assert_eq!(drawn, format!("[{}]", button.window.label()));
        }
        // Abutting, in narrowest-first order, all on one row.
        assert!(buttons
            .windows(2)
            .all(|p| p[0].rect.x + p[0].rect.w == p[1].rect.x));
        assert!(buttons.iter().all(|b| b.rect.y == buttons[0].rect.y));
        assert_eq!(
            buttons.iter().map(|b| b.window).collect::<Vec<_>>(),
            RecallWindow::ALL.to_vec()
        );
        // The selected period is the only one drawn in the live style — the
        // card has to say which window its figures cover.
        let live: Vec<RecallWindow> = buttons
            .iter()
            .filter(|b| {
                f.get(b.rect.x + 1, b.rect.y)
                    .is_some_and(|c| c.style == Theme::rift_dark().key_hint())
            })
            .map(|b| b.window)
            .collect();
        assert_eq!(live, vec![RecallWindow::Last7Days]);
    }

    /// A recall panel whose window holds both a read side and an avoided side.
    fn barred_col3(read: u64, avoided: u64) -> Col3 {
        Col3 {
            savings: RecallView::Data(SavingsData {
                target_pct: Some(90),
                usd_saved_micros: None,
                usd_label: None,
                usd_rows: vec![],
                usd_read_micros: None,
                usd_read_label: None,
                by_worktree: WorktreeAxis::default(),
                share: Box::new(ShareData {
                    overall: Some(3.0),
                    avoided_tokens: avoided,
                    read_tokens: read,
                    ..ShareData::default()
                }),
            }),
            ..Col3::default()
        }
    }

    /// The bars go through the **render seam**, so the ASCII floor draws them
    /// too — and they share one scale, so their lengths are comparable.
    ///
    /// Two independently-normalised bars would draw a repository that avoided
    /// 1 % of its reads exactly like one that avoided 99 %, which is the one
    /// thing a reader takes from putting the pair on one card.
    #[test]
    fn the_read_and_saved_bars_share_one_scale_through_the_render_seam() {
        let col3 = barred_col3(10_000, 1_000);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 44,
            h: 13,
        };
        for unicode in [false, true] {
            let mut f = Frame::new(Size::new(44, 13));
            let mut b = AsciiBackend::new(if unicode {
                Capabilities::UNICODE
            } else {
                Capabilities::empty()
            });
            draw_one(
                &col3,
                SectionId::Recall,
                false,
                unicode,
                false,
                rect,
                &mut b,
                &mut f,
            );
            let text = text_of(&f);
            assert!(text.contains("read"), "the read bar is labelled:\n{text}");
            assert!(text.contains("saved"), "the saved bar is labelled:\n{text}");
            assert!(text.contains("10k"), "the read figure:\n{text}");
            assert!(text.contains("1.0k"), "the saved figure:\n{text}");

            // Both bars drew, and the shorter one is genuinely shorter: the
            // saved row lights fewer cells than the read row on the same scale.
            let lit = |row: u16| -> usize {
                (0..f.width())
                    .filter(|x| {
                        f.get(*x, row)
                            .is_some_and(|c| !c.ch.is_whitespace() && c.ch != '│' && c.ch != '|')
                    })
                    .count()
            };
            // Inner rows: border(1) + headline, serves, periods → bars at 4, 5.
            let (read_row, saved_row) = (lit(4), lit(5));
            assert!(
                read_row > 0 && saved_row > 0,
                "both bars drew (unicode={unicode})"
            );
            assert!(
                read_row > saved_row,
                "one scale: 10k must draw longer than 1.0k (unicode={unicode}), \
                 got {read_row} vs {saved_row}\n{text}"
            );
        }
    }

    /// The unit toggle flips what the bars read in, and the chip says which
    /// unit is on screen now rather than which one a press would reach.
    #[test]
    fn the_unit_toggle_switches_the_bar_figures_and_names_the_live_unit() {
        let mut col3 = barred_col3(10_000, 1_000);
        if let RecallView::Data(sd) = &mut col3.savings {
            sd.usd_read_micros = Some(50_000_000);
            sd.usd_read_label = Some("~$50.00".to_owned());
            sd.usd_saved_micros = Some(5_000_000);
            sd.usd_label = Some("~$5.00".to_owned());
        }
        let render = |col3: &Col3| -> String {
            let mut f = Frame::new(Size::new(44, 13));
            let mut b = AsciiBackend::new(Capabilities::empty());
            draw_one(
                col3,
                SectionId::Recall,
                false,
                false,
                false,
                f.rect(),
                &mut b,
                &mut f,
            );
            text_of(&f)
        };

        let tokens = render(&col3);
        assert!(
            tokens.contains("[tok]"),
            "the live unit is named:\n{tokens}"
        );
        assert!(
            tokens.contains("10k") && tokens.contains("1.0k"),
            "{tokens}"
        );
        assert!(!tokens.contains("~$50.00"), "{tokens}");

        col3.recall_unit = RecallUnit::Usd;
        let dollars = render(&col3);
        assert!(
            dollars.contains("[usd]"),
            "the live unit is named:\n{dollars}"
        );
        assert!(
            dollars.contains("~$50.00") && dollars.contains("~$5.00"),
            "both bars are valued, tilde mandatory:\n{dollars}"
        );
        // The title is untouched while the request can be honoured.
        assert!(dollars.contains("recall "), "{dollars}");
        assert!(!dollars.contains("no seeded price"), "{dollars}");
    }

    /// Asked for dollars with no seeded price, the card shows **tokens** and
    /// says why in its title — it never invents a rate, and it never draws one
    /// priced bar beside one unpriced one.
    ///
    /// All-or-nothing because a mixed pair would draw a proportion nobody
    /// computed, which is worse than the token view the viewer did not ask
    /// for but can at least read.
    #[test]
    fn dollars_without_a_seeded_price_fall_back_to_tokens_and_say_so() {
        let mut col3 = barred_col3(10_000, 1_000);
        col3.recall_unit = RecallUnit::Usd;
        // Only the saved side priced: the pair is not comparable in dollars.
        if let RecallView::Data(sd) = &mut col3.savings {
            sd.usd_saved_micros = Some(5_000_000);
            sd.usd_label = Some("~$5.00".to_owned());
        }
        assert!(!recall_usd_available(&col3), "half a basis is not a basis");
        assert_eq!(recall_effective_unit(&col3), RecallUnit::Tokens);
        assert_eq!(recall_title(&col3), "recall · no seeded price");

        let mut f = Frame::new(Size::new(44, 13));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_one(
            &col3,
            SectionId::Recall,
            false,
            false,
            false,
            f.rect(),
            &mut b,
            &mut f,
        );
        let text = text_of(&f);
        assert!(
            text.contains("no seeded price"),
            "the reason is on the card:\n{text}"
        );
        assert!(
            text.contains("[tok]"),
            "the chip names what is drawn:\n{text}"
        );
        // The BAR rows only. The headline's own `[~$5.00]` savings chip is a
        // different affordance with its own basis and is right to keep
        // drawing; what must not appear is a dollar figure on a bar whose
        // partner has none.
        // Rows 4 and 5: border, headline, serves, periods, then the two bars.
        // Selected by position rather than by text, because "saved" also
        // opens the headline and a filter on it would sweep three rows.
        let bars: Vec<&str> = text.lines().skip(4).take(2).collect();
        assert_eq!(bars.len(), 2, "both bars drew:\n{text}");
        assert!(
            bars[0].contains("read") && bars[1].contains("saved"),
            "{text}"
        );
        for row in &bars {
            assert!(
                !row.contains('$'),
                "a priced bar beside an unpriced one: {row}"
            );
        }
        assert!(
            bars[0].contains("10k"),
            "the read bar in tokens: {}",
            bars[0]
        );
        assert!(
            bars[1].contains("1.0k"),
            "the saved bar in tokens: {}",
            bars[1]
        );
    }

    /// The title is byte-identical to the bare one whenever the card shows
    /// what was asked for — the activity card's off-strip rule, and what keeps
    /// every golden that does not exercise the refusal unchanged.
    #[test]
    fn the_recall_title_is_bare_until_the_dollar_view_is_refused() {
        let priced = {
            let mut col3 = barred_col3(10_000, 1_000);
            if let RecallView::Data(sd) = &mut col3.savings {
                sd.usd_read_micros = Some(1);
                sd.usd_read_label = Some("~<$0.01".to_owned());
                sd.usd_saved_micros = Some(1);
                sd.usd_label = Some("~<$0.01".to_owned());
            }
            col3
        };
        assert_eq!(recall_title(&Col3::default()), "recall");
        assert_eq!(recall_title(&barred_col3(10, 1)), "recall", "tokens asked");
        assert_eq!(recall_title(&priced), "recall", "dollars available");
        let mut asked = priced.clone();
        asked.recall_unit = RecallUnit::Usd;
        assert_eq!(recall_title(&asked), "recall", "dollars asked and given");
    }

    /// The toggle sits on the period row without touching the periods, at
    /// every width the row is drawn at.
    #[test]
    fn the_unit_chip_never_collides_with_a_period_button() {
        let col3 = barred_col3(10, 1);
        for w in 20..90u16 {
            let inner = Rect {
                x: 1,
                y: 1,
                w,
                h: 8,
            };
            let buttons = recall_window_buttons(false, inner);
            let Some(chip) = recall_unit_rect(&col3, false, inner) else {
                continue;
            };
            assert!(!buttons.is_empty(), "a chip without a period row at w={w}");
            assert_eq!(chip.y, buttons[0].rect.y, "same row, w={w}");
            let last = buttons.last().expect("periods");
            assert!(
                chip.x > last.rect.x + last.rect.w,
                "the chip overlaps the periods at w={w}"
            );
            assert!(
                u32::from(chip.x) + u32::from(chip.w) <= u32::from(inner.x) + u32::from(inner.w),
                "the chip runs past the panel at w={w}"
            );
        }
        // The tiers with no period row have no chip either.
        assert_eq!(
            recall_unit_rect(
                &col3,
                true,
                Rect {
                    x: 1,
                    y: 1,
                    w: 60,
                    h: 8
                }
            ),
            None,
            "collapsed"
        );
    }

    /// A window with nothing on either side draws no bars at all, and gives
    /// the rows back to the call chart.
    ///
    /// Absence and a measured zero are different facts. Two empty rows
    /// labelled `read` and `saved` would look like a pair of zeroes somebody
    /// measured, which is exactly the claim the card has no basis for.
    #[test]
    fn an_unmeasured_window_draws_no_bars() {
        let col3 = barred_col3(0, 0);
        let mut f = Frame::new(Size::new(44, 13));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_one(
            &col3,
            SectionId::Recall,
            false,
            false,
            false,
            f.rect(),
            &mut b,
            &mut f,
        );
        let text = text_of(&f);
        assert!(!text.contains("saved  "), "no bar rows:\n{text}");
        assert!(
            text.contains("[24h]"),
            "the period row is unaffected:\n{text}"
        );
    }

    /// A positive value always lights at least one sample column.
    ///
    /// A bar that rounded a small-but-real saving down to nothing would draw
    /// absence, and the card would be claiming a measurement it did not make.
    #[test]
    fn a_tiny_but_real_bar_is_short_never_empty() {
        let area = Rect {
            x: 0,
            y: 0,
            w: 20,
            h: 1,
        };
        let colour = Section::Recall.accent();
        assert!(crate::charts::hbar_samples(0.0, area, colour).is_empty());
        assert!(crate::charts::hbar_samples(-1.0, area, colour).is_empty());
        assert!(crate::charts::hbar_samples(f64::NAN, area, colour).is_empty());
        assert!(
            !crate::charts::hbar_samples(0.000_01, area, colour).is_empty(),
            "a real value never rounds to an empty bar"
        );
        // Monotonic in the fraction, and full at 1.0 (and beyond, clamped).
        let width = |f: f64| crate::charts::hbar_samples(f, area, colour).len();
        assert!(width(0.25) < width(0.5) && width(0.5) < width(1.0));
        assert_eq!(width(1.0), width(2.0), "the fill clamps at the area");
    }

    /// A narrow window over a quiet ledger puts the card into its placeholder
    /// state — and that is exactly when the way back must still be on screen.
    ///
    /// Drawn before the placeholder's early return for that reason. Without
    /// it a viewer who asked for the last 24 hours of a repository that served
    /// last week would be looking at "enabled, no serves yet" with no visible
    /// way to widen the window again.
    #[test]
    fn the_period_row_survives_the_placeholder_states() {
        for (savings, calls) in [
            (RecallView::Quiet, RecallView::Quiet),
            (RecallView::Disabled, RecallView::Disabled),
        ] {
            let col3 = Col3 {
                savings,
                calls,
                recall_window: RecallWindow::Today,
                ..Col3::default()
            };
            let mut f = Frame::new(Size::new(40, 10));
            let mut b = AsciiBackend::new(Capabilities::empty());
            draw_one(
                &col3,
                SectionId::Recall,
                false,
                false,
                false,
                f.rect(),
                &mut b,
                &mut f,
            );
            let text = text_of(&f);
            for window in RecallWindow::ALL {
                assert!(
                    text.contains(&format!("[{}]", window.label())),
                    "the {} button vanished behind the placeholder:\n{text}",
                    window.label()
                );
            }
        }
    }

    /// The degraded tiers draw no period row, and they draw **none of it**:
    /// a half-row of buttons reads as "these are the periods", and a viewer
    /// would never learn about the ones that did not fit.
    #[test]
    fn the_period_row_is_all_four_buttons_or_nothing() {
        let inner = |w: u16, h: u16| Rect { x: 1, y: 1, w, h };
        assert!(
            recall_window_buttons(true, inner(38, 8)).is_empty(),
            "collapsed"
        );
        assert!(
            recall_window_buttons(false, inner(27, 8)).is_empty(),
            "narrow"
        );
        assert!(
            recall_window_buttons(false, inner(38, 6)).is_empty(),
            "short"
        );
        for w in 28..80u16 {
            let solved = recall_window_buttons(false, inner(w, 8));
            assert!(
                solved.is_empty() || solved.len() == RecallWindow::ALL.len(),
                "a partial period row at w={w}: {solved:?}"
            );
            // Whatever is drawn stays inside the panel.
            for button in &solved {
                assert!(u32::from(button.rect.x) + u32::from(button.rect.w) <= u32::from(1 + w));
            }
        }
    }

    /// A token panel with a total, at a width column 3 actually has.
    fn tokens_col3(total: f64) -> Col3 {
        Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![1.0, 2.0],
                total,
                by_worktree: WorktreeAxis {
                    rows: vec![WorktreeSlice {
                        name: Some("main".to_owned()),
                        tokens: 7_500,
                    }],
                    family_only: 0,
                },
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input: 1.0,
                read: 1.0,
                output: 1.0,
                cache_write: 1.0,
                read_fraction: 0.5,
            }),
            ..Col3::default()
        }
    }

    /// the family view: the `Σ` total IS the axis's affordance — one solver
    /// feeds the draw and the hit-test, so the clickable cells are exactly
    /// the drawn glyphs, and they are drawn in the key-hint style that marks
    /// an act (not the section-title style the rest of the title carries).
    #[test]
    fn the_sigma_total_is_the_drawn_axis_affordance() {
        let col3 = tokens_col3(7_500.0);
        let rect = Rect {
            x: 0,
            y: 0,
            w: 34,
            h: 9,
        };
        let mut f = Frame::new(Size::new(34, 9));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_one(
            &col3,
            SectionId::TokenUsage,
            false,
            false,
            false,
            rect,
            &mut b,
            &mut f,
        );
        let chip = token_total_rect(&col3, rect).expect("a total is an affordance");
        let drawn: String = (chip.x..chip.x + chip.w)
            .map(|x| f.get(x, chip.y).map_or(' ', |c| c.ch))
            .collect();
        assert_eq!(drawn, "Σ 7.5k", "the solver names the total's own cells");
        assert_eq!(chip.y, rect.y, "the title row, not the body");
        // Marked as an act, and distinguishable from the title beside it.
        let theme = Theme::rift_dark();
        assert_eq!(
            f.get(chip.x, chip.y).map(|c| c.style),
            Some(theme.key_hint())
        );
        assert_ne!(
            theme.key_hint(),
            theme.title(Section::TokenUsage),
            "an affordance that looked exactly like its title would teach nothing"
        );
        // The word before it is still the title, in the title's own style.
        assert_eq!(
            f.get(chip.x - 2, chip.y).map(|c| c.style),
            Some(theme.title(Section::TokenUsage))
        );
    }

    /// No total, no border, or a clipped title → no target at all. A
    /// truncated affordance is not one, and a click must never land on a
    /// panel that never offered it (the `recall_cost_rect` rule, the priced savings estimate).
    #[test]
    fn no_total_and_no_room_offer_no_axis_target() {
        let wide = Rect {
            x: 0,
            y: 0,
            w: 34,
            h: 9,
        };
        assert_eq!(
            token_total_rect(&Col3::default(), wide),
            None,
            "no facts in scope ⇒ no total ⇒ nothing to decompose"
        );
        let col3 = tokens_col3(7_500.0);
        assert_eq!(
            token_total_rect(
                &col3,
                Rect {
                    x: 0,
                    y: 0,
                    w: 1,
                    h: 9
                }
            ),
            None,
            "too narrow to carry a frame ⇒ no title row exists"
        );
        assert_eq!(
            token_total_rect(
                &col3,
                Rect {
                    x: 0,
                    y: 0,
                    w: 34,
                    h: 1
                }
            ),
            None,
            "too short to carry a frame"
        );
        assert_eq!(
            token_total_rect(
                &col3,
                Rect {
                    x: 0,
                    y: 0,
                    w: 20,
                    h: 9
                }
            ),
            None,
            "`token usage  Σ 7.5k` does not fit whole ⇒ no partial chip"
        );
        // …and the panel still draws its (clipped) title at that width: the
        // affordance sheds, the panel does not.
        let mut f = Frame::new(Size::new(20, 9));
        let mut b = AsciiBackend::new(Capabilities::empty());
        draw_one(
            &col3,
            SectionId::TokenUsage,
            false,
            false,
            false,
            Rect {
                x: 0,
                y: 0,
                w: 20,
                h: 9,
            },
            &mut b,
            &mut f,
        );
        let row: String = (0..20).map(|x| f.get(x, 0).map_or(' ', |c| c.ch)).collect();
        assert!(row.contains("token usage"), "{row}");
    }

    /// the family view + worktree provenance: the Tokens detail's body names each
    /// member and says what `unknown` means. Honest absence has its own
    /// words — an axis with nothing in it draws no empty table.
    #[test]
    fn the_tokens_detail_body_discloses_the_rule_that_put_each_fact_in_its_row() {
        let mut col3 = tokens_col3(1_115.0);
        if let TokensView::Data(d) = &mut col3.tokens {
            d.by_worktree = WorktreeAxis {
                rows: vec![
                    WorktreeSlice {
                        name: Some("main".to_owned()),
                        tokens: 750,
                    },
                    WorktreeSlice {
                        name: Some("swt-d".to_owned()),
                        tokens: 300,
                    },
                    WorktreeSlice {
                        name: None,
                        tokens: 65,
                    },
                ],
                family_only: 0,
            };
        }
        let body = worktree_axis_lines(&col3).join("\n");
        assert!(body.contains("family total  1,115 tok"), "{body}");
        assert!(body.contains("main     750 tok"), "{body}");
        assert!(body.contains("swt-d    300 tok"), "{body}");
        assert!(body.contains("unknown   65 tok"), "{body}");
        assert!(
            body.contains("the LAST worktree it was"),
            "the rule is stated, not implied: {body}"
        );
        assert!(body.contains("never relabelled."), "{body}");
        // Absence, in both of its shapes.
        assert!(worktree_axis_lines(&Col3::default())
            .join(" ")
            .contains("no token facts in scope"));
        let mut bare = tokens_col3(5.0);
        if let TokensView::Data(d) = &mut bare.tokens {
            d.by_worktree = WorktreeAxis::default();
        }
        assert!(worktree_axis_lines(&bare)
            .join(" ")
            .contains("no worktree facts in scope"));
    }

    /// the priced savings estimate: the modal discloses the whole basis — rows that visibly
    /// sum, the class assumption, unpriced coverage, and the
    /// estimate-of-an-estimate statement. Every money string is pre-rendered
    /// (this function never formats money).
    #[test]
    fn breakdown_lines_disclose_the_whole_basis() {
        let sd = SavingsData {
            target_pct: Some(90),
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
            usd_read_micros: None,
            usd_read_label: None,
            by_worktree: WorktreeAxis::default(),
            share: Box::new(ShareData::default()),
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

    // ---- the family view: the "by worktree" axis ---------------------------

    /// The table renders the axis's OWN order (the feed establishes it — see
    /// `feeds::worktree_axis`; a renderer that re-sorted would hide a
    /// producer that had stopped), aligns on the widest name and the widest
    /// count, and spells an absent name `unknown` — never `main`, never
    /// blank (worktree provenance).
    #[test]
    fn the_axis_table_aligns_and_names_unknown_as_unknown() {
        let axis = WorktreeAxis {
            rows: vec![
                WorktreeSlice {
                    name: Some("swt-d".to_owned()),
                    tokens: 900_000,
                },
                WorktreeSlice {
                    name: Some("main".to_owned()),
                    tokens: 12_345,
                },
                WorktreeSlice {
                    name: None,
                    tokens: 1_500,
                },
            ],
            family_only: 0,
        };
        let rows = worktree_rows(&axis);
        assert_eq!(
            rows,
            vec![
                "swt-d    900,000 tok".to_owned(),
                "main      12,345 tok".to_owned(),
                "unknown    1,500 tok".to_owned(),
            ],
            "aligned, in the axis's order, `unknown` spelled out"
        );
        // The word is worktree provenance's, and the absent name never borrows the
        // main work tree's.
        assert!(rows.iter().any(|r| r.starts_with("unknown")));
        assert_eq!(
            rows.iter().filter(|r| r.starts_with("main")).count(),
            1,
            "only the row that IS main says main"
        );
    }

    /// ★ the family view: a serve carries no session key, so it cannot be
    /// attributed to a member — it counts under the family only, and the
    /// modal SAYS so rather than splitting the figure on a guess. (No serve
    /// relation in `recall.db` holds a session id: `memd_audit_event`,
    /// `serve_call_event`, `serve_session_event`.)
    #[test]
    fn serves_with_no_session_key_count_under_family_only() {
        let sd = SavingsData {
            target_pct: Some(90),
            usd_saved_micros: Some(2_500_000),
            usd_label: Some("~$2.50".to_owned()),
            usd_rows: vec![UsdRow {
                model: "claude-opus-4-8".to_owned(),
                tokens: 1_000_000,
                price_per_mtok_micros: Some(5_000_000),
                subtotal_micros: Some(2_500_000),
                rate_label: Some("$5.00/MTok".to_owned()),
                subtotal_label: Some("~$2.50".to_owned()),
            }],
            usd_read_micros: None,
            usd_read_label: None,
            by_worktree: WorktreeAxis {
                rows: Vec::new(),
                family_only: 1_000_000,
            },
            share: Box::new(ShareData::default()),
        };
        let body = usd_breakdown_lines(&sd).join("\n");
        assert!(body.contains("by worktree"), "the axis is present: {body}");
        assert!(
            body.contains("family-only  1,000,000 tok"),
            "the whole avoided total sits under family-only: {body}"
        );
        assert!(
            body.contains("no serve relation records the session that was"),
            "the modal says WHY, it does not just show a row: {body}"
        );
        assert!(
            body.contains("this figure is the family's"),
            "and what the figure therefore means: {body}"
        );
        // Nothing was invented: no member name, and no `unknown` row either
        // (unknown is a session with no observation; this has no session).
        assert!(!body.contains("unknown"), "{body}");
    }

    /// An axis with nothing in it adds nothing to the modal — the savings body
    /// is byte-identical to what it was before the axis existed, so a project with no
    /// savings basis grows no empty table.
    #[test]
    fn an_empty_axis_adds_no_block_to_the_savings_modal() {
        let sd = SavingsData {
            target_pct: Some(90),
            usd_saved_micros: Some(1),
            usd_label: Some("~$0.01".to_owned()),
            usd_rows: vec![UsdRow {
                model: "m".to_owned(),
                tokens: 10,
                price_per_mtok_micros: Some(1),
                subtotal_micros: Some(1),
                rate_label: Some("$0.00/MTok".to_owned()),
                subtotal_label: Some("~$0.01".to_owned()),
            }],
            usd_read_micros: None,
            usd_read_label: None,
            by_worktree: WorktreeAxis::default(),
            share: Box::new(ShareData::default()),
        };
        assert!(!usd_breakdown_lines(&sd).join("\n").contains("by worktree"));
        assert!(WorktreeAxis::default().is_empty());
        assert_eq!(WorktreeAxis::default().total(), 0);
    }

    /// [`WorktreeAxis::total`] is the sum it claims to be, and it saturates
    /// rather than wrapping on a corrupt projection's absurd counts.
    #[test]
    fn the_axis_total_is_its_rows_plus_the_family_remainder() {
        let axis = WorktreeAxis {
            rows: vec![
                WorktreeSlice {
                    name: Some("a".to_owned()),
                    tokens: 7,
                },
                WorktreeSlice {
                    name: None,
                    tokens: 3,
                },
            ],
            family_only: 5,
        };
        assert_eq!(axis.total(), 15);
        let absurd = WorktreeAxis {
            rows: vec![
                WorktreeSlice {
                    name: Some("a".to_owned()),
                    tokens: u64::MAX,
                },
                WorktreeSlice {
                    name: Some("b".to_owned()),
                    tokens: u64::MAX,
                },
            ],
            family_only: 1,
        };
        assert_eq!(absurd.total(), u64::MAX, "saturates, never wraps");
    }

    #[test]
    fn fmt_count_groups_thousands() {
        assert_eq!(fmt_count(0), "0");
        assert_eq!(fmt_count(999), "999");
        assert_eq!(fmt_count(1_000), "1,000");
        assert_eq!(fmt_count(14_594_095), "14,594,095");
    }

    /// the priced savings estimate: money never animates — the estimate and its rows are
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

    fn savings(avoided: u64) -> RecallView<SavingsData> {
        RecallView::Data(SavingsData {
            target_pct: Some(90),
            share: Box::new(ShareData {
                overall: Some(3.0),
                avoided_tokens: avoided,
                read_tokens: 1_000,
                ..ShareData::default()
            }),
            ..SavingsData::default()
        })
    }

    /// The savings card **snaps**: every figure on it is a headline over one
    /// window, and a frame between two measurements is a number no window
    /// held. Only the per-channel call chart, a series over time, animates.
    #[test]
    fn the_savings_card_is_taken_from_the_target_frame_whole() {
        let from = Col3 {
            savings: savings(100),
            ..Col3::default()
        };
        let to = Col3 {
            savings: savings(700),
            ..Col3::default()
        };
        for t in [0.0, 0.5, 1.0] {
            assert_eq!(
                interpolate(&from, &to, t).savings,
                to.savings,
                "the card is the target's at t={t}"
            );
        }
        assert!(
            !should_animate(&from, &to),
            "a changed savings figure is not motion"
        );
    }

    #[test]
    fn placeholder_to_data_snaps_not_tweens() {
        let from = Col3::default(); // NoActivity everywhere
        let to = Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![5.0],
                total: 5.0,
                by_worktree: WorktreeAxis::default(),
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

    /// The call chart is the card's one series over time, so it is the one
    /// thing on it with motion to show.
    #[test]
    fn recall_call_series_change_animates() {
        let card = |value: f64| Col3 {
            savings: savings(700),
            calls: RecallView::Data(CallsData {
                total: 2,
                window_end_ms: 1_800_000,
                series: vec![ChannelCallSeries {
                    channel: "hook_inject".into(),
                    last_call_at: 1_799_000,
                    rate: vec![value; 30],
                }],
            }),
            ..Col3::default()
        };
        let (from, to) = (card(1.0), card(4.0));
        assert!(should_animate(&from, &to));
        match interpolate(&from, &to, 0.5).calls {
            RecallView::Data(d) => assert_eq!(d.series[0].rate[0], 2.5, "half-way"),
            _ => panic!("expected Data"),
        }
    }

    #[test]
    fn token_composition_snaps_atomically() {
        let make = |input, read| Col3 {
            tokens: TokensView::Data(TokensData {
                rate: vec![input, read],
                total: input + read,
                by_worktree: WorktreeAxis::default(),
            }),
            breakdown: BreakdownView::Data(BreakdownData {
                input,
                read,
                output: 0.0,
                cache_write: 0.0,
                read_fraction: read / (input + read),
            }),
            ..Col3::default()
        };
        let from = make(10.0, 10.0);
        let to = make(20.0, 80.0);
        assert!(!should_animate(&from, &to));
        let mid = interpolate(&from, &to, 0.5);
        assert_eq!(mid.tokens, to.tokens);
        assert_eq!(mid.breakdown, to.breakdown);
    }

    #[test]
    fn recall_lines_snap_when_the_window_or_channel_identity_changes() {
        let calls = |window_end_ms, channel: &str, value| CallsData {
            total: 1,
            window_end_ms,
            series: vec![ChannelCallSeries {
                channel: channel.into(),
                last_call_at: window_end_ms - 1,
                rate: vec![value; 30],
            }],
        };
        let base = calls(1_800_000, "hook_inject", 1.0);
        assert!(calls_can_animate(
            &base,
            &calls(1_800_000, "hook_inject", 2.0)
        ));
        assert!(!calls_can_animate(
            &base,
            &calls(1_860_000, "hook_inject", 2.0)
        ));
        assert!(!calls_can_animate(
            &base,
            &calls(1_800_000, "skill_cli", 2.0)
        ));
    }

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
        assert_eq!(fmt_km(1_500.0), "1.5k");
        assert_eq!(fmt_km(10_000.0), "10k");
        assert_eq!(fmt_km(120_000.0), "120k");
        assert_eq!(fmt_km(1_250_000.0), "1.2M"); // {:.1} rounds half-to-even
        assert_eq!(fmt_km(3_400_000.0), "3.4M");
        assert_eq!(fmt_km(f64::NAN), "0");
        assert_eq!(fmt_km(-5.0), "0");
    }

    fn share_fixture(overall: Option<f64>) -> SavingsData {
        SavingsData {
            target_pct: Some(90),
            usd_saved_micros: None,
            usd_label: None,
            usd_rows: Vec::new(),
            usd_read_micros: None,
            usd_read_label: None,
            by_worktree: WorktreeAxis::default(),
            share: Box::new(ShareData {
                window: None,
                overall,
                honesty: 9_803,
                avoided_tokens: 10_000,
                read_tokens: 9_000,
                sessions_with_reads: 3,
                reads_unmeasured: 0,
                by_family: Some(10.0),
                excluding: Some(5.3),
                excluded: vec!["codex".to_owned()],
                families: vec![
                    FamilyShareRow {
                        family: "claude-code".to_owned(),
                        share: Some(5.3),
                        honesty: 340,
                        sessions_with_reads: 3,
                        decided_without_reads: false,
                    },
                    FamilyShareRow {
                        family: "codex".to_owned(),
                        share: Some(100.0),
                        honesty: 500,
                        sessions_with_reads: 0,
                        decided_without_reads: true,
                    },
                ],
            }),
        }
    }

    /// An unanswerable share draws no percentage on the card — `saved —`,
    /// never `0 %`, because the two mean opposite things and only one of them
    /// is a measurement — while the **target** chip beside it stays, since a
    /// target is a goal the product states rather than a figure it measured.
    /// It is also the modal's door, and a door that vanished with the
    /// measurement would take the explanation of the absence with it.
    #[test]
    fn an_unanswerable_share_draws_no_percentage_but_keeps_its_door() {
        let col3 = Col3 {
            savings: RecallView::Data(share_fixture(None)),
            ..Col3::default()
        };
        let inner = Rect {
            x: 0,
            y: 0,
            w: 60,
            h: 10,
        };
        assert_eq!(recall_headlines(&col3).0, "saved —");
        assert!(recall_share_rect(&col3, false, inner).is_some());

        let answered = Col3 {
            savings: RecallView::Data(share_fixture(Some(3.04))),
            ..Col3::default()
        };
        assert_eq!(
            recall_headlines(&answered).0,
            "saved 3.0% (10k)",
            "the shared share, one decimal like the command line"
        );
        assert_eq!(
            share_chip_label(&share_fixture(Some(3.04))).as_deref(),
            Some("[of 90% target]")
        );
        // A card with no target draws no chip: absence, not a zero target.
        let targetless = SavingsData {
            target_pct: None,
            ..share_fixture(Some(3.04))
        };
        assert_eq!(share_chip_label(&targetless), None);
        let chip = recall_share_rect(&answered, false, inner).expect("a target");
        assert_eq!(chip.w, 15, "the rect is exactly the glyphs it draws");
        // Collapsed and narrow shed it, like every other chip on this row.
        assert_eq!(recall_share_rect(&answered, true, inner), None);
        assert_eq!(
            recall_share_rect(
                &answered,
                false,
                Rect {
                    x: 0,
                    y: 0,
                    w: 27,
                    h: 10
                }
            ),
            None
        );
    }

    /// The breakdown modal carries the per-family rows (share, honesty, n) and
    /// names the family whose reads were never captured — the number the
    /// excluding variant exists for is explained where it is read.
    #[test]
    fn the_breakdown_modal_publishes_the_per_family_shares() {
        let sd = share_fixture(Some(10.0));
        let body = usd_breakdown_lines(&sd).join("\n");
        assert!(body.contains("savings share"), "{body}");
        assert!(body.contains("overall              10.0%"), "{body}");
        assert!(body.contains("sessions with reads  3"), "{body}");
        assert!(body.contains("claude-code"), "{body}");
        assert!(
            body.contains("codex") && body.contains("decided, no captured reads"),
            "{body}"
        );
        assert!(
            body.contains("reads: 5.3% (excluded: codex)"),
            "the excluding variant names what it excluded: {body}"
        );
        // Reachable with no dollar figure at all: the share chip is the door
        // on every project whose models carry no seeded price.
        assert!(body.contains("no dollar figure"), "{body}");
    }

    /// **The card names the period it is reporting.** A person who moved the
    /// window with `rift savings reset` and then read a card still captioned
    /// "since the projection was reset" would be reading the right number
    /// under the wrong claim — and would have no way to tell.
    #[test]
    fn the_modal_names_the_window_the_share_covers() {
        let mut sd = share_fixture(Some(10.0));
        let body = usd_breakdown_lines(&sd).join("\n");
        assert!(body.contains("since the projection was reset"), "{body}");

        sd.share.window = Some("since the window was reset 2026-09-07T18:00:00.000Z".to_owned());
        let moved = usd_breakdown_lines(&sd).join("\n");
        assert!(
            moved.contains("since the window was reset 2026-09-07T18:00:00.000Z"),
            "{moved}"
        );
        assert!(
            !moved.contains("since the projection was reset"),
            "one window sentence, not two: {moved}"
        );

        // And with nothing to report, the absence is dated too — "unanswerable
        // since the reset" and "unanswerable since Tuesday" are different
        // claims about the same plane.
        sd.share.overall = None;
        let empty = savings_share_lines(&sd.share, sd.target_pct).join("\n");
        assert!(empty.contains("unanswerable"), "{empty}");
        assert!(
            empty.contains("since the window was reset 2026-09-07T18:00:00.000Z"),
            "{empty}"
        );
    }

    /// The share is a headline, not an animation: interpolating two frames
    /// takes it from the target whole, so no frame of a tween is ever read as
    /// a measurement.
    #[test]
    fn the_share_is_never_tweened() {
        let from = Col3 {
            savings: RecallView::Data(share_fixture(Some(1.0))),
            ..Col3::default()
        };
        let to = Col3 {
            savings: RecallView::Data(share_fixture(Some(9.0))),
            ..Col3::default()
        };
        let mid = interpolate(&from, &to, 0.5);
        let RecallView::Data(sd) = mid.savings else {
            panic!("data");
        };
        assert_eq!(sd.share.overall, Some(9.0));
    }
}
