//! Five-platform archive contract, source/version binding and native smoke evidence.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;
use std::process::Command;

use sha2::{Digest, Sha256};

use super::{ensure, process, read, write, Result};

pub(super) const TARGETS: [&str; 5] = [
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "aarch64-unknown-linux-gnu",
    "x86_64-unknown-linux-gnu",
    "x86_64-pc-windows-msvc",
];

pub(super) fn version() -> Result<String> {
    let root = crate::workspace_root();
    let tag = read(&root.join("release/alpha-version"))?.trim().to_owned();
    let (core, suffix) = tag
        .strip_prefix('v')
        .and_then(|rest| rest.split_once("-alpha"))
        .ok_or("invalid alpha version")?;
    let digits = |s: &str| !s.is_empty() && s.len() <= 6 && s.bytes().all(|b| b.is_ascii_digit());
    ensure(
        core.split('.').count() == 3
            && core.split('.').all(digits)
            && (suffix.is_empty() || suffix.strip_prefix('.').is_some_and(digits)),
        "invalid alpha version",
    )?;
    let workspace = crate::read_toml(&root.join("Cargo.toml"))?;
    let binary = crate::read_toml(&root.join("bins/rift/Cargo.toml"))?;
    let declared = workspace
        .get("workspace")
        .and_then(|v| v.get("package"))
        .and_then(|v| v.get("version"))
        .and_then(toml::Value::as_str);
    let inherited = binary
        .get("package")
        .and_then(|v| v.get("version"))
        .and_then(|v| v.get("workspace"))
        .and_then(toml::Value::as_bool);
    ensure(
        declared == tag.strip_prefix('v') && inherited == Some(true),
        "CLI, workspace and release versions disagree",
    )?;
    Ok(tag)
}

pub(super) fn host() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "apple-darwin",
        "linux" => "unknown-linux-gnu",
        "windows" => "pc-windows-msvc",
        _ => return Err("unsupported host OS".to_owned()),
    };
    let target = format!("{}-{os}", std::env::consts::ARCH);
    ensure(TARGETS.contains(&target.as_str()), "unsupported host CPU")?;
    Ok(target)
}

pub(super) fn name(tag: &str, target: &str) -> String {
    format!(
        "rift-{tag}-{target}.{}",
        if target.contains("windows") {
            "zip"
        } else {
            "tar.gz"
        }
    )
}

pub(super) fn digest(path: &Path) -> Result<String> {
    let mut file = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| e.to_string())?;
    Ok(format!("{:x}", hasher.finalize()))
}

pub(super) fn verify_version(binary: &Path, tag: &str) -> Result<()> {
    let reported = process::must(Command::new(binary).arg("--version"), 10)?;
    ensure(
        reported.trim() == format!("rift {}", tag.trim_start_matches('v')),
        &format!(
            "{} reports {:?}; expected rift {}",
            binary.display(),
            reported.trim(),
            tag.trim_start_matches('v')
        ),
    )
}

