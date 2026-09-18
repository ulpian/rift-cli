//! `rift memory` — the human-facing recall surface.
//!
//! Every command reuses Recall's **transport-independent library dispatch**
//! ([`rift_recall_serve::ServeDispatcher`]): the same four-state envelopes, per-turn
//! budgets, and policy gates as the agent-facing `rift recall` verbs. This
//! human surface is another caller of the one bounded implementation.
//!
//! `reread-report` renders the honest, pre-optimization measurement: session count, repeated/avoidable rates, per-component and
//! per-session breakdowns, and the tokenizer provenance — then appends an
//! immutable metric snapshot so the measurement history is tamper-evident.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rift_agent::window_journal::{Interval, Line as JournalLine};
use rift_config::EffectiveConfig;
use rift_core::{Result, RiftError, Timestamp};
use rift_recall_ledger::metrics::{compute_metrics, record_metric_snapshot, ContextMetrics};
use rift_recall_ledger::savings::Savings;
use rift_recall_ledger::savings_window::{self, Boundary, Marker};
use rift_recall_policy::{PolicyEngine, ServePolicy, TurnBudget};
use rift_recall_serve::{
    RecallContext, ServeDispatcher, ToolError, ToolName, ToolRequest, ToolResponse,
};
use rift_recall_substrate::{
    worktree, GixAncestry, GixSubstrate, MemorySubstrate, SubstrateReader as _,
};
use rift_recall_types::{
    AncestryOracle, NoAncestry, NoProvenance, ProvenanceOracle, Scope, WorktreeScope,
};
use rift_store::{CanonicalStore, SqliteStore, Value};

use crate::cli::{exit, MemoryCmd, Outcome, SavingsArgs, SavingsCmd};

/// Route one `rift memory` subcommand to its handler. Repo-scoped: the target
/// is the WORKTREE ROOT discovered upward from `root` (a subdirectory
/// invocation must answer for the repo, never report a false-empty cwd).
pub(crate) fn run(cmd: &MemoryCmd, root: &Path) -> Result<Outcome> {
    let repo = crate::worktree_root(root);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)?;
    rt.block_on(dispatch(cmd, &repo))
}

async fn dispatch(cmd: &MemoryCmd, repo: &Path) -> Result<Outcome> {
    let family = crate::family::resolve_at(repo)?;
    let db = rift_store::recall_db_for_family(&family);
    let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())?;
    let sub = GixSubstrate::open(repo)?;

    // Answer on fresh state: the same ingest pass the launch auto-ingest and
    // the memory daemon run (watermark-idempotent).
    let paths = refresh_recall(&store, &sub, repo).await?;

    // Two kinds of question live behind this one command. The
    // three delta verbs SERVE a person standing in one checkout, so they are
    // contained to that checkout's line and say what they held back;
    // `reread-report` computes a FAMILY fact — a total that quietly dropped
    // every member but this one would be a number nobody could audit.
    match cmd {
        MemoryCmd::DiffSince(a) => {
            let (scope, ancestry) = serving_scope(&family, repo)?;
            diff_since(&store, repo, &scope, ancestry.as_ref(), &a.point).await
        }
        MemoryCmd::FileIfChanged(a) => {
            let (scope, ancestry) = serving_scope(&family, repo)?;
            file_if_changed(&store, repo, &scope, ancestry.as_ref(), &a.path).await
        }
        MemoryCmd::CostHotspots(a) => {
            let (scope, ancestry) = serving_scope(&family, repo)?;
            cost_hotspots(&store, repo, &scope, ancestry.as_ref(), a.top).await
        }
        MemoryCmd::RereadReport => {
            let scope = Scope::unbounded(family.recall_id.as_str());
            reread_report(&store, repo, &scope, &paths).await
        }
    }
}

/// The scope and oracle every **serving** path in this binary builds
/// the family's recall id, this checkout's own `HEAD`, and a
/// `gix` oracle to decide the branch axis against.
///
/// `HEAD` is read from the discovered `RepoPaths` as a file, never through a
/// `git` process (the 100 ms latency budget is shared with the hook path that
/// also calls this). Unreadable or unborn ⇒ `None`, which the predicate reads
/// as *exclude and report* — the one direction that cannot invent a saving.
///
/// The worktree axis stays [`WorktreeScope::Any`]: a summary computed in one
/// member must be servable in the next, which is the whole point of the family
/// sharing a store.
pub(crate) fn serving_scope(
    family: &rift_git::family::Family,
    repo: &Path,
) -> Result<(Scope, Box<dyn AncestryOracle>)> {
    let head = rift_git::discover(repo)
        .ok()
        .and_then(|paths| rift_git::head_state(&paths).oid);
    let scope = Scope::for_worktree(
        family.recall_id.as_str(),
        u32::MAX,
        head,
        WorktreeScope::Any,
    );
    let ancestry: Box<dyn AncestryOracle> = match GixAncestry::open(repo) {
        Ok(oracle) => Box::new(oracle),
        // A repository this process cannot open answers *unknown*, which
        // excludes and reports. Same direction, said out loud.
        Err(_) => Box::new(NoAncestry),
    };
    Ok((scope, ancestry))
}

/// The one recall ingest pass: blob-oid map first, then the
/// incremental graph ingest (already-projected checkpoints are skipped — the
/// watermark that makes launch-twice a no-op). Returns the read paths (the
/// worktree-hash inputs some callers reuse). Shared by `rift memory *`, the
/// dashboard's launch auto-ingest, and (via the same library calls) rift-memd.
pub(crate) async fn refresh_recall(
    store: &SqliteStore,
    sub: &GixSubstrate,
    repo: &Path,
) -> Result<std::collections::BTreeSet<String>> {
    // The ONE shared production pass: both front doors wrap the same
    // builder, so first-writer-wins can no longer freeze a divergent
    // projection (this door used to skip the developer axis fully).
    let pass = rift_recall_ledger::production::production_ingest(store, sub, repo).await?;
    Ok(pass.read_paths)
}

/// The dashboard's launch auto-ingest: one pass for
/// the launch-selected repository, **only when rift-total-recall is enabled
/// there and the repository is set up on this machine** — an unguarded
/// ingest would mint a family identity for a repository nobody initialized
/// here, create `recall.db` and silently flip the memory sections'
/// Disabled/Quiet distinction. Returns `Ok(false)` when either is not so
/// (nothing touched), `Ok(true)` after a completed pass.
pub(crate) fn launch_ingest(repo: &Path) -> Result<bool> {
    let cfg = EffectiveConfig::load(&repo.join(".rift"))
        .or_else(|_| EffectiveConfig::secure_default())?;
    if !cfg.recall_enabled() || !crate::family::provisioned_here(repo) {
        return Ok(false);
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)?;
    rt.block_on(async {
        let db = crate::family::recall_db(repo)?;
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())?;
        let sub = GixSubstrate::open(repo)?;
        refresh_recall(&store, &sub, repo).await?;
        Ok::<_, RiftError>(())
    })?;
    Ok(true)
}

/// Dispatch one tool call through the shared bounded surface: the same policy
/// gates (`.rift/policies/serve.toml` overlay, fail-closed on a malformed
/// config) and per-turn budget every serving channel applies. Shared with the
/// agent-facing `rift recall <verb>` skill surface, so both CLI
/// spellings are callers of one dispatcher, never two implementations.
///
/// The [`ToolError`] survives here: the skill surface reports the dispatcher's
/// own refusal tag and exit class, and only this human-facing wrapper flattens
/// it into a generic storage error.
pub(crate) async fn tool_call_raw(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
    provenance: &dyn ProvenanceOracle,
    tool: ToolName,
    req: &ToolRequest,
) -> std::result::Result<ToolResponse, ToolError> {
    let rift_dir = repo.join(".rift");
    let config = match EffectiveConfig::load(&rift_dir) {
        Ok(c) => c,
        Err(_) => {
            EffectiveConfig::secure_default().map_err(|e| ToolError::Internal(e.to_string()))?
        }
    };
    let policy = PolicyEngine::new(&config).with_serve_policy(ServePolicy::load(&rift_dir));
    let server = ServeDispatcher::new(policy);
    // THE READER IS THE REPOSITORY, for the verbs that can need bytes.
    //
    // A verb that serves a delta against the revision its caller holds has to
    // fetch that revision's blob and read the named file as it is now; an
    // empty in-memory substrate can do neither, so every such answer was
    // `basis_unavailable` however much the projection knew.
    //
    // NOTHING IS OPENED ON A HOOK PATH. This function has two callers: the CLI
    // surface (`rift memory <verb>` / `rift recall <verb>`), and the push
    // tier's `changed_since`, which asks exactly one verb — `diff_since`. That
    // verb is answered from the projection alone and says so
    // (`may_read_the_substrate`), so the hook path takes the second arm and
    // opens no repository, exactly as it did before. Nothing here is reachable
    // from the `PreToolUse` gate at all.
    //
    // What the reader opens is git, and git's objects for a linked worktree
    // live in the family's common directory — the same store every member
    // shares — so the basis a sibling member committed is reachable here. No
    // identity, cache or durable key is derived from the checkout path; the
    // projection this dispatch reads was already resolved family-first by the
    // caller. The checkout matters for exactly one thing, which is what "the
    // file as it is now" means to the caller who named it.
    //
    // Conservative when it cannot: a path that is not a readable repository
    // falls back to today's empty reader, and the verb answers
    // `basis_unavailable` rather than failing the call.
    let mut budget = TurnBudget::default();
    // The ladder's second rung, opened for the same verbs and under the same
    // rule: the family's own store, reached from the common directory every
    // member shares, so a revision a sibling's read recorded answers here too.
    // A checkout whose family cannot be proven from here simply has the rung
    // it always had.
    let snapshots = tool
        .may_read_the_substrate()
        .then(|| crate::hooks::basis_snapshots_at(repo))
        .flatten();
    let opened = tool
        .may_read_the_substrate()
        .then(|| GixSubstrate::open(repo).ok())
        .flatten();
    if let Some(reader) = opened {
        let ctx = RecallContext {
            store,
            reader: &reader,
            snapshots: snapshots.as_ref(),
            scope: scope.clone(),
            ancestry,
            provenance,
            now: Timestamp::now(),
        };
        return server.dispatch(tool, req, &ctx, &mut budget).await;
    }
    let reader = MemorySubstrate::local();
    let ctx = RecallContext {
        store,
        reader: &reader,
        snapshots: snapshots.as_ref(),
        scope: scope.clone(),
        ancestry,
        provenance,
        now: Timestamp::now(),
    };
    server.dispatch(tool, req, &ctx, &mut budget).await
}

/// [`tool_call_raw`] for the human surface: a dispatcher refusal becomes a
/// generic command failure.
async fn tool_call(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
    tool: ToolName,
    req: &ToolRequest,
) -> Result<ToolResponse> {
    tool_call_raw(store, repo, scope, ancestry, &NoProvenance, tool, req)
        .await
        .map_err(|e| RiftError::storage(format!("memory tool failed: {e:?}")))
}

/// `rift memory diff-since <point>`: everything that changed since a checkpoint
/// — the known set is what had been read *by* that checkpoint, so the answer is
/// exactly the delta an agent (or human) still needs to look at.
async fn diff_since(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
    point: &str,
) -> Result<Outcome> {
    // Resolve the point to a checkpoint (id or unique prefix).
    let rows = store
        .query(
            "SELECT node_id, ordinal FROM n_checkpoint WHERE node_id LIKE ?1 || '%' \
             ORDER BY node_id LIMIT 2",
            &[Value::text(point)],
        )
        .await?;
    let (ckpt, ordinal) = match rows.as_slice() {
        [only] => (only.get_text(0)?, only.get_i64(1)?),
        [] => {
            return Ok(Outcome::coded(
                exit::FAILURE,
                format!("no checkpoint matches '{point}'"),
                serde_json::json!({ "error": "unknown_checkpoint", "point": point }),
            ))
        }
        _ => {
            return Ok(Outcome::coded(
                exit::FAILURE,
                format!("'{point}' is ambiguous (matches multiple checkpoints)"),
                serde_json::json!({ "error": "ambiguous_checkpoint", "point": point }),
            ))
        }
    };

    // The hashes known as of that checkpoint (reads at ordinal ≤ point's).
    let known_rows = store
        .query(
            "SELECT DISTINCT fr.content_sha256 FROM n_file_read r \
             JOIN n_file_revision fr ON fr.node_id = r.file_revision \
             JOIN n_checkpoint c ON c.node_id = r.checkpoint_id \
             WHERE c.ordinal <= ?1",
            &[Value::Int(ordinal)],
        )
        .await?;
    let known_hashes: Vec<String> = known_rows
        .iter()
        .filter_map(|r| r.get_text(0).ok())
        .collect();

    let req = ToolRequest {
        known_hashes,
        ..ToolRequest::default()
    };
    let resp = tool_call(store, repo, scope, ancestry, ToolName::DiffSince, &req).await?;
    let changed = resp.payload.get("changed").cloned().unwrap_or_default();
    let unchanged = resp.payload.get("unchanged").cloned().unwrap_or_default();
    let n_changed = changed.as_array().map_or(0, Vec::len);
    let n_unchanged = unchanged.as_array().map_or(0, Vec::len);

    let mut human = format!("since {ckpt}: {n_changed} changed, {n_unchanged} unchanged\n");
    for c in changed.as_array().into_iter().flatten().take(50) {
        let path = c.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        human.push_str(&format!("  changed   {path}\n"));
    }
    for u in unchanged.as_array().into_iter().flatten().take(50) {
        human.push_str(&format!("  unchanged {}\n", u.as_str().unwrap_or("?")));
    }
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "since": ckpt,
            "changed": changed,
            "unchanged": unchanged,
            "estimated_tokens_avoided": resp.estimated_tokens_avoided,
        }),
    ))
}

