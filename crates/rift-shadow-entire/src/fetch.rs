//! P1 B2 — the production download behind the [`SubstrateFetcher`] seam.
//!
//! This is the **only** network call in the wrapper. It downloads the upstream
//! release archive (HTTPS-only, bounded in time and size), **extracts** the
//! capture binary (`entire` from a `*.tar.gz`, `entire.exe` from a `*.zip`), and
//! returns those binary bytes. It performs **no** integrity decision of its own —
//! [`PinnedSubstrate::ensure_installed`](crate::PinnedSubstrate::ensure_installed)
//! hashes the returned bytes against the pinned per-platform value and refuses to
//! write anything executable on a mismatch. So a tampered archive that extracts to
//! the wrong bytes is caught before anything is ever marked executable.
//!
//! Compiled only under the `provision-http` feature; tests and the default build
//! use the in-memory fixture fetcher in [`crate::pin`].
//!
//! No upstream brand and no URL/byte content is ever logged from here.

use std::io::{Cursor, Read as _};
use std::time::Duration;

use ureq::Agent;

use crate::pin::SubstrateFetcher;
use crate::SubstrateError;

/// Hard ceiling on a download so a hostile or misconfigured endpoint cannot
/// exhaust memory. Comfortably above any real backend archive.
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;

/// Wall-clock budget for the whole request (connect → body read).
const REQUEST_TIMEOUT: Duration = Duration::from_mins(2);

/// The capture binary's name inside a `*.tar.gz` archive.
const UNIX_MEMBER: &str = "entire";
/// The capture binary's name inside a `*.zip` archive.
const WINDOWS_MEMBER: &str = "entire.exe";

/// The production fetcher: one bounded HTTPS `GET` (rustls/ring) + archive extract.
pub struct HttpFetcher {
    agent: Agent,
    max_bytes: u64,
}

impl HttpFetcher {
    /// A fetcher with the default time/size bounds.
    #[must_use]
    pub fn new() -> Self {
        Self::with_limits(REQUEST_TIMEOUT, MAX_DOWNLOAD_BYTES)
    }

    /// A fetcher with explicit bounds (testable; keeps the constants in one place).
    #[must_use]
    pub fn with_limits(timeout: Duration, max_bytes: u64) -> Self {
        let config = Agent::config_builder()
            .timeout_global(Some(timeout))
            .build();
        Self {
            agent: config.into(),
            max_bytes,
        }
    }

    /// Download the raw archive bytes at `url`.
    fn download(&self, url: &str) -> Result<Vec<u8>, SubstrateError> {
        let mut response = self
            .agent
            .get(url)
            .call()
            .map_err(|_| SubstrateError::Unavailable)?;
        if !response.status().is_success() {
            return Err(SubstrateError::Unavailable);
        }
        response
            .body_mut()
            .with_config()
            .limit(self.max_bytes)
            .read_to_vec()
            .map_err(|_| SubstrateError::Unavailable)
    }
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self::new()
    }
}

impl SubstrateFetcher for HttpFetcher {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, SubstrateError> {
        // HTTPS-only: a binary we are about to verify-and-execute is never pulled
        // over plaintext, regardless of what a manifest claims.
        if !url.starts_with("https://") {
            return Err(SubstrateError::Unavailable);
        }
        let archive = self.download(url)?;
        extract_binary(&archive, url)
    }
}

/// Extract the capture binary from a release archive, dispatching on the URL's
/// archive extension. The binary is the member at the archive root. (`*.tar.gz`
/// and `*.tgz` both report a `gz` extension.)
fn extract_binary(archive: &[u8], url: &str) -> Result<Vec<u8>, SubstrateError> {
    let ext = std::path::Path::new(url)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    if ext.eq_ignore_ascii_case("zip") {
        extract_zip_member(archive, WINDOWS_MEMBER)
    } else if ext.eq_ignore_ascii_case("gz") || ext.eq_ignore_ascii_case("tgz") {
        extract_tar_gz_member(archive, UNIX_MEMBER)
    } else {
        Err(SubstrateError::Unavailable)
    }
}

