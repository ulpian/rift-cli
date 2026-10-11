//! The store reads under the savings share: the audit relation's totals, the
//! channel relation by family, and the capture projection's reads and measured
//! results by family.
//!
//! Apart from the arithmetic in the parent because these are the only places a
//! savings figure touches SQL, and a reader checking what a figure counts reads
//! the query and nothing else.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_store::CanonicalStore;

use super::{
    in_the_share, window, ChannelTokens, ClaimedByPulls, PREVENTED_VERB, UNATTRIBUTED_FAMILY,
};

/// Σ avoided, Σ returned, and the prevented part of the first, over the audit
/// relation from `since_ms` on.
///
/// The third figure is a **subtotal of the first**, taken in the same pass
/// over the same rows: two queries could disagree about a window, and a
/// breakdown that does not add up to the number it breaks down is worse than
/// no breakdown at all.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn audit_totals(
    recall: &impl CanonicalStore,
    since_ms: Option<i64>,
) -> Result<AuditTotals> {
    let (clause, params) = window("recorded_at", since_ms);
    let rows = recall
        .query(
            &format!(
                "SELECT COALESCE(SUM(tokens_avoided), 0), COALESCE(SUM(tokens_returned), 0), \
                 COALESCE(SUM(CASE WHEN verb = '{PREVENTED_VERB}' THEN tokens_avoided ELSE 0 END), 0) \
                 FROM memd_audit_event{clause}"
            ),
            &params,
        )
        .await?;
    let sum = |i: usize| -> u64 {
        rows.first()
            .and_then(|r| r.get_i64(i).ok())
            .and_then(|n| u64::try_from(n).ok())
            .unwrap_or(0)
    };
    Ok(AuditTotals {
        avoided: sum(0),
        returned: sum(1),
        prevented: sum(2),
    })
}

/// What each family decided in the share, and what the pulls claimed, from
/// the only relation that names a family and a channel.
///
/// Every row of the relation counts once, on one side of [`in_the_share`]: a
/// decision in the share joins its family's row, and a pull joins the claimed
/// line whatever its tool name says, so no avoided token is in a family's
/// share and beside it as well.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn decisions_by_family(
    recall: &impl CanonicalStore,
    since_ms: Option<i64>,
) -> Result<Decisions> {
    let (clause, params) = window("recorded_at", since_ms);
    let rows = recall
        .query(
            &format!(
                "SELECT channel, tool_name, COALESCE(SUM(tokens_avoided), 0), \
                 COALESCE(SUM(tokens_returned), 0), COUNT(*), \
                 COALESCE(SUM(CASE WHEN terminal_verb = '{PREVENTED_VERB}' \
                 THEN tokens_avoided ELSE 0 END), 0) \
                 FROM serve_call_event{clause} \
                 GROUP BY channel, tool_name ORDER BY tool_name ASC, channel ASC"
            ),
            &params,
        )
        .await?;
    let mut out = Decisions::default();
    for r in &rows {
        let channel = r.get_text(0)?;
        let tool = r.get_text(1)?;
        let n = |i: usize| u64::try_from(r.get_i64(i).unwrap_or(0)).unwrap_or(0);
        if !in_the_share(&channel) {
            out.pulls.book(n(4), n(2), n(3));
            continue;
        }
        // `<family>:<step>`; a name with no step is the family itself.
        let family = tool
            .split_once(':')
            .map_or(tool.clone(), |(f, _)| f.to_owned());
        let entry = out.families.entry(family).or_default();
        entry.avoided = entry.avoided.saturating_add(n(2));
        entry.returned = entry.returned.saturating_add(n(3));
        entry.rows = entry.rows.saturating_add(n(4));
        entry.prevented = entry.prevented.saturating_add(n(5));
    }
    Ok(out)
}

/// The audit relation's three sums over one window.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AuditTotals {
    /// Σ avoided, pulls included: the relation names no channel.
    pub avoided: u64,
    /// Σ returned, pulls included.
    pub returned: u64,
    /// The prevented part of `avoided`, inside it and never beside it.
    pub prevented: u64,
}

