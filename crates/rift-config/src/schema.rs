//! The raw policy file schema: the `*Policy` tables `.rift/config.toml`
//! deserializes into, with their serde defaults.
//!
//! Kept apart from the effective configuration because this is where a new
//! policy key is declared, and a key's `deny_unknown_fields` table and its
//! absent-key default belong next to each other rather than next to the
//! validation that later reads them.

fn default_true() -> bool {
    true
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
    /// Optional-feature enablement: the per-repo desired state the
    /// feature lifecycle writes and the dashboard/doctor read.
    #[serde(default)]
    pub features: FeaturePolicy,
    /// Where this repository's checkpoints are shipped to and fetched from.
    #[serde(default)]
    pub checkpoint: CheckpointPolicy,
}

/// The checkpoint **destination**: which git remote this repository's capture
/// refs belong to.
///
/// ## Why this is a typed field and not an unknown key
/// `rift configure --checkpoint-remote` has written `[checkpoint] remote` into
/// `.rift/config.toml` since before this struct existed, while [`RiftConfig`]
/// carried no such field and denies unknown fields — so the very act of
/// configuring a checkpoint remote made [`EffectiveConfig::load`](crate::EffectiveConfig::load) **fail** for
/// that repository, taking the project's whole declared policy with it. The
/// field exists so that stops being true; the test
/// `a_configured_checkpoint_remote_does_not_break_the_policy_load` is what
/// would redden if it were removed again.
///
/// ## Why it is not in [`EffectiveConfig`](crate::EffectiveConfig) and not in the policy hash
/// `EffectiveConfig` is the *security policy* — what may be captured, what is
/// redacted, what is sensitive — and `policy_hash` identifies that policy's
/// content, which is why [`PolicySource`](crate::PolicySource) is deliberately excluded from it
/// too. A checkpoint remote is a **destination**, not a policy: two clones of
/// one project with byte-identical redaction rules must hash identically even
/// when only one of them has a remote configured, and pointing a repository at
/// a different remote must not make every checkpoint captured afterwards look
/// as though it was captured under a different policy. Read it through
/// [`checkpoint_remote`](crate::checkpoint_remote), which is the single resolver.
#[derive(Debug, Clone, Default, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointPolicy {
    /// The git remote (a configured remote name, or a URL) that carries this
    /// repository's checkpoint and chain refs. Absent means **not configured**,
    /// which is a refusal to ship anywhere, never a fallback to `origin`.
    #[serde(default)]
    pub remote: Option<String>,
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

/// Recall enablement, the native-search key, and the one retired spelling still
/// accepted for it.
///
/// `total_recall` is **on unless declared off**. A repository that never
/// mentions the key has the memory-delta plane, because a recall plane nobody
/// turned on saves nobody anything; off is a declaration — `total_recall =
/// false` — and never an omission. Turning it on wires skills into agent
/// clients, and that still happens only through `rift init` or Project
/// Settings, which read this key rather than acting on the file's absence.
///
/// `search` records that native file search is installed here. Like the
/// spelling it replaces it is accepted and then ignored: search reads how to
/// behave from its own policy table, not from a bool in `[features]`, so this
/// key has no getter, no runtime effect and no place in the policy hash — and
/// declaring it is therefore never the thing that changes a repository's
/// derived summaries.
///
/// `fff` is that older spelling. It is accepted for one more release so a
/// repository configured before the rename still loads, and its **presence**
/// (not its value) is reported by
/// [`EffectiveConfig::retired_feature_key_notice`](crate::EffectiveConfig::retired_feature_key_notice) so a `doctor` surface can
/// ask for the rename once instead of failing the load.
///
/// The graph-analysis key that used to sit beside it is gone rather than
/// renamed: the packages behind it were deleted by owner decision and are never
/// coming back, so `deny_unknown_fields` now refuses it. A config that still
/// carries the line does not load, and deleting the line is the whole fix —
/// which is a better answer than accepting policy for a feature that cannot
/// exist.
#[derive(Debug, Clone, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeaturePolicy {
    /// rift-total-recall is installed for this repository. On unless the
    /// key is written `false` (see the type's documentation).
    #[serde(default = "default_true")]
    pub total_recall: bool,
    /// Native file search is installed for this repository. Accepted and
    /// recorded; deliberately unread, see the type's documentation.
    #[serde(default)]
    pub search: bool,
    /// The retired spelling of `search`, accepted for one more release.
    /// `Option` because presence, not value, is what the rename notice reports.
    #[serde(default)]
    #[serde(rename = "fff")]
    pub(crate) legacy_search: Option<bool>,
}