/// `rift memory file-if-changed <path>`: does the worktree file differ from
/// what was last captured? The current worktree hash is the "known" hash — an
/// `unchanged` answer means a re-read would be pure waste.
async fn file_if_changed(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
    path: &str,
) -> Result<Outcome> {
    let mut one = BTreeSet::new();
    one.insert(path.to_string());
    let current = worktree::hash_paths(repo, &one);
    let req = ToolRequest {
        target: Some(path.to_string()),
        known_hashes: current.values().cloned().collect(),
        ..ToolRequest::default()
    };
    let resp = tool_call(store, repo, scope, ancestry, ToolName::FileIfChanged, &req).await?;
    let result = resp.payload.get("result").cloned().unwrap_or_default();
    let state = result
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let human = match state.as_str() {
        "unchanged" => format!("{path}: unchanged since last captured read (re-read avoidable)"),
        "changed" => format!("{path}: CHANGED since last captured read"),
        _ => format!("{path}: {state} (no captured read of this path)"),
    };
    Ok(Outcome::ok(
        human,
        serde_json::json!({ "path": path, "result": result }),
    ))
}

/// `rift memory cost-hotspots`: where the context tokens go.
async fn cost_hotspots(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
    top: u32,
) -> Result<Outcome> {
    let req = ToolRequest {
        top_n: Some(top),
        ..ToolRequest::default()
    };
    let resp = tool_call(store, repo, scope, ancestry, ToolName::CostHotspots, &req).await?;
    let hotspots = resp.payload.get("hotspots").cloned().unwrap_or_default();
    let mut human = format!("top {top} context-token hotspots:\n");
    for h in hotspots.as_array().into_iter().flatten() {
        let area = h.get("area").and_then(|v| v.as_str()).unwrap_or("?");
        let tokens = h
            .get("est_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let share = h
            .get("share")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        human.push_str(&format!(
            "  {tokens:>10} tok  {:>5.1}%  {area}\n",
            share * 100.0
        ));
    }
    if hotspots.as_array().is_none_or(Vec::is_empty) {
        human.push_str(&format!("  {}\n", empty_reads_diagnosis(repo, store).await));
    }
    Ok(Outcome::ok(
        human,
        serde_json::json!({ "hotspots": hotspots }),
    ))
}

/// Name the broken memory-chain link behind an empty read set: no checkpoints → not ingested → genuinely no reads — each with
/// its next command. An empty panel is a diagnosis, not a shrug.
pub(crate) async fn empty_reads_diagnosis(repo: &Path, store: &SqliteStore) -> String {
    let checkpoints = GixSubstrate::open(repo)
        .and_then(|sub| sub.checkpoints())
        .map_or(0, |c| c.len());
    if checkpoints == 0 {
        return "(no captured reads yet — this repository has no checkpoints; \
                commit with a live session, or see `rift doctor`)"
            .to_owned();
    }
    let ingested = store
        .query("SELECT COUNT(*) FROM n_file_read", &[])
        .await
        .ok()
        .and_then(|rows| rows.first().and_then(|r| r.get_i64(0).ok()))
        .unwrap_or(0);
    if ingested == 0 {
        return format!(
            "(no captured reads yet — {checkpoints} checkpoint(s) exist but carry no \
             read facts; sessions before the read-fact capture landed have none, \
             or see `rift doctor`)"
        );
    }
    "(no reads in scope)".to_owned()
}

// ----- the repeat classes (the pre-interception baseline) -------------------

/// One captured read, reduced to the five fields the classification needs.
///
/// The rows come from the FAMILY's capture projection — one store per
/// repository, shared by every member — so the classes are a family fact for
/// the same reason `reread-report`'s other numbers are: a total that dropped
/// every worktree but this one is a number nobody could audit. Nothing here
/// filters by checkout path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReadFact {
    /// The projected session the read belongs to.
    pub(crate) session_id: String,
    /// Repo-relative path read.
    pub(crate) path: String,
    /// Tool-call start (epoch ms) — the ordering axis.
    pub(crate) started_at: i64,
    /// The tool call, used only to break timestamp ties deterministically.
    pub(crate) tool_call_id: String,
    /// The revision identity a repeat is judged against: one immutable
    /// `(path, blob_oid)` fact, so two reads sharing it read the same bytes.
    pub(crate) file_revision_id: String,
    /// Measured token count over the read-time bytes, when one was captured.
    /// Reads recorded before the token-count relation existed have none; they
    /// are counted as reads and contribute nothing to the token column, and
    /// the report says how many there were.
    pub(crate) input_tokens: Option<u64>,
    /// The family whose session made the call, as the projection recorded it.
    pub(crate) agent: String,
    /// The tool that made it, as the family spells it.
    pub(crate) tool_name: String,
}

/// Whether this read arrived through a shell command rather than a read tool.
///
/// Decided by the PINNED tool table, not by a list of names retyped here: a
/// family's shell tool is whichever tool its own output pin gives the shell
/// shape. A family or a tool the table does not know is not a shell read —
/// the conservative direction, and the report publishes the unknown count
/// beside the share so nobody has to guess how much it hid.
fn is_shell_read(fact: &ReadFact) -> bool {
    is_shell_tool(&fact.agent, &fact.tool_name)
}

/// Whether `(family, tool)` is that family's shell, from the family's own
/// pinned table and never from a list of tool names retyped here.
///
/// Spelled once and used twice: by the reread report's classes, and by the
/// savings denominator, which splits the repository content a window was
/// handed between the read tool and the command line. A tool the pins do not
/// name is not a shell — the conservative direction, because the alternative
/// is a shell figure that grows every time a family adds a tool.
pub(crate) fn is_shell_tool(agent: &str, tool: &str) -> bool {
    rift_agent::tool_fields::tool_field_row(&rift_core::AgentId::new(agent.to_owned()), tool)
        .is_some_and(|row| row.shape == rift_agent::tool_fields::ToolShape::Shell)
}

/// Which of the three classes a read falls in. The three are exhaustive and
/// mutually exclusive by construction: a read either has no earlier read of
/// its path in its session (first), or it has one at the same revision
/// (unchanged repeat), or it has one only at a different revision (changed
/// repeat).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadClass {
    /// No earlier read of this path in this session.
    FirstRead,
    /// An earlier read of this path at the SAME revision exists.
    RereadUnchanged,
    /// Earlier reads of this path exist, all at OTHER revisions.
    RereadChanged,
}

/// Classify every read, returning one class per input in input order.
///
/// "Earlier" is strictly earlier by `started_at`, exactly as the measurement
/// query defines it: two reads recorded in the same millisecond are each
/// other's peers, not each other's priors, so both can be first reads. The
/// alternative — an arbitrary tie-break — would invent a repeat out of clock
/// granularity, and this report exists to be believed.
pub(crate) fn classify_reads(facts: &[ReadFact]) -> Vec<ReadClass> {
    // Group by (session, path): a repeat is only ever a repeat within one
    // session's context window, and only of the same path.
    let mut groups: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for (i, f) in facts.iter().enumerate() {
        groups
            .entry((f.session_id.as_str(), f.path.as_str()))
            .or_default()
            .push(i);
    }

    let mut out = vec![ReadClass::FirstRead; facts.len()];
    for idx in groups.values() {
        let mut ordered: Vec<usize> = idx.clone();
        ordered.sort_by(|&a, &b| {
            let (fa, fb) = (facts.get(a), facts.get(b));
            match (fa, fb) {
                (Some(x), Some(y)) => (x.started_at, x.tool_call_id.as_str())
                    .cmp(&(y.started_at, y.tool_call_id.as_str())),
                _ => std::cmp::Ordering::Equal,
            }
        });
        // Revisions read strictly before the fact under judgement. `pending`
        // holds the ones at the current timestamp, which are peers until the
        // clock moves.
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut pending: Vec<(i64, &str)> = Vec::new();
        for &i in &ordered {
            let Some(f) = facts.get(i) else { continue };
            let mut still: Vec<(i64, &str)> = Vec::new();
            for (at, rev) in pending.drain(..) {
                if at < f.started_at {
                    seen.insert(rev);
                } else {
                    still.push((at, rev));
                }
            }
            pending = still;
            let same_rev = seen.contains(f.file_revision_id.as_str());
            let other_rev = seen.len() > usize::from(same_rev);
            if let Some(slot) = out.get_mut(i) {
                *slot = if same_rev {
                    ReadClass::RereadUnchanged
                } else if other_rev {
                    ReadClass::RereadChanged
                } else {
                    ReadClass::FirstRead
                };
            }
            pending.push((f.started_at, f.file_revision_id.as_str()));
        }
    }
    out
}

/// Reads and tokens for one class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct ClassTotals {
    /// How many read calls fell in the class.
    pub(crate) reads: u64,
    /// Measured tokens over those reads (uncounted reads contribute 0).
    pub(crate) tokens: u64,
}

impl ClassTotals {
    fn add(&mut self, tokens: Option<u64>) {
        self.reads = self.reads.saturating_add(1);
        self.tokens = self.tokens.saturating_add(tokens.unwrap_or(0));
    }
}

/// One session's three classes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct SessionClasses {
    /// The projected session id.
    pub(crate) session_id: String,
    /// First reads in this session.
    pub(crate) first_read: ClassTotals,
    /// Unchanged repeats in this session.
    pub(crate) reread_unchanged: ClassTotals,
    /// Changed repeats in this session.
    pub(crate) reread_changed: ClassTotals,
}

/// The baseline the interception work is measured against: how the family's
/// captured reads partition into first reads and the two kinds of repeat.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct RepeatClasses {
    /// The capture projection these rows came from (named so the number can be
    /// re-derived by hand).
    pub(crate) projection: String,
    /// Sessions with at least one captured read.
    pub(crate) sessions: u64,
    /// First read of a path in a session.
    pub(crate) first_read: ClassTotals,
    /// Re-read of a revision the session had already read.
    pub(crate) reread_unchanged: ClassTotals,
    /// Re-read of a path the session had only read at other revisions.
    pub(crate) reread_changed: ClassTotals,
    /// Reads with no measured token count (counted as reads, worth 0 tokens).
    pub(crate) reads_without_token_counts: u64,
    /// Reads that arrived as a shell command rather than through a read tool.
    pub(crate) shell_reads: u64,
    /// Reads whose `(family, tool)` the pinned table does not know, and which
    /// are therefore counted as not-shell.
    pub(crate) reads_of_unknown_tools: u64,
    /// Per session, descending by tokens then session id.
    pub(crate) per_session: Vec<SessionClasses>,
    /// Why the numbers are absent, when they are.
    pub(crate) note: Option<String>,
}

impl RepeatClasses {
    /// Every read, regardless of class — the denominator of the read column.
    pub(crate) fn total_reads(&self) -> u64 {
        self.first_read
            .reads
            .saturating_add(self.reread_unchanged.reads)
            .saturating_add(self.reread_changed.reads)
    }

    /// Every measured token, regardless of class — the share denominator.
    pub(crate) fn total_tokens(&self) -> u64 {
        self.first_read
            .tokens
            .saturating_add(self.reread_unchanged.tokens)
            .saturating_add(self.reread_changed.tokens)
    }

    /// The addressable pool: both kinds of repeat together.
    pub(crate) fn repeats(&self) -> ClassTotals {
        ClassTotals {
            reads: self
                .reread_unchanged
                .reads
                .saturating_add(self.reread_changed.reads),
            tokens: self
                .reread_unchanged
                .tokens
                .saturating_add(self.reread_changed.tokens),
        }
    }

    /// An empty result carrying the reason it is empty.
    fn unavailable(projection: String, note: String) -> Self {
        Self {
            projection,
            sessions: 0,
            first_read: ClassTotals::default(),
            reread_unchanged: ClassTotals::default(),
            reread_changed: ClassTotals::default(),
            reads_without_token_counts: 0,
            shell_reads: 0,
            reads_of_unknown_tools: 0,
            per_session: Vec::new(),
            note: Some(note),
        }
    }
}

