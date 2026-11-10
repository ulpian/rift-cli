//! Signing download manifests.
//!
//! The private key is a 32-byte Ed25519 seed held in Secret Manager, in the
//! release project named by `release/public-storage.toml`. It is created once
//! by `cargo rift signing-key create`, which pipes it straight into the secret
//! (it never touches disk), and read back only for the moment a manifest is
//! signed. The public half is recorded in `public-storage.toml` and compiled
//! into the binary (`MANIFEST_PUBLIC_KEY` in `bins/rift/src/installation`),
//! where a test keeps the two equal. Signing refuses a secret whose public
//! half differs from the recorded one, so a swapped secret cannot produce
//! manifests that installed binaries would then refuse.
//!
//! `RIFT_RELEASE_SIGNING_KEY_FILE` names a file holding the seed as hex, for
//! signing without cloud access.

use std::io::Write as _;
use std::process::{Command, Stdio};

use ed25519_dalek::{Signer as _, SigningKey};
use zeroize::Zeroizing;

use super::{ensure, process, Result};

/// The Secret Manager secret that holds the seed, as hex.
const SECRET: &str = "rift-release-manifest-key";

/// The detached signature of `message`, as lowercase hex.
pub(super) fn sign(message: &[u8]) -> Result<String> {
    let key = SigningKey::from_bytes(&*seed()?);
    ensure(
        hex(key.verifying_key().as_bytes()) == recorded_public_key()?,
        "the signing secret's public key differs from release/public-storage.toml \
         manifest_public_key; installed binaries would refuse its signatures",
    )?;
    Ok(hex(&key.sign(message).to_bytes()))
}

/// Create the signing key in Secret Manager and print its public half. Refuses
/// when the secret already exists: replacing it would strand every installed
/// binary, which trusts only the recorded public key.
pub(super) fn create() -> Result<()> {
    let project = project()?;
    let exists = Command::new("gcloud")
        .args([
            "secrets",
            "describe",
            SECRET,
            "--project",
            &project,
            "--quiet",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    ensure(
        !exists.success(),
        "the signing secret already exists; it is never replaced",
    )?;
    let mut seed = Zeroizing::new([0u8; 32]);
    getrandom::getrandom(seed.as_mut()).map_err(|e| e.to_string())?;
    let mut child = Command::new("gcloud")
        .args([
            "secrets",
            "create",
            SECRET,
            "--project",
            &project,
            "--replication-policy=automatic",
            "--data-file=-",
            "--quiet",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let text = Zeroizing::new(hex(seed.as_ref()));
    child
        .stdin
        .take()
        .ok_or("gcloud stdin unavailable")?
        .write_all(text.as_bytes())
        .map_err(|e| e.to_string())?;
    ensure(
        child.wait().map_err(|e| e.to_string())?.success(),
        "gcloud could not create the signing secret",
    )?;
    let public = hex(SigningKey::from_bytes(&seed).verifying_key().as_bytes());
    println!(
        "Created {SECRET} in {project}.\nRecord this public key as manifest_public_key in \
         release/public-storage.toml and as MANIFEST_PUBLIC_KEY in the binary:\n{public}"
    );
    Ok(())
}

fn seed() -> Result<Zeroizing<[u8; 32]>> {
    let text = Zeroizing::new(match std::env::var_os("RIFT_RELEASE_SIGNING_KEY_FILE") {
        Some(path) => std::fs::read_to_string(path).map_err(|e| e.to_string())?,
        None => process::must(
            Command::new("gcloud").args([
                "secrets",
                "versions",
                "access",
                "latest",
                "--secret",
                SECRET,
                "--project",
                &project()?,
                "--quiet",
            ]),
            60,
        )?,
    });
    let mut seed = Zeroizing::new([0u8; 32]);
    unhex(text.trim(), seed.as_mut()).ok_or("the signing seed is not 64 hex digits")?;
    Ok(seed)
}

fn storage_field(key: &str) -> Result<String> {
    let recorded = crate::read_toml(&crate::workspace_root().join("release/public-storage.toml"))?;
    recorded
        .get(key)
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| format!("release/public-storage.toml has no {key}"))
}

fn project() -> Result<String> {
    storage_field("project")
}

pub(super) fn recorded_public_key() -> Result<String> {
    storage_field("manifest_public_key")
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn unhex(text: &str, out: &mut [u8]) -> Option<()> {
    if text.len() != out.len() * 2 || !text.is_ascii() {
        return None;
    }
    for (slot, pair) in out.iter_mut().zip(text.as_bytes().chunks(2)) {
        *slot = u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok()?;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_round_trips_and_refuses_the_wrong_length() {
        let mut out = [0u8; 4];
        unhex(&hex(&[0, 1, 0xab, 0xff]), &mut out).unwrap();
        assert_eq!(out, [0, 1, 0xab, 0xff]);
        assert!(unhex("abc", &mut out).is_none());
        assert!(unhex("zzzzzzzz", &mut out).is_none());
    }
}
