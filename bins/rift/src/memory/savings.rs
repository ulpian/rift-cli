//! `rift savings` — the owner's evaluation figure, on its own verb so it can be
//! read without running (and appending to) the whole reread report.
//!
//! Every number here is composed by `rift_recall_ledger::savings`, which the
//! report, the recall status table and the dashboard call as well: four
//! surfaces, one computation.

use std::path::Path;

use rift_core::{Result, RiftError};
use rift_recall_ledger::audit::{pull_claim_totals, PullClaimTotals};
use rift_recall_ledger::savings::Savings;
use rift_recall_ledger::savings_window::{self, Boundary, Marker};
use rift_store::SqliteStore;

use crate::cli::{Outcome, SavingsArgs, SavingsCmd};

use super::classes::is_shell_tool;
use super::journals::{member_window_journals, Compaction};
use super::panes::{family_enforce_split, WindowPane};
use super::render::{fmt_share, pad};

/// The savings figure plus the window it was measured over.
pub(crate) struct SavingsReport {
    /// The capture projection the read side came from.
    pub(crate) projection_db: String,
    /// The recall projection the avoided side came from.
    pub(crate) recall_db: String,
    /// Why the figures are absent, when they are.
    pub(crate) note: Option<String>,
    /// The composed figure.
    pub(crate) savings: Savings,
    /// **The window the figures above cover**, and where its boundary came
    /// from: the family's marker, a one-call `--since`, or the whole record.
    ///
    /// This is what `boundary_ms` and `boundary_source` publish. Before a
    /// boundary could be moved they carried the observe/enforce cut, which is
    /// a different question — where declining began, not where counting
    /// begins — and now travels under its own name below.
    pub(crate) window: Boundary,
    /// Where the observe window ends and the enforce window begins, over the
    /// whole record.
    pub(crate) enforce_cut_ms: Option<i64>,
    /// How that cut was resolved, printed beside it.
    pub(crate) enforce_cut_source: String,
    /// Sessions the capture projection started before the cut.
    pub(crate) observe_sessions: u64,
    /// Sessions it started from the cut on.
    pub(crate) enforce_sessions: u64,
}

impl SavingsReport {
    /// An empty report carrying the reason it is empty.
    fn unavailable(projection_db: String, recall_db: String, note: String) -> Self {
        Self {
            projection_db,
            recall_db,
            note: Some(note),
            savings: Savings::default(),
            window: Boundary::SinceReset,
            enforce_cut_ms: None,
            enforce_cut_source: "not resolved".to_owned(),
            observe_sessions: 0,
            enforce_sessions: 0,
        }
    }
}

/// Sessions the pane's rows account for.
fn pane_sessions(p: &WindowPane) -> u64 {
    p.families
        .iter()
        .fold(0u64, |acc, f| acc.saturating_add(f.sessions))
}

/// Which window one call asks for, which [`family_savings`] resolves against
/// the family's marker.
///
/// It is three cases rather than an `Option<i64>` because the third composes
/// with the marker instead of replacing it, and a bare instant cannot say
/// which of the two it is. The clock is read by the caller, never here: a
/// measurement that sampled the wall clock could not be tested twice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum WindowRequest {
    /// The family's stored boundary marker, or the whole record without one —
    /// what every surface has always meant by "since the reset".
    #[default]
    Family,
    /// `--since <instant>`: a stated instant, answering exactly itself.
    Since(i64),
    /// `--last <window>`: a trailing period, composed with the marker.
    Trailing {
        /// Where the trailing period starts, in epoch milliseconds.
        cut_ms: i64,
        /// How it was spelled (`24h`, `7d`, `30d`) — the dashboard's own.
        spelling: &'static str,
    },
}

/// Compose the savings figure for the family `repo` belongs to.
///
/// Read-only on both stores, for the reason every measurement here is: a
/// figure that migrated the projection it measured would be measuring itself.
async fn family_savings(repo: &Path, request: WindowRequest) -> SavingsReport {
    let (Ok(family), Ok(projection), Ok(recall)) = (
        crate::family::resolve_at(repo),
        crate::family::projection_db(repo),
        crate::family::recall_db(repo),
    ) else {
        return SavingsReport::unavailable(
            "(unresolved)".to_owned(),
            "(unresolved)".to_owned(),
            "the family's stores did not resolve".to_owned(),
        );
    };
    // The boundary is the FAMILY's, read from the common dir every member
    // shares — so a subagent's temporary worktree measures the period the
    // person set, not one of its own.
    let window = match request {
        WindowRequest::Family => savings_window::resolve(&family.common_dir, None),
        WindowRequest::Since(ms) => savings_window::resolve(&family.common_dir, Some(ms)),
        WindowRequest::Trailing { cut_ms, spelling } => {
            savings_window::resolve_trailing(&family.common_dir, Some(cut_ms), spelling)
        }
    };
    let (shown_p, shown_r) = (
        projection.display().to_string(),
        recall.display().to_string(),
    );
    if !projection.exists() || !recall.exists() {
        return SavingsReport::unavailable(
            shown_p,
            shown_r,
            "nothing has been captured or served for this family yet".to_owned(),
        );
    }
    let (Ok(capture), Ok(serving)) = (
        SqliteStore::open_read_only(&projection),
        SqliteStore::open_read_only(&recall),
    ) else {
        return SavingsReport::unavailable(
            shown_p,
            shown_r,
            "a projection could not be opened read-only".to_owned(),
        );
    };
    // The channel split is made from the families' own pinned tool tables,
    // which the ledger crate is a leaf and cannot read — so the classifier is
    // handed in here, where those tables are already linked. A caller with no
    // classifier gets absence rather than a zero, which is the rung-4 rule.
    let shell: &(dyn Fn(&str, &str) -> bool + Sync) = &is_shell_tool;
    let Ok(savings) =
        rift_recall_ledger::savings::compose_with(&serving, &capture, window.cut_ms(), Some(shell))
            .await
    else {
        return SavingsReport::unavailable(
            shown_p,
            shown_r,
            "a projection did not answer".to_owned(),
        );
    };
    // The observe/enforce cut, from the same split the report publishes: one
    // cut, so the verb and the report cannot disagree about which sessions
    // were observed and which were enforced. It is reported over the whole
    // record whatever the window is — where declining began is a fact about
    // this repository, not about the period somebody chose to measure.
    let split = family_enforce_split(repo).await;
    SavingsReport {
        projection_db: shown_p,
        recall_db: shown_r,
        note: None,
        savings,
        window,
        enforce_cut_ms: split.boundary_ms,
        enforce_cut_source: split.boundary_source,
        observe_sessions: pane_sessions(&split.observe),
        enforce_sessions: pane_sessions(&split.enforce),
    }
}

