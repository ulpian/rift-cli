//! Native capture-integration lifecycle driven by the complete adapter registry.
//!
//! This is deliberately separate from the legacy four-client Recall
//! installer. Detection and managed capture wiring are independent facts; no
//! server probe, connection lease, or transport state participates here.
//!
//! One module per artifact Rift owns: the descriptor, the hook file, the skill
//! region, the manifest that fingerprints them — and the proofs that stand in
//! for a manifest entry that never travelled. This file holds the report types
//! every surface reads and the read-only diagnosis over the whole registry.

mod antigravity;
pub(crate) mod codex_client;
mod codex_identity;
mod codex_trust;
mod compose;
mod descriptor;
#[cfg(test)]
mod fixtures;
mod hooks;
mod legacy;
mod lifecycle;
mod manifest;
pub mod permissions;
mod skill;

use std::path::{Path, PathBuf};

use rift_core::AgentId;
use rift_shadow_hooks::{AdapterRegistry, AgentAdapter};
use serde::{Deserialize, Serialize};

use crate::InstallError;

use descriptor::{descriptor_path, rift_owns_descriptor};
use hooks::{hook_family, inspect_client_hooks};
use manifest::read_manifest;

pub(crate) use codex_identity::newest_proven_codex_version;
pub use codex_trust::{
    codex_home_from_process, codex_hook_trust, codex_trust, CodexClient, CodexHookTrust,
    CodexHookVerdict, CodexSurface, CodexTrust,
};
pub use compose::{hook_composition, HookComposition};
pub use hooks::{hook_file, installed_hook_binary};
pub use lifecycle::{disable, disable_for_adoption, enable, enable_for_adoption};
pub use skill::{install_recall_skill, recall_skill_present, remove_recall_skill, skill_region};

/// The alpha's supported set and its predicate live beside the adapter
/// registry in `rift-shadow-hooks`, and are re-exported here because this is
/// the crate that enforces them: [`enable`] refuses everything outside
/// [`ALPHA_AGENTS`]. The definition moved so the completeness sweep can bind
/// its unsupported-adapter rows to the very list the installer refuses on —
/// a second copy would drift, and the drift would read as a table calling an
/// agent not-yet-supported while `enable` wired it.
pub use rift_shadow_hooks::{is_alpha_supported, ALPHA_AGENTS};

/// The detection vocabulary — one rule for every surface (see
/// [`crate::detect`]).
pub use crate::detect::{
    DetectionEnvironment, NativeIdeHost, NativeInstallSource, NativeInstallation,
};

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
    /// Codex's trust in each hook Rift wrote into `.codex/hooks.json`: present
    /// for Codex when the file holds a Rift hook, `None` for every other row.
    /// Read at diagnosis time only, never on a hook's decision path.
    pub codex_trust: Option<CodexTrust>,
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

fn diagnose_one(
    repo: &Path,
    agent: &AgentId,
    adapter: &dyn AgentAdapter,
    env: &DetectionEnvironment,
) -> NativeIntegrationStatus {
    let token = agent.as_str();
    let mut status = installation_candidate(token, env);
    status.wiring = inspect_wiring(repo, token, adapter);
    status.hook_wiring =
        hook_family(token).map(|family| inspect_client_hooks(repo, family, status.wiring));
    status.codex_trust = (token == AgentId::CODEX.as_str())
        .then(|| codex_trust(repo, env.codex_home.as_deref()))
        .flatten();
    status
}

/// Detect harnesses for installer defaults without inspecting any project artifacts.
#[must_use]
pub fn installation_candidates(env: &DetectionEnvironment) -> Vec<NativeIntegrationStatus> {
    AdapterRegistry::with_builtin()
        .agents()
        .iter()
        .map(|agent| installation_candidate(agent.as_str(), env))
        .collect()
}

fn installation_candidate(token: &str, env: &DetectionEnvironment) -> NativeIntegrationStatus {
    let installations = crate::detect::installations(token, env);
    let executable = crate::detect::first_executable(&installations);
    let detection = if installations.is_empty() {
        NativeDetection::NotDetected
    } else {
        NativeDetection::Detected
    };
    NativeIntegrationStatus {
        agent: token.to_owned(),
        display_name: display_name(token).to_owned(),
        detection,
        wiring: NativeWiring::Missing,
        executable,
        installations,
        hook_wiring: None,
        codex_trust: None,
    }
}

fn inspect_wiring(repo: &Path, token: &str, adapter: &dyn AgentAdapter) -> NativeWiring {
    let Ok(bytes) = std::fs::read(descriptor_path(repo, token)) else {
        return NativeWiring::Missing;
    };
    if serde_json::from_slice::<serde_json::Value>(&bytes).is_err() {
        return NativeWiring::Invalid;
    }
    if rift_owns_descriptor(&bytes, token, &read_manifest(repo), Some(adapter)) {
        NativeWiring::Managed
    } else {
        NativeWiring::Conflict
    }
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

/// Write `bytes` to `path` unless the file already holds exactly them; `true`
/// when a write happened, so a report can list the files that changed.
fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<bool, InstallError> {
    if std::fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(false);
    }
    crate::atomic_write(path, bytes)?;
    Ok(true)
}

fn remove_file(path: &Path) -> Result<(), InstallError> {
    std::fs::remove_file(path).map_err(|source| InstallError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
