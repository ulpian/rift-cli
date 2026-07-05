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

/// Restrict a projection to its owner: `0o700` on the containing directory and
/// `0o600` on the DB file (unix). This is the filesystem leg of the residual-
/// exposure mitigation for non-encrypted columns (phase-03 step-05 adversary
/// #3, the local disk reader); SQLite creates `-wal`/`-shm` sidecars with the
/// DB file's mode, so they inherit it. On Windows, `%LOCALAPPDATA%` is already
/// scoped to the user profile's ACL; a tighter ACL story is Stage-6 hardening.
///
/// # Errors
/// [`std::io::Error`] if permissions cannot be applied to an existing path.
pub fn harden_projection_permissions(db_path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Some(dir) = db_path.parent() {
            if dir.exists() {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
            }
        }
        if db_path.exists() {
            std::fs::set_permissions(db_path, std::fs::Permissions::from_mode(0o600))?;
        }
    }
    #[cfg(not(unix))]
    {
        let _ = db_path;
    }
    Ok(())
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

/// The Stage 3 recall projection database for `repo` under an explicit cache
/// `root`: `<root>/rift/<repo_id>/recall.db`. A **separate** file from
/// [`projection_db_under`] so `rift-memd rebuild` can destroy only the recall
/// projection, leaving the M1 capture projection untouched.
#[must_use]
pub fn recall_db_under(root: &Path, repo: &RepoId) -> PathBuf {
    projection_dir_under(root, repo).join("recall.db")
}

/// The recall projection database for `repo`, resolving the cache root from `env`.
#[must_use]
pub fn recall_db_from(env: impl Fn(&str) -> Option<String>, repo: &RepoId) -> PathBuf {
    recall_db_under(&resolve_cache_root(env), repo)
}

/// The default recall projection database file for this machine — typically
/// `~/.cache/rift/<repo_id>/recall.db`.
#[must_use]
pub fn default_recall_db(repo: &RepoId) -> PathBuf {
    recall_db_from(|k| std::env::var(k).ok(), repo)
}

/// The canonical **recall** repo id for a worktree path: the first 16 bytes of
/// `sha256(canonical_path)` as hex. This is the single authority both `rift-memd`
/// (which reads recall.db) and `rift hooks memd` (which appends the serve-audit
/// trail) resolve, so they never disagree on which `recall.db` a repo owns.
///
/// # Errors
/// [`rift_core::RiftError`] if [`RepoId::new`] rejects the derived slug (it never
/// does — the slug is 32 lowercase hex chars).
pub fn recall_repo_id(repo: &Path) -> rift_core::Result<RepoId> {
    use sha2::{Digest, Sha256};
    let abs = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let digest = Sha256::digest(abs.to_string_lossy().as_bytes());
    let mut hex = String::with_capacity(32);
    use core::fmt::Write as _;
    for b in digest.iter().take(16) {
        let _ = write!(hex, "{b:02x}");
    }
    RepoId::new(hex)
}

/// The recall projection database file for a worktree `repo` path, resolving both
/// the [`recall_repo_id`] and the cache root from the real environment.
///
/// # Errors
/// Propagates [`recall_repo_id`].
pub fn recall_db_for_repo(repo: &Path) -> rift_core::Result<PathBuf> {
    Ok(recall_db_from(
        |k| std::env::var(k).ok(),
        &recall_repo_id(repo)?,
    ))
}

/// The canonical **capture** repo id for a worktree path — the derivation the
/// `rift` binary uses for `projection.db` (a domain-separated leaf hash, distinct
/// from [`recall_repo_id`]'s plain digest). Exposed here so the recall side can
/// locate the M1 capture projection (e.g. the identity plane's `session_binding`
/// rows that populate the developer scope axis) without re-deriving ad hoc.
///
/// # Errors
/// [`rift_core::RiftError`] if [`RepoId::new`] rejects the derived slug (it never
/// does — the slug is 32 lowercase hex chars).
pub fn capture_repo_id(repo: &Path) -> rift_core::Result<RepoId> {
    let abs = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    let slug = rift_hashchain::leaf_hash(abs.to_string_lossy().as_bytes()).to_hex();
    RepoId::new(slug.get(..32).unwrap_or(&slug))
}

