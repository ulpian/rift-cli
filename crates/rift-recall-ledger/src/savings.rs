//! The savings share: `avoided ÷ (avoided + read)`, since the projection was
//! reset — **one computation, in one place**.
//!
//! Four surfaces publish this figure (the reread report, the `savings` verb,
//! the recall status table and the dashboard's Results-reuse card and its
//! breakdown modal). Each of them opens a different pair of stores at a
//! different moment, so the only way the four can be one number is for the
//! arithmetic AND the two queries under it to live here and be called, never
//! re-derived. A second copy is how a report and a dashboard start disagreeing
//! about the same repository in front of the person judging it.
//!
//! Two relations, never joined:
//!
//! * the **audit** relation (`memd_audit_event`) carries what every decision
//!   avoided and returned. It names no family, so it is the whole-family
//!   headline and nothing else.
//! * the **channel** relation (`serve_call_event`) is the only per-family axis
//!   the serving side has (`tool_name` is `<family>:<step>`). Its figures are
//!   its own.
//!
//! The read side of the denominator comes from a third store, the capture
//! projection, keyed by the family's capture id. A family's share therefore
//! pairs the channel relation's avoided tokens with the projection's read
//! tokens — joined on the family NAME and nothing else, which every caller
//! prints beside the number.
//!
//! The window the figure covers is an **input**, not something this module
//! decides: [`crate::savings_window`] resolves the family's boundary marker (or
//! a one-call override) and every caller passes the same cut in, so two
//! surfaces cannot publish the same arithmetic over two different periods.
//!
//! Absence is never zero here. `None` means the question has no answer yet (no
//! token on either side); a caller that printed `0 %` for it would be claiming
//! a measurement nobody made.
//!
//! Two facts about the fraction are published rather than papered over, because
//! both move the figure and neither can be fixed here:
//!
//! * **The two sides are cut on two clocks.** The numerator's rows are placed
//!   by `memd_audit_event.recorded_at` — when Rift decided — and the
//!   denominator's by `tool_call.started_at` — when the agent read. They are
//!   the instants each relation actually records, and the second is the one the
//!   reread report already places a read by, so the two documents stay
//!   comparable. Near a boundary a decision and the read it answers can
//!   therefore fall on opposite sides of the cut. The window each surface used
//!   is printed beside every figure for exactly this reason.
//! * **A read with no token count is a denominator row of weight zero**, and
//!   there are usually some: the count is recorded late and sparsely. They are
//!   counted into [`Savings::reads_unmeasured`] and published, never imputed.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_store::{CanonicalStore, Value};

/// The audit verb a **prevented** read is booked under — a read Rift caused
/// not to happen, rather than one it declined.
///
/// Prevented rows are ordinary rows of the two relations below, so they are
/// already inside every avoided total here; what this names is the way to
/// report them *beside* the measured intercepts as well as inside them. It is
/// spelled once, and `bins/rift` pins it against the event kind's own verb, so
/// the two cannot drift apart.
pub const PREVENTED_VERB: &str = "memd.read_prevented";

/// The name a read whose session row the projection has not caught up with is
/// credited to. It is a real bucket, not a family: keeping it visible is what
/// stops those reads from being silently folded into a family that may not
/// have made them.
pub const UNATTRIBUTED_FAMILY: &str = "(unattributed)";

/// `avoided ÷ (avoided + read)` as a percentage.
///
/// `None` when the denominator is empty — a share of nothing is unanswerable,
/// not zero.
#[must_use]
pub fn share(avoided: u64, read: u64) -> Option<f64> {
    let denominator = avoided.saturating_add(read);
    if denominator == 0 {
        return None;
    }
    Some((avoided as f64) * 100.0 / (denominator as f64))
}

/// The honesty gate as one signed number: what was avoided, net of what the
/// reasons and injections that avoided it cost.
///
/// Signed on purpose — an observe window that injects and declines nothing is
/// legitimately negative, and a gate that could not say so would be useless.
#[must_use]
pub fn honesty(avoided: u64, returned: u64) -> i64 {
    let avoided = i64::try_from(avoided).unwrap_or(i64::MAX);
    let returned = i64::try_from(returned).unwrap_or(i64::MAX);
    avoided.saturating_sub(returned)
}

/// "Is this family's tool a shell?", answered from the families' own pinned
/// tool tables.
///
/// A parameter rather than a table here, because this crate is a leaf: the
/// pins live beside the adapters, and a second copy of them in the ledger is
/// exactly the "two authorities for one number" shape this plane keeps out.
pub type ShellClassifier<'a> = &'a (dyn Fn(&str, &str) -> bool + Sync);

