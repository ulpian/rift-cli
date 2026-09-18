//! The two workflow panels — dispatch and recap — and their view-models.
//!
//! These two carry `rift dispatch` / `rift recap` facts, and they carry them as
//! FINISHED TEXT: every label below has already been through
//! [`crate::escape_for_terminal`] in the data plane, and every money figure has
//! already been through the cost crate's formatter. A renderer that had to
//! escape or to format would be a second place either rule could be got wrong.
//!
//! **The cards never re-derive a citation ordinal.** `rift dispatch --json` and
//! `rift recap --json` replace payload-derived strings with `cite:<n>` and print
//! the map on their human surface; a card is a human surface reading the data
//! plane directly, so it shows the value itself. What it must never do is print
//! or invent an ordinal — a dashboard and a document that disagreed about which
//! value `cite:3` names would be worse than either alone.
//!
//! Both are text tables over already-finished values, which is why neither takes
//! a `RenderBackend`: there is no chart, no shader tier, and nothing to degrade.
//! Every string they draw came out of the data plane escaped, and every number
//! is a count the window yielded — so a hostile filename can occupy a row here
//! and still be inert text.
//!
//! One module for the pair rather than one each, because they share the row
//! budget rule (`fit`) and the absence vocabulary, and because a `dispatch`
//! module beside [`crate::dispatch`] would name two different things.

use crate::layout::Rect;
use crate::render::frame::Frame;
use crate::theme::{Section, Theme};

use super::format::{f64_of, fmt_count, fmt_km};
use super::{body, panel_frame, Col3};

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

pub(super) fn draw_dispatch_panel(
    col3: &Col3,
    theme: Theme,
    focused: bool,
    collapsed: bool,
    rect: Rect,
    frame: &mut Frame,
) {
    let headline = dispatch_headline(&col3.dispatch);
    if collapsed {
        let tail = dispatch_collapsed_tail(&col3.dispatch);
        body(frame, rect, 0, &format!("{headline}{tail}"), theme.body());
        return;
    }
    let inner = panel_frame(theme, Section::Dispatch, focused, &headline, rect, frame);
    match &col3.dispatch {
        // Two different absences, two different sentences — never one zero row.
        DispatchView::NoProjection => {
            body(frame, inner, 0, "(no capture projection here)", theme.dim());
        }
        DispatchView::NoActivity => {
            body(frame, inner, 0, "(nothing captured yet)", theme.dim());
        }
        DispatchView::Data(d) => draw_dispatch_facts(d, theme, inner, frame),
    }
}

/// The dispatch panel's title: the checkpoint count when the window has one.
fn dispatch_headline(view: &DispatchView) -> String {
    match view {
        DispatchView::Data(d) => format!("dispatch  {} checkpoints", fmt_count(d.checkpoints)),
        DispatchView::NoProjection | DispatchView::NoActivity => "dispatch".to_string(),
    }
}

/// What the collapsed dispatch row carries after its headline.
///
/// A one-row panel has no border and therefore no title — `chrome::panel`
/// returns the area untouched below two rows — so the collapsed line has to
/// name the panel itself, or the surviving row is an unlabelled number.
fn dispatch_collapsed_tail(view: &DispatchView) -> String {
    match view {
        DispatchView::Data(d) => format!(" · {} sessions", fmt_count(d.sessions)),
        DispatchView::NoProjection => " · no projection".to_string(),
        DispatchView::NoActivity => " · nothing captured".to_string(),
    }
}

/// The window's facts, row by row: the sessions, the attribution split, the
/// time row when it has something to say, the files row and the files.
fn draw_dispatch_facts(d: &DispatchData, theme: Theme, inner: Rect, frame: &mut Frame) {
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
    let dy = draw_dispatch_time_row(d, theme, inner, 2, frame);
    draw_dispatch_files_row(d, theme, inner, dy, frame);
    draw_touched_files(d, theme, inner, dy + 1, frame);
}

/// The time row, returning the row the next block starts on.
///
/// It is the panel's honesty about `checkpoint.created_at`: a shard placed by
/// the sessions it archives is a LOWER BOUND, and a shard that cannot be
/// placed at all is excluded from bounded windows rather than absent. A window
/// where neither happened does not draw the row at all.
fn draw_dispatch_time_row(
    d: &DispatchData,
    theme: Theme,
    inner: Rect,
    dy: u16,
    frame: &mut Frame,
) -> u16 {
    if d.derived == 0 && d.unresolved == 0 {
        return dy;
    }
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
    dy + 1
}

