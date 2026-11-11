//! Where the rebuildable projection lives on disk.
//!
//! The per-repo projection is keyed by [`RepoId`] under a cache *root* chosen,
//! in order, from `RIFT_CACHE_DIR` (an explicit override — also what tests set
//! for hermeticity), `XDG_CACHE_HOME`, then `$HOME/.cache` on unix /
//! `%LOCALAPPDATA%` on Windows, and never the working directory: with none of
//! them set, Rift's binaries refuse (see [`configured_cache_root`]). This mirrors the
//! env-override discipline of `rift_registry::resolve_from`, so the projection
//! path is testable without ever touching the real home directory.

use std::path::{Path, PathBuf};

use rift_core::{FamilyIds, RepoId};

/// What a process should say when [`configured_cache_root`] has no answer.
pub const CACHE_ROOT_UNSET: &str =
    "no cache directory: set HOME (or LOCALAPPDATA on Windows) or RIFT_CACHE_DIR";

/// The root [`resolve_cache_root`] answers when the environment names none:
/// a path under a device file, so every directory or store created under it
/// fails instead of landing somewhere. Rift's binaries refuse before reaching
/// it (they ask [`configured_cache_root`] first); the old fallback, `.`, put
/// projections and the install salt in whatever repository a cron job or a
/// service happened to run in.
///
/// Windows has no device file that refuses a child: `\\.\NUL\…` answers
/// `create_dir_all` with success. The interior NUL byte there is refused by
/// the standard library (and by SQLite's C string) before any system call.
const UNRESOLVED_CACHE_ROOT: &str = if cfg!(windows) {
    "\\\\.\\NUL\\rift-cache-unset\0"
} else {
    "/dev/null/rift-cache-unset"
};

/// The cache *root* the environment names, or `None` when it names none:
/// `RIFT_CACHE_DIR`, `XDG_CACHE_HOME`, then `$HOME/.cache` on unix /
/// `%LOCALAPPDATA%` on Windows. An empty value counts as unset, because an
/// empty path resolves against the process's directory.
#[must_use]
pub fn configured_cache_root(env: impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let set = |key: &str| env(key).filter(|value| !value.is_empty());
    set("RIFT_CACHE_DIR")
        .map(PathBuf::from)
        .or_else(|| set("XDG_CACHE_HOME").map(PathBuf::from))
        .or_else(|| {
            if cfg!(windows) {
                set("LOCALAPPDATA").map(PathBuf::from)
            } else {
                set("HOME").map(|h| PathBuf::from(h).join(".cache"))
            }
        })
}

/// Resolve the cache *root* from an injected environment lookup:
/// [`configured_cache_root`], or a root nothing can be created under.
#[must_use]
pub fn resolve_cache_root(env: impl Fn(&str) -> Option<String>) -> PathBuf {
    configured_cache_root(env).unwrap_or_else(|| PathBuf::from(UNRESOLVED_CACHE_ROOT))
}

/// Restrict a projection to its owner: `0o700` on the containing directory and
/// `0o600` on the DB file (unix). This is the filesystem leg of the residual-
/// exposure mitigation for non-encrypted columns (the local-disk-reader
/// adversary); SQLite creates `-wal`/`-shm` sidecars with the DB file's mode,
/// so they inherit it. On Windows, `%LOCALAPPDATA%` is already scoped to the
/// user profile's ACL; a tighter ACL story is a later hardening item.
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

/// The recall projection database for `repo` under an explicit cache
/// `root`: `<root>/rift/<repo_id>/recall.db`. A **separate** file from
/// [`projection_db_under`] so `rift-memd rebuild` can destroy only the recall
/// projection, leaving the capture projection untouched.
#[must_use]
pub fn recall_db_under(root: &Path, repo: &RepoId) -> PathBuf {
    projection_dir_under(root, repo).join("recall.db")
}

