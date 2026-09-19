//! The two windows: before the first booked decline, and after.
//!
//! Every figure the report can carry is placed on the side of the cut it
//! happened on, so the window before declining began and the window after it
//! are compared as measured periods rather than blended into one total.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rift_core::Result;
use rift_store::{CanonicalStore, SqliteStore};

use super::classes::{classify_reads, read_facts, ClassTotals, ReadClass, ReadFact};
use super::journals::{empty_buckets, fold_split_journals, RetryAfterDeny, SliceBucket};
use super::render::{fmt_share, pad, share_of};

/// The verb whose FIRST row is the moment this repository stopped observing a
/// repeat and began declining one.
///
/// The flip itself is a build, not a row, so no report can read it. What a
/// report CAN prove is the first decision the audit relation holds, and that
/// is the cut published here — named, with its millisecond, so a reader who
/// would rather cut at the landing time can see exactly what would move.
const ENFORCE_BOUNDARY_VERB: &str = "memd.read_intercepted";

/// The serving step a decline is booked under. The three spellings here are
/// the hook path's own; a step this list does not name is neither a decline
/// nor a replacement to this report.
const DECLINE_STEP: &str = "read-gate";

/// The serving step a bounded replacement is booked under.
const REPLACEMENT_STEP: &str = "read-replacement";

/// The serving step a replacement that could not be delivered falls back to.
const FALLBACK_STEP: &str = "read-replacement-fallback";

/// The name a read whose session row the projection has not caught up with is
/// credited to, so it stays visible instead of joining a family it may not be.
const UNATTRIBUTED_FAMILY: &str = rift_recall_ledger::savings::UNATTRIBUTED_FAMILY;

/// One family's figures inside one of the two windows.
///
/// The sessions and reads are the capture projection's; the decisions are the
/// serving relation's. Neither relation carries the other's key, so the two
/// halves of a row are joined by the family NAME and nothing else — a row is a
/// name one of them used, and the block says so above the table.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct FamilyWindow {
    /// The family, as the relation that named it spells it.
    pub(super) family: String,
    /// Sessions the capture projection started inside the window.
    pub(super) sessions: u64,
    /// Distinct client-side ids behind them: fewer than `sessions` means the
    /// family folded several of its threads under one id.
    pub(super) native_ids: u64,
    /// Of those sessions, the ones that read a file.
    pub(super) sessions_with_reads: u64,
    /// Reads with no earlier read of the path in their session.
    pub(super) first_read: ClassTotals,
    /// Reads of a revision their session had already read.
    pub(super) reread_unchanged: ClassTotals,
    /// Reads of a path their session had read at another revision.
    pub(super) reread_changed: ClassTotals,
    /// Declines booked for this family.
    pub(super) declines: u64,
    /// Bounded replacements delivered to it.
    pub(super) replacements: u64,
    /// Replacements that could not be delivered and fell back.
    pub(super) fallbacks: u64,
    /// What its decisions avoided, in the serving relation's own figures.
    pub(super) tokens_avoided: u64,
    /// What they returned, in the same relation's own figures.
    pub(super) tokens_returned: u64,
}

impl FamilyWindow {
    /// An empty row for `family`.
    fn new(family: &str) -> Self {
        Self {
            family: family.to_owned(),
            sessions: 0,
            native_ids: 0,
            sessions_with_reads: 0,
            first_read: ClassTotals::default(),
            reread_unchanged: ClassTotals::default(),
            reread_changed: ClassTotals::default(),
            declines: 0,
            replacements: 0,
            fallbacks: 0,
            tokens_avoided: 0,
            tokens_returned: 0,
        }
    }
    /// Read calls over the three classes.
    pub(super) fn reads(&self) -> u64 {
        self.first_read
            .reads
            .saturating_add(self.reread_unchanged.reads)
            .saturating_add(self.reread_changed.reads)
    }

    /// Measured read tokens over the three classes.
    pub(super) fn read_tokens(&self) -> u64 {
        self.first_read
            .tokens
            .saturating_add(self.reread_unchanged.tokens)
            .saturating_add(self.reread_changed.tokens)
    }

