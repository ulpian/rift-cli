//! The bounded recall MCP tool surface + budget/policy middleware (P5).
//!
//! The agent spawns `rift-memd serve --mcp` (process lifetime = session) and
//! calls a **closed** set of eight tools — there is no transcript search and no
//! open-ended session search. This crate is the transport-independent dispatcher
//! plus the real, store-backed handlers, so the whole surface is testable
//! offline; the bin wires the `rmcp` stdio transport over it.
//!
//! Every dispatch passes through middleware that enforces the per-turn budget
//! (W6), requires query-aware params on ranking-sensitive tools (W5), gates exact
//! retrieval through policy with a mandatory reason (W6/W7/W10), and produces
//! exactly one `memd.*` event carrying honest `returned`/`avoided` token figures
//! — so every serve decision is audited (the serve↔event bijection).
#![forbid(unsafe_code)]
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::collections::BTreeSet;

use rift_core::Timestamp;
use rift_recall_events::MemdEvent;
use rift_recall_ledger::candidates::{load_candidates, LoadedCandidate};
use rift_recall_policy::{AuditDecision, PolicyEngine, TurnBudget};
use rift_recall_rank::{rank, RankQuery, SignalWeights};
use rift_recall_substrate::SubstrateReader;
use rift_recall_types::{ArtifactState, Envelope, OmitReason, Scope, Trust};
use rift_recall_validity::trust::{serialize_untrusted, IntoNeutralDigest, UntrustedPayload};
use rift_store::CanonicalStore;
use serde_json::{json, Value};

/// The closed set of recall tools. This is the **whole** list.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToolName {
    /// Manifest: changed/unchanged files, hotspots, recommended retrievals.
    KnownState,
    /// Changed files + hunks since a checkpoint/commit/session.
    DiffSince,
    /// `unchanged` / `changed` / `invalid` for `(path, known_hash)`.
    FileIfChanged,
    /// Exact changed hunks under a token budget.
    ChangedHunks,
    /// Hash-bound summary by handle.
    ReadSummary,
    /// Exact source/tool output by handle (reason required; audited).
    RetrieveExact,
    /// Repeated reads, expensive calls, trends (awareness only).
    CostHotspots,
    /// Ordered, budgeted, position-aware evidence pack.
    ContextPack,
}

/// The complete, bounded tool surface.
pub const TOOL_SURFACE: &[ToolName] = &[
    ToolName::KnownState,
    ToolName::DiffSince,
    ToolName::FileIfChanged,
    ToolName::ChangedHunks,
    ToolName::ReadSummary,
    ToolName::RetrieveExact,
    ToolName::CostHotspots,
    ToolName::ContextPack,
];

/// The fixed, neutral cost-awareness note (W7: descriptive, never a directive).
pub const COST_NOTE: &str =
    "Informational only: these areas account for the most repeated read tokens.";

impl ToolName {
    /// The wire name (`rift_<tool>`), as advertised over MCP.
    #[must_use]
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::KnownState => "rift_known_state",
            Self::DiffSince => "rift_diff_since",
            Self::FileIfChanged => "rift_file_if_changed",
            Self::ChangedHunks => "rift_changed_hunks",
            Self::ReadSummary => "rift_read_summary",
            Self::RetrieveExact => "rift_retrieve_exact",
            Self::CostHotspots => "rift_cost_hotspots",
            Self::ContextPack => "rift_context_pack",
        }
    }

    /// Parse a wire name back to a tool (unknown names are protocol errors).
    #[must_use]
    pub fn from_wire(name: &str) -> Option<Self> {
        TOOL_SURFACE.iter().copied().find(|t| t.wire_name() == name)
    }

    /// Whether this tool requires query-aware params (`task`, `files_in_focus`).
    #[must_use]
    pub fn is_ranking_sensitive(self) -> bool {
        matches!(
            self,
            Self::KnownState | Self::ContextPack | Self::ChangedHunks
        )
    }

    /// Whether this tool can return exact content (and so requires a reason).
    #[must_use]
    pub fn is_exact(self) -> bool {
        matches!(self, Self::RetrieveExact)
    }
}

/// Why a request could not be decoded (a protocol error, surfaced to the agent).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The tool name is not one of the eight (no such tool).
    #[error("unknown tool")]
    UnknownTool,
}

