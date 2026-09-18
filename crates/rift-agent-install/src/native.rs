//! Native capture-integration lifecycle driven by the complete adapter registry.
//!
//! This is deliberately separate from the legacy four-client Recall
//! installer. Detection and managed capture wiring are independent facts; no
//! server probe, connection lease, or transport state participates here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_core::AgentId;
use rift_shadow_hooks::{AdapterRegistry, AgentAdapter};
use serde::{Deserialize, Serialize};

use crate::InstallError;

/// Whether the agent is installed on this machine, by any of the evidence
/// classes in [`crate::detect`]: a `PATH` command, a vendor install, a
/// verified IDE extension, a JetBrains plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeDetection {
    /// At least one install was found.
    Detected,
    /// No install was found; the integration remains selectable.
    NotDetected,
}

/// Ownership/validity of Rift's project-local capture descriptor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeWiring {
    /// No descriptor exists.
    Missing,
    /// A Rift-managed descriptor exists and its fingerprint is current.
    Managed,
    /// A file exists at the managed path but Rift cannot prove ownership.
    Conflict,
    /// The descriptor is not valid JSON.
    Invalid,
}

/// The alpha's supported set and its predicate live beside the adapter
/// registry in `rift-shadow-hooks`, and are re-exported here because this is
/// the crate that enforces them: [`enable`] refuses everything outside
/// [`ALPHA_AGENTS`]. The definition moved so the completeness sweep can bind
/// its unsupported-adapter rows to the very list the installer refuses on —
/// a second copy would drift, and the drift would read as a table calling an
/// agent not-yet-supported while `enable` wired it.
pub use rift_shadow_hooks::{is_alpha_supported, ALPHA_AGENTS};

/// Every token in `selection` this alpha cannot capture — **all of them**, in
/// the order given, deduplicated. Empty means the whole selection is wirable.
///
/// This exists so a caller can ask the question **once, up front, about the
/// whole selection**, and both halves of that sentence were paid for:
///
/// * *Once* — [`enable`]'s refusal is per-agent, so a caller looping over a
///   selection discovers offenders one run at a time. Naming the second only
///   after the user has removed the first is a worse answer than naming both.
/// * *Up front* — `rift init` refused inside its wiring loop, which runs
///   **after** the git hooks are installed, so the first refusal returned an
///   error over a half-initialised repository. A selection that will be
///   refused must be refused before anything is written.
///
/// Note what this does **not** check: registry membership. An unknown token is
/// a different refusal with a different message ([`enable`] gives it one), and
/// folding the two together would report an agent that does not exist as one
/// that is merely coming soon.
#[must_use]
pub fn unsupported_alpha_agents<'a, I>(selection: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut refused: Vec<String> = Vec::new();
    for token in selection {
        if !is_alpha_supported(token) && !refused.iter().any(|seen| seen == token) {
            refused.push(token.to_owned());
        }
    }
    refused
}

/// Ownership/validity of an agent's native project-hook surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NativeHookWiring {
    /// No Rift hook entries exist.
    Missing,
    /// Rift hook entries match the current ownership fingerprint.
    Managed,
    /// A recognizable pre-manifest Rift hook bundle can be refreshed safely.
    Stale,
    /// Rift-like hook entries exist but ownership cannot be proved.
    Conflict,
    /// The hook document is not a valid JSON object.
    Invalid,
}

/// Whether Codex has recorded trust for a project's `.codex/hooks.json`.
///
/// Codex's own state, read and never written. A member's fresh hook
/// file is a new absolute path, hence untrusted until the user approves it once
/// inside Codex.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexTrust {
    /// Codex has recorded trust for this exact path.
    Approved,
    /// Codex has no record of this path; the user approves it once, in Codex.
    Pending,
}

impl CodexTrust {
    /// The line adoption and `rift worktree status` print.
    #[must_use]
    pub const fn message(self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Pending => "pending (approve once in Codex)",
        }
    }
}

/// The detection vocabulary — one rule for every surface (see
/// [`crate::detect`]).
pub use crate::detect::{
    DetectionEnvironment, NativeIdeHost, NativeInstallSource, NativeInstallation,
};

/// Truthful read-only state for one adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeIntegrationStatus {
    /// Registry token.
    pub agent: String,
    /// Human-facing name.
    pub display_name: String,
    /// Whether any install evidence was found — `installations` is non-empty.
    /// Independent of wiring.
    pub detection: NativeDetection,
    /// Managed project-local wiring state.
    pub wiring: NativeWiring,
    /// The first runnable binary among `installations`, if any provides one.
    pub executable: Option<PathBuf>,
    /// Every install found, in [`crate::detect`]'s reporting order: the `PATH`
    /// command, vendor installs, IDE extensions, JetBrains plugins.
    pub installations: Vec<NativeInstallation>,
    /// Native project-hook state when the adapter exposes a separately managed
    /// hook surface: present for the three supported alpha families whose hook
    /// file explicit [`enable`] writes, `None` for every other adapter.
    pub hook_wiring: Option<NativeHookWiring>,
}

impl NativeIntegrationStatus {
    /// Whether this integration is enabled by a managed descriptor.
    #[must_use]
    pub fn enabled(&self) -> bool {
        self.wiring == NativeWiring::Managed
            && self
                .hook_wiring
                .is_none_or(|hooks| hooks == NativeHookWiring::Managed)
    }

    /// Whether the agent is installed on this machine by any evidence class.
    /// **The** predicate: `rift init`'s auto-wire, Canopy's pre-tick, doctor
    /// and `rift recall status` all read this one.
    #[must_use]
    pub fn detected(&self) -> bool {
        self.detection == NativeDetection::Detected
    }

    /// The one sentence every surface prints for detection: `absent`, or
    /// `present via <source> [<version>]` for each install found, joined by
    /// ` + ` — so a user surprised by an auto-wire can read why Rift believes
    /// the agent is installed.
    #[must_use]
    pub fn availability(&self) -> String {
        crate::describe_installations(&self.installations)
    }
}

/// Result of one lifecycle operation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NativeIntegrationReport {
    /// Repository-owned files whose bytes changed.
    pub changed_files: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OwnershipManifest {
    #[serde(default = "manifest_version")]
    schema_version: u16,
    #[serde(default)]
    descriptors: BTreeMap<String, String>,
    #[serde(default)]
    hooks: BTreeMap<String, OwnedHook>,
    #[serde(default)]
    skills: BTreeMap<String, OwnedSkill>,
}

impl Default for OwnershipManifest {
    fn default() -> Self {
        Self {
            schema_version: manifest_version(),
            descriptors: BTreeMap::new(),
            hooks: BTreeMap::new(),
            skills: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OwnedSkill {
    path: String,
    region_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct OwnedHook {
    path: String,
    fragment_hash: String,
}

const fn manifest_version() -> u16 {
    1
}

/// Diagnose every adapter in stable registry order.
#[must_use]
pub fn diagnose_all(repo: &Path) -> Vec<NativeIntegrationStatus> {
    diagnose_all_with_environment(repo, &DetectionEnvironment::from_process())
}

/// [`diagnose_all`] over an injected machine — the form every test uses, and
/// every caller that already holds an environment, so one report never reads
/// the process twice.
#[must_use]
pub fn diagnose_all_with_environment(
    repo: &Path,
    env: &DetectionEnvironment,
) -> Vec<NativeIntegrationStatus> {
    let registry = AdapterRegistry::with_builtin();
    registry
        .agents()
        .iter()
        .filter_map(|agent| {
            registry
                .get(agent)
                .map(|adapter| diagnose_one(repo, agent, adapter, env))
        })
        .collect()
}

/// Enable one adapter using the same descriptor lifecycle used by the CLI and
/// Project Settings modal.
///
/// # Errors
/// Unknown adapters and unowned/modified descriptors are refused.
pub fn enable(
    repo: &Path,
    rift_bin: &Path,
    agent_token: &str,
) -> Result<NativeIntegrationReport, InstallError> {
    enable_with_policy(repo, rift_bin, agent_token, true)
}

/// Provision an enabled adapter in an automatically adopted worktree.
///
/// This may refresh Rift-owned descriptors, but it never writes
/// Antigravity's tracked `.agents/hooks.json`; only the operator's explicit
/// [`enable`] command has that authority.
///
/// # Errors
/// The same descriptor ownership checks as [`enable`] apply.
pub fn enable_for_adoption(
    repo: &Path,
    rift_bin: &Path,
    agent_token: &str,
) -> Result<NativeIntegrationReport, InstallError> {
    enable_with_policy(repo, rift_bin, agent_token, false)
}

fn enable_with_policy(
    repo: &Path,
    rift_bin: &Path,
    agent_token: &str,
    write_tracked_hook: bool,
) -> Result<NativeIntegrationReport, InstallError> {
    let registry = AdapterRegistry::with_builtin();
    let agent = AgentId::new(agent_token);
    let adapter = registry.get(&agent).ok_or_else(|| InstallError::Config {
        path: descriptor_path(repo, agent_token),
        reason: "no built-in adapter exists for this agent".to_owned(),
    })?;
    if adapter.verbs().is_empty() {
        return Err(InstallError::Config {
            path: descriptor_path(repo, agent_token),
            reason: "adapter declares no native lifecycle verbs".to_owned(),
        });
    }
    // The alpha-set refusal happens HERE — before the descriptor bytes are built,
    // before the manifest is read, before `prepare_hook_enable` — so a
    // not-yet-supported agent cannot leave a partial install behind. A
    // "coming soon" path that writes a descriptor anyway is exactly the class
    // this refusal exists to remove.
    if !is_alpha_supported(agent_token) {
        return Err(InstallError::Config {
            path: descriptor_path(repo, agent_token),
            reason: format!(
                "capture for {agent_token} is not supported yet — this alpha captures {}. \
                 The adapter stays listed, but Rift will not write a descriptor that wires \
                 a hook surface this agent does not send",
                ALPHA_AGENTS.join(", ")
            ),
        });
    }

    let path = descriptor_path(repo, agent_token);
    let bytes = descriptor_bytes(
        rift_bin,
        agent_token,
        adapter.verbs(),
        &adapter.compat_notes(),
    );
    let next_hash = fingerprint(&bytes);
    let mut manifest = read_manifest(repo);
    let family = writable_hook_family(agent_token, write_tracked_hook);
    let client_hook = family
        .map(|family| prepare_hook_enable(repo, family, rift_bin, &manifest))
        .transpose()?;
    let mut changed = Vec::new();
    let mut write_descriptor = false;
    if let Some(existing) = read_optional(&path)? {
        let existing_hash = fingerprint(&existing);
        let owned_hash = manifest.descriptors.get(agent_token);
        let exact = existing == bytes;
        let manifest_owned = owned_hash.is_some_and(|hash| hash == &existing_hash);
        let self_identifying = owned_descriptor(&existing, agent_token);
        let recognized_legacy = legacy_owned_descriptor(&existing, agent_token, adapter);
        if !(exact || manifest_owned && self_identifying || recognized_legacy) {
            return Err(InstallError::Config {
                path,
                reason: "managed integration file differs from Rift's ownership fingerprint"
                    .to_owned(),
            });
        }
        if !exact {
            write_descriptor = true;
        }
    } else {
        write_descriptor = true;
    }
    if write_descriptor {
        super::atomic_write(&path, &bytes)?;
        changed.push(path.clone());
    }
    manifest
        .descriptors
        .insert(agent_token.to_owned(), next_hash);
    if let (Some(family), Some((hook_path, hook_bytes, fragment_hash))) = (family, client_hook) {
        if std::fs::read(&hook_path).ok().as_deref() != Some(hook_bytes.as_slice()) {
            super::atomic_write(&hook_path, &hook_bytes)?;
            changed.push(hook_path);
        }
        manifest.hooks.insert(
            agent_token.to_owned(),
            OwnedHook {
                path: family.hook_path().to_owned(),
                fragment_hash,
            },
        );
    }
    if write_manifest(repo, &manifest)? {
        changed.push(manifest_path(repo));
    }
    Ok(NativeIntegrationReport {
        changed_files: changed,
    })
}

/// Disable one adapter, removing only a descriptor whose current bytes match
/// Rift's recorded fingerprint.
///
/// # Errors
/// A modified/unowned descriptor is refused and left byte-identical.
pub fn disable(repo: &Path, agent_token: &str) -> Result<NativeIntegrationReport, InstallError> {
    disable_with_policy(repo, agent_token, true)
}

/// Remove an enabled adapter during automatic worktree reconciliation without
/// editing Antigravity's tracked hook file.
///
/// # Errors
/// The same descriptor ownership checks as [`disable`] apply.
pub fn disable_for_adoption(
    repo: &Path,
    agent_token: &str,
) -> Result<NativeIntegrationReport, InstallError> {
    disable_with_policy(repo, agent_token, false)
}

fn disable_with_policy(
    repo: &Path,
    agent_token: &str,
    write_tracked_hook: bool,
) -> Result<NativeIntegrationReport, InstallError> {
    let path = descriptor_path(repo, agent_token);
    let mut manifest = read_manifest(repo);
    let client_hook = writable_hook_family(agent_token, write_tracked_hook)
        .map(|family| prepare_hook_disable(repo, family, &manifest))
        .transpose()?
        .flatten();
    let mut changed = Vec::new();
    let mut remove_descriptor = false;
    if let Some(existing) = read_optional(&path)? {
        let registry = AdapterRegistry::with_builtin();
        let agent = AgentId::new(agent_token);
        let current_hash = fingerprint(&existing);
        let owned = manifest
            .descriptors
            .get(agent_token)
            .is_some_and(|hash| hash == &current_hash)
            && owned_descriptor(&existing, agent_token)
            || registry
                .get(&agent)
                .is_some_and(|adapter| legacy_owned_descriptor(&existing, agent_token, adapter));
        if !owned {
            return Err(InstallError::Config {
                path,
                reason: "managed integration file differs from Rift's ownership fingerprint"
                    .to_owned(),
            });
        }
        remove_descriptor = true;
    }
    if remove_descriptor {
        std::fs::remove_file(&path).map_err(|source| InstallError::Io {
            path: path.clone(),
            source,
        })?;
        changed.push(path);
    }
    if let Some((hook_path, hook_bytes)) = client_hook {
        if std::fs::read(&hook_path).ok().as_deref() != Some(hook_bytes.as_slice()) {
            super::atomic_write(&hook_path, &hook_bytes)?;
            changed.push(hook_path);
        }
    }
    manifest.descriptors.remove(agent_token);
    if write_tracked_hook || agent_token != "antigravity" {
        manifest.hooks.remove(agent_token);
    }
    if write_manifest(repo, &manifest)? {
        changed.push(manifest_path(repo));
    }
    Ok(NativeIntegrationReport {
        changed_files: changed,
    })
}

/// Install the Recall pointer for one enabled native adapter. Shared agent
/// instruction files keep user content around a token-specific managed fence;
/// agent-specific files are never adopted when an unmarked file already exists.
///
/// # Errors
/// Refuses an unowned file at a Rift-specific path and reports I/O failures.
pub fn install_recall_skill(
    repo: &Path,
    agent_token: &str,
    body: &str,
) -> Result<NativeIntegrationReport, InstallError> {
    if !is_alpha_supported(agent_token) {
        return Err(InstallError::Config {
            path: repo.join(".rift/agents"),
            reason: format!(
                "recall rules for {agent_token} are coming soon; this alpha supports {}",
                ALPHA_AGENTS.join(", ")
            ),
        });
    }
    let rel = skill_path(agent_token).ok_or_else(|| InstallError::Config {
        path: repo.join(".rift/agents"),
        reason: "no managed skill channel exists for this adapter".to_owned(),
    })?;
    let path = repo.join(rel);
    let region = skill_region(agent_token, body);
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            return Err(InstallError::Io {
                path: path.clone(),
                source,
            })
        }
    };
    let shared = skill_is_shared(agent_token);
    let next = match existing.as_deref() {
        None => format!(
            "{}{region}",
            skill_frontmatter(agent_token).unwrap_or_default()
        ),
        Some(text) => match skill_fence(text, agent_token) {
            Some((start, end)) => format!("{}{}{}", &text[..start], region, &text[end..]),
            None if shared => {
                let separator = if text.ends_with('\n') { "\n" } else { "\n\n" };
                format!("{text}{separator}{region}")
            }
            None => {
                return Err(InstallError::Config {
                    path,
                    reason: "an unowned file exists at Rift's managed skill path".to_owned(),
                })
            }
        },
    };
    let mut changed = Vec::new();
    if existing.as_deref() != Some(next.as_str()) {
        super::atomic_write(&path, next.as_bytes())?;
        changed.push(path.clone());
    }
    let mut manifest = read_manifest(repo);
    manifest.skills.insert(
        agent_token.to_owned(),
        OwnedSkill {
            path: rel.to_owned(),
            region_hash: fingerprint(region.as_bytes()),
        },
    );
    if write_manifest(repo, &manifest)? {
        changed.push(manifest_path(repo));
    }
    Ok(NativeIntegrationReport {
        changed_files: changed,
    })
}

/// Remove only the fingerprint-matching Recall block for one adapter.
///
/// # Errors
/// Refuses a modified managed block and leaves it untouched.
pub fn remove_recall_skill(
    repo: &Path,
    agent_token: &str,
) -> Result<NativeIntegrationReport, InstallError> {
    let mut manifest = read_manifest(repo);
    let Some(owned) = manifest.skills.get(agent_token).cloned() else {
        return Ok(NativeIntegrationReport::default());
    };
    let path = repo.join(&owned.path);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(InstallError::Io {
                path: path.clone(),
                source,
            })
        }
    };
    let mut changed = Vec::new();
    if !text.is_empty() {
        let Some((start, end)) = skill_fence(&text, agent_token) else {
            return Err(InstallError::Config {
                path,
                reason: "managed skill fence is missing".to_owned(),
            });
        };
        let Some(region) = text.get(start..end) else {
            return Err(InstallError::Config {
                path,
                reason: "managed skill fence is invalid".to_owned(),
            });
        };
        if fingerprint(region.as_bytes()) != owned.region_hash {
            return Err(InstallError::Config {
                path,
                reason: "managed skill block differs from Rift's ownership fingerprint".to_owned(),
            });
        }
        let rest = format!("{}{}", &text[..start], &text[end..]);
        if skill_is_shared(agent_token) && !rest.trim().is_empty() {
            super::atomic_write(&path, format!("{}\n", rest.trim_end()).as_bytes())?;
        } else {
            std::fs::remove_file(&path).map_err(|source| InstallError::Io {
                path: path.clone(),
                source,
            })?;
        }
        changed.push(path);
    }
    manifest.skills.remove(agent_token);
    if write_manifest(repo, &manifest)? {
        changed.push(manifest_path(repo));
    }
    Ok(NativeIntegrationReport {
        changed_files: changed,
    })
}

