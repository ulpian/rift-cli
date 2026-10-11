//! The levers, as the two audit relations record them.
//!
//! The relations key on different ids and are never joined: the audit relation
//! carries the honesty gate, the channel relation carries the family. A figure
//! summed from one is never published under the other's name.

use std::collections::BTreeMap;
use std::path::Path;

use rift_core::Result;
use rift_recall_ledger::savings::{in_the_share, ClaimedByPulls};
use rift_store::{CanonicalStore, SqliteStore};

use super::render::{pad, share_of};

/// How long after an injection a pull is still credited to it.
///
/// Neither serving channel has a turn to record — a hook injection is one hook
/// event and a skill call is one process — so `serve_call_event` carries a
/// wall-clock millisecond and nothing about the conversation around it. Ten
/// minutes is the span this report calls "within a turn of": long enough to
/// cover a turn that stops to run a build, short enough that a pull in the
/// next sitting is not credited to yesterday's injection. The span is printed
/// with the figure so a reader who disagrees can re-derive it.
const PULL_AFTER_INJECTION_MS: i64 = 600_000;

/// One decision kind, summed out of the audit relation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct LeverKind {
    /// The `memd.*` verb the rows carry.
    pub(super) verb: String,
    /// How many rows there are.
    pub(super) rows: u64,
    /// What those decisions' frames and injections cost.
    pub(super) tokens_returned: u64,
    /// What those decisions avoided.
    pub(super) tokens_avoided: u64,
}

/// One family's decisions, out of the relation that names a family.
///
/// `serve_call_event.tool_name` is `<family>:<hook step>`, which is the only
/// per-family axis either relation has. Its figures are ITS OWN: the two
/// relations key on different ids and are never joined, so a family's share of
/// a figure summed from the other relation is not available and is not
/// guessed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct FamilyLevers {
    /// The family, as the hook wrote it.
    pub(super) family: String,
    /// The hook step that decided (`read-gate`, `recovery-map`, …).
    pub(super) step: String,
    /// Rows for this `(family, step)`.
    pub(super) rows: u64,
    /// What they returned, in this relation's own figures.
    pub(super) tokens_returned: u64,
    /// What they avoided, in this relation's own figures.
    pub(super) tokens_avoided: u64,
}

/// Pulls that followed an injection, per family.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct PullsAfterInjection {
    /// The family whose injection the pulls followed.
    pub(super) family: String,
    /// Injections this family was handed.
    pub(super) injections: u64,
    /// Skill-CLI calls inside [`PULL_AFTER_INJECTION_MS`] of one of them.
    pub(super) pulls: u64,
}

/// What [`channel_rows`] reads out of the channel relation in one pass.
struct ChannelRows {
    /// The per-family split.
    families: Vec<FamilyLevers>,
    /// Pull-after-injection, per family.
    pulls: Vec<PullsAfterInjection>,
    /// Skill-CLI calls with no injection inside the span before them.
    pulls_without_injection: u64,
    /// What the rows outside the share booked.
    claimed: ClaimedByPulls,
}

/// What the audit relations say about every lever, for this family.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(super) struct Levers {
    /// The recall projection the rows came from.
    pub(super) recall_db: String,
    /// Why the numbers are absent, when they are.
    pub(super) note: Option<String>,
    /// Every decision kind that has a row, by verb.
    pub(super) kinds: Vec<LeverKind>,
    /// Σ avoided over the audit relation — the honesty gate's credit side.
    pub(super) tokens_avoided: u64,
    /// Σ returned over the same relation — its spend side.
    pub(super) tokens_returned: u64,
    /// What the pulls booked, from the channel relation: inside the two
    /// totals above, and taken out of them before the share is divided.
    pub(super) claimed_by_pulls: ClaimedByPulls,
    /// The per-family split, in the other relation's own figures.
    pub(super) families: Vec<FamilyLevers>,
    /// Pull-after-injection, per family.
    pub(super) pulls: Vec<PullsAfterInjection>,
    /// Skill-CLI calls with no injection inside the span before them.
    pub(super) pulls_without_injection: u64,
    /// Distinct `(path, revision)` pairs a read recorded.
    pub(super) held_revisions: u64,
    /// Of those, the ones the blob map can reach as a git blob.
    pub(super) held_with_blob: u64,
}

