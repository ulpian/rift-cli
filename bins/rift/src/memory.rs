//! `rift memory` — the human-facing recall surface (PRD-M2 gate artifacts).
//!
//! Every command reuses the recall server's **library dispatch**
//! ([`rift_recall_mcp::McpServer`]): the same four-state envelopes, per-turn
//! budgets, and policy gates the MCP tools run behind — the CLI is a second
//! caller of the one bounded surface, never a second implementation. The MCP
//! transport itself stays in `rift-memd`.
//!
//! `reread-report` renders the honest, pre-optimization measurement PRD-M2
//! gates on: session count, repeated/avoidable rates, per-component and
//! per-session breakdowns, and the tokenizer provenance — then appends an
//! immutable metric snapshot so the measurement history is tamper-evident.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rift_config::EffectiveConfig;
use rift_core::{Result, RiftError, Timestamp};
use rift_recall_ledger::ingest::{self, IngestOptions};
use rift_recall_ledger::metrics::{compute_metrics, record_metric_snapshot, ContextMetrics};
use rift_recall_mcp::{McpServer, RecallContext, ToolName, ToolRequest, ToolResponse};
use rift_recall_policy::{PolicyEngine, ServePolicy, TurnBudget};
use rift_recall_substrate::{worktree, GixSubstrate, MemorySubstrate, SubstrateReader as _};
use rift_recall_types::Scope;
use rift_store::{CanonicalStore, SqliteStore, Value};

use crate::cli::{exit, MemoryCmd, Outcome};

/// Route one `rift memory` subcommand to its handler.
pub(crate) fn run(cmd: &MemoryCmd, root: &Path) -> Result<Outcome> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)?;
    rt.block_on(dispatch(cmd, root))
}

async fn dispatch(cmd: &MemoryCmd, repo: &Path) -> Result<Outcome> {
    let db = rift_store::recall_db_for_repo(repo)?;
    let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())?;
    let sub = GixSubstrate::open(repo)?;

    // Answer on fresh state: the same ingest pass the launch auto-ingest and
    // the memory daemon run (watermark-idempotent).
    let paths = refresh_recall(&store, &sub, repo).await?;

    let repo_id = rift_store::recall_repo_id(repo)?;
    let scope = Scope::default_for(repo_id.as_str(), u32::MAX);

    match cmd {
        MemoryCmd::DiffSince(a) => diff_since(&store, repo, &scope, &a.point).await,
        MemoryCmd::FileIfChanged(a) => file_if_changed(&store, repo, &scope, &a.path).await,
        MemoryCmd::CostHotspots(a) => cost_hotspots(&store, repo, &scope, a.top).await,
        MemoryCmd::RereadReport => reread_report(&store, repo, &scope, &paths).await,
    }
}

/// The one recall ingest pass (M5-06): blob-oid map first (ADR-0005), then the
/// incremental graph ingest (already-projected checkpoints are skipped — the
/// watermark that makes launch-twice a no-op). Returns the read paths (the
/// worktree-hash inputs some callers reuse). Shared by `rift memory *`, the
/// dashboard's launch auto-ingest, and (via the same library calls) rift-memd.
pub(crate) async fn refresh_recall(
    store: &SqliteStore,
    sub: &GixSubstrate,
    repo: &Path,
) -> Result<std::collections::BTreeSet<String>> {
    let (reads, _issues) = ingest::read_facts_all(sub)?;
    let paths = ingest::read_paths(&reads);
    let blobs = worktree::hash_blobs(repo, &paths);
    rift_recall_ledger::blob_map::persist_blob_oid_map(store, &blobs, 0).await?;
    // The token fallback rides EVERY ingest front door (M6-12): first writer
    // wins on content-addressed rows, so an unresolved launch-time ingest
    // would freeze 0-token reads that rift-memd could have resolved.
    let capture = rift_store::capture_projection_db_for_repo(repo)
        .ok()
        .filter(|db| db.exists())
        .and_then(|db| rift_store::open_capture_projection(&db).ok());
    let opts = IngestOptions {
        token_estimate_by_hash: rift_recall_ledger::tokens::fallback_token_estimates(
            &reads,
            store,
            capture.as_ref(),
            sub,
            repo,
        )
        .await,
        ..IngestOptions::default()
    };
    ingest::ingest_with(store, sub, &opts).await?;
    Ok(paths)
}

/// The dashboard's launch auto-ingest (M5-06, PRD user story 29): one pass for
/// the launch-selected repository, **only when rift-total-recall is enabled
/// there** — an unguarded ingest would create `recall.db` and silently flip
/// the memory sections' Disabled/Quiet distinction. Returns `Ok(false)` when
/// recall is off (nothing touched), `Ok(true)` after a completed pass.
pub(crate) fn launch_ingest(repo: &Path) -> Result<bool> {
    let cfg = EffectiveConfig::load(&repo.join(".rift"))
        .or_else(|_| EffectiveConfig::secure_default())?;
    if !cfg.recall_enabled() {
        return Ok(false);
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(RiftError::from)?;
    rt.block_on(async {
        let db = rift_store::recall_db_for_repo(repo)?;
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())?;
        let sub = GixSubstrate::open(repo)?;
        refresh_recall(&store, &sub, repo).await?;
        Ok::<_, RiftError>(())
    })?;
    Ok(true)
}

