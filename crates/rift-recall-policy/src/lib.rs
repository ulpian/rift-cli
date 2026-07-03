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
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

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

/// How aggressively the pack compiler may compress a path's content. Sensitive
/// paths force [`Optimization::Conservative`] — safety overrides cost (W7): a
/// sensitive file is served whole or by handle, never as a lossy summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Optimization {
    /// Normal compaction (summaries/truncation allowed).
    Normal,
    /// Conservative: no lossy summary substitution for this path.
    Conservative,
}

/// Errors loading a non-empty reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("an exact-retrieval reason must be non-empty")]
pub struct EmptyReason;

/// A reason for exact retrieval that is **type-guaranteed** non-empty (W10): the
/// only constructor rejects blank/whitespace, so "no reason" is unrepresentable
/// at the `rift_retrieve_exact` boundary and the audit event always carries one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NonEmptyReason(String);

impl NonEmptyReason {
    /// Construct from a string, rejecting blank/whitespace.
    ///
    /// # Errors
    /// [`EmptyReason`] if `s` is empty or only whitespace.
    pub fn new(s: impl Into<String>) -> Result<Self, EmptyReason> {
        let s = s.into();
        if s.trim().is_empty() {
            return Err(EmptyReason);
        }
        Ok(Self(s))
    }

    /// The reason text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The serving policy engine, parameterized by the effective project config plus
/// the recall-serving policy (denied paths + raw-transcript allow-list). The
/// extra lists default empty — **fail-closed**: no denied paths beyond the
/// config's sensitive handling, and raw transcripts disabled.
pub struct PolicyEngine<'a> {
    config: &'a EffectiveConfig,
    denied_paths: Vec<String>,
    raw_transcript_globs: Vec<String>,
}

impl<'a> PolicyEngine<'a> {
    /// Wrap an effective config (no extra denied paths; transcripts disabled).
    #[must_use]
    pub fn new(config: &'a EffectiveConfig) -> Self {
        Self {
            config,
            denied_paths: Vec::new(),
            raw_transcript_globs: Vec::new(),
        }
    }

    /// Add path globs that are **never** served (not even by handle).
    #[must_use]
    pub fn with_denied_paths(mut self, globs: Vec<String>) -> Self {
        self.denied_paths = globs;
        self
    }

    /// Add path globs for which raw transcript retrieval is explicitly enabled
    /// (still trust-checked + audited). Empty = transcripts off (the default).
    #[must_use]
    pub fn with_raw_transcript_globs(mut self, globs: Vec<String>) -> Self {
        self.raw_transcript_globs = globs;
        self
    }

    /// Whether a model is allowed (deny wins; empty allow = allow-all).
    #[must_use]
    pub fn model_allowed(&self, model: &str) -> bool {
        self.config.is_model_allowed(model)
    }

    /// Whether a path is on the deny-list (never served, not even by handle).
    #[must_use]
    pub fn is_denied_path(&self, path: &str) -> bool {
        self.denied_paths
            .iter()
            .any(|g| rift_config::glob_match(g, path))
    }

    /// Whether raw transcript retrieval is enabled for `path`.
    #[must_use]
    pub fn raw_transcript_allowed(&self, path: &str) -> bool {
        self.raw_transcript_globs
            .iter()
            .any(|g| rift_config::glob_match(g, path))
    }

    /// The compaction policy for a path (Conservative for sensitive paths, W7).
    #[must_use]
    pub fn optimization_for(&self, path: &str) -> Optimization {
        if self.config.is_sensitive_path(path) {
            Optimization::Conservative
        } else {
            Optimization::Normal
        }
    }

    /// Whether a path's content must be served *exact-or-not-at-all* (no lossy
    /// summary substitution). True for sensitive paths (W7).
    #[must_use]
    pub fn exact_source_required(&self, path: &str) -> bool {
        self.config.is_sensitive_path(path)
    }

    /// Decide an exact-retrieval request. Precedence is most-restrictive first:
    /// denied path → tool-output-untrusted (never raw) → transcript-while-disabled
    /// → reason mandatory (sensitive paths surface a specific reason) → allow. A
    /// grant returns the audit record to emit.
    #[must_use]
    pub fn decide_exact(&self, path: &str, trust: Trust, reason: Option<&str>) -> AuditDecision {
        // 1. denied path: nothing is served, not even a handle.
        if self.is_denied_path(path) {
            return AuditDecision::Deny(OmitReason::PolicyDeniedExact);
        }
        // 2. arbitrary tool output is never served as raw exact text (W9).
        if matches!(trust, Trust::ToolOutputUntrusted) {
            return AuditDecision::Deny(OmitReason::PolicyDeniedExact);
        }
        // 3. transcripts are off unless explicitly enabled for this path.
        if matches!(trust, Trust::TranscriptUntrusted) && !self.raw_transcript_allowed(path) {
            return AuditDecision::Deny(OmitReason::PolicyDeniedExact);
        }
        // 4. reason is mandatory (W6); a sensitive path surfaces a specific reason.
        let sensitive = self.config.is_sensitive_path(path);
        match reason {
            None | Some("") => {
                if sensitive {
                    AuditDecision::Deny(OmitReason::SensitivePathRequiresReason)
                } else {
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

    #[test]
    fn denied_path_is_refused_even_with_reason() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg).with_denied_paths(vec!["**/vault/**".to_string()]);
        assert!(p.is_denied_path("app/vault/key"));
        assert_eq!(
            p.decide_exact("app/vault/key", Trust::CodeFact, Some("debugging")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact),
            "a denied path is refused even with a reason"
        );
    }

    #[test]
    fn transcript_off_by_default_on_unless_enabled() {
        let cfg = config();
        let off = PolicyEngine::new(&cfg);
        assert_eq!(
            off.decide_exact("log/x", Trust::TranscriptUntrusted, Some("audit")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact),
            "transcripts are off by default"
        );
        let on = PolicyEngine::new(&cfg).with_raw_transcript_globs(vec!["log/**".to_string()]);
        assert!(matches!(
            on.decide_exact("log/x", Trust::TranscriptUntrusted, Some("audit")),
            AuditDecision::Allow { .. }
        ));
    }

    #[test]
    fn sensitive_paths_force_conservative_and_exact_source() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        assert_eq!(
            p.optimization_for("config/.env"),
            Optimization::Conservative
        );
        assert!(p.exact_source_required("config/.env"));
        assert_eq!(p.optimization_for("src/main.rs"), Optimization::Normal);
        assert!(!p.exact_source_required("src/main.rs"));
    }

    #[test]
    fn non_empty_reason_rejects_blank() {
        assert_eq!(NonEmptyReason::new("   ").unwrap_err(), EmptyReason);
        assert_eq!(
            NonEmptyReason::new("debugging").unwrap().as_str(),
            "debugging"
        );
    }
}