    /// The owner's figure for this family: avoided ÷ (avoided + read), the
    /// avoided side from the serving relation and the read side from the
    /// capture projection. `None` when neither side holds a token.
    pub(super) fn savings_share(&self) -> Option<f64> {
        share_of(self.tokens_avoided, self.read_tokens())
    }
}

/// One of the two windows: every figure the relations and this member's
/// journals hold between its bounds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct WindowPane {
    /// The per-family rows.
    pub(super) families: Vec<FamilyWindow>,
    /// Σ avoided over the AUDIT relation inside the window — the relation the
    /// whole-report honesty figure is summed from, so the two agree.
    pub(super) tokens_avoided: u64,
    /// Σ returned over that relation inside the window.
    pub(super) tokens_returned: u64,
    /// Context windows of THIS member with a journal line inside it.
    pub(super) windows: u64,
    /// Journal lines inside it.
    pub(super) lines: u64,
    /// Read-shaped calls among them.
    pub(super) reads: u64,
    /// Repeats the gate would have declined and did not.
    pub(super) observed: u64,
    /// Repeats it declined.
    pub(super) declined: u64,
    /// Declines by frame, with the raw retries that followed them.
    pub(super) retries: Vec<RetryAfterDeny>,
    /// The DECLINED slices by length: under enforce the would-have-declined
    /// pool no longer exists, so this is the distribution the floor ruling is
    /// evaluated against in its place.
    pub(super) declined_buckets: Vec<SliceBucket>,
}

impl WindowPane {
    /// An empty window.
    pub(super) fn new() -> Self {
        Self {
            families: Vec::new(),
            tokens_avoided: 0,
            tokens_returned: 0,
            windows: 0,
            lines: 0,
            reads: 0,
            observed: 0,
            declined: 0,
            retries: Vec::new(),
            declined_buckets: empty_buckets(),
        }
    }
    /// Measured read tokens over every family in the window.
    pub(super) fn read_tokens(&self) -> u64 {
        self.families
            .iter()
            .fold(0u64, |acc, f| acc.saturating_add(f.read_tokens()))
    }

    /// The honesty gate for the window, over the audit relation alone.
    pub(super) fn net_tokens(&self) -> i64 {
        rift_recall_ledger::savings::honesty(self.tokens_avoided, self.tokens_returned)
    }

    /// The owner's figure for the window: avoided ÷ (avoided + read).
    pub(super) fn savings_share(&self) -> Option<f64> {
        share_of(self.tokens_avoided, self.read_tokens())
    }
}

/// The post-reset data cut in two at the first booked decline.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct EnforceSplit {
    /// The cut, in epoch milliseconds, or `None` when nothing was ever
    /// declined and the enforce window is empty by construction.
    pub(super) boundary_ms: Option<i64>,
    /// Where the cut came from, printed beside it.
    pub(super) boundary_source: String,
    /// Why the figures are absent, when they are.
    pub(super) note: Option<String>,
    /// Everything before the cut.
    pub(super) observe: WindowPane,
    /// Everything from the cut on.
    pub(super) enforce: WindowPane,
}

impl EnforceSplit {
    /// An empty split carrying the reason it is empty.
    fn unavailable(note: String) -> Self {
        Self {
            boundary_ms: None,
            boundary_source: "not resolved".to_owned(),
            note: Some(note),
            observe: WindowPane::new(),
            enforce: WindowPane::new(),
        }
    }
}

/// Every captured session as `(family, started_at, client-side id)`.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
async fn session_facts(store: &SqliteStore) -> Result<Vec<(String, i64, String)>> {
    let rows = store
        .query(
            "SELECT s.agent, s.started_at, COALESCE(i.native_session_id, '') \
             FROM session s \
             LEFT JOIN capture_session_identity i ON i.session_id = s.session_id \
             ORDER BY s.started_at ASC, s.session_id ASC",
            &[],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push((
            r.get_text(0).unwrap_or_default(),
            r.get_i64(1)?,
            r.get_text(2).unwrap_or_default(),
        ));
    }
    Ok(out)
}

