//! The one constructor of the cipher that opens the recall projection's
//! encrypted columns.
//!
//! Every production path in this binary that writes or reads a sensitive
//! column (today `digest_blob.bytes`, the bound summary and outline bodies)
//! takes its cipher from here; the library crates take a `&dyn ColumnCipher`
//! from their caller and never construct one. `xtask ci-meta` holds the
//! no-key `PlainCipher` out of this crate's and the recall crates' production
//! code, so a new call site cannot quietly write plaintext.
//!
//! **Custody.** One random 32-byte key per repository family, keyed by the
//! family's recall id (the id that keys `recall.db`), in
//! `<config-root>/rift/keys/<recall_id>.key` — the machine's *config* root,
//! not the cache root the projection lives under. A copy, backup or sync of
//! the cache without the config directory is ciphertext without its key. An
//! attacker running as the user can read both; this is not a defence against
//! one.
//!
//! **Cost on a hook.** The cipher is built without touching the key: the key
//! file is read on the first encrypt or decrypt, once per process (the
//! process-wide [`FileKeyStore`] memoizes it). A hook that never opens a
//! summary reads nothing.
//!
//! **Failure is a miss.** A key that cannot be read or minted makes every
//! encrypt fail (the binding is not written — never written in plaintext
//! instead) and every encrypted read fail, which the ledger turns into a
//! degraded, unserved summary. Rows written plain-framed before this existed
//! still read. A key file that disappears is minted afresh; the rows sealed
//! under the old key become misses until the projection is rebuilt.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use rift_core::{Result, RiftError};
use rift_git::Family;
use rift_store::{ColumnCipher, DekCipher, FileKeyStore};

/// The cipher for one family's recall projection, or the refusal a caller
/// with no resolvable family gets.
pub(crate) enum ColumnKey {
    /// Keyed by the family's file-held key, loaded on first use.
    Family(DekCipher<&'static FileKeyStore>),
    /// No family identity could be resolved, so there is no key to use:
    /// nothing is written and nothing encrypted is read.
    Unresolved,
}

/// The cipher for `family`'s recall projection.
pub(crate) fn column_cipher(family: &Family) -> ColumnKey {
    ColumnKey::Family(DekCipher::new(keys(), family.recall_id.clone()))
}

/// The cipher for the family the checkout at `root` belongs to.
pub(crate) fn column_cipher_at(root: &Path) -> ColumnKey {
    match crate::family::try_at(root) {
        Some(family) => column_cipher(&family),
        None => ColumnKey::Unresolved,
    }
}

impl ColumnCipher for ColumnKey {
    fn encrypt(
        &self,
        table: &str,
        column: &str,
        row_id: &str,
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        match self {
            Self::Family(cipher) => cipher.encrypt(table, column, row_id, plaintext),
            Self::Unresolved => Err(unresolved(table, column)),
        }
    }

    fn decrypt(&self, table: &str, column: &str, row_id: &str, stored: &[u8]) -> Result<Vec<u8>> {
        match self {
            Self::Family(cipher) => cipher.decrypt(table, column, row_id, stored),
            Self::Unresolved => Err(unresolved(table, column)),
        }
    }
}

fn unresolved(table: &str, column: &str) -> RiftError {
    RiftError::crypto(format!(
        "{table}.{column}: no repository family resolved, so no column key"
    ))
}

/// The process-wide key store: one per process, so a key file is read once
/// however many ciphers the process builds.
fn keys() -> &'static FileKeyStore {
    static KEYS: OnceLock<FileKeyStore> = OnceLock::new();
    KEYS.get_or_init(|| FileKeyStore::new(key_root()))
}

/// `<config-root>/rift/keys`: beside the machine's project registry, through
/// the same config-dir resolver, and never under the cache root.
#[cfg(not(test))]
fn key_root() -> PathBuf {
    rift_registry::default_registry_path().with_file_name("keys")
}

