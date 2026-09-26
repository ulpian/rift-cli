//! The validated, hash-stamped effective configuration and its assembly from
//! the raw schema.
//!
//! Kept apart from the schema so the validation pass, the policy hash and the
//! getters downstream crates consult read as one unit.

use std::path::Path;

use rift_core::{Blake3Id, RepoId, Result, RiftError};

use crate::glob::{self, glob_match};
use crate::schema::{RiftConfig, CHECKPOINT_STRATEGIES};
use crate::source::{classify_policy_source, PolicySource};

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
    feature_total_recall: bool,
    legacy_search_key: bool,
    policy_hash: Blake3Id,
    policy_source: PolicySource,
}

impl EffectiveConfig {
    /// Load and validate `.rift/config.toml` from `rift_dir`. A missing file —
    /// or one the work tree does not own (see `classify_policy_source`) — yields
    /// secure defaults.
    ///
    /// # Errors
    /// [`RiftError::Config`] (aggregating all problems) on parse or validation
    /// failure, or [`RiftError::Io`] if the file cannot be read.
    pub fn load(rift_dir: &Path) -> Result<Self> {
        let (raw, source) = load_raw(rift_dir)?;
        let mut effective = Self::from_config(raw)?;
        effective.policy_source = source;
        Ok(effective)
    }

    /// Where the effective policy came from — [`PolicySource::Declared`],
    /// [`PolicySource::Default`], or a [`PolicySource::Refused`] carrying the
    /// reason. A refused load is the case worth surfacing: the built-in floor
    /// holds, and everything the project declared does not apply.
    #[must_use]
    pub fn policy_source(&self) -> PolicySource {
        self.policy_source
    }

    /// Build the secure-default effective config (no `.rift/` present).
    ///
    /// # Errors
    /// Never fails in practice; returns the validated default.
    pub fn secure_default() -> Result<Self> {
        let mut effective = Self::from_config(RiftConfig::secure_default())?;
        effective.policy_source = PolicySource::Default;
        Ok(effective)
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
    /// hangs, or allocates unboundedly.
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
            feature_total_recall: cfg.features.total_recall,
            legacy_search_key: cfg.features.legacy_search.is_some(),
            policy_hash: Blake3Id::from_bytes([0; 32]),
            // `load` overwrites this; every other constructor is handed its
            // policy directly, so the declared policy is what applied.
            policy_source: PolicySource::Declared,
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
        field(
            "feature_total_recall",
            &self.feature_total_recall.to_string(),
        );
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

    /// Whether rift-total-recall is enabled for this repository:
    /// gates dashboard auto-ingest and native hook/skill provisioning. Default
    /// off.
    #[must_use]
    pub fn recall_enabled(&self) -> bool {
        self.feature_total_recall
    }

    /// The one-line notice a `doctor` surface prints when `[features]` still
    /// spells the native-search key the retired way.
    ///
    /// A renamed key is the one deprecation a repository cannot see for itself:
    /// the old spelling still parses, still hashes the same, and still has no
    /// effect, so nothing about a normal run says anything is wrong — and then
    /// the release that stops accepting it turns a silent no-op into a config
    /// that will not load. Saying it once, out of the load path and on a surface
    /// whose job is to tell you about your setup, is the whole mechanism. It is
    /// `None` for the current spelling, and `None` when neither key is present.
    #[must_use]
    pub fn retired_feature_key_notice(&self) -> Option<&'static str> {
        self.legacy_search_key.then_some(
            "[features] fff is the retired spelling of the native-search key: rename `fff` to \
             `search` in .rift/config.toml — the old spelling is accepted for one more release, \
             and neither key changes how search behaves",
        )
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

/// The idle sweep's floor for the repository whose policy lives in
/// `rift_dir`, in milliseconds: a live session untouched for longer is
/// condensed and ended by the next sweep.
///
/// Read beside the effective policy rather than folded into it. It is not
/// policy: it moves *when* a quiet session is finalized, never what is
/// captured, redacted or served, so it stays out of the policy hash and out
/// of [`EffectiveConfig`]. A file that cannot be read, or does not load,
/// answers the default, exactly as the effective policy falls back to its
/// secure default.
#[must_use]
pub fn idle_sweep_ms(rift_dir: &Path) -> i64 {
    match load_raw(rift_dir) {
        Ok((raw, _)) => raw.capture.idle_sweep_ms(),
        Err(_) => RiftConfig::secure_default().capture.idle_sweep_ms(),
    }
}

/// The raw policy file and where it came from: the declared file when the
/// work tree owns one, the secure default otherwise.
fn load_raw(rift_dir: &Path) -> Result<(RiftConfig, PolicySource)> {
    let path = rift_dir.join("config.toml");
    let source = classify_policy_source(rift_dir, &path);
    if !source.is_declared_policy_applied() {
        return Ok((RiftConfig::secure_default(), source));
    }
    let text = std::fs::read_to_string(&path)?;
    let raw = toml::from_str::<RiftConfig>(&text)
        .map_err(|e| RiftError::config(format!("{}: {e}", path.display())))?;
    Ok((raw, source))
}