/// Every serving decision as `(family, step, recorded_at, returned, avoided)`,
/// with the family split out of `tool_name` exactly as the per-family table
/// above splits it.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
async fn serve_facts(store: &SqliteStore) -> Result<Vec<(String, String, i64, u64, u64)>> {
    let rows = store
        .query(
            "SELECT tool_name, tokens_returned, tokens_avoided, recorded_at \
             FROM serve_call_event ORDER BY recorded_at ASC, call_id ASC",
            &[],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let tool = r.get_text(0)?;
        let (family, step) = tool.split_once(':').map_or_else(
            || (tool.clone(), String::new()),
            |(f, s)| (f.to_owned(), s.to_owned()),
        );
        out.push((
            family,
            step,
            r.get_i64(3)?,
            u64::try_from(r.get_i64(1)?).unwrap_or(0),
            u64::try_from(r.get_i64(2)?).unwrap_or(0),
        ));
    }
    Ok(out)
}

/// Every audit row as `(verb, recorded_at, returned, avoided)`.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
async fn audit_facts(store: &SqliteStore) -> Result<Vec<(String, i64, u64, u64)>> {
    let rows = store
        .query(
            "SELECT verb, tokens_returned, tokens_avoided, recorded_at \
             FROM memd_audit_event ORDER BY recorded_at ASC, audit_id ASC",
            &[],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push((
            r.get_text(0)?,
            r.get_i64(3)?,
            u64::try_from(r.get_i64(1)?).unwrap_or(0),
            u64::try_from(r.get_i64(2)?).unwrap_or(0),
        ));
    }
    Ok(out)
}

/// The cut: the first [`ENFORCE_BOUNDARY_VERB`] row, or nothing.
fn enforce_boundary(audit: &[(String, i64, u64, u64)]) -> Option<i64> {
    audit
        .iter()
        .filter(|(verb, ..)| verb == ENFORCE_BOUNDARY_VERB)
        .map(|&(_, at, _, _)| at)
        .min()
}

/// The family a read or session is credited to, with the empty string named
/// rather than folded into whichever family sorts first.
fn family_name(agent: &str) -> String {
    if agent.is_empty() {
        UNATTRIBUTED_FAMILY.to_owned()
    } else {
        agent.to_owned()
    }
}

