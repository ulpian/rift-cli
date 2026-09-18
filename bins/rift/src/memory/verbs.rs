//! The three delta verbs and the one bounded dispatch behind them.
//!
//! Each verb SERVES a person standing in one checkout: the answer is contained
//! to that checkout's line and says what it held back. The family-wide
//! measurements live in the sibling modules, which read stores rather than
//! serve them.

use std::collections::BTreeSet;
use std::path::Path;

use rift_config::EffectiveConfig;
use rift_core::{Result, RiftError, Timestamp};
use rift_recall_policy::{PolicyEngine, ServePolicy, TurnBudget};
use rift_recall_serve::{
    RecallContext, ServeDispatcher, ToolError, ToolName, ToolRequest, ToolResponse,
};
use rift_recall_substrate::{worktree, GixSubstrate, MemorySubstrate, SubstrateReader};
use rift_recall_types::{AncestryOracle, NoProvenance, ProvenanceOracle, Scope};
use rift_store::{CanonicalStore, SqliteStore, Value};

use crate::cli::{exit, Outcome};

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
    // The ladder's second rung, opened for the same verbs and under the same
    // rule as the reader below: the family's own store, reached from the common
    // directory every member shares, so a revision a sibling's read recorded
    // answers here too. A checkout whose family cannot be proven from here
    // simply has the rung it always had.
    let snapshots = tool
        .may_read_the_substrate()
        .then(|| crate::hooks::basis_snapshots_at(repo))
        .flatten();
    let at = ServeAt {
        store,
        scope,
        ancestry,
        provenance,
        snapshots: snapshots.as_ref(),
    };
    match repository_reader(tool, repo) {
        Some(reader) => dispatch_over(&server, &at, &reader, tool, req).await,
        None => dispatch_over(&server, &at, &MemorySubstrate::local(), tool, req).await,
    }
}

/// Everything one dispatch needs except the reader it reads bytes through.
///
/// Five borrows that always travel together and are never stored: a context
/// struct keeps the call sites of [`dispatch_over`] readable where the reader
/// is the only thing that differs between them.
struct ServeAt<'a> {
    store: &'a SqliteStore,
    scope: &'a Scope,
    ancestry: &'a dyn AncestryOracle,
    provenance: &'a dyn ProvenanceOracle,
    snapshots: Option<&'a rift_recall_serve::basis_store::BasisStore>,
}

/// **The reader is the repository**, for the verbs that can need bytes.
///
/// A verb that serves a delta against the revision its caller holds has to
/// fetch that revision's blob and read the named file as it is now; an empty
/// in-memory substrate can do neither, so every such answer would be
/// `basis_unavailable` however much the projection knew.
///
/// NOTHING IS OPENED ON A HOOK PATH. [`tool_call_raw`] has two callers: the CLI
/// surface (`rift memory <verb>` / `rift recall <verb>`), and the push tier's
/// `changed_since`, which asks exactly one verb — `diff_since`. That verb is
/// answered from the projection alone and says so (`may_read_the_substrate`),
/// so the hook path gets `None` here and opens no repository. Nothing here is
/// reachable from the `PreToolUse` gate at all.
///
/// What the reader opens is git, and git's objects for a linked worktree live
/// in the family's common directory — the same store every member shares — so
/// the basis a sibling member committed is reachable here. No identity, cache
/// or durable key is derived from the checkout path; the projection this
/// dispatch reads was already resolved family-first by the caller. The checkout
/// matters for exactly one thing, which is what "the file as it is now" means
/// to the caller who named it.
///
/// Conservative when it cannot: a path that is not a readable repository is
/// `None`, the in-memory reader stands in, and the verb answers
/// `basis_unavailable` rather than failing the call.
fn repository_reader(tool: ToolName, repo: &Path) -> Option<GixSubstrate> {
    tool.may_read_the_substrate()
        .then(|| GixSubstrate::open(repo).ok())
        .flatten()
}

/// Serve one call through `reader`, on a budget of its own.
async fn dispatch_over<R: SubstrateReader>(
    server: &ServeDispatcher<'_>,
    at: &ServeAt<'_>,
    reader: &R,
    tool: ToolName,
    req: &ToolRequest,
) -> std::result::Result<ToolResponse, ToolError> {
    let ctx = RecallContext {
        store: at.store,
        reader,
        snapshots: at.snapshots,
        scope: at.scope.clone(),
        ancestry: at.ancestry,
        provenance: at.provenance,
        now: Timestamp::now(),
    };
    let mut budget = TurnBudget::default();
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
pub(super) async fn diff_since(
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
pub(super) async fn file_if_changed(
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
pub(super) async fn cost_hotspots(
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
async fn empty_reads_diagnosis(repo: &Path, store: &SqliteStore) -> String {
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
