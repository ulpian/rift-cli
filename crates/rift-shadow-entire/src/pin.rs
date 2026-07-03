//! P1 S1 — silent install & version pinning.
//!
//! Vendor a **single, exact** capture-backend build per Rift release, verify it by
//! SHA-256 against a value baked into the binary (`pinned-substrate.toml`,
//! `include_str!`-embedded), and install it into a Rift-private directory that is
//! **never** on `PATH`, under an **opaque** name. This is the supply-chain root of
//! the wrapper: the backend we exec is exactly the one we pinned, or we don't run.
//!
//! The single network call (the download) is injected via the [`SubstrateFetcher`]
//! seam: tests use an in-memory fixture; the production [`HttpFetcher`](crate::HttpFetcher)
//! (behind the `provision-http` feature) downloads the pinned upstream release
//! archive from GitHub and extracts the binary. The embedded manifest pins real
//! per-platform URLs + the SHA-256 of the **extracted binary**, so a download whose
//! bytes do not match is **never** written executable (fail-closed). `rift setup`
//! provisions by default.
//!
//! No upstream brand appears in any string this module can emit.

use std::path::{Path, PathBuf};

use rift_core::Sha256Id;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::SubstrateError;

/// The embedded pin manifest. `include_str!` so an on-disk edit cannot trick the
/// running wrapper.
const EMBEDDED_MANIFEST: &str = include_str!("../pinned-substrate.toml");

/// Opaque on-disk name for the backend binary — a curious `ls` never reveals the
/// upstream tool.
const OPAQUE_BIN_NAME: &str = "rift-substrate";

#[derive(Debug, Deserialize)]
struct Manifest {
    #[allow(dead_code)] // parsed for forward-compat; pinned to 1 today
    schema: u32,
    substrate_version: String,
    rift_release: String,
    #[serde(default)]
    asset: Vec<RawAsset>,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    os: String,
    arch: String,
    url: String,
    sha256: String,
}

/// One pinned asset (download + verified hash) for a specific OS/arch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubstrateAsset {
    /// Target OS (`macos` | `linux` | `windows`).
    pub os: String,
    /// Target arch (`aarch64` | `x86_64`).
    pub arch: String,
    /// Release-archive download URL (the only network call in the wrapper).
    pub url: String,
    /// The verified SHA-256 the extracted binary must match.
    pub sha256: Sha256Id,
}

/// The one capture-backend build this Rift release is allowed to run.
#[derive(Debug, Clone)]
pub struct PinnedSubstrate {
    /// Upstream version (never logged at user level).
    pub substrate_version: String,
    /// The Rift release this pin shipped with.
    pub rift_release: String,
    /// The asset for the resolved OS/arch.
    pub asset: SubstrateAsset,
    /// Private install root (off `PATH`, `0700`).
    pub install_root: PathBuf,
}

/// The outcome of a successful provision — enough for the caller to record an
/// append-only `substrate_install` audit row. The audit lives in `rift-store`; the
/// wrapper itself stays free of a storage dependency.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    /// Absolute path to the verified, installed binary.
    pub binary: PathBuf,
    /// The verified SHA-256 (content-addressed identity of the build).
    pub substrate_sha256: Sha256Id,
    /// Upstream version provisioned.
    pub substrate_version: String,
    /// The Rift release this pin shipped with.
    pub rift_release: String,
    /// The private install root.
    pub install_root: PathBuf,
}

/// A source of capture-binary bytes. Injected so tests use a local fixture and
/// production uses the HTTP download-and-extract ([`HttpFetcher`](crate::HttpFetcher)) —
/// the **only** network call in the wrapper.
pub trait SubstrateFetcher {
    /// Fetch (download + extract) the capture binary at the archive `url` once.
    ///
    /// # Errors
    /// [`SubstrateError::Unavailable`] if the bytes cannot be retrieved.
    fn fetch(&self, url: &str) -> Result<Vec<u8>, SubstrateError>;
}

fn sha256_of(bytes: &[u8]) -> Sha256Id {
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut got = [0u8; 32];
    got.copy_from_slice(&out);
    Sha256Id::from_bytes(got)
}