/// Fold everything that falls on one side of the cut into one window.
///
/// A session is placed by when it STARTED and a read by when the call was
/// made, so a session that straddles the cut has each read counted on the side
/// it happened, never all of them on the side the session opened.
fn build_pane(
    sessions: &[(String, i64, String)],
    facts: &[ReadFact],
    classes: &[ReadClass],
    serve: &[(String, String, i64, u64, u64)],
    audit: &[(String, i64, u64, u64)],
    cut: i64,
    enforced: bool,
) -> WindowPane {
    let inside = |at: i64| (at >= cut) == enforced;
    let mut pane = WindowPane::new();
    let mut rows: BTreeMap<String, FamilyWindow> = BTreeMap::new();
    let mut natives: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut reading: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    fn row<'a>(rows: &'a mut BTreeMap<String, FamilyWindow>, family: &str) -> &'a mut FamilyWindow {
        rows.entry(family.to_owned())
            .or_insert_with(|| FamilyWindow::new(family))
    }
    for (agent, _, native) in sessions.iter().filter(|(_, at, _)| inside(*at)) {
        let family = family_name(agent);
        let entry = row(&mut rows, &family);
        entry.sessions = entry.sessions.saturating_add(1);
        if !native.is_empty() {
            natives.entry(family).or_default().insert(native.to_owned());
        }
    }
    for (f, class) in facts.iter().zip(classes.iter()) {
        if !inside(f.started_at) {
            continue;
        }
        let family = family_name(&f.agent);
        let entry = row(&mut rows, &family);
        let totals = match class {
            ReadClass::FirstRead => &mut entry.first_read,
            ReadClass::RereadUnchanged => &mut entry.reread_unchanged,
            ReadClass::RereadChanged => &mut entry.reread_changed,
        };
        totals.reads = totals.reads.saturating_add(1);
        totals.tokens = totals.tokens.saturating_add(f.input_tokens.unwrap_or(0));
        reading
            .entry(family)
            .or_default()
            .insert(f.session_id.clone());
    }
    for (family, step, _, returned, avoided) in serve.iter().filter(|(.., at, _, _)| inside(*at)) {
        let entry = row(&mut rows, family);
        entry.tokens_avoided = entry.tokens_avoided.saturating_add(*avoided);
        entry.tokens_returned = entry.tokens_returned.saturating_add(*returned);
        match step.as_str() {
            DECLINE_STEP => entry.declines = entry.declines.saturating_add(1),
            REPLACEMENT_STEP => entry.replacements = entry.replacements.saturating_add(1),
            FALLBACK_STEP => entry.fallbacks = entry.fallbacks.saturating_add(1),
            _ => {}
        }
    }
    for (family, ids) in natives {
        if let Some(entry) = rows.get_mut(&family) {
            entry.native_ids = ids.len() as u64;
        }
    }
    for (family, ids) in reading {
        if let Some(entry) = rows.get_mut(&family) {
            entry.sessions_with_reads = ids.len() as u64;
        }
    }
    for (_, _, returned, avoided) in audit.iter().filter(|(_, at, _, _)| inside(*at)) {
        pane.tokens_avoided = pane.tokens_avoided.saturating_add(*avoided);
        pane.tokens_returned = pane.tokens_returned.saturating_add(*returned);
    }
    pane.families = rows.into_values().collect();
    pane
}
/// The two windows for the family `repo` belongs to.
pub(super) async fn family_enforce_split(repo: &Path) -> EnforceSplit {
    let (Ok(projection), Ok(recall)) = (
        crate::family::projection_db(repo),
        crate::family::recall_db(repo),
    ) else {
        return EnforceSplit::unavailable("the family's stores did not resolve".to_owned());
    };
    // Read-only on both, for the reason every other block here is: a
    // measurement must never migrate the projection it is measuring.
    let (Ok(capture), Ok(serving)) = (
        SqliteStore::open_read_only(&projection),
        SqliteStore::open_read_only(&recall),
    ) else {
        return EnforceSplit::unavailable("a projection could not be opened read-only".to_owned());
    };
    let (Ok(sessions), Ok(facts)) = (session_facts(&capture).await, read_facts(&capture).await)
    else {
        return EnforceSplit::unavailable("the capture projection did not answer".to_owned());
    };
    let (Ok(serve), Ok(audit)) = (serve_facts(&serving).await, audit_facts(&serving).await) else {
        return EnforceSplit::unavailable("the recall projection did not answer".to_owned());
    };
    let classes = classify_reads(&facts);
    let boundary = enforce_boundary(&audit);
    let cut = boundary.unwrap_or(i64::MAX);
    let mut observe = build_pane(&sessions, &facts, &classes, &serve, &audit, cut, false);
    let mut enforce = build_pane(&sessions, &facts, &classes, &serve, &audit, cut, true);
    fold_split_journals(repo, boundary, &mut observe, &mut enforce);
    EnforceSplit {
        boundary_ms: boundary,
        boundary_source: match boundary {
            Some(_) => format!("first {ENFORCE_BOUNDARY_VERB} row"),
            None => format!("no {ENFORCE_BOUNDARY_VERB} row — the enforce window is empty"),
        },
        note: None,
        observe,
        enforce,
    }
}

/// Render one window's rows.
fn render_pane(label: &str, p: &WindowPane) -> String {
    let mut out = format!("  {label} window:\n");
    render_pane_families(p, &mut out);
    render_pane_totals(p, &mut out);
    render_pane_retries(p, &mut out);
    render_pane_declined(p, &mut out);
    out
}

