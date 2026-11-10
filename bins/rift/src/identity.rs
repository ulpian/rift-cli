//! Capture-time identity wiring: provision the per-install
//! salt, observe the local identity, and bind projection sessions to a
//! developer. Best-effort by contract — called from the never-fail hook path,
//! so every error degrades to "no binding this time" (the next hook retries;
//! all writes are idempotent).
//!
//! Salt custody: the OS keychain (`rift`/`install-salt`) when available; a
//! `0o600` file under `<cache root>/rift/` as the documented fallback
//! for headless machines (the salt is an unlinkability nonce, not a key); a
//! zero salt as the last resort (ids remain hashed, just unsalted).

use std::path::Path;

use rift_core::identity::{IdentitySource as _, MachineId};
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

/// Rift's own directory under the machine-global cache root, beside the
/// family projections. The salt and the machine-id cache once sat at the
/// cache root itself (`~/.cache`, `%LOCALAPPDATA%`), two generic names among
/// every other program's files; they are read there once more to move them.
const STATE_DIR: &str = "rift";

/// Fallback custody: a `0o600` salt file under Rift's directory of the
/// machine-global cache root.
///
/// The salt is never re-minted while one exists: every salted identity
/// already recorded depends on it. So a salt at the old location is moved,
/// not replaced, and the new file is renamed into place before the old one
/// goes — a concurrent hook that finds the old one gone finds the new one on
/// its second look rather than minting a third.
fn salt_from_file(cache_root: &Path) -> Option<[u8; 32]> {
    let dir = cache_root.join(STATE_DIR);
    let path = dir.join(SALT_FILE);
    if let Some(salt) = read_salt(&path) {
        return Some(salt);
    }
    let legacy = cache_root.join(SALT_FILE);
    if let Some(salt) = read_salt(&legacy) {
        if write_owner_only(&dir, SALT_FILE, &salt) {
            let _ = std::fs::remove_file(&legacy);
        }
        return Some(salt);
    }
    if let Some(salt) = read_salt(&path) {
        return Some(salt);
    }
    let mut salt = [0u8; 32];
    getrandom::getrandom(&mut salt).ok()?;
    write_owner_only(&dir, SALT_FILE, &salt).then_some(salt)
}

/// A salt file's 32 bytes; anything else is no salt.
fn read_salt(path: &Path) -> Option<[u8; 32]> {
    std::fs::read(path).ok()?.as_slice().try_into().ok()
}

/// Write `name` under `dir`: a temp file made owner-only, then renamed into
/// place, so no reader sees it partial or wider than `0o600`. Best-effort.
fn write_owner_only(dir: &Path, name: &str, body: &[u8]) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let tmp = dir.join(format!("{name}.tmp"));
    if std::fs::write(&tmp, body).is_err() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, dir.join(name)).is_ok()
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
    salt_from_file(&crate::family::cache_root()).unwrap_or([0u8; 32])
}

/// The machine-id cache file under the machine-global cache root.
const MACHINE_ID_FILE: &str = "machine-id.json";
/// The cache's contract version.
const MACHINE_ID_SCHEMA: u32 = 1;
/// The HKDF label the salt tag is derived under.
const MACHINE_ID_TAG_LABEL: &str = "rift.machine-id-cache.v1";

/// What the cache file holds. **Never the raw hardware id**: `machine_id` is
/// the salted blake3 digest `rift-core` derives (the only machine-shaped
/// value the projection ever stores either), and `salt_tag` is an HKDF tag
/// of the install salt it was derived under — enough to refuse a cache made
/// under another install, not enough to recover the salt.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct MachineIdCache {
    schema: u32,
    salt_tag: String,
    machine_id: String,
}

/// The salted machine id for this install: from the cache under `root` when
/// it holds one made under `salt`, else derived once by `derive` and cached.
///
/// Why a cache at all: the derivation reads the platform fingerprint, which
/// on macOS is an `ioreg -rd1 -c IOPlatformExpertDevice` child process — the
/// one spawn `identity-attribution` still counted on every session's first
/// hook. The digest it feeds does not change between hooks, so it is derived
/// once per install and read from a file after that (settled 2026-08-30).
/// The cached value is exactly what the derivation would
/// produce, so the binding evidence of a session bound before this cache
/// existed equals the evidence of one bound after.
///
/// Every failure degrades to `derive`: a missing, unreadable, corrupt or
/// foreign-salt file is replaced (atomically, owner-only) by a fresh
/// derivation; a file that cannot be written costs one derivation per hook,
/// which is what every hook paid before. `None` from `derive` (a platform
/// with no fingerprint) is never cached — absence is re-asked, not remembered.
fn cached_machine_id(
    root: &Path,
    salt: &[u8; 32],
    derive: impl FnOnce() -> Option<MachineId>,
) -> Option<MachineId> {
    let tag =
        rift_core::Blake3Id::from_bytes(rift_crypto::salt_tag(salt, MACHINE_ID_TAG_LABEL)).to_hex();
    let dir = root.join(STATE_DIR);
    if let Some(hit) = read_machine_id_cache(&dir.join(MACHINE_ID_FILE), &tag) {
        return Some(hit);
    }
    // The old location, read once: a hit moves it, so the fingerprint is not
    // read again just because the file changed directories.
    let legacy = root.join(MACHINE_ID_FILE);
    let id = match read_machine_id_cache(&legacy, &tag) {
        Some(hit) => hit,
        None => derive()?,
    };
    write_machine_id_cache(&dir, &tag, id);
    let _ = std::fs::remove_file(&legacy);
    Some(id)
}