/// Aggregate classified reads into the report's rows.
pub(crate) fn repeat_classes(projection: String, facts: &[ReadFact]) -> RepeatClasses {
    let classes = classify_reads(facts);
    let mut totals = RepeatClasses {
        projection,
        sessions: 0,
        first_read: ClassTotals::default(),
        reread_unchanged: ClassTotals::default(),
        reread_changed: ClassTotals::default(),
        reads_without_token_counts: 0,
        shell_reads: 0,
        reads_of_unknown_tools: 0,
        per_session: Vec::new(),
        note: None,
    };
    let mut by_session: BTreeMap<&str, SessionClasses> = BTreeMap::new();
    for (i, f) in facts.iter().enumerate() {
        let Some(class) = classes.get(i).copied() else {
            continue;
        };
        if f.input_tokens.is_none() {
            totals.reads_without_token_counts = totals.reads_without_token_counts.saturating_add(1);
        }
        if is_shell_read(f) {
            totals.shell_reads = totals.shell_reads.saturating_add(1);
        } else if rift_agent::tool_fields::tool_field_row(
            &rift_core::AgentId::new(f.agent.clone()),
            &f.tool_name,
        )
        .is_none()
        {
            totals.reads_of_unknown_tools = totals.reads_of_unknown_tools.saturating_add(1);
        }
        let session = by_session
            .entry(f.session_id.as_str())
            .or_insert_with(|| SessionClasses {
                session_id: f.session_id.clone(),
                first_read: ClassTotals::default(),
                reread_unchanged: ClassTotals::default(),
                reread_changed: ClassTotals::default(),
            });
        match class {
            ReadClass::FirstRead => {
                totals.first_read.add(f.input_tokens);
                session.first_read.add(f.input_tokens);
            }
            ReadClass::RereadUnchanged => {
                totals.reread_unchanged.add(f.input_tokens);
                session.reread_unchanged.add(f.input_tokens);
            }
            ReadClass::RereadChanged => {
                totals.reread_changed.add(f.input_tokens);
                session.reread_changed.add(f.input_tokens);
            }
        }
    }
    totals.sessions = u64::try_from(by_session.len()).unwrap_or(u64::MAX);
    let mut per_session: Vec<SessionClasses> = by_session.into_values().collect();
    per_session.sort_by(|a, b| {
        let at = a.first_read.tokens + a.reread_unchanged.tokens + a.reread_changed.tokens;
        let bt = b.first_read.tokens + b.reread_unchanged.tokens + b.reread_changed.tokens;
        bt.cmp(&at).then_with(|| a.session_id.cmp(&b.session_id))
    });
    totals.per_session = per_session;
    totals
}

/// Read every captured read fact of this family, oldest first.
///
/// The joins are the measurement query's, unchanged: a read is a `tool_call`
/// with a resolved `tool_reads_file` link, attributed to its session through
/// its turn, with the token count LEFT joined because it is recorded late and
/// sparsely.
///
/// # Errors
/// [`RiftError::Storage`] on a query failure.
pub(crate) async fn read_facts(store: &SqliteStore) -> Result<Vec<ReadFact>> {
    let rows = store
        .query(
            "SELECT t.session_id, fr.path, tc.started_at, tc.tool_call_id, \
             fr.file_revision_id, frt.input_tokens, s.agent, tc.tool_name \
             FROM tool_call tc \
             JOIN turn t ON t.turn_id = tc.turn_id \
             JOIN tool_reads_file trf ON trf.tool_call_id = tc.tool_call_id \
             JOIN file_revision fr ON fr.file_revision_id = trf.file_revision_id \
             LEFT JOIN file_read_tokens frt ON frt.tool_call_id = tc.tool_call_id \
             LEFT JOIN session s ON s.session_id = t.session_id \
             ORDER BY t.session_id ASC, fr.path ASC, tc.started_at ASC, tc.tool_call_id ASC",
            &[],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(ReadFact {
            session_id: r.get_text(0)?,
            path: r.get_text(1)?,
            started_at: r.get_i64(2)?,
            tool_call_id: r.get_text(3)?,
            file_revision_id: r.get_text(4)?,
            input_tokens: r.get_opt_i64(5)?.and_then(|t| u64::try_from(t).ok()),
            // LEFT joined on purpose: a session row the projection has not
            // caught up with must not drop the read out of the classification.
            agent: r.get_text(6).unwrap_or_default(),
            tool_name: r.get_text(7)?,
        });
    }
    Ok(out)
}

/// The repeat classes for the family `repo` belongs to.
///
/// A projection this process cannot open is a NOTE, not a failure: the rest of
/// the reread report is computed from a different store and stays useful, and
/// a baseline that refuses to print is a baseline nobody records.
pub(crate) async fn family_repeat_classes(repo: &Path) -> RepeatClasses {
    let db = match crate::family::projection_db(repo) {
        Ok(db) => db,
        Err(e) => return RepeatClasses::unavailable("(unresolved)".to_owned(), e.to_string()),
    };
    let shown = db.display().to_string();
    if !db.exists() {
        return RepeatClasses::unavailable(
            shown,
            "no capture projection yet — nothing has been captured for this family".to_owned(),
        );
    }
    // Read-only: a measurement must not migrate the projection the owner's
    // live sessions are appending to, and a report is never worth a write.
    let store = match SqliteStore::open_read_only(&db) {
        Ok(store) => store,
        Err(e) => return RepeatClasses::unavailable(shown, e.to_string()),
    };
    match read_facts(&store).await {
        Ok(facts) => repeat_classes(shown, &facts),
        Err(e) => RepeatClasses::unavailable(shown, e.to_string()),
    }
}

/// One class row: name, reads, tokens, share of measured read tokens.
fn class_row(label: &str, t: ClassTotals, denominator: u64) -> String {
    let share = if denominator == 0 {
        0.0
    } else {
        (t.tokens as f64) * 100.0 / (denominator as f64)
    };
    format!(
        "  {label:<18}{:>8}{:>12}{:>9.1}%\n",
        t.reads, t.tokens, share
    )
}

/// Render the repeat-class block (golden-locked).
pub(crate) fn render_repeat_classes(c: &RepeatClasses) -> String {
    let mut out = String::new();
    out.push_str("repeat classes (capture projection):\n");
    out.push_str(&format!("  projection:            {}\n", c.projection));
    if let Some(note) = &c.note {
        out.push_str(&format!("  not measured:          {note}\n"));
        return out;
    }
    out.push_str(&format!("  sessions with reads:   {}\n", c.sessions));
    out.push_str(&format!("  reads classified:      {}\n", c.total_reads()));
    out.push_str(&format!(
        "  reads without counts:  {}\n",
        c.reads_without_token_counts
    ));
    let shell_share = if c.total_reads() == 0 {
        0.0
    } else {
        (c.shell_reads as f64) * 100.0 / (c.total_reads() as f64)
    };
    out.push_str(&format!(
        "  shell reads:           {} ({shell_share:.1}%, {} through a tool the pins do not name)\n",
        c.shell_reads, c.reads_of_unknown_tools
    ));
    let denominator = c.total_tokens();
    out.push_str("  class                reads      tokens    share\n");
    out.push_str(&class_row("first-read", c.first_read, denominator));
    out.push_str(&class_row(
        "reread-unchanged",
        c.reread_unchanged,
        denominator,
    ));
    out.push_str(&class_row("reread-changed", c.reread_changed, denominator));
    out.push_str(&class_row("total repeats", c.repeats(), denominator));
    out.push_str("per session:\n");
    if c.per_session.is_empty() {
        out.push_str("  (none)\n");
    }
    for s in &c.per_session {
        let short: String = s.session_id.chars().take(12).collect();
        out.push_str(&format!(
            "  {short:<12}  first {}  unchanged {}  changed {}\n",
            s.first_read.reads, s.reread_unchanged.reads, s.reread_changed.reads
        ));
    }
    out
}

// ----- the levers, as the two audit relations record them ------------------

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
pub(crate) struct LeverKind {
    /// The `memd.*` verb the rows carry.
    pub(crate) verb: String,
    /// How many rows there are.
    pub(crate) rows: u64,
    /// What those decisions' frames and injections cost.
    pub(crate) tokens_returned: u64,
    /// What those decisions avoided.
    pub(crate) tokens_avoided: u64,
}

/// One family's decisions, out of the relation that names a family.
///
/// `serve_call_event.tool_name` is `<family>:<hook step>`, which is the only
/// per-family axis either relation has. Its figures are ITS OWN: the two
/// relations key on different ids and are never joined, so a family's share of
/// a figure summed from the other relation is not available and is not
/// guessed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct FamilyLevers {
    /// The family, as the hook wrote it.
    pub(crate) family: String,
    /// The hook step that decided (`read-gate`, `recovery-map`, …).
    pub(crate) step: String,
    /// Rows for this `(family, step)`.
    pub(crate) rows: u64,
    /// What they returned, in this relation's own figures.
    pub(crate) tokens_returned: u64,
    /// What they avoided, in this relation's own figures.
    pub(crate) tokens_avoided: u64,
}

/// Pulls that followed an injection, per family.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct PullsAfterInjection {
    /// The family whose injection the pulls followed.
    pub(crate) family: String,
    /// Injections this family was handed.
    pub(crate) injections: u64,
    /// Skill-CLI calls inside [`PULL_AFTER_INJECTION_MS`] of one of them.
    pub(crate) pulls: u64,
}

