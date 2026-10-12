//! The exact-retrieval engine: which paths are never served, which need a
//! reason, which are served conservatively, and the audit record a grant
//! carries. Its own module because it is the one half of the crate that reads
//! the project config's sensitive paths, and the precedence it applies is
//! pinned by the property tests and the red-team corpus.

use rift_config::EffectiveConfig;
use rift_recall_types::{OmitReason, Trust};

use crate::serve_policy::default_raw_transcript_globs;
use crate::ServePolicy;

/// The serve-policy semantics version ([`PolicyEngine::version`]): part of the
/// pack identity, so any semantic policy change invalidates pack keys.
pub const SERVE_POLICY_VERSION: u32 = 1;

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
/// paths force [`Optimization::Conservative`] — safety overrides cost: a
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

/// A reason for exact retrieval that is **type-guaranteed** non-empty: the
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
/// the recall-serving policy (denied paths + raw-transcript allow-list). Without
/// an overlay it holds the overlay's defaults: no denied paths beyond the
/// config's sensitive handling, and raw transcripts on every path while the
/// config's `capture.raw_transcripts` is on.
pub struct PolicyEngine<'a> {
    config: &'a EffectiveConfig,
    denied_paths: Vec<String>,
    raw_transcript_globs: Vec<String>,
    fail_closed: bool,
}

