//! Lifecycle of installer-managed executables. Development binaries are never removed.

#[cfg(feature = "tui")]
mod configure;
#[cfg(any(feature = "tui", test))]
pub(crate) mod defaults;

pub(crate) fn preferences(ascii: bool) -> Result<Outcome> {
    #[cfg(feature = "tui")]
    {
        configure::run(ascii)
    }
    #[cfg(not(feature = "tui"))]
    {
        let _ = ascii;
        Err(refusal(
            "installer preferences require a build with terminal UI support",
        ))
    }
}

mod lock;
mod path_entry;
mod release_feed;
mod uninstall;
pub(crate) mod update_check;

use crate::cli::{Outcome, UninstallArgs, UpdateArgs};
use rift_core::{Result, RiftError};
use sha2::{Digest as _, Sha256};
use std::io::Read as _;
use std::path::{Path, PathBuf};

pub(crate) use uninstall::run as uninstall;

struct Installation {
    binary: PathBuf,
    receipt: PathBuf,
    checksum: String,
    version: String,
}

impl Installation {
    fn current() -> Result<Self> {
        let binary = std::env::current_exe()?;
        Self::at(binary)
    }

    fn at(binary: PathBuf) -> Result<Self> {
        let parent = binary
            .parent()
            .ok_or_else(|| refusal("executable has no installation directory"))?;
        let receipt = parent.join("rift.install-receipt");
        regular_file(&binary)?;
        regular_file(&receipt).map_err(|_| refusal("this executable is not installer-managed; development binaries and their hooks were preserved"))?;
        if std::fs::metadata(&receipt)?.len() > 512 {
            return Err(refusal("invalid installer receipt"));
        }
        let text = std::fs::read_to_string(&receipt)?;
        let mut lines = text.lines();
        if lines.next() != Some("rift-install-v1") {
            return Err(refusal("unrecognized installer receipt"));
        }
        let checksum = lines.next().unwrap_or_default().to_owned();
        let version = lines.next().unwrap_or_default().to_owned();
        if checksum.len() != 64
            || !checksum.bytes().all(|c| c.is_ascii_hexdigit())
            || !valid_version(&version)
            || lines.next().is_some()
        {
            return Err(refusal("invalid installer receipt"));
        }
        if digest(&binary)? != checksum {
            return Err(refusal(
                "executable differs from its installation receipt; nothing was replaced or removed",
            ));
        }
        Ok(Self {
            binary,
            receipt,
            checksum,
            version,
        })
    }

    fn recheck(&self) -> Result<()> {
        regular_file(&self.binary)?;
        regular_file(&self.receipt)?;
        if digest(&self.binary)? != self.checksum {
            return Err(refusal("executable changed during lifecycle operation"));
        }
        let current = Installation::at(self.binary.clone())?;
        if current.version != self.version || current.checksum != self.checksum {
            return Err(refusal(
                "installation receipt changed during lifecycle operation",
            ));
        }
        Ok(())
    }
}

/// The release bucket every download link must point into. Kept equal to
/// `release/public-storage.toml` by a test and by the release tooling: the
/// manifest carries the checksums, so the bucket it comes from is the anchor.
const RELEASE_BUCKET: &str = "rift-e59ea04337d9";

/// Whether `link` points into the release bucket over HTTPS. A dot segment,
/// a percent escape or a backslash is refused outright: curl resolves
/// `bucket/../other` before fetching, so a prefix test alone would pass a
/// link that fetches from another bucket.
fn in_release_bucket(link: &str) -> bool {
    if link.contains("/.") || link.contains('%') || link.contains('\\') {
        return false;
    }
    link.strip_prefix("https://storage.googleapis.com/")
        .and_then(|rest| rest.strip_prefix(RELEASE_BUCKET))
        .is_some_and(|rest| rest.starts_with('/'))
}