/// What the audit relations say about every lever, for this family.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Levers {
    /// The recall projection the rows came from.
    pub(crate) recall_db: String,
    /// Why the numbers are absent, when they are.
    pub(crate) note: Option<String>,
    /// Every decision kind that has a row, by verb.
    pub(crate) kinds: Vec<LeverKind>,
    /// Σ avoided over the audit relation — the honesty gate's credit side.
    pub(crate) tokens_avoided: u64,
    /// Σ returned over the same relation — its spend side.
    pub(crate) tokens_returned: u64,
    /// The per-family split, in the other relation's own figures.
    pub(crate) families: Vec<FamilyLevers>,
    /// Pull-after-injection, per family.
    pub(crate) pulls: Vec<PullsAfterInjection>,
    /// Skill-CLI calls with no injection inside the span before them.
    pub(crate) pulls_without_injection: u64,
    /// Distinct `(path, revision)` pairs a read recorded.
    pub(crate) held_revisions: u64,
    /// Of those, the ones the blob map can reach as a git blob.
    pub(crate) held_with_blob: u64,
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
    pub(crate) fn net_tokens(&self) -> i64 {
        rift_recall_ledger::savings::honesty(self.tokens_avoided, self.tokens_returned)
    }

    /// The owner's figure: avoided ÷ (avoided + read), over everything since
    /// the projection was reset. `None` when neither side has a token in it —
    /// a share of nothing is not zero, it is unanswerable.
    pub(crate) fn savings_share(&self, read_tokens: u64) -> Option<f64> {
        share_of(self.tokens_avoided, read_tokens)
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
async fn channel_rows(
    store: &SqliteStore,
) -> Result<(Vec<FamilyLevers>, Vec<PullsAfterInjection>, u64)> {
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
        if channel == "hook_inject" {
            // Only a step that pushes something is an injection to pull after;
            // a decline is an answer to a call the agent already made.
            if step == "recovery-map" || step == "boundary" {
                let seen = pulls
                    .entry(family.clone())
                    .or_insert_with(|| PullsAfterInjection {
                        family: family.clone(),
                        injections: 0,
                        pulls: 0,
                    });
                seen.injections = seen.injections.saturating_add(1);
                last_injection = Some((family, at));
            }
            continue;
        }
        match last_injection.as_ref() {
            Some((injected_for, when)) if at.saturating_sub(*when) <= PULL_AFTER_INJECTION_MS => {
                let seen =
                    pulls
                        .entry(injected_for.clone())
                        .or_insert_with(|| PullsAfterInjection {
                            family: injected_for.clone(),
                            injections: 0,
                            pulls: 0,
                        });
                seen.pulls = seen.pulls.saturating_add(1);
            }
            _ => unattributed = unattributed.saturating_add(1),
        }
    }
    Ok((
        families.into_values().collect(),
        pulls.into_values().collect(),
        unattributed,
    ))
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
/// [`RiftError::Storage`] on a query failure.
pub(crate) async fn levers(store: &SqliteStore, recall_db: String) -> Result<Levers> {
    let kinds = audit_kinds(store).await?;
    let (families, pulls, pulls_without_injection) = channel_rows(store).await?;
    let (held_revisions, held_with_blob) = basis_availability(store).await?;
    let tokens_avoided = kinds.iter().map(|k| k.tokens_avoided).sum();
    let tokens_returned = kinds.iter().map(|k| k.tokens_returned).sum();
    Ok(Levers {
        recall_db,
        note: None,
        kinds,
        tokens_avoided,
        tokens_returned,
        families,
        pulls,
        pulls_without_injection,
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
pub(crate) async fn family_levers(repo: &Path) -> Levers {
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

/// Left-pad a label to `width` COLUMNS, not bytes.
///
/// Two labels here carry characters outside ASCII (`≥`, an en dash), and
/// `{:<n}` counts bytes: a table padded that way is crooked exactly where the
/// interesting rows are. Wide characters are not a case this table has, so
/// counting characters is the whole of it.
fn pad(label: &str, width: usize) -> String {
    let mut out = label.to_owned();
    for _ in label.chars().count()..width {
        out.push(' ');
    }
    out
}

/// Render the lever block (golden-locked).
pub(crate) fn render_levers(l: &Levers, read_tokens: u64) -> String {
    let mut out = String::new();
    out.push_str("levers (audit relations, everything recorded since the reset):\n");
    out.push_str(&format!("  recall projection:     {}\n", l.recall_db));
    if let Some(note) = &l.note {
        out.push_str(&format!("  not measured:          {note}\n"));
        return out;
    }
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
    let savings = l.savings_share(read_tokens).map_or_else(
        || "  savings: unanswerable — nothing was read and nothing was avoided\n".to_owned(),
        |share| {
            format!(
                "  savings: avoided ÷ (avoided + read) = {share:.1}% ({} ÷ {})\n",
                l.tokens_avoided,
                l.tokens_avoided.saturating_add(read_tokens)
            )
        },
    );
    out.push_str(&savings);
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
    out
}

// ----- what this member's own window journals say ---------------------------

/// How many turns after an eviction a read still counts as recovery.
///
/// Three: the turn the map arrives in, the turn that acts on it, and one
/// more. The first re-read after a compaction is deliberately allowed (the
/// window no longer holds the bytes), so the reading that follows an eviction
/// is recovery rather than repetition — and after three turns it is ordinary
/// work again, which is why the share is published with the number rather
/// than folded into a single "post-compaction" total.
const POST_COMPACTION_TURNS: u32 = 3;

/// The slice-length buckets the line floor is priced in: below the shipped
/// floor, and then the orders of magnitude above it.
const SLICE_LENGTH_BUCKETS: &[(&str, u32, u32)] = &[
    ("< 24 lines", 0, 23),
    ("24–99 lines", 24, 99),
    ("100–999 lines", 100, 999),
    ("≥ 1000 lines", 1000, u32::MAX),
];

/// The four slice-length buckets, all zero.
fn empty_buckets() -> Vec<SliceBucket> {
    SLICE_LENGTH_BUCKETS
        .iter()
        .map(|(label, _, _)| SliceBucket {
            label: (*label).to_owned(),
            reads: 0,
            tokens: 0,
            unmeasured: 0,
        })
        .collect()
}

/// The bucket a slice of `lines` lines falls in.
fn bucket_of(lines: u32) -> usize {
    SLICE_LENGTH_BUCKETS
        .iter()
        .position(|(_, lo, hi)| lines >= *lo && lines <= *hi)
        .unwrap_or(0)
}

/// One slice-length bucket of the would-have-declined pool.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct SliceBucket {
    /// The bucket, as the report prints it.
    pub(crate) label: String,
    /// Observed repeats whose slice falls in it.
    pub(crate) reads: u64,
    /// Their measured tokens.
    pub(crate) tokens: u64,
    /// Of those reads, the ones whose bytes could not be re-counted.
    pub(crate) unmeasured: u64,
}

/// Denies of one frame kind, and how many were asked for again.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct RetryAfterDeny {
    /// The frame the decline carried.
    pub(crate) frame: String,
    /// Declines of that shape.
    pub(crate) denies: u64,
    /// Of those, the ones re-issued inside the same turn.
    pub(crate) retried: u64,
}

/// What this member's window journals say about the window that is open.
///
/// The journals are per-member run state, not a family store: this is what one
/// checkout's context windows did. Windows are not sessions — a subagent has
/// its own journal under its parent's session id — so both are counted and
/// neither is reported as the other.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct WindowJournals {
    /// The directory the rows came from.
    pub(crate) dir: String,
    /// Why the numbers are absent, when they are.
    pub(crate) note: Option<String>,
    /// Context windows with a journal file.
    pub(crate) windows: u64,
    /// Lines across all of them.
    pub(crate) lines: u64,
    /// Read-shaped calls recorded.
    pub(crate) reads: u64,
    /// Their measured tokens.
    pub(crate) read_tokens: u64,
    /// Reads whose bytes could not be re-counted under the identity the
    /// journal named them by.
    pub(crate) reads_unmeasured: u64,
    /// Reads whose slice is past the cap, and so were never eligible for a
    /// decline at all.
    pub(crate) reads_over_cap: u64,
    /// The cap they were judged against.
    pub(crate) max_slice_bytes: u64,
    /// The line floor the pool below is judged against.
    pub(crate) min_slice_lines: u32,
    /// Repeats the gate would have declined and did not.
    pub(crate) pool_reads: u64,
    /// Their measured tokens.
    pub(crate) pool_tokens: u64,
    /// Of those, the ones whose bytes could not be re-counted.
    pub(crate) pool_unmeasured: u64,
    /// The pool by slice length — the distribution the floor is priced from.
    pub(crate) buckets: Vec<SliceBucket>,
    /// Declines, by frame, with the repeats that followed them.
    pub(crate) retries: Vec<RetryAfterDeny>,
    /// Consecutive reads of one path whose ranges abut.
    pub(crate) adjacent_reads: u64,
    /// Evictions recorded.
    pub(crate) evictions: u64,
    /// Read tokens inside [`POST_COMPACTION_TURNS`] of one.
    pub(crate) post_compaction_tokens: u64,
}

impl WindowJournals {
    /// An empty result carrying the reason it is empty.
    fn unavailable(dir: String, note: String) -> Self {
        Self {
            dir,
            note: Some(note),
            windows: 0,
            lines: 0,
            reads: 0,
            read_tokens: 0,
            reads_unmeasured: 0,
            reads_over_cap: 0,
            max_slice_bytes: 0,
            min_slice_lines: 0,
            pool_reads: 0,
            pool_tokens: 0,
            pool_unmeasured: 0,
            buckets: Vec::new(),
            retries: Vec::new(),
            adjacent_reads: 0,
            evictions: 0,
            post_compaction_tokens: 0,
        }
    }
}

/// The bytes a journal line named, re-counted under the identity it named them
/// by — or nothing.
///
/// The working file is the only source consulted, and only while it still
/// hashes to the revision the line recorded. A file edited since is NOT
/// re-counted under the new bytes: that would price a slice nobody read. Such
/// a read is counted and declared unmeasured, never imputed — the posture the
/// serving audit takes with the same problem.
struct SliceMeter {
    work_tree: PathBuf,
    /// `(path, revision)` → the file's text, or `None` once it is known not to
    /// hash to that revision any more.
    seen: BTreeMap<(String, String), Option<String>>,
}

impl SliceMeter {
    fn new(work_tree: &Path) -> Self {
        Self {
            work_tree: work_tree.to_path_buf(),
            seen: BTreeMap::new(),
        }
    }

    /// The text of `path` at `revision`, read at most once per pair.
    fn text(&mut self, path: &str, revision: &str) -> Option<&String> {
        let key = (path.to_owned(), revision.to_owned());
        if !self.seen.contains_key(&key) {
            let full = self.work_tree.join(path);
            let held = std::fs::read(&full).ok().and_then(|bytes| {
                (rift_agent::content_sha256(&bytes).to_hex() == revision)
                    .then(|| String::from_utf8_lossy(&bytes).into_owned())
            });
            self.seen.insert(key.clone(), held);
        }
        self.seen.get(&key).and_then(Option::as_ref)
    }

    /// `(bytes, tokens)` of the run of lines a line named, or `None` when the
    /// bytes are no longer the bytes it named.
    fn measure(&mut self, path: &str, revision: &str, range: Interval) -> Option<(u64, u64)> {
        let text = self.text(path, revision)?;
        let slice: String = text
            .split_inclusive('\n')
            .skip(range.lo as usize - 1)
            .take(range.lines() as usize)
            .collect();
        Some((
            u64::try_from(slice.len()).unwrap_or(0),
            u64::from(rift_tokens::count(&slice)),
        ))
    }
}

/// Which frame a decline carried, worked out from the lines that came before
/// it — the same two facts the decision itself is made of.
///
/// A revision the window read is a plain decline; a revision it holds only by
/// having written it is a delta decline. Anything else is a decline this
/// reader cannot name, and it says so rather than guessing.
fn frame_before(lines: &[JournalLine], path: &str, revision: &str) -> &'static str {
    let mut frame = "unknown";
    for line in lines {
        match line {
            JournalLine::Read {
                path: p, hash: h, ..
            } if p == path && h.to_hex() == revision => frame = "plain",
            JournalLine::Edit {
                path: p,
                hash_new: h,
                ..
            } if p == path && h.to_hex() == revision && frame == "unknown" => frame = "delta",
            _ => {}
        }
    }
    frame
}

/// Fold one window's lines into the running figures.
#[allow(clippy::too_many_lines)]
fn fold_window(lines: &[JournalLine], meter: &mut SliceMeter, out: &mut WindowJournals) {
    let mut retries: BTreeMap<&'static str, (u64, u64)> = BTreeMap::new();
    let mut last_read: Option<(String, Interval)> = None;
    let mut evicted_at: Option<u32> = None;
    for (i, line) in lines.iter().enumerate() {
        out.lines = out.lines.saturating_add(1);
        match line {
            JournalLine::Read {
                turn,
                path,
                hash,
                range,
                ..
            } => {
                out.reads = out.reads.saturating_add(1);
                match meter.measure(path, &hash.to_hex(), *range) {
                    Some((bytes, tokens)) => {
                        out.read_tokens = out.read_tokens.saturating_add(tokens);
                        if bytes > out.max_slice_bytes {
                            out.reads_over_cap = out.reads_over_cap.saturating_add(1);
                        }
                        if evicted_at
                            .is_some_and(|at| *turn >= at && *turn <= at + POST_COMPACTION_TURNS)
                        {
                            out.post_compaction_tokens =
                                out.post_compaction_tokens.saturating_add(tokens);
                        }
                    }
                    None => out.reads_unmeasured = out.reads_unmeasured.saturating_add(1),
                }
                if last_read
                    .as_ref()
                    .is_some_and(|(p, prev)| p == path && range.lo == prev.hi.saturating_add(1))
                {
                    out.adjacent_reads = out.adjacent_reads.saturating_add(1);
                }
                last_read = Some((path.clone(), *range));
            }
            JournalLine::Observe {
                path, hash, range, ..
            } => {
                out.pool_reads = out.pool_reads.saturating_add(1);
                let measured = meter.measure(path, &hash.to_hex(), *range);
                if let Some(slot) = out.buckets.get_mut(bucket_of(range.lines())) {
                    slot.reads = slot.reads.saturating_add(1);
                    match measured {
                        Some((_, tokens)) => slot.tokens = slot.tokens.saturating_add(tokens),
                        None => slot.unmeasured = slot.unmeasured.saturating_add(1),
                    }
                }
                match measured {
                    Some((_, tokens)) => out.pool_tokens = out.pool_tokens.saturating_add(tokens),
                    None => out.pool_unmeasured = out.pool_unmeasured.saturating_add(1),
                }
            }
            JournalLine::Deny {
                turn,
                path,
                hash,
                range,
                ..
            } => {
                let revision = hash.to_hex();
                let frame = frame_before(lines.get(..i).unwrap_or_default(), path, &revision);
                let entry = retries.entry(frame).or_insert((0, 0));
                entry.0 = entry.0.saturating_add(1);
                // The repeat the decline bounds: the same key, asked for again
                // inside the same turn. A later turn coming back to the file is
                // ordinary work, not a retry of a decline.
                let asked_again = lines.iter().skip(i + 1).any(|later| match later {
                    JournalLine::Read {
                        turn: t,
                        path: p,
                        hash: h,
                        range: r,
                        ..
                    } => t == turn && p == path && h.to_hex() == revision && r == range,
                    _ => false,
                });
                if asked_again {
                    entry.1 = entry.1.saturating_add(1);
                }
            }
            JournalLine::Evict { turn, .. } => {
                out.evictions = out.evictions.saturating_add(1);
                evicted_at = Some(*turn);
            }
            // Neither is a read, a decline or an emptying: an announcement
            // and its booking are counted where the savings are, not in a
            // report about the reads that happened.
            JournalLine::Expect { .. }
            | JournalLine::Edit { .. }
            | JournalLine::Hold { .. }
            | JournalLine::Prevented { .. } => {}
        }
    }
    for (frame, (denies, retried)) in retries {
        match out.retries.iter_mut().find(|r| r.frame == frame) {
            Some(row) => {
                row.denies = row.denies.saturating_add(denies);
                row.retried = row.retried.saturating_add(retried);
            }
            None => out.retries.push(RetryAfterDeny {
                frame: frame.to_owned(),
                denies,
                retried,
            }),
        }
    }
}

/// Read every window journal this member holds and fold them into one account.
///
/// Only the append files are read — the live one and the rolled ones. A
/// snapshot is a fold of rolled files the decision path uses to answer
/// quickly; reading the rows themselves is what a report wants, and it cannot
/// double-count a row it never reads twice.
pub(crate) fn window_journals(
    work_tree: &Path,
    max_slice_bytes: u64,
    min_slice_lines: u32,
) -> WindowJournals {
    let dir = crate::window_journal::dir(work_tree);
    let shown = dir.display().to_string();
    // A directory this member cannot read is a NOTE with the reason, not a
    // silent zero: an empty pool and an unreadable one are different facts.
    if let Err(e) = std::fs::read_dir(&dir) {
        return WindowJournals::unavailable(shown, e.to_string());
    }
    let mut out = WindowJournals::unavailable(shown, String::new());
    out.note = None;
    out.max_slice_bytes = max_slice_bytes;
    out.min_slice_lines = min_slice_lines;
    out.buckets = empty_buckets();
    let mut meter = SliceMeter::new(work_tree);
    for lines in member_windows(&dir) {
        out.windows = out.windows.saturating_add(1);
        fold_window(&lines, &mut meter, &mut out);
    }
    out
}

/// Every context window this member holds, as its lines in file order.
///
/// One window is one key however many files it has rolled through, and only
/// the append files are read: a snapshot is a fold of rows this pass reads for
/// itself, so a pass that read both would count them twice.
fn member_windows(dir: &Path) -> Vec<Vec<JournalLine>> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut keyed: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if Path::new(&name).extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let Some(key) = name.split('.').next() else {
            continue;
        };
        keyed.entry(key.to_owned()).or_default().push(entry.path());
    }
    let mut out = Vec::with_capacity(keyed.len());
    for (_, mut files) in keyed {
        files.sort();
        let mut lines: Vec<JournalLine> = Vec::new();
        for file in &files {
            if let Ok(text) = std::fs::read_to_string(file) {
                lines.extend(
                    rift_agent::window_journal::Journal::parse(&text)
                        .lines()
                        .to_vec(),
                );
            }
        }
        out.push(lines);
    }
    out
}