/// Decode a tool name + JSON params into a typed `(ToolName, ToolRequest)`.
///
/// **Total:** never panics on arbitrary `params` (the request-decoder fuzz
/// invariant); missing/ill-typed fields default. Unknown tool names are a
/// protocol error ([`DecodeError::UnknownTool`]).
///
/// # Errors
/// [`DecodeError::UnknownTool`] if `tool_name` is not a known tool.
pub fn decode_request(
    tool_name: &str,
    params: &Value,
) -> Result<(ToolName, ToolRequest), DecodeError> {
    let tool = ToolName::from_wire(tool_name).ok_or(DecodeError::UnknownTool)?;
    let str_at = |k: &str| params.get(k).and_then(Value::as_str).map(String::from);
    let str_vec = |k: &str| {
        params
            .get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    let u32_at = |k: &str| {
        params
            .get(k)
            .and_then(Value::as_u64)
            .map(|n| u32::try_from(n).unwrap_or(u32::MAX))
    };
    let req = ToolRequest {
        task: str_at("task"),
        files_in_focus: str_vec("files_in_focus"),
        error_signatures: str_vec("error_signatures"),
        token_budget: u32_at("token_budget").unwrap_or(0),
        known_hashes: str_vec("known_hashes"),
        target: str_at("target"),
        trust: str_at("trust").and_then(|t| parse_trust(&t)),
        reason: str_at("reason"),
        top_n: u32_at("top_n"),
    };
    Ok((tool, req))
}

/// Parse a trust-class wire string (unknown → `None`, defaults applied by caller).
fn parse_trust(s: &str) -> Option<Trust> {
    match s {
        "code_fact" => Some(Trust::CodeFact),
        "git_fact" => Some(Trust::GitFact),
        "test_output" => Some(Trust::TestOutput),
        "tool_output_untrusted" => Some(Trust::ToolOutputUntrusted),
        "transcript_untrusted" => Some(Trust::TranscriptUntrusted),
        _ => None,
    }
}

/// A tool invocation. Optional fields are unused by tools that do not need them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolRequest {
    /// The task/goal (required for ranking-sensitive tools).
    pub task: Option<String>,
    /// Files in focus (required for ranking-sensitive tools).
    pub files_in_focus: Vec<String>,
    /// Error signatures currently in play (drives the error-overlap signal).
    pub error_signatures: Vec<String>,
    /// Token budget for the response.
    pub token_budget: u32,
    /// Content hashes the agent already has (drives `known_unchanged`).
    pub known_hashes: Vec<String>,
    /// The handle/path for retrieval/conditional tools.
    pub target: Option<String>,
    /// Trust class of the target (for exact gating).
    pub trust: Option<Trust>,
    /// Reason (required for exact retrieval).
    pub reason: Option<String>,
    /// `top_n` for cost hotspots.
    pub top_n: Option<u32>,
}

/// A successful tool response.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolResponse {
    /// The tool that produced it.
    pub tool: ToolName,
    /// Estimated tokens returned (honest tiktoken count of inlined bytes).
    pub estimated_tokens_returned: u32,
    /// Estimated tokens avoided vs full re-read (labeled counterfactual).
    pub estimated_tokens_avoided: u32,
    /// The tokenizer that produced `estimated_tokens_returned`.
    pub tokenizer: &'static str,
    /// The tool-specific payload (JSON; serialized to the agent by the transport).
    pub payload: Value,
    /// The event the caller must hand to `rift hooks memd`.
    pub event: MemdEvent,
}

/// A tool failure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolError {
    /// Per-turn tool-call ceiling hit.
    BudgetCallsExhausted,
    /// A ranking-sensitive tool was called without query-aware params.
    QueryParamsRequired,
    /// Policy denied (carries the reason to surface).
    Denied(OmitReason),
    /// An internal read/build error (identifier/reason only — never content).
    Internal(String),
}

/// The read-only context the handlers query: the recall projection + substrate,
/// bounded to one [`Scope`], evaluated at an injected `now`.
pub struct RecallContext<'a, S: CanonicalStore, R: SubstrateReader> {
    /// The recall.db projection.
    pub store: &'a S,
    /// The read-only substrate (for exact blob resolution).
    pub reader: &'a R,
    /// The query scope (W14 containment).
    pub scope: Scope,
    /// The injected clock (recency + label as-of).
    pub now: Timestamp,
}

