//! Where the rebuildable projection lives on disk.
//!
//! The per-repo projection is keyed by [`RepoId`] under a cache *root* chosen,
//! in order, from `RIFT_CACHE_DIR` (an explicit override — also what tests set
//! for hermeticity), `XDG_CACHE_HOME`, then `$HOME/.cache` on unix /
//! `%LOCALAPPDATA%` on Windows, falling back to `.`. This mirrors the
//! env-override discipline of `rift_registry::resolve_from`, so the projection
//! path is testable without ever touching the real home directory.

use std::path::{Path, PathBuf};

use rift_core::RepoId;

/// Resolve the cache *root* from an injected environment lookup.
#[must_use]
pub fn resolve_cache_root(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    env("RIFT_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| env("XDG_CACHE_HOME").map(PathBuf::from))
        .or_else(|| {
            if cfg!(windows) {
                env("LOCALAPPDATA").map(PathBuf::from)
            } else {
                env("HOME").map(|h| PathBuf::from(h).join(".cache"))
            }
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

/// The projection directory for `repo` under an explicit cache `root`:
/// `<root>/rift/<repo_id>/`.
#[must_use]
pub fn projection_dir_under(root: &Path, repo: &RepoId) -> PathBuf {
    root.join("rift").join(repo.as_str())
}

/// The projection database file for `repo` under an explicit cache `root`:
/// `<root>/rift/<repo_id>/projection.db`.
#[must_use]
pub fn projection_db_under(root: &Path, repo: &RepoId) -> PathBuf {
    projection_dir_under(root, repo).join("projection.db")
}

/// The projection directory for `repo`, resolving the cache root from `env`.
#[must_use]
pub fn projection_dir_from(env: impl Fn(&str) -> Option<String>, repo: &RepoId) -> PathBuf {
    projection_dir_under(&resolve_cache_root(env), repo)
}

/// The projection database file for `repo`, resolving the cache root from `env`.
#[must_use]
pub fn projection_db_from(env: impl Fn(&str) -> Option<String>, repo: &RepoId) -> PathBuf {
    projection_db_under(&resolve_cache_root(env), repo)
}

/// The default projection directory for this machine — [`projection_dir_from`]
/// over the real process environment. Typically `~/.cache/rift/<repo_id>/`.
#[must_use]
pub fn default_projection_dir(repo: &RepoId) -> PathBuf {
    projection_dir_from(|k| std::env::var(k).ok(), repo)
}

/// The default projection database file for this machine.
#[must_use]
pub fn default_projection_db(repo: &RepoId) -> PathBuf {
    projection_db_from(|k| std::env::var(k).ok(), repo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo() -> RepoId {
        RepoId::new("abc123").unwrap()
    }

    /// Build an env closure from a fixed table (None for anything absent).
    fn env_of(pairs: &[(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |k| {
            owned
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
        }
    }

    #[test]
    fn explicit_override_wins_and_layout_is_repo_scoped() {
        let root = resolve_cache_root(env_of(&[
            ("RIFT_CACHE_DIR", "/tmp/rc"),
            ("XDG_CACHE_HOME", "/tmp/xdg"),
            ("HOME", "/home/u"),
        ]));
        assert_eq!(root, PathBuf::from("/tmp/rc"));
        let db = projection_db_from(env_of(&[("RIFT_CACHE_DIR", "/tmp/rc")]), &repo());
        assert_eq!(db, PathBuf::from("/tmp/rc/rift/abc123/projection.db"));
    }

    #[test]
    fn xdg_then_home_fallback() {
        let xdg = resolve_cache_root(env_of(&[("XDG_CACHE_HOME", "/x"), ("HOME", "/home/u")]));
        assert_eq!(xdg, PathBuf::from("/x"));
        if !cfg!(windows) {
            let home = resolve_cache_root(env_of(&[("HOME", "/home/u")]));
            assert_eq!(home, PathBuf::from("/home/u/.cache"));
        }
    }

    #[test]
    fn empty_env_falls_back_to_dot() {
        assert_eq!(resolve_cache_root(env_of(&[])), PathBuf::from("."));
    }

    #[test]
    fn dir_and_db_share_the_repo_scoped_prefix() {
        let root = Path::new("/cache");
        let dir = projection_dir_under(root, &repo());
        let db = projection_db_under(root, &repo());
        assert!(db.starts_with(&dir));
        assert_eq!(dir, PathBuf::from("/cache/rift/abc123"));
    }
}