/// The test build's key root: one directory per test process under the
/// system temp dir, never the developer's real config — the same reasoning
/// as `family::cache_root`'s test body.
#[cfg(test)]
fn key_root() -> PathBuf {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        tempfile::Builder::new()
            .prefix("rift-test-keys-")
            .tempdir_in(std::env::temp_dir())
            .expect("the test process's key root is created under the system temp dir")
            .keep()
            .join("keys")
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_ledger::validity_store::{current_summary, put_summary, CurrentSummary};
    use rift_recall_serve::Validity;
    use rift_store::{CanonicalStore, KeyStore, SqliteStore};

    const SOURCE: &[u8] = b"fn alpha() {}\n";
    const DIGEST: &[u8] = b"SECRET-DIGEST-SENTINEL";

    fn family_store(tmp: &Path) -> (Family, SqliteStore) {
        rift_git::test_support::fake_repo(tmp).unwrap();
        let family = crate::family::resolve_at(tmp).unwrap();
        let db = crate::family::recall_db_of(&family);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = rift_store::open_recall_projection(&db).unwrap();
        (family, store)
    }

    fn block<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(f)
    }

    async fn stored_digest(store: &SqliteStore) -> Vec<u8> {
        let rows = store
            .query("SELECT bytes FROM digest_blob", &[])
            .await
            .unwrap();
        rows[0].get_blob(0).unwrap()
    }

    fn served(store: &SqliteStore, cipher: &dyn ColumnCipher) -> CurrentSummary {
        let current = rift_shadow::sha256_hex(&[SOURCE]);
        block(current_summary(
            store,
            cipher,
            "a.rs",
            Some(&current),
            rift_recall_serve::outline_versions(rift_recall_policy::SERVE_POLICY_VERSION),
        ))
        .unwrap()
    }

    fn drop_store(family: &Family) {
        let db = crate::family::recall_db_of(family);
        let _ = std::fs::remove_dir_all(db.parent().unwrap());
    }

    /// The production write path stores ciphertext: the stored digest is not
    /// plain-framed, does not contain its plaintext, and reads back through
    /// the same constructor.
    #[test]
    fn a_digest_written_through_the_production_cipher_is_ciphertext_at_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let (family, store) = family_store(tmp.path());
        let cipher = column_cipher_at(tmp.path());
        block(put_summary(
            &store,
            &cipher,
            "a.rs",
            SOURCE,
            rift_recall_serve::outline_versions(rift_recall_policy::SERVE_POLICY_VERSION),
            DIGEST,
        ))
        .unwrap();

        let stored = block(stored_digest(&store));
        assert!(!rift_store::is_plain_framed(&stored));
        assert!(!stored.windows(DIGEST.len()).any(|w| w == DIGEST));
        let CurrentSummary::Bound(Validity::Valid(summary)) = served(&store, &cipher) else {
            panic!("the summary written under the family key must serve");
        };
        assert_eq!(summary.digest(), DIGEST);
        assert!(keys().key_path(&family.recall_id).is_file());
        drop_store(&family);
    }

    /// A row written plain-framed before keys existed still serves.
    #[test]
    fn a_plain_framed_row_from_before_key_custody_still_serves() {
        let tmp = tempfile::tempdir().unwrap();
        let (family, store) = family_store(tmp.path());
        block(put_summary(
            &store,
            &rift_store::PlainCipher,
            "a.rs",
            SOURCE,
            rift_recall_serve::outline_versions(rift_recall_policy::SERVE_POLICY_VERSION),
            DIGEST,
        ))
        .unwrap();
        let served = served(&store, &column_cipher(&family));
        assert!(matches!(served, CurrentSummary::Bound(Validity::Valid(_))));
        drop_store(&family);
    }

    /// A lost key file is minted afresh, and what the old key sealed becomes a
    /// miss — degraded, carrying no bytes — rather than an error or garbage.
    #[test]
    fn rows_sealed_under_a_lost_key_are_misses_under_the_new_one() {
        let tmp = tempfile::tempdir().unwrap();
        let (family, store) = family_store(tmp.path());
        let dir = tmp.path().join("keys");
        let before = FileKeyStore::new(dir.clone());
        let old = DekCipher::new(&before, family.recall_id.clone());
        block(put_summary(
            &store,
            &old,
            "a.rs",
            SOURCE,
            rift_recall_serve::outline_versions(rift_recall_policy::SERVE_POLICY_VERSION),
            DIGEST,
        ))
        .unwrap();
        std::fs::remove_file(before.key_path(&family.recall_id)).unwrap();

        let after = FileKeyStore::new(dir);
        let fresh = DekCipher::new(&after, family.recall_id.clone());
        assert!(matches!(
            served(&store, &fresh),
            CurrentSummary::Bound(Validity::Degraded { .. })
        ));
        assert!(after.repo_dek(&family.recall_id).is_ok());
        drop_store(&family);
    }

    /// With no family there is no key: nothing is sealed, nothing opened.
    #[test]
    fn an_unresolved_family_writes_nothing_and_reads_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let cipher = column_cipher_at(tmp.path());
        assert!(matches!(cipher, ColumnKey::Unresolved));
        assert!(cipher.encrypt("digest_blob", "bytes", "r", b"x").is_err());
        assert!(cipher.decrypt("digest_blob", "bytes", "r", b"\0x").is_err());
    }
}