impl Levers {
    /// An empty result carrying the reason it is empty.
    fn unavailable(recall_db: String, note: String) -> Self {
        Self {
            recall_db,
            note: Some(note),
            kinds: Vec::new(),
            tokens_avoided: 0,
            tokens_returned: 0,
            claimed_by_pulls: ClaimedByPulls::default(),
            families: Vec::new(),
            pulls: Vec::new(),
            pulls_without_injection: 0,
            held_revisions: 0,
            held_with_blob: 0,
        }
    }

    /// The honesty gate as one number: what was avoided, net of what the
    /// reasons and injections that avoided it cost. Signed, because an
    /// observe-mode window that injects and declines nothing is legitimately
    /// negative and a report that could not say so would be useless.
    pub(super) fn net_tokens(&self) -> i64 {
        rift_recall_ledger::savings::honesty(self.tokens_avoided, self.tokens_returned)
    }

    /// The owner's figure: avoided ÷ (avoided + read), over everything since
    /// the projection was reset, with the pulls' claim taken out of the
    /// numerator by the ledger's own rule. `None` when neither side has a
    /// token in it — a share of nothing is not zero, it is unanswerable.
    pub(super) fn savings_share(&self, read_tokens: u64) -> Option<f64> {
        share_of(self.avoided_in_share(), read_tokens)
    }

    /// The audit relation's avoided total less what the pulls claimed.
    fn avoided_in_share(&self) -> u64 {
        let claimed = &self.claimed_by_pulls;
        claimed
            .in_share_of(self.tokens_avoided, self.tokens_returned)
            .0
    }
}

/// Every decision kind with a row, summed by verb.
async fn audit_kinds(store: &SqliteStore) -> Result<Vec<LeverKind>> {
    let rows = store
        .query(
            "SELECT verb, COUNT(*), COALESCE(SUM(tokens_returned), 0), \
             COALESCE(SUM(tokens_avoided), 0) \
             FROM memd_audit_event GROUP BY verb ORDER BY verb ASC",
            &[],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(LeverKind {
            verb: r.get_text(0)?,
            rows: u64::try_from(r.get_i64(1)?).unwrap_or(0),
            tokens_returned: u64::try_from(r.get_i64(2)?).unwrap_or(0),
            tokens_avoided: u64::try_from(r.get_i64(3)?).unwrap_or(0),
        });
    }
    Ok(out)
}

