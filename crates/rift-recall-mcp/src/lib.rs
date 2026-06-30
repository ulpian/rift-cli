//! The bounded recall MCP tool surface + budget/policy middleware.
//!
//! The agent spawns `rift-memd serve --mcp` (process lifetime = session) and
//! calls a **closed** set of eight tools — there is no transcript search and no
//! open-ended session search. Every dispatch passes through middleware that
//! enforces the per-turn budgets (W6), requires query-aware params on
//! ranking-sensitive tools (W5), requires a `reason` for exact retrieval, and
//! emits a `memd.*` event carrying tokens returned/avoided.
//!
//! The `rmcp` stdio transport is the seam; this crate is the transport-independent
//! dispatcher so it is fully testable offline.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

use rift_recall_events::MemdEvent;
use rift_recall_policy::{AuditDecision, PolicyEngine, TurnBudget};
use rift_recall_types::{OmitReason, Trust};

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

impl ToolName {
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

/// A tool invocation.
#[derive(Clone, Debug, Default)]
pub struct ToolRequest {
    /// The task/goal (required for ranking-sensitive tools).
    pub task: Option<String>,
    /// Files in focus (required for ranking-sensitive tools).
    pub files_in_focus: Vec<String>,
    /// Token budget for the response.
    pub token_budget: u32,
    /// The handle/path for retrieval tools.
    pub target: Option<String>,
    /// Trust class of the target (for exact gating).
    pub trust: Option<Trust>,
    /// Reason (required for exact retrieval).
    pub reason: Option<String>,
}

/// A successful tool response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolResponse {
    /// The tool that produced it.
    pub tool: ToolName,
    /// Estimated tokens returned.
    pub estimated_tokens_returned: u32,
    /// Estimated tokens avoided vs full re-read.
    pub estimated_tokens_avoided: u32,
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
}

/// The recall MCP dispatcher. Holds the per-turn budget; the agent's config
/// spawns it for the session.
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

    /// Dispatch a tool call through budget + policy middleware.
    ///
    /// # Errors
    /// [`ToolError`] if the call exceeds budget, omits required query params, or
    /// is denied by policy.
    pub fn dispatch(
        &mut self,
        tool: ToolName,
        req: &ToolRequest,
    ) -> Result<ToolResponse, ToolError> {
        // 1. per-turn call ceiling (W6)
        if !self.budget.try_call() {
            return Err(ToolError::BudgetCallsExhausted);
        }
        // 2. query-aware requirement (W5)
        if tool.is_ranking_sensitive()
            && (req.task.as_deref().unwrap_or("").is_empty() || req.files_in_focus.is_empty())
        {
            return Err(ToolError::QueryParamsRequired);
        }
        // 3. exact retrieval: reason required + policy + audit (W6/W7/W10)
        if tool.is_exact() {
            let path = req.target.as_deref().unwrap_or("");
            let trust = req.trust.unwrap_or(Trust::ToolOutputUntrusted);
            match self.policy.decide_exact(path, trust, req.reason.as_deref()) {
                AuditDecision::Allow { path, reason, .. } => {
                    let returned = 200.min(self.budget.tokens_remaining());
                    if !self.budget.try_tokens(returned) {
                        return Err(ToolError::Denied(OmitReason::BudgetExhausted));
                    }
                    let event = MemdEvent::ExactRetrieved {
                        handle: path,
                        reason,
                        estimated_tokens_returned: returned,
                    };
                    return Ok(ToolResponse {
                        tool,
                        estimated_tokens_returned: returned,
                        estimated_tokens_avoided: 0,
                        event,
                    });
                }
                AuditDecision::Deny(reason) => return Err(ToolError::Denied(reason)),
            }
        }
        // 4. manifest-first tools: cheap response, avoided > returned
        let returned = 50.min(req.token_budget.max(1));
        let _ = self.budget.try_tokens(returned);
        let avoided = req.token_budget.saturating_mul(4); // illustrative: deltas avoid full re-reads
        let event = MemdEvent::ManifestServed {
            estimated_tokens_returned: returned,
            estimated_tokens_avoided: avoided,
        };
        Ok(ToolResponse {
            tool,
            estimated_tokens_returned: returned,
            estimated_tokens_avoided: avoided,
            event,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_config::EffectiveConfig;

    fn cfg() -> EffectiveConfig {
        EffectiveConfig::secure_default().unwrap()
    }

    fn ranking_req() -> ToolRequest {
        ToolRequest {
            task: Some("fix retry".into()),
            files_in_focus: vec!["src/retry.rs".into()],
            token_budget: 1000,
            ..ToolRequest::default()
        }
    }

    #[test]
    fn tool_surface_is_bounded_to_eight() {
        assert_eq!(TOOL_SURFACE.len(), 8);
        // there is no transcript-search / session-search tool
        assert!(!TOOL_SURFACE
            .iter()
            .any(|t| format!("{t:?}").to_lowercase().contains("transcript")));
    }

    #[test]
    fn ranking_tool_requires_query_params() {
        let cfg = cfg();
        let mut s = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
        let bare = ToolRequest {
            token_budget: 1000,
            ..ToolRequest::default()
        };
        assert_eq!(
            s.dispatch(ToolName::KnownState, &bare),
            Err(ToolError::QueryParamsRequired)
        );
        assert!(s.dispatch(ToolName::ContextPack, &ranking_req()).is_ok());
    }

    #[test]
    fn per_turn_call_budget_is_enforced() {
        let cfg = cfg();
        let mut s = McpServer::new(TurnBudget::new(2, 10_000), PolicyEngine::new(&cfg));
        assert!(s
            .dispatch(
                ToolName::CostHotspots,
                &ToolRequest {
                    token_budget: 100,
                    ..Default::default()
                }
            )
            .is_ok());
        assert!(s
            .dispatch(
                ToolName::CostHotspots,
                &ToolRequest {
                    token_budget: 100,
                    ..Default::default()
                }
            )
            .is_ok());
        assert_eq!(
            s.dispatch(
                ToolName::CostHotspots,
                &ToolRequest {
                    token_budget: 100,
                    ..Default::default()
                }
            ),
            Err(ToolError::BudgetCallsExhausted)
        );
    }

    #[test]
    fn exact_requires_reason_and_audits() {
        let cfg = cfg();
        let mut s = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
        let no_reason = ToolRequest {
            target: Some("src/main.rs".into()),
            trust: Some(Trust::CodeFact),
            token_budget: 1000,
            ..Default::default()
        };
        assert_eq!(
            s.dispatch(ToolName::RetrieveExact, &no_reason),
            Err(ToolError::Denied(OmitReason::PolicyDeniedExact))
        );
        let with_reason = ToolRequest {
            reason: Some("debugging".into()),
            ..no_reason
        };
        let resp = s.dispatch(ToolName::RetrieveExact, &with_reason).unwrap();
        assert!(matches!(resp.event, MemdEvent::ExactRetrieved { .. }));
    }

    #[test]
    fn manifest_reports_tokens_avoided() {
        let cfg = cfg();
        let mut s = McpServer::new(TurnBudget::default(), PolicyEngine::new(&cfg));
        let resp = s.dispatch(ToolName::KnownState, &ranking_req()).unwrap();
        assert!(resp.estimated_tokens_avoided > resp.estimated_tokens_returned);
    }
}