fn diagnose_one(
    repo: &Path,
    agent: &AgentId,
    adapter: &dyn AgentAdapter,
    env: &DetectionEnvironment,
) -> NativeIntegrationStatus {
    let token = agent.as_str();
    let installations = crate::detect::installations(token, env);
    let executable = crate::detect::first_executable(&installations);
    let wiring = inspect_wiring(repo, token, adapter);
    NativeIntegrationStatus {
        agent: token.to_owned(),
        display_name: display_name(token).to_owned(),
        detection: if installations.is_empty() {
            NativeDetection::NotDetected
        } else {
            NativeDetection::Detected
        },
        wiring,
        executable,
        installations,
        hook_wiring: hook_family(token).map(|family| inspect_client_hooks(repo, family, wiring)),
    }
}

fn inspect_wiring(repo: &Path, token: &str, adapter: &dyn AgentAdapter) -> NativeWiring {
    let path = descriptor_path(repo, token);
    let Ok(bytes) = std::fs::read(&path) else {
        return NativeWiring::Missing;
    };
    if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
        return NativeWiring::Invalid;
    }
    let manifest = read_manifest(repo);
    let hash = fingerprint(&bytes);
    if owned_descriptor(&bytes, token)
        && manifest
            .descriptors
            .get(token)
            .is_some_and(|expected| expected == &hash)
        || legacy_owned_descriptor(&bytes, token, adapter)
    {
        NativeWiring::Managed
    } else {
        NativeWiring::Conflict
    }
}

/// The client families whose native project-hook file explicit [`enable`]
/// writes, keyed by registry token. Antigravity's `.agents/hooks.json` is
/// tracked, so automatic worktree adoption remains verify-only; the explicit
/// command is the sole native path allowed to fingerprint-merge its reserved
/// `rift-recall` bundle. Gemini is outside this alpha and [`enable`] refuses it
/// long before here.
///
/// Claude Code joined this table late. Until then the only emitter of its
/// fragment was the legacy whole-set installer, whose last product caller went
/// away in `fce2baea` (2026-08-11) — so `rift enable --agent claude-code`
/// wrote a descriptor advertising nine hooks and registered none of them, in every
/// fresh clone and every linked worktree, and nothing reddened because nothing
/// compared the two.
fn hook_family(agent_token: &str) -> Option<super::ClientFamily> {
    match agent_token {
        "claude-code" => Some(super::ClientFamily::ClaudeCode),
        "codex" => Some(super::ClientFamily::Codex),
        "antigravity" => Some(super::ClientFamily::Antigravity),
        _ => None,
    }
}

fn writable_hook_family(
    agent_token: &str,
    write_tracked_hook: bool,
) -> Option<super::ClientFamily> {
    hook_family(agent_token)
        .filter(|family| write_tracked_hook || *family != super::ClientFamily::Antigravity)
}

/// The repository-relative native hook file Rift writes for `agent_token`, if
/// it writes one — so a caller can NAME it (an enable's output line, a
/// refusal's repair instruction) without keeping a second table of paths.
#[must_use]
pub fn hook_file(agent_token: &str) -> Option<&'static str> {
    hook_family(agent_token).map(super::ClientFamily::hook_path)
}

fn client_hook_path(repo: &Path, family: super::ClientFamily) -> PathBuf {
    repo.join(family.hook_path())
}

/// Refuse a symlinked hook file, and a symlinked project directory around it,
/// before anything is written through either.
fn refuse_hook_symlinks(path: &Path, family: super::ClientFamily) -> Result<(), InstallError> {
    if let Some(dir) = path.parent() {
        refuse_symlink(dir, &format!("{} project directory", family.display_name()))?;
    }
    refuse_symlink(path, &format!("{} hook file", family.display_name()))
}

fn inspect_client_hooks(
    repo: &Path,
    family: super::ClientFamily,
    descriptor_wiring: NativeWiring,
) -> NativeHookWiring {
    let path = client_hook_path(repo, family);
    if !path.exists() {
        return NativeHookWiring::Missing;
    }
    if path.parent().is_some_and(path_is_symlink) || path_is_symlink(&path) {
        return NativeHookWiring::Conflict;
    }
    let Ok(bytes) = std::fs::read(&path) else {
        return NativeHookWiring::Invalid;
    };
    let Ok(root) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return NativeHookWiring::Invalid;
    };
    let Some(root) = root.as_object() else {
        return NativeHookWiring::Invalid;
    };
    if family == super::ClientFamily::Antigravity {
        return inspect_antigravity_hooks(repo, root, descriptor_wiring);
    }
    let Some(hooks) = root.get("hooks") else {
        return NativeHookWiring::Missing;
    };
    let Some(hooks) = hooks.as_object() else {
        return NativeHookWiring::Invalid;
    };
    let current = super::hook_subset(hooks, family);
    let contains_rift =
        super::fragment_contains_rift_command(&serde_json::Value::Object(hooks.clone()), family);
    if !contains_rift {
        return NativeHookWiring::Missing;
    }
    if !super::fragment_contains_rift_command(&current, family) {
        return NativeHookWiring::Conflict;
    }
    let current_hash = super::fingerprint_json(&current);
    let manifest = read_manifest(repo);
    if manifest
        .hooks
        .get(family.token())
        .is_some_and(|owned| owned.fragment_hash == current_hash)
    {
        return NativeHookWiring::Managed;
    }
    if descriptor_wiring == NativeWiring::Managed {
        if let Some(rift_bin) = descriptor_rift_bin(repo, family.token()) {
            if current == super::hook_fragment(family, &rift_bin) {
                return NativeHookWiring::Managed;
            }
        }
    }
    if legacy_owned_hook_fragment(repo, family, &current) {
        NativeHookWiring::Stale
    } else {
        NativeHookWiring::Conflict
    }
}

fn inspect_antigravity_hooks(
    repo: &Path,
    root: &serde_json::Map<String, serde_json::Value>,
    descriptor_wiring: NativeWiring,
) -> NativeHookWiring {
    let family = super::ClientFamily::Antigravity;
    let Some(current) = root.get("rift-recall") else {
        return NativeHookWiring::Missing;
    };
    let current_hash = super::fingerprint_json(current);
    let manifest = read_manifest(repo);
    if manifest
        .hooks
        .get(family.token())
        .is_some_and(|owned| owned.fragment_hash == current_hash)
    {
        return NativeHookWiring::Managed;
    }
    if descriptor_wiring == NativeWiring::Managed {
        if let Some(rift_bin) = descriptor_rift_bin(repo, family.token()) {
            if current == &super::hook_fragment(family, &rift_bin) {
                return NativeHookWiring::Managed;
            }
        }
    }
    if recognizable_antigravity_hook_fragment(current) {
        NativeHookWiring::Stale
    } else {
        NativeHookWiring::Conflict
    }
}

/// Plan the write of one client's hook fragment: the path, the merged bytes,
/// and the fingerprint the ownership manifest will record.
///
/// Nothing is written here. A refusal must leave the tree exactly as it was,
/// and [`enable`] writes the descriptor first, so the conflict has to be known
/// before either file is touched.
fn prepare_hook_enable(
    repo: &Path,
    family: super::ClientFamily,
    rift_bin: &Path,
    manifest: &OwnershipManifest,
) -> Result<(PathBuf, Vec<u8>, String), InstallError> {
    let path = client_hook_path(repo, family);
    refuse_hook_symlinks(&path, family)?;
    let root = super::read_json_object(&path)?;
    if family == super::ClientFamily::Antigravity {
        return prepare_antigravity_hook_enable(path, root, rift_bin, manifest);
    }
    let hook_value = root
        .get("hooks")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let current = hook_value.as_object().map_or_else(
        || serde_json::json!({}),
        |hooks| super::hook_subset(hooks, family),
    );
    let contains_rift = super::fragment_contains_rift_command(&hook_value, family);
    let current_hash = super::fingerprint_json(&current);
    let manifest_owned = manifest
        .hooks
        .get(family.token())
        .is_some_and(|owned| owned.fragment_hash == current_hash);
    if contains_rift && !(manifest_owned || legacy_owned_hook_fragment(repo, family, &current)) {
        return Err(InstallError::Config {
            path,
            reason: format!(
                "{} Rift hooks differ from Rift's ownership fingerprint",
                family.display_name()
            ),
        });
    }
    let expected = super::hook_fragment(family, rift_bin);
    let previous = contains_rift.then_some(current_hash);
    let bytes = super::merge_standard_hooks(&path, family, &expected, previous.as_deref(), false)?;
    Ok((path, bytes, super::fingerprint_json(&expected)))
}

fn prepare_antigravity_hook_enable(
    path: PathBuf,
    root: serde_json::Map<String, serde_json::Value>,
    rift_bin: &Path,
    manifest: &OwnershipManifest,
) -> Result<(PathBuf, Vec<u8>, String), InstallError> {
    let family = super::ClientFamily::Antigravity;
    let expected = super::hook_fragment(family, rift_bin);
    let previous = if let Some(current) = root.get("rift-recall") {
        let current_hash = super::fingerprint_json(current);
        let manifest_owned = manifest
            .hooks
            .get(family.token())
            .is_some_and(|owned| owned.fragment_hash == current_hash);
        let has_manifest_record = manifest.hooks.contains_key(family.token());
        let safely_owned = if has_manifest_record {
            manifest_owned
        } else {
            current == &expected
        };
        if !safely_owned {
            return Err(InstallError::Config {
                path,
                reason:
                    "reserved `rift-recall` hook bundle differs from Rift's ownership fingerprint"
                        .to_owned(),
            });
        }
        Some(current_hash)
    } else {
        None
    };
    let bytes = super::merge_antigravity_hooks(&path, &expected, previous.as_deref(), false)?;
    Ok((path, bytes, super::fingerprint_json(&expected)))
}

