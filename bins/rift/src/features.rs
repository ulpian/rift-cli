//! The real feature manager (M5-12/13, ADR-0013): the injected
//! [`rift_canopy::FeatureManager`] whose results drive the lifecycle rows.
//!
//! rift-total-recall installs FOR REAL: the per-repo policy config gains the
//! feature registry entry, `.mcp.json` is provisioned (one writer, the
//! byte-preserving splice in [`crate::mcpjson`]), the initial recall ingest
//! runs, and the memory daemon's serve path is verified — only then does the
//! manager return Ok (the row shows On). Remove reverses the registry write
//! and exactly our `.mcp.json` entry, leaving data intact. fff/codegraph (and
//! the language variants) stay staged: their flips persist desired state (the
//! append-only toggle events already carry it) and the row states plainly
//! that the engine lands at M7 — no theater.

use std::path::Path;

use rift_canopy::{Feature, FeatureAction, FeatureActionKind, FeatureManager};

/// The production manager, injected at launch beside the dispatcher.
pub(crate) struct RiftFeatureManager;

impl FeatureManager for RiftFeatureManager {
    fn apply(&mut self, action: &FeatureAction) -> Result<(), String> {
        let repo = Path::new(&action.path);
        match (action.feature, action.kind) {
            (Feature::TotalRecall, FeatureActionKind::Install | FeatureActionKind::Reinstall) => {
                recall_install(repo)
            }
            (Feature::TotalRecall, FeatureActionKind::Remove) => recall_remove(repo),
            // Staged features (engines land at M7): the desired state is
            // already persisted by the toggle events; nothing installs.
            _ => Ok(()),
        }
    }
}

/// The rift-total-recall install chain (M5-13): registry write → `.mcp.json`
/// provisioning → initial ingest → verified serving. Each failed leg surfaces
/// its reason (the row's Failed text); On is a fact, not a hope.
fn recall_install(repo: &Path) -> Result<(), String> {
    write_recall_flag(repo, true)?;
    let memd = crate::resolve_memd_binary().ok_or_else(|| {
        "rift-memd not found — build it (`cargo build -p rift-memd`) or put it on PATH".to_string()
    })?;
    crate::mcpjson::provision(repo, &memd)?;
    crate::memory::launch_ingest(repo).map_err(|e| format!("initial recall ingest failed: {e}"))?;
    crate::memory::verify_recall_serves(repo)?;
    Ok(())
}

/// Remove: reverse the registry write and exactly our `.mcp.json` entry;
/// recall data (the projection, checkpoints) stays intact.
fn recall_remove(repo: &Path) -> Result<(), String> {
    write_recall_flag(repo, false)?;
    crate::mcpjson::remove(repo)?;
    Ok(())
}

/// Write `[features] total_recall` into `.rift/config.toml` — a
/// parse→modify→serialize round-trip over the TOML document. A missing or
/// comments-only file is upgraded to a minimal valid config (rift's own
/// scaffold file; comments do not survive a rewrite, which is documented
/// here); a file that fails to PARSE as TOML is refused, never clobbered.
fn write_recall_flag(repo: &Path, enabled: bool) -> Result<(), String> {
    let rift_dir = repo.join(".rift");
    std::fs::create_dir_all(&rift_dir).map_err(|e| format!(".rift: {e}"))?;
    let path = rift_dir.join("config.toml");
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut doc: toml::Value = if text.trim().is_empty() {
        toml::Value::Table(toml::map::Map::new())
    } else {
        toml::from_str(&text)
            .map_err(|_| ".rift/config.toml is not valid TOML — fix it by hand".to_string())?
    };
    let table = doc
        .as_table_mut()
        .ok_or_else(|| ".rift/config.toml is not a TOML table".to_string())?;
    // The schema requires these; a sparse scaffold gains them.
    table
        .entry("schema_version")
        .or_insert(toml::Value::Integer(1));
    if !table.contains_key("project") {
        let mut project = toml::map::Map::new();
        let name = repo.file_name().map_or_else(
            || "unnamed".to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        project.insert("name".into(), toml::Value::String(name));
        table.insert("project".into(), toml::Value::Table(project));
    }
    let features = table
        .entry("features")
        .or_insert(toml::Value::Table(toml::map::Map::new()));
    let features_table = features
        .as_table_mut()
        .ok_or_else(|| "[features] is not a table".to_string())?;
    features_table.insert("total_recall".into(), toml::Value::Boolean(enabled));
    let out = toml::to_string_pretty(&doc).map_err(|e| format!("serialize config: {e}"))?;
    // Fail-closed: the write must produce a config the loader accepts.
    rift_config::EffectiveConfig::from_toml_str(&out)
        .map_err(|e| format!("config write would not validate: {e}"))?;
    std::fs::write(&path, out).map_err(|e| format!("config write: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The W2 dogfood (run explicitly): flip rift-total-recall ON for THIS
    /// repository through the REAL install chain — registry write, .mcp.json
    /// provisioning, initial ingest, verified serving. `cargo test -p rift
    /// --bin rift -- --ignored dogfood_install_recall_here`.
    #[test]
    #[ignore = "dogfood: installs recall on the REAL repository (run explicitly)"]
    fn dogfood_install_recall_here() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .expect("workspace root");
        let mut m = RiftFeatureManager;
        m.apply(&FeatureAction {
            feature: Feature::TotalRecall,
            kind: FeatureActionKind::Install,
            path: repo.to_string_lossy().into_owned(),
        })
        .expect("the real install chain succeeds on this repository");
    }

    #[test]
    fn staged_features_accept_without_installing() {
        let mut m = RiftFeatureManager;
        for feature in [Feature::Fff, Feature::Codegraph, Feature::CodegraphTs] {
            let r = m.apply(&FeatureAction {
                feature,
                kind: FeatureActionKind::Install,
                path: "/nonexistent".into(),
            });
            assert!(r.is_ok(), "staged {feature:?} never fails");
        }
    }

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

    #[test]
    fn install_fails_with_reason_when_memd_leg_breaks() {
        // Drive the real chain on a non-repo tempdir with memd resolvable:
        // the registry + .mcp.json legs succeed, the INGEST leg fails
        // honestly (not a git repository) — Failed-with-reason, not a hang.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let mut m = RiftFeatureManager;
        let r = m.apply(&FeatureAction {
            feature: Feature::TotalRecall,
            kind: FeatureActionKind::Install,
            path: repo.to_string_lossy().into_owned(),
        });
        let err = r.unwrap_err();
        assert!(
            err.contains("ingest failed") || err.contains("rift-memd not found"),
            "a broken leg names itself: {err}"
        );
    }
}