impl<'a> PolicyEngine<'a> {
    /// Wrap an effective config under the overlay's defaults.
    #[must_use]
    pub fn new(config: &'a EffectiveConfig) -> Self {
        Self {
            config,
            denied_paths: Vec::new(),
            raw_transcript_globs: default_raw_transcript_globs(),
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

    /// Replace the path globs raw transcript retrieval is allowed on (still
    /// trust-checked + audited, and only while the config's switch is on).
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

    /// The policy semantics version (an input to a pack's cache key).
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
        self.effective_raw_transcript_globs()
            .iter()
            .any(|g| rift_config::glob_match(g, &p))
    }

    /// The one place the project config's `capture.raw_transcripts` is read:
    /// off empties the allow-list whatever the overlay wrote, and on leaves the
    /// overlay's list (every path, unless the overlay narrowed it). The overlay
    /// cannot widen past the switch, so a repository that wrote the option off
    /// keeps it off.
    fn effective_raw_transcript_globs(&self) -> &[String] {
        if !self.config.raw_transcripts_enabled() {
            return &[];
        }
        &self.raw_transcript_globs
    }

    /// The compaction policy for a path (Conservative for sensitive paths).
    #[must_use]
    pub fn optimization_for(&self, path: &str) -> Optimization {
        if self.is_sensitive(path) {
            Optimization::Conservative
        } else {
            Optimization::Normal
        }
    }

    /// Whether a path's content must be served *exact-or-not-at-all* (no lossy
    /// summary substitution). True for sensitive paths.
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
        // 2. arbitrary tool output is never served as raw exact text.
        if matches!(trust, Trust::ToolOutputUntrusted) {
            return AuditDecision::Deny(OmitReason::PolicyDeniedExact);
        }
        // 3. transcripts are off unless explicitly enabled for this path.
        if matches!(trust, Trust::TranscriptUntrusted) && !self.raw_transcript_allowed(path) {
            return AuditDecision::Deny(OmitReason::PolicyDeniedExact);
        }
        // 4. reason is mandatory; a sensitive path surfaces a specific reason.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> EffectiveConfig {
        // sensitive default includes **/.env*, **/*secret* etc.
        EffectiveConfig::secure_default().unwrap()
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

    /// A project config whose capture section writes the raw-transcript
    /// option one way or the other.
    fn capture_config(raw_transcripts: bool) -> EffectiveConfig {
        EffectiveConfig::from_toml_str(&format!(
            "schema_version = 1\n[project]\nname = \"x\"\n[capture]\nraw_transcripts = {raw_transcripts}\nacknowledge_risk = {raw_transcripts}\n"
        ))
        .unwrap()
    }

    #[test]
    fn the_capture_option_on_serves_transcripts_under_the_default_globs() {
        let cfg = capture_config(true);
        let p = PolicyEngine::new(&cfg).with_serve_policy(ServePolicy::default());
        assert!(p.raw_transcript_allowed("anything/at/all.jsonl"));
        assert!(matches!(
            p.decide_exact("log/x", Trust::TranscriptUntrusted, Some("audit")),
            AuditDecision::Allow { .. }
        ));
        assert!(PolicyEngine::new(&cfg).raw_transcript_allowed("log/x"));
    }

    #[test]
    fn the_capture_option_off_refuses_every_transcript_whatever_the_globs() {
        let cfg = capture_config(false);
        let p = PolicyEngine::new(&cfg).with_raw_transcript_globs(vec!["**".to_string()]);
        assert!(!p.raw_transcript_allowed("log/x"));
        assert_eq!(
            p.decide_exact("log/x", Trust::TranscriptUntrusted, Some("audit")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact),
            "written off stays off"
        );
    }

    #[test]
    fn written_globs_narrow_transcripts_while_the_option_is_on() {
        let cfg = capture_config(true);
        let p = PolicyEngine::new(&cfg).with_raw_transcript_globs(vec!["log/**".to_string()]);
        assert!(matches!(
            p.decide_exact("log/x", Trust::TranscriptUntrusted, Some("audit")),
            AuditDecision::Allow { .. }
        ));
        assert_eq!(
            p.decide_exact("notes/x", Trust::TranscriptUntrusted, Some("audit")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
    }

    /// Allowing transcripts relaxes one step of the precedence and no other:
    /// a denied path, untrusted tool output, a missing reason, a sensitive
    /// path and an unreadable policy all still decide first.
    #[test]
    fn transcripts_on_never_outrank_a_deny_a_sensitive_path_or_tool_output() {
        let cfg = capture_config(true);
        let p = PolicyEngine::new(&cfg).with_denied_paths(vec!["**/vault/**".to_string()]);
        let transcript = Trust::TranscriptUntrusted;
        assert_eq!(
            p.decide_exact("app/vault/t.jsonl", transcript, Some("audit")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
        assert_eq!(
            p.decide_exact("log/x", Trust::ToolOutputUntrusted, Some("audit")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
        assert_eq!(
            p.decide_exact("log/x", transcript, None),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
        assert_eq!(
            p.decide_exact("config/.env", transcript, None),
            AuditDecision::Deny(OmitReason::SensitivePathRequiresReason)
        );
        match p.decide_exact("config/.env", transcript, Some("audit")) {
            AuditDecision::Allow { sensitive, .. } => assert!(sensitive),
            other => panic!("expected an audited sensitive grant, got {other:?}"),
        }
        let closed = PolicyEngine::new(&cfg).with_serve_policy(ServePolicy {
            fail_closed: true,
            ..ServePolicy::default()
        });
        assert_eq!(
            closed.decide_exact("log/x", transcript, Some("audit")),
            AuditDecision::Deny(OmitReason::PolicyDeniedExact)
        );
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

        let cfg = capture_config(true);
        let p = PolicyEngine::new(&cfg).with_serve_policy(sp);
        assert!(p.is_denied_path("app/vault/key"));
        assert!(p.raw_transcript_allowed("logs/session.jsonl"));
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

    /// The version is a pack-key input, so it names the policy semantics and
    /// not the overlay a repository loaded: a fail-closed engine keys packs
    /// the same way an open one does.
    #[test]
    fn the_policy_version_is_the_semantics_constant_whatever_the_overlay() {
        let cfg = config();
        let open = PolicyEngine::new(&cfg);
        let closed = PolicyEngine::new(&cfg).with_serve_policy(ServePolicy {
            fail_closed: true,
            ..ServePolicy::default()
        });
        assert_eq!(open.version(), SERVE_POLICY_VERSION);
        assert_eq!(closed.version(), SERVE_POLICY_VERSION);
    }

    #[test]
    fn a_transcript_path_that_escapes_the_root_is_never_allowed_raw() {
        let cfg = capture_config(true);
        let p = PolicyEngine::new(&cfg).with_raw_transcript_globs(vec!["logs/**".to_string()]);
        assert!(p.raw_transcript_allowed("logs/session.jsonl"));
        assert!(
            !p.raw_transcript_allowed("logs/../../logs/session.jsonl"),
            "an escaping path is refused before any glob is consulted"
        );
    }

    #[test]
    fn a_path_that_escapes_the_root_is_handled_as_sensitive() {
        let cfg = config();
        let p = PolicyEngine::new(&cfg);
        assert_eq!(
            p.optimization_for("../src/main.rs"),
            Optimization::Conservative
        );
        assert!(p.exact_source_required("../src/main.rs"));
        assert_eq!(
            p.downgrade_for("../src/main.rs", false),
            Some(Downgrade::ExactToHandle)
        );
    }
}