fn prepare_hook_disable(
    repo: &Path,
    family: super::ClientFamily,
    manifest: &OwnershipManifest,
) -> Result<Option<(PathBuf, Vec<u8>)>, InstallError> {
    let path = client_hook_path(repo, family);
    refuse_hook_symlinks(&path, family)?;
    if !path.exists() {
        return Ok(None);
    }
    let root = super::read_json_object(&path)?;
    if family == super::ClientFamily::Antigravity {
        return prepare_antigravity_hook_disable(repo, path, root, manifest);
    }
    let Some(hooks) = root.get("hooks").and_then(serde_json::Value::as_object) else {
        if root.contains_key("hooks") {
            return Err(InstallError::Config {
                path,
                reason: "`hooks` is not an object".to_owned(),
            });
        }
        return Ok(None);
    };
    let current = super::hook_subset(hooks, family);
    let contains_rift =
        super::fragment_contains_rift_command(&serde_json::Value::Object(hooks.clone()), family);
    if !contains_rift {
        return Ok(None);
    }
    let current_hash = super::fingerprint_json(&current);
    let manifest_owned = manifest
        .hooks
        .get(family.token())
        .is_some_and(|owned| owned.fragment_hash == current_hash);
    if !(manifest_owned || legacy_owned_hook_fragment(repo, family, &current)) {
        return Err(InstallError::Config {
            path,
            reason: format!(
                "{} Rift hooks differ from Rift's ownership fingerprint",
                family.display_name()
            ),
        });
    }
    let bytes = super::remove_standard_hooks(&path, family, Some(&current_hash))?;
    Ok(bytes.map(|bytes| (path, bytes)))
}

fn prepare_antigravity_hook_disable(
    repo: &Path,
    path: PathBuf,
    root: serde_json::Map<String, serde_json::Value>,
    manifest: &OwnershipManifest,
) -> Result<Option<(PathBuf, Vec<u8>)>, InstallError> {
    let family = super::ClientFamily::Antigravity;
    let Some(current) = root.get("rift-recall") else {
        return Ok(None);
    };
    let current_hash = super::fingerprint_json(current);
    let manifest_record = manifest.hooks.get(family.token());
    let manifest_owned = manifest_record.is_some_and(|owned| owned.fragment_hash == current_hash);
    let descriptor_owned = manifest_record.is_none()
        && descriptor_rift_bin(repo, family.token())
            .is_some_and(|rift_bin| current == &super::hook_fragment(family, &rift_bin));
    if !(manifest_owned || descriptor_owned) {
        return Err(InstallError::Config {
            path,
            reason: "reserved `rift-recall` hook bundle differs from Rift's ownership fingerprint"
                .to_owned(),
        });
    }
    let bytes = super::remove_antigravity_hooks(&path, Some(&current_hash))?;
    Ok(bytes.map(|bytes| (path, bytes)))
}

fn recognizable_antigravity_hook_fragment(value: &serde_json::Value) -> bool {
    let family = super::ClientFamily::Antigravity;
    super::rift_hook_binary(value, family)
        .is_some_and(|rift_bin| value == &super::hook_fragment(family, &rift_bin))
}

/// Whether an installed hook fragment is Rift's OWN output, proved by something
/// other than today's ownership manifest — the shape the owner set on the
/// descriptor side, which the hooks side re-owed on 2026-08-24: a byte-recognisable legacy record is
/// RECOGNISED and refreshed, never reported as a conflict.
///
/// Three proofs, in cost order, any one of which is enough.
///
/// **1. The shape.** [`super::recognizable_rift_hook_fragment`] re-emits every
/// group from the candidate's OWN key set, so a fragment written from a
/// superseded verb table still recognizes while a single wrong byte still
/// refuses. The body this replaced compared against TODAY's
/// [`super::hook_fragment`], so the moment a verb table moved — Codex 9 → 10
/// when `SessionEnd` joined — a hook file Rift itself wrote stopped matching
/// every ownership route and `enable`, `disable` and Project Settings all
/// refused the repair they were asked to perform. Calibrated by
/// [`a_moved_verb_table_leaves_the_installed_hook_fragment_refreshable`]; the
/// deliberately unhandled shrink direction is planted by
/// [`a_rift_group_under_a_retired_verb_key_is_still_a_conflict`].
///
/// **2. The legacy installer's manifest**, `.rift/agents/recall-install.json`,
/// which fingerprints the fragment it wrote per client. The legacy installer
/// is the one that wrote the Claude Code fragment until `fce2baea`, and on a
/// machine that ran it the fingerprint is the only
/// record naming this file as Rift's — the native manifest has never held a
/// `hooks` entry for Claude Code.
///
/// **3. The sidecar** it left beside that manifest, `.rift/agents/
/// claude-code.claude-settings.json`: the fragment as written. Nothing has read
/// it since `fce2baea`. It is read here, and never written and never deleted —
/// a dead file that proves a fragment is ours outlives its tidiness, and this
/// crate does not remove artifacts it can still learn from.
fn legacy_owned_hook_fragment(
    repo: &Path,
    family: super::ClientFamily,
    current: &serde_json::Value,
) -> bool {
    super::recognizable_rift_hook_fragment(current, family).is_some()
        || legacy_install_manifest_owns(repo, family, &super::fingerprint_json(current))
        || legacy_sidecar_owns(repo, family, current)
}

/// Proof 2: the legacy multi-client installer's manifest claims this family
/// with exactly the fragment fingerprint now on disk.
fn legacy_install_manifest_owns(repo: &Path, family: super::ClientFamily, hash: &str) -> bool {
    let Ok(bytes) = std::fs::read(repo.join(".rift/agents/recall-install.json")) else {
        return false;
    };
    let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    manifest
        .get("clients")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|clients| {
            clients.iter().any(|client| {
                client.get("client").and_then(serde_json::Value::as_str) == Some(family.token())
                    && client
                        .get("hook_fingerprint")
                        .and_then(serde_json::Value::as_str)
                        == Some(hash)
            })
        })
}

/// Proof 3: the dead sidecar holds the same Rift groups the file now holds.
///
/// Claude Code only: it is the one family the legacy installer wrote a sidecar
/// copy for, and inventing the path for the others would be a second table of
/// filenames nothing ever wrote.
fn legacy_sidecar_owns(
    repo: &Path,
    family: super::ClientFamily,
    current: &serde_json::Value,
) -> bool {
    if family != super::ClientFamily::ClaudeCode {
        return false;
    }
    let path = repo.join(".rift/agents/claude-code.claude-settings.json");
    let Ok(bytes) = std::fs::read(path) else {
        return false;
    };
    let Ok(sidecar) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return false;
    };
    sidecar
        .get("hooks")
        .and_then(serde_json::Value::as_object)
        .is_some_and(|hooks| &super::hook_subset(hooks, family) == current)
}

/// Whether Codex has recorded trust for a project's hook file.
///
/// Codex trusts hooks **per absolute path and hash**, in
/// `~/.codex/config.toml`'s `[hooks.state."<abs>/.codex/hooks.json:<event>:i:j"]`
/// tables (verified 2026-08-30: nine entries for this repository's file). A
/// linked worktree's `.codex/hooks.json` is a NEW path, so it is untrusted
/// until the user approves it inside Codex — which is a fact adoption has to
/// report, because a fragment Rift wrote correctly still will not run.
///
/// Rift never writes `~/.codex/config.toml` or any other global client
/// configuration, so this is a read and only a read: no `codex_home`
/// means no read at all.
///
/// "Approved" means Codex has recorded trust for this path at least once. It
/// deliberately does not compare `trusted_hash`: what that hash covers is
/// Codex's business, and inventing an answer would be worse than the honest
/// one, which is why the pending wording tells the user where to go rather
/// than promising the hooks will fire.
#[must_use]
pub fn codex_hook_trust(hooks_json: &Path, codex_home: Option<&Path>) -> CodexTrust {
    let Some(home) = codex_home else {
        return CodexTrust::Pending;
    };
    let Ok(text) = std::fs::read_to_string(home.join("config.toml")) else {
        return CodexTrust::Pending;
    };
    let Ok(doc) = text.parse::<toml_edit::DocumentMut>() else {
        return CodexTrust::Pending;
    };
    let Some(state) = doc
        .get("hooks")
        .and_then(toml_edit::Item::as_table)
        .and_then(|hooks| hooks.get("state"))
        .and_then(toml_edit::Item::as_table)
    else {
        return CodexTrust::Pending;
    };
    // Both spellings of the member's path: Codex recorded whatever it resolved,
    // and on this machine `/var` and `/private/var` are the same directory.
    let mut prefixes = vec![format!("{}:", hooks_json.display())];
    if let Ok(canonical) = std::fs::canonicalize(hooks_json) {
        prefixes.push(format!("{}:", canonical.display()));
    }
    if state.iter().any(|(key, _)| {
        prefixes
            .iter()
            .any(|prefix| key.starts_with(prefix.as_str()))
    }) {
        CodexTrust::Approved
    } else {
        CodexTrust::Pending
    }
}

/// The absolute `rift` binary an agent's INSTALLED hook fragment invokes, read
/// without writing anything.
///
/// The question adoption asks about a file it may not repair: `.agents/hooks.json`
/// is tracked by the repository, so a member whose bundle names a binary that
/// is no longer the family's record is reported `stale` and left exactly as it
/// is. `None` means no file, no Rift command in it, or a command
/// this emitter did not write.
#[must_use]
pub fn installed_hook_binary(repo: &Path, agent_token: &str) -> Option<PathBuf> {
    let family = super::ClientFamily::from_token(agent_token)?;
    let root = super::read_json_object(&repo.join(family.hook_path())).ok()?;
    let fragment = if family == super::ClientFamily::Antigravity {
        root.get("rift-recall")?
    } else {
        root.get("hooks")?
    };
    super::rift_hook_binary(fragment, family)
}

/// Whether this checkout carries the Recall skill region for `agent_token`.
/// `None` when the adapter has no skill file at all.
///
/// Read-only, and that is the whole point: the skill files are `AGENTS.md`,
/// `.agents/rules/rift-recall.md` and their siblings — files the repository
/// TRACKS. Adoption reports a missing region and leaves the explicit
/// `rift enable <agent>` path to write it, because a git hook that dirties the
/// tree the user just created is not a repair.
#[must_use]
pub fn recall_skill_present(repo: &Path, agent_token: &str) -> Option<bool> {
    let relative = skill_path(agent_token)?;
    Some(
        std::fs::read_to_string(repo.join(relative))
            .is_ok_and(|text| skill_fence(&text, agent_token).is_some()),
    )
}

fn descriptor_rift_bin(repo: &Path, token: &str) -> Option<PathBuf> {
    let value: serde_json::Value =
        serde_json::from_slice(&std::fs::read(descriptor_path(repo, token)).ok()?).ok()?;
    let path = PathBuf::from(value.get("rift_bin")?.as_str()?);
    crate::absolute_on_any_platform(&path).then_some(path)
}

fn refuse_symlink(path: &Path, label: &str) -> Result<(), InstallError> {
    if path_is_symlink(path) {
        return Err(InstallError::Config {
            path: path.to_path_buf(),
            reason: format!("refusing symlinked {label}"),
        });
    }
    Ok(())
}

fn path_is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

fn descriptor_bytes(
    rift_bin: &Path,
    token: &str,
    verbs: &[&str],
    notes: &rift_shadow_hooks::CompatNotes,
) -> Vec<u8> {
    let hooks: Vec<serde_json::Value> = verbs
        .iter()
        .map(|verb| {
            serde_json::json!({
                "verb": verb,
                "command": format!("{} hooks {token} {verb}", rift_bin.display()),
            })
        })
        .collect();
    let value = serde_json::json!({
        "schema_version": 1,
        "managed_by": "rift",
        "agent": token,
        "rift_bin": rift_bin.display().to_string(),
        "hooks": hooks,
        "compat": {
            "best_effort": notes.best_effort,
            "surfaces_token_usage": notes.surfaces_token_usage,
            "lifecycle_gaps": notes.lifecycle_gaps,
        },
    });
    let mut bytes = serde_json::to_vec_pretty(&value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

fn owned_descriptor(bytes: &[u8], token: &str) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .is_some_and(|value| {
            value.get("managed_by").and_then(serde_json::Value::as_str) == Some("rift")
                && value.get("agent").and_then(serde_json::Value::as_str) == Some(token)
        })
}

/// Recognize a descriptor written by `rift enable --agent` whose ownership the
/// manifest cannot vouch for.
///
/// **Three arms. The second is the migration the verb-table correction re-owed;
/// the third is the same migration one schema later.**
///
/// Arm 1 is the original: the whole parsed document equals the value derived
/// from TODAY's `verbs()` + `compat_notes()`. That arm is exact and cheap, and
/// it is also the reason a corrected verb table unrecognizes a pre-manifest
/// install — the document was derived from the table of its day, so a moved
/// table stops it matching and `inspect_wiring` reports
/// [`NativeWiring::Conflict`] on a file Rift itself wrote. That breakage is
/// PLANTED, not argued, by
/// [`a_moved_verb_table_conflicts_the_pre_manifest_install_without_arm_two`].
///
/// **MEASURED ON THIS MACHINE BEFORE THE ARM WAS WRITTEN**, because the
/// original sizing of this population and the tree disagreed:
/// the main checkout's `.rift/agents/codex.json` carries `managed_by`
/// and is listed in the manifest, so it takes the manifest path and is merely
/// rewritten; `.rift/agents/claude-code.json` has NO `managed_by` and is absent
/// from the manifest, so it is a genuine pre-manifest descriptor and arm 1 is
/// the only thing recognizing it. Its table did not move then, so
/// nothing on this machine broke — but the population is real, and the next
/// table correction breaks it.
///
/// Arm 2 therefore recognizes the SHAPE rather than the CONTENT: a document
/// whose key set, hook-command spelling and self-consistency are unmistakably
/// Rift's own pre-manifest output, whatever verb table it was written from.
/// This is version-proof where a list of superseded tables would grow forever
/// and be wrong the first time someone forgot to append to it.
///
/// What it does NOT relax: `agent` must be this token, `rift_bin` must be
/// absolute, every `hooks[i].command` must be exactly
/// `"<rift_bin> hooks <token> <verbs[i]>"` in order, and `compat` must carry
/// exactly the three keys Rift writes. A merely similar user file still fails
/// and is never adopted — and the only consequence of a true positive is that
/// `enable` rewrites the file to the current shape and takes ownership in the
/// manifest, which is what running `rift enable` asks for.
fn legacy_owned_descriptor(bytes: &[u8], token: &str, adapter: &dyn AgentAdapter) -> bool {
    let Ok(actual) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return false;
    };
    let Some(rift_bin) = actual.get("rift_bin").and_then(serde_json::Value::as_str) else {
        return false;
    };
    if !crate::absolute_on_any_platform(Path::new(rift_bin)) {
        return false;
    }
    actual == legacy_descriptor_value(Path::new(rift_bin), token, adapter)
        || pre_manifest_descriptor_shape(&actual, token, rift_bin)
        || current_descriptor_shape(&actual, token, rift_bin)
}

