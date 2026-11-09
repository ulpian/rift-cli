//! Per-user onboarding choices, separate from authoritative repository policy.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use rift_canopy::{ProjectInitialization, ProjectSettingId, ProjectSettingsView, ProjectSetupPlan};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Defaults {
    schema_version: u8,
    harnesses: Vec<String>,
    /// Written and validated so files from every alpha build keep loading,
    /// but no longer read: features ship on and are switched per project, so
    /// an old machine-wide "off" must not follow a user into new projects.
    features: BTreeMap<String, bool>,
}

impl Defaults {
    #[cfg(any(feature = "tui", test))]
    pub(super) fn from_plan(plan: &ProjectSetupPlan) -> Self {
        let values = [
            plan.total_recall,
            plan.turn_injection,
            plan.documents,
            plan.search,
            plan.tool_call_capture,
            plan.token_usage_capture,
            plan.raw_transcripts,
        ];
        Self {
            schema_version: 1,
            harnesses: plan.integrations.clone(),
            features: ProjectSettingId::ALL
                .into_iter()
                .zip(values)
                .map(|(id, value)| (id.token().into(), value))
                .collect(),
        }
    }

    fn validate(&self) -> std::io::Result<()> {
        let valid = self.schema_version == 1
            && self
                .harnesses
                .iter()
                .all(|agent| rift_agent_install::native::is_alpha_supported(agent))
            && self.features.len() == ProjectSettingId::ALL.len()
            && ProjectSettingId::ALL
                .iter()
                .all(|id| self.features.contains_key(id.token()));
        if valid {
            Ok(())
        } else {
            Err(invalid(
                "unsupported installation defaults; inspect defaults.toml",
            ))
        }
    }

    pub(super) fn seed(&self, view: &mut ProjectSettingsView) {
        // Resume plans and existing project policy always win. Permissions are
        // deliberately never global defaults: each project reviews them afresh.
        if !matches!(
            view.initialization,
            ProjectInitialization::NotInitialized | ProjectInitialization::NotProvisioned
        ) {
            return;
        }
        for row in &mut view.integrations {
            row.selected = row.alpha_supported && self.harnesses.contains(&row.agent);
        }
    }
}

#[cfg(feature = "tui")]
pub(crate) fn default_path() -> PathBuf {
    rift_registry::default_registry_path().with_file_name("defaults.toml")
}

pub(super) struct Snapshot {
    path: PathBuf,
    bytes: Option<Vec<u8>>,
    pub(super) defaults: Option<Defaults>,
}

impl Snapshot {
    pub(super) fn load(path: PathBuf) -> std::io::Result<Self> {
        let bytes = read(&path)?;
        let defaults = bytes
            .as_deref()
            .map(|bytes| {
                let text = std::str::from_utf8(bytes)
                    .map_err(|_| invalid("defaults.toml is not UTF-8"))?;
                let defaults: Defaults =
                    toml::from_str(text).map_err(|_| invalid("invalid defaults.toml"))?;
                defaults.validate()?;
                Ok::<_, std::io::Error>(defaults)
            })
            .transpose()?;
        Ok(Self {
            path,
            bytes,
            defaults,
        })
    }

    pub(super) fn save(&self, defaults: &Defaults) -> std::io::Result<()> {
        defaults.validate()?;
        let parent = self
            .path
            .parent()
            .ok_or_else(|| invalid("defaults path has no parent"))?;
        refuse_link(parent)?;
        std::fs::create_dir_all(parent)?;
        let lock = parent.join(".defaults.lock");
        std::fs::create_dir(&lock).map_err(|_| {
            invalid("defaults are being edited, or .defaults.lock needs inspection")
        })?;
        let _lock = Cleanup {
            path: lock,
            directory: true,
        };
        if read(&self.path)? != self.bytes {
            return Err(invalid("defaults changed on disk; rerun `rift install`"));
        }
        let temporary = parent.join(".defaults.toml.tmp");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let _temporary = Cleanup {
            path: temporary.clone(),
            directory: false,
        };
        let text =
            toml::to_string_pretty(defaults).map_err(|_| invalid("cannot serialize defaults"))?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        drop(file);
        // Same-directory rename replaces atomically on Unix and Windows.
        std::fs::rename(temporary, &self.path)?;
        if read(&self.path)?.as_deref() != Some(text.as_bytes()) {
            return Err(invalid("defaults verification failed"));
        }
        Ok(())
    }
}

