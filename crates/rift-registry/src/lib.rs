//! Global project registry — the persisted list of repositories Canopy watches.
//!
//! Rift's per-repo policy lives in a committed `.rift/config.toml` (see
//! `rift-config`); that is one project's *policy*. This crate is the orthogonal,
//! machine-global *index* of which repos the `rift-canopy` dashboard lists in
//! column 1. It is a thin, append-style TOML file
//! (`~/.config/rift/projects.toml` by default): [`ProjectRegistry::add`] dedups
//! by **canonical** path, so registering the same repo twice — or through a
//! second spelling of one directory — is a no-op, and
//! [`ProjectRegistry::save`] writes atomically (temp file + rename) so a crash
//! never leaves a half-written registry.
//!
//! # One row per repository *family*
//!
//! A linked git worktree is a second path to one repository, not a second
//! repository. Registering it produced a duplicate project: two rows, two
//! cards, one repository. [`ProjectRegistry::add_with_family`] closes that by
//! deduplicating on a **family id the caller injects** — an opaque string this
//! crate never interprets and never derives, which is what keeps the registry
//! index free of any dependency on git. The bin resolves the family (it owns
//! the repository it runs in); this crate only remembers which id a row was
//! registered under, and refuses a second row that shares it.
//!
//! Supersession — replacing a member's row with its family root's — is the
//! append-only Canopy store's job; this file is imported once
//! into that store and is not the dashboard's source of truth.
//!
//! The config-directory lookup is injected ([`resolve_from`]) so it is fully
//! deterministic under test; [`default_registry_path`] is the thin wrapper over
//! the real process environment.
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

use std::path::{Path, PathBuf};

use rift_core::{Result, RiftError};

/// One registered project: a display name and the repository path.
///
/// `deny_unknown_fields` (CLAUDE.md's config rule): a key this build does not
/// know is a malformed registry, not a field to ignore — the alternative is a
/// typo silently dropping a project from the dashboard.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegisteredProject {
    /// Human-readable project name (defaults to the directory basename).
    pub name: String,
    /// Path to the project's repository root — **canonical**, as
    /// [`ProjectRegistry::add`] resolves it, unless the directory did not exist
    /// when it was registered.
    pub path: PathBuf,
    /// The repository family this row holds the one seat for, as the caller
    /// injected it. An opaque token: this crate compares it and
    /// never interprets it.
    ///
    /// `None` on every row written before this rule, and on a path that is in
    /// no repository at all. Absent from the serialized form when `None`, so a
    /// registry of legacy rows round-trips byte for byte.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_id: Option<String>,
}

/// The machine-global set of projects Canopy watches.
///
/// Persisted as TOML. Append-style: [`add`](ProjectRegistry::add) dedups by
/// canonical `path` and [`add_with_family`](ProjectRegistry::add_with_family)
/// additionally by family id, so the registry never holds two entries for the
/// same repository — by either spelling, or by two members of one family.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectRegistry {
    /// The registered projects, in registration order.
    #[serde(default)]
    projects: Vec<RegisteredProject>,
}

