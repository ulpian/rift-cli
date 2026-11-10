//! The signed release feed: the manifest that names the newest release.
//!
//! The release tooling signs every download manifest with a key held in the
//! owner's secret store and publishes the newest one under `latest/`. This
//! module fetches a manifest and its detached signature, and accepts the
//! manifest only when the signature verifies against [`MANIFEST_PUBLIC_KEY`].
//! The manifest carries every archive's checksum, so a verified manifest
//! anchors the whole download: someone able to write to the bucket can no
//! longer swap an archive and its checksum together.
//!
//! The shipped binary carries no network crate, so the fetch runs `curl`
//! (bundled with macOS, Windows 10 and later, and the Linux distributions
//! the installer supports), HTTPS only, size- and time-bounded.

use std::time::Duration;

use rift_core::{Result, RiftError};

use super::{alpha_key, refusal, RELEASE_BUCKET};

/// The Ed25519 public key that signs release manifests. Equal to
/// `manifest_public_key` in `release/public-storage.toml` (a test holds them
/// together); the private half never leaves the release project's Secret
/// Manager.
const MANIFEST_PUBLIC_KEY: [u8; 32] = [
    0xdd, 0xd5, 0xc7, 0xb7, 0xe9, 0x38, 0x57, 0xd0, 0xbd, 0xd6, 0xde, 0x3c, 0x4d, 0x02, 0x26, 0x35,
    0x84, 0x86, 0x4e, 0x6a, 0x10, 0x3d, 0x5b, 0x3e, 0xb1, 0xd6, 0xa2, 0x18, 0x51, 0x92, 0x60, 0x97,
];

/// The largest manifest accepted; the installer enforces the same bound.
const MAX_MANIFEST_BYTES: u64 = 1_048_576;

/// A release manifest whose signature verified.
pub(super) struct SignedManifest {
    /// The exact bytes that were signed, handed to the installer unchanged.
    pub(super) bytes: Vec<u8>,
    /// The release tag the manifest names, already checked to be an alpha tag.
    pub(super) version: String,
}

/// The link to the newest release's manifest.
pub(super) fn latest_link() -> String {
    format!("https://storage.googleapis.com/{RELEASE_BUCKET}/latest/downloads.tsv")
}

/// Fetch the manifest at `link` and its `.sig`, and return it only when the
/// signature verifies and it names a valid release tag.
pub(super) fn fetch_signed(link: &str, timeout: Duration) -> Result<SignedManifest> {
    let bytes = fetch(link, MAX_MANIFEST_BYTES, timeout)?;
    let signature = fetch(&format!("{link}.sig"), 1024, timeout)?;
    accept(&MANIFEST_PUBLIC_KEY, bytes, &signature)
}

/// `bytes` as a [`SignedManifest`] when `signature` (hex, one line) is
/// `key`'s signature over exactly those bytes.
fn accept(key: &[u8; 32], bytes: Vec<u8>, signature: &[u8]) -> Result<SignedManifest> {
    let signature = std::str::from_utf8(signature)
        .ok()
        .and_then(|text| unhex::<64>(text.trim()))
        .ok_or_else(|| refusal("the release manifest's signature is malformed"))?;
    if !rift_crypto::verify_ed25519(key, &bytes, &signature) {
        return Err(refusal(
            "the release manifest's signature does not verify; nothing was downloaded or changed",
        ));
    }
    let version = std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| text.lines().next())
        .and_then(|header| header.strip_prefix("rift-download-v1\t"))
        .filter(|version| alpha_key(version).is_some())
        .ok_or_else(|| refusal("the release manifest names no valid release"))?
        .to_owned();
    Ok(SignedManifest { bytes, version })
}

/// The body at `url`, over HTTPS only (redirects included), at most
/// `max_bytes`, within `timeout`.
fn fetch(url: &str, max_bytes: u64, timeout: Duration) -> Result<Vec<u8>> {
    let output = std::process::Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
        .args([
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--fail",
            "--silent",
        ])
        .args(["--location", "--max-redirs", "2"])
        .arg("--max-filesize")
        .arg(max_bytes.to_string())
        .arg("--max-time")
        .arg(timeout.as_secs().max(1).to_string())
        .arg("--")
        .arg(url)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|_| refusal("curl is required to check for updates"))?;
    if !output.status.success() {
        return Err(RiftError::Other(format!(
            "could not fetch the release feed (curl exit {})",
            output.status.code().unwrap_or(-1)
        )));
    }
    Ok(output.stdout)
}

/// `text` as `N` bytes of lowercase or uppercase hex.
fn unhex<const N: usize>(text: &str) -> Option<[u8; N]> {
    if text.len() != N * 2 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn to_hex(bytes: &[u8]) -> String {
        use std::fmt::Write as _;
        bytes.iter().fold(String::new(), |mut out, b| {
            let _ = write!(out, "{b:02x}");
            out
        })
    }

    #[test]
    fn the_compiled_public_key_is_the_one_release_tooling_signs_with() {
        let recorded = include_str!("../../../../release/public-storage.toml");
        let hex = to_hex(&MANIFEST_PUBLIC_KEY);
        assert!(
            recorded.contains(&format!("manifest_public_key = \"{hex}\"")),
            "MANIFEST_PUBLIC_KEY and release/public-storage.toml drifted apart"
        );
    }

    fn signed(manifest: &[u8]) -> ([u8; 32], String) {
        use ed25519_dalek::{Signer as _, SigningKey};
        let key = SigningKey::from_bytes(&[3u8; 32]);
        let signature = key.sign(manifest).to_bytes();
        let hex = to_hex(&signature);
        (key.verifying_key().to_bytes(), hex)
    }

    #[test]
    fn a_correctly_signed_manifest_is_accepted_with_its_version() {
        let manifest = b"rift-download-v1\tv1.2.0-alpha\nrow\n";
        let (key, signature) = signed(manifest);
        let accepted =
            accept(&key, manifest.to_vec(), format!("{signature}\n").as_bytes()).unwrap();
        assert_eq!(accepted.version, "v1.2.0-alpha");
        assert_eq!(accepted.bytes, manifest);
    }

    #[test]
    fn an_unsigned_altered_or_foreign_manifest_is_refused() {
        let manifest = b"rift-download-v1\tv1.2.0-alpha\nrow\n";
        let (key, signature) = signed(manifest);
        let altered = b"rift-download-v1\tv1.2.0-alpha\nROW\n".to_vec();
        assert!(accept(&key, altered, signature.as_bytes()).is_err());
        assert!(accept(
            &MANIFEST_PUBLIC_KEY,
            manifest.to_vec(),
            signature.as_bytes()
        )
        .is_err());
        assert!(accept(&key, manifest.to_vec(), b"").is_err());
        assert!(accept(&key, manifest.to_vec(), b"not hex").is_err());
        let (key, signature) = signed(b"rift-download-v1\tv1.2.0\n");
        assert!(
            accept(
                &key,
                b"rift-download-v1\tv1.2.0\n".to_vec(),
                signature.as_bytes()
            )
            .is_err(),
            "a non-alpha tag is refused even when signed"
        );
    }

    #[test]
    fn hex_decoding_is_exact_length() {
        assert_eq!(unhex::<2>("0aFf"), Some([0x0a, 0xff]));
        assert_eq!(unhex::<2>("0aF"), None);
        assert_eq!(unhex::<2>("zz00"), None);
    }

    #[test]
    fn the_latest_link_is_inside_the_release_bucket() {
        assert!(super::super::in_release_bucket(&latest_link()));
    }
}