struct Cleanup {
    path: PathBuf,
    directory: bool,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.directory {
            let _ = std::fs::remove_dir(&self.path);
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::other(message)
}

fn refuse_link(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() => Err(invalid("defaults refuse symbolic links")),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn read(path: &Path) -> std::io::Result<Option<Vec<u8>>> {
    if let Some(parent) = path.parent() {
        refuse_link(parent)?;
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if !metadata.is_file() || metadata.len() > 65_536 {
        return Err(invalid("defaults must be a small regular file"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(65_537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        return Err(invalid("defaults file is too large"));
    }
    Ok(Some(bytes))
}

#[cfg(feature = "tui")]
pub(crate) fn seed(view: &mut ProjectSettingsView) -> rift_core::Result<()> {
    if !matches!(
        view.initialization,
        ProjectInitialization::NotInitialized | ProjectInitialization::NotProvisioned
    ) {
        return Ok(());
    }
    let snapshot = Snapshot::load(default_path())?;
    if let Some(defaults) = snapshot.defaults {
        defaults.seed(view);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choices() -> ProjectSettingsView {
        let mut view = ProjectSettingsView::thin(String::new(), "fixture".into());
        view.settings = ProjectSettingId::ALL
            .into_iter()
            .map(|id| rift_canopy::ProjectSettingRow {
                id,
                enabled: true,
                actionable: true,
                detail: None,
            })
            .collect();
        view
    }

    #[test]
    fn defaults_round_trip_seed_new_projects_and_preserve_repository_or_pending_choices() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("defaults.toml");
        let mut source = choices();
        source.settings.first_mut().unwrap().enabled = false;
        let defaults = Defaults::from_plan(&ProjectSetupPlan::from_view(&source));
        Snapshot::load(path.clone())
            .unwrap()
            .save(&defaults)
            .unwrap();
        let loaded = Snapshot::load(path).unwrap().defaults.unwrap();
        assert_eq!(loaded, defaults);
        let mut fresh = choices();
        loaded.seed(&mut fresh);
        assert!(
            fresh.settings.iter().all(|row| row.enabled),
            "a feature switched off machine-wide by an older build stays on in a new project"
        );
        for state in [
            ProjectInitialization::NotProvisioned,
            ProjectInitialization::SetupIncomplete,
            ProjectInitialization::Initialized,
        ] {
            let mut existing = choices();
            existing.initialization = state;
            let before = existing.clone();
            loaded.seed(&mut existing);
            assert_eq!(existing, before);
        }
        let mut declared = choices();
        declared.policy.state = rift_canopy::ProjectPolicyState::Declared;
        loaded.seed(&mut declared);
        assert!(declared.settings.first().unwrap().enabled);
        assert!(!loaded.features["total_recall"], "the file keeps its shape");
    }

    #[test]
    fn stale_or_invalid_defaults_are_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("defaults.toml");
        let defaults = Defaults::from_plan(&ProjectSetupPlan::from_view(&choices()));
        let snapshot = Snapshot::load(path.clone()).unwrap();
        std::fs::write(&path, b"external edit").unwrap();
        assert!(snapshot.save(&defaults).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"external edit");
        assert!(Snapshot::load(path).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_defaults_and_temporary_paths_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("defaults.toml");
        let target = dir.path().join("external");
        std::fs::write(&target, "preserve").unwrap();
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(Snapshot::load(path.clone()).is_err());
        std::fs::remove_file(&path).unwrap();
        let snapshot = Snapshot::load(path).unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join(".defaults.toml.tmp")).unwrap();
        assert!(snapshot
            .save(&Defaults::from_plan(&ProjectSetupPlan::from_view(
                &choices()
            )))
            .is_err());
        assert_eq!(std::fs::read_to_string(target).unwrap(), "preserve");
    }
}