/// One row per family the window names, with its own savings share.
fn render_pane_families(p: &WindowPane, out: &mut String) {
    out.push_str(&format!(
        "    {}{:>6}{:>6}{:>7}{:>7}{:>11}{:>6}{:>6}{:>5}{:>10}{:>10}{:>9}\n",
        pad("family", 20),
        "sess",
        "ids",
        "wread",
        "reads",
        "readtok",
        "decl",
        "repl",
        "fb",
        "avoided",
        "returned",
        "savings"
    ));
    if p.families.is_empty() {
        out.push_str("    (no session, read or decision in this window)\n");
    }
    for f in &p.families {
        let share = fmt_share(f.savings_share());
        out.push_str(&format!(
            "    {}{:>6}{:>6}{:>7}{:>7}{:>11}{:>6}{:>6}{:>5}{:>10}{:>10}{share:>9}\n",
            pad(&f.family, 20),
            f.sessions,
            f.native_ids,
            f.sessions_with_reads,
            f.reads(),
            f.read_tokens(),
            f.declines,
            f.replacements,
            f.fallbacks,
            f.tokens_avoided,
            f.tokens_returned
        ));
    }
}

/// The window's three summary lines: the honesty gate, the owner's share, and
/// what this member's journals hold inside the same bounds.
fn render_pane_totals(p: &WindowPane, out: &mut String) {
    out.push_str(&format!(
        "    honesty (audit relation): avoided {} − returned {} = {}\n",
        p.tokens_avoided,
        p.tokens_returned,
        p.net_tokens()
    ));
    out.push_str(&format!(
        "    savings: avoided ÷ (avoided + read) = {} ({} ÷ {})\n",
        fmt_share(p.savings_share()),
        p.tokens_avoided,
        p.tokens_avoided.saturating_add(p.read_tokens())
    ));
    out.push_str(&format!(
        "    journals: {} windows, {} lines, {} reads, {} observed, {} declined\n",
        p.windows, p.lines, p.reads, p.observed, p.declined
    ));
}

/// Declines by frame, with the raw retries that followed them.
fn render_pane_retries(p: &WindowPane, out: &mut String) {
    out.push_str(&format!(
        "    {}{:>8}{:>12}\n",
        pad("retry after decline", 22),
        "declines",
        "retried"
    ));
    if p.retries.is_empty() {
        out.push_str("      (no decline was recorded in this window)\n");
    }
    for r in &p.retries {
        let retried = if r.denies == 0 {
            0.0
        } else {
            (r.retried as f64) * 100.0 / (r.denies as f64)
        };
        out.push_str(&format!(
            "      {}{:>8}{:>12}  {retried:.1}%\n",
            pad(&r.frame, 20),
            r.denies,
            r.retried
        ));
    }
}

/// The declined slices by length — the distribution the line floor is
/// evaluated against once the would-have-declined pool no longer exists.
fn render_pane_declined(p: &WindowPane, out: &mut String) {
    out.push_str(&format!(
        "    {}{:>8}{:>12}{:>12}\n",
        pad("declined by length", 22),
        "reads",
        "tokens",
        "unmeasured"
    ));
    for b in &p.declined_buckets {
        out.push_str(&format!(
            "      {}{:>8}{:>12}{:>12}\n",
            pad(&b.label, 20),
            b.reads,
            b.tokens,
            b.unmeasured
        ));
    }
}

