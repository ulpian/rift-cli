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

mod edit;
mod glob;
pub mod redactor_packs;

use std::path::Path;

use rift_core::{Blake3Id, RepoId, Result, RiftError};

pub use edit::{
    load_project_policy, patch_project_policy, set_checkpoint_remote, PolicyEditError,
    PolicyRevision, ProjectPolicyPatch, ProjectPolicySnapshot,
};
pub use glob::{glob_match, wildcard_count, MAX_PATTERN_LEN, MAX_WILDCARDS};
pub use redactor_packs::{
    load_redactor_packs, redactor_pack_dir, LoadedPacks, PackDiagnostic, RedactorPack,
    RedactorRule, RedactorSample, REDACTORS_DIR_NAME, REDACTOR_PACK_SCHEMA_VERSION,
};

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
/// configuring a checkpoint remote made [`EffectiveConfig::load`] **fail** for
/// that repository, taking the project's whole declared policy with it. The
/// field exists so that stops being true; the test
/// `a_configured_checkpoint_remote_does_not_break_the_policy_load` is what
/// would redden if it were removed again.
///
/// ## Why it is not in [`EffectiveConfig`] and not in the policy hash
/// `EffectiveConfig` is the *security policy* — what may be captured, what is
/// redacted, what is sensitive — and `policy_hash` identifies that policy's
/// content, which is why [`PolicySource`] is deliberately excluded from it
/// too. A checkpoint remote is a **destination**, not a policy: two clones of
/// one project with byte-identical redaction rules must hash identically even
/// when only one of them has a remote configured, and pointing a repository at
/// a different remote must not make every checkpoint captured afterwards look
/// as though it was captured under a different policy. Read it through
/// [`checkpoint_remote`], which is the single resolver.
#[derive(Debug, Clone, Default, ::serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointPolicy {
    /// The git remote (a configured remote name, or a URL) that carries this
    /// repository's checkpoint and chain refs. Absent means **not configured**,
    /// which is a refusal to ship anywhere, never a fallback to `origin`.
    #[serde(default)]
    pub remote: Option<String>,
}

/// The configured checkpoint remote for `work_tree`, or `None` when the
/// repository has not named one.
///
/// **The single resolver.** Every consumer — the resume fetch ladder, the push
/// shipper, `rift sync` — reads the setting through this function, so "which
/// remote is the checkpoint remote" has exactly one answer per repository and
/// two callers cannot drift into disagreeing about it.
///
/// **There is deliberately no default.** An unconfigured repository resolves to
/// `None`, and every caller must treat that as "do not ship", never as
/// "`origin`". `origin` on a developer's machine is whatever project they
/// cloned; on the machine this was written on it is an unrelated third party's
/// repository, and a shipper that fell back to it would publish this
/// repository's capture ledger there.
///
/// The value is screened before it is returned because it becomes an argv word
/// in a `git` invocation: a leading `-` would be read as a flag, and
/// whitespace, NUL or a control byte cannot name a remote. A present-but-unsafe
/// value is an `Err` rather than a silent `None`, so a typo is reported instead
/// of masquerading as "not configured".
///
/// # Errors
/// [`RiftError::Config`] when `[checkpoint] remote` is present but is not a
/// usable remote token. Naming only the reason — never the value.
pub fn checkpoint_remote(work_tree: &Path) -> Result<Option<String>> {
    let rift_dir = work_tree.join(".rift");
    let path = rift_dir.join("config.toml");
    if !classify_policy_source(&rift_dir, &path).is_declared_policy_applied() {
        return Ok(None);
    }
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Ok(None);
    };
    let Ok(doc) = text.parse::<::toml::Value>() else {
        return Ok(None);
    };
    let Some(item) = doc.get("checkpoint").and_then(|c| c.get("remote")) else {
        return Ok(None);
    };
    let raw = item
        .as_str()
        .ok_or_else(|| RiftError::config("[checkpoint] remote must be a string"))?;
    if raw.is_empty() {
        return Err(RiftError::config("[checkpoint] remote is empty"));
    }
    if raw.starts_with('-') {
        return Err(RiftError::config(
            "[checkpoint] remote may not begin with '-' (it would be read as a git flag)",
        ));
    }
    if raw
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || c == '\0')
    {
        return Err(RiftError::config(
            "[checkpoint] remote may not contain whitespace or control characters",
        ));
    }
    Ok(Some(raw.to_owned()))
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
/// [`EffectiveConfig::retired_feature_key_notice`] so a `doctor` surface can
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
    legacy_search: Option<bool>,
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
            features: FeaturePolicy::default(),
            checkpoint: CheckpointPolicy::default(),
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
    feature_total_recall: bool,
    legacy_search_key: bool,
    policy_hash: Blake3Id,
    policy_source: PolicySource,
}