/// The channel relation read on both sides of [`in_the_share`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Decisions {
    /// Each family's decisions in the share, by family name.
    pub families: BTreeMap<String, FamilyDecisions>,
    /// Every pull, whichever name it was made under.
    pub pulls: ClaimedByPulls,
}

/// What one family's rows of the channel relation add up to.
///
/// A named struct rather than a tuple because it grew a fourth figure that is
/// a *part* of the first, and a reader of `(u64, u64, u64, u64)` has no way to
/// know which.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FamilyDecisions {
    /// Σ avoided over its rows.
    pub avoided: u64,
    /// Σ returned over its rows.
    pub returned: u64,
    /// How many rows it has — its decisions.
    pub rows: u64,
    /// The prevented part of `avoided`, inside it and never beside it.
    pub prevented: u64,
}

/// What one family's rows of the capture projection add up to: its read
/// tokens, the sessions behind them, and the reads that carry no token count.
///
/// A named struct for [`FamilyDecisions`]'s reason — the third figure is not a
/// part of the first two, it is the count of rows that weigh **nothing** in
/// them, and a reader of `(u64, u64, u64)` has no way to know that.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FamilyReads {
    /// Σ recorded read tokens.
    pub tokens: u64,
    /// Distinct sessions that read a file.
    pub sessions: u64,
    /// Reads with no `file_read_tokens` row — counted as reads, worth zero.
    pub unmeasured: u64,
}

/// What each family read, from the capture projection.
///
/// The joins are the measurement query's, unchanged — a read is a `tool_call`
/// with a resolved `tool_reads_file` link, attributed through its turn, with
/// the token count LEFT joined because it is recorded late and sparsely — so
/// this total is the same set of rows the reread report classifies, summed in
/// SQL instead of in memory.
///
/// That LEFT join is also why the third figure exists. A read the projection
/// never froze a count for stays in the row set and adds nothing to the sum,
/// so it is a denominator row of weight zero. It is **counted, not imputed**:
/// the count leaves this function beside the tokens and every surface prints
/// it, rather than a figure invented from bytes that have since moved.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn reads_by_family(
    projection: &impl CanonicalStore,
    since_ms: Option<i64>,
) -> Result<BTreeMap<String, FamilyReads>> {
    // A read is placed by when the tool call was made, which is the same
    // instant the reread report places one by — the two documents cut their
    // windows on one clock or they are not comparable.
    let (clause, params) = window("tc.started_at", since_ms);
    let rows = projection
        .query(
            &format!(
                "SELECT COALESCE(s.agent, ''), COALESCE(SUM(frt.input_tokens), 0), \
                 COUNT(DISTINCT t.session_id), \
                 COALESCE(SUM(CASE WHEN frt.tool_call_id IS NULL THEN 1 ELSE 0 END), 0) \
                 FROM tool_call tc \
                 JOIN turn t ON t.turn_id = tc.turn_id \
                 JOIN tool_reads_file trf ON trf.tool_call_id = tc.tool_call_id \
                 JOIN file_revision fr ON fr.file_revision_id = trf.file_revision_id \
                 LEFT JOIN file_read_tokens frt ON frt.tool_call_id = tc.tool_call_id \
                 LEFT JOIN session s ON s.session_id = t.session_id{clause} \
                 GROUP BY COALESCE(s.agent, '') ORDER BY 1 ASC"
            ),
            &params,
        )
        .await?;
    let mut out: BTreeMap<String, FamilyReads> = BTreeMap::new();
    for r in &rows {
        let agent = r.get_text(0).unwrap_or_default();
        let family = if agent.is_empty() {
            UNATTRIBUTED_FAMILY.to_owned()
        } else {
            agent
        };
        let n = |i: usize| u64::try_from(r.get_i64(i).unwrap_or(0)).unwrap_or(0);
        let entry = out.entry(family).or_default();
        entry.tokens = entry.tokens.saturating_add(n(1));
        entry.sessions = entry.sessions.saturating_add(n(2));
        entry.unmeasured = entry.unmeasured.saturating_add(n(3));
    }
    Ok(out)
}

