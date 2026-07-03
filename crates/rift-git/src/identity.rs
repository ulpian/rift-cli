//! The production [`IdentitySource`]: git config + a per-OS machine
//! fingerprint (phase-04 step-03).
//!
//! Reads are strictly **local** — git config via an argv-array `git` shell-out
//! (no shell, no network), the machine fingerprint from the platform's local
//! identifier store — honoring the trait's no-network contract. The raw
//! fingerprint is returned as a [`Zeroizing`] buffer (read transiently, wiped
//! on drop); only the salted hash ever leaves `rift-core`'s derivation. The
//! per-install salt is injected by the caller (custodied in the OS keychain by
//! `bins/rift`), keeping this crate keychain-blind.

use std::path::PathBuf;
use std::process::Command;

use rift_core::identity::IdentitySource;
use zeroize::Zeroizing;

/// Environment variable for an explicit identity override (e.g. CI bots).
pub const ENV_DEVELOPER_OVERRIDE: &str = "RIFT_DEVELOPER";

/// Production identity inputs: git `user.name`/`user.email` scoped to a
/// worktree, the platform machine id, and an injected install salt.
pub struct GitConfigIdentity {
    work_tree: PathBuf,
    salt: [u8; 32],
}

impl GitConfigIdentity {
    /// An identity source for the repo at `work_tree` with the given
    /// per-install `salt` (provision it from the OS keychain).
    #[must_use]
    pub fn new(work_tree: impl Into<PathBuf>, salt: [u8; 32]) -> Self {
        Self {
            work_tree: work_tree.into(),
            salt,
        }
    }

    /// `git config --get <key>` in the worktree (argv array, no shell). `None`
    /// on any failure — identity derivation degrades honestly, never errors.
    fn git_config(&self, key: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.work_tree)
            .args(["config", "--get", key])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let value = String::from_utf8(output.stdout).ok()?;
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }
}

/// The platform machine identifier, read transiently. Not a secret in the
/// cryptographic sense, but treated with fingerprint hygiene: zeroized after
/// hashing so the raw value never lingers or gets persisted/logged.
fn machine_fingerprint_raw() -> Option<Zeroizing<String>> {
    #[cfg(target_os = "linux")]
    {
        for path in ["/etc/machine-id", "/var/lib/dbus/machine-id"] {
            if let Ok(s) = std::fs::read_to_string(path) {
                let t = s.trim();
                if !t.is_empty() {
                    return Some(Zeroizing::new(t.to_string()));
                }
            }
        }
        None
    }
    #[cfg(target_os = "macos")]
    {
        // `ioreg -rd1 -c IOPlatformExpertDevice` prints `"IOPlatformUUID" = "…"`.
        let output = Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = Zeroizing::new(String::from_utf8(output.stdout).ok()?);
        let uuid = text
            .lines()
            .find(|l| l.contains("IOPlatformUUID"))
            .and_then(|l| l.split('"').nth(3))?;
        Some(Zeroizing::new(uuid.to_string()))
    }
    #[cfg(target_os = "windows")]
    {
        // `reg query HKLM\...\Cryptography /v MachineGuid` prints the GUID last.
        let output = Command::new("reg")
            .args([
                "query",
                r"HKLM\SOFTWARE\Microsoft\Cryptography",
                "/v",
                "MachineGuid",
            ])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = Zeroizing::new(String::from_utf8(output.stdout).ok()?);
        let guid = text
            .lines()
            .find(|l| l.contains("MachineGuid"))
            .and_then(|l| l.split_whitespace().last())?;
        Some(Zeroizing::new(guid.to_string()))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

impl IdentitySource for GitConfigIdentity {
    fn git_author_name(&self) -> Option<String> {
        self.git_config("user.name")
    }

    fn git_email(&self) -> Option<String> {
        self.git_config("user.email")
    }

    fn machine_fingerprint(&self) -> Option<Zeroizing<String>> {
        machine_fingerprint_raw()
    }

    fn install_salt(&self) -> [u8; 32] {
        self.salt
    }

    fn env_developer_override(&self) -> Option<String> {
        std::env::var(ENV_DEVELOPER_OVERRIDE)
            .ok()
            .filter(|v| !v.trim().is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::identity::{binding_basis, snapshot, BindingBasis};
    use rift_core::Timestamp;

    /// The production source derives a usable snapshot inside a scratch repo
    /// with explicit local config — no panic, salted hashed ids, no raw
    /// fingerprint in Debug output. (Runs `git` locally only.)
    #[test]
    fn derives_identity_from_scratch_repo_config() {
        let tmp = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(tmp.path())
                .args(args)
                .output()
                .expect("git runs")
        };
        assert!(run(&["init", "-q"]).status.success());
        run(&["config", "user.name", "Test User"]);
        run(&["config", "user.email", "Test@Example.com"]);

        let src = GitConfigIdentity::new(tmp.path(), [7u8; 32]);
        assert_eq!(src.git_author_name().as_deref(), Some("Test User"));
        assert_eq!(src.git_email().as_deref(), Some("Test@Example.com"));

        let snap = snapshot(&src, Timestamp::from_millis(1));
        assert_eq!(snap.git_email_normalized, "test@example.com");
        let basis = binding_basis(&src);
        assert_eq!(basis, BindingBasis::GitAuthorMatch);

        // If the platform exposes a fingerprint, its raw value must not appear
        // in the snapshot's Debug output (only the salted hash does).
        if let Some(fp) = src.machine_fingerprint() {
            let dbg = format!("{snap:?}");
            assert!(!dbg.contains(fp.as_str()));
        }
    }

    #[test]
    fn missing_config_degrades_to_none_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        // Not even a git repo — every git read degrades to None.
        let src = GitConfigIdentity::new(tmp.path().join("nope"), [0u8; 32]);
        assert_eq!(src.git_config("user.email"), None);
    }
}