/// The files row. Unasked is not zero: the policy never answered, and the row
/// says so in the dim register rather than claiming a clean window.
fn draw_dispatch_files_row(
    d: &DispatchData,
    theme: Theme,
    inner: Rect,
    dy: u16,
    frame: &mut Frame,
) {
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
        None => body(
            frame,
            inner,
            dy,
            &format!("files {}  ·  sensitive unasked", fmt_count(d.files_total)),
            theme.dim(),
        ),
    }
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

pub(super) fn draw_recap_panel(
    col3: &Col3,
    theme: Theme,
    focused: bool,
    collapsed: bool,
    rect: Rect,
    frame: &mut Frame,
) {
    let headline = recap_headline(&col3.recap);
    if collapsed {
        let tail = recap_collapsed_tail(&col3.recap);
        body(frame, rect, 0, &format!("{headline}{tail}"), theme.body());
        return;
    }
    let inner = panel_frame(theme, Section::Recap, focused, &headline, rect, frame);
    match &col3.recap {
        RecapView::NoProjection => {
            body(frame, inner, 0, "(no capture projection here)", theme.dim());
        }
        RecapView::NoActivity => {
            body(frame, inner, 0, "(no agent sessions yet)", theme.dim());
        }
        RecapView::Data(d) => draw_recap_facts(d, theme, inner, frame),
    }
}

/// The recap panel's title: the session count when the window has one.
fn recap_headline(view: &RecapView) -> String {
    match view {
        RecapView::Data(d) => format!("recap  {} sessions", fmt_count(d.sessions)),
        RecapView::NoProjection | RecapView::NoActivity => "recap".to_string(),
    }
}

/// What the collapsed recap row carries after its headline — as the dispatch
/// tail: the collapsed row is the only row, so it names its own panel.
fn recap_collapsed_tail(view: &RecapView) -> String {
    match view {
        RecapView::Data(d) => format!(" · {} tokens", fmt_km(f64_of(d.tokens))),
        RecapView::NoProjection => " · no projection".to_string(),
        RecapView::NoActivity => " · no sessions".to_string(),
    }
}

/// The window's facts: the token total with its cost, the split under its
/// axis label, and recall's ledger last.
fn draw_recap_facts(d: &RecapData, theme: Theme, inner: Rect, frame: &mut Frame) {
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
    let dy = draw_recap_split(d, theme, inner, 2, frame);
    draw_recap_ledger(d.savings, theme, inner, dy, frame);
}

/// The split rows from `dy` down, returning the row the ledger starts on.
///
/// Recall's tail is reserved before the split is drawn, so a repository with
/// many agents cannot push the ledger off the panel — the split is the part
/// that degrades, and it says how much it dropped.
fn draw_recap_split(d: &RecapData, theme: Theme, inner: Rect, dy: u16, frame: &mut Frame) -> u16 {
    let tail = if d.savings.is_some() { 2u16 } else { 1 };
    let split_room = usize::from(inner.h.saturating_sub(dy + tail));
    let (shown, hidden) = fit(d.split.len(), split_room, d.split_total);
    let mut dy = dy;
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
    dy
}

/// The ledger is last and is never a zero: absent means this repository has
/// no `recall.db`, which is a different fact from a ledger that avoided
/// nothing. The caveat rides in the text and not in a comment — the avoided
/// figure is an ESTIMATED UPPER BOUND, by the owner's standing rule, and no
/// claim rests on it, so the surface that shows it says so.
fn draw_recap_ledger(
    savings: Option<RecapSavings>,
    theme: Theme,
    inner: Rect,
    dy: u16,
    frame: &mut Frame,
) {
    let Some(s) = savings else {
        body(frame, inner, dy, "no recall ledger here", theme.dim());
        return;
    };
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::render::frame::Size;
    use crate::render::{AsciiBackend, Capabilities};

    use super::super::fixtures::panel_text;
    use super::super::{draw_one, SectionId};

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
}