/// What a registration attempt did — and, when it did nothing, why.
///
/// `add` collapses this to a `bool` for the callers that only report "new" or
/// "already there"; the refusal carries the registered path because the one
/// thing a user needs on being refused is where the family already lives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Added {
    /// A new row was appended.
    Registered,
    /// This exact directory is already registered. Nothing changed.
    AlreadyPresent,
    /// Another path of the same repository family holds the family's one row.
    /// Nothing was added; the registered path is named.
    FamilyRegisteredAs(PathBuf),
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

    /// Register a project. Returns `true` if it was added, `false` if the same
    /// directory was already present (dedup is by canonical path; idempotent).
    ///
    /// The family-blind door: use [`Self::add_with_family`] wherever the
    /// caller can resolve a family, which is everywhere inside a repository.
    pub fn add(&mut self, name: impl Into<String>, path: PathBuf) -> bool {
        self.add_with_family(name, path, None) == Added::Registered
    }

    /// Register a project under its repository family, refusing a second row
    /// for a family that already has one.
    ///
    /// Two dedup keys, in this order:
    ///
    /// 1. the **canonical** path — `/repo`, `/repo/.`, `/repo/../repo` and a
    ///    symlinked spelling are one directory, and comparing the raw strings
    ///    (as this did) registered each of them as its own project;
    /// 2. the injected `family_id` — a linked worktree is a second path to one
    ///    repository, so its family already holds the family's seat and the
    ///    row is refused rather than duplicated.
    ///
    /// A row with no recorded family id (every row written before this rule)
    /// never matches key 2: an absent id is "unknown", and treating it as a
    /// match would let one legacy row swallow every unrelated repository.
    ///
    /// The path is stored canonicalized, so the row a later reader compares is
    /// the spelling this function compared. Where the directory does not exist,
    /// the given spelling is kept verbatim — a registry entry for a path that
    /// is temporarily unmounted is still a row, and inventing a canonical form
    /// for it is not possible.
    pub fn add_with_family(
        &mut self,
        name: impl Into<String>,
        path: PathBuf,
        family_id: Option<&str>,
    ) -> Added {
        let canonical = comparable(&path);
        if self
            .projects
            .iter()
            .any(|p| comparable(&p.path) == canonical)
        {
            return Added::AlreadyPresent;
        }
        if let Some(family_id) = family_id {
            if let Some(existing) = self
                .projects
                .iter()
                .find(|p| p.family_id.as_deref() == Some(family_id))
            {
                return Added::FamilyRegisteredAs(existing.path.clone());
            }
        }
        self.projects.push(RegisteredProject {
            name: name.into(),
            path: canonical,
            family_id: family_id.map(str::to_owned),
        });
        Added::Registered
    }

    /// The registered projects, in registration order.
    #[must_use]
    pub fn projects(&self) -> &[RegisteredProject] {
        &self.projects
    }

    /// Drop every row `gone` selects and return the dropped rows, in
    /// registration order. This file is a per-machine index, so a row is
    /// removed rather than superseded; the caller decides what is provably
    /// gone, and this crate never looks at the disk for it.
    pub fn remove_where(
        &mut self,
        gone: impl Fn(&RegisteredProject) -> bool,
    ) -> Vec<RegisteredProject> {
        let (dropped, kept) = std::mem::take(&mut self.projects)
            .into_iter()
            .partition(|p| gone(p));
        self.projects = kept;
        dropped
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

/// A path as this registry compares and stores it: canonical where the
/// directory resolves, the given spelling where it does not.
///
/// The fallback is not laziness. A registered project whose volume is
/// unmounted still has a row, and `canonicalize` cannot answer for it; keeping
/// the recorded spelling means such a row still dedups against itself.
fn comparable(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Resolve the registry file path from an injected environment lookup.
///
/// The registry file is always `<config-root>/rift/projects.toml`, the root
/// from [`configured_config_root`] or, with none named, one nothing can be
/// created under.
#[must_use]
pub fn resolve_from(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    let base = configured_config_root(env).unwrap_or_else(|| PathBuf::from(UNRESOLVED_CONFIG_ROOT));
    base.join("rift").join("projects.toml")
}

/// What a process should say when [`configured_config_root`] has no answer.
pub const CONFIG_ROOT_UNSET: &str =
    "no config directory: set HOME (or APPDATA on Windows) or RIFT_CONFIG_DIR";

/// The root [`resolve_from`] answers when the environment names none: a path
/// under a device file, so the registry can be neither read nor written there.
/// Rift's binaries refuse before reaching it (they ask
/// [`configured_config_root`] first); the old fallback, `.`, wrote the
/// registry into whatever directory the process ran in.
///
/// Windows has no device file that refuses a child: `\\.\NUL\…` answers
/// `create_dir_all` with success. The interior NUL byte there is refused by
/// the standard library before any system call.
const UNRESOLVED_CONFIG_ROOT: &str = if cfg!(windows) {
    "\\\\.\\NUL\\rift-config-unset\0"
} else {
    "/dev/null/rift-config-unset"
};

/// The config *root* the environment names, or `None` when it names none:
/// `RIFT_CONFIG_DIR` (an explicit override, also what tests set for
/// hermeticity), `XDG_CONFIG_HOME`, then `$HOME/.config` on unix /
/// `%APPDATA%` on Windows. An empty value counts as unset, because an empty
/// path resolves against the process's directory.
#[must_use]
pub fn configured_config_root(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |key: &str| env(key).filter(|value| !value.is_empty());
    set("RIFT_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| set("XDG_CONFIG_HOME").map(PathBuf::from))
        .or_else(|| {
            if cfg!(windows) {
                set("APPDATA").map(PathBuf::from)
            } else {
                set("HOME").map(|h| PathBuf::from(h).join(".config"))
            }
        })
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
            family_id: None,
        }
    }

    #[test]
    fn remove_where_drops_only_the_selected_rows_and_keeps_the_order_of_the_rest() {
        let mut reg = ProjectRegistry::default();
        for name in ["a", "b", "c"] {
            assert!(reg.add(name, PathBuf::from(format!("/r/{name}"))));
        }
        let dropped = reg.remove_where(|p| p.name == "b");
        assert_eq!(dropped, vec![proj("b", "/r/b")]);
        assert_eq!(reg.projects(), &[proj("a", "/r/a"), proj("c", "/r/c")]);
        assert!(reg.remove_where(|_| false).is_empty());
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

    #[test]
    fn an_environment_naming_no_root_is_refused_and_never_the_working_directory() {
        assert_eq!(configured_config_root(|_| None), None);
        assert_eq!(
            configured_config_root(|_| Some(String::new())),
            None,
            "an empty value would resolve against the process's directory"
        );
        let fallback = resolve_from(|_| None);
        assert!(
            std::fs::create_dir_all(fallback.parent().unwrap()).is_err(),
            "nothing can be created under the unresolved root: {fallback:?}"
        );
    }

    /// ★ One directory, one row, whatever the caller spelled it.
    ///
    /// `add` compared raw strings, so `/repo` and `/repo/../repo` registered as
    /// two projects — two cards for one repository, and two rows for the family
    /// dedup below to disagree about. A `..` hop rather than a `.` one on
    /// purpose: `Path` equality already normalises `.` away, so only a spelling
    /// that survives component comparison states the claim.
    #[test]
    fn add_canonicalizes_the_path_before_it_compares_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let detour = dir.path().join("repo/../repo");
        assert_ne!(repo, detour, "the two spellings differ as paths");

        let mut reg = ProjectRegistry::default();
        assert!(reg.add("repo", detour.clone()));
        assert!(
            !reg.add("repo-again", detour),
            "idempotent in its own terms"
        );
        assert!(
            !reg.add("repo-canonical", repo.clone()),
            "and across spellings"
        );
        assert_eq!(reg.projects().len(), 1);
        assert_eq!(
            reg.projects()[0].path,
            std::fs::canonicalize(&repo).unwrap(),
            "the row stores the spelling it compares"
        );
    }

    /// ★ A second member of a registered family is refused, and
    /// the refusal names where the family already lives.
    ///
    /// The family id is injected: this crate never resolves one, which is what
    /// keeps a machine-global index free of a dependency on git. Two real,
    /// distinct directories are used, so path dedup cannot be what refuses the
    /// second one.
    #[test]
    fn add_with_family_keeps_one_row_per_family() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        let member = dir.path().join("member");
        std::fs::create_dir_all(&main).unwrap();
        std::fs::create_dir_all(&member).unwrap();

        let mut reg = ProjectRegistry::default();
        assert_eq!(
            reg.add_with_family("main", main.clone(), Some("fam-1")),
            Added::Registered
        );
        assert_eq!(
            reg.add_with_family("member", member.clone(), Some("fam-1")),
            Added::FamilyRegisteredAs(std::fs::canonicalize(&main).unwrap()),
            "the member is refused, and told which path holds the family's row"
        );
        assert_eq!(reg.projects().len(), 1);

        // A different family is not caught by the same net.
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(
            reg.add_with_family("other", other, Some("fam-2")),
            Added::Registered
        );
        assert_eq!(reg.projects().len(), 2);
        assert_eq!(reg.projects()[0].family_id.as_deref(), Some("fam-1"));
    }

    /// A row with no recorded family id — every row written before this rule —
    /// blocks nothing but its own path.
    ///
    /// The alternative is worse than a duplicate: comparing `None` to `None`
    /// would make ONE legacy row refuse every repository on the machine.
    #[test]
    fn a_legacy_row_without_a_family_never_swallows_another_repository() {
        let dir = tempfile::tempdir().unwrap();
        let legacy = dir.path().join("legacy");
        let fresh = dir.path().join("fresh");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::create_dir_all(&fresh).unwrap();

        let mut reg = ProjectRegistry::default();
        assert_eq!(
            reg.add_with_family("legacy", legacy, None),
            Added::Registered
        );
        assert_eq!(
            reg.add_with_family("fresh", fresh, Some("fam-9")),
            Added::Registered,
            "an unknown family id is unknown, never a match"
        );
        assert_eq!(reg.projects().len(), 2);
    }

    /// ★ The on-disk registry is name/path only, and it stays that way: a file
    /// written before this change round-trips byte for byte, and an unknown key
    /// is a config error rather than a silently dropped project.
    #[test]
    fn the_legacy_file_shape_round_trips_and_a_stray_key_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("projects.toml");
        std::fs::write(
            &path,
            "[[projects]]\nname = \"alpha\"\npath = \"/r/alpha\"\n",
        )
        .unwrap();

        let loaded = ProjectRegistry::load(&path).unwrap();
        assert_eq!(loaded.projects(), &[proj("alpha", "/r/alpha")]);
        loaded.save(&path).unwrap();
        assert_eq!(
            ProjectRegistry::load(&path).unwrap(),
            loaded,
            "a family-less row is written back without inventing a family key"
        );
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("family_id"),
            "and without the field at all"
        );

        let stray = dir.path().join("stray.toml");
        std::fs::write(
            &stray,
            "[[projects]]\nname = \"a\"\npath = \"/r/a\"\nlable = \"typo\"\n",
        )
        .unwrap();
        assert!(
            matches!(ProjectRegistry::load(&stray), Err(RiftError::Config(_))),
            "deny_unknown_fields: a typo is a refusal, not a dropped project"
        );
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