/// Where the repository content a window was handed came from.
///
/// The denominator is **repository content on any channel**, not read-shaped
/// calls: a search returns repository content too, and until the capture
/// projection had a relation that could hold one, every published share was
/// the share of a pool smaller than the one the levers act on.
///
/// The two read figures are `Option` on purpose. Splitting a read between a
/// structured read tool and a shell command line is a question only the
/// families' own pinned tables can answer, and this crate is a leaf that does
/// not read them — so a caller that supplies the classifier gets the split and
/// one that does not gets **absence**, never a zero that reads like a
/// measurement (`docs/guide/08-cost-and-savings.md` §8.7, rung 4). Their sum,
/// known or not, is always `Savings::read_tokens` minus `search`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ChannelTokens {
    /// Read through a structured read tool.
    pub read_tool: Option<u64>,
    /// Read by a command line the shell grammar recognised.
    pub shell_read: Option<u64>,
    /// Returned by a measured search.
    pub search: u64,
}

/// One family's inputs to the share, from the two relations that hold them.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct FamilyShare {
    /// The family, as the relation that named it spells it.
    pub family: String,
    /// What its decisions avoided (the channel relation's own figure).
    pub tokens_avoided: u64,
    /// How much of that was avoided by a read that never happened, rather
    /// than by one that was declined. **Part of `tokens_avoided`, never
    /// beside it**: it is separable, not additional.
    pub tokens_prevented: u64,
    /// What they returned (the same relation).
    pub tokens_returned: u64,
    /// Rows the channel relation holds for it — its decisions.
    pub decisions: u64,
    /// Repository-content tokens the capture projection recorded for it, on
    /// every channel: file reads plus measured search results.
    pub read_tokens: u64,
    /// Where those tokens came from.
    pub channels: ChannelTokens,
    /// Calls of this family that returned repository content and that nobody
    /// measured — a **count of calls**, never a count of tokens: file reads
    /// with no `file_read_tokens` row and searches the worker could not run
    /// again.
    ///
    /// They are inside the denominator's row set and contribute zero to it,
    /// which lifts the share by however much they would have weighed. The
    /// figure is published rather than corrected: imputing a count for bytes
    /// nobody froze is the imputation-without-evidence the honesty ladder
    /// forbids, and a denominator quietly missing reads is exactly what a
    /// reader has to be able to see.
    pub reads_unmeasured: u64,
    /// Sessions of this family that read a file: the `n` behind its share.
    pub sessions_with_reads: u64,
}

impl FamilyShare {
    /// This family's share, or `None` when neither side holds a token.
    #[must_use]
    pub fn share(&self) -> Option<f64> {
        share(self.tokens_avoided, self.read_tokens)
    }

    /// This family's honesty figure.
    #[must_use]
    pub fn honesty(&self) -> i64 {
        honesty(self.tokens_avoided, self.tokens_returned)
    }

    /// A family that decided and whose reads this repository never captured.
    ///
    /// Codex was the recorded case, and how it stopped being one says what
    /// this predicate is for. Its reads arrive as shell command lines, and
    /// while the projection resolved none of them to a file its avoided
    /// tokens landed in the numerator with no denominator of their own behind
    /// them. A family in that state inflates every share it is counted in,
    /// which is why the excluding variant exists and is published beside the
    /// plain one. The answer was to capture the reads, not to soften the
    /// predicate: a command line the pinned grammar recognises now records a
    /// read fact like any other, and one whose range is not derivable still
    /// records nothing — so a family can land here again, and is still
    /// named rather than averaged in when it does.
    #[must_use]
    pub fn decided_without_reads(&self) -> bool {
        self.decisions > 0 && self.read_tokens == 0
    }
}

/// Every figure the savings share is composed of, for one repository family.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct Savings {
    /// Σ avoided over the audit relation — the headline numerator.
    pub tokens_avoided: u64,
    /// The part of it that reads which never happened account for. **Inside
    /// `tokens_avoided`, not added to it** — the owner's figure is one
    /// figure, and this is the breakdown of it the honesty rule asks for.
    pub tokens_prevented: u64,
    /// Σ returned over the same relation — the honesty gate's spend side.
    pub tokens_returned: u64,
    /// Every repository-content token the capture projection holds, on every
    /// channel: file reads plus measured search results.
    pub read_tokens: u64,
    /// Where those tokens came from.
    pub channels: ChannelTokens,
    /// Calls that returned repository content and that nobody measured — a
    /// **count of calls**, published beside the denominator so a reader can
    /// see how much of the pool is missing from it rather than assume none is.
    pub reads_unmeasured: u64,
    /// Sessions that read a file, over every family: the headline's `n`.
    pub sessions_with_reads: u64,
    /// The per-family rows, ordered by family name.
    pub families: Vec<FamilyShare>,
}