/// Lowercase-hex SHA-256 of `bytes` — the exact digest the pin verifies with.
/// Exported for the scripted pin-cut (`xtask pin-cut`), which must hash archives
/// and extracted binaries with the same code path the installer uses.
#[must_use]
pub fn sha256_hex_of(bytes: &[u8]) -> String {
    sha256_of(bytes).to_hex()
}

fn parse_manifest(s: &str) -> Result<Manifest, SubstrateError> {
    toml::from_str(s).map_err(|_| SubstrateError::Unavailable)
}

fn pick_asset(m: &Manifest, os: &str, arch: &str) -> Result<SubstrateAsset, SubstrateError> {
    let raw = m
        .asset
        .iter()
        .find(|a| a.os == os && a.arch == arch)
        .ok_or(SubstrateError::Platform)?;
    Ok(SubstrateAsset {
        os: raw.os.clone(),
        arch: raw.arch.clone(),
        url: raw.url.clone(),
        // A malformed pinned hash is a build-time error, surfaced as Integrity.
        sha256: Sha256Id::parse_hex(&raw.sha256).map_err(|_| SubstrateError::Integrity)?,
    })
}

/// Resolve the private, off-`PATH` data root for backend installs.
#[must_use]
fn default_install_root() -> PathBuf {
    #[cfg(windows)]
    {
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            return PathBuf::from(local).join("rift").join("substrate");
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
            return PathBuf::from(xdg).join("rift").join("substrate");
        }
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("rift")
                .join("substrate");
        }
    }
    std::env::temp_dir().join("rift").join("substrate")
}

impl PinnedSubstrate {
    /// Resolve the pin for the **current** OS/arch from the embedded manifest,
    /// installing under the default private data root.
    ///
    /// # Errors
    /// [`SubstrateError::Platform`] if the current platform has no pinned asset,
    /// [`SubstrateError::Integrity`] if a pinned hash is malformed,
    /// [`SubstrateError::Unavailable`] if the embedded manifest cannot be parsed.
    pub fn from_embedded() -> Result<Self, SubstrateError> {
        Self::from_manifest_str(
            EMBEDDED_MANIFEST,
            std::env::consts::OS,
            std::env::consts::ARCH,
            default_install_root(),
        )
    }

    /// Resolve a pin from an explicit manifest + OS/arch + install root (testable).
    ///
    /// # Errors
    /// As [`PinnedSubstrate::from_embedded`].
    pub fn from_manifest_str(
        manifest: &str,
        os: &str,
        arch: &str,
        install_root: PathBuf,
    ) -> Result<Self, SubstrateError> {
        let m = parse_manifest(manifest)?;
        let asset = pick_asset(&m, os, arch)?;
        Ok(Self {
            substrate_version: m.substrate_version,
            rift_release: m.rift_release,
            asset,
            install_root,
        })
    }

    /// The absolute path to the opaque binary for this pin:
    /// `<install_root>/<version>/<arch>/bin/rift-substrate[.exe]`. Never on `PATH`.
    #[must_use]
    pub fn binary_path(&self) -> PathBuf {
        let mut p = self
            .install_root
            .join(&self.substrate_version)
            .join(&self.asset.arch)
            .join("bin")
            .join(OPAQUE_BIN_NAME);
        if self.asset.os == "windows" {
            p.set_extension("exe");
        }
        p
    }

    /// Re-hash an on-disk binary and confirm it matches the pinned SHA-256.
    ///
    /// # Errors
    /// [`SubstrateError::Unavailable`] if the file cannot be read,
    /// [`SubstrateError::Integrity`] on hash mismatch.
    pub fn verify_on_disk(&self, p: &Path) -> Result<(), SubstrateError> {
        let bytes = std::fs::read(p).map_err(|_| SubstrateError::Unavailable)?;
        if sha256_of(&bytes) == self.asset.sha256 {
            Ok(())
        } else {
            Err(SubstrateError::Integrity)
        }
    }

