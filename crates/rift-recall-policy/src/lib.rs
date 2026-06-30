//! Recall serving policy: per-turn budgets, sensitive-path gates, trust
//! enforcement, and audited exact retrieval.
//!
//! The serve gate is policy-first (`05-security` §5.5): the default response is a
//! manifest, not content; exact content requires a `reason`, passes policy, and
//! is audited. Budgets bound over-retrieval (W6); sensitive paths force a reason
//! (W7/W10); untrusted content is never served raw (W9). Every granted exact
//! retrieval yields an [`AuditDecision::Allow`] carrying the audit record the
//! caller must emit through `rift hooks memd`.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

use rift_config::EffectiveConfig;
use rift_recall_types::{OmitReason, Trust};

/// Default per-turn tool-call ceiling.
pub const DEFAULT_MAX_TOOL_CALLS: u32 = 4;
/// Default per-turn returned-token ceiling.
pub const DEFAULT_MAX_TOKENS_PER_TURN: u32 = 2000;

/// A per-turn budget bounding over-retrieval.
#[derive(Clone, Copy, Debug)]
pub struct TurnBudget {
    max_tool_calls: u32,
    max_tokens: u32,
    calls_used: u32,
    tokens_used: u32,
}

impl Default for TurnBudget {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_TOOL_CALLS, DEFAULT_MAX_TOKENS_PER_TURN)
    }
}

impl TurnBudget {
    /// A budget with explicit ceilings.
    #[must_use]
    pub fn new(max_tool_calls: u32, max_tokens: u32) -> Self {
        Self {
            max_tool_calls,
            max_tokens,
            calls_used: 0,
            tokens_used: 0,
        }
    }

    /// Try to account for one tool call. Returns `false` if the ceiling is hit.
    #[must_use]
    pub fn try_call(&mut self) -> bool {
        if self.calls_used >= self.max_tool_calls {
            return false;
        }
        self.calls_used += 1;
        true
    }

    /// Try to account for `tokens` returned. Returns `false` if it would exceed
    /// the ceiling (and accounts nothing).
    #[must_use]
    pub fn try_tokens(&mut self, tokens: u32) -> bool {
        if self.tokens_used + tokens > self.max_tokens {
            return false;
        }
        self.tokens_used += tokens;
        true
    }

    /// Remaining tokens this turn.
    #[must_use]
    pub fn tokens_remaining(&self) -> u32 {
        self.max_tokens.saturating_sub(self.tokens_used)
    }
}

/// The decision for an exact-retrieval request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditDecision {
    /// Allowed — emit this audit record (`memd.exact_retrieved`).
    Allow {
        /// The path retrieved.
        path: String,
        /// The supplied reason.
        reason: String,
        /// Whether the path was sensitive.
        sensitive: bool,
    },
    /// Denied — surface this omit reason; no content is served.
    Deny(OmitReason),
}

/// The serving policy engine, parameterized by the effective project config.
pub struct PolicyEngine<'a> {
    config: &'a EffectiveConfig,
}

impl<'a> PolicyEngine<'a> {
    /// Wrap an effective config.
    #[must_use]
    pub fn new(config: &'a EffectiveConfig) -> Self {
        Self { config }
    }

    /// Whether a model is allowed (deny wins; empty allow = allow-all).
    #[must_use]
    pub fn model_allowed(&self, model: &str) -> bool {
        self.config.is_model_allowed(model)
    }

    /// Decide an exact-retrieval request. Reason is mandatory; sensitive paths
    /// require a reason; untrusted content is never served raw; transcripts are
    /// off unless explicitly enabled. A grant returns the audit record to emit.
    #[must_use]
    pub fn decide_exact(&self, path: &str, trust: Trust, reason: Option<&str>) -> AuditDecision {
        // untrusted content is never served as raw exact text
        if !trust.allows_raw_inline() {
            // transcripts only if explicitly enabled, and even then never raw here
            return AuditDecision::Deny(OmitReason::PolicyDeniedExact);
        }
        let sensitive = self.config.is_sensitive_path(path);
        match reason {
            None | Some("") => {
                if sensitive {
                    AuditDecision::Deny(OmitReason::SensitivePathRequiresReason)
                } else {
                    // exact retrieval always requires a reason (W6)
                    AuditDecision::Deny(OmitReason::PolicyDeniedExact)
                }
            }
            Some(r) => AuditDecision::Allow {
                path: path.to_string(),
                reason: r.to_string(),
                sensitive,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> EffectiveConfig {
        // sensitive default includes **/.env*, **/*secret* etc.
        EffectiveConfig::secure_default().unwrap()
    }

    #[test]
    fn budget_caps_calls_and_tokens() {
        let mut b = TurnBudget::new(2, 100);
        assert!(b.try_call());
        assert!(b.try_call());
        assert!(!b.try_call(), "third call exceeds ceiling");
        assert!(b.try_tokens(60));
        assert!(!b.try_tokens(60), "would exceed token ceiling");
        assert!(b.try_tokens(40));
        assert_eq!(b.tokens_remaining(), 0);
    }

    #[test]
    fn exact_requires_reason() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        assert_eq!(
            p.decide_exact("src/main.rs", Trust::CodeFact, None),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
        assert!(matches!(
            p.decide_exact("src/main.rs", Trust::CodeFact, Some("debugging the crash")),
            AuditDecision::Allow { .. }
        ));
    }

    #[test]
    fn sensitive_path_requires_reason_specifically() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        assert_eq!(
            p.decide_exact("config/.env", Trust::CodeFact, None),
            AuditDecision::Deny(OmitReason::SensitivePathRequiresReason)
        );
        // with a reason, allowed but flagged sensitive (audited)
        match p.decide_exact(
            "config/.env",
            Trust::CodeFact,
            Some("inspecting prod config"),
        ) {
            AuditDecision::Allow { sensitive, .. } => assert!(sensitive),
            other => panic!("expected allow, got {other:?}"),
        }
    }

    #[test]
    fn untrusted_content_is_never_exact() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        assert_eq!(
            p.decide_exact("tool.out", Trust::ToolOutputUntrusted, Some("any reason")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
        assert_eq!(
            p.decide_exact("chat", Trust::TranscriptUntrusted, Some("any reason")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
    }

    #[test]
    fn model_allow_deny_passthrough() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        // secure default has empty allow → allow-all
        assert!(p.model_allowed("claude-opus-4-8"));
    }
}