pub(crate) fn update(args: &UpdateArgs) -> Result<Outcome> {
    // With no link, the newest release: `latest/` in the release bucket.
    let link = args
        .from
        .clone()
        .or_else(|| std::env::var("RIFT_DOWNLOAD_MANIFEST").ok())
        .filter(|link| !link.trim().is_empty())
        .unwrap_or_else(release_feed::latest_link);
    let link = link.trim();
    if !in_release_bucket(link) {
        return Err(refusal(
            "This link is not from the Rift release bucket; run `rift update` without --from.",
        ));
    }
    let install = Installation::current()?;
    let signed = release_feed::fetch_signed(link, std::time::Duration::from_mins(1))?;
    let downgrade = std::env::var("RIFT_ALLOW_DOWNGRADE").is_ok_and(|v| v == "1");
    if alpha_key(&signed.version) <= alpha_key(&install.version) && !downgrade {
        return Ok(Outcome::ok(
            format!("Rift {} is up to date.", install.version),
            serde_json::json!({ "operation": "update", "version": install.version, "latest": signed.version, "status": "up_to_date" }),
        ));
    }
    let parent = install
        .binary
        .parent()
        .ok_or_else(|| refusal("installation directory unavailable"))?;
    let verified = VerifiedManifest::write(parent, &signed.bytes)?;
    run_installer(&install, parent, link, &verified.path)
}

/// The verified manifest, written beside the binary for the installer to copy
/// and removed when the update ends however it ends.
struct VerifiedManifest {
    path: PathBuf,
}

impl VerifiedManifest {
    fn write(dir: &Path, bytes: &[u8]) -> Result<Self> {
        use std::io::Write as _;
        let path = dir.join(format!(".rift-update-manifest-{}", std::process::id()));
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        let guard = Self { path };
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(guard)
    }
}