/// Whether `actual` is structurally one of Rift's own pre-manifest descriptors
/// for `token`, derived from ANY verb table. See [`legacy_owned_descriptor`].
fn pre_manifest_descriptor_shape(actual: &serde_json::Value, token: &str, rift_bin: &str) -> bool {
    let Some(root) = actual.as_object() else {
        return false;
    };
    // The exact key set Rift wrote before `managed_by`/`schema_version` existed.
    // A superset is a user file with extra fields; a subset is not ours.
    let keys: std::collections::BTreeSet<&str> = root.keys().map(String::as_str).collect();
    if keys
        != ["agent", "compat", "hooks", "rift_bin", "verbs"]
            .into_iter()
            .collect()
    {
        return false;
    }
    if root.get("agent").and_then(serde_json::Value::as_str) != Some(token) {
        return false;
    }
    let compat_keys: Option<std::collections::BTreeSet<&str>> = root
        .get("compat")
        .and_then(serde_json::Value::as_object)
        .map(|c| c.keys().map(String::as_str).collect());
    if compat_keys
        != Some(
            ["best_effort", "lifecycle_gaps", "surfaces_token_usage"]
                .into_iter()
                .collect(),
        )
    {
        return false;
    }
    let Some(verbs) = root.get("verbs").and_then(serde_json::Value::as_array) else {
        return false;
    };
    let Some(hooks) = root.get("hooks").and_then(serde_json::Value::as_array) else {
        return false;
    };
    if verbs.is_empty() || verbs.len() != hooks.len() {
        return false;
    }
    verbs.iter().zip(hooks).all(|(verb, hook)| {
        let Some(verb) = verb.as_str() else {
            return false;
        };
        rift_shaped_descriptor_hook(hook, token, rift_bin, verb)
    })
}

/// Whether `actual` is structurally one of Rift's own CURRENT-schema
/// descriptors for `token`, derived from ANY verb table — the third arm of
/// [`legacy_owned_descriptor`], for a descriptor whose manifest entry is gone.
///
/// The population is real and one `git clone` away: descriptors have been
/// committed to repositories while `.rift/state/` (the ownership manifest) is
/// gitignored, so a fresh checkout — or a state wipe — holds a file Rift wrote
/// in today's schema that the manifest cannot vouch for. Before this arm that
/// file matched nothing (arm 1 derives the pre-manifest key set, arm 2 demands
/// it) and read as `Conflict`, with both `enable` and `disable` refusing —
/// the descriptor-level twin of the hooks-side stranding the same correction
/// named.
///
/// Same trust model as arm 2: the exact key set Rift writes today
/// (`managed_by` + `schema_version`, no `verbs`), `agent` equal to this token,
/// `schema_version` 1, `compat` carrying exactly its three keys, and every
/// hook exactly `{verb, command}` with the UNQUOTED command spelling of
/// [`descriptor_bytes`] — deliberately distinct from the quoted hook-file
/// spelling. Verb-set free, so a table move cannot unrecognize it; a merely
/// similar user file fails on the first wrong byte and is never adopted.
fn current_descriptor_shape(actual: &serde_json::Value, token: &str, rift_bin: &str) -> bool {
    let Some(root) = actual.as_object() else {
        return false;
    };
    let keys: std::collections::BTreeSet<&str> = root.keys().map(String::as_str).collect();
    if keys
        != [
            "agent",
            "compat",
            "hooks",
            "managed_by",
            "rift_bin",
            "schema_version",
        ]
        .into_iter()
        .collect()
    {
        return false;
    }
    if root.get("managed_by").and_then(serde_json::Value::as_str) != Some("rift")
        || root.get("agent").and_then(serde_json::Value::as_str) != Some(token)
        || root
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
    {
        return false;
    }
    let compat_keys: Option<std::collections::BTreeSet<&str>> = root
        .get("compat")
        .and_then(serde_json::Value::as_object)
        .map(|c| c.keys().map(String::as_str).collect());
    if compat_keys
        != Some(
            ["best_effort", "lifecycle_gaps", "surfaces_token_usage"]
                .into_iter()
                .collect(),
        )
    {
        return false;
    }
    let Some(hooks) = root.get("hooks").and_then(serde_json::Value::as_array) else {
        return false;
    };
    if hooks.is_empty() {
        return false;
    }
    hooks.iter().all(|hook| {
        let Some(verb) = hook.get("verb").and_then(serde_json::Value::as_str) else {
            return false;
        };
        rift_shaped_descriptor_hook(hook, token, rift_bin, verb)
    })
}

/// One descriptor hook entry in Rift's own spelling: exactly `{verb, command}`
/// with `command == "<rift_bin> hooks <token> <verb>"` (unquoted binary).
fn rift_shaped_descriptor_hook(
    hook: &serde_json::Value,
    token: &str,
    rift_bin: &str,
    verb: &str,
) -> bool {
    let Some(hook) = hook.as_object() else {
        return false;
    };
    let named = hook.get("verb").and_then(serde_json::Value::as_str) == Some(verb);
    let commanded = hook.get("command").and_then(serde_json::Value::as_str)
        == Some(format!("{rift_bin} hooks {token} {verb}").as_str());
    named && commanded && hook.len() == 2
}