/// The channel-attributed rows: the per-family split, and which pulls
/// followed an injection.
///
/// The split counts EVERY row of the relation. The channel decides only what
/// a row is to the pull figures beneath it — an injection to be pulled after,
/// or a pull — never whether the family it names is credited with it.
async fn channel_rows(store: &SqliteStore) -> Result<ChannelRows> {
    let rows = store
        .query(
            "SELECT channel, tool_name, tokens_returned, tokens_avoided, recorded_at \
             FROM serve_call_event ORDER BY recorded_at ASC, call_id ASC",
            &[],
        )
        .await?;
    let mut families: BTreeMap<(String, String), FamilyLevers> = BTreeMap::new();
    let mut pulls: BTreeMap<String, PullsAfterInjection> = BTreeMap::new();
    let mut unattributed = 0u64;
    let mut claimed = ClaimedByPulls::default();
    // The most recent injection, whoever it was for: a pull is credited to the
    // family whose injection it followed, and to no one when the nearest
    // injection is further back than the span.
    let mut last_injection: Option<(String, i64)> = None;
    for r in &rows {
        let channel = r.get_text(0)?;
        let tool = r.get_text(1)?;
        let returned = u64::try_from(r.get_i64(2)?).unwrap_or(0);
        let avoided = u64::try_from(r.get_i64(3)?).unwrap_or(0);
        let at = r.get_i64(4)?;
        let (family, step) = tool.split_once(':').map_or_else(
            || (tool.clone(), String::new()),
            |(f, s)| (f.to_owned(), s.to_owned()),
        );
        // EVERY row, on either channel. The heading says "per family", and a
        // table built from the injection channel alone left a family's own
        // skill-CLI calls out of the figures it is read for — invisible under
        // any family however many of them there were.
        let entry = families
            .entry((family.clone(), step.clone()))
            .or_insert_with(|| FamilyLevers {
                family: family.clone(),
                step: step.clone(),
                rows: 0,
                tokens_returned: 0,
                tokens_avoided: 0,
            });
        entry.rows = entry.rows.saturating_add(1);
        entry.tokens_returned = entry.tokens_returned.saturating_add(returned);
        entry.tokens_avoided = entry.tokens_avoided.saturating_add(avoided);
        if !in_the_share(&channel) {
            claimed.book(1, avoided, returned);
        }
        if channel == "hook_inject" {
            // Only a step that pushes something is an injection to pull after;
            // a decline is an answer to a call the agent already made.
            if step == "recovery-map" || step == "boundary" {
                let seen = pull_row(&mut pulls, &family);
                seen.injections = seen.injections.saturating_add(1);
                last_injection = Some((family, at));
            }
            continue;
        }
        match last_injection.as_ref() {
            Some((injected_for, when)) if at.saturating_sub(*when) <= PULL_AFTER_INJECTION_MS => {
                let seen = pull_row(&mut pulls, injected_for);
                seen.pulls = seen.pulls.saturating_add(1);
            }
            _ => unattributed = unattributed.saturating_add(1),
        }
    }
    Ok(ChannelRows {
        families: families.into_values().collect(),
        pulls: pulls.into_values().collect(),
        pulls_without_injection: unattributed,
        claimed,
    })
}

/// The pull row for `family`, opened empty when the family has none yet.
fn pull_row<'a>(
    pulls: &'a mut BTreeMap<String, PullsAfterInjection>,
    family: &str,
) -> &'a mut PullsAfterInjection {
    pulls
        .entry(family.to_owned())
        .or_insert_with(|| PullsAfterInjection {
            family: family.to_owned(),
            injections: 0,
            pulls: 0,
        })
}

/// How many recorded read revisions the blob map can reach as a git blob.
///
/// The same pair the bridge counts (`(path, content_sha256)` of a recorded
/// read), and the same numerator (a row in the blob map), read rather than
/// re-derived: this report writes nothing.
async fn basis_availability(store: &SqliteStore) -> Result<(u64, u64)> {
    let held = store
        .query(
            "SELECT COUNT(*) FROM (SELECT DISTINCT r.path, r.content_sha256 \
             FROM n_file_revision r JOIN n_file_read k ON k.file_revision = r.node_id)",
            &[],
        )
        .await?;
    let mapped = store
        .query(
            "SELECT COUNT(*) FROM (SELECT DISTINCT r.path, r.content_sha256 \
             FROM n_file_revision r JOIN n_file_read k ON k.file_revision = r.node_id \
             JOIN n_blob_oid_map m ON m.content_sha256 = r.content_sha256)",
            &[],
        )
        .await?;
    let count = |rows: &[rift_store::Row]| -> u64 {
        rows.first()
            .and_then(|r| r.get_i64(0).ok())
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0)
    };
    Ok((count(&held), count(&mapped)))
}

/// Every lever figure the audit relations hold, from one open store.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub(super) async fn levers(store: &SqliteStore, recall_db: String) -> Result<Levers> {
    let kinds = audit_kinds(store).await?;
    let channel = channel_rows(store).await?;
    let (held_revisions, held_with_blob) = basis_availability(store).await?;
    let tokens_avoided = kinds.iter().map(|k| k.tokens_avoided).sum();
    let tokens_returned = kinds.iter().map(|k| k.tokens_returned).sum();
    Ok(Levers {
        recall_db,
        note: None,
        kinds,
        tokens_avoided,
        tokens_returned,
        claimed_by_pulls: channel.claimed,
        families: channel.families,
        pulls: channel.pulls,
        pulls_without_injection: channel.pulls_without_injection,
        held_revisions,
        held_with_blob,
    })
}

