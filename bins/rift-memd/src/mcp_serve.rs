//! The live `rmcp` stdio MCP server (Stage 3 Phase 5 Step 1).
//!
//! `rift-memd serve --mcp` is spawned by the agent's MCP config (process lifetime
//! = session). It speaks MCP over **stdio** via the official `rmcp` SDK,
//! advertising exactly the eight bounded recall tools and **no** prompts or
//! resources (minimal attack surface). stdout carries only protocol bytes; the
//! store is opened **read-only** and one `repo_id` is pinned per process, so the
//! scope cannot cross repositories. EOF / a closed stdin ends the session.
//!
//! Each `tools/call` decodes its params, dispatches through the transport-
//! independent [`McpServer`] (budget + policy + audit middleware), hands the
//! resulting `memd.*` event to `rift hooks memd` (best-effort, never blocking the
//! response), and returns the tool payload as MCP text content.
//!
//! Budget note: MCP has no "turn" delimiter, so each call gets a fresh per-turn
//! budget (the ceiling applies per call). Cross-call turn budgeting is a
//! follow-up once a turn signal is available.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rift_config::EffectiveConfig;
use rift_core::Timestamp;
use rift_core::{Result, RiftError};
use rift_recall_mcp::{
    decode_request, McpServer, RecallContext, ToolName, ToolRequest, TOOL_SURFACE,
};
use rift_recall_policy::{PolicyEngine, TurnBudget};
use rift_recall_substrate::{GixSubstrate, MemorySubstrate, SubstrateReader};
use rift_recall_types::Scope;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, JsonObject, ListToolsResult,
    PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::RequestContext;
use rmcp::{ErrorData, RoleServer, ServiceExt};

/// The agent-spawned recall server. Holds only `Send + Sync` state; the recall
/// store + substrate are opened per call (read-only).
pub(crate) struct RiftRecallServer {
    repo: PathBuf,
    config: EffectiveConfig,
}

impl RiftRecallServer {
    fn new(repo: &Path) -> Result<Self> {
        // Fail-closed: a missing/malformed config resolves to secure defaults.
        let config = match EffectiveConfig::load(&repo.join(".rift")) {
            Ok(c) => c,
            Err(_) => EffectiveConfig::secure_default()?,
        };
        Ok(Self {
            repo: repo.to_path_buf(),
            config,
        })
    }
}

/// One short, neutral description per tool (advertised in `tools/list`).
fn tool_description(tool: ToolName) -> &'static str {
    match tool {
        ToolName::KnownState => {
            "Manifest of what changed vs what the agent already knows (ranked, four-state)."
        }
        ToolName::DiffSince => "Changed and unchanged files since a known point.",
        ToolName::FileIfChanged => "Conditional check: is a (path, known_hash) still current?",
        ToolName::ChangedHunks => "The actual changed hunks, capped and token-budgeted.",
        ToolName::ReadSummary => {
            "A hash-bound, validity-checked summary (stale source is never served)."
        }
        ToolName::RetrieveExact => "Exact bytes — reason required, policy-gated, always audited.",
        ToolName::CostHotspots => "Awareness-only: areas with the most repeated read tokens.",
        ToolName::ContextPack => "An ordered, budgeted, position-aware evidence pack.",
    }
}

/// A minimal but valid input schema for a tool (ranking tools mark their
/// query-aware params required, W5).
fn tool_schema(tool: ToolName) -> Arc<JsonObject> {
    let mut schema = serde_json::json!({
        "type": "object",
        "properties": {
            "task": { "type": "string" },
            "files_in_focus": { "type": "array", "items": { "type": "string" } },
            "error_signatures": { "type": "array", "items": { "type": "string" } },
            "token_budget": { "type": "integer" },
            "known_hashes": { "type": "array", "items": { "type": "string" } },
            "target": { "type": "string" },
            "trust": { "type": "string" },
            "reason": { "type": "string" },
            "top_n": { "type": "integer" }
        }
    });
    if tool.is_ranking_sensitive() {
        if let Some(obj) = schema.as_object_mut() {
            obj.insert(
                "required".into(),
                serde_json::json!(["task", "files_in_focus"]),
            );
        }
    }
    let obj = schema.as_object().cloned().unwrap_or_default();
    Arc::new(obj)
}

/// The eight tool descriptors (the whole advertised surface).
fn tool_descriptors() -> Vec<Tool> {
    TOOL_SURFACE
        .iter()
        .map(|&t| Tool::new(t.wire_name(), tool_description(t), tool_schema(t)))
        .collect()
}

/// Best-effort: hand a serving event to `rift hooks memd <verb>` (the Go capture
/// CLI redacts + chains it). Never blocks the response; all errors are ignored.
fn emit_event(event: &rift_recall_events::MemdEvent) {
    use std::process::{Command, Stdio};
    let verb = event.verb();
    let json = event.to_hook_json();
    if let Ok(mut child) = Command::new("rift")
        .args(["hooks", "memd", verb])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        if let Some(stdin) = child.stdin.take() {
            use std::io::Write as _;
            let mut stdin = stdin;
            let _ = stdin.write_all(json.as_bytes());
        }
        // Detach: do not wait (non-blocking audit handoff).
        drop(child);
    }
}