/// Render the savings figure (golden-locked).
fn render_savings(r: &SavingsReport) -> String {
    let s = &r.savings;
    let mut out = String::new();
    out.push_str("savings — avoided ÷ (avoided + read), over the window named below\n");
    out.push_str(&format!("  capture projection:    {}\n", r.projection_db));
    out.push_str(&format!("  recall projection:     {}\n", r.recall_db));
    if let Some(note) = &r.note {
        out.push_str(&format!("  not measured:          {note}\n"));
        return out;
    }
    render_window_line(r, &mut out);
    render_headline(s, &mut out);
    render_family_rows(s, &mut out);
    render_family_shares(s, &mut out);
    out
}

/// The window the figures cover, the observe/enforce cut, and the sessions
/// either side of it.
///
/// The session counts come from that cut, which spans the whole record; a
/// window narrower than the record says so beside them rather than let a reader
/// pair "12 enforce sessions" with a figure that covers two.
fn render_window_line(r: &SavingsReport, out: &mut String) {
    out.push_str(&format!("  window:                {}\n", r.window.label()));
    out.push_str(&format!(
        "  observe/enforce cut:   {} ms — {}\n",
        r.enforce_cut_ms.unwrap_or_default(),
        r.enforce_cut_source
    ));
    out.push_str(&format!(
        "  sessions:              observe {} / enforce {}{}\n",
        r.observe_sessions,
        r.enforce_sessions,
        if r.window.is_cut() {
            "  (the whole record, not the window)"
        } else {
            ""
        }
    ));
}

/// The figure itself, its denominator broken down by channel, and the two
/// caveats every reader of it needs.
fn render_headline(s: &Savings, out: &mut String) {
    out.push_str(&format!(
        "  overall:               {} ({} ÷ {})  [audit relation]\n",
        fmt_share(s.share()),
        s.tokens_avoided,
        s.tokens_avoided.saturating_add(s.read_tokens)
    ));
    out.push_str(&format!(
        "  of it prevented:       {} (reads that never happened, inside the figure above)\n",
        s.tokens_prevented
    ));
    // The denominator's own breakdown. It is repository content on any
    // channel, so it says which — and an absent figure prints as `n/a`, never
    // as a zero that would read like a measurement.
    let chan = |n: Option<u64>| n.map_or_else(|| "n/a".to_owned(), |n| n.to_string());
    out.push_str(&format!(
        "  read tokens by channel: read tool {} / shell read {} / search {}\n",
        chan(s.channels.read_tool),
        chan(s.channels.shell_read),
        s.channels.search
    ));
    out.push_str(&format!(
        "  honesty:               avoided {} − returned {} = {}\n",
        s.tokens_avoided,
        s.tokens_returned,
        s.honesty()
    ));
    out.push_str(&format!(
        "  n (sessions with reads): {}\n",
        s.sessions_with_reads
    ));
    // The hole in the denominator, named rather than filled. A read whose
    // token count was never frozen is a row in the denominator's set worth
    // zero tokens, so the share above is over fewer measured reads than its
    // `n` suggests — and the two sides are cut on two clocks, which is
    // visible only near a boundary but is a fact about every figure here.
    out.push_str(&format!(
        "  reads not measured:    {} (in the denominator, worth 0 tokens)\n",
        s.reads_unmeasured
    ));
    out.push_str(
        "  the numerator is cut on the decision's clock (`recorded_at`) and the\n  \
         denominator on the read's (`tool_call.started_at`).\n",
    );
}

/// The same figure summed over the relation that names a family, and the same
/// again with the families that decided without a captured read left out —
/// named, so the exclusion is auditable.
fn render_family_shares(s: &Savings, out: &mut String) {
    out.push_str(&format!(
        "  by family:             {}  [channel relation]\n",
        fmt_share(s.families_share())
    ));
    let excluded = s.excluded();
    let named = if excluded.is_empty() {
        "none".to_owned()
    } else {
        excluded.join(", ")
    };
    out.push_str(&format!(
        "  excluding families that decided with no captured reads: {}  (excluded: {named})\n",
        fmt_share(s.share_excluding_undenominated())
    ));
}