/// Render the split block (golden-locked).
pub(super) fn render_enforce_split(s: &EnforceSplit) -> String {
    let mut out = String::new();
    out.push_str("observe and enforce windows (the post-reset data, cut once):\n");
    if let Some(note) = &s.note {
        out.push_str(&format!("  not measured:          {note}\n"));
        return out;
    }
    out.push_str(&format!(
        "  boundary:              {} ms — {}\n",
        s.boundary_ms.unwrap_or_default(),
        s.boundary_source
    ));
    out.push_str(
        "  a row is a name one relation used: the capture projection's sessions and\n  \
         reads and the serving relation's decisions are joined by that name alone,\n  \
         so a family with decisions and no captured read has no read side and its\n  \
         savings read 100%.\n",
    );
    out.push_str(&render_pane("observe", &s.observe));
    out.push_str(&render_pane("enforce", &s.enforce));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One captured read, positioned in time and attributed to a family.
    fn read_fact(session: &str, agent: &str, at: i64, rev: &str, tokens: u64) -> ReadFact {
        ReadFact {
            session_id: session.to_owned(),
            path: "a.txt".to_owned(),
            started_at: at,
            tool_call_id: format!("{session}-{at}"),
            file_revision_id: rev.to_owned(),
            input_tokens: Some(tokens),
            agent: agent.to_owned(),
            tool_name: "Read".to_owned(),
        }
    }

    /// ★ The cut is the first booked decline, and everything is placed by when
    /// it happened rather than by when its session opened: a session that was
    /// already running when the gate began declining has each of its reads
    /// counted on the side of the boundary that read is on.
    #[test]
    fn the_two_windows_are_cut_at_the_first_booked_decline() {
        let audit = vec![
            ("memd.manifest_injected".to_owned(), 100, 40, 0),
            ("memd.read_intercepted".to_owned(), 500, 20, 900),
            ("memd.read_intercepted".to_owned(), 700, 20, 800),
        ];
        assert_eq!(
            enforce_boundary(&audit),
            Some(500),
            "the cut is the FIRST decline, not the last"
        );
        let sessions = vec![
            ("claude-code".to_owned(), 100, "n1".to_owned()),
            ("codex".to_owned(), 600, "n2".to_owned()),
        ];
        let facts = vec![
            read_fact("s1", "claude-code", 120, "rev-a", 300),
            read_fact("s1", "claude-code", 520, "rev-a", 300),
            read_fact("s2", "codex", 610, "rev-b", 100),
        ];
        let classes = classify_reads(&facts);
        let serve = vec![(
            "claude-code".to_owned(),
            "read-gate".to_owned(),
            520,
            20,
            900,
        )];
        let observe = build_pane(&sessions, &facts, &classes, &serve, &audit, 500, false);
        let enforce = build_pane(&sessions, &facts, &classes, &serve, &audit, 500, true);

        let before = observe.families.first().unwrap();
        assert_eq!(before.family, "claude-code");
        assert_eq!((before.sessions, before.native_ids), (1, 1));
        assert_eq!(before.sessions_with_reads, 1);
        assert_eq!(
            before.first_read.reads, 1,
            "the first read is before the cut"
        );
        assert_eq!(before.first_read.tokens, 300);
        assert_eq!(before.reread_unchanged.reads, 0);
        assert_eq!(before.declines, 0);
        assert_eq!((observe.tokens_avoided, observe.tokens_returned), (0, 40));

        let after: BTreeMap<&str, &FamilyWindow> = enforce
            .families
            .iter()
            .map(|f| (f.family.as_str(), f))
            .collect();
        let claude = after.get("claude-code").unwrap();
        assert_eq!(
            claude.sessions, 0,
            "the session opened before the cut and is counted there once"
        );
        assert_eq!(claude.sessions_with_reads, 1, "its later read is here");
        assert_eq!(claude.reread_unchanged.reads, 1);
        assert_eq!(claude.reread_unchanged.tokens, 300);
        assert_eq!((claude.declines, claude.tokens_avoided), (1, 900));
        let codex = after.get("codex").unwrap();
        assert_eq!((codex.sessions, codex.first_read.reads), (1, 1));
        assert_eq!(
            (enforce.tokens_avoided, enforce.tokens_returned),
            (1700, 40),
            "both decline rows, and only the returned tokens booked with them"
        );
    }

    /// ★ Sessions and the ids their client used are two counts, not one: a
    /// family that folds several threads under one id shows fewer ids than
    /// sessions, and the report must not print either as the other.
    #[test]
    fn threads_folded_under_one_client_id_are_two_sessions_and_one_id() {
        let sessions = vec![
            ("codex".to_owned(), 10, "thread-1".to_owned()),
            ("codex".to_owned(), 20, "thread-1".to_owned()),
            ("codex".to_owned(), 30, String::new()),
        ];
        let pane = build_pane(&sessions, &[], &[], &[], &[], i64::MAX, false);
        let row = pane.families.first().unwrap();
        assert_eq!(row.sessions, 3);
        assert_eq!(
            row.native_ids, 1,
            "one id behind two of them, none for the third"
        );
    }

    /// ★ The owner's figure, in both places it is printed: what was avoided
    /// over what was avoided plus what was still read. A denominator of
    /// nothing is unanswerable, not zero — and a family with decisions but no
    /// captured read has no read side at all, which the block says out loud.
    #[test]
    fn the_split_savings_share_is_avoided_over_avoided_plus_read() {
        let mut row = FamilyWindow::new("claude-code");
        row.tokens_avoided = 900;
        row.first_read = ClassTotals {
            reads: 1,
            tokens: 100,
        };
        assert_eq!(row.savings_share(), Some(90.0));
        assert_eq!(FamilyWindow::new("codex").savings_share(), None);
        let mut only_decisions = FamilyWindow::new("codex");
        only_decisions.tokens_avoided = 7;
        assert_eq!(only_decisions.savings_share(), Some(100.0));

        let mut pane = WindowPane::new();
        assert_eq!(pane.savings_share(), None);
        pane.families = vec![row];
        pane.tokens_avoided = 900;
        pane.tokens_returned = 100;
        assert_eq!(pane.savings_share(), Some(90.0));
        assert_eq!(pane.net_tokens(), 800);
    }

    /// ★ The split block's shape is a locked artifact: a report whose columns
    /// move between windows is a report nobody can diff.
    #[test]
    fn the_window_split_shape_is_locked() {
        let mut family = FamilyWindow::new("claude-code");
        family.sessions = 4;
        family.native_ids = 4;
        family.sessions_with_reads = 2;
        family.first_read = ClassTotals {
            reads: 3,
            tokens: 300,
        };
        family.reread_unchanged = ClassTotals {
            reads: 1,
            tokens: 600,
        };
        family.declines = 2;
        family.replacements = 1;
        family.fallbacks = 1;
        family.tokens_avoided = 900;
        family.tokens_returned = 180;
        let mut enforce = WindowPane::new();
        enforce.families = vec![family];
        enforce.tokens_avoided = 900;
        enforce.tokens_returned = 180;
        enforce.windows = 2;
        enforce.lines = 9;
        enforce.reads = 5;
        enforce.observed = 1;
        enforce.declined = 2;
        enforce.retries = vec![RetryAfterDeny {
            frame: "plain".to_owned(),
            denies: 2,
            retried: 1,
        }];
        if let Some(slot) = enforce.declined_buckets.get_mut(1) {
            slot.reads = 2;
            slot.tokens = 700;
        }
        let split = EnforceSplit {
            boundary_ms: Some(1_700_000_000_000),
            boundary_source: "first memd.read_intercepted row".to_owned(),
            note: None,
            observe: WindowPane::new(),
            enforce,
        };
        let golden = "\
observe and enforce windows (the post-reset data, cut once):
  boundary:              1700000000000 ms — first memd.read_intercepted row
  a row is a name one relation used: the capture projection's sessions and
  reads and the serving relation's decisions are joined by that name alone,
  so a family with decisions and no captured read has no read side and its
  savings read 100%.
  observe window:
    family                sess   ids  wread  reads    readtok  decl  repl   fb   avoided  returned  savings
    (no session, read or decision in this window)
    honesty (audit relation): avoided 0 − returned 0 = 0
    savings: avoided ÷ (avoided + read) = n/a (0 ÷ 0)
    journals: 0 windows, 0 lines, 0 reads, 0 observed, 0 declined
    retry after decline   declines     retried
      (no decline was recorded in this window)
    declined by length       reads      tokens  unmeasured
      < 24 lines                 0           0           0
      24–99 lines                0           0           0
      100–999 lines              0           0           0
      ≥ 1000 lines               0           0           0
  enforce window:
    family                sess   ids  wread  reads    readtok  decl  repl   fb   avoided  returned  savings
    claude-code              4     4      2      4        900     2     1    1       900       180    50.0%
    honesty (audit relation): avoided 900 − returned 180 = 720
    savings: avoided ÷ (avoided + read) = 50.0% (900 ÷ 1800)
    journals: 2 windows, 9 lines, 5 reads, 1 observed, 2 declined
    retry after decline   declines     retried
      plain                      2           1  50.0%
    declined by length       reads      tokens  unmeasured
      < 24 lines                 0           0           0
      24–99 lines                2         700           0
      100–999 lines              0           0           0
      ≥ 1000 lines               0           0           0
";
        assert_eq!(render_enforce_split(&split), golden);
    }
}