/// Why a policy file that is present was not applied. Each variant names what
/// [`std::fs::symlink_metadata`] actually saw, because that is the whole of what
/// this guard can know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyRefusal {
    /// `.rift` is not a directory — a symlink into somewhere else on the
    /// machine, or a plain file standing in its place.
    RiftDirIsNotADirectory,
    /// `.rift/config.toml` is a symlink, so reading it would read a file the
    /// work tree does not own.
    ConfigIsASymlink,
    /// `.rift/config.toml` is neither a regular file nor a symlink — a
    /// directory, a fifo, a device.
    ConfigIsNotARegularFile,
}

/// Where the effective policy came from.
///
/// The load path degrades to secure defaults whenever it cannot trust the file,
/// which is the right posture and was, until this existed, **silent**. What
/// degrading loses is not the built-in floor — that always holds, and a refused
/// load produces exactly the same `sensitive_paths` a default load does — it is
/// the *developer's declared* policy: their extra `sensitive_paths`, their
/// model allow/deny lists, `raw_transcripts`, the retention class, the
/// checkpoint strategy, the entropy threshold. A project can therefore be
/// running with none of the policy it wrote and no way to tell.
///
/// This is the answer, carried on the config itself so a caller cannot forget to
/// ask for it. Deliberately **not** part of `policy_hash`: the hash identifies
/// the policy's content, and two projects with identical policy must hash the
/// same whether one of them reached it by refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicySource {
    /// The project's declared policy was read and applied.
    Declared,
    /// No `.rift/config.toml` is present; the built-in secure defaults apply and
    /// nothing was lost.
    Default,
    /// A policy file is present and was **refused**. The built-in floor is in
    /// force; everything the project declared is not.
    Refused(PolicyRefusal),
}

impl PolicySource {
    /// A one-line diagnostic when the declared policy did not apply, so the
    /// surface that reports it does not re-derive the reasoning.
    #[must_use]
    pub fn diagnostic(self) -> Option<&'static str> {
        match self {
            Self::Declared | Self::Default => None,
            Self::Refused(PolicyRefusal::RiftDirIsNotADirectory) => Some(
                ".rift is not a directory this work tree owns (a symlink or a file stands in \
                 its place), so .rift/config.toml was not read: built-in secure defaults are \
                 in force and the project's declared policy is not applied",
            ),
            Self::Refused(PolicyRefusal::ConfigIsASymlink) => Some(
                ".rift/config.toml is a symlink, so it was not read: built-in secure defaults \
                 are in force and the project's declared policy is not applied",
            ),
            Self::Refused(PolicyRefusal::ConfigIsNotARegularFile) => Some(
                ".rift/config.toml is not a regular file, so it was not read: built-in secure \
                 defaults are in force and the project's declared policy is not applied",
            ),
        }
    }

    /// Whether the project's declared policy is actually in force.
    #[must_use]
    pub fn is_declared_policy_applied(self) -> bool {
        matches!(self, Self::Declared)
    }
}