/// The lever figures for the family `repo` belongs to.
///
/// Opened READ-ONLY: a measurement must not migrate the projection it is
/// measuring, and a report is never worth a write to the store the owner's
/// live sessions are appending to. A projection this process cannot open is a
/// note, not a failure, for the same reason the repeat classes' is.
pub(super) async fn family_levers(repo: &Path) -> Levers {
    let db = match crate::family::recall_db(repo) {
        Ok(db) => db,
        Err(e) => return Levers::unavailable("(unresolved)".to_owned(), e.to_string()),
    };
    let shown = db.display().to_string();
    if !db.exists() {
        return Levers::unavailable(
            shown,
            "no recall projection yet — nothing has been served for this family".to_owned(),
        );
    }
    let store = match SqliteStore::open_read_only(&db) {
        Ok(store) => store,
        Err(e) => return Levers::unavailable(shown, e.to_string()),
    };
    match levers(&store, shown.clone()).await {
        Ok(l) => l,
        Err(e) => Levers::unavailable(shown, e.to_string()),
    }
}

/// Render the lever block (golden-locked).
pub(super) fn render_levers(l: &Levers, read_tokens: u64) -> String {
    let mut out = String::new();
    out.push_str("levers (audit relations, everything recorded since the reset):\n");
    out.push_str(&format!("  recall projection:     {}\n", l.recall_db));
    if let Some(note) = &l.note {
        out.push_str(&format!("  not measured:          {note}\n"));
        return out;
    }
    render_kinds(l, read_tokens, &mut out);
    render_family_split(l, &mut out);
    render_pull_after_injection(l, &mut out);
    render_basis_availability(l, &mut out);
    out
}

/// The audit relation's own rows, and the two figures summed from them: the
/// honesty gate and the owner's share.
fn render_kinds(l: &Levers, read_tokens: u64, out: &mut String) {
    out.push_str(&format!(
        "  {}{:>8}{:>12}{:>12}\n",
        pad("verb", 24),
        "rows",
        "returned",
        "avoided"
    ));
    if l.kinds.is_empty() {
        out.push_str("  (none)\n");
    }
    for k in &l.kinds {
        out.push_str(&format!(
            "  {}{:>8}{:>12}{:>12}\n",
            pad(&k.verb, 24),
            k.rows,
            k.tokens_returned,
            k.tokens_avoided
        ));
    }
    out.push_str(&format!(
        "  honesty (this relation): avoided {} − returned {} = {}\n",
        l.tokens_avoided,
        l.tokens_returned,
        l.net_tokens()
    ));
    let avoided = l.avoided_in_share();
    let savings = l.savings_share(read_tokens).map_or_else(
        || "  savings: unanswerable — nothing was read and nothing was avoided\n".to_owned(),
        |share| {
            format!(
                "  savings: avoided ÷ (avoided + read) = {share:.1}% ({} ÷ {})\n",
                avoided,
                avoided.saturating_add(read_tokens)
            )
        },
    );
    out.push_str(&savings);
    out.push_str(&render_claimed(&l.claimed_by_pulls, "  "));
}

/// The pulls' claim, under a share and outside it, when there was a pull.
pub(super) fn render_claimed(claimed: &ClaimedByPulls, indent: &str) -> String {
    if claimed.decisions == 0 {
        return String::new();
    }
    format!(
        "{indent}claimed by pulls: {} avoided on {} pulls, {} returned (beside the share, not in it)\n",
        claimed.tokens_avoided, claimed.decisions, claimed.tokens_returned
    )
}

/// The channel relation's per-family rows, under a heading that says whose
/// figures they are.
fn render_family_split(l: &Levers, out: &mut String) {
    out.push_str("per family (the other relation, its own figures, never joined):\n");
    if l.families.is_empty() {
        out.push_str("  (none)\n");
    }
    for f in &l.families {
        out.push_str(&format!(
            "  {}{:>8}{:>12}{:>12}\n",
            pad(&format!("{}:{}", f.family, f.step), 24),
            f.rows,
            f.tokens_returned,
            f.tokens_avoided
        ));
    }
}