/// The window journals of the member `repo` is, judged against the caps this
/// repository's own policy resolves for Claude Code.
pub(crate) fn member_window_journals(repo: &Path) -> WindowJournals {
    let policy = rift_recall_policy::ServePolicy::load(&repo.join(".rift"));
    let agent = rift_core::AgentId::CLAUDE_CODE;
    let effective = rift_recall_policy::resolve(
        &policy,
        agent.as_str(),
        crate::recall::family_capability(&agent),
    );
    window_journals(repo, effective.max_slice_bytes, effective.min_slice_lines)
}

/// Render the window-journal block (golden-locked).
#[allow(clippy::too_many_lines)]
pub(crate) fn render_window_journals(w: &WindowJournals) -> String {
    let mut out = String::new();
    out.push_str("window journals (this member's own run state):\n");
    out.push_str(&format!("  directory:             {}\n", w.dir));
    if let Some(note) = &w.note {
        out.push_str(&format!("  not measured:          {note}\n"));
        return out;
    }
    out.push_str(&format!("  windows:               {}\n", w.windows));
    out.push_str(&format!("  lines:                 {}\n", w.lines));
    out.push_str(&format!(
        "  reads:                 {} ({} tok, {} unmeasured)\n",
        w.reads, w.read_tokens, w.reads_unmeasured
    ));
    out.push_str(&format!(
        "  past the {} B cap:  {} (never eligible for a decline)\n",
        w.max_slice_bytes, w.reads_over_cap
    ));
    out.push_str(&format!(
        "  would-have-declined:   {} reads, {} tok ({} unmeasured), floor {} lines\n",
        w.pool_reads, w.pool_tokens, w.pool_unmeasured, w.min_slice_lines
    ));
    out.push_str(&format!(
        "  {}{:>8}{:>12}{:>12}\n",
        pad("by slice length", 20),
        "reads",
        "tokens",
        "unmeasured"
    ));
    for b in &w.buckets {
        out.push_str(&format!(
            "    {}{:>8}{:>12}{:>12}\n",
            pad(&b.label, 18),
            b.reads,
            b.tokens,
            b.unmeasured
        ));
    }
    out.push_str(&format!(
        "  {}{:>8}{:>12}\n",
        pad("retry after decline", 22),
        "declines",
        "retried"
    ));
    if w.retries.is_empty() {
        out.push_str("    (no decline was recorded in this window)\n");
    }
    for r in &w.retries {
        let share = if r.denies == 0 {
            0.0
        } else {
            (r.retried as f64) * 100.0 / (r.denies as f64)
        };
        out.push_str(&format!(
            "    {}{:>8}{:>12}  {share:.1}%\n",
            pad(&r.frame, 20),
            r.denies,
            r.retried
        ));
    }
    out.push_str(&format!("  range-walking:         {}\n", w.adjacent_reads));
    let share = if w.read_tokens == 0 {
        0.0
    } else {
        (w.post_compaction_tokens as f64) * 100.0 / (w.read_tokens as f64)
    };
    out.push_str(&format!("  evictions:             {}\n", w.evictions));
    out.push_str(&format!(
        "  read within {POST_COMPACTION_TURNS} turns of one: {} of {} tok ({share:.1}%)\n",
        w.post_compaction_tokens, w.read_tokens
    ));
    out
}

// ----- the two windows: before the first booked decline, and after ----------

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
pub(crate) struct FamilyWindow {
    /// The family, as the relation that named it spells it.
    pub(crate) family: String,
    /// Sessions the capture projection started inside the window.
    pub(crate) sessions: u64,
    /// Distinct client-side ids behind them: fewer than `sessions` means the
    /// family folded several of its threads under one id.
    pub(crate) native_ids: u64,
    /// Of those sessions, the ones that read a file.
    pub(crate) sessions_with_reads: u64,
    /// Reads with no earlier read of the path in their session.
    pub(crate) first_read: ClassTotals,
    /// Reads of a revision their session had already read.
    pub(crate) reread_unchanged: ClassTotals,
    /// Reads of a path their session had read at another revision.
    pub(crate) reread_changed: ClassTotals,
    /// Declines booked for this family.
    pub(crate) declines: u64,
    /// Bounded replacements delivered to it.
    pub(crate) replacements: u64,
    /// Replacements that could not be delivered and fell back.
    pub(crate) fallbacks: u64,
    /// What its decisions avoided, in the serving relation's own figures.
    pub(crate) tokens_avoided: u64,
    /// What they returned, in the same relation's own figures.
    pub(crate) tokens_returned: u64,
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
    pub(crate) fn reads(&self) -> u64 {
        self.first_read
            .reads
            .saturating_add(self.reread_unchanged.reads)
            .saturating_add(self.reread_changed.reads)
    }

    /// Measured read tokens over the three classes.
    pub(crate) fn read_tokens(&self) -> u64 {
        self.first_read
            .tokens
            .saturating_add(self.reread_unchanged.tokens)
            .saturating_add(self.reread_changed.tokens)
    }

    /// The owner's figure for this family: avoided ÷ (avoided + read), the
    /// avoided side from the serving relation and the read side from the
    /// capture projection. `None` when neither side holds a token.
    pub(crate) fn savings_share(&self) -> Option<f64> {
        share_of(self.tokens_avoided, self.read_tokens())
    }
}

/// `avoided ÷ (avoided + read)` as a percentage, or nothing when the
/// denominator is empty: a share of nothing is unanswerable, not zero.
///
/// One computation, one place: the report, the `savings` verb, the recall
/// status table and the dashboard all reach the same function, so the figure
/// the owner judges cannot differ between the surface it is read on.
fn share_of(avoided: u64, read: u64) -> Option<f64> {
    rift_recall_ledger::savings::share(avoided, read)
}

/// One of the two windows: every figure the relations and this member's
/// journals hold between its bounds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct WindowPane {
    /// The per-family rows.
    pub(crate) families: Vec<FamilyWindow>,
    /// Σ avoided over the AUDIT relation inside the window — the relation the
    /// whole-report honesty figure is summed from, so the two agree.
    pub(crate) tokens_avoided: u64,
    /// Σ returned over that relation inside the window.
    pub(crate) tokens_returned: u64,
    /// Context windows of THIS member with a journal line inside it.
    pub(crate) windows: u64,
    /// Journal lines inside it.
    pub(crate) lines: u64,
    /// Read-shaped calls among them.
    pub(crate) reads: u64,
    /// Repeats the gate would have declined and did not.
    pub(crate) observed: u64,
    /// Repeats it declined.
    pub(crate) declined: u64,
    /// Declines by frame, with the raw retries that followed them.
    pub(crate) retries: Vec<RetryAfterDeny>,
    /// The DECLINED slices by length: under enforce the would-have-declined
    /// pool no longer exists, so this is the distribution the floor ruling is
    /// evaluated against in its place.
    pub(crate) declined_buckets: Vec<SliceBucket>,
}

