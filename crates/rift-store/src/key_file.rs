//! File-backed custody of the per-family column key (the [`KeyStore`] that
//! ships).
//!
//! One 32-byte key per repository family, in its own file under a directory
//! the caller names — the machine's *config* root, never the cache root the
//! projection lives in. That split is the whole protection: a copy, backup or
//! sync of the cache without the config directory carries ciphertext and no
//! key. It does not protect against code running as the user, who can read
//! both.
//!
//! The OS keychain was rejected for this key. An ad-hoc-signed macOS binary
//! loses its keychain grant on every update and the next access prompts —
//! which would stall an agent hook waiting on a dialog — and Linux keyutils
//! forgets its entries at reboot. A hook can afford one small file read; it
//! cannot afford either of those.
//!
//! Creation is race-safe and never replaces a key: the bytes go to a
//! `create_new` temporary in the same directory, are fsynced, and are then
//! hard-linked to the final name, which fails if another process got there
//! first — whose key is then read instead. A torn write therefore never
//! leaves a short key under the real name.

use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rift_core::{RepoId, Result, RiftError};
use rift_crypto::{DataKey, KEY_LEN};

use crate::crypto::KeyStore;

/// Per-family key files under one directory, each loaded at most once per
/// instance.
///
/// Only successes are memoized: a key that could not be read is asked for
/// again on the next use, so a repaired file is picked up without a restart.
pub struct FileKeyStore {
    dir: PathBuf,
    loaded: Mutex<BTreeMap<String, DataKey>>,
}

impl FileKeyStore {
    /// Key files live directly under `dir`, one `<repo_id>.key` per family.
    #[must_use]
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            loaded: Mutex::new(BTreeMap::new()),
        }
    }

    /// The file holding `repo_id`'s key.
    #[must_use]
    pub fn key_path(&self, repo_id: &RepoId) -> PathBuf {
        self.dir.join(format!("{}.key", repo_id.as_str()))
    }
}

impl KeyStore for FileKeyStore {
    fn repo_dek(&self, repo_id: &RepoId) -> Result<DataKey> {
        // The lock is held across the load so two threads of one process
        // never mint two keys; the file I/O under it is one small read.
        let mut loaded = self
            .loaded
            .lock()
            .map_err(|_| RiftError::crypto("key file memo poisoned"))?;
        if let Some(key) = loaded.get(repo_id.as_str()) {
            return Ok(key.clone());
        }
        let key = load_or_mint_key_file(&self.key_path(repo_id))?;
        loaded.insert(repo_id.as_str().to_owned(), key.clone());
        Ok(key)
    }
}

/// The key at `path`, minting and durably recording a fresh random one when
/// the file does not exist.
///
/// A file that exists but is not exactly [`KEY_LEN`] bytes, or (on unix) is
/// readable by anyone but its owner, is refused rather than replaced: a key
/// is never overwritten, because rows encrypted under it would silently
/// become unreadable.
///
/// # Errors
/// [`RiftError::Crypto`] for a malformed or over-permissive key file, or an
/// unavailable RNG; [`RiftError::Io`] for a directory or file that cannot be
/// created or read. No error carries key bytes.
pub fn load_or_mint_key_file(path: &Path) -> Result<DataKey> {
    if let Some(key) = read_key_file(path)? {
        return Ok(key);
    }
    let dir = path
        .parent()
        .ok_or_else(|| RiftError::crypto("key file path has no directory"))?;
    ensure_private_dir(dir)?;
    let key = DataKey::generate()?;
    if publish_new_key(dir, path, &key)? {
        return Ok(key);
    }
    // Another process published first: its key is the family's key.
    read_key_file(path)?.ok_or_else(|| RiftError::crypto("key file vanished after a race"))
}

/// The key at `path`, or `None` when no file is there.
fn read_key_file(path: &Path) -> Result<Option<DataKey>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    refuse_shared_file(&file, path)?;
    let mut bytes = [0u8; KEY_LEN];
    let mut extra = [0u8; 1];
    file.read_exact(&mut bytes).map_err(|_| malformed(path))?;
    if file.read(&mut extra)? != 0 {
        return Err(malformed(path));
    }
    Ok(Some(DataKey::from_bytes(bytes)))
}

fn malformed(path: &Path) -> RiftError {
    RiftError::crypto(format!(
        "column key file {} is not a {KEY_LEN}-byte key; it was left as it is",
        path.display()
    ))
}

/// Write `key` to a fresh temporary beside `path`, fsync it, and link it to
/// `path` without replacing anything. `false` when `path` already existed.
fn publish_new_key(dir: &Path, path: &Path, key: &DataKey) -> Result<bool> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| RiftError::crypto("key file path has no name"))?;
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    // A temporary left by a crashed process that had this pid is stale.
    let _ = std::fs::remove_file(&tmp);
    let written = write_private_file(&tmp, key);
    let linked = written.and_then(|()| std::fs::hard_link(&tmp, path).map_err(Into::into));
    let _ = std::fs::remove_file(&tmp);
    match linked {
        Ok(()) => {
            sync_dir(dir);
            Ok(true)
        }
        Err(RiftError::Io(e)) if e.kind() == ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e),
    }
}