/// The cached id, if the file parses, is this schema, and was made under the
/// salt `tag` names. Anything else is a miss.
fn read_machine_id_cache(path: &Path, tag: &str) -> Option<MachineId> {
    let bytes = std::fs::read(path).ok()?;
    let cache: MachineIdCache = serde_json::from_slice(&bytes).ok()?;
    if cache.schema != MACHINE_ID_SCHEMA || cache.salt_tag != tag {
        return None;
    }
    rift_core::Blake3Id::parse_hex(&cache.machine_id)
        .ok()
        .map(MachineId)
}

/// Write the cache: temp file + rename, owner-only. Best-effort — a failure
/// here changes nothing but the next hook's cost.
fn write_machine_id_cache(dir: &Path, tag: &str, id: MachineId) {
    let Ok(body) = serde_json::to_vec(&MachineIdCache {
        schema: MACHINE_ID_SCHEMA,
        salt_tag: tag.to_owned(),
        machine_id: id.0.to_hex(),
    }) else {
        return;
    };
    let _ = write_owner_only(dir, MACHINE_ID_FILE, &body);
}

/// Derive (or read back) this install's machine id now, so a session's first
/// hook does not pay the fingerprint read. The `rift hooks install` warm-up.
/// Returns whether an id is cached afterwards.
pub(crate) fn warm_machine_id(salt: &[u8; 32]) -> bool {
    let probe = GitConfigIdentity::new(".", *salt);
    cached_machine_id(&crate::family::cache_root(), salt, || probe.machine_id()).is_some()
}