impl Savings {
    /// The owner's figure: `avoided ÷ (avoided + read)` over everything since
    /// the reset, as a percentage. `None` when nothing was read and nothing
    /// avoided.
    #[must_use]
    pub fn share(&self) -> Option<f64> {
        share(self.tokens_avoided, self.read_tokens)
    }

    /// The honesty gate beside the share.
    #[must_use]
    pub fn honesty(&self) -> i64 {
        honesty(self.tokens_avoided, self.tokens_returned)
    }

    /// The same share over the per-family rows alone.
    ///
    /// It is computed from the CHANNEL relation, so it is not required to
    /// equal [`Self::share`] and the callers print which relation each came
    /// from. Its only job is to be the comparable twin of
    /// [`Self::share_excluding_undenominated`]: both sides of that comparison
    /// have to be summed from the same rows or the difference between them
    /// would be an artefact of the relation, not of the exclusion.
    #[must_use]
    pub fn families_share(&self) -> Option<f64> {
        let (avoided, read) = self.families.iter().fold((0u64, 0u64), |(a, r), f| {
            (
                a.saturating_add(f.tokens_avoided),
                r.saturating_add(f.read_tokens),
            )
        });
        share(avoided, read)
    }

    /// [`Self::families_share`] with every family that decided and whose reads
    /// were never captured left out of both sides.
    #[must_use]
    pub fn share_excluding_undenominated(&self) -> Option<f64> {
        let (avoided, read) = self
            .families
            .iter()
            .filter(|f| !f.decided_without_reads())
            .fold((0u64, 0u64), |(a, r), f| {
                (
                    a.saturating_add(f.tokens_avoided),
                    r.saturating_add(f.read_tokens),
                )
            });
        share(avoided, read)
    }

    /// The families [`Self::share_excluding_undenominated`] leaves out, named
    /// so the exclusion is auditable rather than implied.
    #[must_use]
    pub fn excluded(&self) -> Vec<&str> {
        self.families
            .iter()
            .filter(|f| f.decided_without_reads())
            .map(|f| f.family.as_str())
            .collect()
    }
}

/// The window clause for one relation's timestamp column, and its parameter.
///
/// `None` is deliberately *no clause at all* rather than a `>= 0`: the query a
/// reader has always run is the query that runs when nobody has set a
/// boundary, so a marker is the only thing that can ever narrow a published
/// denominator. The column names are this module's own constants; nothing a
/// caller types reaches the SQL.
fn window(column: &str, since_ms: Option<i64>) -> (String, Vec<Value>) {
    since_ms.map_or_else(
        || (String::new(), Vec::new()),
        |ms| (format!(" WHERE {column} >= ?1"), vec![Value::Int(ms)]),
    )
}

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
) -> Result<(u64, u64, u64)> {
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
    Ok((sum(0), sum(1), sum(2)))
}