pub(super) fn archive(binary: &Path, output: &Path, windows: bool) -> Result<()> {
    let file = File::create(output).map_err(|e| e.to_string())?;
    if windows {
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file(
            "rift.exe",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .map_err(|e| e.to_string())?;
        std::io::copy(
            &mut File::open(binary).map_err(|e| e.to_string())?,
            &mut zip,
        )
        .map_err(|e| e.to_string())?;
        zip.finish().map_err(|e| e.to_string())?;
    } else {
        let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut tar = tar::Builder::new(encoder);
        let mut file = File::open(binary).map_err(|e| e.to_string())?;
        let mut header = tar::Header::new_gnu();
        header.set_size(file.metadata().map_err(|e| e.to_string())?.len());
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, "rift", &mut file)
            .map_err(|e| e.to_string())?;
        tar.into_inner()
            .map_err(|e| e.to_string())?
            .finish()
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(super) fn package(target: &str, binary: &Path, out: &Path) -> Result<()> {
    ensure(
        host()? == target,
        "packaging requires a native host for version verification",
    )?;
    let tag = version()?;
    verify_version(binary, &tag)?;
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let destination = out.join(name(&tag, target));
    ensure(
        !destination.exists(),
        "archive already exists; choose a clean output folder",
    )?;
    archive(binary, &destination, target.contains("windows"))?;
    println!("{}", destination.display());
    Ok(())
}

pub(super) fn parse_checksums(text: &str, tag: &str) -> Result<BTreeMap<String, String>> {
    let mut entries = BTreeMap::new();
    for line in text.lines() {
        let (sha, filename) = line.split_once("  ").ok_or("invalid checksum line")?;
        ensure(
            sha.len() == 64
                && sha
                    .bytes()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
            "invalid SHA-256",
        )?;
        ensure(
            entries
                .insert(filename.to_owned(), sha.to_owned())
                .is_none(),
            "duplicate checksum",
        )?;
    }
    ensure(
        entries.len() == TARGETS.len()
            && TARGETS.iter().all(|t| entries.contains_key(&name(tag, t))),
        "all five current-version official archives are required",
    )?;
    Ok(entries)
}

pub(super) fn assets(dist: &Path) -> Result<(String, BTreeMap<String, String>)> {
    let tag = version()?;
    let entries = parse_checksums(&read(&dist.join("SHA256SUMS"))?, &tag)?;
    for (filename, sha) in &entries {
        ensure(
            digest(&dist.join(filename))? == *sha,
            &format!("checksum mismatch: {filename}"),
        )?;
    }
    Ok((tag, entries))
}

pub(super) fn checksums(dist: &Path) -> Result<()> {
    let tag = version()?;
    let mut entries = BTreeMap::new();
    for target in TARGETS {
        let filename = name(&tag, target);
        entries.insert(filename.clone(), digest(&dist.join(filename))?);
    }
    let mut text = String::new();
    for (name, sha) in entries {
        text.push_str(&format!("{sha}  {name}\n"));
    }
    write(&dist.join("SHA256SUMS"), text)
}

fn member_digest(path: &Path, windows: bool) -> Result<String> {
    let file = File::open(path).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    if windows {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        ensure(zip.len() == 1, "expected exactly one archive member")?;
        let mut entry = zip.by_index(0).map_err(|e| e.to_string())?;
        ensure(
            entry.name() == "rift.exe" && entry.is_file() && !entry.is_symlink(),
            "invalid ZIP member",
        )?;
        std::io::copy(&mut entry, &mut hash).map_err(|e| e.to_string())?;
    } else {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
        let mut entries = tar.entries().map_err(|e| e.to_string())?;
        let mut entry = entries
            .next()
            .ok_or("empty archive")?
            .map_err(|e| e.to_string())?;
        ensure(
            entry.path().map_err(|e| e.to_string())? == Path::new("rift")
                && entry.header().entry_type().is_file(),
            "invalid tar member",
        )?;
        std::io::copy(&mut entry, &mut hash).map_err(|e| e.to_string())?;
        ensure(entries.next().is_none(), "unexpected extra tar member")?;
    }
    Ok(format!("{:x}", hash.finalize()))
}

pub(super) fn native_check(target: &str, binary: &Path, archive: &Path, out: &Path) -> Result<()> {
    ensure(
        host()? == target,
        "native checks must run on the target OS and CPU",
    )?;
    let tag = version()?;
    verify_version(binary, &tag)?;
    process::must(Command::new(binary).arg("--help"), 10)?;
    let sha = digest(binary)?;
    ensure(
        member_digest(archive, target.contains("windows"))? == sha,
        "archive executable differs from the native-tested executable",
    )?;
    let commit = process::git(&["rev-parse", "HEAD"])?;
    let record = serde_json::json!({"schema":1, "target":target, "version":tag,
        "commit":commit, "archive_sha256":digest(archive)?, "binary_sha256":sha,
        "checks":["version", "help", "archive-binary-match"]});
    std::fs::create_dir_all(out).map_err(|e| e.to_string())?;
    write(&out.join(format!("{target}.json")), format!("{record}\n"))?;
    println!(
        "PASS {target}: native version/help and archive bytes. Install/TUI acceptance is separate."
    );
    Ok(())
}

pub(super) fn admit(
    evidence: &Path,
    tag: &str,
    commit: &str,
    entries: &BTreeMap<String, String>,
) -> Result<()> {
    for target in TARGETS {
        let record: serde_json::Value =
            serde_json::from_str(&read(&evidence.join(format!("{target}.json")))?)
                .map_err(|e| e.to_string())?;
        let matches =
            |key: &str, value: &str| record.get(key).and_then(|v| v.as_str()) == Some(value);
        ensure(
            record.get("schema").and_then(serde_json::Value::as_u64) == Some(1)
                && matches("target", target)
                && matches("version", tag)
                && matches("commit", commit)
                && entries
                    .get(&name(tag, target))
                    .is_some_and(|sha| matches("archive_sha256", sha))
                && record.get("checks")
                    == Some(&serde_json::json!([
                        "version",
                        "help",
                        "archive-binary-match"
                    ])),
            &format!("missing or mismatched native evidence for {target}"),
        )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_and_windows_archives_contain_only_the_original_binary() {
        let temporary = tempfile::tempdir().unwrap();
        let binary = temporary.path().join("binary");
        write(&binary, b"binary fixture bytes").unwrap();
        for windows in [false, true] {
            let path = temporary.path().join(if windows {
                "archive.zip"
            } else {
                "archive.tar.gz"
            });
            archive(&binary, &path, windows).unwrap();
            assert_eq!(
                member_digest(&path, windows).unwrap(),
                digest(&binary).unwrap()
            );
        }
    }
}
