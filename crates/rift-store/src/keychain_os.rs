//! The real OS keychain backend — an external-I/O boundary behind the `keyring`
//! feature (Security.framework on macOS, Credential Manager on Windows, kernel
//! keyutils on Linux).
//!
//! This file is **excluded from the coverage gate**: it can only be exercised
//! against a live keychain (the `#[ignore]` round-trip test below), so unit
//! coverage is not meaningful here. The *testable* envelope/seam logic lives in
//! [`crate::keychain`] and is fully covered via the in-memory backend.

use rift_core::{Result, RiftError};

use crate::keychain::KeychainBackend;

/// Credentials stored in the user's OS keychain.
pub struct OsKeychain;

impl KeychainBackend for OsKeychain {
    fn get(&self, service: &str, account: &str) -> Result<Option<Vec<u8>>> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| RiftError::crypto(format!("keychain entry: {e}")))?;
        match entry.get_secret() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(RiftError::crypto(format!("keychain get: {e}"))),
        }
    }

    fn set(&self, service: &str, account: &str, secret: &[u8]) -> Result<()> {
        let entry = keyring::Entry::new(service, account)
            .map_err(|e| RiftError::crypto(format!("keychain entry: {e}")))?;
        entry
            .set_secret(secret)
            .map_err(|e| RiftError::crypto(format!("keychain set: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::KeyStore;
    use crate::keychain::KeyringKeyStore;
    use rift_core::RepoId;

    // Runs only on a dev machine with a real keychain:
    //   cargo test -p rift-store --features keyring -- --ignored
    #[test]
    #[ignore = "requires a real OS keychain"]
    fn real_os_keychain_roundtrip() {
        let ks = KeyringKeyStore::with_service(OsKeychain, "rift-itest-DELETEME");
        let repo = RepoId::new("itest-repo").unwrap();
        let dek1 = ks.repo_dek(&repo).unwrap();
        let dek2 = ks.repo_dek(&repo).unwrap();
        assert_eq!(dek1.expose(), dek2.expose());
    }
}
