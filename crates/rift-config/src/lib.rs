//! Typed, validated policy-as-code loaded from the committed `.rift/` directory.
//!
//! The only Rift state in the working tree is a thin committed `.rift/`
//! (system-architecture §2.4). This crate turns it into a typed
//! [`EffectiveConfig`] downstream crates consult for security decisions: which
//! models are allowed, which paths are sensitive, what is captured, how long
//! things are retained.
//!
//! ## Format note (deliberate deviation from the step's illustrative YAML)
//! The dossier pins `toml` (maintained, `cargo-deny`-clean) and flags
//! `serde_yaml` as unmaintained. So Rift loads `.rift/config.toml` rather than
//! `project.yaml` — same typed model, same `deny_unknown_fields` strictness,
//! same secure defaults, supply-chain-clean.
//!
//! Loading is **strict**: unknown keys are hard errors (a typo cannot silently
//! disable a redactor), the dangerous `raw_transcripts` toggle requires an
//! explicit `acknowledge_risk`, built-in sensitive paths are unioned in and
//! cannot be dropped, and validation **aggregates** every problem at once.
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

pub mod features;
mod glob;

use std::path::Path;

use rift_core::{Blake3Id, RepoId, Result, RiftError};

pub use features::{Feature, FeatureRegistry, Trust as FeatureTrust};
pub use glob::{glob_match, wildcard_count, MAX_PATTERN_LEN, MAX_WILDCARDS};

/// Schema versions this build understands.
const SUPPORTED_SCHEMA_VERSIONS: &[u32] = &[1];

/// Built-in sensitive path globs. Unioned into every config; a user may *add*
/// to these but can never remove them (a security default, `05` §5.5).
pub const BUILTIN_SENSITIVE_PATHS: &[&str] = &[
    "**/.env*",
    "**/*secret*",
    "**/*credentials*",
    "**/id_rsa",
    "**/id_dsa",
    "**/id_ecdsa",
    "**/id_ed25519",
    "**/*.pem",
    "**/*.key",
];

fn default_true() -> bool {
    true
}

fn default_entropy() -> f32 {
    4.5
}

/// The raw, deserialized config (before validation / defaulting / hashing).
#[derive(Debug, Clone, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RiftConfig {
    /// Config schema version (validated against the supported set).
    pub schema_version: u32,
    /// Project metadata.
    pub project: ProjectMeta,
    /// Redaction tuning.
    #[serde(default)]
    pub redaction: RedactionPolicy,
    /// Model allow/deny lists.
    #[serde(default)]
    pub models: ModelPolicy,
    /// Sensitive-path globs.
    #[serde(default)]
    pub paths: PathPolicy,
    /// What to capture.
    #[serde(default)]
    pub capture: CapturePolicy,
    /// Retention classes.
    #[serde(default)]
    pub retention: RetentionPolicy,
}

/// Project identity.
#[derive(Debug, Clone, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectMeta {
    /// Human-readable project name.
    pub name: String,
    /// Optional explicit repo id (otherwise derived elsewhere).
    #[serde(default)]
    pub repo_id: Option<String>,
}

/// Redaction tuning knobs.
#[derive(Debug, Clone, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionPolicy {
    /// Shannon-entropy threshold (bits/char) above which a token is treated as a
    /// likely secret.
    #[serde(default = "default_entropy")]
    pub entropy_threshold: f32,
    /// Extra user-supplied secret patterns (added to the built-in set).
    #[serde(default)]
    pub extra_patterns: Vec<String>,
}

impl Default for RedactionPolicy {
    fn default() -> Self {
        Self {
            entropy_threshold: default_entropy(),
            extra_patterns: Vec::new(),
        }
    }
}

/// Model allow/deny lists. Empty `allow` means allow-all; `deny` always wins.
#[derive(Debug, Clone, Default, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelPolicy {
    /// Allowed model globs (empty = allow-all unless denied).
    #[serde(default)]
    pub allow: Vec<String>,
    /// Denied model globs (override allow).
    #[serde(default)]
    pub deny: Vec<String>,
}