/// The M1 capture projection database file for a worktree `repo` path, resolving
/// both the [`capture_repo_id`] and the cache root from the real environment.
///
/// # Errors
/// Propagates [`capture_repo_id`].
pub fn capture_projection_db_for_repo(repo: &Path) -> rift_core::Result<PathBuf> {
    Ok(projection_db_from(
        |k| std::env::var(k).ok(),
        &capture_repo_id(repo)?,
    ))
}

/// The machine-global canopy UI-state database under an explicit cache `root`:
/// `<root>/rift/canopy/ui-state.db`. **Repo-independent** (the dashboard spans
/// repos), so it lives under a fixed `canopy/` prefix rather than a `<repo_id>/`.
#[must_use]
pub fn canopy_ui_db_under(root: &Path) -> PathBuf {
    root.join("rift").join("canopy").join("ui-state.db")
}

/// The canopy UI-state database, resolving the cache root from `env`.
#[must_use]
pub fn canopy_ui_db_from(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    canopy_ui_db_under(&resolve_cache_root(env))
}

/// The default canopy UI-state database for this machine — typically
/// `~/.cache/rift/canopy/ui-state.db`.
#[must_use]
pub fn default_canopy_ui_db() -> PathBuf {
    canopy_ui_db_from(|k| std::env::var(k).ok())
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

    #[test]
    fn from_helpers_and_machine_defaults_are_exercised() {
        assert_eq!(
            projection_dir_from(env_of(&[("RIFT_CACHE_DIR", "/c")]), &repo()),
            PathBuf::from("/c/rift/abc123")
        );
        // default_* read the real process env; exercise them (path is machine-dependent).
        assert!(default_projection_dir(&repo()).ends_with(PathBuf::from("rift").join("abc123")));
        assert!(default_projection_db(&repo()).ends_with("projection.db"));
    }

    #[test]
    fn recall_repo_id_is_stable_and_recall_db_for_repo_ends_in_recall_db() {
        // Deterministic 32-hex slug over the canonical path (shared authority for
        // rift-memd and `rift hooks memd`, so they agree on the recall.db).
        let a = recall_repo_id(Path::new(".")).unwrap();
        let b = recall_repo_id(Path::new(".")).unwrap();
        assert_eq!(a.as_str(), b.as_str());
        assert_eq!(a.as_str().len(), 32);
        assert!(a.as_str().chars().all(|c| c.is_ascii_hexdigit()));
        // A non-existent path still yields an id (canonicalize falls back).
        assert_eq!(
            recall_repo_id(Path::new("/no/such/dir/xyz"))
                .unwrap()
                .as_str()
                .len(),
            32
        );
        // The resolved db path is repo-scoped and named recall.db.
        let db = recall_db_for_repo(Path::new(".")).unwrap();
        assert!(db.ends_with("recall.db"));
        assert!(db.to_string_lossy().contains(a.as_str()));
    }

    #[test]
    fn capture_repo_id_is_stable_and_distinct_from_recall_id() {
        // The capture derivation (leaf hash) and the recall derivation (plain
        // digest) are deliberately distinct id spaces; both are stable 32-hex.
        let a = capture_repo_id(Path::new(".")).unwrap();
        let b = capture_repo_id(Path::new(".")).unwrap();
        assert_eq!(a.as_str(), b.as_str());
        assert_eq!(a.as_str().len(), 32);
        assert_ne!(
            a.as_str(),
            recall_repo_id(Path::new(".")).unwrap().as_str(),
            "capture and recall id spaces are domain-separated"
        );
        let db = capture_projection_db_for_repo(Path::new(".")).unwrap();
        assert!(db.ends_with("projection.db"));
        assert!(db.to_string_lossy().contains(a.as_str()));
    }

    #[test]
    fn canopy_ui_db_is_machine_global_not_repo_scoped() {
        // Repo-independent: a fixed `canopy/` prefix, never a `<repo_id>/`.
        assert_eq!(
            canopy_ui_db_from(env_of(&[("RIFT_CACHE_DIR", "/c")])),
            PathBuf::from("/c/rift/canopy/ui-state.db")
        );
        assert_eq!(
            canopy_ui_db_under(Path::new("/cache")),
            PathBuf::from("/cache/rift/canopy/ui-state.db")
        );
        assert!(default_canopy_ui_db().ends_with("ui-state.db"));
    }
}