impl ServerHandler for RiftRecallServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            // Tools only — no prompts, no resources (minimal attack surface).
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "rift-memd".to_string(),
                title: Some("Rift total-recall".to_string()),
                version: env!("CARGO_PKG_VERSION").to_string(),
                description: Some(
                    "Bounded delta retrieval: 8 tools that serve deltas, not re-reads.".to_string(),
                ),
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "Start with rift_known_state. Retrieval is budgeted and audited; \
                 exact retrieval requires a reason."
                    .to_string(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tool_descriptors()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> std::result::Result<CallToolResult, ErrorData> {
        let params = request
            .arguments
            .clone()
            .map_or(serde_json::Value::Null, serde_json::Value::Object);
        let (tool, req) = decode_request(&request.name, &params)
            .map_err(|_| ErrorData::invalid_params("unknown tool", None))?;

        // Open the read-only recall store for this repo.
        let db = crate::recall_db_path(&self.repo)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let store = crate::open_recall_db(&db)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;

        // Build a `Send` substrate reader. Only retrieve_exact needs real bytes;
        // resolve the one requested blob synchronously (so the non-`Send`
        // `GixSubstrate` never crosses an await), then dispatch over an in-memory
        // reader. (Content-hash → git-oid resolution for exact bytes is a
        // follow-up; the gate + audit are exercised regardless.)
        let reader = build_reader(&self.repo, tool, &req);

        let repo_id = crate::repo_id_for(&self.repo)
            .map_err(|e| ErrorData::internal_error(e.to_string(), None))?;
        let ctx = RecallContext {
            store: &store,
            reader: &reader,
            scope: Scope::default_for(repo_id.as_str(), u32::MAX),
            now: Timestamp::now(),
        };
        let mut server = McpServer::new(TurnBudget::default(), PolicyEngine::new(&self.config));
        let resp = server
            .dispatch(tool, &req, &ctx)
            .await
            .map_err(|e| ErrorData::internal_error(format!("{e:?}"), None))?;

        emit_event(&resp.event);
        Ok(CallToolResult::success(vec![Content::text(
            resp.payload.to_string(),
        )]))
    }
}

/// Build the in-memory substrate reader for a call. For `rift_retrieve_exact`,
/// pre-fetch the one blob the target encodes from the real repo (synchronously),
/// so the dispatch future stays `Send`.
fn build_reader(repo: &Path, tool: ToolName, req: &ToolRequest) -> MemorySubstrate {
    let mut mem = MemorySubstrate::local();
    if tool.is_exact() {
        if let Some(target) = &req.target {
            let sha = rift_recall_mcp::exact_sha(target);
            if let Ok(gix) = GixSubstrate::open(repo) {
                if let Ok(Some(bytes)) = gix.blob(sha) {
                    mem = mem.with_blob(sha, &bytes);
                }
            }
        }
    }
    mem
}

/// Run the stdio MCP server until the agent closes stdin (EOF → graceful exit).
///
/// # Errors
/// [`RiftError::Substrate`] if the transport fails to initialize or run.
pub(crate) async fn serve_mcp(repo: &Path) -> Result<()> {
    let handler = RiftRecallServer::new(repo)?;
    let running = handler
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| RiftError::substrate(format!("mcp serve init: {e}")))?;
    running
        .waiting()
        .await
        .map_err(|e| RiftError::substrate(format!("mcp serve: {e}")))?;
    Ok(())
}

/// The agent MCP-config snippet (`.mcp.json`-style) that spawns this server.
#[must_use]
pub(crate) fn mcp_config_json(repo: &Path) -> String {
    let snippet = serde_json::json!({
        "mcpServers": {
            "rift-recall": {
                "command": "rift-memd",
                "args": ["serve", "--mcp"],
                "cwd": repo.display().to_string(),
            }
        }
    });
    serde_json::to_string_pretty(&snippet).unwrap_or_else(|_| "{}".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertises_exactly_eight_tools_with_schemas() {
        let tools = tool_descriptors();
        assert_eq!(tools.len(), 8);
        // ranking-sensitive tools mark task/files_in_focus required (W5).
        let ks = tools.iter().find(|t| t.name == "rift_known_state").unwrap();
        assert!(ks.input_schema.contains_key("required"));
        // exact tool exists and has a non-empty description.
        let ex = tools
            .iter()
            .find(|t| t.name == "rift_retrieve_exact")
            .unwrap();
        assert!(ex.description.as_ref().is_some_and(|d| !d.is_empty()));
    }

    #[test]
    fn mcp_config_snippet_is_well_formed() {
        let s = mcp_config_json(Path::new("/repo"));
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["mcpServers"]["rift-recall"]["command"], "rift-memd");
        assert_eq!(v["mcpServers"]["rift-recall"]["args"][1], "--mcp");
    }
}