fn legacy_descriptor_value(
    rift_bin: &Path,
    token: &str,
    adapter: &dyn AgentAdapter,
) -> serde_json::Value {
    let verbs = adapter.verbs();
    let notes = adapter.compat_notes();
    let hooks = verbs
        .iter()
        .map(|verb| {
            serde_json::json!({
                "verb": verb,
                "command": format!("{} hooks {token} {verb}", rift_bin.display()),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "agent": token,
        "rift_bin": rift_bin.display().to_string(),
        "verbs": verbs,
        "hooks": hooks,
        "compat": {
            "best_effort": notes.best_effort,
            "surfaces_token_usage": notes.surfaces_token_usage,
            "lifecycle_gaps": notes.lifecycle_gaps,
        },
    })
}

fn descriptor_path(repo: &Path, token: &str) -> PathBuf {
    repo.join(".rift/agents").join(format!("{token}.json"))
}

fn manifest_path(repo: &Path) -> PathBuf {
    repo.join(".rift/state/native-integrations.json")
}

fn read_manifest(repo: &Path) -> OwnershipManifest {
    std::fs::read(manifest_path(repo))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .filter(|manifest: &OwnershipManifest| manifest.schema_version == manifest_version())
        .unwrap_or_default()
}

fn write_manifest(repo: &Path, manifest: &OwnershipManifest) -> Result<bool, InstallError> {
    let path = manifest_path(repo);
    let mut bytes = serde_json::to_vec_pretty(manifest).unwrap_or_default();
    bytes.push(b'\n');
    if std::fs::read(&path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(false);
    }
    super::atomic_write(&path, &bytes)?;
    Ok(true)
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>, InstallError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(InstallError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn fingerprint(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn display_name(token: &str) -> &'static str {
    match token {
        "claude-code" => "Claude Code",
        "codex" => "Codex",
        "gemini" => "Gemini CLI",
        "antigravity" => "Antigravity",
        "cursor" => "Cursor",
        "copilot-cli" => "Copilot CLI",
        "opencode" => "OpenCode",
        "factory-droid" => "Factory Droid",
        "ollama" => "Ollama",
        _ => "Agent",
    }
}

/// The instructions file Rift writes its Recall region into, per adapter.
///
/// The table names exactly the tokens [`install_recall_skill`] will write for —
/// the agents this alpha captures. A path defined for a token the writer
/// refuses is a channel that cannot be reached: it reads as support and is
/// not.
fn skill_path(token: &str) -> Option<&'static str> {
    match token {
        "claude-code" => Some(".claude/skills/rift-recall/SKILL.md"),
        "codex" => Some("AGENTS.md"),
        "antigravity" => Some(".agents/rules/rift-recall.md"),
        _ => None,
    }
}

/// Whether the file at [`skill_path`] is one the repository already uses for
/// its own instructions, so Rift writes inside a fence and never owns the
/// whole document.
fn skill_is_shared(token: &str) -> bool {
    token == "codex"
}

/// The YAML frontmatter a family's channel requires ahead of Rift's fence.
///
/// Claude Code discovers a skill by this block, so it has to lead the file —
/// which is why the fence wraps the body and not the whole document. Written
/// once, when Rift creates the file: a re-install rewrites only what is
/// between the markers, so a user who edits the description keeps it, and
/// `remove_recall_skill` deletes the whole file Rift created, frontmatter
/// included.
///
/// This is the one live fact the retired `skills.rs` held and this path never
/// did; it is ported here rather than deleted with it.
fn skill_frontmatter(token: &str) -> Option<&'static str> {
    match token {
        "claude-code" => Some(
            "---\n\
             name: rift-recall\n\
             description: Ask Rift what changed since the state you already hold, \
             instead of re-reading files. Use at the start of a turn, before reading \
             source you may already have.\n\
             ---\n\n",
        ),
        _ => None,
    }
}

fn skill_region(token: &str, body: &str) -> String {
    format!(
        "<!-- RIFT-MANAGED:{token}:begin -->\n{}\n<!-- RIFT-MANAGED:{token}:end -->\n",
        body.trim_end()
    )
}

fn skill_fence(text: &str, token: &str) -> Option<(usize, usize)> {
    let begin = format!("<!-- RIFT-MANAGED:{token}:begin -->");
    let close = format!("<!-- RIFT-MANAGED:{token}:end -->");
    let start = text.find(&begin)?;
    let close_start = start + text.get(start..)?.find(&close)?;
    let mut end = close_start + close.len();
    if text.get(end..).is_some_and(|rest| rest.starts_with('\n')) {
        end += 1;
    }
    Some((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_hook_path(repo: &Path) -> PathBuf {
        client_hook_path(repo, super::super::ClientFamily::Codex)
    }

    fn claude_hook_path(repo: &Path) -> PathBuf {
        client_hook_path(repo, super::super::ClientFamily::ClaudeCode)
    }

    fn antigravity_hook_path(repo: &Path) -> PathBuf {
        client_hook_path(repo, super::super::ClientFamily::Antigravity)
    }

    #[test]
    fn all_nine_registry_adapters_have_stable_native_rows() {
        let dir = tempfile::tempdir().unwrap();
        let rows = diagnose_all(dir.path());
        assert_eq!(rows.len(), 9);
        assert_eq!(
            rows.iter()
                .map(|row| row.agent.as_str())
                .collect::<Vec<_>>(),
            vec![
                "claude-code",
                "codex",
                "gemini",
                "antigravity",
                "cursor",
                "copilot-cli",
                "opencode",
                "factory-droid",
                "ollama",
            ]
        );
        assert!(rows.iter().all(|row| row.wiring == NativeWiring::Missing));
    }

    /// **The alpha-set refusal, PLANTED rather than trusted: a refused agent
    /// must leave the filesystem exactly as it found it.**
    ///
    /// The failure this guards is not "the refusal is missing" — that would be
    /// obvious. It is the refusal arriving *after* something has already been
    /// written, so `rift enable --agent cursor` reports an error and still
    /// leaves a descriptor, a manifest entry, or a `.codex/hooks.json` group
    /// behind for the next `doctor` run to read as a real install. So the
    /// assertions are about the tree, not about the `Err`: no descriptor
    /// appears, no manifest appears, and a file already sitting at the
    /// descriptor path comes back BYTE-IDENTICAL.
    #[test]
    fn a_not_yet_supported_agent_is_refused_and_writes_nothing_at_all() {
        for token in [
            "cursor",
            "copilot-cli",
            "opencode",
            "factory-droid",
            "ollama",
        ] {
            assert!(
                !is_alpha_supported(token),
                "{token} must not be in the alpha"
            );
            let dir = tempfile::tempdir().unwrap();
            let refused = enable(dir.path(), Path::new("/opt/rift"), token).unwrap_err();
            match &refused {
                InstallError::Config { reason, .. } => assert!(
                    reason.contains("not supported yet"),
                    "{token}: the refusal must say why: {reason}"
                ),
                other => panic!("{token}: expected a Config refusal, got {other:?}"),
            }
            assert!(
                !descriptor_path(dir.path(), token).exists(),
                "{token}: a refused enable wrote a descriptor"
            );
            assert!(
                !manifest_path(dir.path()).exists(),
                "{token}: a refused enable wrote an ownership manifest"
            );
            assert!(
                !dir.path().join(".rift/agents").exists(),
                "{token}: a refused enable created the descriptor directory"
            );

            // A file already at that path is somebody else's and stays exactly
            // as it was — a refusal must not truncate, rewrite or delete it.
            let path = descriptor_path(dir.path(), token);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mine = b"{\"managed_by\":\"not-rift\"}\n";
            std::fs::write(&path, mine).unwrap();
            assert!(enable(dir.path(), Path::new("/opt/rift"), token).is_err());
            assert_eq!(
                std::fs::read(&path).unwrap(),
                mine,
                "{token}: a refused enable disturbed a file it does not own"
            );
        }
    }

    /// The other half of the same rule: the agents the alpha DOES support are
    /// untouched by the refusal, so it cannot be a blanket stop.
    ///
    /// Three since Gemini CLI was demoted on 2026-08-24. The count
    /// is asserted rather than derived so a silent widening of `ALPHA_AGENTS`
    /// cannot pass as "the loop still ran".
    #[test]
    fn every_alpha_agent_still_enables() {
        for token in ALPHA_AGENTS {
            let dir = tempfile::tempdir().unwrap();
            let report = enable(dir.path(), Path::new("/opt/rift"), token)
                .unwrap_or_else(|e| panic!("{token}: {e}"));
            assert!(
                descriptor_path(dir.path(), token).is_file(),
                "{token}: enabled without a descriptor"
            );
            assert!(!report.changed_files.is_empty(), "{token}: changed nothing");
        }
        assert_eq!(ALPHA_AGENTS.len(), 3);
        let registry = AdapterRegistry::with_builtin();
        assert_eq!(
            registry.agents().len(),
            9,
            "all nine adapters stay REGISTERED; only enable() discriminates"
        );
    }

    /// **Every offender at once, never the first.**
    ///
    /// The defect this pins is not "unsupported agents are allowed through" —
    /// [`enable`] already refuses those. It is a caller that learns about them
    /// one per run: `rift init` returned on the first non-OK outcome inside
    /// its wiring loop, so a machine with three coming-soon agents installed
    /// took three runs to enumerate them, each one erroring after the git
    /// hooks were already on disk.
    ///
    /// The selection below is deliberately interleaved and deliberately holds
    /// a repeat, so an implementation that stops at the first miss, or that
    /// reports a duplicate twice, cannot pass by accident.
    #[test]
    fn an_unsupported_selection_reports_every_offender_in_order_and_once_each() {
        assert_eq!(
            unsupported_alpha_agents([
                "cursor",
                "claude-code",
                "ollama",
                "codex",
                "cursor",
                "factory-droid",
            ]),
            vec![
                "cursor".to_owned(),
                "ollama".to_owned(),
                "factory-droid".to_owned()
            ],
            "all three, in the order given, and `cursor` only once"
        );
    }

    /// The other arm: a selection this alpha can wire yields nothing to refuse,
    /// so the up-front check cannot become a blanket stop on `rift init`.
    #[test]
    fn a_fully_supported_selection_has_nothing_to_refuse() {
        assert!(unsupported_alpha_agents(ALPHA_AGENTS.iter().copied()).is_empty());
        assert!(unsupported_alpha_agents(std::iter::empty()).is_empty());
        // An unknown token is *not* this function's business — it is refused
        // elsewhere, with a message that says it does not exist rather than
        // that it is coming soon.
        assert_eq!(
            unsupported_alpha_agents(["no-such-agent"]),
            vec!["no-such-agent".to_owned()],
            "unknown tokens fall to the caller's own registry check first"
        );
    }

    /// One agent's verb table frozen at a point in time, which is exactly what
    /// an installed descriptor is: a document derived from `verbs()` and
    /// `compat_notes()` on the day it was written.
    struct FrozenCursor {
        verbs: &'static [&'static str],
    }

    /// Cursor's declared table today — the one every existing install's
    /// descriptor was derived from.
    const CURSOR_AS_INSTALLED: &[&str] = &["session_start", "edit", "session_end"];

    /// Cursor's real hook surface, cited to the pinned tag by
    /// `rift-shadow-hooks/tests/adapter_sweep.rs` (`cursor.verb_surface`). This
    /// is what the table becomes when the sweep's Gap row is closed.
    const CURSOR_AS_PUBLISHED: &[&str] = &[
        "sessionStart",
        "sessionEnd",
        "beforeSubmitPrompt",
        "stop",
        "preCompact",
        "subagentStart",
        "subagentStop",
    ];

    impl AgentAdapter for FrozenCursor {
        fn agent_id(&self) -> AgentId {
            AgentId::new("cursor")
        }

        fn map(
            &self,
            verb: &str,
            _native: &serde_json::Value,
        ) -> Result<Vec<rift_shadow_hooks::CanonicalEvent>, rift_shadow_hooks::AdapterError>
        {
            Err(rift_shadow_hooks::AdapterError::UnknownVerb(
                verb.to_owned(),
            ))
        }

        fn verbs(&self) -> &'static [&'static str] {
            self.verbs
        }

        fn compat_notes(&self) -> rift_shadow_hooks::CompatNotes {
            rift_shadow_hooks::CompatNotes {
                agent: AgentId::new("cursor"),
                lifecycle_gaps: &[],
                surfaces_token_usage: false,
                best_effort: true,
                note: "frozen table, test double",
            }
        }
    }

    /// **MEASURED, NOT REASONED: correcting a verb table breaks the
    /// PRE-MANIFEST population and spares the manifest-owned one.**
    ///
    /// It was first sized as "every pre-existing install"; it is smaller
    /// than that, and the difference is the whole shape of the migration owed.
    /// Two recognizers run over a descriptor, and only one of them reads the
    /// verb table:
    ///
    /// * `legacy_owned_descriptor` compares the WHOLE document against a value
    ///   derived from `verbs()` + `compat_notes()`, so a moved table stops it
    ///   matching — `NativeWiring::Conflict` in doctor and Project Settings,
    ///   and `InstallError::Config` out of `enable`;
    /// * `owned_descriptor` + the manifest checks `managed_by`/`agent` and
    ///   compares the manifest hash against the hash of the bytes ALREADY ON
    ///   DISK — never against a freshly derived value — so a moved table cannot
    ///   unmatch it. It is simply rewritten.
    ///
    /// Both halves are driven here rather than argued, and the `enable` half
    /// goes through the shipped entry point with the real registry.
    #[test]
    fn a_moved_verb_table_conflicts_the_pre_manifest_install_without_arm_two() {
        let dir = tempfile::tempdir().unwrap();
        let token = "cursor";
        let rift_bin = Path::new("/opt/rift");
        let as_installed = FrozenCursor {
            verbs: CURSOR_AS_INSTALLED,
        };
        let as_published = FrozenCursor {
            verbs: CURSOR_AS_PUBLISHED,
        };
        let path = descriptor_path(dir.path(), token);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // (1) THE PRE-MANIFEST POPULATION. The descriptor an older `rift enable`
        // left behind, derived from the table of its day.
        let legacy =
            serde_json::to_vec_pretty(&legacy_descriptor_value(rift_bin, token, &as_installed))
                .unwrap();
        std::fs::write(&path, &legacy).unwrap();
        assert_eq!(
            inspect_wiring(dir.path(), token, &as_installed),
            NativeWiring::Managed,
            "the adapter that wrote it must still recognize it"
        );
        // THE BREAKAGE, still planted rather than argued: arm 1 alone does not
        // recognize it. Asserted on the predicate `inspect_wiring` reads, so the
        // plant survives the migration that closes it — deleting this line would
        // leave the migration with nothing to be a migration FROM.
        assert!(
            !(serde_json::to_vec_pretty(&legacy_descriptor_value(rift_bin, token, &as_installed))
                .map(|bytes| {
                    let actual: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    actual == legacy_descriptor_value(rift_bin, token, &as_published)
                })
                .unwrap()),
            "THE BREAKAGE: a corrected verb table unrecognizes a pre-manifest \
             install through arm 1 (whole-document equality)"
        );

        // …AND ARM 2 CLOSES IT. The same file, the same moved table, now
        // recognized by shape: `enable` migrates it instead of refusing, and
        // the migrated document carries the manifest marker.
        assert_eq!(
            inspect_wiring(dir.path(), token, &as_published),
            NativeWiring::Managed,
            "the migration must adopt a descriptor Rift itself wrote from an \
             older table"
        );

        // The negative arm, which is what stops the migration being a hole: a
        // merely similar user file is STILL refused. One field is wrong — the
        // hook command names a different agent than the descriptor does.
        let mut forged = legacy_descriptor_value(rift_bin, token, &as_installed);
        forged["hooks"][0]["command"] = serde_json::json!(format!(
            "{} hooks not-{token} session_start",
            rift_bin.display()
        ));
        std::fs::write(&path, serde_json::to_vec_pretty(&forged).unwrap()).unwrap();
        assert_eq!(
            inspect_wiring(dir.path(), token, &as_published),
            NativeWiring::Conflict,
            "a file that merely resembles a Rift descriptor is never adopted"
        );
        let refused = enable(dir.path(), rift_bin, token).unwrap_err();
        assert!(
            matches!(refused, InstallError::Config { .. }),
            "expected a Config refusal, got {refused:?}"
        );

        // (2) THE MANIFEST-OWNED POPULATION survives the identical change.
        let dir = tempfile::tempdir().unwrap();
        let path = descriptor_path(dir.path(), token);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let current = descriptor_bytes(
            rift_bin,
            token,
            as_installed.verbs(),
            &as_installed.compat_notes(),
        );
        std::fs::write(&path, &current).unwrap();
        let mut manifest = OwnershipManifest::default();
        manifest
            .descriptors
            .insert(token.to_owned(), fingerprint(&current));
        write_manifest(dir.path(), &manifest).unwrap();
        assert_eq!(
            inspect_wiring(dir.path(), token, &as_published),
            NativeWiring::Managed,
            "a manifest-owned install must survive the correction, not conflict"
        );
    }

    #[test]
    fn the_exact_pre_manifest_descriptor_is_managed_and_migrates_safely() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AdapterRegistry::with_builtin();
        let agent = AgentId::new("claude-code");
        let adapter = registry.get(&agent).unwrap();
        let path = descriptor_path(dir.path(), agent.as_str());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&legacy_descriptor_value(
                Path::new("/opt/rift"),
                agent.as_str(),
                adapter,
            ))
            .unwrap(),
        )
        .unwrap();

        let before = diagnose_all(dir.path());
        let claude = before
            .iter()
            .find(|row| row.agent == agent.as_str())
            .unwrap();
        assert_eq!(claude.wiring, NativeWiring::Managed);
        // Made visible when Claude Code joined the native table: the descriptor alone is not the
        // integration. This row used to read `enabled()` with no Rift hook
        // registered anywhere — the exact state this repository's own checkout
        // was in, and the reason a Claude Code session captured nothing after a
        // successful-looking `rift enable`.
        assert_eq!(claude.hook_wiring, Some(NativeHookWiring::Missing));
        assert!(
            !claude.enabled(),
            "a descriptor with no hook fragment is not an enabled integration"
        );

        let report = enable(dir.path(), Path::new("/opt/rift"), agent.as_str()).unwrap();
        assert_eq!(
            report
                .changed_files
                .iter()
                .map(|file| file.strip_prefix(dir.path()).unwrap().to_owned())
                .collect::<std::collections::BTreeSet<_>>(),
            [
                ".claude/settings.local.json",
                ".rift/agents/claude-code.json",
                ".rift/state/native-integrations.json",
            ]
            .into_iter()
            .map(PathBuf::from)
            .collect::<std::collections::BTreeSet<_>>(),
            "the migration writes the descriptor, the fragment and the manifest"
        );
        let migrated: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(migrated["managed_by"], serde_json::json!("rift"));
        assert!(manifest_path(dir.path()).is_file());
        assert!(
            diagnose_all(dir.path())
                .iter()
                .find(|row| row.agent == agent.as_str())
                .unwrap()
                .enabled(),
            "descriptor plus fragment is what enabled means"
        );
        assert!(enable(dir.path(), Path::new("/opt/rift"), agent.as_str())
            .unwrap()
            .changed_files
            .is_empty());
    }

    #[test]
    fn a_modified_pre_manifest_descriptor_remains_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AdapterRegistry::with_builtin();
        let agent = AgentId::new("claude-code");
        let adapter = registry.get(&agent).unwrap();
        let path = descriptor_path(dir.path(), agent.as_str());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut value = legacy_descriptor_value(Path::new("/opt/rift"), agent.as_str(), adapter);
        value["hooks"][0]["command"] = serde_json::json!("user-owned command");
        let bytes = serde_json::to_vec_pretty(&value).unwrap();
        std::fs::write(&path, &bytes).unwrap();

        let before = diagnose_all(dir.path());
        assert_eq!(
            before
                .iter()
                .find(|row| row.agent == agent.as_str())
                .unwrap()
                .wiring,
            NativeWiring::Conflict
        );
        assert!(enable(dir.path(), Path::new("/opt/rift"), agent.as_str()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    #[test]
    fn the_exact_pre_manifest_descriptor_can_be_removed_without_a_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let registry = AdapterRegistry::with_builtin();
        let agent = AgentId::new("codex");
        let adapter = registry.get(&agent).unwrap();
        let path = descriptor_path(dir.path(), agent.as_str());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(&legacy_descriptor_value(
                Path::new("/opt/rift"),
                agent.as_str(),
                adapter,
            ))
            .unwrap(),
        )
        .unwrap();

        let report = disable(dir.path(), agent.as_str()).unwrap();
        assert_eq!(
            report.changed_files,
            vec![path.clone(), manifest_path(dir.path())]
        );
        assert!(!path.exists());
    }

    #[test]
    fn enable_is_idempotent_and_disable_requires_the_fingerprint() {
        let dir = tempfile::tempdir().unwrap();
        let bin = Path::new("/opt/rift");
        let first = enable(dir.path(), bin, "codex").unwrap();
        assert_eq!(first.changed_files.len(), 3);
        assert!(enable(dir.path(), bin, "codex")
            .unwrap()
            .changed_files
            .is_empty());
        assert!(diagnose_all(dir.path())
            .iter()
            .find(|row| row.agent == "codex")
            .unwrap()
            .enabled());

        let path = descriptor_path(dir.path(), "codex");
        std::fs::write(
            &path,
            "{\"managed_by\":\"rift\",\"agent\":\"codex\",\"edited\":true}",
        )
        .unwrap();
        let error = disable(dir.path(), "codex").unwrap_err();
        assert!(error.to_string().contains("ownership fingerprint"));
        assert!(path.exists(), "a conflicting managed file must survive");
    }

    #[test]
    fn detects_the_verified_codex_extension_without_claiming_a_path_cli() {
        let repo = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let old = home
            .path()
            .join(".vscode/extensions/openai.chatgpt-26.700.1-darwin-x64");
        let current = home
            .path()
            .join(".vscode/extensions/openai.chatgpt-26.803.61601-darwin-x64");
        let impostor = home
            .path()
            .join(".vscode/extensions/openai.chatgpt-99.0.0-darwin-x64");
        for (path, publisher, version) in [
            (&old, "openai", "26.700.1"),
            (&current, "openai", "26.803.61601"),
            (&impostor, "someone-else", "99.0.0"),
        ] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::write(
                path.join("package.json"),
                serde_json::to_vec(&serde_json::json!({
                    "name": "chatgpt",
                    "publisher": publisher,
                    "version": version,
                }))
                .unwrap(),
            )
            .unwrap();
        }

        let rows = diagnose_all_with_environment(
            repo.path(),
            &DetectionEnvironment {
                home: Some(home.path().to_path_buf()),
                ..DetectionEnvironment::default()
            },
        );
        let codex = rows.iter().find(|row| row.agent == "codex").unwrap();
        // One rule: an extension-only install IS detected — the gap where
        // Canopy pre-ticked this row and `rift init` skipped it is closed.
        assert_eq!(codex.detection, NativeDetection::Detected);
        assert!(codex.detected());
        assert!(
            codex.executable.is_none(),
            "no bundled binary was planted, so nothing claims a runnable CLI"
        );
        assert_eq!(codex.installations.len(), 1);
        assert_eq!(
            codex.installations[0].source,
            NativeInstallSource::IdeExtension(NativeIdeHost::VsCode)
        );
        assert_eq!(
            codex.installations[0].version.as_deref(),
            Some("26.803.61601")
        );
        assert_eq!(
            codex.availability(),
            "present via VS Code extension 26.803.61601"
        );
        let claude = rows.iter().find(|row| row.agent == "claude-code").unwrap();
        assert!(!claude.detected());
        assert_eq!(claude.availability(), "absent");
    }

    #[test]
    fn codex_hooks_preserve_user_entries_and_disable_only_owned_groups() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let user_group = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{"type": "command", "command": "user-start"}]
                }],
                "PreToolUse": [{
                    "matcher": "*",
                    "hooks": [{"type": "command", "command": "user-pre-tool"}]
                }]
            }
        });
        std::fs::write(&path, super::super::pretty_json(&user_group)).unwrap();

        enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(installed["hooks"]["SessionStart"]
            .as_array()
            .is_some_and(|groups| groups.len() == 2));
        let pre_tool = installed["hooks"]["PreToolUse"]
            .as_array()
            .expect("the tool event remains an array");
        assert_eq!(pre_tool.len(), 2);
        assert!(pre_tool[0].to_string().contains(" hooks codex PreToolUse"));
        assert_eq!(pre_tool[1], user_group["hooks"]["PreToolUse"][0]);
        enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        let reinstalled: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            reinstalled, installed,
            "re-enabling preserves both precedence and unrelated ordering byte-for-byte"
        );
        let codex = diagnose_all_with_environment(dir.path(), &DetectionEnvironment::default())
            .into_iter()
            .find(|row| row.agent == "codex")
            .unwrap();
        assert_eq!(codex.hook_wiring, Some(NativeHookWiring::Managed));
        assert!(codex.enabled());

        disable(dir.path(), "codex").unwrap();
        let remaining: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(remaining, user_group);
    }

    /// **D12: the Claude Code hook fragment had no live writer.**
    ///
    /// The legacy whole-set installer was the only emitter of
    /// `hook_fragment(ClaudeCode, …)`, and its last product caller went away in
    /// `fce2baea` (2026-08-11). From then on `rift enable --agent claude-code`
    /// wrote a descriptor advertising nine verbs, recorded it in the manifest,
    /// reported success — and registered no hook anywhere, in every fresh clone
    /// and every linked worktree. Nothing reddened because nothing compared the
    /// descriptor with the settings file.
    ///
    /// The fragment is written by the same emitter Codex's is, under the same
    /// manifest ownership, so the two cannot drift; the assertions here are the
    /// four facts that were missing: the file exists, it carries exactly the
    /// adapter's declared verbs, the user's own keys survive, and the manifest
    /// says the file is Rift's.
    #[test]
    fn claude_code_enable_registers_the_hooks_its_descriptor_advertises() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let user_settings = serde_json::json!({
            "permissions": { "allow": ["Bash(git status:*)"] },
            "hooks": {
                "SessionStart": [{
                    "hooks": [{"type": "command", "command": "their-own-hook"}]
                }]
            }
        });
        std::fs::write(&path, super::super::pretty_json(&user_settings)).unwrap();

        let report = enable(dir.path(), Path::new("/opt/rift"), "claude-code").unwrap();
        assert!(report.changed_files.contains(&path), "{report:?}");

        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            installed["permissions"], user_settings["permissions"],
            "an unrelated key is preserved verbatim"
        );
        assert!(
            installed["hooks"]["SessionStart"]
                .as_array()
                .is_some_and(|groups| groups
                    .iter()
                    .any(|group| group["hooks"][0]["command"] == "their-own-hook")),
            "the user's own hook survives beside Rift's: {installed}"
        );
        let registered: Vec<&str> = installed["hooks"]
            .as_object()
            .unwrap()
            .iter()
            .filter(|(_, groups)| {
                super::super::fragment_contains_rift_command(
                    groups,
                    super::super::ClientFamily::ClaudeCode,
                )
            })
            .map(|(event, _)| event.as_str())
            .collect();
        let declared = AdapterRegistry::with_builtin()
            .get(&AgentId::new("claude-code"))
            .unwrap()
            .verbs();
        assert_eq!(
            registered.len(),
            declared.len(),
            "a hook per declared verb, not a second table: {registered:?} vs {declared:?}"
        );

        let manifest = read_manifest(dir.path());
        let owned = manifest
            .hooks
            .get("claude-code")
            .expect("manifest ownership");
        assert_eq!(owned.path, ".claude/settings.local.json");
        assert_eq!(
            hook_file("claude-code"),
            Some(".claude/settings.local.json")
        );

        let row = diagnose_all_with_environment(dir.path(), &DetectionEnvironment::default())
            .into_iter()
            .find(|row| row.agent == "claude-code")
            .unwrap();
        assert_eq!(row.hook_wiring, Some(NativeHookWiring::Managed));
        assert!(
            row.enabled(),
            "descriptor plus fragment is what enabled means"
        );

        // Idempotent, then a clean round trip that leaves the user's file as
        // Rift found it.
        assert!(enable(dir.path(), Path::new("/opt/rift"), "claude-code")
            .unwrap()
            .changed_files
            .is_empty());
        disable(dir.path(), "claude-code").unwrap();
        let remaining: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(remaining, user_settings);
    }

    /// The never-clobber rule reaches the new file on its first day: a fragment
    /// carrying a Rift command Rift did not write is a conflict, and the
    /// refusal lands before the descriptor is touched.
    #[test]
    fn a_user_modified_claude_fragment_is_refused_as_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let theirs = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "command",
                        "command": "\"/their/rift\" hooks claude-code SessionStart",
                        "timeout": 90
                    }]
                }]
            }
        });
        let bytes = super::super::pretty_json(&theirs);
        std::fs::write(&path, &bytes).unwrap();

        let error = enable(dir.path(), Path::new("/opt/rift"), "claude-code").unwrap_err();
        assert!(
            error.to_string().contains("ownership fingerprint"),
            "{error}"
        );
        assert_eq!(std::fs::read(&path).unwrap(), bytes, "left byte-identical");
        assert!(!descriptor_path(dir.path(), "claude-code").exists());
        assert_eq!(
            diagnose_all_with_environment(dir.path(), &DetectionEnvironment::default())
                .into_iter()
                .find(|row| row.agent == "claude-code")
                .unwrap()
                .hook_wiring,
            Some(NativeHookWiring::Conflict)
        );
    }

    /// Today's fragment with one field edited: still unmistakably Rift's
    /// command, but no longer re-emittable from its own keys — so only a
    /// RECORD of the install can prove it is ours.
    fn unrecognisable_rift_fragment(rift_bin: &Path) -> serde_json::Value {
        let mut fragment =
            super::super::hook_fragment(super::super::ClientFamily::ClaudeCode, rift_bin);
        for (_, groups) in fragment.as_object_mut().unwrap() {
            groups[0]["hooks"][0]["timeout"] = serde_json::json!(5);
        }
        assert!(
            super::super::recognizable_rift_hook_fragment(
                &fragment,
                super::super::ClientFamily::ClaudeCode
            )
            .is_none(),
            "the fixture must defeat the shape route, or it proves nothing"
        );
        fragment
    }

    /// **The recognise-and-refresh shape, for the manifest the retired
    /// installer left behind**: a byte-recognisable legacy fingerprint is RECOGNISED and
    /// refreshed, never called a conflict.
    ///
    /// This is the state of every machine that ran `rift recall init` before
    /// `fce2baea`: `.claude/settings.local.json` holds a fragment the legacy
    /// installer wrote, `.rift/agents/recall-install.json` fingerprints it,
    /// and the native manifest has never held a `hooks` entry
    /// for Claude Code. Without this arm the writer's first act on a real
    /// machine would be to refuse to repair its own output.
    #[test]
    fn the_legacy_installers_manifest_still_proves_the_claude_fragment_is_ours() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let fragment = unrecognisable_rift_fragment(Path::new("/old/rift"));
        std::fs::write(
            &path,
            super::super::pretty_json(&serde_json::json!({ "hooks": fragment.clone() })),
        )
        .unwrap();
        let legacy = dir.path().join(".rift/agents/recall-install.json");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(
            &legacy,
            super::super::pretty_json(&serde_json::json!({
                "schema_version": 1,
                "clients": [{
                    "client": "claude-code",
                    "hook_path": ".claude/settings.local.json",
                    "hook_fingerprint": super::super::fingerprint_json(&fragment),
                }],
            })),
        )
        .unwrap();

        assert_eq!(
            diagnose_all_with_environment(dir.path(), &DetectionEnvironment::default())
                .into_iter()
                .find(|row| row.agent == "claude-code")
                .unwrap()
                .hook_wiring,
            Some(NativeHookWiring::Stale),
            "recognised, not conflicted"
        );

        enable(dir.path(), Path::new("/opt/rift"), "claude-code").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            installed["hooks"],
            super::super::hook_fragment(
                super::super::ClientFamily::ClaudeCode,
                Path::new("/opt/rift")
            ),
            "refreshed to today's table and today's binary"
        );
    }

    /// The other legacy record: `.rift/agents/claude-code.claude-settings.json`,
    /// the sidecar copy of what the retired installer wrote. Nothing has read
    /// it since `fce2baea`; it is read here as ownership evidence, and left
    /// exactly where it is — this crate does not delete a file it can still
    /// learn from.
    #[test]
    fn the_dead_claude_settings_sidecar_still_proves_the_fragment_is_ours() {
        let dir = tempfile::tempdir().unwrap();
        let path = claude_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let fragment = unrecognisable_rift_fragment(Path::new("/old/rift"));
        std::fs::write(
            &path,
            super::super::pretty_json(&serde_json::json!({ "hooks": fragment.clone() })),
        )
        .unwrap();
        let sidecar = dir
            .path()
            .join(".rift/agents/claude-code.claude-settings.json");
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        let sidecar_bytes =
            super::super::pretty_json(&serde_json::json!({ "hooks": fragment.clone() }));
        std::fs::write(&sidecar, &sidecar_bytes).unwrap();

        enable(dir.path(), Path::new("/opt/rift"), "claude-code").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            installed["hooks"],
            super::super::hook_fragment(
                super::super::ClientFamily::ClaudeCode,
                Path::new("/opt/rift")
            )
        );
        assert_eq!(
            std::fs::read(&sidecar).unwrap(),
            sidecar_bytes,
            "the sidecar is evidence, not a file to tidy away"
        );
    }

    /// **Codex's trust is Codex's to give.** Rift reads
    /// `[hooks.state]` to say whether a member's hook file has been approved,
    /// and the file it reads comes back byte-identical — a fresh worktree's
    /// `.codex/hooks.json` is a NEW absolute path, so it is untrusted no matter
    /// how correctly Rift wrote it, and the only honest thing to do is say so.
    ///
    /// The second fixture is a macOS hazard, not a hypothetical: a temp path is
    /// `/var/folders/…` and its canonical form is `/private/var/folders/…`, and
    /// Codex records whichever it resolved.
    #[test]
    fn codex_trust_is_read_from_codex_state_and_never_written() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("codex-home");
        std::fs::create_dir_all(&home).unwrap();
        let member = dir.path().join("member");
        let hooks = member.join(".codex/hooks.json");
        std::fs::create_dir_all(hooks.parent().unwrap()).unwrap();
        std::fs::write(&hooks, "{}").unwrap();
        let canonical = std::fs::canonicalize(&hooks).unwrap();

        let config = home.join("config.toml");
        let unapproved = dir.path().join("other/.codex/hooks.json");
        for (recorded, label) in [(&hooks, "as given"), (&canonical, "canonicalized")] {
            // A TOML basic string escapes its backslashes, as Codex does when
            // it records a Windows path.
            let bytes = format!(
                "[hooks.state.\"{}:pre_tool_use:0:0\"]\ntrusted_hash = \"sha256:abc\"\n",
                recorded.display().to_string().replace('\\', "\\\\")
            );
            std::fs::write(&config, &bytes).unwrap();
            assert_eq!(
                codex_hook_trust(&hooks, Some(&home)),
                CodexTrust::Approved,
                "{label}: a recorded path is approved"
            );
            assert_eq!(
                codex_hook_trust(&unapproved, Some(&home)),
                CodexTrust::Pending,
                "{label}: another member's path is not"
            );
            assert_eq!(
                std::fs::read_to_string(&config).unwrap(),
                bytes,
                "{label}: Rift never writes global client configuration"
            );
        }

        assert_eq!(
            codex_hook_trust(&hooks, None),
            CodexTrust::Pending,
            "no Codex home means no read at all"
        );
        assert_eq!(
            CodexTrust::Pending.message(),
            "pending (approve once in Codex)"
        );
    }

    /// The verify-only read behind adoption's `stale` report: which `rift` does
    /// this installed fragment actually invoke? Antigravity's bundle has its own
    /// shape and its own file — which the repository TRACKS — so it is the one
    /// that has to work through this route rather than through an install.
    #[test]
    fn installed_hook_binary_reads_every_family_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let tracked = dir.path().join(".agents/hooks.json");
        std::fs::create_dir_all(tracked.parent().unwrap()).unwrap();
        let bundle = serde_json::json!({
            "rift-recall": super::super::hook_fragment(
                super::super::ClientFamily::Antigravity,
                Path::new("/old/rift"),
            ),
            "someone-else": { "enabled": true },
        });
        let bytes = super::super::pretty_json(&bundle);
        std::fs::write(&tracked, &bytes).unwrap();

        assert_eq!(
            installed_hook_binary(dir.path(), "antigravity"),
            Some(PathBuf::from("/old/rift"))
        );
        assert_eq!(std::fs::read(&tracked).unwrap(), bytes, "read, not written");
        assert_eq!(
            installed_hook_binary(dir.path(), "claude-code"),
            None,
            "no file, no claim"
        );
        assert_eq!(
            installed_hook_binary(dir.path(), "not-a-family"),
            None,
            "an unknown token names no hook file"
        );

        enable(dir.path(), Path::new("/opt/rift"), "claude-code").unwrap();
        assert_eq!(
            installed_hook_binary(dir.path(), "claude-code"),
            Some(PathBuf::from("/opt/rift"))
        );
    }

    /// The Recall skill regions live in TRACKED files (`AGENTS.md`,
    /// `.agents/rules/rift-recall.md`), so adoption reports them and the
    /// explicit `rift enable` path writes them. This is the reporting half.
    #[test]
    fn the_recall_skill_region_is_reported_not_written() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(recall_skill_present(dir.path(), "codex"), Some(false));
        assert_eq!(
            recall_skill_present(dir.path(), "not-a-family"),
            None,
            "an adapter with no skill file has no answer to give"
        );

        let tracked = dir.path().join("AGENTS.md");
        std::fs::write(&tracked, "# house rules\n").unwrap();
        assert_eq!(recall_skill_present(dir.path(), "codex"), Some(false));
        assert_eq!(
            std::fs::read_to_string(&tracked).unwrap(),
            "# house rules\n",
            "asking is not writing"
        );

        install_recall_skill(dir.path(), "codex", "ask rift first").unwrap();
        assert_eq!(recall_skill_present(dir.path(), "codex"), Some(true));
    }

    #[test]
    fn explicit_antigravity_enable_merges_and_disable_removes_only_the_reserved_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let path = antigravity_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let user_bundle = serde_json::json!({
            "team-lint": {
                "enabled": true,
                "PreToolUse": [{"matcher": "write_file", "hooks": []}]
            }
        });
        std::fs::write(&path, super::super::pretty_json(&user_bundle)).unwrap();

        let report = enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(installed["team-lint"], user_bundle["team-lint"]);
        assert_eq!(
            installed["rift-recall"],
            super::super::hook_fragment(
                super::super::ClientFamily::Antigravity,
                Path::new("/opt/rift"),
            )
        );
        assert!(
            report.changed_files.contains(&path),
            "the explicit command reports the tracked hook file it changed"
        );
        assert_eq!(hook_file("antigravity"), Some(".agents/hooks.json"));
        assert_eq!(
            diagnose_all_with_environment(dir.path(), &DetectionEnvironment::default())
                .into_iter()
                .find(|row| row.agent == "antigravity")
                .unwrap()
                .hook_wiring,
            Some(NativeHookWiring::Managed)
        );

        let second = enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap();
        assert!(second.changed_files.is_empty(), "enable is idempotent");

        disable(dir.path(), "antigravity").unwrap();
        let remaining: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            remaining, user_bundle,
            "disable preserves unrelated bundles"
        );
    }

    #[test]
    fn antigravity_adoption_refreshes_the_descriptor_but_never_the_tracked_hook_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = antigravity_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let tracked = serde_json::json!({"team-lint": {"enabled": true}});
        let tracked_bytes = super::super::pretty_json(&tracked);
        std::fs::write(&path, &tracked_bytes).unwrap();

        let report =
            enable_for_adoption(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap();
        assert!(descriptor_path(dir.path(), "antigravity").exists());
        assert!(!report.changed_files.contains(&path));
        assert_eq!(std::fs::read(&path).unwrap(), tracked_bytes);

        let before_disable = std::fs::read(&path).unwrap();
        disable_for_adoption(dir.path(), "antigravity").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before_disable);
    }

    #[test]
    fn malformed_or_edited_antigravity_hooks_are_refused_before_any_native_write() {
        for (label, bytes) in [
            ("malformed", b"{not-json".as_slice()),
            ("non-object", b"[]".as_slice()),
            (
                "edited-reserved",
                br#"{"rift-recall":{"enabled":false},"team-lint":{"enabled":true}}"#.as_slice(),
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = antigravity_hook_path(dir.path());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, bytes).unwrap();
            let manifest = manifest_path(dir.path());
            std::fs::create_dir_all(manifest.parent().unwrap()).unwrap();
            let manifest_bytes = b"owner-state-must-not-change\n";
            std::fs::write(&manifest, manifest_bytes).unwrap();

            let error = enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap_err();
            assert!(
                error.to_string().contains("invalid JSON object")
                    || error.to_string().contains("ownership fingerprint"),
                "{label}: {error}"
            );
            assert_eq!(std::fs::read(&path).unwrap(), bytes, "{label}: hook bytes");
            assert_eq!(
                std::fs::read(&manifest).unwrap(),
                manifest_bytes,
                "{label}: manifest bytes"
            );
            assert!(
                !descriptor_path(dir.path(), "antigravity").exists(),
                "{label}: descriptor was written before refusal"
            );
        }
    }

    #[test]
    fn an_edited_installed_antigravity_bundle_is_not_removed() {
        let dir = tempfile::tempdir().unwrap();
        let path = antigravity_hook_path(dir.path());
        enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap();
        let mut installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        installed["rift-recall"]["enabled"] = false.into();
        let edited = super::super::pretty_json(&installed);
        std::fs::write(&path, &edited).unwrap();

        let error = disable(dir.path(), "antigravity").unwrap_err();
        assert!(error.to_string().contains("ownership fingerprint"));
        assert_eq!(std::fs::read(&path).unwrap(), edited);
        assert!(
            descriptor_path(dir.path(), "antigravity").exists(),
            "disable must preflight the reserved bundle before removing the descriptor"
        );
    }

    #[test]
    fn changing_every_antigravity_command_path_does_not_bypass_the_fingerprint() {
        fn rewrite_strings(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::String(text) => {
                    *text = text.replace("/opt/rift", "/tmp/edited-rift");
                }
                serde_json::Value::Array(values) => {
                    values.iter_mut().for_each(rewrite_strings);
                }
                serde_json::Value::Object(values) => {
                    values.values_mut().for_each(rewrite_strings);
                }
                _ => {}
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let path = antigravity_hook_path(dir.path());
        enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap();
        let mut installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        rewrite_strings(&mut installed["rift-recall"]);
        let edited = super::super::pretty_json(&installed);
        std::fs::write(&path, &edited).unwrap();

        let enable_error = enable(dir.path(), Path::new("/tmp/edited-rift"), "antigravity")
            .expect_err("a consistently edited bundle is still not manifest-owned");
        assert!(enable_error.to_string().contains("ownership fingerprint"));
        assert_eq!(std::fs::read(&path).unwrap(), edited);

        let disable_error = disable(dir.path(), "antigravity")
            .expect_err("disable must enforce the same fingerprint");
        assert!(disable_error.to_string().contains("ownership fingerprint"));
        assert_eq!(std::fs::read(&path).unwrap(), edited);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_antigravity_hook_surfaces_are_refused_before_any_native_write() {
        use std::os::unix::fs::symlink;

        for symlink_directory in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let outside = dir.path().join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            let outside_file = outside.join("hooks.json");
            let outside_bytes = b"{\"team-lint\":{\"enabled\":true}}\n";
            std::fs::write(&outside_file, outside_bytes).unwrap();
            if symlink_directory {
                symlink(&outside, dir.path().join(".agents")).unwrap();
            } else {
                std::fs::create_dir_all(dir.path().join(".agents")).unwrap();
                symlink(&outside_file, antigravity_hook_path(dir.path())).unwrap();
            }

            let error = enable(dir.path(), Path::new("/opt/rift"), "antigravity").unwrap_err();
            assert!(error.to_string().contains("symlinked Antigravity"));
            assert_eq!(std::fs::read(&outside_file).unwrap(), outside_bytes);
            assert!(!descriptor_path(dir.path(), "antigravity").exists());
            assert!(!manifest_path(dir.path()).exists());
        }
    }

    #[test]
    fn codex_hook_conflicts_are_refused_before_descriptor_changes() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let conflicting = serde_json::json!({
            "hooks": {
                "SessionStart": [{
                    "hooks": [{
                        "type": "command",
                        "command": "\"/user/rift\" hooks codex SessionStart"
                    }]
                }]
            }
        });
        let bytes = super::super::pretty_json(&conflicting);
        std::fs::write(&path, &bytes).unwrap();

        let error = enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap_err();
        assert!(error.to_string().contains("ownership fingerprint"));
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(!descriptor_path(dir.path(), "codex").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_codex_hook_file_is_refused_without_writes() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside.json");
        let outside_bytes = b"{\"hooks\":{}}";
        std::fs::write(&outside, outside_bytes).unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        symlink(&outside, &path).unwrap();

        let error = enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap_err();
        assert!(error.to_string().contains("symlinked Codex hook file"));
        assert_eq!(std::fs::read(outside).unwrap(), outside_bytes);
        assert!(!descriptor_path(dir.path(), "codex").exists());
    }

    #[test]
    fn exact_legacy_codex_hooks_are_adopted_and_refreshed() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let legacy = serde_json::json!({
            "hooks": super::super::hook_fragment(
                super::super::ClientFamily::Codex,
                Path::new("/old/rift"),
            )
        });
        std::fs::write(&path, super::super::pretty_json(&legacy)).unwrap();
        assert_eq!(
            diagnose_all_with_environment(dir.path(), &DetectionEnvironment::default())
                .into_iter()
                .find(|row| row.agent == "codex")
                .unwrap()
                .hook_wiring,
            Some(NativeHookWiring::Stale)
        );

        enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(
            installed["hooks"],
            super::super::hook_fragment(super::super::ClientFamily::Codex, Path::new("/opt/rift"),)
        );
    }

    /// **Hooks side: correcting a verb table must leave a hook file Rift itself
    /// wrote REFRESHABLE, never conflicted.**
    ///
    /// The descriptor half of the same rule landed with
    /// [`a_moved_verb_table_conflicts_the_pre_manifest_install_without_arm_two`];
    /// this is the half it was missing, measured live on the main
    /// checkout: `.codex/hooks.json` held the byte-exact 9-verb emission from
    /// before `SessionEnd` joined `CodexAdapter::verbs()`, the manifest
    /// predated its `hooks` map altogether, and every ownership route compared
    /// against TODAY's 10-key fragment — so inspect read `Conflict`, and
    /// enable, disable, and Project Settings all refused the repair they were
    /// asked to perform.
    #[test]
    fn a_moved_verb_table_leaves_the_installed_hook_fragment_refreshable() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old_bin = Path::new("/old/rift");
        let new_bin = Path::new("/opt/rift");

        // The superseded emission: today's fragment minus `SessionEnd`, which
        // is exactly what the uncorrected table produced.
        let mut superseded =
            super::super::hook_fragment(super::super::ClientFamily::Codex, old_bin);
        assert!(
            superseded
                .as_object_mut()
                .unwrap()
                .remove("SessionEnd")
                .is_some(),
            "SessionEnd must be in today's table for this plant to mean anything"
        );
        // THE CALIBRATION CLAUSE: the superseded fragment no longer equals
        // today's, so a recognizer built on today-fragment equality — the
        // previous body of `recognizable_codex_hook_fragment` — cannot pass
        // this test. Deleting this line leaves the migration with nothing to
        // be a migration FROM.
        assert_ne!(
            superseded,
            super::super::hook_fragment(super::super::ClientFamily::Codex, old_bin),
            "THE BREAKAGE: a moved verb table unmatches the installed fragment"
        );
        let user_group =
            serde_json::json!({"hooks": [{"type": "command", "command": "user-start"}]});
        superseded["SessionStart"]
            .as_array_mut()
            .unwrap()
            .insert(0, user_group.clone());
        std::fs::write(
            &path,
            super::super::pretty_json(&serde_json::json!({"hooks": superseded})),
        )
        .unwrap();

        // No manifest anywhere — the stranded population has none.
        assert_eq!(
            diagnose_all_with_environment(dir.path(), &DetectionEnvironment::default())
                .into_iter()
                .find(|row| row.agent == "codex")
                .unwrap()
                .hook_wiring,
            Some(NativeHookWiring::Stale),
            "a fragment Rift wrote from an older table is refreshable, not a conflict"
        );

        enable(dir.path(), new_bin, "codex").unwrap();
        let installed: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let expected = super::super::hook_fragment(super::super::ClientFamily::Codex, new_bin);
        for (event, groups) in expected.as_object().unwrap() {
            if event == "SessionStart" {
                continue;
            }
            assert_eq!(&installed["hooks"][event], groups, "{event} not refreshed");
        }
        assert_eq!(
            installed["hooks"]["SessionStart"].as_array().map(Vec::len),
            Some(2),
            "the user group must survive the refresh"
        );
        assert_eq!(
            installed["hooks"]["SessionStart"][0], user_group,
            "user entries are preserved, never rewritten"
        );
        assert_eq!(
            read_manifest(dir.path())
                .hooks
                .get("codex")
                .map(|owned| owned.fragment_hash.clone()),
            Some(super::super::fingerprint_json(&expected)),
            "the refresh takes manifest ownership of the new fragment"
        );
        let row = diagnose_all_with_environment(dir.path(), &DetectionEnvironment::default())
            .into_iter()
            .find(|row| row.agent == "codex")
            .unwrap();
        assert_eq!(row.hook_wiring, Some(NativeHookWiring::Managed));
        assert!(row.enabled());
    }

    /// Disable symmetry for the same stranded population: the superseded
    /// fragment is recognized, Rift's groups leave, the user's stays, and a
    /// second disable is a no-op.
    #[test]
    fn a_moved_verb_table_still_disables_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut superseded =
            super::super::hook_fragment(super::super::ClientFamily::Codex, Path::new("/old/rift"));
        assert!(superseded
            .as_object_mut()
            .unwrap()
            .remove("SessionEnd")
            .is_some());
        let user_group =
            serde_json::json!({"hooks": [{"type": "command", "command": "user-start"}]});
        superseded["SessionStart"]
            .as_array_mut()
            .unwrap()
            .insert(0, user_group.clone());
        std::fs::write(
            &path,
            super::super::pretty_json(&serde_json::json!({"hooks": superseded})),
        )
        .unwrap();

        disable(dir.path(), "codex").unwrap();
        let remaining: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            remaining,
            serde_json::json!({"hooks": {"SessionStart": [user_group]}}),
            "only Rift's groups leave; the user's stays"
        );
        assert!(disable(dir.path(), "codex")
            .unwrap()
            .changed_files
            .is_empty());
    }

    /// The negative arms that stop the shape migration being a hole: each
    /// candidate differs from Rift's emission by exactly one byte-level fact,
    /// and each stays a refused `Conflict` in BOTH repair directions with the
    /// file left byte-identical.
    #[test]
    fn a_hook_file_that_merely_resembles_rifts_stays_a_conflict() {
        let handler = |verb: &str, bin: &str| {
            serde_json::json!({
                "type": "command",
                "command": format!("\"{bin}\" hooks codex {verb}"),
                "timeout": 10,
            })
        };
        let cases = [
            (
                "an edited timeout",
                serde_json::json!({
                    "SessionStart": [{"hooks": [{
                        "type": "command",
                        "command": "\"/opt/rift\" hooks codex SessionStart",
                        "timeout": 11,
                    }]}]
                }),
            ),
            (
                "a second handler in the group",
                serde_json::json!({
                    "SessionStart": [{"hooks": [
                        handler("SessionStart", "/opt/rift"),
                        handler("SessionStart", "/opt/rift"),
                    ]}]
                }),
            ),
            (
                "a verb that does not match its key",
                serde_json::json!({
                    "Stop": [{"hooks": [handler("SessionStart", "/opt/rift")]}]
                }),
            ),
            (
                "a relative binary",
                serde_json::json!({
                    "SessionStart": [{"hooks": [handler("SessionStart", "rift")]}]
                }),
            ),
            (
                "two binaries across groups",
                serde_json::json!({
                    "SessionStart": [{"hooks": [handler("SessionStart", "/a/rift")]}],
                    "Stop": [{"hooks": [handler("Stop", "/b/rift")]}]
                }),
            ),
            (
                "a matcher on a non-tool event",
                serde_json::json!({
                    "SessionStart": [{
                        "matcher": "*",
                        "hooks": [handler("SessionStart", "/opt/rift")],
                    }]
                }),
            ),
        ];
        for (why, hooks) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = codex_hook_path(dir.path());
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let bytes = super::super::pretty_json(&serde_json::json!({"hooks": hooks}));
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(
                diagnose_all_with_environment(dir.path(), &DetectionEnvironment::default())
                    .into_iter()
                    .find(|row| row.agent == "codex")
                    .unwrap()
                    .hook_wiring,
                Some(NativeHookWiring::Conflict),
                "{why}: must stay a conflict"
            );
            let enable_err = enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap_err();
            assert!(
                enable_err.to_string().contains("ownership fingerprint"),
                "{why}: {enable_err}"
            );
            let disable_err = disable(dir.path(), "codex").unwrap_err();
            assert!(
                disable_err.to_string().contains("ownership fingerprint"),
                "{why}: {disable_err}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "{why}: bytes must be untouched"
            );
            assert!(
                !descriptor_path(dir.path(), "codex").exists(),
                "{why}: a refusal wrote a descriptor"
            );
        }
    }

    /// **The deliberately unhandled direction: a verb table that SHRINKS.**
    ///
    /// A Rift-shaped group under a key today's table no longer declares never
    /// enters `hook_subset`, so ownership cannot be established over it and
    /// the file reads `Conflict`. Handling it means widening the subset's key
    /// domain — fingerprint semantics for every standard family — against a
    /// measured population of zero: no adapter's table has ever shrunk. So the
    /// case is PLANTED, not handled, exactly the way arm 1's breakage was
    /// planted before the verb-table correction falsified its premise: when a table first
    /// shrinks, this test is what that migration is FROM.
    #[test]
    fn a_rift_group_under_a_retired_verb_key_is_still_a_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let path = codex_hook_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // "PreTask" is a verb no Codex table ever declared — standing in for a
        // retired one, which is indistinguishable to the subset.
        let bytes = super::super::pretty_json(&serde_json::json!({
            "hooks": {
                "PreTask": [{"hooks": [{
                    "type": "command",
                    "command": "\"/opt/rift\" hooks codex PreTask",
                    "timeout": 10,
                }]}]
            }
        }));
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            diagnose_all_with_environment(dir.path(), &DetectionEnvironment::default())
                .into_iter()
                .find(|row| row.agent == "codex")
                .unwrap()
                .hook_wiring,
            Some(NativeHookWiring::Conflict)
        );
        assert!(enable(dir.path(), Path::new("/opt/rift"), "codex").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
    }

    /// **The clone landmine, closed: a CURRENT-schema descriptor whose
    /// manifest never travelled is still Rift's own file.**
    ///
    /// Descriptors have been committed to repositories while `.rift/state/`
    /// is gitignored, so a fresh checkout — or a state wipe — holds a
    /// descriptor Rift wrote in today's schema with no manifest entry to
    /// vouch for it. Arm 1 derives the pre-manifest key set and arm 2 demands
    /// it, so before the third arm this file matched NOTHING and read
    /// `Conflict`, with `enable` and `disable` both refusing — the
    /// descriptor-level twin of the hooks-side stranding. The moved-table
    /// half drives the arm through a frozen adapter so it stays version-proof,
    /// not merely schema-proof.
    #[test]
    fn a_cloned_descriptor_without_local_state_is_still_rift_managed() {
        // Half 1: today's schema, today's table, no manifest → Managed, and
        // `enable` re-adopts it into a fresh manifest.
        let dir = tempfile::tempdir().unwrap();
        let registry = AdapterRegistry::with_builtin();
        let adapter = registry.get(&AgentId::new("codex")).unwrap();
        let path = descriptor_path(dir.path(), "codex");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            descriptor_bytes(
                Path::new("/opt/rift"),
                "codex",
                adapter.verbs(),
                &adapter.compat_notes(),
            ),
        )
        .unwrap();
        assert_eq!(
            inspect_wiring(dir.path(), "codex", adapter),
            NativeWiring::Managed,
            "a descriptor Rift wrote must not conflict because the manifest \
             stayed behind on another machine"
        );
        enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap();
        assert!(
            read_manifest(dir.path()).descriptors.contains_key("codex"),
            "enable must re-adopt the descriptor into the manifest"
        );

        // Half 2: the same recognition survives a MOVED verb table — the file
        // was written from the installed table, inspected under the published
        // one.
        let dir = tempfile::tempdir().unwrap();
        let as_installed = FrozenCursor {
            verbs: CURSOR_AS_INSTALLED,
        };
        let as_published = FrozenCursor {
            verbs: CURSOR_AS_PUBLISHED,
        };
        let path = descriptor_path(dir.path(), "cursor");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            descriptor_bytes(
                Path::new("/opt/rift"),
                "cursor",
                as_installed.verbs(),
                &as_installed.compat_notes(),
            ),
        )
        .unwrap();
        assert_eq!(
            inspect_wiring(dir.path(), "cursor", &as_published),
            NativeWiring::Managed,
            "the arm is verb-set free: a table move cannot unrecognize it"
        );
    }

    /// One byte-level deviation from Rift's current descriptor emission.
    type DescriptorMutation = fn(&mut serde_json::Value);

    /// The negative arms for the third recognizer: one wrong byte-level fact
    /// each, all `Conflict`, all refused by `enable`, bytes untouched.
    #[test]
    fn a_file_that_merely_resembles_a_current_descriptor_is_never_adopted() {
        let registry = AdapterRegistry::with_builtin();
        let adapter = registry.get(&AgentId::new("codex")).unwrap();
        let pristine: serde_json::Value = serde_json::from_slice(&descriptor_bytes(
            Path::new("/opt/rift"),
            "codex",
            adapter.verbs(),
            &adapter.compat_notes(),
        ))
        .unwrap();
        let cases: [(&str, DescriptorMutation); 6] = [
            ("a command naming another agent", |v| {
                v["hooks"][0]["command"] =
                    serde_json::json!("/opt/rift hooks not-codex SessionStart");
            }),
            ("an extra top-level key", |v| {
                v["note"] = serde_json::json!("mine");
            }),
            ("a foreign managed_by", |v| {
                v["managed_by"] = serde_json::json!("other");
            }),
            ("a relative rift_bin", |v| {
                v["rift_bin"] = serde_json::json!("target/debug/rift");
            }),
            ("a three-key hook entry", |v| {
                v["hooks"][0]["timeout"] = serde_json::json!(10);
            }),
            ("an unknown schema_version", |v| {
                v["schema_version"] = serde_json::json!(2);
            }),
        ];
        for (why, mutate) in cases {
            let dir = tempfile::tempdir().unwrap();
            let path = descriptor_path(dir.path(), "codex");
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut value = pristine.clone();
            mutate(&mut value);
            let mut bytes = serde_json::to_vec_pretty(&value).unwrap();
            bytes.push(b'\n');
            std::fs::write(&path, &bytes).unwrap();
            assert_eq!(
                inspect_wiring(dir.path(), "codex", adapter),
                NativeWiring::Conflict,
                "{why}: must stay a conflict"
            );
            let error = enable(dir.path(), Path::new("/opt/rift"), "codex").unwrap_err();
            assert!(
                error.to_string().contains("ownership fingerprint"),
                "{why}: {error}"
            );
            assert_eq!(
                std::fs::read(&path).unwrap(),
                bytes,
                "{why}: bytes must be untouched"
            );
        }
    }

    /// **The subject must be a SUPPORTED agent or this test stops testing what
    /// it is named for.** It used to write its user file under `gemini`; Gemini
    /// was then demoted, and `enable` now refuses an unsupported agent
    /// BEFORE it reads the descriptor — so the refusal would have been the
    /// unsupported-agent one and the ownership guard would have gone
    /// unexercised while the test kept passing on a different error. That is a
    /// silent coverage loss, which is the shape a scope change most easily
    /// causes, so the agent here is deliberately one the product wires.
    #[test]
    fn an_unowned_descriptor_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = descriptor_path(dir.path(), "claude-code");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let user = b"{\"agent\":\"mine\"}";
        std::fs::write(&path, user).unwrap();
        let error = enable(dir.path(), Path::new("/opt/rift"), "claude-code").unwrap_err();
        assert!(
            error.to_string().contains("ownership fingerprint"),
            "the ownership guard must be what refuses, not the scope guard: {error}"
        );
        assert_eq!(std::fs::read(path).unwrap(), user);
    }

    #[test]
    fn recall_skills_are_per_adapter_and_fingerprint_safe() {
        let dir = tempfile::tempdir().unwrap();
        let user = "# User rules\n";
        std::fs::write(dir.path().join("AGENTS.md"), user).unwrap();
        install_recall_skill(dir.path(), "codex", "Ask `rift agent-help`.").unwrap();
        let path = dir.path().join("AGENTS.md");
        let installed = std::fs::read_to_string(&path).unwrap();
        assert!(installed.starts_with(user));
        assert!(installed.contains("RIFT-MANAGED:codex:begin"));
        remove_recall_skill(dir.path(), "codex").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), user);

        install_recall_skill(dir.path(), "antigravity", "Ask Rift.").unwrap();
        let rules = dir.path().join(skill_path("antigravity").unwrap());
        std::fs::write(
            &rules,
            std::fs::read_to_string(&rules)
                .unwrap()
                .replace("Ask Rift.", "user edit"),
        )
        .unwrap();
        assert!(remove_recall_skill(dir.path(), "antigravity").is_err());
        assert!(rules.exists());
    }

    /// Claude Code discovers a skill by its YAML frontmatter, so a `SKILL.md`
    /// without one is a file the agent never reads. The frontmatter leads the
    /// file, the fence follows it, a re-install rewrites only the fence, and
    /// removal takes the whole file Rift created.
    #[test]
    fn the_claude_code_skill_leads_with_the_frontmatter_that_makes_it_discoverable() {
        let dir = tempfile::tempdir().unwrap();
        install_recall_skill(dir.path(), "claude-code", "Ask `rift agent-help`.").unwrap();
        let path = dir.path().join(skill_path("claude-code").unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("---\nname: rift-recall\n"), "{text}");
        assert!(
            text.contains("\n---\n\n<!-- RIFT-MANAGED:claude-code:begin -->"),
            "the fence must follow the frontmatter: {text}"
        );

        install_recall_skill(dir.path(), "claude-code", "A different body.").unwrap();
        let again = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            again.matches("name: rift-recall").count(),
            1,
            "a re-install duplicated the frontmatter: {again}"
        );
        assert!(again.starts_with("---\n"), "{again}");
        assert!(again.contains("A different body."), "{again}");
        assert!(!again.contains("Ask `rift agent-help`."), "{again}");

        remove_recall_skill(dir.path(), "claude-code").unwrap();
        assert!(
            !path.exists(),
            "Rift created the file and must remove it whole"
        );
    }

    /// The never-clobber rule, on a path Rift owns the NAME of but not the
    /// file at it. Ported from the retired `skills.rs`, which held the ONLY
    /// assertion of this behaviour anywhere in the workspace.
    ///
    /// Measured before porting, not assumed: the refusal's reason string
    /// occurs exactly twice in the tree — once in `install_recall_skill`'s
    /// docstring and once in the arm itself — and no test named it. Replacing
    /// the `None =>` arm with a silent write left the whole workspace green
    /// while the function overwrote a file it does not own, which is the
    /// behaviour both its own docstring and this module's header forbid.
    ///
    /// The assertions are on the TREE rather than on the `Err`, for the same
    /// reason the alpha-refusal test is: the failure worth guarding is not a
    /// missing refusal but one arriving after a write.
    #[test]
    fn an_unowned_file_at_a_rift_owned_skill_path_is_never_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(skill_path("claude-code").unwrap());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mine = "---\nname: mine\n---\n\nMy own skill, thanks.\n";
        std::fs::write(&path, mine).unwrap();

        let error = install_recall_skill(dir.path(), "claude-code", "Ask `rift agent-help`.")
            .expect_err("an unowned file must be refused, never adopted");
        assert!(error.to_string().contains("unowned file"), "{error}");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            mine,
            "a file Rift does not own was rewritten"
        );

        // The refusal has to land before ownership is recorded: a manifest
        // entry for a file Rift never wrote would let `remove_recall_skill`
        // delete somebody else's file later.
        assert!(
            !read_manifest(dir.path()).skills.contains_key("claude-code"),
            "a refused install claimed ownership"
        );

        // And removal leaves it exactly as it is.
        remove_recall_skill(dir.path(), "claude-code").unwrap();
        assert!(path.exists(), "a user's file was deleted");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), mine);
    }

    /// The channels Rift does not own a discovery contract for get no
    /// frontmatter at all: an unowned prefix on a shared instructions file
    /// would be Rift writing outside its fence.
    #[test]
    fn only_the_claude_code_channel_gets_a_frontmatter_prefix() {
        for token in ["codex", "antigravity"] {
            let dir = tempfile::tempdir().unwrap();
            install_recall_skill(dir.path(), token, "Ask Rift.").unwrap();
            let text =
                std::fs::read_to_string(dir.path().join(skill_path(token).unwrap())).unwrap();
            assert!(
                text.starts_with("<!-- RIFT-MANAGED:"),
                "{token} gained a prefix Rift does not own: {text}"
            );
        }
    }

    #[test]
    fn coming_soon_agents_receive_no_recall_file_or_manifest_claim() {
        for token in [
            "gemini",
            "cursor",
            "copilot-cli",
            "opencode",
            "factory-droid",
            "ollama",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let error = install_recall_skill(dir.path(), token, "Ask Rift.")
                .expect_err("a coming-soon adapter must be refused before writing");
            assert!(
                error.to_string().contains("coming soon"),
                "{token}: {error}"
            );
            assert_eq!(
                skill_path(token),
                None,
                "{token} keeps an instructions path the writer refuses to use"
            );
            assert!(
                !manifest_path(dir.path()).exists(),
                "{token} received an ownership manifest"
            );
        }
    }
}