    fn report(&self, binary: PathBuf) -> InstallReport {
        InstallReport {
            binary,
            substrate_sha256: self.asset.sha256,
            substrate_version: self.substrate_version.clone(),
            rift_release: self.rift_release.clone(),
            install_root: self.install_root.clone(),
        }
    }

    /// Idempotently provision + verify the pinned binary, returning an
    /// [`InstallReport`] the caller can audit. A present, valid binary performs
    /// **no** fetch; a tampered/partial one is removed and reinstalled; bytes that
    /// fail the hash check are **never** written executable.
    ///
    /// # Errors
    /// [`SubstrateError::Integrity`] on a hash mismatch (bytes discarded),
    /// [`SubstrateError::Unavailable`] on fetch/IO failure.
    pub fn ensure_installed(
        &self,
        fetcher: &impl SubstrateFetcher,
    ) -> Result<InstallReport, SubstrateError> {
        let path = self.binary_path();

        // Fast path: a present, valid binary needs no network.
        if path.exists() {
            match self.verify_on_disk(&path) {
                Ok(()) => return Ok(self.report(path)),
                Err(_) => {
                    // Tampered or partial — remove and reinstall.
                    let _ = std::fs::remove_file(&path);
                }
            }
        }

        // The one network call. Hash BEFORE the bytes are ever marked executable.
        let bytes = fetcher.fetch(&self.asset.url)?;
        if sha256_of(&bytes) != self.asset.sha256 {
            return Err(SubstrateError::Integrity);
        }

        let dir = path.parent().ok_or(SubstrateError::Unavailable)?;
        std::fs::create_dir_all(dir).map_err(|_| SubstrateError::Unavailable)?;
        set_private_dir(dir);

        // Write to a temp path, fsync, then atomically rename into place.
        let tmp = dir.join(concat!(".", "rift-substrate", ".tmp"));
        write_then_sync(&tmp, &bytes)?;
        set_private_exe(&tmp);
        std::fs::rename(&tmp, &path).map_err(|_| SubstrateError::Unavailable)?;

        // Defense in depth: re-verify the final on-disk bytes.
        self.verify_on_disk(&path)?;
        Ok(self.report(path))
    }
}

fn write_then_sync(path: &Path, bytes: &[u8]) -> Result<(), SubstrateError> {
    use std::io::Write as _;
    let mut f = std::fs::File::create(path).map_err(|_| SubstrateError::Unavailable)?;
    f.write_all(bytes)
        .map_err(|_| SubstrateError::Unavailable)?;
    f.sync_all().map_err(|_| SubstrateError::Unavailable)?;
    Ok(())
}