impl WindowPane {
    /// An empty window.
    fn new() -> Self {
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
    pub(crate) fn read_tokens(&self) -> u64 {
        self.families
            .iter()
            .fold(0u64, |acc, f| acc.saturating_add(f.read_tokens()))
    }

    /// The honesty gate for the window, over the audit relation alone.
    pub(crate) fn net_tokens(&self) -> i64 {
        rift_recall_ledger::savings::honesty(self.tokens_avoided, self.tokens_returned)
    }

    /// The owner's figure for the window: avoided ÷ (avoided + read).
    pub(crate) fn savings_share(&self) -> Option<f64> {
        share_of(self.tokens_avoided, self.read_tokens())
    }
}

/// The post-reset data cut in two at the first booked decline.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub(crate) struct EnforceSplit {
    /// The cut, in epoch milliseconds, or `None` when nothing was ever
    /// declined and the enforce window is empty by construction.
    pub(crate) boundary_ms: Option<i64>,
    /// Where the cut came from, printed beside it.
    pub(crate) boundary_source: String,
    /// Why the figures are absent, when they are.
    pub(crate) note: Option<String>,
    /// Everything before the cut.
    pub(crate) observe: WindowPane,
    /// Everything from the cut on.
    pub(crate) enforce: WindowPane,
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
/// [`RiftError::Storage`] on a query failure.
pub(crate) async fn session_facts(store: &SqliteStore) -> Result<Vec<(String, i64, String)>> {
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
/// [`RiftError::Storage`] on a query failure.
pub(crate) async fn serve_facts(
    store: &SqliteStore,
) -> Result<Vec<(String, String, i64, u64, u64)>> {
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
/// [`RiftError::Storage`] on a query failure.
pub(crate) async fn audit_facts(store: &SqliteStore) -> Result<Vec<(String, i64, u64, u64)>> {
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
pub(crate) fn enforce_boundary(audit: &[(String, i64, u64, u64)]) -> Option<i64> {
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
pub(crate) fn build_pane(
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

/// When a journal line was written, in epoch milliseconds.
fn line_at_ms(line: &JournalLine) -> i64 {
    let ms = match line {
        JournalLine::Read { at_ms, .. }
        | JournalLine::Deny { at_ms, .. }
        | JournalLine::Observe { at_ms, .. }
        | JournalLine::Expect { at_ms, .. }
        | JournalLine::Edit { at_ms, .. }
        | JournalLine::Hold { at_ms, .. }
        | JournalLine::Prevented { at_ms, .. }
        | JournalLine::Evict { at_ms, .. } => *at_ms,
    };
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// Fold this member's journal lines into the two windows.
///
/// A decline's frame and its retry are decided over the WHOLE window's lines,
/// not the half the cut left it: a decline is judged by everything its window
/// had read before it, whichever side of the boundary that reading was on.
pub(crate) fn fold_split_journals(
    work_tree: &Path,
    boundary_ms: Option<i64>,
    observe: &mut WindowPane,
    enforce: &mut WindowPane,
) {
    let cut = boundary_ms.unwrap_or(i64::MAX);
    let mut meter = SliceMeter::new(work_tree);
    for lines in member_windows(&crate::window_journal::dir(work_tree)) {
        let (mut in_observe, mut in_enforce) = (false, false);
        let mut retries: BTreeMap<(bool, &'static str), (u64, u64)> = BTreeMap::new();
        for (i, line) in lines.iter().enumerate() {
            let enforced = line_at_ms(line) >= cut;
            if enforced {
                in_enforce = true;
            } else {
                in_observe = true;
            }
            let pane: &mut WindowPane = if enforced {
                &mut *enforce
            } else {
                &mut *observe
            };
            pane.lines = pane.lines.saturating_add(1);
            match line {
                JournalLine::Read { .. } => pane.reads = pane.reads.saturating_add(1),
                JournalLine::Observe { .. } => pane.observed = pane.observed.saturating_add(1),
                JournalLine::Deny {
                    turn,
                    path,
                    hash,
                    range,
                    ..
                } => {
                    pane.declined = pane.declined.saturating_add(1);
                    let revision = hash.to_hex();
                    let measured = meter.measure(path, &revision, *range);
                    if let Some(slot) = pane.declined_buckets.get_mut(bucket_of(range.lines())) {
                        slot.reads = slot.reads.saturating_add(1);
                        match measured {
                            Some((_, tokens)) => slot.tokens = slot.tokens.saturating_add(tokens),
                            None => slot.unmeasured = slot.unmeasured.saturating_add(1),
                        }
                    }
                    let frame = frame_before(lines.get(..i).unwrap_or_default(), path, &revision);
                    let entry = retries.entry((enforced, frame)).or_insert((0, 0));
                    entry.0 = entry.0.saturating_add(1);
                    // The repeat the decline bounds: the same key, asked for
                    // again inside the same turn — the fail-open raw retry.
                    let asked_again = lines.iter().skip(i + 1).any(|later| match later {
                        JournalLine::Read {
                            turn: t,
                            path: p,
                            hash: h,
                            range: r,
                            ..
                        } => t == turn && p == path && h.to_hex() == revision && r == range,
                        _ => false,
                    });
                    if asked_again {
                        entry.1 = entry.1.saturating_add(1);
                    }
                }
                JournalLine::Expect { .. }
                | JournalLine::Edit { .. }
                | JournalLine::Evict { .. }
                | JournalLine::Hold { .. }
                | JournalLine::Prevented { .. } => {}
            }
        }
        if in_observe {
            observe.windows = observe.windows.saturating_add(1);
        }
        if in_enforce {
            enforce.windows = enforce.windows.saturating_add(1);
        }
        for ((enforced, frame), (denies, retried)) in retries {
            let pane: &mut WindowPane = if enforced {
                &mut *enforce
            } else {
                &mut *observe
            };
            match pane.retries.iter_mut().find(|r| r.frame == frame) {
                Some(existing) => {
                    existing.denies = existing.denies.saturating_add(denies);
                    existing.retried = existing.retried.saturating_add(retried);
                }
                None => pane.retries.push(RetryAfterDeny {
                    frame: frame.to_owned(),
                    denies,
                    retried,
                }),
            }
        }
    }
}

/// The two windows for the family `repo` belongs to.
pub(crate) async fn family_enforce_split(repo: &Path) -> EnforceSplit {
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
#[allow(clippy::too_many_lines)]
fn render_pane(label: &str, p: &WindowPane) -> String {
    let mut out = String::new();
    out.push_str(&format!("  {label} window:\n"));
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
        let share = f
            .savings_share()
            .map_or_else(|| "n/a".to_owned(), |s| format!("{s:.1}%"));
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
    out.push_str(&format!(
        "    honesty (audit relation): avoided {} − returned {} = {}\n",
        p.tokens_avoided,
        p.tokens_returned,
        p.net_tokens()
    ));
    let share = p
        .savings_share()
        .map_or_else(|| "n/a".to_owned(), |s| format!("{s:.1}%"));
    out.push_str(&format!(
        "    savings: avoided ÷ (avoided + read) = {share} ({} ÷ {})\n",
        p.tokens_avoided,
        p.tokens_avoided.saturating_add(p.read_tokens())
    ));
    out.push_str(&format!(
        "    journals: {} windows, {} lines, {} reads, {} observed, {} declined\n",
        p.windows, p.lines, p.reads, p.observed, p.declined
    ));
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
    out
}

/// Render the split block (golden-locked).
pub(crate) fn render_enforce_split(s: &EnforceSplit) -> String {
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

// --- `rift savings` -------------------------------------------------------
//
// The owner's evaluation figure, on its own verb so it can be read without
// running (and appending to) the whole reread report. Every number here is
// composed by `rift_recall_ledger::savings`, which the report, the recall
// status table and the dashboard call as well: four surfaces, one computation.

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

/// Compose the savings figure for the family `repo` belongs to.
///
/// Read-only on both stores, for the reason every measurement here is: a
/// figure that migrated the projection it measured would be measuring itself.
/// Which window one call asks for — the input [`family_savings`] resolves
/// against the family's marker.
///
/// It is three cases rather than an `Option<i64>` because the third composes
/// with the marker instead of replacing it, and a bare instant cannot say
/// which of the two it is. The clock is read by the caller, never here: a
/// measurement that sampled the wall clock could not be tested twice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum WindowRequest {
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

pub(crate) async fn family_savings(repo: &Path, request: WindowRequest) -> SavingsReport {
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

/// A share as the surfaces spell it, or the report's own word for a question
/// with no answer yet (`n/a`, as every other block here already writes it).
pub(crate) fn fmt_share(share: Option<f64>) -> String {
    share.map_or_else(|| "n/a".to_owned(), |s| format!("{s:.1}%"))
}

/// Render the savings figure (golden-locked).
pub(crate) fn render_savings(r: &SavingsReport) -> String {
    let s = &r.savings;
    let mut out = String::new();
    out.push_str("savings — avoided ÷ (avoided + read), over the window named below\n");
    out.push_str(&format!("  capture projection:    {}\n", r.projection_db));
    out.push_str(&format!("  recall projection:     {}\n", r.recall_db));
    if let Some(note) = &r.note {
        out.push_str(&format!("  not measured:          {note}\n"));
        return out;
    }
    out.push_str(&format!("  window:                {}\n", r.window.label()));
    out.push_str(&format!(
        "  observe/enforce cut:   {} ms — {}\n",
        r.enforce_cut_ms.unwrap_or_default(),
        r.enforce_cut_source
    ));
    // The session counts come from that cut, which spans the whole record; a
    // window narrower than the record must say so beside them rather than let
    // a reader pair "12 enforce sessions" with a figure that covers two.
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
    render_family_rows(s, &mut out);
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
    out
}

/// The per-family table of [`render_savings`]: one row per family the
/// channel relation names, joined to the projection by name.
fn render_family_rows(s: &rift_recall_ledger::savings::Savings, out: &mut String) {
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
    Ok(Outcome::ok(
        render_savings(&report).trim_end().to_owned(),
        savings_json(&report),
    ))
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
pub(crate) fn savings_report_for(root: &Path, request: WindowRequest) -> Result<SavingsReport> {
    let repo = crate::worktree_root(root);
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)?;
    Ok(rt.block_on(family_savings(&repo, request)))
}

/// The data behind one reread report (separated from rendering so the report
/// format is lockable by a golden test with fixed inputs).
pub(crate) struct RereadReport {
    /// Captured sessions covered.
    pub(crate) sessions: u64,
    /// The aggregate metrics.
    pub(crate) metrics: ContextMetrics,
    /// `(session_id, input_tokens)` per session, descending.
    pub(crate) per_session: Vec<(String, u64)>,
    /// The tokenizer that produced every number ("skeptical buyer" provenance).
    pub(crate) tokenizer: &'static str,
}

/// Render the locked report format (golden-tested).
pub(crate) fn render_reread_report(r: &RereadReport) -> String {
    let m = &r.metrics;
    let unchanged_component = m.avoidable_tokens.saturating_sub(m.repeated_read_tokens);
    let mut out = String::new();
    out.push_str(&format!("reread report — {} session(s)\n", r.sessions));
    out.push_str(&format!("tokenizer:               {}\n", r.tokenizer));
    out.push_str(&format!(
        "total input tokens:      {}\n",
        m.total_input_tokens
    ));
    out.push_str(&format!(
        "repeated_context_rate:   {:.4}\n",
        m.repeated_context_rate
    ));
    out.push_str(&format!(
        "avoidable_context_rate:  {:.4}\n",
        m.avoidable_context_rate
    ));
    out.push_str("components:\n");
    out.push_str(&format!(
        "  repeated re-reads:     {}\n",
        m.repeated_read_tokens
    ));
    out.push_str(&format!("  unchanged-vs-worktree: {unchanged_component}\n"));
    out.push_str("per session:\n");
    if r.per_session.is_empty() {
        out.push_str("  (none)\n");
    }
    for (sid, tokens) in &r.per_session {
        let short: String = sid.chars().take(12).collect();
        out.push_str(&format!("  {short:<12}  {tokens} tok\n"));
    }
    out
}

/// The reread report's window note: empty unless this family's savings window
/// is narrower than the record the report covers.
fn family_window_note(repo: &Path) -> String {
    let Ok(family) = crate::family::resolve_at(repo) else {
        return String::new();
    };
    let window = savings_window::resolve(&family.common_dir, None);
    if !window.is_cut() {
        return String::new();
    }
    format!(
        "  the savings line above covers the whole record; `rift savings` reports\n  \
         this family's window ({})\n",
        window.label()
    )
}

/// `rift memory reread-report`: the recall plane's savings evidence — honest,
/// explainable, appended immutably.
async fn reread_report(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    read_paths: &BTreeSet<String>,
) -> Result<Outcome> {
    let worktree_map = worktree::hash_paths(repo, read_paths);
    let metrics = compute_metrics(store, scope, &worktree_map, &NoAncestry).await?;
    record_metric_snapshot(store, &metrics, crate::hooks::now_ms()).await?;

    let sessions = u64::try_from(store.count("n_session").await?).unwrap_or(0);
    let rows = store
        .query(
            "SELECT e.from_node, CAST(SUM(r.input_tokens) AS INTEGER) FROM n_file_read r \
             JOIN graph_edge e ON e.to_node = r.checkpoint_id AND e.kind = 'created' \
             GROUP BY e.from_node ORDER BY SUM(r.input_tokens) DESC, e.from_node ASC",
            &[],
        )
        .await?;
    let per_session: Vec<(String, u64)> = rows
        .iter()
        .filter_map(|r| {
            let sid = r.get_text(0).ok()?;
            let tok = u64::try_from(r.get_i64(1).ok()?).unwrap_or(0);
            Some((sid, tok))
        })
        .collect();

    let report = RereadReport {
        sessions,
        metrics: metrics.clone(),
        per_session,
        tokenizer: rift_tokens::TOKENIZER,
    };
    // The pre-interception baseline, from the OTHER store: the recall
    // projection above knows revisions but not the order the reads happened
    // in, and the classes are an ordering fact. Both are family-keyed, so the
    // two halves of this report answer for the same set of checkouts.
    let classes = family_repeat_classes(repo).await;
    // The levers come from the recall projection's two audit relations, which
    // is a third store again: what was decided is not what was captured, and
    // the report says which relation every figure came out of.
    let levers = family_levers(repo).await;
    // And the fourth source: this member's own run state. The decisions the
    // gate took live in files, not in either projection, and a pool nobody
    // declined is only ever in the journal that recorded observing it.
    let windows = member_window_journals(repo);
    // And the same four sources again, cut once at the moment the gate began
    // declining: an observe half and an enforce half, so the two are compared
    // as measured windows rather than as one blended total.
    let split = family_enforce_split(repo).await;
    let human = format!(
        "{}{}{}{}{}{}",
        render_reread_report(&report),
        render_repeat_classes(&classes),
        render_levers(&levers, classes.total_tokens()),
        // This document covers the whole record; `rift savings` answers over
        // the family's window. When a boundary marker makes those two
        // different periods, the report says so here rather than let one
        // figure's name stand over two of them.
        family_window_note(repo),
        render_window_journals(&windows),
        render_enforce_split(&split)
    );
    let db_path: PathBuf = crate::family::recall_db(repo)?;
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "sessions": report.sessions,
            "tokenizer": report.tokenizer,
            "metrics": metrics,
            "per_session": report.per_session,
            "repeat_classes": classes,
            "levers": levers,
            "window_journals": windows,
            "enforce_split": split,
            "snapshot": "appended",
            "recall_db": db_path.display().to_string(),
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launch_ingest_is_inert_when_recall_is_disabled() {
        // The enablement guard: an unguarded ingest would create recall.db and
        // silently flip the memory sections' Disabled/Quiet distinction. The
        // guard fires BEFORE any path/store resolution, so a disabled repo is
        // untouched fully. Disabled is DECLARED — `total_recall = false` —
        // since the key is on unless written off.
        let tmp = tempfile::tempdir().unwrap();
        let rift_dir = tmp.path().join(".rift");
        std::fs::create_dir_all(&rift_dir).unwrap();
        let write = |features: &str| {
            std::fs::write(
                rift_dir.join("config.toml"),
                format!("schema_version = 1\n[project]\nname = \"t\"\n{features}"),
            )
            .unwrap();
        };
        write("[features]\ntotal_recall = false\n");
        let ran = launch_ingest(tmp.path()).unwrap();
        assert!(!ran, "recall declared off -> no ingest ran");

        // With recall on — written, or left to the default — the guard's
        // second question is asked: this tempdir is no repository and so is
        // not set up here, and the answer is the same quiet `false` rather
        // than a pass that would have minted an identity to fail against.
        // The pass itself runs in the e2e suites, inside repositories `init`
        // provisioned.
        for on in ["[features]\ntotal_recall = true\n", ""] {
            write(on);
            assert!(
                !launch_ingest(tmp.path()).unwrap(),
                "recall on for {on:?} but not set up here -> nothing touched"
            );
        }
        assert!(
            !tmp.path().join(".git").exists(),
            "nothing minted a repository where there was none"
        );
    }

    /// The reread-report format is a locked artifact: this golden test and a
    /// snapshot test together pin it.
    #[test]
    fn reread_report_format_is_locked() {
        let report = RereadReport {
            sessions: 12,
            metrics: ContextMetrics {
                scope_hash: "abc".into(),
                total_input_tokens: 10_000,
                repeated_read_tokens: 1_500,
                avoidable_tokens: 2_200,
                repeated_context_rate: 0.15,
                avoidable_context_rate: 0.22,
            },
            per_session: vec![
                ("session-aaaa-1111".into(), 6_000),
                ("session-bbbb-2222".into(), 4_000),
            ],
            tokenizer: "tiktoken/o200k_base",
        };
        let rendered = render_reread_report(&report);
        let golden = "\
reread report — 12 session(s)
tokenizer:               tiktoken/o200k_base
total input tokens:      10000
repeated_context_rate:   0.1500
avoidable_context_rate:  0.2200
components:
  repeated re-reads:     1500
  unchanged-vs-worktree: 700
per session:
  session-aaaa  6000 tok
  session-bbbb  4000 tok
";
        assert_eq!(rendered, golden);
    }

    /// Build a read fact; `rev` is the revision identity a repeat is judged
    /// against, so two facts with the same `rev` are the same bytes.
    fn fact(session: &str, path: &str, at: i64, call: &str, rev: &str) -> ReadFact {
        ReadFact {
            session_id: session.to_owned(),
            path: path.to_owned(),
            started_at: at,
            tool_call_id: call.to_owned(),
            file_revision_id: rev.to_owned(),
            input_tokens: Some(100),
            agent: "claude-code".to_owned(),
            tool_name: "Read".to_owned(),
        }
    }

    /// The claim the whole baseline rests on: every captured read lands in
    /// exactly one of the three classes, so the three counts sum to the read
    /// count and no read can be spent twice or lost. The fixture also pins
    /// each boundary the classification has to get right — a different path,
    /// a different session, a different revision, and the timestamp tie.
    #[test]
    fn repeat_classes_partition_the_reads() {
        let facts = vec![
            // s1/a: first, then the same bytes again, then changed bytes.
            fact("s1", "a.rs", 10, "c1", "rev-a1"),
            fact("s1", "a.rs", 20, "c2", "rev-a1"),
            fact("s1", "a.rs", 30, "c3", "rev-a2"),
            // …and back to the first revision: an earlier read at THAT
            // revision exists, so this is an unchanged repeat, not a changed
            // one — the class is about what the context holds, not about the
            // most recent read.
            fact("s1", "a.rs", 40, "c4", "rev-a1"),
            // A different path in the same session is its own first read.
            fact("s1", "b.rs", 50, "c5", "rev-b1"),
            // The same path in a DIFFERENT session is a first read: a repeat
            // is only a repeat within one context window.
            fact("s2", "a.rs", 60, "c6", "rev-a1"),
            // Two reads in the same millisecond are peers, never priors:
            // both are first reads.
            fact("s2", "c.rs", 70, "c7", "rev-c1"),
            fact("s2", "c.rs", 70, "c8", "rev-c1"),
        ];

        let classes = classify_reads(&facts);
        assert_eq!(
            classes,
            vec![
                ReadClass::FirstRead,
                ReadClass::RereadUnchanged,
                ReadClass::RereadChanged,
                ReadClass::RereadUnchanged,
                ReadClass::FirstRead,
                ReadClass::FirstRead,
                ReadClass::FirstRead,
                ReadClass::FirstRead,
            ]
        );

        let c = repeat_classes("p.db".to_owned(), &facts);
        assert_eq!(c.total_reads(), facts.len() as u64, "every read is classed");
        assert_eq!(c.first_read.reads, 5);
        assert_eq!(c.reread_unchanged.reads, 2);
        assert_eq!(c.reread_changed.reads, 1);
        assert_eq!(c.repeats().reads, 3);
        assert_eq!(c.total_tokens(), 800);
        assert_eq!(c.sessions, 2);
        assert_eq!(c.reads_without_token_counts, 0);
        assert!(c.note.is_none());

        // The partition holds over the per-session rows too — the total is
        // the sum of the parts, in both columns.
        let (mut reads, mut tokens) = (0u64, 0u64);
        for s in &c.per_session {
            reads += s.first_read.reads + s.reread_unchanged.reads + s.reread_changed.reads;
            tokens += s.first_read.tokens + s.reread_unchanged.tokens + s.reread_changed.tokens;
        }
        assert_eq!(reads, c.total_reads());
        assert_eq!(tokens, c.total_tokens());
    }

    /// A read captured before the token-count relation existed is a read that
    /// happened: it is counted, it is worth nothing in the token column, and
    /// the report says how many such reads it is standing on.
    #[test]
    fn uncounted_reads_are_counted_as_reads_and_declared() {
        let mut facts = vec![fact("s1", "a.rs", 10, "c1", "rev-a1")];
        facts.push(ReadFact {
            input_tokens: None,
            ..fact("s1", "a.rs", 20, "c2", "rev-a1")
        });
        let c = repeat_classes("p.db".to_owned(), &facts);
        assert_eq!(c.total_reads(), 2);
        assert_eq!(c.total_tokens(), 100);
        assert_eq!(c.reads_without_token_counts, 1);
        assert_eq!(c.reread_unchanged.reads, 1);
        assert_eq!(c.reread_unchanged.tokens, 0);
    }

    /// The repeat-class block is a locked artifact: the baseline recorded
    /// against it and every later run are compared line for line, so the
    /// shape is pinned here. The lever columns that once read
    /// `not recorded yet` are gone from it: every one of them now has a
    /// number, in the blocks the whole-report golden below locks.
    #[test]
    fn repeat_classes_block_is_locked() {
        let classes = RepeatClasses {
            projection: "/cache/rift/fam/projection.db".to_owned(),
            sessions: 2,
            first_read: ClassTotals {
                reads: 307,
                tokens: 2_011_810,
            },
            reread_unchanged: ClassTotals {
                reads: 98,
                tokens: 500_428,
            },
            reread_changed: ClassTotals {
                reads: 58,
                tokens: 866_705,
            },
            reads_without_token_counts: 4,
            shell_reads: 12,
            reads_of_unknown_tools: 1,
            per_session: vec![
                SessionClasses {
                    session_id: "session-aaaa-1111".to_owned(),
                    first_read: ClassTotals {
                        reads: 200,
                        tokens: 1_500_000,
                    },
                    reread_unchanged: ClassTotals {
                        reads: 60,
                        tokens: 300_000,
                    },
                    reread_changed: ClassTotals {
                        reads: 30,
                        tokens: 400_000,
                    },
                },
                SessionClasses {
                    session_id: "session-bbbb-2222".to_owned(),
                    first_read: ClassTotals {
                        reads: 107,
                        tokens: 511_810,
                    },
                    reread_unchanged: ClassTotals {
                        reads: 38,
                        tokens: 200_428,
                    },
                    reread_changed: ClassTotals {
                        reads: 28,
                        tokens: 466_705,
                    },
                },
            ],
            note: None,
        };
        let golden = "\
repeat classes (capture projection):
  projection:            /cache/rift/fam/projection.db
  sessions with reads:   2
  reads classified:      463
  reads without counts:  4
  shell reads:           12 (2.6%, 1 through a tool the pins do not name)
  class                reads      tokens    share
  first-read             307     2011810     59.5%
  reread-unchanged        98      500428     14.8%
  reread-changed          58      866705     25.7%
  total repeats          156     1367133     40.5%
per session:
  session-aaaa  first 200  unchanged 60  changed 30
  session-bbbb  first 107  unchanged 38  changed 28
";
        assert_eq!(render_repeat_classes(&classes), golden);
    }

    /// A projection that could not be read prints WHY and stops — never a
    /// row of zeroes a reader would mistake for "no repeats here".
    #[test]
    fn an_unreadable_projection_renders_its_reason_not_zeroes() {
        let rendered = render_repeat_classes(&RepeatClasses::unavailable(
            "/cache/rift/fam/projection.db".to_owned(),
            "no capture projection yet".to_owned(),
        ));
        let golden = "\
repeat classes (capture projection):
  projection:            /cache/rift/fam/projection.db
  not measured:          no capture projection yet
";
        assert_eq!(rendered, golden);
    }

    #[test]
    fn empty_report_renders_the_none_marker() {
        let report = RereadReport {
            sessions: 0,
            metrics: ContextMetrics {
                scope_hash: String::new(),
                total_input_tokens: 0,
                repeated_read_tokens: 0,
                avoidable_tokens: 0,
                repeated_context_rate: 0.0,
                avoidable_context_rate: 0.0,
            },
            per_session: Vec::new(),
            tokenizer: "tiktoken/o200k_base",
        };
        assert!(render_reread_report(&report).contains("(none)"));
    }

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

    /// ★ A read that arrived as a shell command is counted as one, and the
    /// family's own pinned table is what decides that — never a list of tool
    /// names retyped here. A tool the pins do not name is not a shell read,
    /// and the count of those is published so the share cannot quietly hide
    /// a family Rift has not pinned.
    #[test]
    fn the_shell_read_share_comes_from_the_pinned_tool_table() {
        let shell = |agent: &str, tool: &str| ReadFact {
            session_id: "s1".to_owned(),
            path: "a.rs".to_owned(),
            started_at: 1,
            tool_call_id: format!("{agent}:{tool}"),
            file_revision_id: "rev1".to_owned(),
            input_tokens: Some(10),
            agent: agent.to_owned(),
            tool_name: tool.to_owned(),
        };
        let facts = vec![
            shell("claude-code", "Read"),
            shell("claude-code", "Bash"),
            shell("antigravity", "run_command"),
            shell("codex", "Bash"),
            shell("gemini", "read_file"),
        ];
        let c = repeat_classes("db".to_owned(), &facts);
        assert_eq!(c.shell_reads, 3, "Bash twice and run_command once");
        assert_eq!(
            c.reads_of_unknown_tools, 1,
            "the family with no pinned row is declared, not counted as shell"
        );
        assert!(render_repeat_classes(&c).contains("shell reads:           3 (60.0%"));
    }

    /// ★ The whole report, locked as one artifact.
    ///
    /// Two windows are compared line for line — the observe window and the
    /// enforce window that follows it — and a block that quietly changed
    /// shape between them would make that comparison a guess. Every figure
    /// this report can carry appears here with a value, so the lock is a lock
    /// on the arithmetic as well as on the layout.
    #[test]
    #[allow(clippy::too_many_lines)] // the fixture IS the test: every figure carries a value
    fn the_whole_report_shape_is_locked() {
        let classes = RepeatClasses {
            projection: "/cache/fam/projection.db".to_owned(),
            sessions: 1,
            first_read: ClassTotals {
                reads: 3,
                tokens: 300,
            },
            reread_unchanged: ClassTotals {
                reads: 2,
                tokens: 600,
            },
            reread_changed: ClassTotals {
                reads: 1,
                tokens: 100,
            },
            reads_without_token_counts: 0,
            shell_reads: 0,
            reads_of_unknown_tools: 0,
            per_session: vec![SessionClasses {
                session_id: "session-aaaa-1111".to_owned(),
                first_read: ClassTotals {
                    reads: 3,
                    tokens: 300,
                },
                reread_unchanged: ClassTotals {
                    reads: 2,
                    tokens: 600,
                },
                reread_changed: ClassTotals {
                    reads: 1,
                    tokens: 100,
                },
            }],
            note: None,
        };
        let levers = Levers {
            recall_db: "/cache/fam/recall.db".to_owned(),
            note: None,
            kinds: vec![
                LeverKind {
                    verb: "memd.read_intercepted".to_owned(),
                    rows: 4,
                    tokens_returned: 160,
                    tokens_avoided: 900,
                },
                LeverKind {
                    verb: "memd.read_repeat_observed".to_owned(),
                    rows: 7,
                    tokens_returned: 0,
                    tokens_avoided: 0,
                },
            ],
            tokens_avoided: 900,
            tokens_returned: 160,
            families: vec![FamilyLevers {
                family: "claude-code".to_owned(),
                step: "read-gate".to_owned(),
                rows: 11,
                tokens_returned: 160,
                tokens_avoided: 900,
            }],
            pulls: vec![PullsAfterInjection {
                family: "claude-code".to_owned(),
                injections: 2,
                pulls: 1,
            }],
            pulls_without_injection: 1,
            held_revisions: 4,
            held_with_blob: 3,
        };
        let windows = WindowJournals {
            dir: "/repo/.rift/state/reads".to_owned(),
            note: None,
            windows: 2,
            lines: 12,
            reads: 6,
            read_tokens: 1000,
            reads_unmeasured: 1,
            reads_over_cap: 1,
            max_slice_bytes: 98_304,
            min_slice_lines: 24,
            pool_reads: 3,
            pool_tokens: 700,
            pool_unmeasured: 0,
            buckets: vec![
                SliceBucket {
                    label: "< 24 lines".to_owned(),
                    reads: 1,
                    tokens: 40,
                    unmeasured: 0,
                },
                SliceBucket {
                    label: "24–99 lines".to_owned(),
                    reads: 1,
                    tokens: 160,
                    unmeasured: 0,
                },
                SliceBucket {
                    label: "100–999 lines".to_owned(),
                    reads: 1,
                    tokens: 500,
                    unmeasured: 0,
                },
                SliceBucket {
                    label: "≥ 1000 lines".to_owned(),
                    reads: 0,
                    tokens: 0,
                    unmeasured: 0,
                },
            ],
            retries: vec![
                RetryAfterDeny {
                    frame: "delta".to_owned(),
                    denies: 2,
                    retried: 0,
                },
                RetryAfterDeny {
                    frame: "plain".to_owned(),
                    denies: 4,
                    retried: 1,
                },
            ],
            adjacent_reads: 2,
            evictions: 1,
            post_compaction_tokens: 250,
        };
        let rendered = format!(
            "{}{}",
            render_levers(&levers, classes.total_tokens()),
            render_window_journals(&windows)
        );
        let golden = "\
levers (audit relations, everything recorded since the reset):
  recall projection:     /cache/fam/recall.db
  verb                        rows    returned     avoided
  memd.read_intercepted          4         160         900
  memd.read_repeat_observed       7           0           0
  honesty (this relation): avoided 900 − returned 160 = 740
  savings: avoided ÷ (avoided + read) = 47.4% (900 ÷ 1900)
per family (the other relation, its own figures, never joined):
  claude-code:read-gate         11         160         900
pull after injection (a skill call within 600000 ms of one):
  claude-code                    2 injected       1 pulled
  pulls with no injection before them: 1
basis availability (recorded read revisions):
  held revisions:        4
  reachable as a blob:   3 (75.0%)
window journals (this member's own run state):
  directory:             /repo/.rift/state/reads
  windows:               2
  lines:                 12
  reads:                 6 (1000 tok, 1 unmeasured)
  past the 98304 B cap:  1 (never eligible for a decline)
  would-have-declined:   3 reads, 700 tok (0 unmeasured), floor 24 lines
  by slice length        reads      tokens  unmeasured
    < 24 lines               1          40           0
    24–99 lines              1         160           0
    100–999 lines            1         500           0
    ≥ 1000 lines             0           0           0
  retry after decline   declines     retried
    delta                      2           0  0.0%
    plain                      4           1  25.0%
  range-walking:         2
  evictions:             1
  read within 3 turns of one: 250 of 1000 tok (25.0%)
";
        assert_eq!(rendered, golden);
    }

    /// A work tree with one file of `lines` numbered lines, and its revision.
    fn seeded_tree(lines: usize) -> (tempfile::TempDir, String) {
        let tmp = tempfile::tempdir().unwrap();
        let body = (1..=lines).fold(String::new(), |mut acc, n| {
            use core::fmt::Write as _;
            let _ = writeln!(acc, "line {n}");
            acc
        });
        std::fs::write(tmp.path().join("a.txt"), &body).unwrap();
        let revision = rift_agent::content_sha256(body.as_bytes()).to_hex();
        (tmp, revision)
    }

    /// Write one window's journal file.
    fn window_file(root: &Path, key: &str, lines: &[JournalLine]) {
        let dir = root.join(".rift").join("state").join("reads");
        std::fs::create_dir_all(&dir).unwrap();
        let body = lines.iter().fold(String::new(), |mut acc, l| {
            use core::fmt::Write as _;
            let _ = writeln!(acc, "{}", l.to_json_line());
            acc
        });
        std::fs::write(dir.join(format!("{key}.jsonl")), body).unwrap();
    }

    fn revision_id(hex: &str) -> rift_core::Sha256Id {
        rift_core::Sha256Id::parse_hex(hex).unwrap()
    }

    fn read_line(turn: u32, hash: &str, lo: u32, hi: u32) -> JournalLine {
        JournalLine::Read {
            turn,
            path: "a.txt".to_owned(),
            hash: revision_id(hash),
            range: Interval::new(lo, hi).unwrap(),
            total_lines: 200,
            at_ms: 1,
        }
    }

    fn observe_line(turn: u32, hash: &str, lo: u32, hi: u32) -> JournalLine {
        JournalLine::Observe {
            turn,
            path: "a.txt".to_owned(),
            hash: revision_id(hash),
            range: Interval::new(lo, hi).unwrap(),
            at_ms: 2,
        }
    }

    /// ★ The pool the floor is priced from: every repeat the gate would have
    /// declined, counted in reads AND in tokens, distributed by the length of
    /// the slice it was about. Without the distribution the floor is a number
    /// nobody can move on evidence.
    #[test]
    fn the_would_have_declined_pool_is_counted_by_slice_length() {
        let (tmp, rev) = seeded_tree(200);
        window_file(
            tmp.path(),
            "w1",
            &[
                read_line(1, &rev, 1, 200),
                observe_line(2, &rev, 1, 10),
                observe_line(3, &rev, 1, 50),
                observe_line(4, &rev, 1, 150),
            ],
        );
        let w = window_journals(tmp.path(), 98_304, 24);
        assert_eq!(w.pool_reads, 3);
        assert!(w.pool_tokens > 0, "the pool is priced, not just counted");
        assert_eq!(w.pool_unmeasured, 0);
        let by_label: Vec<(String, u64)> = w
            .buckets
            .iter()
            .map(|b| (b.label.clone(), b.reads))
            .collect();
        assert_eq!(
            by_label,
            vec![
                ("< 24 lines".to_owned(), 1),
                ("24–99 lines".to_owned(), 1),
                ("100–999 lines".to_owned(), 1),
                ("≥ 1000 lines".to_owned(), 0),
            ]
        );
        let priced: u64 = w.buckets.iter().map(|b| b.tokens).sum();
        assert_eq!(priced, w.pool_tokens, "the buckets partition the pool");
    }

    /// ★ A slice whose bytes are no longer the bytes the journal named is
    /// declared unmeasured, never re-counted under the new bytes: pricing a
    /// slice nobody read is worse than admitting the count is missing.
    #[test]
    fn a_slice_whose_bytes_moved_is_declared_unmeasured_never_imputed() {
        let (tmp, rev) = seeded_tree(40);
        window_file(
            tmp.path(),
            "w1",
            &[read_line(1, &rev, 1, 40), observe_line(2, &rev, 1, 40)],
        );
        std::fs::write(tmp.path().join("a.txt"), "everything is different now\n").unwrap();
        let w = window_journals(tmp.path(), 98_304, 24);
        assert_eq!((w.reads, w.read_tokens, w.reads_unmeasured), (1, 0, 1));
        assert_eq!((w.pool_reads, w.pool_tokens, w.pool_unmeasured), (1, 0, 1));
    }

    /// A decline is answered by the same call inside the same turn or it is
    /// not answered at all, and each frame is reported on its own: the plain
    /// frame and the delta frame are different claims about what the window
    /// holds, so one retry rate over both would hide which one the model
    /// disbelieves.
    #[test]
    fn a_retry_is_counted_inside_the_turn_and_against_its_own_frame() {
        let (tmp, rev) = seeded_tree(200);
        let denied = |turn: u32, lo: u32, hi: u32| JournalLine::Deny {
            turn,
            path: "a.txt".to_owned(),
            hash: revision_id(&rev),
            range: Interval::new(lo, hi).unwrap(),
            at_ms: 3,
        };
        window_file(
            tmp.path(),
            "w1",
            &[
                // Read, then declined, then asked for again in the same turn.
                read_line(1, &rev, 1, 100),
                denied(2, 1, 100),
                read_line(2, &rev, 1, 100),
                // Held by having written it: a delta decline, not retried.
                JournalLine::Edit {
                    turn: 3,
                    path: "b.txt".to_owned(),
                    hash_old: None,
                    hash_new: revision_id(&rev),
                    sites: vec![Interval::new(4, 6).unwrap()],
                    shift: 0,
                    full: true,
                    at_ms: 4,
                },
                JournalLine::Deny {
                    turn: 4,
                    path: "b.txt".to_owned(),
                    hash: revision_id(&rev),
                    range: Interval::new(1, 100).unwrap(),
                    at_ms: 5,
                },
            ],
        );
        let w = window_journals(tmp.path(), 98_304, 24);
        let mut rows: Vec<(String, u64, u64)> = w
            .retries
            .iter()
            .map(|r| (r.frame.clone(), r.denies, r.retried))
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec![("delta".to_owned(), 1, 0), ("plain".to_owned(), 1, 1),]
        );
    }

    /// Two signals a summary rung would answer: reading a file a page at a
    /// time, and the reading that follows a compaction.
    #[test]
    fn range_walking_and_post_eviction_reading_are_both_counted() {
        let (tmp, rev) = seeded_tree(200);
        window_file(
            tmp.path(),
            "w1",
            &[
                read_line(1, &rev, 1, 50),
                read_line(1, &rev, 51, 100),
                read_line(1, &rev, 101, 150),
                JournalLine::Evict {
                    turn: 2,
                    reason: rift_agent::window_journal::EvictReason::Compact,
                    at_ms: 6,
                },
                read_line(3, &rev, 1, 50),
                read_line(9, &rev, 120, 170),
            ],
        );
        let w = window_journals(tmp.path(), 98_304, 24);
        assert_eq!(w.adjacent_reads, 2, "two abutting pairs, not three reads");
        assert_eq!(w.evictions, 1);
        assert!(
            w.post_compaction_tokens > 0 && w.post_compaction_tokens < w.read_tokens,
            "the turn three lines away counts; the one seven turns away does not"
        );
    }

    /// Windows are counted as windows. A subagent has a journal of its own
    /// under its parent's session id, so a count of windows is not a count of
    /// sessions and this report never prints one as the other.
    #[test]
    fn windows_are_counted_per_journal_and_rolled_files_are_one_window() {
        let (tmp, rev) = seeded_tree(200);
        window_file(tmp.path(), "w1", &[read_line(1, &rev, 1, 10)]);
        window_file(tmp.path(), "w2", &[read_line(1, &rev, 1, 10)]);
        let dir = tmp.path().join(".rift").join("state").join("reads");
        std::fs::write(
            dir.join("w1.1.rolled.jsonl"),
            format!("{}\n", read_line(1, &rev, 20, 30).to_json_line()),
        )
        .unwrap();
        // A snapshot is a fold of rows this pass reads for itself: reading it
        // too would count them twice.
        std::fs::write(dir.join("w1.1.snapshot.json"), "{}").unwrap();
        let w = window_journals(tmp.path(), 98_304, 24);
        assert_eq!((w.windows, w.reads), (2, 3));
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

    /// ★ Under enforce the would-have-declined pool no longer exists, so the
    /// floor is priced from the DECLINED slices instead — each in the window
    /// its decline was booked in, with the raw retry that followed it.
    #[test]
    fn a_declined_slice_is_priced_by_length_in_the_window_it_was_booked_in() {
        let (tmp, rev) = seeded_tree(200);
        let deny = |turn: u32, lo: u32, hi: u32, at_ms: u64| JournalLine::Deny {
            turn,
            path: "a.txt".to_owned(),
            hash: revision_id(&rev),
            range: Interval::new(lo, hi).unwrap(),
            at_ms,
        };
        let mut first = read_line(1, &rev, 1, 40);
        if let JournalLine::Read { at_ms, .. } = &mut first {
            *at_ms = 10;
        }
        let mut retry = read_line(1, &rev, 1, 40);
        if let JournalLine::Read { at_ms, .. } = &mut retry {
            *at_ms = 30;
        }
        window_file(
            tmp.path(),
            "w1",
            &[first, deny(1, 1, 40, 20), retry, deny(4, 1, 150, 90)],
        );
        let (mut observe, mut enforce) = (WindowPane::new(), WindowPane::new());
        fold_split_journals(tmp.path(), Some(50), &mut observe, &mut enforce);

        assert_eq!((observe.windows, enforce.windows), (1, 1), "it straddles");
        assert_eq!((observe.reads, observe.declined), (2, 1));
        assert_eq!((enforce.reads, enforce.declined), (0, 1));
        assert_eq!(
            observe.declined_buckets.get(1).map(|b| b.reads),
            Some(1),
            "a 40-line decline is priced in the 24-99 bucket, before the cut"
        );
        assert_eq!(
            enforce.declined_buckets.get(2).map(|b| b.reads),
            Some(1),
            "a 150-line decline is priced in the 100-999 bucket, after it"
        );
        assert!(observe
            .declined_buckets
            .get(1)
            .is_some_and(|b| b.tokens > 0));
        let before = observe.retries.first().unwrap();
        assert_eq!(
            (before.frame.as_str(), before.denies, before.retried),
            ("plain", 1, 1)
        );
        let after = enforce.retries.first().unwrap();
        assert_eq!(
            (after.frame.as_str(), after.denies, after.retried),
            ("plain", 1, 0),
            "the frame is read from the whole window, the count lands in its half"
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

    /// The verb's shape, locked: an owner reading this number has to be able
    /// to see which relation each half came from and how many sessions are
    /// behind it, or the percentage is a claim rather than a measurement.
    #[test]
    #[allow(clippy::too_many_lines)] // the fixture IS the test: every figure carries a value
    fn the_savings_shape_is_locked() {
        use rift_recall_ledger::savings::FamilyShare;
        let report = SavingsReport {
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
        };
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
        assert_eq!(render_savings(&report), golden);

        // The same figures over a boundary marker: the window line names the
        // marker, and the session counts — which come from the whole record's
        // observe/enforce cut — say so rather than being read as the window's.
        let marked = SavingsReport {
            window: Boundary::Marker(Box::new(Marker {
                schema_version: 1,
                boundary_ms: 1_700_000_000_000,
                boundary: "2023-11-14T22:13:20.000Z".to_owned(),
                set_at_ms: 1_700_000_000_500,
                set_at: "2023-11-14T22:13:20.500Z".to_owned(),
                set_by: rift_recall_ledger::savings_window::RESET_VERB.to_owned(),
            })),
            ..report
        };
        let rendered = render_savings(&marked);
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
        // The JSON says the same thing, in the fields a script reads.
        let json = savings_json(&marked);
        assert_eq!(json["window"]["boundary_ms"], 1_700_000_000_000_i64);
        assert_eq!(json["window"]["marker_set_at_ms"], 1_700_000_000_500_i64);
        assert_eq!(json["window"]["marker_set_by"], "rift savings reset");
        // …and the observe/enforce cut keeps its numbers under its own name.
        assert_eq!(json["window"]["enforce_cut_ms"], 1_700_000_000_000_i64);
        assert_eq!(
            json["window"]["enforce_cut_source"],
            "first memd.read_intercepted row"
        );
        // The denominator's own breakdown, in the keys a script reads: it
        // sums to `read_tokens` exactly, so a reader may take a channel out
        // rather than having to trust the total. `reads_unmeasured` is beside
        // it and is a count of CALLS, which is why it is not in the sum.
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