/// Sensitive-path policy.
#[derive(Debug, Clone, Default, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathPolicy {
    /// Globs whose matches force exact-source-required + conservative serving.
    #[serde(default)]
    pub sensitive_paths: Vec<String>,
}

/// Capture policy. Raw-transcript retrieval is **off** unless explicitly enabled
/// *and* acknowledged (secure default).
#[derive(Debug, Clone, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturePolicy {
    /// Enable raw-transcript retrieval (dangerous; requires `acknowledge_risk`).
    #[serde(default)]
    pub raw_transcripts: bool,
    /// Required `true` sibling when `raw_transcripts` is enabled.
    #[serde(default)]
    pub acknowledge_risk: bool,
    /// Capture tool calls.
    #[serde(default = "default_true")]
    pub tool_calls: bool,
    /// Capture token usage.
    #[serde(default = "default_true")]
    pub token_usage: bool,
    /// The base checkpoint strategy (`manual-commit` default, `on-pr`,
    /// `scheduled`, `policy-driven`). Policy-driven always composes on top:
    /// a sensitive-path touch archives under any base.
    #[serde(default = "default_checkpoint_strategy")]
    pub checkpoint_strategy: String,
    /// The `scheduled` strategy's interval, in minutes (≥ 1).
    #[serde(default = "default_schedule_interval_minutes")]
    pub schedule_interval_minutes: u32,
}

/// The four checkpoint-strategy tokens (GLOSSARY "Checkpoint strategy").
pub const CHECKPOINT_STRATEGIES: &[&str] =
    &["manual-commit", "on-pr", "scheduled", "policy-driven"];

fn default_checkpoint_strategy() -> String {
    "manual-commit".to_string()
}

fn default_schedule_interval_minutes() -> u32 {
    30
}

impl Default for CapturePolicy {
    fn default() -> Self {
        Self {
            raw_transcripts: false,
            acknowledge_risk: false,
            tool_calls: true,
            token_usage: true,
            checkpoint_strategy: default_checkpoint_strategy(),
            schedule_interval_minutes: default_schedule_interval_minutes(),
        }
    }
}

/// Retention policy vocabulary (enforcement lives in `rift-store`).
#[derive(Debug, Clone, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RetentionPolicy {
    /// The default retention class.
    #[serde(default = "default_class")]
    pub default_class: String,
    /// All defined classes.
    #[serde(default)]
    pub classes: Vec<String>,
}

fn default_class() -> String {
    "standard".to_string()
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            default_class: default_class(),
            classes: vec![default_class()],
        }
    }
}

impl RiftConfig {
    /// The secure-default config used when `.rift/` is absent or sparse.
    #[must_use]
    pub fn secure_default() -> Self {
        Self {
            schema_version: 1,
            project: ProjectMeta {
                name: "unnamed".to_string(),
                repo_id: None,
            },
            redaction: RedactionPolicy::default(),
            models: ModelPolicy::default(),
            paths: PathPolicy::default(),
            capture: CapturePolicy::default(),
            retention: RetentionPolicy::default(),
        }
    }
}

/// The validated, immutable, hash-stamped effective configuration.
#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    schema_version: u32,
    project_name: String,
    repo_id: Option<RepoId>,
    allow_models: Vec<String>,
    deny_models: Vec<String>,
    sensitive_paths: Vec<String>,
    raw_transcripts: bool,
    capture_tool_calls: bool,
    capture_token_usage: bool,
    checkpoint_strategy: String,
    schedule_interval_minutes: u32,
    entropy_threshold: f32,
    extra_patterns: Vec<String>,
    default_retention_class: String,
    retention_classes: Vec<String>,
    policy_hash: Blake3Id,
}

impl EffectiveConfig {
    /// Load and validate `.rift/config.toml` from `rift_dir`. A missing file
    /// yields secure defaults.
    ///
    /// # Errors
    /// [`RiftError::Config`] (aggregating all problems) on parse or validation
    /// failure, or [`RiftError::Io`] if the file cannot be read.
    pub fn load(rift_dir: &Path) -> Result<Self> {
        let path = rift_dir.join("config.toml");
        let raw = if path.exists() {
            let text = std::fs::read_to_string(&path)?;
            toml::from_str::<RiftConfig>(&text)
                .map_err(|e| RiftError::config(format!("{}: {e}", path.display())))?
        } else {
            RiftConfig::secure_default()
        };
        Self::from_config(raw)
    }