/// The per-family table of [`render_savings`]: one row per family the
/// channel relation names, joined to the projection by name.
fn render_family_rows(s: &Savings, out: &mut String) {
    out.push_str("per family (the relation that names one, joined to the projection by name):\n");
    out.push_str(&format!(
        "  {}{:>9}{:>12}{:>6}{:>11}{:>11}\n",
        pad("family", 22),
        "share",
        "honesty",
        "n",
        "decisions",
        "prevented"
    ));
    if s.families.is_empty() {
        out.push_str("  (no family has a decision or a read yet)\n");
    }
    for f in &s.families {
        let flag = if f.decided_without_reads() {
            "  decided, no captured reads"
        } else {
            ""
        };
        out.push_str(&format!(
            "  {}{:>9}{:>12}{:>6}{:>11}{:>11}{flag}\n",
            pad(&f.family, 22),
            fmt_share(f.share()),
            f.honesty(),
            f.sessions_with_reads,
            f.decisions,
            f.tokens_prevented
        ));
    }
}

/// The savings figure as a JSON document, for the `--json` arm and for any
/// caller that would otherwise re-derive it.
pub(crate) fn savings_json(r: &SavingsReport) -> serde_json::Value {
    let s = &r.savings;
    serde_json::json!({
        "definition": "avoided / (avoided + read), over the window in `window`",
        "projection_db": r.projection_db,
        "recall_db": r.recall_db,
        "note": r.note,
        // `boundary_ms` / `boundary_source` name THE WINDOW THE FIGURES COVER:
        // null and "the projection reset …" when nobody has set a boundary,
        // which is what every figure published before a marker existed
        // covered. The observe/enforce cut they used to carry keeps its
        // numbers under a name that says which question it answers.
        "window": {
            "boundary_ms": r.window.cut_ms(),
            "boundary_source": r.window.label(),
            "marker_set_at_ms": marker_field(&r.window, |m| m.set_at_ms),
            "marker_set_by": match &r.window {
                Boundary::Marker(m) => Some(m.set_by.clone()),
                Boundary::SinceReset | Boundary::Since(_) | Boundary::Trailing { .. } => None,
            },
            "enforce_cut_ms": r.enforce_cut_ms,
            "enforce_cut_source": r.enforce_cut_source,
            "observe_sessions": r.observe_sessions,
            "enforce_sessions": r.enforce_sessions,
        },
        "overall": {
            "share_pct": s.share(),
            "tokens_avoided": s.tokens_avoided,
            // The prevented part of `tokens_avoided`, never a second figure
            // beside it: a reader that added the two would double a saving.
            "tokens_prevented": s.tokens_prevented,
            "tokens_returned": s.tokens_returned,
            // The denominator: repository content on ANY channel, which is
            // file reads plus measured search results. `channels` is its
            // breakdown and sums to it exactly, so a reader can take a channel
            // out rather than having to trust the total.
            "read_tokens": s.read_tokens,
            "channels": {
                "read_tool": s.channels.read_tool,
                "shell_read": s.channels.shell_read,
                "search": s.channels.search,
            },
            // A COUNT OF CALLS, not of tokens: calls that returned repository
            // content and that nobody measured. Published beside the
            // denominator so a reader can see how much of the pool is missing
            // from it rather than assume none is.
            "reads_unmeasured": s.reads_unmeasured,
            "honesty": s.honesty(),
            "sessions_with_reads": s.sessions_with_reads,
            // Reads the projection never froze a token count for. They are
            // rows of the denominator worth zero, so the share is over fewer
            // measured reads than `sessions_with_reads` suggests. Published,
            // never imputed — and never subtracted from the denominator
            // here, which is a different lever.
            "reads_unmeasured": s.reads_unmeasured,
            "relation": "memd_audit_event",
            // The two sides are placed by two clocks, each the instant its
            // own relation records.
            "numerator_clock": "memd_audit_event.recorded_at",
            "denominator_clock": "tool_call.started_at",
        },
        "by_family": s.families,
        "families_share_pct": s.families_share(),
        "share_excluding_undenominated_pct": s.share_excluding_undenominated(),
        "excluded": s.excluded(),
    })
}

/// The savings document with this member's compaction account added under
/// one new key.
///
/// Added by the verb rather than by [`savings_json`], which the status table
/// shares: the table never reads the journals, and every key the document
/// already had stays byte-identical beside the new one.
fn with_compaction(mut json: serde_json::Value, compaction: &Compaction) -> serde_json::Value {
    let Some(map) = json.as_object_mut() else {
        return json;
    };
    map.insert(
        "compaction".to_owned(),
        serde_json::to_value(compaction).unwrap_or_default(),
    );
    json
}

/// The marker's own metadata, when the window came from one.
fn marker_field<T>(window: &Boundary, field: impl Fn(&Marker) -> T) -> Option<T> {
    match window {
        Boundary::Marker(m) => Some(field(m)),
        Boundary::SinceReset | Boundary::Since(_) | Boundary::Trailing { .. } => None,
    }
}

