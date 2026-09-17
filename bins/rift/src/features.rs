//! Shared Total Recall lifecycle operations.
//!
//! Total Recall installs through the per-repo policy and native adapters:
//! initial ingest runs, and every enabled native adapter gains its managed
//! Recall pointer. Remove clears only those fingerprint-matching skill blocks;
//! capture integrations, injection configuration, and captured data remain.

use std::path::Path;

/// Shared Recall lifecycle report used by CLI init, `rift recall`, and Project
/// Settings.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub(crate) struct RecallLifecycleReport {
    pub(crate) integrations: Vec<String>,
    pub(crate) changed_files: Vec<std::path::PathBuf>,
}

/// The rift-total-recall install chain: staged multi-client provisioning →
/// registry write → initial ingest → verified serving. Each failed leg
/// surfaces its reason (the row's Failed text); On is a fact, not a hope.
pub(crate) fn recall_install(repo: &Path) -> Result<RecallLifecycleReport, String> {
    recall_provision(repo, true)
}

/// The install chain with the policy write made optional. `declare = false`
/// is the machine-local half alone — skills for the enabled adapters and the
/// initial ingest — for a repository whose committed configuration already
/// declares Recall on: setting up a second machine must not rewrite the file
/// the team shares, even to the value it already holds.
pub(crate) fn recall_provision(
    repo: &Path,
    declare: bool,
) -> Result<RecallLifecycleReport, String> {
    // Validate Rift's own config before the shared installer performs any
    // native-client writes. This keeps malformed/conflicting setup fail-closed.
    let policy = rift_config::load_project_policy(repo).map_err(|e| e.to_string())?;
    let mut report = RecallLifecycleReport::default();
    for row in rift_agent_install::native::diagnose_all(repo)
        .into_iter()
        .filter(rift_agent_install::native::NativeIntegrationStatus::enabled)
    {
        let changed = rift_agent_install::native::install_recall_skill(
            repo,
            &row.agent,
            &crate::agent_help::skill_body(),
        )
        .map_err(|e| format!("{} skill: {e}", row.display_name))?;
        report.integrations.push(row.agent);
        report.changed_files.extend(changed.changed_files);
    }
    if declare {
        rift_config::patch_project_policy(
            repo,
            &policy.revision,
            rift_config::ProjectPolicyPatch {
                total_recall: Some(true),
                ..rift_config::ProjectPolicyPatch::default()
            },
        )
        .map_err(|e| e.to_string())?;
        report.changed_files.push(repo.join(".rift/config.toml"));
    }
    crate::memory::launch_ingest(repo).map_err(|e| format!("initial recall ingest failed: {e}"))?;
    report.changed_files.sort();
    report.changed_files.dedup();
    Ok(report)
}

/// Remove Rift-owned native-client entries and reverse the registry flag;
/// recall data (the projection and checkpoints) stays intact.
pub(crate) fn recall_remove(repo: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    let policy = rift_config::load_project_policy(repo).map_err(|e| e.to_string())?;
    let mut changed = Vec::new();
    for row in rift_agent_install::native::diagnose_all(repo)
        .into_iter()
        .filter(rift_agent_install::native::NativeIntegrationStatus::enabled)
    {
        changed.extend(
            rift_agent_install::native::remove_recall_skill(repo, &row.agent)
                .map_err(|e| format!("{} skill: {e}", row.display_name))?
                .changed_files,
        );
    }
    rift_config::patch_project_policy(
        repo,
        &policy.revision,
        rift_config::ProjectPolicyPatch {
            total_recall: Some(false),
            ..rift_config::ProjectPolicyPatch::default()
        },
    )
    .map_err(|e| e.to_string())?;
    changed.push(repo.join(".rift/config.toml"));
    changed.sort();
    changed.dedup();
    Ok(changed)
}

/// Write `[features] total_recall` into `.rift/config.toml` — a
/// parse→modify→serialize round-trip over the TOML document. A missing or
/// comments-only file is upgraded to a minimal valid config (rift's own
/// scaffold file; comments do not survive a rewrite, which is documented
/// here); a file that fails to PARSE as TOML is refused, never clobbered.
#[cfg(test)]
fn write_recall_flag(repo: &Path, enabled: bool) -> Result<(), String> {
    let loaded = rift_config::load_project_policy(repo).map_err(|e| e.to_string())?;
    rift_config::patch_project_policy(
        repo,
        &loaded.revision,
        rift_config::ProjectPolicyPatch {
            total_recall: Some(enabled),
            ..rift_config::ProjectPolicyPatch::default()
        },
    )
    .map(|_| ())
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recall_flag_round_trips_and_upgrades_a_sparse_scaffold() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        // rift's own comments-only scaffold: upgraded to a valid config.
        std::fs::write(
            repo.join(".rift/config.toml"),
            "# Rift project configuration (.rift/config.toml).\n",
        )
        .unwrap();
        write_recall_flag(repo, true).unwrap();
        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        assert!(cfg.recall_enabled(), "the flag reads back");
        write_recall_flag(repo, false).unwrap();
        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        assert!(!cfg.recall_enabled());
    }

    #[test]
    fn recall_flag_preserves_existing_policy_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        std::fs::write(
            repo.join(".rift/config.toml"),
            "schema_version = 1\n\n[project]\nname = \"keepme\"\n\n[paths]\nsensitive_paths = [\"secrets/**\"]\n",
        )
        .unwrap();
        write_recall_flag(repo, true).unwrap();
        let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).unwrap();
        assert!(cfg.recall_enabled());
        assert_eq!(cfg.project_name(), "keepme", "project meta preserved");
        assert!(
            cfg.sensitive_paths().iter().any(|p| p == "secrets/**"),
            "policy keys preserved"
        );
    }

    #[test]
    fn invalid_toml_is_refused_never_clobbered() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::fs::create_dir_all(repo.join(".rift")).unwrap();
        std::fs::write(repo.join(".rift/config.toml"), "not [ valid toml").unwrap();
        let err = write_recall_flag(repo, true).unwrap_err();
        assert!(err.contains("not valid TOML"), "{err}");
        assert_eq!(
            std::fs::read_to_string(repo.join(".rift/config.toml")).unwrap(),
            "not [ valid toml"
        );
    }
}