/// What each family decided, from the only relation that names one.
///
/// Every row of the relation counts, on either channel: a table built from the
/// injection channel alone leaves a family's own skill-CLI calls out of the
/// figures it is read for.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn decisions_by_family(
    recall: &impl CanonicalStore,
    since_ms: Option<i64>,
) -> Result<BTreeMap<String, FamilyDecisions>> {
    let (clause, params) = window("recorded_at", since_ms);
    let rows = recall
        .query(
            &format!(
                "SELECT tool_name, COALESCE(SUM(tokens_avoided), 0), \
                 COALESCE(SUM(tokens_returned), 0), COUNT(*), \
                 COALESCE(SUM(CASE WHEN terminal_verb = '{PREVENTED_VERB}' \
                 THEN tokens_avoided ELSE 0 END), 0) \
                 FROM serve_call_event{clause} GROUP BY tool_name ORDER BY tool_name ASC"
            ),
            &params,
        )
        .await?;
    let mut out: BTreeMap<String, FamilyDecisions> = BTreeMap::new();
    for r in &rows {
        let tool = r.get_text(0)?;
        // `<family>:<step>`; a name with no step is the family itself.
        let family = tool
            .split_once(':')
            .map_or(tool.clone(), |(f, _)| f.to_owned());
        let entry = out.entry(family).or_default();
        let n = |i: usize| u64::try_from(r.get_i64(i).unwrap_or(0)).unwrap_or(0);
        entry.avoided = entry.avoided.saturating_add(n(1));
        entry.returned = entry.returned.saturating_add(n(2));
        entry.rows = entry.rows.saturating_add(n(3));
        entry.prevented = entry.prevented.saturating_add(n(4));
    }
    Ok(out)
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

/// Compose the savings figure from the family's two open stores, over the
/// window that starts at `since_ms`.
///
/// `since_ms` is the boundary
/// [`crate::savings_window::Boundary::cut_ms`] resolved — the family's marker,
/// a one-call override, or `None` for everything the record holds. It is an
/// **input** rather than something this module derives, so that every surface
/// publishing the share is publishing the same window as well as the same
/// arithmetic.
///
/// Both handles should be opened READ-ONLY by the caller: a measurement must
/// never migrate the projection it is measuring.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure against either store.
pub async fn compose(
    recall: &impl CanonicalStore,
    projection: &impl CanonicalStore,
    since_ms: Option<i64>,
) -> Result<Savings> {
    compose_with(recall, projection, since_ms, None).await
}

/// [`compose`] with the read channel split as well.
///
/// `shell` answers "is this family's tool a shell?" from the families' own
/// pinned tool tables, which this crate is a leaf and cannot read. Passing
/// `None` is not a claim that nothing was read by a shell: it leaves both read
/// figures **absent**, which is what a surface with no honest basis for a
/// number shows instead of a zero.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure against either store.
pub async fn compose_with(
    recall: &impl CanonicalStore,
    projection: &impl CanonicalStore,
    since_ms: Option<i64>,
    shell: Option<ShellClassifier<'_>>,
) -> Result<Savings> {
    let (tokens_avoided, tokens_returned, tokens_prevented) =
        audit_totals(recall, since_ms).await?;
    let decisions = decisions_by_family(recall, since_ms).await?;
    let reads = reads_by_family(projection, since_ms).await?;
    let results = tool_results_by_family(projection, since_ms).await?;
    let split = match shell {
        None => BTreeMap::new(),
        Some(is_shell) => {
            let mut split: BTreeMap<String, (u64, u64)> = BTreeMap::new();
            for ((family, tool), tokens) in reads_by_family_and_tool(projection, since_ms).await? {
                let entry = split.entry(family.clone()).or_insert((0, 0));
                if is_shell(&family, &tool) {
                    entry.1 = entry.1.saturating_add(tokens);
                } else {
                    entry.0 = entry.0.saturating_add(tokens);
                }
            }
            split
        }
    };
    Ok(assemble(
        tokens_avoided,
        tokens_returned,
        tokens_prevented,
        &decisions,
        &reads,
        &results,
        (shell.is_some()).then_some(&split),
    ))
}

/// The pure half of [`compose`]: fold the two per-family maps into rows.
///
/// Split out so the composition is testable without a store, and so the two
/// maps are joined in exactly one place.
#[must_use]
#[allow(clippy::too_many_arguments)] // one join, one argument per relation it reads
pub fn assemble(
    tokens_avoided: u64,
    tokens_returned: u64,
    tokens_prevented: u64,
    decisions: &BTreeMap<String, FamilyDecisions>,
    reads: &BTreeMap<String, FamilyReads>,
    results: &BTreeMap<String, (ChannelTokens, u64)>,
    read_split: Option<&BTreeMap<String, (u64, u64)>>,
) -> Savings {
    let mut families: BTreeMap<String, FamilyShare> = BTreeMap::new();
    for (family, decided) in decisions {
        let entry = families.entry(family.clone()).or_default();
        entry.family.clone_from(family);
        entry.tokens_avoided = decided.avoided;
        entry.tokens_returned = decided.returned;
        entry.decisions = decided.rows;
        entry.tokens_prevented = decided.prevented;
    }
    for (family, read) in reads {
        let entry = families.entry(family.clone()).or_default();
        entry.family.clone_from(family);
        entry.read_tokens = read.tokens;
        entry.sessions_with_reads = read.sessions;
        entry.reads_unmeasured = read.unmeasured;
        if let Some(split) = read_split {
            let (tool, shell) = split.get(family).copied().unwrap_or((0, 0));
            entry.channels.read_tool = Some(tool);
            entry.channels.shell_read = Some(shell);
        }
    }
    // The measured searches: repository content on a channel that has never
    // been in the denominator before, added to it here and published under its
    // own name so a reader can take it back out. A search nobody measured
    // joins the unmeasured count beside the file reads that carry no token
    // row: both are calls that returned repository content and weigh nothing.
    for (family, (channels, unmeasured)) in results {
        let entry = families.entry(family.clone()).or_default();
        entry.family.clone_from(family);
        entry.channels.search = channels.search;
        entry.reads_unmeasured = entry.reads_unmeasured.saturating_add(*unmeasured);
        entry.read_tokens = entry.read_tokens.saturating_add(channels.search);
        if read_split.is_some() && entry.channels.read_tool.is_none() {
            // A family whose only content-returning calls were searches: the
            // split is known and both halves of it are zero.
            entry.channels.read_tool = Some(0);
            entry.channels.shell_read = Some(0);
        }
    }
    let fold = |pick: fn(&FamilyShare) -> u64| {
        families
            .values()
            .fold(0u64, |acc, f| acc.saturating_add(pick(f)))
    };
    let read_tokens = fold(|f| f.read_tokens);
    let sessions_with_reads = reads
        .values()
        .fold(0u64, |acc, r| acc.saturating_add(r.sessions));
    let channels = ChannelTokens {
        read_tool: read_split.map(|_| fold(|f| f.channels.read_tool.unwrap_or(0))),
        shell_read: read_split.map(|_| fold(|f| f.channels.shell_read.unwrap_or(0))),
        search: fold(|f| f.channels.search),
    };
    Savings {
        tokens_avoided,
        tokens_prevented,
        tokens_returned,
        read_tokens,
        channels,
        reads_unmeasured: fold(|f| f.reads_unmeasured),
        sessions_with_reads,
        families: families.into_values().collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn family(name: &str, avoided: u64, returned: u64, decisions: u64, read: u64) -> FamilyShare {
        FamilyShare {
            family: name.to_owned(),
            tokens_avoided: avoided,
            tokens_returned: returned,
            decisions,
            read_tokens: read,
            sessions_with_reads: u64::from(read > 0),
            tokens_prevented: 0,
            channels: ChannelTokens::default(),
            reads_unmeasured: 0,
        }
    }

    fn read(tokens: u64, sessions: u64) -> FamilyReads {
        FamilyReads {
            tokens,
            sessions,
            unmeasured: 0,
        }
    }

    #[test]
    fn the_share_is_avoided_over_avoided_plus_read() {
        assert_eq!(share(900, 100), Some(90.0));
        assert_eq!(share(900, 900), Some(50.0));
        assert_eq!(share(0, 100), Some(0.0));
        assert_eq!(share(100, 0), Some(100.0));
        // A share of nothing is unanswerable, never 0 %.
        assert_eq!(share(0, 0), None);
    }

    #[test]
    fn the_honesty_figure_is_signed() {
        assert_eq!(honesty(900, 160), 740);
        assert_eq!(honesty(0, 432), -432);
    }

    /// The Codex case, as measured on this repository: a family whose reads
    /// arrive as shell calls the projection resolves to no file decides all
    /// the same, and its avoided tokens have no denominator of their own. The
    /// plain share carries them; the excluding variant must not.
    #[test]
    fn a_family_with_decisions_and_no_reads_is_excluded_from_the_excluding_variant() {
        let s = Savings {
            tokens_avoided: 1_000,
            tokens_prevented: 0,
            tokens_returned: 0,
            read_tokens: 1_000,
            sessions_with_reads: 1,
            channels: ChannelTokens::default(),
            reads_unmeasured: 0,
            families: vec![
                family("claude-code", 500, 0, 4, 1_000),
                family("codex", 500, 0, 3, 0),
            ],
        };
        assert!(s.families[1].decided_without_reads());
        assert!(!s.families[0].decided_without_reads());
        // 1000 avoided over 1000 avoided + 1000 read.
        assert_eq!(s.families_share(), Some(50.0));
        // Codex out of BOTH sides: 500 over 500 + 1000.
        let excluding = s.share_excluding_undenominated().expect("answerable");
        assert!((excluding - 100.0 / 3.0).abs() < 1e-9, "{excluding}");
        assert_eq!(s.excluded(), vec!["codex"]);
    }

    /// A family that read and never decided is NOT excluded: it has a
    /// denominator, and dropping it would flatter the share.
    #[test]
    fn a_family_that_read_and_never_decided_stays_in_the_excluding_variant() {
        let s = Savings {
            families: vec![family("antigravity", 0, 0, 0, 4_000)],
            ..Savings::default()
        };
        assert!(!s.families[0].decided_without_reads());
        assert_eq!(s.share_excluding_undenominated(), Some(0.0));
        assert!(s.excluded().is_empty());
    }

    #[test]
    fn the_two_relations_are_joined_by_name_and_nothing_else() {
        let decided = |avoided, returned, rows, prevented| FamilyDecisions {
            avoided,
            returned,
            rows,
            prevented,
        };
        let decisions = BTreeMap::from([
            ("codex".to_owned(), decided(300, 20, 2, 0)),
            ("claude-code".to_owned(), decided(700, 40, 5, 250)),
        ]);
        let reads = BTreeMap::from([
            ("claude-code".to_owned(), read(9_000, 3)),
            ("(unattributed)".to_owned(), read(1_000, 1)),
        ]);
        let s = assemble(1_100, 60, 250, &decisions, &reads, &BTreeMap::new(), None);
        assert_eq!(s.families.len(), 3, "three names across two relations");
        // The headline is the AUDIT relation's total, not the sum of rows.
        assert_eq!(s.tokens_avoided, 1_100);
        assert_eq!(s.read_tokens, 10_000);
        assert_eq!(s.sessions_with_reads, 4);
        assert_eq!(s.share(), share(1_100, 10_000));
        assert_eq!(s.honesty(), 1_040);
        assert_eq!(s.excluded(), vec!["codex"]);
        // The prevented figure is INSIDE the avoided one, per family and
        // overall: a breakdown that did not add up would be a second claim.
        assert_eq!(s.tokens_prevented, 250);
        let cc = s
            .families
            .iter()
            .find(|f| f.family == "claude-code")
            .expect("the family that prevented a read");
        assert_eq!(cc.tokens_prevented, 250);
        assert!(cc.tokens_prevented <= cc.tokens_avoided);
    }

    #[test]
    fn an_empty_family_set_answers_nothing_rather_than_zero() {
        let s = Savings::default();
        assert_eq!(s.share(), None);
        assert_eq!(s.families_share(), None);
        assert_eq!(s.share_excluding_undenominated(), None);
    }

    /// One session, one turn and two reads of two files — with a frozen token
    /// count on the **first only**, which is the case the figure exists for.
    async fn seed_two_reads_one_unmeasured(projection: &rift_store::SqliteStore) {
        fixture_session_and_turn(projection).await;
        for (i, path) in ["src/a.rs", "src/b.rs"].iter().enumerate() {
            fixture_read(projection, i, path).await;
        }
        // Only the FIRST read carries a frozen count.
        fixture_frozen_count(projection).await;
    }

    /// Insert one capture-projection row: the seeders' only write.
    async fn fixture_put(
        projection: &rift_store::SqliteStore,
        table: &str,
        cols: &[&str],
        vals: Vec<Value>,
    ) {
        projection.insert(table, cols, &vals).await.unwrap();
    }

    /// The one session (agent `claude-code`) and the one turn every read of
    /// the seeder hangs from.
    async fn fixture_session_and_turn(projection: &rift_store::SqliteStore) {
        const SESSION: [&str; 6] = [
            "session_id",
            "agent",
            "model_primary",
            "repo_id",
            "started_at",
            "schema_version",
        ];
        const TURN: [&str; 5] = [
            "turn_id",
            "session_id",
            "ordinal",
            "started_at",
            "schema_version",
        ];
        let session = vec![
            Value::text("s1"),
            Value::text("claude-code"),
            Value::text("m"),
            Value::text("r"),
            Value::Int(1),
            Value::Int(1),
        ];
        fixture_put(projection, "session", &SESSION, session).await;
        let turn = vec![
            Value::text("t1"),
            Value::text("s1"),
            Value::Int(0),
            Value::Int(1),
            Value::Int(1),
        ];
        fixture_put(projection, "turn", &TURN, turn).await;
    }

    /// One read of `path`: the call `tc{i}`, the revision `fr{i}` it read,
    /// and the link between them — and no token count.
    async fn fixture_read(projection: &rift_store::SqliteStore, i: usize, path: &str) {
        const CALL: [&str; 5] = [
            "tool_call_id",
            "turn_id",
            "tool_name",
            "started_at",
            "schema_version",
        ];
        const REVISION: [&str; 5] = [
            "file_revision_id",
            "path",
            "blob_oid",
            "content_sha256",
            "schema_version",
        ];
        const READS: [&str; 2] = ["tool_call_id", "file_revision_id"];
        let (call, revision) = (format!("tc{i}"), format!("fr{i}"));
        let row = vec![
            Value::text(call.as_str()),
            Value::text("t1"),
            Value::text("Read"),
            Value::Int(10),
            Value::Int(1),
        ];
        fixture_put(projection, "tool_call", &CALL, row).await;
        let row = vec![
            Value::text(revision.as_str()),
            Value::text(path),
            Value::text("b"),
            Value::text("c".repeat(64)),
            Value::Int(1),
        ];
        fixture_put(projection, "file_revision", &REVISION, row).await;
        let link = vec![Value::text(call.as_str()), Value::text(revision.as_str())];
        fixture_put(projection, "tool_reads_file", &READS, link).await;
    }

    /// The frozen token count of the first read, `tc0`: 9,000 tokens.
    async fn fixture_frozen_count(projection: &rift_store::SqliteStore) {
        const TOKENS: [&str; 4] = [
            "tool_call_id",
            "input_tokens",
            "tokenizer",
            "schema_version",
        ];
        let frozen = vec![
            Value::text("tc0"),
            Value::Int(9_000),
            Value::text("cl100k_base"),
            Value::Int(1),
        ];
        fixture_put(projection, "file_read_tokens", &TOKENS, frozen).await;
    }

    /// A read whose token count was never frozen is a denominator row of
    /// weight zero, and the figure says how many there were.
    ///
    /// It is the whole reason the count is published: the two reads below are
    /// indistinguishable in the sum — one weighs 9,000 tokens and one weighs
    /// nothing — so a share of 10 % over "two reads" is really a share over
    /// one read and a row nobody measured.
    #[tokio::test]
    async fn a_read_with_no_token_count_is_counted_and_never_imputed() {
        let projection = rift_store::SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&projection, &rift_store::default_migrations()).unwrap();
        seed_two_reads_one_unmeasured(&projection).await;
        let reads = reads_by_family(&projection, None).await.unwrap();
        assert_eq!(
            reads.get("claude-code"),
            Some(&FamilyReads {
                tokens: 9_000,
                sessions: 1,
                unmeasured: 1,
            })
        );
        let s = assemble(
            1_000,
            0,
            0,
            &BTreeMap::new(),
            &reads,
            &BTreeMap::new(),
            None,
        );
        assert_eq!(s.read_tokens, 9_000, "the unmeasured read adds nothing");
        assert_eq!(s.reads_unmeasured, 1, "…and is counted where it is visible");
        assert_eq!(s.families[0].reads_unmeasured, 1);
        assert_eq!(s.share(), share(1_000, 9_000));
    }

    /// No boundary means the query a reader has always run: no clause, no
    /// parameter. A marker is the only thing that can narrow a denominator.
    #[test]
    fn no_boundary_adds_no_clause_and_a_boundary_binds_one_parameter() {
        assert_eq!(window("recorded_at", None), (String::new(), Vec::new()));
        assert_eq!(
            window("recorded_at", Some(7)),
            (" WHERE recorded_at >= ?1".to_owned(), vec![Value::Int(7)])
        );
    }

    /// The window cuts BOTH sides of the fraction over the real relations: a
    /// boundary that dropped avoided tokens and kept the reads (or the other
    /// way about) would publish a share of two different periods.
    #[tokio::test]
    async fn the_boundary_cuts_the_numerator_and_the_denominator_alike() {
        let recall = fixture_decisions_either_side_of_2_000().await;
        assert_eq!(audit_totals(&recall, None).await.unwrap(), (200, 2, 0));
        assert_eq!(
            audit_totals(&recall, Some(2_000)).await.unwrap(),
            (100, 1, 0)
        );
        assert_eq!(audit_totals(&recall, Some(9_000)).await.unwrap(), (0, 0, 0));
        let whole = decisions_by_family(&recall, None).await.unwrap();
        assert_eq!(
            whole.get("claude-code"),
            Some(&FamilyDecisions {
                avoided: 200,
                returned: 2,
                rows: 2,
                prevented: 0,
            })
        );
        let cut = decisions_by_family(&recall, Some(2_000)).await.unwrap();
        assert_eq!(
            cut.get("claude-code"),
            Some(&FamilyDecisions {
                avoided: 100,
                returned: 1,
                rows: 1,
                prevented: 0,
            })
        );
        assert!(decisions_by_family(&recall, Some(9_000))
            .await
            .unwrap()
            .is_empty());
    }

    /// A recall store holding two read-gate decisions of 100 avoided tokens,
    /// one recorded at 1,000 and one at 3,000, each written to both relations
    /// the figure reads — the audit trail and the serve-call log.
    async fn fixture_decisions_either_side_of_2_000() -> rift_store::SqliteStore {
        let recall = rift_store::SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&recall, &rift_store::recall_migrations()).unwrap();
        for (i, at) in [(0usize, 1_000i64), (1, 3_000)] {
            let cols: &[&str] = &[
                "audit_id",
                "verb",
                "handle",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ];
            recall
                .insert(
                    "memd_audit_event",
                    cols,
                    &[
                        Value::text(format!("a{i}")),
                        Value::text("memd.read_intercepted"),
                        Value::text("h"),
                        Value::Int(1),
                        Value::Int(100),
                        Value::Int(at),
                        Value::Int(1),
                    ],
                )
                .await
                .unwrap();
            recall
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
                        Value::text(format!("c{i}")),
                        Value::text("hook_inject"),
                        Value::text("claude-code:read-gate"),
                        Value::text("memd.read_intercepted"),
                        Value::Int(1),
                        Value::Int(100),
                        Value::Int(at),
                        Value::Int(1),
                    ],
                )
                .await
                .unwrap();
        }
        recall
    }

    /// **A prevented read enters the avoided total under its own verb.** Both
    /// relations report it the same way and neither adds it a second time: the
    /// figure a reader compares is a part of the headline, not a rival to it.
    #[tokio::test]
    async fn a_prevented_row_is_a_separable_part_of_the_avoided_total() {
        let recall = rift_store::SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&recall, &rift_store::recall_migrations()).unwrap();
        for (i, (verb, avoided)) in [("memd.read_intercepted", 100i64), (PREVENTED_VERB, 400)]
            .iter()
            .enumerate()
        {
            recall
                .insert(
                    "memd_audit_event",
                    &[
                        "audit_id",
                        "verb",
                        "handle",
                        "tokens_returned",
                        "tokens_avoided",
                        "recorded_at",
                        "schema_version",
                    ],
                    &[
                        Value::text(format!("p{i}")),
                        Value::text(*verb),
                        Value::text("h"),
                        Value::Int(0),
                        Value::Int(*avoided),
                        Value::Int(1_000),
                        Value::Int(1),
                    ],
                )
                .await
                .unwrap();
            recall
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
                        Value::text(format!("q{i}")),
                        Value::text("hook_inject"),
                        Value::text("claude-code:read-gate"),
                        Value::text(*verb),
                        Value::Int(0),
                        Value::Int(*avoided),
                        Value::Int(1_000),
                        Value::Int(1),
                    ],
                )
                .await
                .unwrap();
        }
        let (avoided, _, prevented) = audit_totals(&recall, None).await.unwrap();
        assert_eq!(avoided, 500, "both rows are avoided tokens");
        assert_eq!(prevented, 400, "and this much of it never ran");
        let families = decisions_by_family(&recall, None).await.unwrap();
        let cc = families.get("claude-code").expect("the deciding family");
        assert_eq!(cc.avoided, 500);
        assert_eq!(cc.prevented, 400);
    }
    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("runtime")
            .block_on(f)
    }

    proptest::proptest! {
        /// ★ **The part never exceeds the whole.** `tokens_prevented` is a
        /// subtotal of `tokens_avoided` by construction — one pass, one set
        /// of rows, a `CASE` over the verb — and it has to stay one under any
        /// mix of verbs and figures, overall and per family. A breakdown that
        /// could exceed the number it breaks down would be a second claim
        /// wearing a subtotal's name.
        #[test]
        fn the_prevented_part_never_exceeds_the_avoided_whole(
            rows in proptest::collection::vec((proptest::bool::ANY, 0i64..5_000), 0..16)
        ) {
            let recall = rift_store::SqliteStore::open_in_memory().unwrap();
            rift_store::apply_migrations(&recall, &rift_store::recall_migrations()).unwrap();
            for (i, (never_ran, avoided)) in rows.iter().enumerate() {
                let verb = if *never_ran { PREVENTED_VERB } else { "memd.read_intercepted" };
                block(recall.insert(
                    "memd_audit_event",
                    &["audit_id", "verb", "handle", "tokens_returned", "tokens_avoided",
                      "recorded_at", "schema_version"],
                    &[Value::text(format!("a{i}")), Value::text(verb), Value::text("h"),
                      Value::Int(1), Value::Int(*avoided), Value::Int(1_000), Value::Int(1)],
                )).unwrap();
                block(recall.insert(
                    "serve_call_event",
                    &["call_id", "channel", "tool_name", "terminal_verb", "tokens_returned",
                      "tokens_avoided", "recorded_at", "schema_version"],
                    &[Value::text(format!("c{i}")), Value::text("hook_inject"),
                      Value::text("claude-code:read-gate"), Value::text(verb), Value::Int(1),
                      Value::Int(*avoided), Value::Int(1_000), Value::Int(1)],
                )).unwrap();
            }
            let (avoided, _, prevented) = block(audit_totals(&recall, None)).unwrap();
            proptest::prop_assert!(prevented <= avoided);
            let families = block(decisions_by_family(&recall, None)).unwrap();
            for decided in families.values() {
                proptest::prop_assert!(decided.prevented <= decided.avoided);
            }
            let composed = assemble(avoided, 0, prevented, &families, &BTreeMap::new(), &BTreeMap::new(), None);
            proptest::prop_assert!(composed.tokens_prevented <= composed.tokens_avoided);
            for f in &composed.families {
                proptest::prop_assert!(f.tokens_prevented <= f.tokens_avoided);
            }
        }
    }
}