/// Injections and the pulls that followed them, with the span printed beside
/// the figure so a reader who disagrees with it can re-derive the number.
fn render_pull_after_injection(l: &Levers, out: &mut String) {
    out.push_str(&format!(
        "pull after injection (a skill call within {PULL_AFTER_INJECTION_MS} ms of one):\n"
    ));
    if l.pulls.is_empty() {
        out.push_str("  (none)\n");
    }
    for p in &l.pulls {
        out.push_str(&format!(
            "  {}{:>8} injected{:>8} pulled\n",
            pad(&p.family, 24),
            p.injections,
            p.pulls
        ));
    }
    out.push_str(&format!(
        "  pulls with no injection before them: {}\n",
        l.pulls_without_injection
    ));
}

/// How much of what was read can still be reached as a git blob.
fn render_basis_availability(l: &Levers, out: &mut String) {
    let share = if l.held_revisions == 0 {
        0.0
    } else {
        (l.held_with_blob as f64) * 100.0 / (l.held_revisions as f64)
    };
    out.push_str("basis availability (recorded read revisions):\n");
    out.push_str(&format!("  held revisions:        {}\n", l.held_revisions));
    out.push_str(&format!(
        "  reachable as a blob:   {} ({share:.1}%)\n",
        l.held_with_blob
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::Value;

    /// A recall projection with the two audit relations migrated and empty.
    fn audit_store() -> SqliteStore {
        let store = SqliteStore::open_in_memory().expect("in-memory store");
        rift_store::apply_migrations(&store, &rift_store::recall_migrations())
            .expect("recall migrations");
        store
    }

    /// Append one audit row under an id of its own.
    async fn audit_row(store: &SqliteStore, id: &str, verb: &str, returned: i64, avoided: i64) {
        store
            .insert(
                "memd_audit_event",
                &[
                    "audit_id",
                    "verb",
                    "handle",
                    "reason",
                    "tokens_returned",
                    "tokens_avoided",
                    "recorded_at",
                    "schema_version",
                ],
                &[
                    Value::text(id),
                    Value::text(verb),
                    Value::Null,
                    Value::text("already_held"),
                    Value::Int(returned),
                    Value::Int(avoided),
                    Value::Int(1),
                    Value::Int(1),
                ],
            )
            .await
            .expect("audit row");
    }

    /// Append one channel-attributed row.
    async fn call_row(store: &SqliteStore, id: &str, channel: &str, tool: &str, at: i64) {
        store
            .insert(
                "serve_call_event",
                &[
                    "call_id",
                    "channel",
                    "tool_name",
                    "terminal_verb",
                    "tokens_returned",
                    "tokens_avoided",
                    "recorded_at",
                    "schema_version",
                ],
                &[
                    Value::text(id),
                    Value::text(channel),
                    Value::text(tool),
                    Value::text("memd.read_intercepted"),
                    Value::Int(10),
                    Value::Int(100),
                    Value::Int(at),
                    Value::Int(1),
                ],
            )
            .await
            .expect("call row");
    }

    /// ★ Every figure is summed out of exactly one relation. The audit
    /// relation carries the honesty gate; the channel relation carries the
    /// family. The two key on different ids and are never joined, so a
    /// family's share of the honesty figures is absent rather than invented.
    #[tokio::test]
    async fn each_lever_figure_is_summed_from_one_relation() {
        let store = audit_store();
        audit_row(&store, "a1", "memd.read_intercepted", 40, 900).await;
        audit_row(&store, "a2", "memd.read_intercepted", 40, 600).await;
        audit_row(&store, "a3", "memd.read_repeat_observed", 0, 0).await;
        audit_row(&store, "a4", "memd.recovery_injected", 108, 0).await;
        call_row(&store, "c1", "hook_inject", "claude-code:read-gate", 1_000).await;

        let l = levers(&store, "db".to_owned()).await.expect("levers");
        assert_eq!(l.kinds.len(), 3, "one row per verb, verbs sorted");
        assert_eq!(l.tokens_avoided, 1_500);
        assert_eq!(l.tokens_returned, 188);
        assert_eq!(l.net_tokens(), 1_312);
        assert_eq!(l.families.len(), 1);
        let family = l.families.first().expect("one family");
        assert_eq!(
            (family.family.as_str(), family.step.as_str()),
            ("claude-code", "read-gate")
        );
        assert_eq!(
            (family.rows, family.tokens_avoided),
            (1, 100),
            "the family's figures are the channel relation's own, not the audit relation's"
        );
    }

    /// ★ **The per-family table counts every channel's rows**, not the
    /// injection channel's alone.
    ///
    /// The heading reads "per family", and a table built from `hook_inject`
    /// rows only left a family's own skill-CLI calls out of the one figure
    /// they are read for: however many there were, they appeared under no
    /// family at all. The pull figures below still key on the channel, because
    /// there the channel is the whole question.
    #[tokio::test]
    async fn the_per_family_table_counts_a_skill_call_as_well_as_an_injection() {
        let store = audit_store();
        call_row(&store, "c1", "hook_inject", "claude-code:read-gate", 1_000).await;
        call_row(
            &store,
            "c2",
            "skill_cli",
            "claude-code:changed-hunks",
            2_000,
        )
        .await;

        let l = levers(&store, "db".to_owned()).await.expect("levers");
        let named: Vec<(&str, &str, u64)> = l
            .families
            .iter()
            .map(|f| (f.family.as_str(), f.step.as_str(), f.rows))
            .collect();
        assert_eq!(
            named,
            vec![
                ("claude-code", "changed-hunks", 1),
                ("claude-code", "read-gate", 1),
            ],
            "the skill call is under its family, beside the injection"
        );
        assert_eq!(
            l.families.iter().map(|f| f.tokens_avoided).sum::<u64>(),
            200,
            "and its figures are the channel relation's own"
        );
    }

    /// ★ **A pull moves the claimed line and never the report's share**: its
    /// avoided tokens have no recorded read behind them. A decision on the
    /// hook channel is in the share and moves it.
    #[tokio::test]
    async fn a_pull_moves_the_claimed_line_and_never_the_reports_share() {
        let store = audit_store();
        audit_row(&store, "a1", "memd.read_intercepted", 10, 100).await;
        call_row(&store, "c1", "hook_inject", "claude-code:read-gate", 1_000).await;
        let before = levers(&store, "db".to_owned()).await.expect("levers");
        assert_eq!(before.savings_share(900), share_of(100, 900));

        audit_row(&store, "a2", "memd.manifest_served", 10, 100).await;
        call_row(&store, "c2", "skill_cli", "known-state", 2_000).await;
        let after = levers(&store, "db".to_owned()).await.expect("levers");
        assert_eq!(after.savings_share(900), before.savings_share(900));
        let claimed = ClaimedByPulls {
            decisions: 1,
            tokens_avoided: 100,
            tokens_returned: 10,
        };
        assert_eq!(after.claimed_by_pulls, claimed);
        let rendered = render_levers(&after, 900);
        assert!(
            rendered.contains("= 10.0% (100 ÷ 1000)\n")
                && rendered.contains(
                    "  claimed by pulls: 100 avoided on 1 pulls, 10 returned (beside the share, not in it)\n"
                ),
            "{rendered}"
        );
        assert!(!render_levers(&before, 900).contains("claimed by pulls"));

        audit_row(&store, "a3", "memd.read_intercepted", 10, 100).await;
        call_row(&store, "c3", "hook_inject", "claude-code:read-gate", 3_000).await;
        let moved = levers(&store, "db".to_owned()).await.expect("levers");
        assert_eq!(moved.savings_share(900), share_of(200, 900));
        assert_eq!(moved.claimed_by_pulls, claimed);
    }

    /// An observe-mode window that injects and declines nothing is honestly
    /// negative, and the report says so rather than clamping to zero.
    #[tokio::test]
    async fn a_window_that_only_spends_reports_a_negative_net() {
        let store = audit_store();
        audit_row(&store, "a1", "memd.manifest_injected", 108, 0).await;
        let l = levers(&store, "db".to_owned()).await.expect("levers");
        assert_eq!(l.net_tokens(), -108);
        assert!(render_levers(&l, 0).contains("= -108"));
    }

    /// ★ The owner's figure: avoided over avoided-plus-read. A share of
    /// nothing is unanswerable, never 0 %.
    #[test]
    fn the_savings_share_is_avoided_over_avoided_plus_read() {
        let mut l = Levers::unavailable("db".to_owned(), "n/a".to_owned());
        l.note = None;
        l.tokens_avoided = 900;
        assert_eq!(l.savings_share(100), Some(90.0));
        assert_eq!(l.savings_share(900), Some(50.0));
        l.tokens_avoided = 0;
        assert_eq!(l.savings_share(0), None, "0 ÷ 0 is not 0 %");
        assert!(render_levers(&l, 0).contains("unanswerable"));
    }

    /// A pull is credited to the injection it followed, and to nobody when the
    /// nearest one is further back than the span this report declares.
    #[tokio::test]
    async fn a_pull_is_credited_only_to_an_injection_inside_the_span() {
        let store = audit_store();
        call_row(
            &store,
            "i1",
            "hook_inject",
            "claude-code:recovery-map",
            1_000,
        )
        .await;
        call_row(
            &store,
            "p1",
            "skill_cli",
            "rift recall changed-hunks",
            2_000,
        )
        .await;
        call_row(
            &store,
            "p2",
            "skill_cli",
            "rift recall changed-hunks",
            9_000_000,
        )
        .await;

        let l = levers(&store, "db".to_owned()).await.expect("levers");
        let pulls = l.pulls.first().expect("one family pulled");
        assert_eq!((pulls.injections, pulls.pulls), (1, 1));
        assert_eq!(
            l.pulls_without_injection, 1,
            "the pull past the span is reported, never credited"
        );
    }

    /// Basis availability is the bridge's own pair and the bridge's own
    /// numerator, read rather than re-derived: this report writes nothing.
    #[tokio::test]
    async fn basis_availability_counts_held_revisions_with_a_blob_row() {
        let store = audit_store();
        for (node, hash) in [("r1", "aa"), ("r2", "bb")] {
            store
                .insert(
                    "n_file_revision",
                    &["node_id", "path", "blob_oid", "content_sha256", "size"],
                    &[
                        Value::text(node),
                        Value::text("src/lib.rs"),
                        Value::text("oid"),
                        Value::text(hash),
                        Value::Int(1),
                    ],
                )
                .await
                .expect("revision");
            store
                .insert(
                    "n_file_read",
                    &[
                        "node_id",
                        "checkpoint_id",
                        "tool_call_id",
                        "file_revision",
                        "input_tokens",
                        "read_ordinal",
                    ],
                    &[
                        Value::text(format!("read-{node}")),
                        Value::text("ckpt"),
                        Value::text("call"),
                        Value::text(node),
                        Value::Int(10),
                        Value::Int(1),
                    ],
                )
                .await
                .expect("read");
        }
        store
            .insert(
                "n_blob_oid_map",
                &["content_sha256", "blob_oid", "path", "first_seen"],
                &[
                    Value::text("aa"),
                    Value::text("oid"),
                    Value::text("src/lib.rs"),
                    Value::Int(1),
                ],
            )
            .await
            .expect("blob row");

        let l = levers(&store, "db".to_owned()).await.expect("levers");
        assert_eq!((l.held_revisions, l.held_with_blob), (2, 1));
        assert!(render_levers(&l, 0).contains("(50.0%)"));
    }

    /// A projection the report cannot read renders the reason, not zeroes —
    /// the same rule the repeat-class block follows.
    #[test]
    fn an_unreadable_recall_projection_renders_its_reason_not_zeroes() {
        let l = Levers::unavailable(
            "/cache/recall.db".to_owned(),
            "no recall projection yet".to_owned(),
        );
        let rendered = render_levers(&l, 0);
        assert_eq!(
            rendered,
            "levers (audit relations, everything recorded since the reset):\n  \
             recall projection:     /cache/recall.db\n  \
             not measured:          no recall projection yet\n"
        );
    }
}