/// `rift savings [--since <rfc3339|ms>] [--json]`, and `rift savings reset`.
///
/// Repo-scoped like every other measurement verb: the target is the worktree
/// root discovered upward from `root`, so a subdirectory invocation answers
/// for the repository rather than reporting a false-empty cwd.
///
/// # Errors
/// An unparsable `--since`, a `reset` that would place the boundary in the
/// future, or a family whose common directory cannot be written.
pub(crate) fn savings(root: &Path, args: &SavingsArgs) -> Result<Outcome> {
    if let Some(SavingsCmd::Reset(reset)) = &args.cmd {
        if args.since.is_some() || args.last.is_some() {
            return Err(RiftError::Validity(
                "`--since` and `--last` read a window for one call; `reset` moves the \
                 stored boundary. Use `rift savings reset --at <rfc3339|ms>` to place it"
                    .to_owned(),
            ));
        }
        return savings_reset(root, reset.at.as_deref());
    }
    let request = window_request(args, crate::hooks::now_ms())?;
    let report = savings_report_for(root, request)?;
    // The journals are this member's own run state, read through the same
    // entry point the reread report reads them by.
    let windows = member_window_journals(&crate::worktree_root(root));
    let mut human = render_savings(&report);
    let mut json = with_compaction(savings_json(&report), &windows.compaction);
    // Beside the figure, never inside it: the claims explain how much of the
    // pull credit was claimed, and change none of it.
    if report.note.is_none() {
        let claims = family_pull_claims(&report);
        human.push_str(&render_pull_claims(claims.as_ref()));
        json = with_pull_claims(json, claims.as_ref());
    }
    Ok(Outcome::ok(human.trim_end().to_owned(), json))
}

/// The pull claims booked over the report's window, read from the recall
/// projection the report names, or `None` where that projection cannot say:
/// it predates the relation, or it would not open.
fn family_pull_claims(report: &SavingsReport) -> Option<PullClaimTotals> {
    let store = SqliteStore::open_read_only(Path::new(&report.recall_db)).ok()?;
    let rt = tokio::runtime::Builder::new_current_thread().build().ok()?;
    rt.block_on(pull_claim_totals(&store, report.window.cut_ms()))
        .ok()
}

/// The one line that sets what pulls claimed beside what this family read,
/// so a claim no recorded read accounts for is visible rather than silent.
pub(super) fn render_pull_claims(totals: Option<&PullClaimTotals>) -> String {
    let Some(t) = totals else {
        return "  pull claims:           not recorded by this projection\n".to_owned();
    };
    format!(
        "  pull claims:           {} stated, {} read by this family, {} stated but never read\n",
        t.stated, t.recorded, t.unrecorded
    )
}

/// A document with the pull claims added under one new key, `null` where the
/// projection cannot say, so every key it already had stays as it was.
pub(super) fn with_pull_claims(
    mut json: serde_json::Value,
    totals: Option<&PullClaimTotals>,
) -> serde_json::Value {
    let Some(map) = json.as_object_mut() else {
        return json;
    };
    map.insert(
        "pull_claims".to_owned(),
        serde_json::to_value(totals).unwrap_or_default(),
    );
    json
}

/// Which window `rift savings` was asked for, against one reading of the
/// clock.
///
/// `--last` is the dashboard's own vocabulary, taken from
/// [`rift_canopy::RecallWindow`] rather than re-spelled here, so `7d` on the
/// card and `7d` in the terminal cannot come to mean two different periods.
/// The clock is passed in: this function is where the spelling is validated
/// and the period is placed, and both have to be testable without a wall
/// clock.
///
/// # Errors
/// [`RiftError::Validity`] for a window spelling that is not one of the
/// dashboard's four, for an unparsable `--since`, or for a clock that cannot
/// place a trailing window at all (before the epoch) — which is a refusal
/// rather than a silent fallback to the whole record under a narrow name.
fn window_request(args: &SavingsArgs, now_ms: i64) -> Result<WindowRequest> {
    if let Some(text) = args.last.as_deref() {
        let window = rift_canopy::RecallWindow::ALL
            .into_iter()
            .find(|w| w.label() == text)
            .ok_or_else(|| {
                RiftError::Validity(format!(
                    "`--last {text}` is not a window: write 24h, 7d, 30d, or all"
                ))
            })?;
        return match window.cut(now_ms) {
            rift_canopy::WindowCut::Marker => Ok(WindowRequest::Family),
            rift_canopy::WindowCut::Since(cut_ms) => Ok(WindowRequest::Trailing {
                cut_ms,
                spelling: window.label(),
            }),
            rift_canopy::WindowCut::NoClock => Err(RiftError::Validity(
                "this machine's clock cannot place a trailing window; use \
                 `--since <rfc3339|ms>` or omit the flag"
                    .to_owned(),
            )),
        };
    }
    Ok(parse_since(args.since.as_deref())?.map_or(WindowRequest::Family, WindowRequest::Since))
}

/// An instant a person typed, refused rather than guessed at.
fn parse_since(text: Option<&str>) -> Result<Option<i64>> {
    let Some(text) = text else { return Ok(None) };
    savings_window::parse_instant(text)
        .map(Some)
        .ok_or_else(|| {
            RiftError::Validity(format!(
                "`--since {text}` is not an instant: write UTC RFC3339 \
             (2026-09-07T18:00:00Z) or epoch milliseconds"
            ))
        })
}