fn write_private_file(path: &Path, key: &DataKey) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(key.expose())?;
    file.sync_all()?;
    Ok(())
}

/// Create `dir` (owner-only on unix) and narrow it to the owner if it was
/// already there wider. On Windows the user profile's ACL is the boundary.
fn ensure_private_dir(dir: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        builder.mode(0o700);
        builder.create(dir)?;
        let mode = std::fs::metadata(dir)?.permissions().mode();
        if mode & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    #[cfg(not(unix))]
    builder.create(dir)?;
    Ok(())
}

/// Refuse a key file anyone but its owner can read or write.
#[cfg(unix)]
fn refuse_shared_file(file: &File, path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = file.metadata()?.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(RiftError::crypto(format!(
            "column key file {} is open to other users (mode {:o}); \
             run `chmod 600` on it",
            path.display(),
            mode & 0o777
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn refuse_shared_file(_file: &File, _path: &Path) -> Result<()> {
    Ok(())
}

/// Make the new directory entry durable. Best effort: not every platform
/// can open a directory for syncing, and the file itself is already synced.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(handle) = File::open(dir) {
        let _ = handle.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(id: &str) -> RepoId {
        RepoId::new(id).unwrap()
    }

    #[test]
    fn a_missing_key_is_minted_once_and_read_back_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("keys").join("fam.key");
        let first = load_or_mint_key_file(&path).unwrap();
        let second = load_or_mint_key_file(&path).unwrap();
        assert_eq!(first.expose(), second.expose());
        assert_eq!(std::fs::read(&path).unwrap(), first.expose());
        let leftovers: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(leftovers, vec![std::ffi::OsString::from("fam.key")]);
    }

    #[cfg(unix)]
    #[test]
    fn a_minted_key_is_owner_only_in_an_owner_only_directory() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("keys");
        let path = dir.join("fam.key");
        load_or_mint_key_file(&path).unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&dir), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn a_key_directory_left_wider_is_narrowed_to_its_owner() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("keys");
        std::fs::create_dir(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        load_or_mint_key_file(&dir.join("fam.key")).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn a_key_file_other_users_can_read_is_refused_and_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("fam.key");
        std::fs::write(&path, [7u8; KEY_LEN]).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let Err(RiftError::Crypto(reason)) = load_or_mint_key_file(&path) else {
            panic!("an over-permissive key file must be refused");
        };
        assert!(reason.contains("chmod 600"));
        assert_eq!(std::fs::read(&path).unwrap(), [7u8; KEY_LEN]);
    }

    #[test]
    fn a_short_or_long_key_file_is_refused_and_never_replaced() {
        let tmp = tempfile::tempdir().unwrap();
        for (name, bytes) in [("short.key", vec![1u8; 5]), ("long.key", vec![1u8; 33])] {
            let path = tmp.path().join(name);
            write_private_file_bytes(&path, &bytes);
            assert!(matches!(
                load_or_mint_key_file(&path),
                Err(RiftError::Crypto(_))
            ));
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
    }

    #[test]
    fn a_key_published_first_by_another_process_wins_the_race() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("fam.key");
        write_private_file_bytes(&path, &[9u8; KEY_LEN]);
        let mine = DataKey::from_bytes([1u8; KEY_LEN]);
        assert!(!publish_new_key(tmp.path(), &path, &mine).unwrap());
        assert_eq!(
            load_or_mint_key_file(&path).unwrap().expose(),
            &[9u8; KEY_LEN]
        );
    }

    #[test]
    fn the_store_keys_each_family_by_its_own_file_and_remembers_it() {
        let tmp = tempfile::tempdir().unwrap();
        let store = FileKeyStore::new(tmp.path().join("keys"));
        let a = store.repo_dek(&repo("fam-a")).unwrap();
        let b = store.repo_dek(&repo("fam-b")).unwrap();
        assert_ne!(a.expose(), b.expose());
        assert!(store.key_path(&repo("fam-a")).ends_with("keys/fam-a.key"));
        // Memoized: the file is read once, so removing it does not change the
        // key this process already holds.
        std::fs::remove_file(store.key_path(&repo("fam-a"))).unwrap();
        assert_eq!(store.repo_dek(&repo("fam-a")).unwrap().expose(), a.expose());
    }

    #[test]
    fn a_path_with_no_directory_is_refused() {
        assert!(load_or_mint_key_file(Path::new("")).is_err());
    }

    fn write_private_file_bytes(path: &Path, bytes: &[u8]) {
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }
}
