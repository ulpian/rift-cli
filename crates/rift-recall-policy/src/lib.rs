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
#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )
)]

use std::path::Path;

use rift_config::EffectiveConfig;
use rift_recall_types::{OmitReason, Trust};

/// Lexically normalize a repo-relative path: drop `.`/empty segments and resolve
/// `..` against earlier segments. Returns `None` if the path escapes the repo
/// root (a `..` with nothing to pop) — an escaping request is never served. This
/// is the traversal defense: a denied glob cannot be dodged by dressing the path
/// up (`foo/../secrets/key` → `secrets/key`).
#[must_use]
pub fn normalize_rel(path: &str) -> Option<String> {
    let mut out: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                out.pop()?; // pop earlier segment; nothing to pop → escapes root
            }
            s => out.push(s),
        }
    }
    Some(out.join("/"))
}

/// A composition downgrade for a sensitive path that cannot be served exactly
/// within budget: hand back a handle/summary rather than deny (losing the delta)
/// or truncate (leaking a partial secret).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Downgrade {
    /// Serve the retrieval handle instead of inlining the exact bytes.
    ExactToHandle,
    /// Serve the validity-checked summary instead of inlining the source.
    InlineToSummary,
}

/// The parsed `.rift/policies/serve.toml` overlay (TOML, `deny_unknown_fields`).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ServePolicyFile {
    #[serde(default)]
    denied_paths: Vec<String>,
    #[serde(default)]
    raw_transcript_globs: Vec<String>,
}

/// The serve-policy overlay loaded at server start: path globs never served, and
/// the per-path raw-transcript allow-list. `fail_closed` is set when a policy
/// file is present but unparseable — the server then denies **all** exact
/// retrieval rather than serve under an unknown policy.
#[derive(Debug, Default, Clone)]
pub struct ServePolicy {
    /// Path globs that are never served (not even by handle).
    pub denied_paths: Vec<String>,
    /// Path globs for which raw transcript retrieval is explicitly enabled.
    pub raw_transcript_globs: Vec<String>,
    /// Fail-closed: policy present but unparseable → deny all exact retrieval.
    pub fail_closed: bool,
}

impl ServePolicy {
    /// Load `.rift/policies/serve.toml`. **Absent** → the permissive default (no
    /// extra denies, transcripts still off). **Present but unparseable** →
    /// [`ServePolicy::fail_closed`] (deny all exact retrieval, W-fail-closed).
    #[must_use]
    pub fn load(rift_dir: &Path) -> Self {
        let path = rift_dir.join("policies").join("serve.toml");
        match std::fs::read_to_string(&path) {
            Err(_) => Self::default(), // absent → default (not fail-closed)
            Ok(text) => match toml::from_str::<ServePolicyFile>(&text) {
                Ok(f) => Self {
                    denied_paths: f.denied_paths,
                    raw_transcript_globs: f.raw_transcript_globs,
                    fail_closed: false,
                },
                Err(_) => Self {
                    denied_paths: Vec::new(),
                    raw_transcript_globs: Vec::new(),
                    fail_closed: true, // unparseable policy → fail closed
                },
            },
        }
    }
}

/// The serve-policy semantics version ([`PolicyEngine::version`]): part of the
/// ADR-0020 pack identity, so any semantic policy change invalidates pack keys.
pub const SERVE_POLICY_VERSION: u32 = 1;

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
        self.calls_used = self.calls_used.saturating_add(1);
        true
    }

    /// Try to account for `tokens` returned. Returns `false` if it would exceed
    /// the ceiling (and accounts nothing). Uses saturating arithmetic so a
    /// hostile/huge `tokens` can never wrap the counter under the ceiling
    /// (ADR-0004).
    #[must_use]
    pub fn try_tokens(&mut self, tokens: u32) -> bool {
        if self.tokens_used.saturating_add(tokens) > self.max_tokens {
            return false;
        }
        self.tokens_used = self.tokens_used.saturating_add(tokens);
        true
    }

    /// Remaining tokens this turn.
    #[must_use]
    pub fn tokens_remaining(&self) -> u32 {
        self.max_tokens.saturating_sub(self.tokens_used)
    }

    /// Remaining tool calls this turn.
    #[must_use]
    pub fn calls_remaining(&self) -> u32 {
        self.max_tool_calls.saturating_sub(self.calls_used)
    }

    /// Anchor a new turn (ADR-0004): zero the call/token draw-down while keeping
    /// the ceilings. A `rift_known_state` call resets the server-held budget so
    /// every subsequent call in the turn draws from a fresh allowance.
    pub fn anchor(&mut self) {
        self.calls_used = 0;
        self.tokens_used = 0;
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
    fail_closed: bool,
}