/// `rift savings reset [--at <rfc3339|ms>]`: move this family's savings window
/// forward, so the next evaluation period starts here.
///
/// The marker is one file in the family's common directory. Nothing is
/// deleted, nothing is rewritten and no relation is touched: the record still
/// holds every row it held a moment ago, and a reader who wants the whole of
/// it asks for `--since 0`.
fn savings_reset(root: &Path, at: Option<&str>) -> Result<Outcome> {
    let repo = crate::worktree_root(root);
    let family = crate::family::resolve_at(&repo)?;
    let now = crate::hooks::now_ms();
    let boundary = match at {
        Some(text) => savings_window::parse_instant(text).ok_or_else(|| {
            RiftError::Validity(format!(
                "`--at {text}` is not an instant: write UTC RFC3339 \
                 (2026-09-07T18:00:00Z) or epoch milliseconds"
            ))
        })?,
        None => now,
    };
    let (marker, previous) = savings_window::write_marker(&family.common_dir, boundary, now)?;
    let path = savings_window::marker_path(&family.common_dir);
    Ok(Outcome::ok(
        render_reset(&path, &marker, previous).trim_end().to_owned(),
        serde_json::json!({
            "marker_path": path.display().to_string(),
            "boundary_ms": marker.boundary_ms,
            "boundary": marker.boundary,
            "set_at_ms": marker.set_at_ms,
            "set_at": marker.set_at,
            "set_by": marker.set_by,
            "previous_boundary_ms": previous,
        }),
    ))
}

/// Render what a reset did (golden-locked).
///
/// It names the boundary it replaced because that is the only record of it:
/// the marker holds one boundary, and a person who reset twice by accident
/// would otherwise have no way to see that they had.
fn render_reset(path: &Path, marker: &Marker, previous: Option<i64>) -> String {
    let mut out = String::new();
    out.push_str("savings window moved\n");
    out.push_str(&format!("  marker:                {}\n", path.display()));
    out.push_str(&format!(
        "  boundary:              {} ({} ms)\n",
        marker.boundary, marker.boundary_ms
    ));
    match previous {
        Some(ms) => out.push_str(&format!(
            "  previous:              {} ({ms} ms)\n",
            savings_window::format_rfc3339_ms(ms)
        )),
        None => out.push_str("  previous:              none — this is the family's first window\n"),
    }
    out.push_str(&format!("  set by:                {}\n", marker.set_by));
    out.push_str(
        "  from here:             `rift savings` counts only what is recorded from the\n  \
         boundary on. Nothing was deleted and no relation was touched: `rift savings\n  \
         --since 0` still answers over the whole record.\n",
    );
    out
}

/// The savings figure for the repository `root` is in, over the family's own
/// window, blocking.
///
/// The one entry point every non-async surface reaches for it — the verb above
/// and `rift recall status` — so the two cannot resolve different stores or
/// cut the window differently.
///
/// # Errors
/// [`RiftError`] only when a runtime cannot be built; an unresolvable or
/// unreadable family is a note on the report, not a failure.
pub(crate) fn savings_report(root: &Path) -> Result<SavingsReport> {
    savings_report_for(root, WindowRequest::Family)
}