/// Verify the memory daemon can SERVE this repository (M5-13): a real tool
/// call through the same bounded MCP dispatch `rift-memd serve --mcp` fronts
/// — policy gates, budget, the freshly-ingested recall.db. Ok = servable; the
/// reason string is the lifecycle row's Failed text.
pub(crate) fn verify_recall_serves(repo: &Path) -> std::result::Result<(), String> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(async {
        let db = rift_store::recall_db_for_repo(repo).map_err(|e| format!("recall db: {e}"))?;
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())
            .map_err(|e| format!("recall db open: {e}"))?;
        let repo_id = rift_store::recall_repo_id(repo).map_err(|e| format!("repo id: {e}"))?;
        let scope = Scope::default_for(repo_id.as_str(), u32::MAX);
        tool_call(
            &store,
            repo,
            &scope,
            ToolName::CostHotspots,
            &ToolRequest {
                top_n: Some(1),
                ..ToolRequest::default()
            },
        )
        .await
        .map(|_| ())
        .map_err(|e| format!("serve check failed: {e}"))
    })
}

/// Dispatch one tool call through the shared bounded surface: the same policy
/// gates (`.rift/policies/serve.toml` overlay, fail-closed on a malformed
/// config) and per-turn budget the MCP transport applies.
async fn tool_call(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    tool: ToolName,
    req: &ToolRequest,
) -> Result<ToolResponse> {
    let rift_dir = repo.join(".rift");
    let config = match EffectiveConfig::load(&rift_dir) {
        Ok(c) => c,
        Err(_) => EffectiveConfig::secure_default()?,
    };
    let policy = PolicyEngine::new(&config).with_serve_policy(ServePolicy::load(&rift_dir));
    let server = McpServer::new(policy);
    let reader = MemorySubstrate::local();
    let ctx = RecallContext {
        store,
        reader: &reader,
        scope: scope.clone(),
        now: Timestamp::now(),
    };
    let mut budget = TurnBudget::default();
    server
        .dispatch(tool, req, &ctx, &mut budget)
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
    let resp = tool_call(store, repo, scope, ToolName::DiffSince, &req).await?;
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
    let resp = tool_call(store, repo, scope, ToolName::FileIfChanged, &req).await?;
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
    top: u32,
) -> Result<Outcome> {
    let req = ToolRequest {
        top_n: Some(top),
        ..ToolRequest::default()
    };
    let resp = tool_call(store, repo, scope, ToolName::CostHotspots, &req).await?;
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

/// Name the broken memory-chain link behind an empty read set (M5-06, PRD user
/// story 31): no checkpoints → not ingested → genuinely no reads — each with
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

/// `rift memory reread-report`: the M2 gate evidence — honest, explainable,
/// appended immutably.
async fn reread_report(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    read_paths: &BTreeSet<String>,
) -> Result<Outcome> {
    let worktree_map = worktree::hash_paths(repo, read_paths);
    let metrics = compute_metrics(store, scope, &worktree_map).await?;
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
    let human = render_reread_report(&report);
    let db_path: PathBuf = rift_store::recall_db_for_repo(repo)?;
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "sessions": report.sessions,
            "tokenizer": report.tokenizer,
            "metrics": metrics,
            "per_session": report.per_session,
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
        // The M5-06 guard: an unguarded ingest would create recall.db and
        // silently flip the memory sections' Disabled/Quiet distinction. The
        // guard fires BEFORE any path/store resolution, so a disabled repo is
        // untouched entirely.
        let tmp = tempfile::tempdir().unwrap();
        let ran = launch_ingest(tmp.path()).unwrap();
        assert!(!ran, "recall disabled -> no ingest ran");

        // With recall enabled in the per-repo policy config the guard opens
        // (the pass itself then needs a git repo, which this tempdir is not —
        // an error is fine here; the GUARD decision is what this pins).
        let rift_dir = tmp.path().join(".rift");
        std::fs::create_dir_all(&rift_dir).unwrap();
        std::fs::write(
            rift_dir.join("config.toml"),
            "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
        )
        .unwrap();
        assert!(
            launch_ingest(tmp.path()).is_err(),
            "guard open -> the pass runs (and fails honestly on a non-repo)"
        );
    }

    /// The reread-report format is a gate artifact: locked by this golden test
    /// (PRD-M2 "its format is locked by a golden fixture and a snapshot test").
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
}
