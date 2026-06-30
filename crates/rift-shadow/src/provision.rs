//! P1 B2 — capture-backend provisioning orchestration + audit.
//!
//! Thin orchestration over the wrapper's [`PinnedSubstrate::ensure_installed`]
//! (which does the fetch → hash-verify → atomic install) plus the append-only
//! `substrate_install` audit row. Takes the fetcher behind the [`SubstrateFetcher`]
//! seam, so the production HTTP fetcher and an in-memory test fixture are
//! interchangeable. Idempotent: a valid on-disk binary performs no network I/O,
//! and the same verified hash is a no-op insert.

use rift_core::{Result, RiftError};
use rift_shadow_entire::{InstallReport, PinnedSubstrate, SubstrateError, SubstrateFetcher};
use rift_store::{
    record_substrate_block_event, record_substrate_install, CanonicalStore, SubstrateBlockEventRow,
    SubstrateInstallRow,
};

use crate::sha256_hex;

/// Map a wrapper error into a Rift-voiced error that never names the upstream tool
/// and carries no key/transcript bytes (only the seam's own Rift-voiced message).
fn map_err(e: SubstrateError) -> RiftError {
    RiftError::storage(format!("capture backend provisioning failed: {e}"))
}

/// Provision the pinned backend and record the append-only `substrate_install`
/// audit row.
///
/// # Errors
/// [`RiftError::Storage`] if provisioning fails (integrity/unavailable/platform)
/// or the audit insert fails.
pub async fn provision_and_audit(
    pin: &PinnedSubstrate,
    fetcher: &impl SubstrateFetcher,
    store: &impl CanonicalStore,
    now_ms: i64,
) -> Result<InstallReport> {
    let report = pin.ensure_installed(fetcher).map_err(map_err)?;
    let sha_hex = report.substrate_sha256.to_hex();
    let row = SubstrateInstallRow {
        // Content-addressed: same verified hash → same id → idempotent no-op.
        install_id: sha256_hex(&[sha_hex.as_bytes()]),
        substrate_sha256: sha_hex,
        rift_release: report.rift_release.clone(),
        install_root: report.install_root.display().to_string(),
        verified_at: now_ms,
        schema_version: 1,
    };
    record_substrate_install(store, &row).await?;
    Ok(report)
}

/// Record a default-deny refusal of a wrapped invocation (P1 S3 audit). `verb` is
/// the refused root token only — never key/transcript bytes, never the brand.
///
/// # Errors
/// [`RiftError::Storage`] on insert failure.
pub async fn record_block(
    store: &impl CanonicalStore,
    verb: &str,
    reason: &str,
    now_ms: i64,
) -> Result<()> {
    let row = SubstrateBlockEventRow {
        block_id: sha256_hex(&[
            verb.as_bytes(),
            b"\0",
            reason.as_bytes(),
            b"\0",
            now_ms.to_string().as_bytes(),
        ]),
        verb: verb.to_owned(),
        reason: reason.to_owned(),
        occurred_at: now_ms,
        schema_version: 1,
    };
    record_substrate_block_event(store, &row).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_store::SqliteStore;
    use std::cell::Cell;

    /// An in-memory fetcher returning fixed bytes; counts calls (idempotency guard).
    struct MemFetcher {
        bytes: Vec<u8>,
        calls: Cell<usize>,
    }
    impl SubstrateFetcher for MemFetcher {
        fn fetch(&self, _url: &str) -> std::result::Result<Vec<u8>, SubstrateError> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.bytes.clone())
        }
    }

    fn hex_of(bytes: &[u8]) -> String {
        crate::sha256_hex(&[bytes])
    }

    fn pin_for(bytes: &[u8], root: std::path::PathBuf) -> PinnedSubstrate {
        let manifest = format!(
            "schema = 1\nsubstrate_version = \"1.2.3\"\nrift_release = \"0.1.0\"\n\
             [[asset]]\nos = \"linux\"\narch = \"x86_64\"\nurl = \"https://x/y\"\nsha256 = \"{}\"\n",
            hex_of(bytes)
        );
        PinnedSubstrate::from_manifest_str(&manifest, "linux", "x86_64", root).unwrap()
    }

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    #[tokio::test]
    async fn provision_records_audit_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"the verified backend bytes".to_vec();
        let pin = pin_for(&bytes, dir.path().to_path_buf());
        let fetcher = MemFetcher {
            bytes: bytes.clone(),
            calls: Cell::new(0),
        };
        let s = store();

        let report = provision_and_audit(&pin, &fetcher, &s, 100).await.unwrap();
        assert!(report.binary.exists());
        assert_eq!(s.count("substrate_install").await.unwrap(), 1);

        // Second run: valid binary present → no refetch; same hash → no new row.
        let report2 = provision_and_audit(&pin, &fetcher, &s, 200).await.unwrap();
        assert_eq!(report2.substrate_sha256, report.substrate_sha256);
        assert_eq!(fetcher.calls.get(), 1, "idempotent: no second fetch");
        assert_eq!(
            s.count("substrate_install").await.unwrap(),
            1,
            "same hash → no new row"
        );
    }

    #[tokio::test]
    async fn block_event_is_audited() {
        let s = store();
        record_block(&s, "auth", "forbidden", 5).await.unwrap();
        record_block(&s, "auth", "forbidden", 5).await.unwrap(); // same → idempotent
        assert_eq!(s.count("substrate_block_event").await.unwrap(), 1);
    }
}