/// Transient client-connection state for `repo` under an explicit cache
/// `root`. This is deliberately outside the worktree; heartbeat leases must
/// never dirty a repository.
///
/// The final path segment stays at its historical spelling: nothing writes
/// this directory any more (the transport whose leases lived here is retired),
/// and the one surviving reader — `rift-agent-install`'s connection-lease
/// probe — must keep finding leases a previous install already wrote. The
/// segment is on-disk read-compat, not vocabulary.
#[must_use]
pub fn connection_state_dir_under(root: &Path, repo: &RepoId) -> PathBuf {
    projection_dir_under(root, repo).join("mcp")
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

// --- the family-keyed product paths -----------------------------------------
//
// These are the functions a product path calls. Everything below them —
// `recall_repo_id`, `recall_db_for_repo`, `capture_repo_id`,
// `capture_projection_db_for_repo`, `connection_state_dir_for_repo` — derives a
// store from *one checkout's path*, which is the rule the family identity
// replaced: a linked worktree is the same repository, and deriving from its
// path gave it a second, empty projection and a second, empty recall store.
// They stay public for adoption (the mint reads them over the resolved main
// work tree), for `rift doctor` (which reports drift between the recorded ids
// and these), and for tests — but a product path must never call them.

/// The capture projection database for a **family**, under an explicit cache
/// `root`: `<root>/rift/<family_id>/projection.db`.
///
/// Every member of the family — the main work tree and every linked worktree —
/// resolves to the same file. That is the whole family rule in one line.
#[must_use]
pub fn projection_db_for_family_under(root: &Path, family: &impl FamilyIds) -> PathBuf {
    projection_db_under(root, family.family_id())
}

/// The capture projection database for a family, resolving the cache root from
/// the real environment. The product path's [`capture_projection_db_for_repo`].
#[must_use]
pub fn projection_db_for_family(family: &impl FamilyIds) -> PathBuf {
    default_projection_db(family.family_id())
}

/// The recall projection database for a **family**, under an explicit cache
/// `root`: `<root>/rift/<recall_id>/recall.db`.
///
/// Keyed by the family's *recall* id, never its capture id: the two id spaces
/// are domain-separated and reading one store under the other's key opens a
/// different, empty file — the recorded dashboard defect this signature makes
/// unspellable.
#[must_use]
pub fn recall_db_for_family_under(root: &Path, family: &impl FamilyIds) -> PathBuf {
    recall_db_under(root, family.recall_id())
}

/// The recall projection database for a family, resolving the cache root from
/// the real environment. The product path's [`recall_db_for_repo`].
#[must_use]
pub fn recall_db_for_family(family: &impl FamilyIds) -> PathBuf {
    recall_db_from(|k| std::env::var(k).ok(), family.recall_id())
}

/// **Adoption/doctor only** — a product path resolves its store through
/// [`recall_db_for_family`].
///
/// The canonical **recall** repo id for a worktree path: the first 16 bytes of
/// `sha256(canonical_path)` as hex. This is the single authority both `rift-memd`
/// (which reads recall.db) and `rift hooks memd` (which appends the serve-audit
/// trail) resolve, so they never disagree on which `recall.db` a repo owns.
///
/// The hash lives in [`rift_core::id::legacy::recall_slug`]; this function owns
/// the [`std::fs::canonicalize`] step (falling back to the given path when it
/// cannot be resolved, which is the historical behaviour a deployed store may
/// already sit under).
///
/// # Errors
/// [`rift_core::RiftError`] if [`RepoId::new`] rejects the derived slug (it never
/// does — the slug is 32 lowercase hex chars).
pub fn recall_repo_id(repo: &Path) -> rift_core::Result<RepoId> {
    let abs = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    RepoId::new(rift_core::id::legacy::recall_slug(&abs))
}

/// **Adoption/doctor only** — see [`recall_db_for_family`].
///
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

/// Transient client-connection-state directory for a worktree, resolving the
/// cache root from the real environment. See
/// [`connection_state_dir_under`] for why its last path segment is frozen.
///
/// # Errors
/// Propagates [`recall_repo_id`].
pub fn connection_state_dir_for_repo(repo: &Path) -> rift_core::Result<PathBuf> {
    Ok(connection_state_dir_under(
        &resolve_cache_root(|k| std::env::var(k).ok()),
        &recall_repo_id(repo)?,
    ))
}

/// **Adoption/doctor only** — a product path resolves its store through
/// [`projection_db_for_family`].
///
/// The canonical **capture** repo id for a worktree path — the derivation the
/// `rift` binary uses for `projection.db` (a domain-separated leaf hash, distinct
/// from [`recall_repo_id`]'s plain digest). Exposed here so the recall side can
/// locate the capture projection (e.g. the identity plane's `session_binding`
/// rows that populate the developer scope axis) without re-deriving ad hoc.
///
/// The hash lives in [`rift_core::id::legacy::capture_slug`] — the same bytes
/// `rift_git::repo_id_for` mints. A test in this module,
/// `capture_slug_is_the_leaf_hash_derivation`, pins it against
/// `rift_hashchain::leaf_hash`, the spelling this call site grew up with.
///
/// # Errors
/// [`rift_core::RiftError`] if [`RepoId::new`] rejects the derived slug (it never
/// does — the slug is 32 lowercase hex chars).
pub fn capture_repo_id(repo: &Path) -> rift_core::Result<RepoId> {
    let abs = std::fs::canonicalize(repo).unwrap_or_else(|_| repo.to_path_buf());
    RepoId::new(rift_core::id::legacy::capture_slug(&abs))
}

/// **Adoption/doctor only** — see [`projection_db_for_family`].
///
/// The capture projection database file for a worktree `repo` path, resolving
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
    fn an_environment_naming_no_root_is_refused_and_never_the_working_directory() {
        assert_eq!(configured_cache_root(env_of(&[])), None);
        assert_eq!(
            configured_cache_root(env_of(&[("RIFT_CACHE_DIR", ""), ("HOME", "")])),
            None,
            "an empty value would resolve against the process's directory"
        );
        let fallback = resolve_cache_root(env_of(&[]));
        assert!(fallback.is_absolute() || cfg!(windows), "{fallback:?}");
        assert!(
            std::fs::create_dir_all(fallback.join("rift")).is_err(),
            "nothing can be created under the unresolved root"
        );
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

    /// The capture slug moved into `rift-core` so `rift-git` and `rift-store`
    /// could stop deriving it twice. This pins the move: the shared function
    /// must produce the same bytes as the `leaf_hash` spelling this call site
    /// used before, for arbitrary path bytes — the guarantee that keeps every
    /// deployed `~/.cache/rift/<slug>/projection.db` findable.
    #[test]
    fn capture_slug_is_the_leaf_hash_derivation() {
        for raw in ["/", "/tmp/x", "/a b/c-d", "/Users/x/Documents/rift"] {
            let path = Path::new(raw);
            let shared = rift_core::id::legacy::capture_slug(path);
            let historical = rift_hashchain::leaf_hash(path.to_string_lossy().as_bytes()).to_hex();
            assert_eq!(
                shared,
                historical.get(..32).unwrap(),
                "capture slug drifted from the leaf-hash derivation for {raw}"
            );
        }
    }

    /// The recall slug likewise: the first 16 digest bytes of the plain
    /// SHA-256 over the path, hex — no domain byte, which is what keeps it a
    /// different id space from the capture slug for the same checkout.
    #[test]
    fn recall_slug_is_the_undomained_digest() {
        use sha2::{Digest as _, Sha256};
        for raw in ["/", "/tmp/x", "/a b/c-d"] {
            let path = Path::new(raw);
            let historical = format!("{:x}", Sha256::digest(path.to_string_lossy().as_bytes()));
            assert_eq!(
                rift_core::id::legacy::recall_slug(path),
                historical.get(..32).unwrap()
            );
        }
    }

    /// A stand-in for `rift_git::Family`. `rift-store` cannot name that type —
    /// it does not depend on `rift-git`, and the workspace graph stays acyclic
    /// — and does not need to: the contract is the trait.
    struct Fam(RepoId, RepoId);

    impl FamilyIds for Fam {
        fn family_id(&self) -> &RepoId {
            &self.0
        }
        fn recall_id(&self) -> &RepoId {
            &self.1
        }
    }

    /// The family's two stores are keyed by its two ids — never one id for
    /// both. Opening `recall.db` under the capture id is the recorded dashboard
    /// defect; these signatures are what make it unspellable.
    #[test]
    fn family_paths_key_each_store_by_its_own_id() {
        let fam = Fam(RepoId::new("cap0").unwrap(), RepoId::new("rec0").unwrap());
        let root = Path::new("/cache");
        assert_eq!(
            projection_db_for_family_under(root, &fam),
            PathBuf::from("/cache/rift/cap0/projection.db")
        );
        assert_eq!(
            recall_db_for_family_under(root, &fam),
            PathBuf::from("/cache/rift/rec0/recall.db")
        );
        // The env-resolving spellings agree with the explicit-root ones under
        // the same root, and each ends at its own file.
        assert!(projection_db_for_family(&fam).ends_with("projection.db"));
        assert!(recall_db_for_family(&fam).ends_with("recall.db"));
        assert!(projection_db_for_family(&fam)
            .to_string_lossy()
            .contains("cap0"));
        assert!(recall_db_for_family(&fam)
            .to_string_lossy()
            .contains("rec0"));
    }

    /// Every member of a family resolves to the SAME two files — the whole
    /// family rule in one assertion. The ids come from the manifest, so two
    /// members holding the same record cannot disagree about where their
    /// stores are, whatever their own paths hash to.
    #[test]
    fn two_members_of_one_family_resolve_to_the_same_stores() {
        let main = Fam(RepoId::new("cap1").unwrap(), RepoId::new("rec1").unwrap());
        let member = Fam(RepoId::new("cap1").unwrap(), RepoId::new("rec1").unwrap());
        let root = Path::new("/cache");
        assert_eq!(
            projection_db_for_family_under(root, &main),
            projection_db_for_family_under(root, &member)
        );
        assert_eq!(
            recall_db_for_family_under(root, &main),
            recall_db_for_family_under(root, &member)
        );
        // And the two stores stay distinct files: adoption does not merge them.
        assert_ne!(
            projection_db_for_family_under(root, &main),
            recall_db_for_family_under(root, &main)
        );
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