impl<'a> PolicyEngine<'a> {
    /// Wrap an effective config (no extra denied paths; transcripts disabled).
    #[must_use]
    pub fn new(config: &'a EffectiveConfig) -> Self {
        Self {
            config,
            denied_paths: Vec::new(),
            raw_transcript_globs: Vec::new(),
            fail_closed: false,
        }
    }

    /// Apply a loaded [`ServePolicy`] overlay (`.rift/policies/serve.toml`): its
    /// denied globs, transcript allow-list, and fail-closed flag.
    #[must_use]
    pub fn with_serve_policy(mut self, sp: ServePolicy) -> Self {
        self.denied_paths = sp.denied_paths;
        self.raw_transcript_globs = sp.raw_transcript_globs;
        self.fail_closed = sp.fail_closed;
        self
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

    /// Whether the engine is fail-closed (an unparseable policy denies all exact).
    #[must_use]
    pub fn is_fail_closed(&self) -> bool {
        self.fail_closed
    }

    /// The policy semantics version (an input to the ADR-0020 `PackKey`).
    /// Bumped whenever a change to serve-policy semantics could alter what a
    /// pack may contain, so a policy change can never re-serve a stale cache.
    #[must_use]
    pub fn version(&self) -> u32 {
        SERVE_POLICY_VERSION
    }

    /// Compose a [`Downgrade`] for a sensitive path whose exact bytes will not fit
    /// the remaining budget: hand back a handle rather than deny or truncate.
    /// `None` means serve exact as usual (not sensitive, or it fits).
    #[must_use]
    pub fn downgrade_for(&self, path: &str, exact_fits_budget: bool) -> Option<Downgrade> {
        if !exact_fits_budget && self.exact_source_required(path) {
            Some(Downgrade::ExactToHandle)
        } else {
            None
        }
    }

    /// Whether a model is allowed (deny wins; empty allow = allow-all).
    #[must_use]
    pub fn model_allowed(&self, model: &str) -> bool {
        self.config.is_model_allowed(model)
    }

    /// Whether a path is on the deny-list (never served, not even by handle). The
    /// path is canonicalized first, so a traversal-dressed request cannot dodge a
    /// denied glob; a path that escapes the repo root is treated as denied.
    #[must_use]
    pub fn is_denied_path(&self, path: &str) -> bool {
        match normalize_rel(path) {
            Some(p) => self
                .denied_paths
                .iter()
                .any(|g| rift_config::glob_match(g, &p)),
            None => true, // an escaping path (`../…`) is never served
        }
    }

    /// Whether raw transcript retrieval is enabled for `path` (canonicalized).
    #[must_use]
    pub fn raw_transcript_allowed(&self, path: &str) -> bool {
        let Some(p) = normalize_rel(path) else {
            return false;
        };
        self.raw_transcript_globs
            .iter()
            .any(|g| rift_config::glob_match(g, &p))
    }

    /// The compaction policy for a path (Conservative for sensitive paths, W7).
    #[must_use]
    pub fn optimization_for(&self, path: &str) -> Optimization {
        if self.is_sensitive(path) {
            Optimization::Conservative
        } else {
            Optimization::Normal
        }
    }

    /// Whether a path's content must be served *exact-or-not-at-all* (no lossy
    /// summary substitution). True for sensitive paths (W7).
    #[must_use]
    pub fn exact_source_required(&self, path: &str) -> bool {
        self.is_sensitive(path)
    }

    /// Whether a path is sensitive, after canonicalizing traversal away. A path
    /// that escapes the repo root is treated as sensitive (fail closed).
    fn is_sensitive(&self, path: &str) -> bool {
        match normalize_rel(path) {
            Some(p) => self.config.is_sensitive_path(&p),
            None => true,
        }
    }

    /// Decide an exact-retrieval request. Precedence is most-restrictive first:
    /// denied path → tool-output-untrusted (never raw) → transcript-while-disabled
    /// → reason mandatory (sensitive paths surface a specific reason) → allow. A
    /// grant returns the audit record to emit.
    #[must_use]
    pub fn decide_exact(&self, path: &str, trust: Trust, reason: Option<&str>) -> AuditDecision {
        // 0. fail closed: an unparseable serve policy denies all exact retrieval.
        if self.fail_closed {
            return AuditDecision::Deny(OmitReason::PolicyDeniedExact);
        }
        // 1. denied path: nothing is served, not even a handle. Canonicalized, so
        //    a traversal-dressed path cannot reach a denied target.
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
        let sensitive = self.is_sensitive(path);
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
    fn token_counter_never_overflows_under_the_ceiling() {
        // A hostile huge `tokens` must be refused, not wrap the counter below the
        // ceiling and slip through (ADR-0004 saturating fix).
        let mut b = TurnBudget::new(4, 100);
        assert!(b.try_tokens(50));
        assert!(
            !b.try_tokens(u32::MAX),
            "u32::MAX must not wrap under ceiling"
        );
        assert_eq!(
            b.tokens_remaining(),
            50,
            "the refused charge accounts nothing"
        );
    }

    #[test]
    fn anchor_resets_draw_down_but_keeps_ceilings() {
        let mut b = TurnBudget::new(2, 100);
        assert!(b.try_call());
        assert!(b.try_tokens(90));
        assert_eq!(b.calls_remaining(), 1);
        assert_eq!(b.tokens_remaining(), 10);
        b.anchor(); // a known_state call anchors a fresh turn
        assert_eq!(
            b.calls_remaining(),
            2,
            "ceiling preserved, draw-down zeroed"
        );
        assert_eq!(b.tokens_remaining(), 100);
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

    #[test]
    fn normalize_resolves_dots_and_rejects_escapes() {
        assert_eq!(normalize_rel("a/./b").as_deref(), Some("a/b"));
        assert_eq!(normalize_rel("a/b/../c").as_deref(), Some("a/c"));
        assert_eq!(
            normalize_rel("foo/../secrets/key").as_deref(),
            Some("secrets/key")
        );
        assert_eq!(normalize_rel("../etc/passwd"), None, "escapes root");
        assert_eq!(normalize_rel("a/../../x"), None, "escapes root");
    }

    #[test]
    fn serve_policy_loads_denied_and_transcript_globs() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        std::fs::write(
            pol.join("serve.toml"),
            "denied_paths = [\"**/vault/**\"]\nraw_transcript_globs = [\"logs/**\"]\n",
        )
        .unwrap();
        let sp = ServePolicy::load(dir.path());
        assert!(!sp.fail_closed);
        assert_eq!(sp.denied_paths, vec!["**/vault/**".to_string()]);
        assert_eq!(sp.raw_transcript_globs, vec!["logs/**".to_string()]);

        let cfg = config();
        let p = PolicyEngine::new(&cfg).with_serve_policy(sp);
        assert!(p.is_denied_path("app/vault/key"));
        assert!(p.raw_transcript_allowed("logs/session.jsonl"));
    }

    #[test]
    fn absent_policy_is_permissive_default() {
        let dir = tempfile::tempdir().unwrap();
        let sp = ServePolicy::load(dir.path());
        assert!(!sp.fail_closed);
        assert!(sp.denied_paths.is_empty());
    }

    #[test]
    fn unparseable_policy_fails_closed_and_denies_all_exact() {
        let dir = tempfile::tempdir().unwrap();
        let pol = dir.path().join("policies");
        std::fs::create_dir_all(&pol).unwrap();
        // an unknown field / broken TOML → fail closed
        std::fs::write(pol.join("serve.toml"), "denied_paths = not-a-list").unwrap();
        let sp = ServePolicy::load(dir.path());
        assert!(sp.fail_closed);

        let cfg = config();
        let p = PolicyEngine::new(&cfg).with_serve_policy(sp);
        assert!(p.is_fail_closed());
        // even a trusted path WITH a reason is denied under an unparseable policy
        assert_eq!(
            p.decide_exact("src/main.rs", Trust::CodeFact, Some("debugging")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
    }

    #[test]
    fn traversal_dressed_path_cannot_dodge_a_denied_glob() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg).with_denied_paths(vec!["secrets/**".to_string()]);
        // a dressed-up path canonicalizes to the denied target
        assert!(p.is_denied_path("app/../secrets/key"));
        assert_eq!(
            p.decide_exact("app/../secrets/key", Trust::CodeFact, Some("r")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact),
            "a denied path cannot be reached by dressing it up"
        );
        // an escaping path is never served
        assert_eq!(
            p.decide_exact("../../etc/passwd", Trust::CodeFact, Some("r")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
    }

    #[test]
    fn sensitive_over_budget_downgrades_to_handle() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        // a sensitive path whose exact bytes will not fit → downgrade to a handle
        assert_eq!(
            p.downgrade_for("config/.env", false),
            Some(Downgrade::ExactToHandle)
        );
        // fits budget → serve exact as usual (no downgrade)
        assert_eq!(p.downgrade_for("config/.env", true), None);
        // not sensitive → no downgrade regardless of budget
        assert_eq!(p.downgrade_for("src/main.rs", false), None);
    }
}