/// Observe the local identity for `work_tree` and bind every projection
/// session that has no binding yet. Idempotent; best-effort (never fails the
/// hook — the caller ignores the `Result`).
///
/// The salt arrives as a **thunk**, not a value, and that is a latency
/// contract rather than a style choice: [`install_salt`] is an OS keychain
/// round-trip, measured at **16–28 ms** inside `post-commit` against a 100 ms
/// git-latency budget, and on all but the first commit of a session there is
/// nothing to bind — so the old signature paid the keychain to learn it had no
/// work. Taking a closure moves that read behind the `unbound.is_empty()`
/// check, where it belongs. Callers still inject it, so no test reaches the
/// real keychain.
pub(crate) async fn bind_new_sessions(
    store: &SqliteStore,
    work_tree: &Path,
    salt: impl FnOnce() -> [u8; 32],
    now_ms: i64,
) -> rift_core::Result<()> {
    let unbound = unbound_session_ids(store).await?;
    if unbound.is_empty() {
        return Ok(());
    }
    let salt = salt();
    let src = GitConfigIdentity::new(work_tree, salt);
    // The machine id from the cache when it has one, else derived by this
    // very source (its fingerprint read, memoized) and cached for the next.
    let src = match cached_machine_id(&crate::family::cache_root(), &salt, || src.machine_id()) {
        Some(id) => src.with_machine_id(id),
        None => src,
    };
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
        bind_new_sessions(&store, tmp.path(), || [7u8; 32], 1)
            .await
            .unwrap();
        assert_eq!(store.count("session_binding").await.unwrap(), 1);
        assert_eq!(store.count("developer_identity_snapshot").await.unwrap(), 1);
        // Re-running binds nothing new (idempotent across hook invocations).
        bind_new_sessions(&store, tmp.path(), || [7u8; 32], 2)
            .await
            .unwrap();
        assert_eq!(store.count("session_binding").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn nothing_to_bind_never_reads_the_salt() {
        // The whole point of the thunk. On a projection with no unbound
        // session — every commit after a session's first — the OS keychain
        // must not be touched at all: it cost 16-28 ms of a 100 ms hook budget
        // to answer a question the projection had already answered.
        let store = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&store, &default_migrations()).unwrap();
        let tmp = tempfile::tempdir().unwrap();

        let mut reads = 0_u32;
        bind_new_sessions(
            &store,
            tmp.path(),
            || {
                reads += 1;
                [7u8; 32]
            },
            1,
        )
        .await
        .unwrap();

        assert_eq!(reads, 0, "no unbound session must not cost a salt read");
        assert_eq!(store.count("session_binding").await.unwrap(), 0);
    }

    /// A fixed "raw fingerprint" and the id `rift-core` derives from it —
    /// the test's stand-in for the platform read.
    const RAW: &str = "RAW-HARDWARE-UUID-0000";
    fn derived(salt: &[u8; 32]) -> MachineId {
        rift_core::identity::derive_machine_id(salt, RAW)
    }

    /// The cache's whole contract: a miss derives exactly once and writes;
    /// a hit derives nothing; nothing raw is on disk; the file is owner-only.
    #[test]
    fn a_cache_hit_derives_nothing_and_a_miss_derives_once() {
        let tmp = tempfile::tempdir().unwrap();
        let salt = [5u8; 32];
        let mut derivations = 0;
        let mut ask = || {
            cached_machine_id(tmp.path(), &salt, || {
                derivations += 1;
                Some(derived(&salt))
            })
        };
        assert_eq!(ask(), Some(derived(&salt)));
        assert_eq!(ask(), Some(derived(&salt)));
        assert_eq!(ask(), Some(derived(&salt)));
        assert_eq!(derivations, 1, "one derivation per install, not per hook");

        let path = tmp.path().join(STATE_DIR).join(MACHINE_ID_FILE);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains(RAW), "the raw id is never on disk: {body}");
        assert!(
            !body.contains(&rift_core::Blake3Id::from_bytes(salt).to_hex()),
            "nor the salt: {body}"
        );
        assert!(body.contains(&derived(&salt).0.to_hex()), "{body}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "owner-only");
        }
    }

    /// A corrupt file, a foreign-salt file and a future schema are all misses
    /// that derive again and are replaced — never trusted, never fatal.
    #[test]
    fn a_corrupt_or_foreign_cache_falls_back_to_derivation() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join(STATE_DIR).join(MACHINE_ID_FILE);
        std::fs::create_dir_all(tmp.path().join(STATE_DIR)).unwrap();
        let salt = [5u8; 32];
        let count = |body: &str| {
            std::fs::write(&path, body).unwrap();
            let mut derivations = 0;
            let got = cached_machine_id(tmp.path(), &salt, || {
                derivations += 1;
                Some(derived(&salt))
            });
            assert_eq!(got, Some(derived(&salt)));
            derivations
        };
        assert_eq!(count("not json"), 1, "corrupt");
        assert_eq!(count(""), 1, "empty");
        assert_eq!(
            count(r#"{"schema":1,"salt_tag":"00","machine_id":"zz"}"#),
            1,
            "unparseable id"
        );
        // A cache made under ANOTHER install's salt is a miss, and the
        // rewrite carries this salt's tag.
        cached_machine_id(tmp.path(), &[6u8; 32], || Some(derived(&[6u8; 32])));
        let mut derivations = 0;
        assert_eq!(
            cached_machine_id(tmp.path(), &salt, || {
                derivations += 1;
                Some(derived(&salt))
            }),
            Some(derived(&salt))
        );
        assert_eq!(derivations, 1, "foreign salt");
        let mut derivations = 0;
        cached_machine_id(tmp.path(), &salt, || {
            derivations += 1;
            Some(derived(&salt))
        });
        assert_eq!(derivations, 0, "and now it is this install's cache");
        // A newer schema is not read by this build.
        let mut v: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["schema"] = serde_json::json!(2);
        assert_eq!(count(&v.to_string()), 1, "future schema");
    }

    /// A platform with no fingerprint caches nothing: absence is asked again,
    /// never remembered as an id.
    #[test]
    fn an_absent_fingerprint_is_never_cached() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(cached_machine_id(tmp.path(), &[1u8; 32], || None), None);
        assert!(!tmp.path().join(STATE_DIR).join(MACHINE_ID_FILE).exists());
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
            let mode = std::fs::metadata(tmp.path().join(STATE_DIR).join(SALT_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "salt file must be owner-only");
        }
    }

    /// A salt at the old, un-namespaced location is the install's salt: it is
    /// moved under Rift's directory byte for byte, never re-minted, and the
    /// cache root is left without it.
    #[test]
    fn a_salt_at_the_old_location_moves_under_rifts_directory_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let old = [9u8; 32];
        std::fs::write(tmp.path().join(SALT_FILE), old).unwrap();

        assert_eq!(salt_from_file(tmp.path()), Some(old));

        assert!(!tmp.path().join(SALT_FILE).exists(), "the old file is gone");
        assert_eq!(
            std::fs::read(tmp.path().join(STATE_DIR).join(SALT_FILE)).unwrap(),
            old
        );
        assert_eq!(salt_from_file(tmp.path()), Some(old), "and read from there");
    }

    /// The machine-id cache moves the same way, and a hit at the old location
    /// derives nothing.
    #[test]
    fn a_machine_id_cache_at_the_old_location_moves_without_a_derivation() {
        let tmp = tempfile::tempdir().unwrap();
        let salt = [5u8; 32];
        cached_machine_id(tmp.path(), &salt, || Some(derived(&salt)));
        let moved = tmp.path().join(STATE_DIR).join(MACHINE_ID_FILE);
        std::fs::rename(&moved, tmp.path().join(MACHINE_ID_FILE)).unwrap();

        let mut derivations = 0;
        let got = cached_machine_id(tmp.path(), &salt, || {
            derivations += 1;
            Some(derived(&salt))
        });

        assert_eq!(got, Some(derived(&salt)));
        assert_eq!(derivations, 0);
        assert!(moved.is_file());
        assert!(!tmp.path().join(MACHINE_ID_FILE).exists());
    }
}
