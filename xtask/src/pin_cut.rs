//! `xtask pin-cut <version>` — the scripted pin-cut (ADR-0007, P1 S1).
//!
//! Re-cuts `crates/rift-shadow-entire/pinned-substrate.toml` for one upstream
//! release:
//!
//! 1. downloads the release's own `checksums.txt` (the upstream checksum
//!    manifest) and the six per-platform archives;
//! 2. cross-checks each **archive**'s SHA-256 against that manifest — a
//!    mismatch aborts the cut (nothing is written);
//! 3. extracts the capture binary through the **same** extraction path
//!    production provisioning uses ([`rift_shadow_entire::fetch::extract_binary`])
//!    and hashes the **extracted binary** — the value the manifest pins;
//! 4. rewrites the manifest, preserving `rift_release` and the documented
//!    header.
//!
//! The printed archive hashes are the review anchors: they must match the
//! hashes recorded in the ADR that authorizes the pin bump (ADR-0007 for
//! v0.7.8). The network is used here and only here in xtask — the pin-cut is
//! an operator action, never CI.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use rift_shadow_entire::fetch::{extract_binary, HttpFetcher};
use rift_shadow_entire::sha256_hex_of;

/// Upstream release-asset base. Production tags only (the manifest's own rule).
const RELEASE_BASE: &str = "https://github.com/entireio/cli/releases/download";

/// The six pinned platforms: manifest `(os, arch)` → upstream asset stem.
const PLATFORMS: &[(&str, &str, &str)] = &[
    ("macos", "aarch64", "entire_darwin_arm64.tar.gz"),
    ("macos", "x86_64", "entire_darwin_amd64.tar.gz"),
    ("linux", "x86_64", "entire_linux_amd64.tar.gz"),
    ("linux", "aarch64", "entire_linux_arm64.tar.gz"),
    ("windows", "x86_64", "entire_windows_amd64.zip"),
    ("windows", "aarch64", "entire_windows_arm64.zip"),
];

/// The manifest header comment, regenerated verbatim on every cut so the file
/// stays self-documenting.
const MANIFEST_HEADER: &str = "\
# pinned-substrate.toml — the ONE capture-backend build this Rift release may run.
#
# This file is `include_str!`-embedded into the binary (P1 S1) so an on-disk edit
# cannot trick the running wrapper. It pins, per OS/arch, the upstream release
# download URL and the SHA-256 of the **extracted binary** (not the archive): the
# wrapper downloads the release archive, extracts the binary, and refuses to
# install anything whose bytes do not hash to the value below. So the backend we
# exec is exactly the one pinned here, or we do not run.
#
# `sha256` is the hash of the extracted `entire`/`entire.exe` member, derived at
# pin-cut from the archives published with the upstream release (whose archive
# checksums were cross-checked against the release's `checksums.txt`). A new
# upstream version is a new Rift release (the pin is release-coupled; there is no
# independent \"upgrade the backend\" command).
#
# No binary is committed to this repo — only this pin. The download happens at
# `rift setup` time (P1 B2) from the upstream GitHub releases, production tags only.
# Re-cut with `cargo run -p xtask -- pin-cut <version>` (the scripted pin-cut).
";

/// One cut platform: the pinned (extracted-binary) hash. The archive hash is
/// verified against the upstream manifest before this is ever constructed.
struct CutAsset {
    os: &'static str,
    arch: &'static str,
    url: String,
    binary_sha256: String,
}

/// Parse `checksums.txt` (`<hex>  <filename>` lines) into `(filename, hex)`.
fn parse_checksums(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            let hash = parts.next()?.to_ascii_lowercase();
            let name = parts.next()?.to_owned();
            (hash.len() == 64).then_some((name, hash))
        })
        .collect()
}

/// Extract the `rift_release` value from the existing manifest (preserved on cut:
/// bumping it is a release action, not a pin action).
fn current_rift_release(manifest: &str) -> Result<String, String> {
    let value: toml::Value =
        toml::from_str(manifest).map_err(|e| format!("current manifest unparseable: {e}"))?;
    value
        .get("rift_release")
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .ok_or_else(|| "current manifest has no rift_release".into())
}

/// Render the new manifest.
fn render_manifest(version: &str, rift_release: &str, assets: &[CutAsset]) -> String {
    let mut out = String::from(MANIFEST_HEADER);
    let _ = write!(
        out,
        "\nschema = 1\nsubstrate_version = \"{version}\"\nrift_release = \"{rift_release}\"\n"
    );
    for a in assets {
        let label = match (a.os, a.arch) {
            ("macos", "aarch64") => "macOS — Apple Silicon",
            ("macos", "x86_64") => "macOS — Intel",
            ("linux", "x86_64") => "Linux — x86_64",
            ("linux", "aarch64") => "Linux — aarch64",
            ("windows", "x86_64") => "Windows — x86_64",
            _ => "Windows — aarch64",
        };
        let _ = write!(
            out,
            "\n# {label}\n[[asset]]\nos = \"{}\"\narch = \"{}\"\nurl = \"{}\"\nsha256 = \"{}\"\n",
            a.os, a.arch, a.url, a.binary_sha256
        );
    }
    out
}

