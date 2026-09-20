//! Enable and disable one adapter: the descriptor, its hook fragment and the
//! ownership manifest move together, and every refusal lands before a write.

use std::path::Path;

use rift_core::AgentId;
use rift_shadow_hooks::{AdapterRegistry, AgentAdapter};

use crate::InstallError;

use super::descriptor::{descriptor_bytes, descriptor_path, rift_owns_descriptor};
use super::hooks::{prepare_hook_disable, prepare_hook_enable, writable_hook_family};
use super::manifest::{
    fingerprint, manifest_path, read_manifest, write_manifest, OwnedHook, OwnershipManifest,
};
use super::{
    is_alpha_supported, remove_file, write_if_changed, NativeIntegrationReport, ALPHA_AGENTS,
};

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

/// The enable pipeline: refuse, plan the hook write, prove the descriptor is
/// ours, then write and record. Both refusals come before the first write, so
/// a refused enable leaves the tree exactly as it found it.
fn enable_with_policy(
    repo: &Path,
    rift_bin: &Path,
    agent_token: &str,
    write_tracked_hook: bool,
) -> Result<NativeIntegrationReport, InstallError> {
    let registry = AdapterRegistry::with_builtin();
    let adapter = supported_adapter(&registry, repo, agent_token)?;
    let path = descriptor_path(repo, agent_token);
    let bytes = descriptor_bytes(
        rift_bin,
        agent_token,
        adapter.verbs(),
        &adapter.compat_notes(),
    );
    let mut manifest = read_manifest(repo);
    let hook = writable_hook_family(agent_token, write_tracked_hook)
        .map(|family| prepare_hook_enable(repo, family, rift_bin, &manifest))
        .transpose()?;
    refuse_unowned_descriptor(&path, &bytes, agent_token, &manifest, adapter)?;

    let mut changed = Vec::new();
    if write_if_changed(&path, &bytes)? {
        changed.push(path);
    }
    manifest
        .descriptors
        .insert(agent_token.to_owned(), fingerprint(&bytes));
    if let Some(hook) = hook {
        if write_if_changed(&hook.path, &hook.bytes)? {
            changed.push(hook.path);
        }
        manifest.hooks.insert(
            agent_token.to_owned(),
            OwnedHook {
                path: hook.family.hook_path().to_owned(),
                fragment_hash: hook.fragment_hash,
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

/// The adapter [`enable`] may write for, or the refusal that stops it: an
/// unknown token, an adapter without lifecycle verbs, an agent outside the
/// alpha.
///
/// The alpha-set refusal happens HERE — before the descriptor bytes are built,
/// before the manifest is read, before the hook plan — so a not-yet-supported
/// agent cannot leave a partial install behind. A "coming soon" path that
/// writes a descriptor anyway is exactly the class this refusal exists to
/// remove.
fn supported_adapter<'a>(
    registry: &'a AdapterRegistry,
    repo: &Path,
    agent_token: &str,
) -> Result<&'a dyn AgentAdapter, InstallError> {
    let refuse = |reason: String| InstallError::Config {
        path: descriptor_path(repo, agent_token),
        reason,
    };
    let Some(adapter) = registry.get(&AgentId::new(agent_token)) else {
        return Err(refuse(
            "no built-in adapter exists for this agent".to_owned(),
        ));
    };
    if adapter.verbs().is_empty() {
        return Err(refuse(
            "adapter declares no native lifecycle verbs".to_owned(),
        ));
    }
    if !is_alpha_supported(agent_token) {
        return Err(refuse(format!(
            "capture for {agent_token} is not supported yet — this alpha captures {}. \
             The adapter stays listed, but Rift will not write a descriptor that wires \
             a hook surface this agent does not send",
            ALPHA_AGENTS.join(", ")
        )));
    }
    Ok(adapter)
}

/// A file already at the descriptor path is left alone unless it is exactly
/// the bytes about to be written, or provably Rift's own; anything else is
/// refused before the first write.
fn refuse_unowned_descriptor(
    path: &Path,
    bytes: &[u8],
    agent_token: &str,
    manifest: &OwnershipManifest,
    adapter: &dyn AgentAdapter,
) -> Result<(), InstallError> {
    let Some(existing) = read_optional(path)? else {
        return Ok(());
    };
    if existing.as_slice() == bytes
        || rift_owns_descriptor(&existing, agent_token, manifest, Some(adapter))
    {
        return Ok(());
    }
    Err(fingerprint_mismatch(path))
}

/// The disable pipeline, the enable's mirror: plan the hook removal, prove the
/// descriptor is ours, then remove and un-record.
fn disable_with_policy(
    repo: &Path,
    agent_token: &str,
    write_tracked_hook: bool,
) -> Result<NativeIntegrationReport, InstallError> {
    let path = descriptor_path(repo, agent_token);
    let mut manifest = read_manifest(repo);
    let hook = writable_hook_family(agent_token, write_tracked_hook)
        .map(|family| prepare_hook_disable(repo, family, &manifest))
        .transpose()?
        .flatten();

    let mut changed = Vec::new();
    if let Some(existing) = read_optional(&path)? {
        let registry = AdapterRegistry::with_builtin();
        let adapter = registry.get(&AgentId::new(agent_token));
        if !rift_owns_descriptor(&existing, agent_token, &manifest, adapter) {
            return Err(fingerprint_mismatch(&path));
        }
        remove_file(&path)?;
        changed.push(path);
    }
    if let Some((hook_path, hook_bytes)) = hook {
        if write_if_changed(&hook_path, &hook_bytes)? {
            changed.push(hook_path);
        }
    }
    manifest.descriptors.remove(agent_token);
    // Adoption never touches Antigravity's tracked hook file, so it keeps the
    // record of the bundle it left in place.
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

fn fingerprint_mismatch(path: &Path) -> InstallError {
    InstallError::Config {
        path: path.to_path_buf(),
        reason: "managed integration file differs from Rift's ownership fingerprint".to_owned(),
    }
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::descriptor::legacy_descriptor_value;
    use super::super::{diagnose_all, NativeHookWiring, NativeWiring};
    use super::*;

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
}