/// [`savings_report`] over a window this call asked for.
///
/// # Errors
/// As [`savings_report`].
fn savings_report_for(root: &Path, request: WindowRequest) -> Result<SavingsReport> {
    let repo = crate::worktree_root(root);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)?;
    Ok(rt.block_on(family_savings(&repo, request)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `--last` is the dashboard's period vocabulary and nothing of its own.
    ///
    /// The four spellings come from `RecallWindow::ALL`, so `7d` here and the
    /// `[7d]` button on the card are the same period by construction rather
    /// than by two tables agreeing. The clock is injected: nothing here reads
    /// the wall.
    #[test]
    fn the_last_flag_takes_the_dashboards_own_windows() {
        // 2026-07-20T00:00:00Z — a clock that can place every window.
        const NOW: i64 = 1_784_505_600_000;
        const DAY: i64 = 86_400_000;
        let ask = |last: Option<&str>, since: Option<&str>| {
            window_request(
                &SavingsArgs {
                    cmd: None,
                    since: since.map(str::to_owned),
                    last: last.map(str::to_owned),
                },
                NOW,
            )
        };
        assert_eq!(
            ask(Some("24h"), None).expect("a window"),
            WindowRequest::Trailing {
                cut_ms: NOW - DAY,
                spelling: "24h"
            }
        );
        assert_eq!(
            ask(Some("7d"), None).expect("a window"),
            WindowRequest::Trailing {
                cut_ms: NOW - 7 * DAY,
                spelling: "7d"
            }
        );
        assert_eq!(
            ask(Some("30d"), None).expect("a window"),
            WindowRequest::Trailing {
                cut_ms: NOW - 30 * DAY,
                spelling: "30d"
            }
        );
        // `all` is the family's own window — the default, spelled.
        assert_eq!(
            ask(Some("all"), None).expect("a window"),
            WindowRequest::Family
        );
        // No flag at all is that same default; `--since` is unchanged.
        assert_eq!(ask(None, None).expect("a window"), WindowRequest::Family);
        assert_eq!(
            ask(None, Some("1700000000000")).expect("an instant"),
            WindowRequest::Since(1_700_000_000_000)
        );
        // A spelling that is not one of the four is refused, naming them.
        let err = ask(Some("1w"), None).expect_err("not a window").to_string();
        assert!(err.contains("24h, 7d, 30d, or all"), "{err}");
        // A clock that cannot place a trailing window is refused rather than
        // silently answered with the whole record under a narrow name.
        let dead = window_request(
            &SavingsArgs {
                cmd: None,
                since: None,
                last: Some("24h".to_owned()),
            },
            0,
        )
        .expect_err("a dead clock places nothing")
        .to_string();
        assert!(dead.contains("cannot place a trailing window"), "{dead}");
    }

    /// The fixture the verb's shape is locked against: every figure the report
    /// can carry, with a value, over the whole record.
    fn locked_report() -> SavingsReport {
        use rift_recall_ledger::savings::FamilyShare;
        SavingsReport {
            projection_db: "/fam/projection.db".to_owned(),
            recall_db: "/fam/recall.db".to_owned(),
            note: None,
            savings: Savings {
                tokens_avoided: 1_000,
                tokens_prevented: 120,
                tokens_returned: 160,
                read_tokens: 9_000,
                // The denominator's breakdown, which sums to it exactly: a
                // reader who takes a channel out is left with the rest.
                channels: rift_recall_ledger::savings::ChannelTokens {
                    read_tool: Some(6_000),
                    shell_read: Some(2_000),
                    search: 1_000,
                },
                reads_unmeasured: 2,
                sessions_with_reads: 3,
                families: vec![
                    FamilyShare {
                        family: "claude-code".to_owned(),
                        tokens_avoided: 500,
                        tokens_prevented: 120,
                        tokens_returned: 160,
                        decisions: 4,
                        read_tokens: 9_000,
                        channels: rift_recall_ledger::savings::ChannelTokens {
                            read_tool: Some(6_000),
                            shell_read: Some(2_000),
                            search: 1_000,
                        },
                        reads_unmeasured: 2,
                        sessions_with_reads: 3,
                    },
                    FamilyShare {
                        family: "codex".to_owned(),
                        tokens_avoided: 500,
                        tokens_prevented: 0,
                        tokens_returned: 0,
                        decisions: 3,
                        read_tokens: 0,
                        channels: rift_recall_ledger::savings::ChannelTokens::default(),
                        reads_unmeasured: 0,
                        sessions_with_reads: 0,
                    },
                ],
            },
            window: Boundary::SinceReset,
            enforce_cut_ms: Some(1_700_000_000_000),
            enforce_cut_source: "first memd.read_intercepted row".to_owned(),
            observe_sessions: 17,
            enforce_sessions: 12,
        }
    }

    /// The same figures over a boundary marker.
    fn locked_report_with_marker() -> SavingsReport {
        SavingsReport {
            window: Boundary::Marker(Box::new(Marker {
                schema_version: 1,
                boundary_ms: 1_700_000_000_000,
                boundary: "2023-11-14T22:13:20.000Z".to_owned(),
                set_at_ms: 1_700_000_000_500,
                set_at: "2023-11-14T22:13:20.500Z".to_owned(),
                set_by: rift_recall_ledger::savings_window::RESET_VERB.to_owned(),
            })),
            ..locked_report()
        }
    }

    /// The verb's shape, locked: an owner reading this number has to be able
    /// to see which relation each half came from and how many sessions are
    /// behind it, or the percentage is a claim rather than a measurement.
    #[test]
    fn the_savings_shape_is_locked() {
        let golden = "\
savings — avoided ÷ (avoided + read), over the window named below
  capture projection:    /fam/projection.db
  recall projection:     /fam/recall.db
  window:                since the projection reset — every recorded row counts (no boundary marker)
  observe/enforce cut:   1700000000000 ms — first memd.read_intercepted row
  sessions:              observe 17 / enforce 12
  overall:               10.0% (1000 ÷ 10000)  [audit relation]
  of it prevented:       120 (reads that never happened, inside the figure above)
  read tokens by channel: read tool 6000 / shell read 2000 / search 1000
  honesty:               avoided 1000 − returned 160 = 840
  n (sessions with reads): 3
  reads not measured:    2 (in the denominator, worth 0 tokens)
  the numerator is cut on the decision's clock (`recorded_at`) and the
  denominator on the read's (`tool_call.started_at`).
per family (the relation that names one, joined to the projection by name):
  family                    share     honesty     n  decisions  prevented
  claude-code                5.3%         340     3          4        120
  codex                    100.0%         500     0          3          0  decided, no captured reads
  by family:             10.0%  [channel relation]
  excluding families that decided with no captured reads: 5.3%  (excluded: codex)
";
        assert_eq!(render_savings(&locked_report()), golden);
    }

    /// The window line names the marker, and the session counts — which come
    /// from the whole record's observe/enforce cut — say so rather than being
    /// read as the window's.
    #[test]
    fn a_boundary_marker_names_itself_beside_the_sessions_it_does_not_cover() {
        let rendered = render_savings(&locked_report_with_marker());
        assert!(
            rendered.contains(
                "  window:                since the boundary marker at \
                 2023-11-14T22:13:20.000Z, set 2023-11-14T22:13:20.500Z by \
                 `rift savings reset`\n"
            ),
            "{rendered}"
        );
        assert!(
            rendered.contains(
                "  sessions:              observe 17 / enforce 12  \
                 (the whole record, not the window)\n"
            ),
            "{rendered}"
        );
    }

    /// The JSON says the same thing as the rendered block, in the fields a
    /// script reads — and the observe/enforce cut keeps its numbers under its
    /// own name.
    #[test]
    fn the_json_window_carries_the_marker_and_the_cut_under_separate_names() {
        let json = savings_json(&locked_report_with_marker());
        assert_eq!(json["window"]["boundary_ms"], 1_700_000_000_000_i64);
        assert_eq!(json["window"]["marker_set_at_ms"], 1_700_000_000_500_i64);
        assert_eq!(json["window"]["marker_set_by"], "rift savings reset");
        assert_eq!(json["window"]["enforce_cut_ms"], 1_700_000_000_000_i64);
        assert_eq!(
            json["window"]["enforce_cut_source"],
            "first memd.read_intercepted row"
        );
    }

    /// ★ The compaction account is one additive key: with it the document is
    /// the document without it plus that key, so no existing reader moves.
    #[test]
    fn the_compaction_account_is_one_additive_key() {
        let without = savings_json(&locked_report());
        assert!(
            without.get("compaction").is_none(),
            "the shared document has none"
        );
        let mut compaction = Compaction {
            evictions: 3,
            sessions: 2,
            compactions_per_session: Some(1.5),
            ..Default::default()
        };
        compaction.storms.count = 3;
        compaction.storms.median_tokens = 40;
        let with = with_compaction(without.clone(), &compaction);
        let c = &with["compaction"];
        assert_eq!(
            (c["evictions"].as_u64(), c["sessions"].as_u64()),
            (Some(3), Some(2))
        );
        assert_eq!(c["compactions_per_session"].as_f64(), Some(1.5));
        assert_eq!(c["storms"]["median_tokens"], 40);
        let mut rest = with.clone();
        rest.as_object_mut().unwrap().remove("compaction");
        assert_eq!(rest, without, "every other key is unchanged");
    }

    /// The denominator's own breakdown, in the keys a script reads: it sums to
    /// `read_tokens` exactly, so a reader may take a channel out rather than
    /// having to trust the total. `reads_unmeasured` is beside it and is a
    /// count of CALLS, which is why it is not in the sum.
    #[test]
    fn the_json_channel_breakdown_sums_to_the_denominator_it_breaks_down() {
        let json = savings_json(&locked_report_with_marker());
        assert_eq!(json["overall"]["read_tokens"], 9_000);
        assert_eq!(json["overall"]["channels"]["read_tool"], 6_000);
        assert_eq!(json["overall"]["channels"]["shell_read"], 2_000);
        assert_eq!(json["overall"]["channels"]["search"], 1_000);
        assert_eq!(json["overall"]["reads_unmeasured"], 2);
        let channels = &json["overall"]["channels"];
        let sum: u64 = ["read_tool", "shell_read", "search"]
            .iter()
            .map(|k| channels[*k].as_u64().unwrap_or_default())
            .sum();
        assert_eq!(
            sum,
            json["overall"]["read_tokens"].as_u64().unwrap_or_default(),
            "the breakdown sums to the denominator it breaks down"
        );
    }

    /// ★ A split nobody could make is **absent**, never a zero: a zero in
    /// `read_tool` would read as a measurement that no read arrived through a
    /// read tool, and the honesty ladder's bottom rung says a surface with no
    /// basis shows nothing at all.
    #[test]
    fn an_unsplit_denominator_publishes_absence_and_not_a_zero() {
        let unsplit = rift_recall_ledger::savings::assemble(
            0,
            0,
            0,
            &std::collections::BTreeMap::new(),
            &std::collections::BTreeMap::from([(
                "claude-code".to_owned(),
                rift_recall_ledger::savings::FamilyReads {
                    tokens: 900,
                    sessions: 1,
                    unmeasured: 0,
                },
            )]),
            &std::collections::BTreeMap::new(),
            None,
        );
        assert_eq!(unsplit.read_tokens, 900);
        assert_eq!(unsplit.channels.read_tool, None);
        assert_eq!(unsplit.channels.shell_read, None);
        let report = SavingsReport {
            projection_db: "p".to_owned(),
            recall_db: "r".to_owned(),
            note: None,
            savings: unsplit,
            window: Boundary::SinceReset,
            enforce_cut_ms: None,
            enforce_cut_source: "none".to_owned(),
            observe_sessions: 0,
            enforce_sessions: 0,
        };
        assert!(
            render_savings(&report)
                .contains("  read tokens by channel: read tool n/a / shell read n/a / search 0\n"),
            "{}",
            render_savings(&report)
        );
        assert!(savings_json(&report)["overall"]["channels"]["read_tool"].is_null());
    }

    /// What a reset says it did, locked: the boundary it set, the one it
    /// replaced, and — in the same breath — that it destroyed nothing. A verb
    /// whose name is "reset" has to say what it did NOT do, or a person will
    /// reasonably assume it emptied something.
    #[test]
    fn the_reset_shape_is_locked() {
        let marker = Marker {
            schema_version: 1,
            boundary_ms: 1_700_000_000_000,
            boundary: "2023-11-14T22:13:20.000Z".to_owned(),
            set_at_ms: 1_700_000_000_000,
            set_at: "2023-11-14T22:13:20.000Z".to_owned(),
            set_by: rift_recall_ledger::savings_window::RESET_VERB.to_owned(),
        };
        let path = Path::new("/fam/.git/rift-family/savings-window.json");
        let first = render_reset(path, &marker, None);
        assert_eq!(
            first,
            "\
savings window moved
  marker:                /fam/.git/rift-family/savings-window.json
  boundary:              2023-11-14T22:13:20.000Z (1700000000000 ms)
  previous:              none — this is the family's first window
  set by:                rift savings reset
  from here:             `rift savings` counts only what is recorded from the
  boundary on. Nothing was deleted and no relation was touched: `rift savings
  --since 0` still answers over the whole record.
"
        );
        // The boundary it replaced is named, because the marker holds one
        // boundary and nothing else records the one before it.
        let again = render_reset(path, &marker, Some(1_600_000_000_000));
        assert!(
            again.contains("  previous:              2020-09-13T12:26:40.000Z (1600000000000 ms)"),
            "{again}"
        );
    }

    /// The verb answers with the figure the shared function computed — no
    /// second arithmetic anywhere on the way to the screen or to `--json`.
    #[test]
    fn the_verb_publishes_the_shared_functions_figure() {
        let report = SavingsReport {
            projection_db: "p".to_owned(),
            recall_db: "r".to_owned(),
            note: None,
            savings: Savings {
                tokens_avoided: 900,
                tokens_prevented: 300,
                tokens_returned: 100,
                read_tokens: 100,
                channels: rift_recall_ledger::savings::ChannelTokens::default(),
                reads_unmeasured: 0,
                sessions_with_reads: 2,
                families: Vec::new(),
            },
            window: Boundary::SinceReset,
            enforce_cut_ms: None,
            enforce_cut_source: "none".to_owned(),
            observe_sessions: 0,
            enforce_sessions: 0,
        };
        let shared = rift_recall_ledger::savings::share(900, 100);
        assert_eq!(report.savings.share(), shared);
        assert!(render_savings(&report).contains("overall:               90.0%"));
        let json = savings_json(&report);
        assert_eq!(json["overall"]["share_pct"], serde_json::json!(90.0));
        assert_eq!(json["overall"]["honesty"], serde_json::json!(800));
        assert_eq!(json["overall"]["relation"], "memd_audit_event");
        // Inside the headline, never beside it: the two published figures add
        // up to the one the share is computed from, and no more.
        assert_eq!(json["overall"]["tokens_prevented"], serde_json::json!(300));
        assert!(render_savings(&report).contains("of it prevented:       300"));
    }

    /// **One spelling of the verb a prevented read is booked under.** The
    /// savings query names it as a literal, the accounting kind names it as a
    /// verb, and this is the only place both are in scope — so it is the only
    /// place they can be held to each other.
    #[test]
    fn the_prevented_verb_the_query_names_is_the_verb_the_event_writes() {
        let event = rift_recall_serve::MemdEvent::ReadPrevented {
            handle: String::new(),
            reason: rift_recall_serve::TAG_PREVENTED.to_owned(),
            estimated_tokens_avoided: 1,
        };
        assert_eq!(
            rift_recall_ledger::savings::PREVENTED_VERB,
            format!("memd.{}", event.verb())
        );
    }

    /// An unresolvable family is a NOTE, not a failure: the owner asked for a
    /// number and gets the reason there is none, on exit 0.
    #[test]
    fn an_unmeasurable_family_says_so_instead_of_printing_zero() {
        let tmp = tempfile::tempdir().unwrap();
        let out = savings(tmp.path(), &SavingsArgs::default()).unwrap();
        assert_eq!(out.code, crate::cli::exit::OK);
        assert!(out.human.contains("not measured:"), "{}", out.human);
        assert!(!out.human.contains("0.0%"), "{}", out.human);
        assert!(out.json["overall"]["share_pct"].is_null());
    }
}

#[cfg(test)]
mod pull_claim_tests {
    use super::*;

    #[test]
    fn the_pull_claim_line_sets_the_stated_count_beside_the_read_one() {
        let totals = PullClaimTotals {
            stated: 7,
            recorded: 4,
            unrecorded: 3,
        };
        assert_eq!(
            render_pull_claims(Some(&totals)),
            "  pull claims:           7 stated, 4 read by this family, 3 stated but never read\n"
        );
        assert_eq!(
            render_pull_claims(None),
            "  pull claims:           not recorded by this projection\n"
        );
    }

    #[test]
    fn the_pull_claims_are_one_additive_key() {
        let before = serde_json::json!({ "overall": { "share_pct": 1.0 } });
        let totals = PullClaimTotals {
            stated: 2,
            recorded: 2,
            unrecorded: 0,
        };
        let after = with_pull_claims(before.clone(), Some(&totals));
        assert_eq!(after["overall"], before["overall"]);
        assert_eq!(
            after["pull_claims"],
            serde_json::json!({ "stated": 2, "recorded": 2, "unrecorded": 0 })
        );
        assert_eq!(
            with_pull_claims(before, None)["pull_claims"],
            serde_json::Value::Null
        );
    }
}