    /// Build the secure-default effective config (no `.rift/` present).
    ///
    /// # Errors
    /// Never fails in practice; returns the validated default.
    pub fn secure_default() -> Result<Self> {
        Self::from_config(RiftConfig::secure_default())
    }

    /// Parse + validate config from raw TOML source — the file-less entry used by
    /// the fuzz target and property tests.
    ///
    /// # Errors
    /// [`RiftError::Config`] on a TOML parse error or any validation problem.
    pub fn from_toml_str(src: &str) -> Result<Self> {
        let cfg: RiftConfig = toml::from_str(src).map_err(|e| RiftError::config(e.to_string()))?;
        Self::from_config(cfg)
    }

    /// Parse + validate config from raw bytes (TOML is UTF-8). The untrusted-input
    /// entry: for *any* byte slice this returns a `Result` — it never panics,
    /// hangs, or allocates unboundedly (06 §6.1.6 / §5.9).
    ///
    /// # Errors
    /// [`RiftError::Config`] on non-UTF-8 input, a TOML parse error, or validation.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let src = std::str::from_utf8(bytes)
            .map_err(|_| RiftError::config("config source is not valid UTF-8"))?;
        Self::from_toml_str(src)
    }

    /// Validate a raw config and produce the effective config.
    ///
    /// # Errors
    /// [`RiftError::Config`] aggregating every validation problem.
    #[allow(clippy::too_many_lines)] // one linear validation pass; splitting would obscure it
    pub fn from_config(cfg: RiftConfig) -> Result<Self> {
        let mut errors: Vec<String> = Vec::new();

        if !SUPPORTED_SCHEMA_VERSIONS.contains(&cfg.schema_version) {
            errors.push(format!(
                "unsupported schema_version {} (supported: {SUPPORTED_SCHEMA_VERSIONS:?})",
                cfg.schema_version
            ));
        }

        for m in &cfg.models.allow {
            if cfg.models.deny.contains(m) {
                errors.push(format!("model {m:?} appears in both allow and deny"));
            }
        }

        let mut check_glob = |g: &str, where_: &str| {
            if g.is_empty() {
                errors.push(format!("empty glob in {where_}"));
            } else if g.contains('\0') {
                errors.push(format!("glob in {where_} contains NUL"));
            } else if g.len() > glob::MAX_PATTERN_LEN {
                errors.push(format!(
                    "glob in {where_} exceeds {} bytes (got {})",
                    glob::MAX_PATTERN_LEN,
                    g.len()
                ));
            } else if glob::wildcard_count(g) > glob::MAX_WILDCARDS {
                errors.push(format!(
                    "glob in {where_} has more than {} wildcards",
                    glob::MAX_WILDCARDS
                ));
            }
        };
        for g in &cfg.paths.sensitive_paths {
            check_glob(g, "paths.sensitive_paths");
        }
        for g in &cfg.models.allow {
            check_glob(g, "models.allow");
        }
        for g in &cfg.models.deny {
            check_glob(g, "models.deny");
        }

        if cfg.capture.raw_transcripts && !cfg.capture.acknowledge_risk {
            errors.push(
                "capture.raw_transcripts requires capture.acknowledge_risk = true".to_string(),
            );
        }

        if !CHECKPOINT_STRATEGIES.contains(&cfg.capture.checkpoint_strategy.as_str()) {
            errors.push(format!(
                "capture.checkpoint_strategy {:?} is not one of {CHECKPOINT_STRATEGIES:?}",
                cfg.capture.checkpoint_strategy
            ));
        }
        if cfg.capture.schedule_interval_minutes == 0 {
            errors.push("capture.schedule_interval_minutes must be >= 1".to_string());
        }

        if cfg.retention.default_class.is_empty() {
            errors.push("retention.default_class must not be empty".to_string());
        } else if !cfg.retention.classes.is_empty()
            && !cfg.retention.classes.contains(&cfg.retention.default_class)
        {
            errors.push(format!(
                "retention.default_class {:?} is not in retention.classes",
                cfg.retention.default_class
            ));
        }

        let repo_id = match &cfg.project.repo_id {
            Some(s) => match RepoId::new(s.clone()) {
                Ok(id) => Some(id),
                Err(e) => {
                    errors.push(format!("project.repo_id invalid: {e}"));
                    None
                }
            },
            None => None,
        };

        if !errors.is_empty() {
            return Err(RiftError::config(errors.join("; ")));
        }

        // Built-in sensitive paths are unioned in and cannot be dropped.
        let mut sensitive: Vec<String> = BUILTIN_SENSITIVE_PATHS
            .iter()
            .map(|s| (*s).to_string())
            .chain(cfg.paths.sensitive_paths.iter().cloned())
            .collect();
        sensitive.sort();
        sensitive.dedup();

        let mut allow = cfg.models.allow.clone();
        allow.sort();
        allow.dedup();
        let mut deny = cfg.models.deny.clone();
        deny.sort();
        deny.dedup();
        let mut extra = cfg.redaction.extra_patterns.clone();
        extra.sort();
        extra.dedup();
        let mut classes = cfg.retention.classes.clone();
        classes.sort();
        classes.dedup();

        let mut effective = Self {
            schema_version: cfg.schema_version,
            project_name: cfg.project.name,
            repo_id,
            allow_models: allow,
            deny_models: deny,
            sensitive_paths: sensitive,
            raw_transcripts: cfg.capture.raw_transcripts,
            capture_tool_calls: cfg.capture.tool_calls,
            capture_token_usage: cfg.capture.token_usage,
            checkpoint_strategy: cfg.capture.checkpoint_strategy,
            schedule_interval_minutes: cfg.capture.schedule_interval_minutes,
            entropy_threshold: cfg.redaction.entropy_threshold,
            extra_patterns: extra,
            default_retention_class: cfg.retention.default_class,
            retention_classes: classes,
            policy_hash: Blake3Id::from_bytes([0; 32]),
        };
        effective.policy_hash = effective.compute_policy_hash();
        Ok(effective)
    }

    fn compute_policy_hash(&self) -> Blake3Id {
        // Canonical, order-independent byte encoding (vecs already sorted).
        let mut h = blake3::Hasher::new();
        let mut field = |label: &str, value: &str| {
            h.update(label.as_bytes());
            h.update(b"=");
            h.update(value.as_bytes());
            h.update(b"\n");
        };
        field("schema_version", &self.schema_version.to_string());
        field("project_name", &self.project_name);
        field("repo_id", self.repo_id.as_ref().map_or("", RepoId::as_str));
        field("raw_transcripts", &self.raw_transcripts.to_string());
        field("capture_tool_calls", &self.capture_tool_calls.to_string());
        field("capture_token_usage", &self.capture_token_usage.to_string());
        field("checkpoint_strategy", &self.checkpoint_strategy);
        field(
            "schedule_interval_minutes",
            &self.schedule_interval_minutes.to_string(),
        );
        field(
            "entropy_threshold",
            &format!("{:.4}", f64::from(self.entropy_threshold)),
        );
        field("default_retention_class", &self.default_retention_class);
        for v in &self.allow_models {
            field("allow_model", v);
        }
        for v in &self.deny_models {
            field("deny_model", v);
        }
        for v in &self.sensitive_paths {
            field("sensitive_path", v);
        }
        for v in &self.extra_patterns {
            field("extra_pattern", v);
        }
        for v in &self.retention_classes {
            field("retention_class", v);
        }
        Blake3Id::from_bytes(*h.finalize().as_bytes())
    }

    /// The config schema version.
    #[must_use]
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// The project name.
    #[must_use]
    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    /// The explicit repo id, if configured.
    #[must_use]
    pub fn repo_id(&self) -> Option<&RepoId> {
        self.repo_id.as_ref()
    }

    /// The effective sensitive-path globs (built-ins + user).
    #[must_use]
    pub fn sensitive_paths(&self) -> &[String] {
        &self.sensitive_paths
    }

    /// Whether `path` matches any sensitive-path glob.
    #[must_use]
    pub fn is_sensitive_path(&self, path: &str) -> bool {
        self.sensitive_paths.iter().any(|g| glob_match(g, path))
    }

    /// The configured base checkpoint strategy token (one of
    /// [`CHECKPOINT_STRATEGIES`]; default `manual-commit`).
    #[must_use]
    pub fn checkpoint_strategy(&self) -> &str {
        &self.checkpoint_strategy
    }

    /// The `scheduled` strategy's interval in milliseconds.
    #[must_use]
    pub fn schedule_interval_ms(&self) -> i64 {
        i64::from(self.schedule_interval_minutes) * 60_000
    }

    /// Whether `model` is allowed: deny wins; empty allow = allow-all.
    #[must_use]
    pub fn is_model_allowed(&self, model: &str) -> bool {
        if self.deny_models.iter().any(|g| glob_match(g, model)) {
            return false;
        }
        self.allow_models.is_empty() || self.allow_models.iter().any(|g| glob_match(g, model))
    }

    /// Whether raw-transcript retrieval is enabled (secure default: false).
    #[must_use]
    pub fn raw_transcripts_enabled(&self) -> bool {
        self.raw_transcripts
    }

    /// Whether tool calls are captured.
    #[must_use]
    pub fn captures_tool_calls(&self) -> bool {
        self.capture_tool_calls
    }

    /// Whether token usage is captured.
    #[must_use]
    pub fn captures_token_usage(&self) -> bool {
        self.capture_token_usage
    }

    /// The redaction entropy threshold (bits/char).
    #[must_use]
    pub fn entropy_threshold(&self) -> f32 {
        self.entropy_threshold
    }

    /// The default retention class.
    #[must_use]
    pub fn default_retention_class(&self) -> &str {
        &self.default_retention_class
    }

    /// The stable, order-independent policy hash. Recall/redaction stamp this
    /// into events and `SummaryBinding.policy_ver` so "which policy produced
    /// this" is answerable and a policy change invalidates derived summaries.
    #[must_use]
    pub fn policy_hash(&self) -> Blake3Id {
        self.policy_hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_src: &str) -> Result<EffectiveConfig> {
        EffectiveConfig::from_toml_str(toml_src)
    }

    proptest::proptest! {
        /// The Stage-1 untrusted parser must never panic/hang/OOM on arbitrary
        /// bytes — it returns a `Result` for any input (06 §6.1.6). Runs in the
        /// normal `cargo test` job, so it gates without nightly/libFuzzer.
        #[test]
        fn parser_never_panics_on_arbitrary_bytes(
            bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..4096)
        ) {
            let _ = EffectiveConfig::from_bytes(&bytes);
        }

        /// Same contract over arbitrary UTF-8 text (exercises the TOML grammar).
        #[test]
        fn parser_never_panics_on_arbitrary_text(s in ".*") {
            let _ = EffectiveConfig::from_toml_str(&s);
        }
    }

    #[test]
    fn loads_minimal_config() {
        let eff = parse("schema_version = 1\n[project]\nname = \"demo\"\n").unwrap();
        assert_eq!(eff.project_name(), "demo");
        assert!(!eff.raw_transcripts_enabled());
        assert!(eff.captures_tool_calls());
    }

    #[test]
    fn missing_rift_dir_yields_secure_defaults() {
        let dir = std::env::temp_dir().join("rift-config-absent-xyz");
        let eff = EffectiveConfig::load(&dir).unwrap();
        assert!(!eff.raw_transcripts_enabled());
        assert!(eff.is_sensitive_path("project/.env"));
        assert!(eff.is_sensitive_path("a/b/id_rsa"));
    }

    #[test]
    fn unknown_policy_key_is_rejected() {
        let err =
            parse("schema_version = 1\n[project]\nname=\"x\"\n[capture]\nraw_transcript = true\n");
        assert!(matches!(err, Err(RiftError::Config(_))));
    }

    #[test]
    fn model_in_allow_and_deny_is_invalid() {
        let err = parse(
            "schema_version=1\n[project]\nname=\"x\"\n[models]\nallow=[\"claude-*\"]\ndeny=[\"claude-*\"]\n",
        );
        match err {
            Err(RiftError::Config(m)) => assert!(m.contains("both allow and deny")),
            other => panic!("expected config error, got {other:?}"),
        }
    }

    /// Contract snapshots (step-04 test plan): a representative effective
    /// config's Debug view and a multi-problem aggregated validation report,
    /// pinned so a shape change is a reviewable diff.
    #[test]
    fn effective_config_and_error_report_snapshots() {
        let cfg = parse(
            "schema_version = 1\n\
             [project]\nname = \"demo\"\nrepo_id = \"repo-demo\"\n\
             [models]\ndeny = [\"gpt-3*\"]\n\
             [paths]\nsensitive_paths = [\"secrets/**\"]\n\
             [capture]\ntool_calls = true\n",
        )
        .expect("valid config");
        insta::assert_snapshot!("effective_config_debug", format!("{cfg:#?}"));

        let err = parse(
            "schema_version = 99\n[project]\nname = \"x\"\n\
             [paths]\nsensitive_paths = [\"\"]\n\
             [models]\nallow = [\"m1\"]\ndeny = [\"m1\"]\n",
        )
        .expect_err("three validation problems");
        insta::assert_snapshot!("aggregated_validation_report", err.to_string());
    }

    #[test]
    fn bad_glob_and_schema_aggregate() {
        let err =
            parse("schema_version=99\n[project]\nname=\"x\"\n[paths]\nsensitive_paths=[\"\"]\n");
        match err {
            Err(RiftError::Config(m)) => {
                assert!(m.contains("unsupported schema_version"));
                assert!(m.contains("empty glob"));
            }
            other => panic!("expected aggregated config error, got {other:?}"),
        }
    }

    #[test]
    fn raw_transcripts_requires_acknowledge() {
        let err =
            parse("schema_version=1\n[project]\nname=\"x\"\n[capture]\nraw_transcripts=true\n");
        assert!(matches!(err, Err(RiftError::Config(_))));
        let ok = parse(
            "schema_version=1\n[project]\nname=\"x\"\n[capture]\nraw_transcripts=true\nacknowledge_risk=true\n",
        )
        .unwrap();
        assert!(ok.raw_transcripts_enabled());
    }

    #[test]
    fn builtin_sensitive_paths_cannot_be_dropped() {
        let eff = parse(
            "schema_version=1\n[project]\nname=\"x\"\n[paths]\nsensitive_paths=[\"**/custom_secret_dir/**\"]\n",
        )
        .unwrap();
        assert!(eff.is_sensitive_path("a/.env"));
        assert!(eff.is_sensitive_path("x/custom_secret_dir/y"));
    }

    #[test]
    fn model_allow_deny_semantics() {
        let eff = parse(
            "schema_version=1\n[project]\nname=\"x\"\n[models]\nallow=[\"claude-*\"]\ndeny=[\"*-experimental\"]\n",
        )
        .unwrap();
        assert!(eff.is_model_allowed("claude-opus-4-8"));
        assert!(!eff.is_model_allowed("claude-experimental"));
        assert!(!eff.is_model_allowed("gpt-4"));
    }

    #[test]
    fn policy_hash_is_stable_across_key_order() {
        let a = parse("schema_version=1\n[project]\nname=\"x\"\n[models]\nallow=[\"a\",\"b\"]\n")
            .unwrap();
        let b = parse("schema_version=1\n[models]\nallow=[\"b\",\"a\"]\n[project]\nname=\"x\"\n")
            .unwrap();
        assert_eq!(a.policy_hash(), b.policy_hash());
    }

    proptest::proptest! {
        /// Same logical config (any vec order) → identical policy hash.
        #[test]
        fn effective_policy_is_deterministic(a in "[a-z]{1,6}", b in "[a-z]{1,6}") {
            proptest::prop_assume!(a != b);
            let one = parse(&format!(
                "schema_version=1\n[project]\nname=\"p\"\n[models]\nallow=[\"{a}\",\"{b}\"]\n"
            )).unwrap();
            let two = parse(&format!(
                "schema_version=1\n[project]\nname=\"p\"\n[models]\nallow=[\"{b}\",\"{a}\"]\n"
            )).unwrap();
            proptest::prop_assert_eq!(one.policy_hash(), two.policy_hash());
        }
    }
}
