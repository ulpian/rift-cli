//! Global project registry — the persisted list of repositories Canopy watches.
//!
//! Rift's per-repo policy lives in a committed `.rift/config.toml` (see
//! `rift-config`); that is one project's *policy*. This crate is the orthogonal,
//! machine-global *index* of which repos the [`rift-canopy`] dashboard lists in
//! column 1. It is a thin, append-style TOML file
//! (`~/.config/rift/projects.toml` by default): [`ProjectRegistry::add`] dedups
//! by path, so registering the same repo twice is a no-op, and
//! [`ProjectRegistry::save`] writes atomically (temp file + rename) so a crash
//! never leaves a half-written registry.
//!
//! The config-directory lookup is injected ([`resolve_from`]) so it is fully
//! deterministic under test; [`default_registry_path`] is the thin wrapper over
//! the real process environment.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

/// One registered project: a display name and the repository path.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RegisteredProject {
    /// Human-readable project name (defaults to the directory basename).
    pub name: String,
    /// Path to the project's repository root (canonical, when registered via the
    /// CLI).
    pub path: PathBuf,
}

/// The machine-global set of projects Canopy watches.
///
/// Persisted as TOML. Append-style: [`add`](ProjectRegistry::add) dedups by
/// `path`, so the registry never holds two entries for the same repo.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectRegistry {
    /// The registered projects, in registration order.
    #[serde(default)]
    projects: Vec<RegisteredProject>,
}

impl ProjectRegistry {
    /// Load the registry from `path`. A missing file yields an empty registry
    /// (the secure, first-run default), not an error.
    ///
    /// # Errors
    /// [`RiftError::Io`] if the file exists but cannot be read, or
    /// [`RiftError::Config`] if its TOML is malformed.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path)?;
        toml::from_str(&text).map_err(|e| RiftError::config(format!("{}: {e}", path.display())))
    }

    /// Register a project. Returns `true` if it was added, `false` if a project
    /// with the same `path` was already present (dedup is by path; idempotent).
    pub fn add(&mut self, name: impl Into<String>, path: PathBuf) -> bool {
        if self.projects.iter().any(|p| p.path == path) {
            return false;
        }
        self.projects.push(RegisteredProject {
            name: name.into(),
            path,
        });
        true
    }

    /// The registered projects, in registration order.
    #[must_use]
    pub fn projects(&self) -> &[RegisteredProject] {
        &self.projects
    }

    /// Persist the registry to `path`, creating parent directories as needed.
    ///
    /// The write is atomic: the TOML is written to a sibling temp file and then
    /// renamed over `path`. A same-filesystem rename is atomic, so a crash mid-
    /// write never leaves a truncated registry.
    ///
    /// # Errors
    /// [`RiftError::Serde`] if serialization fails, or [`RiftError::Io`] if the
    /// directory cannot be created or the file cannot be written/renamed.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| RiftError::Serde(e.to_string()))?;
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

/// Resolve the registry file path from an injected environment lookup.
///
/// The config *root* is chosen from, in order: `RIFT_CONFIG_DIR` (an explicit
/// override, also what tests set for hermeticity), `XDG_CONFIG_HOME`, then
/// `$HOME/.config` on unix / `%APPDATA%` on Windows, falling back to `.`. The
/// registry file is always `<config-root>/rift/projects.toml`.
#[must_use]
pub fn resolve_from(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let base = env("RIFT_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| env("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| {
            if cfg!(windows) {
                env("APPDATA").map(PathBuf::from)
            } else {
                env("HOME").map(|h| PathBuf::from(h).join(".config"))
            }
        })
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("rift").join("projects.toml")
}

/// The default registry path for this machine — [`resolve_from`] over the real
/// process environment. Typically `~/.config/rift/projects.toml`.
#[must_use]
pub fn default_registry_path() -> PathBuf {
    resolve_from(|k| std::env::var(k).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(name: &str, path: &str) -> RegisteredProject {
        RegisteredProject {
            name: name.to_string(),
            path: PathBuf::from(path),
        }
    }

    #[test]
    fn missing_file_is_empty_registry() {
        let dir = tempfile::tempdir().unwrap();
        let reg = ProjectRegistry::load(&dir.path().join("does-not-exist.toml")).unwrap();
        assert!(reg.projects().is_empty());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("projects.toml");
        let mut reg = ProjectRegistry::default();
        assert!(reg.add("alpha", PathBuf::from("/r/alpha")));
        assert!(reg.add("beta", PathBuf::from("/r/beta")));
        reg.save(&path).unwrap();
        assert!(path.exists(), "save creates parent dirs and the file");

        let loaded = ProjectRegistry::load(&path).unwrap();
        assert_eq!(loaded, reg);
        assert_eq!(
            loaded.projects(),
            &[proj("alpha", "/r/alpha"), proj("beta", "/r/beta")]
        );
    }

    #[test]
    fn add_dedups_by_path_idempotently() {
        let mut reg = ProjectRegistry::default();
        assert!(reg.add("first", PathBuf::from("/r/x")));
        // Same path, different name → rejected, registry unchanged.
        assert!(!reg.add("second", PathBuf::from("/r/x")));
        assert_eq!(reg.projects().len(), 1);
        assert_eq!(reg.projects()[0].name, "first");
    }

    #[test]
    fn malformed_toml_is_a_config_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.toml");
        std::fs::write(&path, "this is = = not toml").unwrap();
        assert!(matches!(
            ProjectRegistry::load(&path),
            Err(RiftError::Config(_))
        ));
    }

    #[test]
    fn resolver_prefers_rift_config_dir() {
        let p = resolve_from(|k| match k {
            "RIFT_CONFIG_DIR" => Some("/cfg".to_string()),
            "XDG_CONFIG_HOME" => Some("/xdg".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        });
        assert_eq!(p, PathBuf::from("/cfg/rift/projects.toml"));
    }

    #[test]
    fn resolver_falls_back_to_xdg_then_home() {
        let xdg = resolve_from(|k| match k {
            "XDG_CONFIG_HOME" => Some("/xdg".to_string()),
            "HOME" => Some("/home/u".to_string()),
            _ => None,
        });
        assert_eq!(xdg, PathBuf::from("/xdg/rift/projects.toml"));

        if !cfg!(windows) {
            let home = resolve_from(|k| match k {
                "HOME" => Some("/home/u".to_string()),
                _ => None,
            });
            assert_eq!(home, PathBuf::from("/home/u/.config/rift/projects.toml"));
        }
    }

    proptest::proptest! {
        /// Adding projects in any order never produces two entries for the same
        /// path, and the count equals the number of distinct paths added.
        #[test]
        fn add_preserves_path_uniqueness(
            entries in proptest::collection::vec(("[a-z]{1,4}", "/r/[a-z]{1,5}"), 0..20)
        ) {
            let mut reg = ProjectRegistry::default();
            for (name, path) in &entries {
                reg.add(name.clone(), PathBuf::from(path));
            }
            let mut paths: Vec<&PathBuf> = reg.projects().iter().map(|p| &p.path).collect();
            let total = paths.len();
            paths.sort();
            paths.dedup();
            proptest::prop_assert_eq!(paths.len(), total, "no duplicate paths survive add()");

            let mut distinct_input: Vec<&String> = entries.iter().map(|(_, p)| p).collect();
            distinct_input.sort();
            distinct_input.dedup();
            proptest::prop_assert_eq!(total, distinct_input.len());
        }
    }
}