/// Extract the root member named `member` from a gzip-compressed tar archive.
fn extract_tar_gz_member(archive: &[u8], member: &str) -> Result<Vec<u8>, SubstrateError> {
    let gz = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(gz);
    let entries = tar.entries().map_err(|_| SubstrateError::Unavailable)?;
    for entry in entries {
        let mut entry = entry.map_err(|_| SubstrateError::Unavailable)?;
        let is_target = entry
            .path()
            .ok()
            .as_deref()
            .and_then(std::path::Path::file_name)
            .and_then(|f| f.to_str())
            == Some(member);
        if is_target {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .map_err(|_| SubstrateError::Unavailable)?;
            return Ok(buf);
        }
    }
    Err(SubstrateError::Unavailable)
}

/// Extract the entry named `member` from a zip archive.
fn extract_zip_member(archive: &[u8], member: &str) -> Result<Vec<u8>, SubstrateError> {
    let mut zip =
        zip::ZipArchive::new(Cursor::new(archive)).map_err(|_| SubstrateError::Unavailable)?;
    let mut file = zip
        .by_name(member)
        .map_err(|_| SubstrateError::Unavailable)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|_| SubstrateError::Unavailable)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    #[test]
    fn rejects_non_https_scheme_without_network() {
        let f = HttpFetcher::new();
        // No socket is opened: the scheme guard fails closed first.
        assert!(matches!(
            f.fetch("http://example.invalid/x"),
            Err(SubstrateError::Unavailable)
        ));
        assert!(matches!(
            f.fetch("ftp://example.invalid/x"),
            Err(SubstrateError::Unavailable)
        ));
        assert!(matches!(
            f.fetch("not-a-url"),
            Err(SubstrateError::Unavailable)
        ));
    }

    #[test]
    fn constructs_with_custom_limits() {
        let _ = HttpFetcher::with_limits(Duration::from_secs(1), 1024);
        let _ = HttpFetcher::default();
    }

    fn tar_gz_with(members: &[(&str, &[u8])]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gz);
        for (name, data) in members {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            builder.append_data(&mut header, name, *data).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    fn zip_with(members: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, data) in members {
            writer.start_file(*name, opts).unwrap();
            writer.write_all(data).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn extracts_binary_from_tar_gz_ignoring_decoys() {
        let archive = tar_gz_with(&[
            ("README.md", b"readme"),
            ("completions/entire.bash", b"completion script"),
            ("git-remote-entire", b"the helper, not the binary"),
            ("entire", b"THE REAL BACKEND BINARY"),
        ]);
        let out = extract_binary(&archive, "https://x/entire_linux_amd64.tar.gz").unwrap();
        assert_eq!(out, b"THE REAL BACKEND BINARY");
    }

    #[test]
    fn extracts_binary_from_zip_ignoring_decoys() {
        let archive = zip_with(&[
            ("README.md", b"readme"),
            ("git-remote-entire.exe", b"the helper"),
            ("entire.exe", b"THE REAL WINDOWS BINARY"),
        ]);
        let out = extract_binary(&archive, "https://x/entire_windows_amd64.zip").unwrap();
        assert_eq!(out, b"THE REAL WINDOWS BINARY");
    }

    #[test]
    fn missing_member_or_unknown_extension_is_unavailable() {
        let archive = tar_gz_with(&[("README.md", b"no binary here")]);
        assert!(matches!(
            extract_binary(&archive, "https://x/foo.tar.gz"),
            Err(SubstrateError::Unavailable)
        ));
        // an unknown archive extension is refused
        assert!(matches!(
            extract_binary(b"raw", "https://x/foo.bin"),
            Err(SubstrateError::Unavailable)
        ));
    }
}