/// The single path decision this crate makes about its policy file: was it
/// applied, is there none, or was it refused and why.
///
/// Both `.rift` and `.rift/config.toml` are repository-shippable names, and a
/// symlink survives a clone (git stores it as mode 120000). `Path::exists` and
/// `read_to_string` both *follow*, so a repository could point its policy file
/// at `~/.aws/credentials` and have this crate read it — and a TOML parse
/// error's `Display` quotes the offending source line, which puts a line of the
/// target on whichever terminal prints the error. Measured, before this guard:
/// `config error: …/config.toml: TOML parse error at line 2, column 21` followed
/// by the credentials line itself. A linked `.rift` **directory** is the worse
/// half: a valid-TOML target parses, so the policy is not echoed but silently
/// *loaded* from outside the work tree.
///
/// `symlink_metadata` reports the link rather than its target, so a linked
/// directory or a linked file is refused and the caller degrades to secure
/// defaults — policy that cannot be trusted is policy that does not apply, never
/// policy that leaks. A directory in the file's place is refused too (it used to
/// reach `read_to_string` and surface an IO error instead).
///
/// Absent and refused are deliberately different answers. Both degrade to the
/// same secure defaults, but only one of them means a developer wrote policy
/// that is not running — see [`PolicySource`].
///
/// Two residuals, both outside what a repository can ship and both stated
/// because this doc's framing is broader than the guard:
///
/// * a **TOCTOU race** between this check and the read, which std cannot close
///   (`O_NOFOLLOW` needs a libc `OpenOptions` flag);
/// * a **hard link**. `symlink_metadata` reports a hard link to
///   `~/.aws/credentials` as an ordinary regular file, because on the filesystem
///   it *is* one — there is no link to see. So the file is read and a TOML parse
///   error still quotes its line. Git cannot represent a hard link (it stores
///   blobs, and a clone writes fresh files), so this is not repo-shippable and
///   is outside the threat model; making one needs local write access to the
///   work tree, which is strictly more than a repository has. What this rules
///   out is a policy file the work tree does not own **and can be told apart by
///   its metadata** — not every file whose bytes live elsewhere.
fn classify_policy_source(rift_dir: &Path, path: &Path) -> PolicySource {
    match std::fs::symlink_metadata(rift_dir) {
        Err(_) => return PolicySource::Default,
        Ok(m) if !m.is_dir() => {
            return PolicySource::Refused(PolicyRefusal::RiftDirIsNotADirectory)
        }
        Ok(_) => {}
    }
    match std::fs::symlink_metadata(path) {
        Err(_) => PolicySource::Default,
        Ok(m) if m.is_file() => PolicySource::Declared,
        Ok(m) if m.is_symlink() => PolicySource::Refused(PolicyRefusal::ConfigIsASymlink),
        Ok(_) => PolicySource::Refused(PolicyRefusal::ConfigIsNotARegularFile),
    }
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
        let path = rift_dir.join("config.toml");
        let source = classify_policy_source(rift_dir, &path);
        let raw = if source.is_declared_policy_applied() {
            let text = std::fs::read_to_string(&path)?;
            toml::from_str::<RiftConfig>(&text)
                .map_err(|e| RiftError::config(format!("{}: {e}", path.display())))?
        } else {
            RiftConfig::secure_default()
        };
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(toml_src: &str) -> Result<EffectiveConfig> {
        EffectiveConfig::from_toml_str(toml_src)
    }

    proptest::proptest! {
        /// The untrusted config parser must never panic/hang/OOM on arbitrary
        /// bytes — it returns a `Result` for any input. Runs in the
        /// normal `cargo test` job, so it gates without nightly/libFuzzer.
        #[test]
        fn parser_never_panics_on_arbitrary_bytes(
            bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..4096)
        ) {
            let _ = EffectiveConfig::from_bytes(&bytes);
        }

        /// Same contract over arbitrary UTF-8 text (exercises the TOML grammar).
        ///
        /// `(?s)` is load-bearing: TOML is line-oriented -- tables, key/value
        /// pairs and comments all end at a newline -- so a generator whose `.`
        /// can never produce one exercises the grammar's least interesting half.
        #[test]
        fn parser_never_panics_on_arbitrary_text(s in "(?s).*") {
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

    /// Writing a checkpoint remote must not cost a repository its policy.
    ///
    /// `[checkpoint]` was an unknown key under `deny_unknown_fields`, so a
    /// repository that had ever run `rift configure --checkpoint-remote` made
    /// [`EffectiveConfig::load`] return `Err` — every declared sensitive path,
    /// model rule and retention class went with it. Delete
    /// `RiftConfig::checkpoint` and this reddens on the first assertion.
    #[test]
    fn a_configured_checkpoint_remote_does_not_break_the_policy_load() {
        let src = "schema_version = 1\n\
                   [project]\nname = \"demo\"\n\
                   [paths]\nsensitive_paths = [\"secrets/**\"]\n\
                   [checkpoint]\nremote = \"rift-origin\"\n";
        let eff = parse(src).expect("a configured checkpoint remote must still load as policy");
        assert!(
            eff.is_sensitive_path("secrets/db.pem"),
            "the project's declared policy must survive the presence of a checkpoint remote"
        );
    }

    /// The destination is not the policy: naming a remote — or changing which
    /// one — must not move `policy_hash`, or every checkpoint captured after a
    /// remote change would look as though it were captured under a different
    /// policy. This is the same exclusion `policy_source` gets, for the same
    /// reason.
    #[test]
    fn the_checkpoint_remote_is_not_part_of_the_policy_hash() {
        let base = "schema_version = 1\n[project]\nname = \"demo\"\n";
        let none = parse(base).unwrap();
        let one = parse(&format!("{base}[checkpoint]\nremote = \"a\"\n")).unwrap();
        let two = parse(&format!("{base}[checkpoint]\nremote = \"b\"\n")).unwrap();
        assert_eq!(none.policy_hash(), one.policy_hash());
        assert_eq!(one.policy_hash(), two.policy_hash());
    }

    /// The single resolver: absent means **not configured**, and every caller
    /// must read that as "do not ship". There is no `origin` fallback anywhere
    /// in this function, which is the whole point of it existing.
    #[test]
    fn an_unconfigured_repository_resolves_to_no_checkpoint_remote() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(checkpoint_remote(tmp.path()).unwrap(), None);

        std::fs::create_dir_all(tmp.path().join(".rift")).unwrap();
        std::fs::write(
            tmp.path().join(".rift/config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n",
        )
        .unwrap();
        assert_eq!(
            checkpoint_remote(tmp.path()).unwrap(),
            None,
            "a config with no [checkpoint] table names no remote"
        );

        std::fs::write(
            tmp.path().join(".rift/config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n[checkpoint]\nremote = \"rift-origin\"\n",
        )
        .unwrap();
        assert_eq!(
            checkpoint_remote(tmp.path()).unwrap().as_deref(),
            Some("rift-origin")
        );
    }

    /// The value becomes an argv word in a `git` invocation, so the shapes that
    /// stop being a remote name are refusals rather than silent `None`s: a typo
    /// must be reported, not disguised as "not configured".
    #[test]
    fn an_unsafe_checkpoint_remote_is_refused_by_reason_not_by_value() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join(".rift")).unwrap();
        let head = "schema_version = 1\n[project]\nname = \"demo\"\n[checkpoint]\n";
        for (value, needle) in [
            ("\"\"", "empty"),
            ("\"--upload-pack=evil\"", "flag"),
            ("\"a b\"", "whitespace"),
            ("\"a\\tb\"", "whitespace"),
            ("7", "string"),
        ] {
            std::fs::write(
                tmp.path().join(".rift/config.toml"),
                format!("{head}remote = {value}\n"),
            )
            .unwrap();
            let err = checkpoint_remote(tmp.path())
                .expect_err("an unusable remote token must be an error");
            let text = err.to_string();
            assert!(
                text.contains(needle),
                "refusal must name the reason ({needle}), got: {text}"
            );
            assert!(
                !text.contains("evil"),
                "a refusal names the reason, never the value: {text}"
            );
        }
    }

    #[test]
    fn missing_rift_dir_yields_secure_defaults() {
        let dir = std::env::temp_dir().join("rift-config-absent-xyz");
        let eff = EffectiveConfig::load(&dir).unwrap();
        assert!(!eff.raw_transcripts_enabled());
        assert!(eff.is_sensitive_path("project/.env"));
        assert!(eff.is_sensitive_path("a/b/id_rsa"));
    }

    /// A policy file the work tree does not own is not policy. The value read
    /// through the link would be wrong at best; the error raised by failing to
    /// parse it carries a line of the target, which is how a credentials file
    /// ends up on a terminal.
    ///
    /// The control leg is the same bytes in a *regular* file: that still errors,
    /// and it still quotes the line, because there the line is the repository's
    /// own content and the operator needs to see it. The difference between the
    /// two legs is the whole guard.
    #[cfg(unix)]
    #[test]
    fn a_linked_policy_file_is_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("credentials");
        std::fs::write(
            &outside,
            "[default]\naws_access_key_id = AKIAIOSFODNN7EXAMPLE\n",
        )
        .unwrap();

        let rift_dir = tmp.path().join("repo/.rift");
        std::fs::create_dir_all(&rift_dir).unwrap();
        std::os::unix::fs::symlink(&outside, rift_dir.join("config.toml")).unwrap();

        let eff = EffectiveConfig::load(&rift_dir).expect("a link degrades, it does not fail");
        assert_eq!(eff.project_name(), "unnamed", "secure defaults expected");
        assert!(eff.is_sensitive_path("project/.env"));

        // control: the same bytes as a real file are read, and the parse error
        // quotes the line — which is exactly what must not happen to a target.
        let owned = tmp.path().join("owned/.rift");
        std::fs::create_dir_all(&owned).unwrap();
        std::fs::copy(&outside, owned.join("config.toml")).unwrap();
        let err = EffectiveConfig::load(&owned).expect_err("a real file still parses or errors");
        assert!(
            err.to_string().contains("AKIAIOSFODNN7EXAMPLE"),
            "control leg must show the mechanism the guard defeats: {err}"
        );
    }

    /// The same for a linked `.rift` itself: the directory name is repository
    /// content too, and `<link>/config.toml` resolves through it just as well.
    #[cfg(unix)]
    #[test]
    fn a_linked_rift_directory_is_not_read() {
        let tmp = tempfile::tempdir().unwrap();
        let elsewhere = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(
            elsewhere.join("config.toml"),
            "schema_version = 1\n[project]\nname = \"leaked\"\n",
        )
        .unwrap();

        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let rift_dir = repo.join(".rift");
        std::os::unix::fs::symlink(&elsewhere, &rift_dir).unwrap();

        let eff = EffectiveConfig::load(&rift_dir).unwrap();
        assert_eq!(
            eff.project_name(),
            "unnamed",
            "a linked .rift must not supply policy"
        );
    }

    /// A directory where the policy file belongs used to reach `read_to_string`
    /// and surface an IO error; it degrades like every other unreadable shape.
    #[test]
    fn a_directory_in_the_policy_files_place_degrades() {
        let tmp = tempfile::tempdir().unwrap();
        let rift_dir = tmp.path().join(".rift");
        std::fs::create_dir_all(rift_dir.join("config.toml")).unwrap();
        let eff = EffectiveConfig::load(&rift_dir).expect("degrades, never fails");
        assert_eq!(eff.project_name(), "unnamed");
    }

    /// The guard does not cost the ordinary case: a real file in a real `.rift`
    /// is still read and still applied.
    #[test]
    fn an_owned_policy_file_is_still_read() {
        let tmp = tempfile::tempdir().unwrap();
        let rift_dir = tmp.path().join(".rift");
        std::fs::create_dir_all(&rift_dir).unwrap();
        std::fs::write(
            rift_dir.join("config.toml"),
            "schema_version = 1\n[project]\nname = \"demo\"\n",
        )
        .unwrap();
        let eff = EffectiveConfig::load(&rift_dir).unwrap();
        assert_eq!(eff.project_name(), "demo");
    }

    /// Refusing is right; refusing **silently** is the half that was missing.
    ///
    /// Every degraded shape produces the same secure defaults, so no caller can
    /// tell "this project declared no policy" from "this project's policy was
    /// refused and none of it is running" — the built-in floor holds either way,
    /// and what is lost is exactly what the developer wrote. `policy_source`
    /// answers it, absent and refused are different answers, and each refusal
    /// carries the reason `symlink_metadata` actually saw.
    #[cfg(unix)]
    #[test]
    fn a_refused_policy_file_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&outside).unwrap();
        let declared = "schema_version = 1\n[project]\nname = \"declared\"\n\
                        [paths]\nsensitive_paths = [\"**/*.ledger\"]\n";
        std::fs::write(outside.join("config.toml"), declared).unwrap();

        // 1. no .rift at all -> Default, no diagnostic
        let absent = tmp.path().join("none/.rift");
        let eff = EffectiveConfig::load(&absent).unwrap();
        assert_eq!(eff.policy_source(), PolicySource::Default);
        assert!(eff.policy_source().diagnostic().is_none());

        // 2. a real .rift with a real config -> Declared
        let owned = tmp.path().join("owned/.rift");
        std::fs::create_dir_all(&owned).unwrap();
        std::fs::write(owned.join("config.toml"), declared).unwrap();
        let applied = EffectiveConfig::load(&owned).unwrap();
        assert_eq!(applied.policy_source(), PolicySource::Declared);
        assert!(applied.policy_source().is_declared_policy_applied());
        assert!(applied.is_sensitive_path("book.ledger"));

        // 3. a linked config.toml -> Refused, and the reason names the link
        let linked_file = tmp.path().join("lf/.rift");
        std::fs::create_dir_all(&linked_file).unwrap();
        std::os::unix::fs::symlink(outside.join("config.toml"), linked_file.join("config.toml"))
            .unwrap();
        // 4. a linked .rift directory -> Refused, different reason
        let linked_dir_repo = tmp.path().join("ld");
        std::fs::create_dir_all(&linked_dir_repo).unwrap();
        let linked_dir = linked_dir_repo.join(".rift");
        std::os::unix::fs::symlink(&outside, &linked_dir).unwrap();
        // 5. a directory where the file belongs -> Refused, third reason
        let dir_in_place = tmp.path().join("dip/.rift");
        std::fs::create_dir_all(dir_in_place.join("config.toml")).unwrap();

        for (dir, reason) in [
            (&linked_file, PolicyRefusal::ConfigIsASymlink),
            (&linked_dir, PolicyRefusal::RiftDirIsNotADirectory),
            (&dir_in_place, PolicyRefusal::ConfigIsNotARegularFile),
        ] {
            let eff = EffectiveConfig::load(dir).expect("a refusal degrades, it does not fail");
            assert_eq!(
                eff.policy_source(),
                PolicySource::Refused(reason),
                "{}",
                dir.display()
            );
            assert!(!eff.policy_source().is_declared_policy_applied());
            let diagnostic = eff
                .policy_source()
                .diagnostic()
                .expect("a refusal must carry a diagnostic");
            assert!(diagnostic.contains("declared policy is not applied"));
            // The built-in floor is identical to the ordinary default load --
            // which is precisely why nothing else can notice the refusal.
            let defaults = EffectiveConfig::secure_default().unwrap();
            assert_eq!(eff.sensitive_paths(), defaults.sensitive_paths());
            assert_eq!(eff.policy_hash(), defaults.policy_hash());
            // ...and the declared policy is gone.
            assert!(!eff.is_sensitive_path("book.ledger"));
        }
    }

    /// The provenance is not part of the policy's identity: two projects with
    /// the same policy hash the same, however each of them got there.
    #[test]
    fn policy_source_is_not_part_of_the_policy_hash() {
        let a = EffectiveConfig::secure_default().unwrap();
        let b = EffectiveConfig::from_config(RiftConfig::secure_default()).unwrap();
        assert_ne!(a.policy_source(), b.policy_source());
        assert_eq!(a.policy_hash(), b.policy_hash());
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

    /// Contract snapshots: a representative effective
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

    /// Declaring the search key — under either spelling, at either value —
    /// leaves the policy hash byte-identical to a config that never mentions
    /// it. That is what makes the rename safe to ship: `policy_ver` is stamped
    /// into served summaries, so a key that fed the hash would invalidate every
    /// repository's derived memory the day this landed.
    #[test]
    fn the_search_feature_key_is_hash_inert_under_both_spellings() {
        let plain = parse("schema_version=1\n[project]\nname=\"x\"\n").unwrap();
        for src in [
            "schema_version=1\n[project]\nname=\"x\"\n[features]\nsearch=true\n",
            "schema_version=1\n[project]\nname=\"x\"\n[features]\nsearch=false\n",
            "schema_version=1\n[project]\nname=\"x\"\n[features]\nfff=true\n",
            "schema_version=1\n[project]\nname=\"x\"\n[features]\nfff=false\n",
        ] {
            let cfg = parse(src).unwrap();
            assert_eq!(plain.policy_hash(), cfg.policy_hash(), "hashed: {src:?}");
            assert!(cfg.recall_enabled(), "recall follows its own key: {src:?}");
        }
    }

    /// Recall is on unless the repository declares it off: an absent
    /// `[features]` table, a table without the key, and an explicit `true`
    /// all enable it; only `total_recall = false` turns it off. The absent
    /// and explicit-on forms hash identically, so a repository that writes
    /// out the default does not invalidate its derived memory.
    #[test]
    fn recall_is_on_unless_declared_off() {
        let absent = parse("schema_version=1\n[project]\nname=\"x\"\n").unwrap();
        assert!(absent.recall_enabled(), "no [features] table: on");
        let empty_table =
            parse("schema_version=1\n[project]\nname=\"x\"\n[features]\nsearch=true\n").unwrap();
        assert!(empty_table.recall_enabled(), "a table without the key: on");
        let explicit_on =
            parse("schema_version=1\n[project]\nname=\"x\"\n[features]\ntotal_recall=true\n")
                .unwrap();
        assert!(explicit_on.recall_enabled());
        assert_eq!(absent.policy_hash(), explicit_on.policy_hash());
        let declared_off =
            parse("schema_version=1\n[project]\nname=\"x\"\n[features]\ntotal_recall=false\n")
                .unwrap();
        assert!(
            !declared_off.recall_enabled(),
            "only a written false is off"
        );
        assert_ne!(absent.policy_hash(), declared_off.policy_hash());
        assert!(
            EffectiveConfig::secure_default().unwrap().recall_enabled(),
            "the no-.rift default agrees with the absent-key default"
        );
        assert!(FeaturePolicy::default().total_recall);
    }

    /// The retired spelling loads and asks to be renamed exactly once; the
    /// current spelling and an absent `[features]` say nothing. A repository
    /// configured before the rename therefore keeps working for this release
    /// and still finds out.
    #[test]
    fn the_retired_search_spelling_asks_for_the_rename_and_the_current_one_is_silent() {
        let notice = parse("schema_version=1\n[project]\nname=\"x\"\n[features]\nfff=false\n")
            .unwrap()
            .retired_feature_key_notice()
            .expect("the retired spelling is present, so it is reported");
        assert!(notice.contains("fff"), "names the key to rename: {notice}");
        assert!(notice.contains("search"), "names the new key: {notice}");
        assert!(
            notice.contains(".rift/config.toml"),
            "names the file to edit: {notice}"
        );
        for quiet in [
            "schema_version=1\n[project]\nname=\"x\"\n[features]\nsearch=true\n",
            "schema_version=1\n[project]\nname=\"x\"\n[features]\ntotal_recall=true\n",
            "schema_version=1\n[project]\nname=\"x\"\n",
        ] {
            assert!(
                parse(quiet).unwrap().retired_feature_key_notice().is_none(),
                "nothing to rename: {quiet:?}"
            );
        }
    }

    /// The graph-analysis feature key is not renamed, it is refused: the
    /// packages behind it were deleted by owner decision, so accepting policy
    /// for it would be accepting policy for something that cannot exist. The
    /// error names the offending key, which is what makes "delete the line" a
    /// fix a reader can perform without this crate's source.
    #[test]
    fn the_deleted_graph_feature_key_no_longer_parses() {
        let err = parse("schema_version=1\n[project]\nname=\"x\"\n[features]\ncodegraph=true\n")
            .expect_err("the key was removed from the accepted set")
            .to_string();
        assert!(err.contains("codegraph"), "names the refused key: {err}");
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