impl Default for FeaturePolicy {
    /// The absent-table policy: Recall on, search unrecorded, no retired key.
    /// Spelled by hand so it agrees with the serde defaults above — a derived
    /// `Default` would have said `false` for a key whose absence means on.
    fn default() -> Self {
        Self {
            total_recall: true,
            search: false,
            legacy_search: None,
        }
    }
}

/// The `[redaction]` table, which holds no setting: patterns are added by a
/// rule pack under `.rift/policies/redactors/`, and the entropy floor is
/// compiled into the redactor.
///
/// The two keys the table once took are still recognized so validation can
/// refuse them by name and point at the rule packs. A plain
/// `deny_unknown_fields` rejection would say "unknown field" and leave the
/// author guessing; accepting them silently would change the policy hash
/// while the masking stayed the same. Any value type is held, so a key of the
/// wrong type is refused for the same reason as one of the right type.
#[derive(Debug, Clone, Default, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedactionPolicy {
    #[serde(default)]
    pub(crate) entropy_threshold: Option<toml::Value>,
    #[serde(default)]
    pub(crate) extra_patterns: Option<toml::Value>,
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

/// Capture policy. Raw-transcript retrieval is **on** unless the file writes it
/// off; a transcript is still served through the untrusted frame, behind the
/// serve overlay's denied paths and the sensitive paths' reason.
#[derive(Debug, Clone, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturePolicy {
    /// Raw-transcript retrieval as the file writes it; absent is on. Kept as
    /// written rather than defaulted so the acknowledgement can guard a file
    /// that switches it on without guarding the shipped default.
    #[serde(default)]
    pub raw_transcripts: Option<bool>,
    /// Required `true` sibling when the file writes `raw_transcripts = true`.
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
    /// How long a session may go without a hook before the idle sweep
    /// condenses and ends it, in minutes. Zero is refused where the file is
    /// read: a floor of zero would end every session between two hooks.
    #[serde(default = "default_idle_sweep_minutes")]
    pub idle_sweep_minutes: std::num::NonZeroU32,
}

/// The four checkpoint-strategy tokens (GLOSSARY "Checkpoint strategy").
///
/// `"import"` is deliberately not here. It is the import marker `rift import`
/// stamps on the read-only checkpoints it lands (`StrategyKind::Import` in
/// `rift-shadow-hooks`), never something a repository can be configured to
/// archive under — so a config naming it is refused at load.
pub const CHECKPOINT_STRATEGIES: &[&str] =
    &["manual-commit", "on-pr", "scheduled", "policy-driven"];

fn default_checkpoint_strategy() -> String {
    "manual-commit".to_string()
}

fn default_schedule_interval_minutes() -> u32 {
    30
}

/// Five minutes: long enough that a developer reading a diff between two
/// prompts keeps their session, short enough that a session whose agent
/// went away without saying so is condensed while its work is still news.
fn default_idle_sweep_minutes() -> std::num::NonZeroU32 {
    std::num::NonZeroU32::MIN.saturating_add(4)
}

impl CapturePolicy {
    /// Whether raw-transcript retrieval is on: as written, else on.
    #[must_use]
    pub fn raw_transcripts_enabled(&self) -> bool {
        self.raw_transcripts.unwrap_or(true)
    }

    /// The idle sweep's floor in milliseconds.
    #[must_use]
    pub fn idle_sweep_ms(&self) -> i64 {
        i64::from(self.idle_sweep_minutes.get()) * 60_000
    }
}

impl Default for CapturePolicy {
    fn default() -> Self {
        Self {
            raw_transcripts: None,
            acknowledge_risk: false,
            tool_calls: true,
            token_usage: true,
            checkpoint_strategy: default_checkpoint_strategy(),
            schedule_interval_minutes: default_schedule_interval_minutes(),
            idle_sweep_minutes: default_idle_sweep_minutes(),
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
            features: FeaturePolicy::default(),
            checkpoint: CheckpointPolicy::default(),
        }
    }
}