/// What each family read, **per tool**, from the capture projection.
///
/// The same rows [`reads_by_family`] sums, grouped one level finer so a caller
/// that holds the families' pinned tool tables can split them between the read
/// tool and the shell. This crate is a leaf and cannot read those tables, so
/// it hands out the tool names and classifies nothing.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn reads_by_family_and_tool(
    projection: &impl CanonicalStore,
    since_ms: Option<i64>,
) -> Result<BTreeMap<(String, String), u64>> {
    let (clause, params) = window("tc.started_at", since_ms);
    let rows = projection
        .query(
            &format!(
                "SELECT COALESCE(s.agent, ''), COALESCE(tc.tool_name, ''), \
                 COALESCE(SUM(frt.input_tokens), 0) \
                 FROM tool_call tc \
                 JOIN turn t ON t.turn_id = tc.turn_id \
                 JOIN tool_reads_file trf ON trf.tool_call_id = tc.tool_call_id \
                 JOIN file_revision fr ON fr.file_revision_id = trf.file_revision_id \
                 LEFT JOIN file_read_tokens frt ON frt.tool_call_id = tc.tool_call_id \
                 LEFT JOIN session s ON s.session_id = t.session_id{clause} \
                 GROUP BY 1, 2 ORDER BY 1, 2"
            ),
            &params,
        )
        .await?;
    let mut out: BTreeMap<(String, String), u64> = BTreeMap::new();
    for r in &rows {
        let agent = r.get_text(0).unwrap_or_default();
        let family = if agent.is_empty() {
            UNATTRIBUTED_FAMILY.to_owned()
        } else {
            agent
        };
        let tokens = u64::try_from(r.get_i64(2).unwrap_or(0)).unwrap_or(0);
        let entry = out
            .entry((family, r.get_text(1).unwrap_or_default()))
            .or_insert(0);
        *entry = entry.saturating_add(tokens);
    }
    Ok(out)
}

/// What each family's **measured tool results** hold: the search tokens, and
/// how many of its content-returning calls nobody measured.
///
/// The relation names its own channel and its own basis, so nothing is
/// inferred here: a row whose basis is `unmeasured` carries no tokens and is
/// counted, and every other row's tokens are summed under its channel.
///
/// A projection older than the relation answers as though every family had
/// none, which is what it had: the query is allowed to fail and the caller
/// reads that as "this projection records no measured results yet" rather than
/// as an error, because a stale binary or an unmigrated database must not
/// break a measurement that never needed it.
///
/// # Errors
/// Never: a projection without the relation yields an empty map.
pub async fn tool_results_by_family(
    projection: &impl CanonicalStore,
    since_ms: Option<i64>,
) -> Result<BTreeMap<String, (ChannelTokens, u64)>> {
    let (clause, params) = window("trt.recorded_at", since_ms);
    let Ok(rows) = projection
        .query(
            &format!(
                "SELECT COALESCE(s.agent, ''), trt.channel, trt.basis, \
                 COALESCE(SUM(trt.output_tokens), 0), COUNT(*) \
                 FROM tool_result_tokens trt \
                 LEFT JOIN session s ON s.session_id = trt.session_id{clause} \
                 GROUP BY 1, 2, 3 ORDER BY 1, 2, 3"
            ),
            &params,
        )
        .await
    else {
        return Ok(BTreeMap::new());
    };
    let mut out: BTreeMap<String, (ChannelTokens, u64)> = BTreeMap::new();
    for r in &rows {
        let agent = r.get_text(0).unwrap_or_default();
        let family = if agent.is_empty() {
            UNATTRIBUTED_FAMILY.to_owned()
        } else {
            agent
        };
        let channel = r.get_text(1).unwrap_or_default();
        let basis = r.get_text(2).unwrap_or_default();
        let tokens = u64::try_from(r.get_i64(3).unwrap_or(0)).unwrap_or(0);
        let calls = u64::try_from(r.get_i64(4).unwrap_or(0)).unwrap_or(0);
        let entry = out.entry(family).or_default();
        if basis == "unmeasured" {
            entry.1 = entry.1.saturating_add(calls);
            continue;
        }
        if channel == "search" {
            entry.0.search = entry.0.search.saturating_add(tokens);
        }
    }
    Ok(out)
}