impl Drop for VerifiedManifest {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Run the installer compiled into this binary over the verified manifest,
/// then read the new executable's version back independently.
fn run_installer(
    install: &Installation,
    parent: &Path,
    link: &str,
    manifest: &Path,
) -> Result<Outcome> {
    #[cfg(not(windows))]
    let mut command = {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", include_str!("../../../install.sh"), "rift-installer"]);
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = std::process::Command::new("powershell.exe");
        command.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            include_str!("../../../install.ps1"),
        ]);
        command
    };
    install.recheck()?;
    let output = command
        .env("RIFT_BIN_DIR", parent)
        .env("RIFT_UPDATE", "1")
        .env("RIFT_DOWNLOAD_MANIFEST", link)
        .env("RIFT_VERIFIED_MANIFEST", manifest)
        .env("RIFT_DOWNLOAD_BUCKET", RELEASE_BUCKET)
        .env("RIFT_UPDATE_PID", std::process::id().to_string())
        .env("RIFT_UPDATE_SHA", &install.checksum)
        .env_remove("RIFT_VERSION")
        .output()?;
    if !output.status.success() {
        return Err(refusal(&format!(
            "update failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let message = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let updated = Installation::at(install.binary.clone())?;
    let readback = std::process::Command::new(&updated.binary)
        .arg("--version")
        .output()?;
    if !readback.status.success()
        || String::from_utf8_lossy(&readback.stdout).trim()
            != format!(
                "rift {}",
                updated.version.strip_prefix('v').unwrap_or_default()
            )
    {
        return Err(refusal(
            "updated executable failed independent version readback",
        ));
    }
    Ok(Outcome::ok(
        message,
        serde_json::json!({ "operation": "update", "previous_version": install.version, "version": updated.version, "binary": install.binary, "status": "completed" }),
    ))
}

/// A release tag this installer understands: `vMAJOR.MINOR.PATCH-alpha`,
/// optionally followed by `.N`. Each number is one to six digits, so a
/// lookalike (`v1.0.0-alphaevil`, a path suffix) is refused.
fn valid_version(version: &str) -> bool {
    alpha_key(version).is_some()
}

/// `[major, minor, patch, n]` of an alpha tag, which orders tags as semver
/// does within the alpha channel (a plain `-alpha` is `n = 0`).
fn alpha_key(version: &str) -> Option<[u32; 4]> {
    let number = |text: &str| {
        (!text.is_empty() && text.len() <= 6 && text.bytes().all(|c| c.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    };
    let (core, rest) = version.strip_prefix('v')?.split_once("-alpha")?;
    let mut parts = core.split('.');
    let key = [
        number(parts.next()?)?,
        number(parts.next()?)?,
        number(parts.next()?)?,
        match rest {
            "" => 0,
            _ => number(rest.strip_prefix('.')?)?,
        },
    ];
    parts.next().is_none().then_some(key)
}

fn regular_file(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(refusal(
            "installation path must be a regular file, not a link",
        ));
    }
    Ok(())
}

fn digest(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 8192];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(buffer.get(..count).unwrap_or_default());
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn refusal(reason: &str) -> RiftError {
    RiftError::Other(reason.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_links_are_pinned_to_the_recorded_release_bucket() {
        let recorded = include_str!("../../../release/public-storage.toml");
        assert!(
            recorded.contains(&format!("bucket = \"{RELEASE_BUCKET}\"")),
            "RELEASE_BUCKET and release/public-storage.toml drifted apart"
        );
        assert!(in_release_bucket(&format!(
            "https://storage.googleapis.com/{RELEASE_BUCKET}/a/downloads.tsv"
        )));
        for foreign in [
            "https://storage.googleapis.com/attacker-bucket/downloads.tsv",
            &format!("https://storage.googleapis.com/{RELEASE_BUCKET}-evil/downloads.tsv"),
            &format!("http://storage.googleapis.com/{RELEASE_BUCKET}/a/downloads.tsv"),
            &format!("https://example.com/{RELEASE_BUCKET}/downloads.tsv"),
            &format!("https://storage.googleapis.com/{RELEASE_BUCKET}"),
            &format!("https://storage.googleapis.com/{RELEASE_BUCKET}/../attacker/downloads.tsv"),
            &format!("https://storage.googleapis.com/{RELEASE_BUCKET}/a/%2e%2e/x.tsv"),
            &format!("https://storage.googleapis.com/{RELEASE_BUCKET}/a\\..\\x.tsv"),
        ] {
            assert!(!in_release_bucket(foreign), "{foreign}");
        }
    }

    #[test]
    fn development_and_modified_binaries_are_never_admitted() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("rift");
        std::fs::write(&binary, b"development").unwrap();
        assert!(Installation::at(binary.clone()).is_err());
        let hash = digest(&binary).unwrap();
        std::fs::write(
            dir.path().join("rift.install-receipt"),
            format!("rift-install-v1\n{hash}\nv1.0.0-alpha\n"),
        )
        .unwrap();
        assert!(Installation::at(binary.clone()).is_ok());
        std::fs::write(&binary, b"changed").unwrap();
        assert!(Installation::at(binary).is_err());
    }
    #[test]
    fn receipts_reject_lookalike_versions_and_changed_receipt_identity() {
        for invalid in [
            "v1.0.0-alphaevil",
            "v1.0.0-alpha.",
            "v1.0.0-alpha.2/other",
            "v1.0-alpha",
            "v1.0.0.0-alpha",
            "v1.1.0",
            "1.1.0-alpha",
            "v1234567.0.0-alpha",
        ] {
            assert!(!valid_version(invalid), "{invalid}");
        }
        assert!(valid_version("v1.0.0-alpha.2"));
        assert!(valid_version("v1.1.0-alpha"));
        assert!(alpha_key("v1.1.0-alpha") > alpha_key("v1.0.0-alpha.9"));
        assert!(alpha_key("v1.0.0-alpha.2") > alpha_key("v1.0.0-alpha"));
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("rift");
        std::fs::write(&binary, b"fixture").unwrap();
        let hash = digest(&binary).unwrap();
        let receipt = dir.path().join("rift.install-receipt");
        std::fs::write(&receipt, format!("rift-install-v1\n{hash}\nv1.0.0-alpha\n")).unwrap();
        let install = Installation::at(binary).unwrap();
        std::fs::write(
            &receipt,
            format!("rift-install-v1\n{hash}\nv1.0.0-alpha.1\n"),
        )
        .unwrap();
        assert!(install.recheck().is_err());
    }
}