#[cfg(unix)]
fn set_private_dir(dir: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn set_private_dir(_dir: &Path) {}

#[cfg(unix)]
fn set_private_exe(path: &Path) {
    use std::os::unix::fs::PermissionsExt as _;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
}
#[cfg(not(unix))]
fn set_private_exe(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixture fetcher returning fixed bytes; records call count.
    struct MemFetcher {
        bytes: Vec<u8>,
        calls: std::cell::Cell<usize>,
    }
    impl SubstrateFetcher for MemFetcher {
        fn fetch(&self, _url: &str) -> Result<Vec<u8>, SubstrateError> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.bytes.clone())
        }
    }

    /// A fetcher that must never be called (idempotency guard).
    struct PanicFetcher;
    impl SubstrateFetcher for PanicFetcher {
        fn fetch(&self, _url: &str) -> Result<Vec<u8>, SubstrateError> {
            panic!("fetch must not be called when a valid binary is present");
        }
    }

    fn manifest_with(hash_hex: &str) -> String {
        format!(
            "schema = 1\n\
             substrate_version = \"1.2.3\"\n\
             rift_release = \"0.1.0\"\n\
             [[asset]]\n\
             os = \"linux\"\n\
             arch = \"x86_64\"\n\
             url = \"file:///fixture\"\n\
             sha256 = \"{hash_hex}\"\n"
        )
    }

    fn pin_for(bytes: &[u8], root: PathBuf) -> PinnedSubstrate {
        let hex = sha256_of(bytes).to_hex();
        PinnedSubstrate::from_manifest_str(&manifest_with(&hex), "linux", "x86_64", root).unwrap()
    }

    #[test]
    fn embedded_manifest_parses_and_picks_each_platform() {
        for (os, arch) in [
            ("macos", "aarch64"),
            ("macos", "x86_64"),
            ("linux", "x86_64"),
            ("linux", "aarch64"),
            ("windows", "x86_64"),
            ("windows", "aarch64"),
        ] {
            let p =
                PinnedSubstrate::from_manifest_str(EMBEDDED_MANIFEST, os, arch, "/tmp/r".into());
            assert!(p.is_ok(), "{os}/{arch} should resolve");
        }
        // unknown platform → Platform
        assert!(matches!(
            PinnedSubstrate::from_manifest_str(EMBEDDED_MANIFEST, "plan9", "sparc", "/x".into()),
            Err(SubstrateError::Platform)
        ));
    }

    #[test]
    fn binary_path_is_opaque_and_under_install_root() {
        let p = pin_for(b"x", "/data/root".into());
        let bp = p.binary_path();
        assert!(bp.starts_with("/data/root"));
        assert!(bp.to_string_lossy().contains("rift-substrate"));
        // never reveals the upstream name
        assert!(!bp.to_string_lossy().to_ascii_lowercase().contains("entire"));
    }

    #[test]
    fn ensure_installed_provisions_verifies_and_reports() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"the pinned backend bytes".to_vec();
        let pin = pin_for(&bytes, dir.path().to_path_buf());
        let fetcher = MemFetcher {
            bytes: bytes.clone(),
            calls: std::cell::Cell::new(0),
        };
        let report = pin.ensure_installed(&fetcher).unwrap();
        assert!(report.binary.exists());
        assert_eq!(report.substrate_sha256, sha256_of(&bytes));
        assert_eq!(report.substrate_version, "1.2.3");
        assert_eq!(fetcher.calls.get(), 1);
        // on-disk re-verify passes
        pin.verify_on_disk(&report.binary).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&report.binary)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700, "binary must be 0700");
        }
    }

    #[test]
    fn ensure_installed_is_idempotent_no_refetch() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"backend".to_vec();
        let pin = pin_for(&bytes, dir.path().to_path_buf());
        pin.ensure_installed(&MemFetcher {
            bytes: bytes.clone(),
            calls: std::cell::Cell::new(0),
        })
        .unwrap();
        // second call: a valid binary is present → fetcher must NOT be called
        let report = pin.ensure_installed(&PanicFetcher).unwrap();
        assert!(report.binary.exists());
    }

    #[test]
    fn tampered_on_disk_binary_triggers_reinstall() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = b"good backend".to_vec();
        let pin = pin_for(&bytes, dir.path().to_path_buf());
        let path = pin.binary_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"TAMPERED").unwrap();
        // a valid fetcher provides the real bytes → reinstall succeeds
        let report = pin
            .ensure_installed(&MemFetcher {
                bytes: bytes.clone(),
                calls: std::cell::Cell::new(0),
            })
            .unwrap();
        pin.verify_on_disk(&report.binary).unwrap();
    }

    #[test]
    fn mismatched_download_is_integrity_and_never_written() {
        let dir = tempfile::tempdir().unwrap();
        // pin expects the hash of `expected`, but the fetcher serves `evil`
        let pin = pin_for(b"expected", dir.path().to_path_buf());
        let err = pin.ensure_installed(&MemFetcher {
            bytes: b"evil different bytes".to_vec(),
            calls: std::cell::Cell::new(0),
        });
        assert!(matches!(err, Err(SubstrateError::Integrity)));
        assert!(
            !pin.binary_path().exists(),
            "bad bytes must never be written"
        );
    }

    #[test]
    fn malformed_manifest_is_unavailable_not_panic() {
        assert!(matches!(
            PinnedSubstrate::from_manifest_str(
                "this is not toml {{{",
                "linux",
                "x86_64",
                "/x".into()
            ),
            Err(SubstrateError::Unavailable)
        ));
    }
}
