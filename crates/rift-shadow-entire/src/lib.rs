//! v0.1 silent capture-backend wrapper.
//!
//! Rift's v0.1 system-of-record wraps a separately-installed Go capture binary
//! (the Entire CLI), but **never surfaces it**: it is installed silently, pinned
//! per Rift release by SHA-256, invoked by absolute path with a cleared
//! environment, and any cloud/auth verb is refused before spawn. All output is
//! rebranded so the upstream name never reaches the user (founding principle 7).
//!
//! All invocations go through the [`EntireRunner`] seam (never
//! `std::process::Command` directly elsewhere). Tests use [`MockRunner`]; the
//! real path is [`SystemRunner`].
//!
//! This module is pure/seam logic: intent → argv translation, the default-deny
//! verb screen, version-pin verification, and output rebranding. The actual
//! subprocess spawn lives behind the trait so it is testable offline.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]
#![allow(clippy::module_name_repetitions, clippy::doc_markdown)]

use std::path::{Path, PathBuf};

use rift_core::Sha256Id;
use sha2::{Digest, Sha256};

/// Errors from the capture-backend seam. Messages are Rift-voiced and never name
/// the upstream tool.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SubstrateError {
    /// The capture backend is not available / failed to run.
    #[error("capture backend unavailable")]
    Unavailable,
    /// The on-disk binary did not match the pinned hash.
    #[error("capture backend integrity check failed")]
    Integrity,
    /// The current platform is unsupported.
    #[error("unsupported platform for capture backend")]
    Platform,
    /// A cloud/auth/network verb was refused (local-only).
    #[error("operation not supported")]
    Blocked,
    /// The backend timed out.
    #[error("capture backend timed out")]
    Timeout,
}

/// A bounded set of local-only capture intents. A closed enum so an unknown or
/// dangerous verb cannot be smuggled through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureIntent {
    /// Initialise capture in a repo.
    Init {
        /// Repo root.
        repo: PathBuf,
    },
    /// Install the git/agent hooks.
    InstallHooks {
        /// Repo root.
        repo: PathBuf,
    },
    /// Archive a checkpoint for a commit.
    ArchiveCheckpoint {
        /// Repo root.
        repo: PathBuf,
        /// The commit oid (validated hex).
        commit: String,
    },
    /// Query status.
    Status {
        /// Repo root.
        repo: PathBuf,
    },
    /// Report the backend version (internally; never surfaced raw).
    Version,
}

impl CaptureIntent {
    /// The argv this intent translates to, with local/offline flags always
    /// forced (belt-and-suspenders with the egress block).
    ///
    /// # Errors
    /// [`SubstrateError::Blocked`] if a commit oid is not valid hex.
    pub fn to_argv(&self) -> Result<Vec<String>, SubstrateError> {
        let argv = match self {
            Self::Init { .. } => vec!["init".into(), "--no-telemetry".into(), "--local".into()],
            Self::InstallHooks { .. } => {
                vec!["hooks".into(), "install".into(), "--abs-paths".into()]
            }
            Self::ArchiveCheckpoint { commit, .. } => {
                if commit.is_empty() || !commit.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(SubstrateError::Blocked);
                }
                vec![
                    "checkpoint".into(),
                    "archive".into(),
                    "--commit".into(),
                    commit.clone(),
                ]
            }
            Self::Status { .. } => vec!["status".into(), "--json".into()],
            Self::Version => vec!["version".into()],
        };
        Ok(argv)
    }
}

/// Verbs allowed in local-only mode.
pub const ALLOWED_LOCAL_VERBS: &[&str] = &["init", "hooks", "checkpoint", "status", "version"];

/// Verbs always refused (cloud/auth/network surface).
pub const FORBIDDEN_VERBS: &[&str] = &[
    "auth", "login", "logout", "cloud", "sync", "push", "pull", "remote", "share", "team", "org",
    "upload", "publish",
];

fn arg_is_networky(arg: &str) -> bool {
    let a = arg.to_ascii_lowercase();
    a.starts_with("http://")
        || a.starts_with("https://")
        || a.contains("--remote")
        || a.contains("--token")
        || a.contains("--cloud")
}

