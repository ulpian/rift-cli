//! Capture-time identity wiring (phase-04 step-03): provision the per-install
//! salt, observe the local identity, and bind projection sessions to a
//! developer. Best-effort by contract — called from the never-fail hook path,
//! so every error degrades to "no binding this time" (the next hook retries;
//! all writes are idempotent).
//!
//! Salt custody: the OS keychain (`rift`/`install-salt`) when available; a
//! `0o600` file under the machine-global cache root as the documented fallback
//! for headless machines (the salt is an unlinkability nonce, not a key); a
//! zero salt as the last resort (ids remain hashed, just unsalted).

use std::path::Path;

use rift_core::{binding_evidence, identity_snapshot, Timestamp};
use rift_git::GitConfigIdentity;
use rift_store::{
    record_identity_snapshot, record_session_binding, unbound_session_ids, KeychainBackend,
    SqliteStore,
};

const SALT_SERVICE: &str = "rift";
const SALT_ACCOUNT: &str = "install-salt";
const SALT_FILE: &str = "install-salt";

/// Load (or mint + store) the per-install identity salt from `backend`.
fn salt_from_keychain(backend: &impl KeychainBackend) -> Option<[u8; 32]> {
    if let Ok(Some(bytes)) = backend.get(SALT_SERVICE, SALT_ACCOUNT) {
        return bytes.as_slice().try_into().ok();
    }
    let mut salt = [0u8; 32];
    getrandom::getrandom(&mut salt).ok()?;
    backend.set(SALT_SERVICE, SALT_ACCOUNT, &salt).ok()?;
    Some(salt)
}

/// Fallback custody: a `0o600` salt file under the machine-global cache root.
fn salt_from_file(cache_root: &Path) -> Option<[u8; 32]> {
    let path = cache_root.join(SALT_FILE);
    if let Ok(bytes) = std::fs::read(&path) {
        return bytes.as_slice().try_into().ok();
    }
    let mut salt = [0u8; 32];
    getrandom::getrandom(&mut salt).ok()?;
    std::fs::create_dir_all(cache_root).ok()?;
    std::fs::write(&path, salt).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Some(salt)
}

/// The per-install salt: keychain → file fallback → zero (documented order).
/// `RIFT_NO_KEYCHAIN` skips straight to the file fallback — the OS keychain
/// can pop an interactive authorization prompt for a freshly-built binary,
/// which a headless run (CI, gate scripts, agents) can never answer.
pub(crate) fn install_salt() -> [u8; 32] {
    if std::env::var_os("RIFT_NO_KEYCHAIN").is_none() {
        if let Some(salt) = salt_from_keychain(&rift_store::OsKeychain) {
            return salt;
        }
    }
    let root = rift_store::resolve_cache_root(|k| std::env::var(k).ok());
    salt_from_file(&root).unwrap_or([0u8; 32])
}

/// Observe the local identity for `work_tree` and bind every projection
/// session that has no binding yet. Idempotent; best-effort (never fails the
/// hook — the caller ignores the `Result`). The `salt` is injected (callers
/// pass [`install_salt`]) so no test can reach the OS keychain.
pub(crate) async fn bind_new_sessions(
    store: &SqliteStore,
    work_tree: &Path,
    salt: [u8; 32],
    now_ms: i64,
) -> rift_core::Result<()> {
    let unbound = unbound_session_ids(store).await?;
    if unbound.is_empty() {
        return Ok(());
    }
    let src = GitConfigIdentity::new(work_tree, salt);
    let at = Timestamp::from_millis(now_ms);
    record_identity_snapshot(store, &identity_snapshot(&src, at)).await?;
    let evidence = binding_evidence(&src, at);
    for session_id in unbound {
        record_session_binding(store, &session_id, &evidence).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::{apply_migrations, default_migrations, CanonicalStore, MemoryKeychain, Value};

    #[tokio::test]
    async fn binds_unbound_sessions_idempotently() {
        let store = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&store, &default_migrations()).unwrap();
        store
            .insert(
                "session",
                &[
                    "session_id",
                    "agent",
                    "model_primary",
                    "repo_id",
                    "started_at",
                    "schema_version",
                ],
                &[
                    Value::text("sess_1"),
                    Value::text("claude_code"),
                    Value::text("m"),
                    Value::text("repo-1"),
                    Value::Int(1),
                    Value::Int(1),
                ],
            )
            .await
            .unwrap();

        let tmp = tempfile::tempdir().unwrap();
        // Salt injected: the test must never reach the OS keychain (a fresh
        // binary triggers an interactive ACL prompt that hangs headless runs).
        bind_new_sessions(&store, tmp.path(), [7u8; 32], 1)
            .await
            .unwrap();
        assert_eq!(store.count("session_binding").await.unwrap(), 1);
        assert_eq!(store.count("developer_identity_snapshot").await.unwrap(), 1);
        // Re-running binds nothing new (idempotent across hook invocations).
        bind_new_sessions(&store, tmp.path(), [7u8; 32], 2)
            .await
            .unwrap();
        assert_eq!(store.count("session_binding").await.unwrap(), 1);
    }

    #[test]
    fn keychain_salt_is_stable_across_loads() {
        let kc = MemoryKeychain::new();
        let a = salt_from_keychain(&kc).unwrap();
        let b = salt_from_keychain(&kc).unwrap();
        assert_eq!(a, b, "second load must reuse the stored salt");
    }

    #[test]
    fn file_salt_is_stable_and_owner_only() {
        let tmp = tempfile::tempdir().unwrap();
        let a = salt_from_file(tmp.path()).unwrap();
        let b = salt_from_file(tmp.path()).unwrap();
        assert_eq!(a, b);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(tmp.path().join(SALT_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "salt file must be owner-only");
        }
    }
}