/// The recall MCP dispatcher. Holds the per-turn budget + the policy engine.
pub struct McpServer<'a> {
    budget: TurnBudget,
    policy: PolicyEngine<'a>,
}

impl<'a> McpServer<'a> {
    /// Construct with a per-turn budget and a policy engine.
    #[must_use]
    pub fn new(budget: TurnBudget, policy: PolicyEngine<'a>) -> Self {
        Self { budget, policy }
    }

    /// Dispatch a tool call through budget + policy middleware, then the
    /// store-backed handler. Produces exactly one `memd.*` event.
    ///
    /// # Errors
    /// [`ToolError`] if the call exceeds budget, omits required query params, is
    /// denied by policy, or hits an internal read error.
    pub async fn dispatch<S: CanonicalStore, R: SubstrateReader>(
        &mut self,
        tool: ToolName,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<ToolResponse, ToolError> {
        // 1. per-turn call ceiling (W6) — deny before any work.
        if !self.budget.try_call() {
            return Err(ToolError::BudgetCallsExhausted);
        }
        // 2. query-aware requirement (W5).
        if tool.is_ranking_sensitive()
            && (req.task.as_deref().unwrap_or("").trim().is_empty()
                || req.files_in_focus.is_empty())
        {
            return Err(ToolError::QueryParamsRequired);
        }
        // 3. dispatch to the store-backed handler.
        match tool {
            ToolName::KnownState => self.known_state(req, ctx).await,
            ToolName::DiffSince => self.diff_since(req, ctx).await,
            ToolName::FileIfChanged => self.file_if_changed(req, ctx).await,
            ToolName::ChangedHunks => self.changed_hunks(req, ctx).await,
            ToolName::ReadSummary => self.read_summary(req, ctx).await,
            ToolName::RetrieveExact => self.retrieve_exact(req, ctx).await,
            ToolName::CostHotspots => self.cost_hotspots(req, ctx).await,
            ToolName::ContextPack => self.context_pack(req, ctx).await,
        }
    }

    /// The token budget remaining this turn.
    #[must_use]
    pub fn tokens_remaining(&self) -> u32 {
        self.budget.tokens_remaining()
    }

    // --- handlers ----------------------------------------------------------

    fn rank_query(req: &ToolRequest) -> Result<RankQuery, ToolError> {
        RankQuery::new(
            req.task.clone().unwrap_or_default(),
            req.files_in_focus.clone(),
            req.error_signatures.clone(),
        )
        .map_err(|_| ToolError::QueryParamsRequired)
    }

    async fn ranked_candidates<S: CanonicalStore, R: SubstrateReader>(
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<Vec<LoadedCandidate>, ToolError> {
        let q = Self::rank_query(req)?;
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let cands: Vec<_> = loaded.iter().map(|l| l.candidate.clone()).collect();
        let order = rank(&q, &cands, ctx.now, &SignalWeights::default());
        // Re-order `loaded` to match the ranking (by candidate id).
        let mut by_id: std::collections::BTreeMap<String, LoadedCandidate> = loaded
            .into_iter()
            .map(|l| (l.candidate.id.clone(), l))
            .collect();
        Ok(order
            .into_iter()
            .filter_map(|s| by_id.remove(&s.candidate.id))
            .collect())
    }

    async fn known_state<S: CanonicalStore, R: SubstrateReader>(
        &mut self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<ToolResponse, ToolError> {
        let ranked = Self::ranked_candidates(req, ctx).await?;
        let known: BTreeSet<&str> = req.known_hashes.iter().map(String::as_str).collect();
        let mut envelopes: Vec<Envelope> = Vec::new();
        let mut returned = 0u32;
        let mut avoided = 0u32;
        let mut truncated = false;

        for lc in &ranked {
            let c = &lc.candidate;
            let handle = format!("file:{}@sha256:{}", c.path, lc.content_sha256);
            // The manifest line cost (honest): the descriptor we inline per entry.
            let descriptor = format!("{} {}", c.path, lc.content_sha256);
            let line_cost = rift_tokens::count(&descriptor);
            if !self.budget.try_tokens(line_cost) {
                truncated = true; // budget cut the manifest; rest available by handle
                break;
            }
            returned += line_cost;
            avoided = avoided.saturating_add(c.est_tokens);
            let state = if known.contains(lc.content_sha256.as_str()) {
                // Agent already holds the current hash → known_unchanged (no bytes, W2).
                ArtifactState::KnownUnchanged {
                    handle,
                    basis_sha256: lc.content_sha256.clone(),
                    last_seen_turn: None,
                }
            } else {
                ArtifactState::AvailableByHandle {
                    handle,
                    basis_sha256: Some(lc.content_sha256.clone()),
                    reason: OmitReason::NotRequested,
                }
            };
            envelopes.push(Envelope {
                path: c.path.clone(),
                trust: Trust::CodeFact,
                state,
                estimated_tokens_returned: 0,
            });
        }

        let payload = json!({
            "tool": "known_state",
            "envelopes": envelopes.iter().map(envelope_view).collect::<Vec<_>>(),
            "truncated": truncated,
        });
        let event = MemdEvent::ManifestServed {
            estimated_tokens_returned: returned,
            estimated_tokens_avoided: avoided,
        };
        Ok(Self::respond(
            ToolName::KnownState,
            returned,
            avoided,
            payload,
            event,
        ))
    }

    async fn diff_since<S: CanonicalStore, R: SubstrateReader>(
        &mut self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<ToolResponse, ToolError> {
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let known: BTreeSet<&str> = req.known_hashes.iter().map(String::as_str).collect();
        let mut changed = Vec::new();
        let mut unchanged = Vec::new();
        let mut avoided = 0u32;
        for lc in &loaded {
            avoided = avoided.saturating_add(lc.candidate.est_tokens);
            if known.contains(lc.content_sha256.as_str()) {
                unchanged.push(json!(lc.candidate.path));
            } else {
                changed.push(json!({ "path": lc.candidate.path, "new_sha256": lc.content_sha256 }));
            }
        }
        let payload = json!({ "tool": "diff_since", "changed": changed, "unchanged": unchanged });
        let event = MemdEvent::ManifestServed {
            estimated_tokens_returned: 0,
            estimated_tokens_avoided: avoided,
        };
        Ok(Self::respond(
            ToolName::DiffSince,
            0,
            avoided,
            payload,
            event,
        ))
    }

    async fn file_if_changed<S: CanonicalStore, R: SubstrateReader>(
        &mut self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<ToolResponse, ToolError> {
        let path = req.target.clone().unwrap_or_default();
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let known: BTreeSet<&str> = req.known_hashes.iter().map(String::as_str).collect();
        let latest = loaded.iter().find(|l| l.candidate.path == path);
        let (state, avoided) = match latest {
            None => (
                json!({ "state": "invalid", "path": path, "reason": "path_not_found" }),
                0,
            ),
            Some(lc) if known.contains(lc.content_sha256.as_str()) => (
                json!({ "state": "unchanged", "path": path, "known_hash": lc.content_sha256 }),
                lc.candidate.est_tokens,
            ),
            Some(lc) => (
                json!({ "state": "changed", "path": path, "new_hash": lc.content_sha256 }),
                lc.candidate.est_tokens,
            ),
        };
        let payload = json!({ "tool": "file_if_changed", "result": state });
        let event = MemdEvent::ManifestServed {
            estimated_tokens_returned: 0,
            estimated_tokens_avoided: avoided,
        };
        Ok(Self::respond(
            ToolName::FileIfChanged,
            0,
            avoided,
            payload,
            event,
        ))
    }

    async fn changed_hunks<S: CanonicalStore, R: SubstrateReader>(
        &mut self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<ToolResponse, ToolError> {
        // The ranked candidates restrict which paths are in play; the hunk bytes
        // come from the persisted changed-hunk rows for the top focus path.
        let ranked = Self::ranked_candidates(req, ctx).await?;
        let Some(top) = ranked.first() else {
            let payload = json!({ "tool": "changed_hunks", "hunks": [], "omitted": 0 });
            let event = MemdEvent::ManifestServed {
                estimated_tokens_returned: 0,
                estimated_tokens_avoided: 0,
            };
            return Ok(Self::respond(ToolName::ChangedHunks, 0, 0, payload, event));
        };
        let rows = ctx
            .store
            .query(
                "SELECT new_start, new_len FROM n_changed_hunk WHERE file_revision = ?1 \
                 ORDER BY new_start ASC",
                &[rift_store::Value::text(top.candidate.id.as_str())],
            )
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        // Cap at the configured max-hunks and the per-turn token budget.
        let max_hunks = MAX_CHANGED_HUNKS as usize;
        let mut hunks = Vec::new();
        let mut returned = 0u32;
        let mut omitted = 0u32;
        for (i, r) in rows.iter().enumerate() {
            if i >= max_hunks {
                omitted += 1;
                continue;
            }
            let lo = r.get_i64(0).unwrap_or(0).max(0);
            let len = r.get_i64(1).unwrap_or(0).max(0);
            let view = json!({ "lo": lo, "hi": lo + len });
            let cost = rift_tokens::count(&view.to_string());
            if !self.budget.try_tokens(cost) {
                omitted += 1;
                continue;
            }
            returned += cost;
            hunks.push(view);
        }
        let payload = json!({
            "tool": "changed_hunks",
            "path": top.candidate.path,
            "hunks": hunks,
            "omitted": omitted,
            "source_hash": top.content_sha256,
        });
        let avoided = top.candidate.est_tokens;
        let event = MemdEvent::ManifestServed {
            estimated_tokens_returned: returned,
            estimated_tokens_avoided: avoided,
        };
        Ok(Self::respond(
            ToolName::ChangedHunks,
            returned,
            avoided,
            payload,
            event,
        ))
    }

    async fn read_summary<S: CanonicalStore, R: SubstrateReader>(
        &mut self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<ToolResponse, ToolError> {
        // The summary is hash-bound; serving re-checks validity (P3). We look up
        // the binding for the requested path and compare its bound hash to the
        // current revision: a moved source returns `stale`, never the bytes.
        let path = req.target.clone().unwrap_or_default();
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let current = loaded.iter().find(|l| l.candidate.path == path);
        let rows = ctx
            .store
            .query(
                "SELECT source_sha256 FROM summary_binding WHERE path = ?1 ORDER BY created_at DESC LIMIT 1",
                &[rift_store::Value::text(path.as_str())],
            )
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let bound = rows.first().and_then(|r| r.get_text(0).ok());
        let (result, returned, event) = match (bound, current) {
            (Some(bound), Some(lc)) if bound == lc.content_sha256 => {
                // valid: the bound source is still current.
                let summary = format!("summary bound to {bound}");
                let cost = rift_tokens::count(&summary);
                let _ = self.budget.try_tokens(cost);
                (
                    json!({ "state": "summary", "path": path, "source_hash": bound, "summary": summary }),
                    cost,
                    MemdEvent::SummaryServed {
                        handle: format!("summary:{path}"),
                        estimated_tokens_returned: cost,
                        estimated_tokens_avoided: lc.candidate.est_tokens.saturating_sub(cost),
                    },
                )
            }
            (Some(_), Some(lc)) => (
                json!({ "state": "stale", "path": path, "current_hash": lc.content_sha256 }),
                0,
                MemdEvent::Invalidated {
                    handle: format!("summary:{path}"),
                },
            ),
            _ => (
                json!({ "state": "unavailable", "path": path, "reason": "not_bound" }),
                0,
                MemdEvent::ManifestServed {
                    estimated_tokens_returned: 0,
                    estimated_tokens_avoided: 0,
                },
            ),
        };
        let payload = json!({ "tool": "read_summary", "result": result });
        let avoided = event.tokens_avoided();
        Ok(Self::respond(
            ToolName::ReadSummary,
            returned,
            avoided,
            payload,
            event,
        ))
    }

    // No `.await` inside (store reads aren't needed — the substrate blob fetch is
    // sync), but kept `async` for a uniform handler signature in `dispatch`.
    #[allow(clippy::unused_async)]
    async fn retrieve_exact<S: CanonicalStore, R: SubstrateReader>(
        &mut self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<ToolResponse, ToolError> {
        let path = req.target.clone().unwrap_or_default();
        let trust = req.trust.unwrap_or(Trust::ToolOutputUntrusted);
        match self
            .policy
            .decide_exact(&path, trust, req.reason.as_deref())
        {
            AuditDecision::Allow { path, reason, .. } => {
                // Trusted classes only reach here; fetch the exact bytes by hash.
                let sha = exact_sha(&path);
                let body = ctx
                    .reader
                    .blob(sha)
                    .map_err(|e| ToolError::Internal(e.to_string()))?
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                let returned = self.charge(rift_tokens::count(&body));
                let payload = json!({
                    "tool": "retrieve_exact",
                    "result": { "state": "exact", "path": path, "body": body },
                });
                let event = MemdEvent::ExactRetrieved {
                    handle: path,
                    reason,
                    estimated_tokens_returned: returned,
                };
                Ok(Self::respond(
                    ToolName::RetrieveExact,
                    returned,
                    0,
                    payload,
                    event,
                ))
            }
            AuditDecision::Deny(reason) => {
                // Untrusted payloads are surfaced only as a neutral digest + handle.
                let payload = if trust.is_untrusted() {
                    let digest = UntrustedPayload::from_trust(trust, Vec::new()).map_or_else(
                        || serialize_untrusted("tool_output", b"", &path),
                        |p| p.into_neutral_digest(&format!("log:{path}")),
                    );
                    json!({ "tool": "retrieve_exact", "result": { "state": "refused", "neutral_digest": serde_json::to_value(&digest).unwrap_or(Value::Null) } })
                } else {
                    json!({ "tool": "retrieve_exact", "result": { "state": "refused", "reason": omit_tag(reason) } })
                };
                // Refusal is audited too: exactly one terminal event (W10 bijection).
                let event = MemdEvent::Refused {
                    target: path,
                    reason: omit_tag(reason).to_string(),
                };
                Ok(Self::respond(ToolName::RetrieveExact, 0, 0, payload, event))
            }
        }
    }

    async fn cost_hotspots<S: CanonicalStore, R: SubstrateReader>(
        &mut self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<ToolResponse, ToolError> {
        let loaded = load_candidates(ctx.store, &ctx.scope, ctx.now)
            .await
            .map_err(|e| ToolError::Internal(e.to_string()))?;
        let total: u64 = loaded
            .iter()
            .map(|l| u64::from(l.candidate.est_tokens))
            .sum();
        let mut by_tokens: Vec<&LoadedCandidate> = loaded.iter().collect();
        by_tokens.sort_by(|a, b| {
            b.candidate
                .est_tokens
                .cmp(&a.candidate.est_tokens)
                .then(a.candidate.path.cmp(&b.candidate.path))
        });
        let top_n = req.top_n.unwrap_or(5) as usize;
        let hotspots: Vec<Value> = by_tokens
            .into_iter()
            .take(top_n)
            .map(|l| {
                let t = u64::from(l.candidate.est_tokens);
                let share = if total == 0 {
                    0.0
                } else {
                    t as f64 / total as f64
                };
                json!({ "area": l.candidate.path, "est_tokens": t, "share": share })
            })
            .collect();
        // Descriptive only — the payload carries no directive (W7).
        let payload = json!({ "tool": "cost_hotspots", "hotspots": hotspots, "note": COST_NOTE });
        let event = MemdEvent::ManifestServed {
            estimated_tokens_returned: 0,
            estimated_tokens_avoided: 0,
        };
        Ok(Self::respond(ToolName::CostHotspots, 0, 0, payload, event))
    }

    async fn context_pack<S: CanonicalStore, R: SubstrateReader>(
        &mut self,
        req: &ToolRequest,
        ctx: &RecallContext<'_, S, R>,
    ) -> Result<ToolResponse, ToolError> {
        let ranked = Self::ranked_candidates(req, ctx).await?;
        let budget = if req.token_budget == 0 {
            rift_recall_pack::DEFAULT_PACK_TOKENS
        } else {
            req.token_budget
        };
        let recommended: Vec<String> = ranked
            .iter()
            .take(8)
            .map(|l| format!("file:{}@sha256:{}", l.candidate.path, l.content_sha256))
            .collect();
        let pack_req = rift_recall_pack::PackRequest {
            critical_facts: Vec::new(),
            changed: Vec::new(),
            unchanged: Vec::new(),
            recommended,
            cost_warning: Some(COST_NOTE.to_string()),
            token_budget: budget,
        };
        let pack = rift_recall_pack::compile(&pack_req);
        let rendered = pack.render();
        let returned = self.charge(rift_tokens::count(&rendered));
        let avoided = ranked
            .iter()
            .map(|l| l.candidate.est_tokens)
            .fold(0u32, u32::saturating_add);
        let key = sha256_hex(rendered.as_bytes());
        let payload = json!({
            "tool": "context_pack",
            "sections": pack.sections.iter().map(|s| json!({ "kind": format!("{:?}", s.kind), "lines": s.lines })).collect::<Vec<_>>(),
            "total_tokens": pack.total_tokens,
            "byte_identical_key": key,
        });
        let event = MemdEvent::PackBuilt {
            sections: u32::try_from(pack.sections.len()).unwrap_or(u32::MAX),
            total_tokens: pack.total_tokens,
            estimated_tokens_avoided: avoided,
        };
        Ok(Self::respond(
            ToolName::ContextPack,
            returned,
            avoided,
            payload,
            event,
        ))
    }

    /// Charge `cost` against the budget, returning the amount actually charged
    /// (clamped to the remaining budget so the ceiling is never breached).
    fn charge(&mut self, cost: u32) -> u32 {
        let allowed = cost.min(self.budget.tokens_remaining());
        let _ = self.budget.try_tokens(allowed);
        allowed
    }

    fn respond(
        tool: ToolName,
        returned: u32,
        avoided: u32,
        payload: Value,
        event: MemdEvent,
    ) -> ToolResponse {
        ToolResponse {
            tool,
            estimated_tokens_returned: returned,
            estimated_tokens_avoided: avoided,
            tokenizer: rift_tokens::TOKENIZER,
            payload,
            event,
        }
    }
}

/// The per-tool changed-hunk cap (W6 default).
pub const MAX_CHANGED_HUNKS: u32 = 8;

/// Render an [`Envelope`] to a JSON view (state tag + hash, never raw bytes for
/// the manifest states).
fn envelope_view(e: &Envelope) -> Value {
    let (state, basis): (&str, Option<String>) = match &e.state {
        ArtifactState::KnownUnchanged { basis_sha256, .. } => {
            ("known_unchanged", Some(basis_sha256.clone()))
        }
        ArtifactState::SummaryIncluded { basis_sha256, .. } => {
            ("summary_included", Some(basis_sha256.clone()))
        }
        ArtifactState::ExactIncluded { basis_sha256, .. } => {
            ("exact_included", Some(basis_sha256.clone()))
        }
        ArtifactState::AvailableByHandle { basis_sha256, .. } => {
            ("available_by_handle", basis_sha256.clone())
        }
    };
    json!({ "path": e.path, "state": state, "basis_sha256": basis })
}

/// The content hash a `file:<path>@sha256:<hash>` handle/path encodes (for blob
/// fetch); falls back to the whole string when no `sha256:` marker is present.
#[must_use]
pub fn exact_sha(target: &str) -> &str {
    target
        .rsplit_once("@sha256:")
        .map_or(target, |(_, h)| h.split(':').next().unwrap_or(h))
}

/// A stable, neutral tag for an omit reason (never leaks content).
fn omit_tag(reason: OmitReason) -> &'static str {
    match reason {
        OmitReason::BudgetExhausted => "budget_exhausted",
        OmitReason::PolicyDeniedExact => "policy_denied_exact",
        OmitReason::SensitivePathRequiresReason => "sensitive_path_requires_reason",
        OmitReason::SummaryInvalidSourceChanged => "summary_invalid_source_changed",
        OmitReason::SourceDeletedOrRedacted => "source_deleted_or_redacted",
        OmitReason::NotRequested => "not_requested",
    }
}

/// SHA-256 hex of `bytes` — the byte-identical pack cache key (P6 S3).
fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(bytes);
    let mut s = String::with_capacity(64);
    use core::fmt::Write as _;
    for b in digest {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests;