/// The manifest path, relative to the workspace root.
fn manifest_path(root: &Path) -> PathBuf {
    root.join("crates")
        .join("rift-shadow-entire")
        .join("pinned-substrate.toml")
}

/// Run the pin-cut. Downloads, cross-checks, extracts, hashes, and rewrites the
/// manifest. Returns the human-readable summary (also printed by the caller).
///
/// # Errors
/// Any download, checksum-manifest mismatch, extraction, or IO failure — the
/// manifest is written only after **all six** platforms verified.
pub fn run(root: &Path, version: &str) -> Result<String, String> {
    if version.is_empty() || !version.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return Err(format!("not a release version: {version:?}"));
    }
    let path = manifest_path(root);
    let current =
        std::fs::read_to_string(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
    let rift_release = current_rift_release(&current)?;

    let fetcher = HttpFetcher::new();
    let sums_url = format!("{RELEASE_BASE}/v{version}/checksums.txt");
    let sums_raw = fetcher
        .download_raw(&sums_url)
        .map_err(|e| format!("download {sums_url}: {e}"))?;
    let sums = parse_checksums(&String::from_utf8_lossy(&sums_raw));
    if sums.is_empty() {
        return Err(format!("no parseable entries in {sums_url}"));
    }

    let mut assets = Vec::new();
    let mut summary = format!(
        "pin-cut v{version} (rift_release {rift_release})\n\
         upstream checksum manifest: {sums_url}\n"
    );
    for (os, arch, stem) in PLATFORMS {
        let url = format!("{RELEASE_BASE}/v{version}/{stem}");
        let expected = sums
            .iter()
            .find(|(name, _)| name == stem)
            .map(|(_, h)| h.clone())
            .ok_or_else(|| format!("{stem}: missing from checksums.txt"))?;
        let archive = fetcher
            .download_raw(&url)
            .map_err(|e| format!("download {url}: {e}"))?;
        let archive_sha = sha256_hex_of(&archive);
        if archive_sha != expected {
            return Err(format!(
                "{stem}: archive sha256 {archive_sha} does not match the upstream \
                 checksum manifest ({expected}) — refusing to cut"
            ));
        }
        let binary = extract_binary(&archive, &url)
            .map_err(|e| format!("{stem}: could not extract the capture binary: {e}"))?;
        let binary_sha = sha256_hex_of(&binary);
        let _ = writeln!(
            summary,
            "  {os}/{arch}: archive {archive_sha} (matches manifest) -> binary {binary_sha}"
        );
        assets.push(CutAsset {
            os,
            arch,
            url,
            binary_sha256: binary_sha,
        });
    }

    let rendered = render_manifest(version, &rift_release, &assets);
    std::fs::write(&path, &rendered).map_err(|e| format!("write {}: {e}", path.display()))?;
    let _ = writeln!(
        summary,
        "wrote {} — review the archive hashes above against the authorizing ADR",
        path.display()
    );
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checksums_parse_ignores_malformed_lines() {
        let text = "abc\n\
            0000000000000000000000000000000000000000000000000000000000000001  a.tar.gz\n\
            not-a-hash  b.tar.gz\n\
            0000000000000000000000000000000000000000000000000000000000000002  c.zip\n";
        let sums = parse_checksums(text);
        assert_eq!(sums.len(), 2);
        assert_eq!(sums[0].0, "a.tar.gz");
    }

    #[test]
    fn manifest_render_is_parseable_and_pins_the_binary_hash() {
        let assets = vec![CutAsset {
            os: "linux",
            arch: "x86_64",
            url: "https://x/y.tar.gz".into(),
            binary_sha256: "b".repeat(64),
        }];
        let rendered = render_manifest("9.9.9", "0.1.0", &assets);
        let value: toml::Value = toml::from_str(&rendered).unwrap();
        assert_eq!(
            value.get("substrate_version").and_then(|v| v.as_str()),
            Some("9.9.9")
        );
        let sha = value.get("asset").unwrap().as_array().unwrap()[0]
            .get("sha256")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(sha, "b".repeat(64), "the manifest pins the BINARY hash");
    }

    #[test]
    fn version_tokens_are_validated() {
        let root = std::path::Path::new("/nonexistent");
        assert!(run(root, "v0.7.8").is_err(), "leading v is not a version");
        assert!(run(root, "0.7.8; rm -rf /").is_err());
    }

    #[test]
    fn rift_release_is_read_from_the_current_manifest() {
        let cur = "schema = 1\nsubstrate_version = \"0.0.1\"\nrift_release = \"0.1.0\"\n";
        assert_eq!(current_rift_release(cur).unwrap(), "0.1.0");
        assert!(current_rift_release("schema = 1\n").is_err());
    }
}