/// Screen an argv before spawn: default-deny. The root verb must be allowed and
/// not forbidden, and no argument may be network-shaped.
///
/// # Errors
/// [`SubstrateError::Blocked`] if the invocation is not a permitted local verb.
pub fn screen(argv: &[String]) -> Result<(), SubstrateError> {
    let root = argv
        .iter()
        .find(|a| !a.starts_with('-'))
        .map_or("", String::as_str);
    let root_lc = root.to_ascii_lowercase();
    if FORBIDDEN_VERBS.contains(&root_lc.as_str()) {
        return Err(SubstrateError::Blocked);
    }
    if !ALLOWED_LOCAL_VERBS.contains(&root_lc.as_str()) {
        return Err(SubstrateError::Blocked);
    }
    if argv.iter().any(|a| arg_is_networky(a)) {
        return Err(SubstrateError::Blocked);
    }
    Ok(())
}

/// Verify a binary on disk matches the pinned SHA-256. Refuses any mismatch
/// *before* the binary is ever executed (supply-chain control).
///
/// # Errors
/// [`SubstrateError::Unavailable`] if the file cannot be read,
/// [`SubstrateError::Integrity`] if the hash does not match.
pub fn verify_binary_sha256(path: &Path, expected: &Sha256Id) -> Result<(), SubstrateError> {
    let bytes = std::fs::read(path).map_err(|_| SubstrateError::Unavailable)?;
    let mut h = Sha256::new();
    h.update(&bytes);
    let out = h.finalize();
    let mut got = [0u8; 32];
    got.copy_from_slice(&out);
    if &Sha256Id::from_bytes(got) == expected {
        Ok(())
    } else {
        Err(SubstrateError::Integrity)
    }
}

/// Forbidden brand tokens that must never reach the user.
const FORBIDDEN_BRAND_TOKENS: &[&str] = &["entire", "entireio"];

/// Rewrite captured output so the upstream brand never leaks. Replaces known
/// brand tokens with "Rift"/"rift" and **fails closed**: if a forbidden token
/// survives rewriting, the whole line is suppressed rather than risk a leak.
#[must_use]
pub fn rebrand(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        // case-insensitive replace of the brand stem
        let rewritten = replace_ci(line, "entireio", "rift");
        let rewritten = replace_ci(&rewritten, "entire", "Rift");
        if FORBIDDEN_BRAND_TOKENS
            .iter()
            .any(|t| rewritten.to_ascii_lowercase().contains(t))
        {
            // fail closed: drop the line
            continue;
        }
        out.push_str(&rewritten);
        out.push('\n');
    }
    out
}

fn replace_ci(haystack: &str, needle: &str, replacement: &str) -> String {
    let mut result = String::with_capacity(haystack.len());
    let lower = haystack.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    let mut idx = 0;
    while idx < haystack.len() {
        if lower[idx..].starts_with(&needle_lower) {
            result.push_str(replacement);
            idx += needle.len();
        } else {
            // advance by one char (UTF-8 safe)
            let ch = haystack[idx..].chars().next().unwrap_or('\u{FFFD}');
            result.push(ch);
            idx += ch.len_utf8();
        }
    }
    result
}

/// Normalized exit classification.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitClass {
    /// Success.
    Ok,
    /// Bad usage (exit 2).
    BadUsage,
    /// Nothing to do.
    NothingToDo,
    /// Backend internal failure (crash / unknown code).
    BackendInternal,
}

impl ExitClass {
    /// Classify a raw process exit code.
    #[must_use]
    pub fn from_code(code: Option<i32>) -> Self {
        match code {
            Some(0) => Self::Ok,
            Some(2) => Self::BadUsage,
            Some(3) => Self::NothingToDo,
            _ => Self::BackendInternal,
        }
    }
}

/// The result of a wrapped invocation. Stdout is already rebranded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunOutcome {
    /// Normalized exit class.
    pub exit: ExitClass,
    /// Rebranded stdout.
    pub stdout: String,
}

/// The wrapper seam. All capture-backend invocations go through this trait.
pub trait EntireRunner {
    /// Run a capture intent.
    ///
    /// # Errors
    /// [`SubstrateError`] on screen/spawn/integrity failure.
    fn run(&self, intent: &CaptureIntent) -> Result<RunOutcome, SubstrateError>;
}

/// A deterministic test double.
#[derive(Default)]
pub struct MockRunner {
    /// Calls observed, for assertions.
    pub calls: std::cell::RefCell<Vec<CaptureIntent>>,
}

impl EntireRunner for MockRunner {
    fn run(&self, intent: &CaptureIntent) -> Result<RunOutcome, SubstrateError> {
        let argv = intent.to_argv()?;
        screen(&argv)?;
        self.calls.borrow_mut().push(intent.clone());
        Ok(RunOutcome {
            exit: ExitClass::Ok,
            stdout: String::new(),
        })
    }
}

/// The production runner: spawns the pinned binary by absolute path with a
/// cleared environment, screening the verb first. (Spawn is exercised by the
/// integration suite against a fake backend script, not in unit tests.)
pub struct SystemRunner {
    /// Absolute path to the pinned, verified backend binary.
    pub binary: PathBuf,
}

impl EntireRunner for SystemRunner {
    fn run(&self, intent: &CaptureIntent) -> Result<RunOutcome, SubstrateError> {
        let argv = intent.to_argv()?;
        screen(&argv)?;
        let cwd = match intent {
            CaptureIntent::Init { repo }
            | CaptureIntent::InstallHooks { repo }
            | CaptureIntent::ArchiveCheckpoint { repo, .. }
            | CaptureIntent::Status { repo } => repo.clone(),
            CaptureIntent::Version => {
                std::env::current_dir().map_err(|_| SubstrateError::Unavailable)?
            }
        };
        let output = std::process::Command::new(&self.binary)
            .args(&argv)
            .current_dir(&cwd)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .output()
            .map_err(|_| SubstrateError::Unavailable)?;
        Ok(RunOutcome {
            exit: ExitClass::from_code(output.status.code()),
            stdout: rebrand(&output.stdout),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translate_builds_expected_argv_with_local_flags() {
        let argv = CaptureIntent::Init { repo: "/r".into() }.to_argv().unwrap();
        assert_eq!(argv, vec!["init", "--no-telemetry", "--local"]);
        let argv = CaptureIntent::Status { repo: "/r".into() }
            .to_argv()
            .unwrap();
        assert!(argv.contains(&"--json".to_string()));
    }

    #[test]
    fn archive_validates_commit_oid() {
        assert!(CaptureIntent::ArchiveCheckpoint {
            repo: "/r".into(),
            commit: "deadbeef".into()
        }
        .to_argv()
        .is_ok());
        assert!(matches!(
            CaptureIntent::ArchiveCheckpoint {
                repo: "/r".into(),
                commit: "not hex!".into()
            }
            .to_argv(),
            Err(SubstrateError::Blocked)
        ));
    }

    #[test]
    fn screen_blocks_auth_and_network() {
        assert!(screen(&["auth".into(), "login".into()]).is_err());
        assert!(screen(&["login".into()]).is_err());
        assert!(screen(&["sync".into()]).is_err());
        assert!(screen(&["status".into(), "--remote".into()]).is_err());
        assert!(screen(&["status".into(), "https://evil".into()]).is_err());
        assert!(screen(&["init".into(), "--local".into()]).is_ok());
        assert!(screen(&["nonexistent_verb".into()]).is_err()); // default-deny
    }

    #[test]
    fn verify_binary_sha256_detects_tamper() {
        let dir = std::env::temp_dir().join(format!("rift-entire-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("backend");
        std::fs::write(&path, b"good binary").unwrap();
        // compute expected
        let mut h = Sha256::new();
        h.update(b"good binary");
        let mut exp = [0u8; 32];
        exp.copy_from_slice(&h.finalize());
        let expected = Sha256Id::from_bytes(exp);
        assert!(verify_binary_sha256(&path, &expected).is_ok());
        // tamper
        std::fs::write(&path, b"evil binary").unwrap();
        assert!(matches!(
            verify_binary_sha256(&path, &expected),
            Err(SubstrateError::Integrity)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rebrand_removes_upstream_brand() {
        let out = rebrand(b"Welcome to Entire CLI v0.6.2\ncheckpoint archived\n");
        assert!(!out.to_ascii_lowercase().contains("entire"));
        assert!(out.contains("checkpoint archived"));
    }

    #[test]
    fn exit_code_mapping_is_total() {
        assert_eq!(ExitClass::from_code(Some(0)), ExitClass::Ok);
        assert_eq!(ExitClass::from_code(Some(2)), ExitClass::BadUsage);
        assert_eq!(ExitClass::from_code(None), ExitClass::BackendInternal);
        assert_eq!(ExitClass::from_code(Some(134)), ExitClass::BackendInternal);
    }

    #[test]
    fn mock_runner_screens_and_records() {
        let r = MockRunner::default();
        r.run(&CaptureIntent::Status { repo: "/r".into() }).unwrap();
        assert_eq!(r.calls.borrow().len(), 1);
    }

    proptest::proptest! {
        /// Rebranded output never contains a forbidden brand token.
        #[test]
        fn rebrand_never_leaks_brand(s in ".{0,200}") {
            let out = rebrand(s.as_bytes());
            let lower = out.to_ascii_lowercase();
            for t in FORBIDDEN_BRAND_TOKENS {
                proptest::prop_assert!(!lower.contains(t));
            }
        }
    }
}
