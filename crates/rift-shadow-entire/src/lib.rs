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
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rift_core::Sha256Id;
use sha2::{Digest, Sha256};

pub mod pin;
pub use pin::{InstallReport, PinnedSubstrate, SubstrateAsset, SubstrateFetcher};

/// Production HTTP(S) download (the only network call). Behind `provision-http`
/// so the default build/tests stay network-free and use the in-memory fetcher.
#[cfg(feature = "provision-http")]
pub mod fetch;
#[cfg(feature = "provision-http")]
pub use fetch::HttpFetcher;

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

/// Limits applied to a wrapped spawn so a hung or chatty backend can never wedge
/// or flood the developer's git op.
#[derive(Clone, Copy, Debug)]
pub struct SpawnLimits {
    /// Wall-clock budget for the child. (Enforcement of the hard kill lands with
    /// the real spawn at release-cut; carried here so the contract is fixed now.)
    pub wall: Duration,
    /// Cap on captured stdout bytes (truncated past this; applied today).
    pub max_output_bytes: usize,
}

impl Default for SpawnLimits {
    fn default() -> Self {
        Self {
            wall: Duration::from_secs(30),
            max_output_bytes: 1 << 20, // 1 MiB
        }
    }
}

/// The minimal, scrubbed environment the wrapped child runs with (P1 S3 egress
/// layer). We start from an empty env (`env_clear`, dropping any `*_TOKEN`/
/// `*_API_*`/telemetry vars) and add only a sanitized `PATH` plus **dead proxies**
/// so a naive HTTP client in the child dead-ends — belt-and-suspenders with the
/// arg-level screen. The OS-level network sandbox (namespace / `sandbox-exec`)
/// is the deferred, feature-gated hardening that composes on top at release-cut;
/// the fail-closed rule is *no network*, never "fall open to a networked child".
#[must_use]
pub fn scrubbed_capture_env() -> Vec<(&'static str, &'static str)> {
    const DEAD: &str = "http://127.0.0.1:0";
    vec![
        ("PATH", "/usr/bin:/bin"),
        ("HTTP_PROXY", DEAD),
        ("HTTPS_PROXY", DEAD),
        ("ALL_PROXY", DEAD),
        ("http_proxy", DEAD),
        ("https_proxy", DEAD),
        ("all_proxy", DEAD),
        ("RIFT_LOCAL_ONLY", "1"),
        ("DO_NOT_TRACK", "1"),
    ]
}

/// The production runner: spawns the pinned binary by absolute path with a
/// cleared+scrubbed environment, screening the verb first. (Spawn is exercised by
/// the integration suite against a fake backend script, not in unit tests; the
/// hard wall-clock kill + OS sandbox land with the real provision at release-cut.)
pub struct SystemRunner {
    /// Absolute path to the pinned, verified backend binary.
    pub binary: PathBuf,
    /// Spawn limits (output cap today; wall-kill at release-cut).
    pub limits: SpawnLimits,
}

impl SystemRunner {
    /// A runner for the verified `binary` with default [`SpawnLimits`].
    #[must_use]
    pub fn new(binary: PathBuf) -> Self {
        Self {
            binary,
            limits: SpawnLimits::default(),
        }
    }
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
        // Spawn with stdin closed and stdout/stderr piped: stdin null so the child
        // can never block on a prompt; stdout drained on a thread so a full pipe
        // can't wedge the child; stderr null so its bytes never reach the user.
        let mut child = Command::new(&self.binary)
            .args(&argv)
            .current_dir(&cwd)
            .env_clear()
            .envs(scrubbed_capture_env())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| SubstrateError::Unavailable)?;

        // Drain stdout on a thread and hand the captured (cap-bounded) bytes back
        // over a channel the moment they're read, so a backend that spawned a
        // grandchild holding the pipe open can never make us block on `join`.
        let stdout = child.stdout.take();
        let cap = u64::try_from(self.limits.max_output_bytes).unwrap_or(u64::MAX);
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut out) = stdout {
                {
                    let mut limited = (&mut out).take(cap);
                    let _ = limited.read_to_end(&mut buf);
                }
                let _ = tx.send(buf); // deliver the captured output ASAP…
                let _ = std::io::copy(&mut out, &mut std::io::sink()); // …then drain.
            } else {
                let _ = tx.send(buf);
            }
        });

        // Poll for exit until the wall-clock budget, then hard-kill + reap. A hung
        // or runaway backend can never wedge the developer's git op past `wall`.
        let deadline = Instant::now() + self.limits.wall;
        let exit = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(status.code()),
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(SubstrateError::Timeout);
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                // A wait error: kill + reap so we never leave a zombie, then bail.
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break Err(SubstrateError::Unavailable);
                }
            }
        };

        // Collect captured output with a short grace. The reader sends as soon as it
        // has read the cap-bounded slice; if a surviving grandchild still holds the
        // pipe it never sends, so we proceed with what we have rather than hang.
        let bytes = rx
            .recv_timeout(Duration::from_millis(250))
            .unwrap_or_default();
        let code = exit?;
        Ok(RunOutcome {
            exit: ExitClass::from_code(code),
            stdout: rebrand(&bytes),
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
    fn scrubbed_env_dead_ends_network_and_drops_secrets() {
        let env = scrubbed_capture_env();
        let map: std::collections::HashMap<_, _> = env.iter().copied().collect();
        // naive HTTP clients dead-end on an unreachable proxy
        assert_eq!(map.get("HTTP_PROXY"), Some(&"http://127.0.0.1:0"));
        assert_eq!(map.get("HTTPS_PROXY"), Some(&"http://127.0.0.1:0"));
        assert!(map.contains_key("PATH"));
        // the allowlist carries no secret-shaped vars (env_clear drops the rest)
        assert!(!env
            .iter()
            .any(|(k, _)| k.to_ascii_uppercase().contains("TOKEN")
                || k.to_ascii_uppercase().contains("API")));
    }

    #[test]
    fn mock_runner_screens_and_records() {
        let r = MockRunner::default();
        r.run(&CaptureIntent::Status { repo: "/r".into() }).unwrap();
        assert_eq!(r.calls.borrow().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn system_runner_spawns_real_binary_scrubbed_and_rebrands() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-backend");
        let marker = dir.path().join("ran.marker");
        {
            let mut f = std::fs::File::create(&script).unwrap();
            // Prove it ran (write a marker), echo a *branded* line (to be rewritten)
            // and the scrubbed proxy env (to prove env_clear + scrubbed env applied).
            writeln!(
                f,
                "#!/bin/sh\ntouch '{}'\necho 'Entire archived checkpoint'\necho \"proxy=$HTTP_PROXY\"\nexit 0",
                marker.display()
            )
            .unwrap();
        }
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = SystemRunner::new(script);
        let out = runner
            .run(&CaptureIntent::ArchiveCheckpoint {
                repo: dir.path().to_path_buf(),
                commit: "deadbeef".into(),
            })
            .unwrap();

        assert_eq!(out.exit, ExitClass::Ok, "exit normalized to Ok");
        assert!(
            marker.exists(),
            "the real binary was spawned by absolute path"
        );
        // The upstream brand is rewritten and never leaks.
        assert!(!out.stdout.to_ascii_lowercase().contains("entire"));
        assert!(out.stdout.to_ascii_lowercase().contains("checkpoint"));
        // The scrubbed env reached the child: HTTP_PROXY is the dead proxy.
        assert!(
            out.stdout.contains("127.0.0.1:0"),
            "child ran under the scrubbed egress env: {:?}",
            out.stdout
        );
    }

    #[cfg(unix)]
    #[test]
    fn system_runner_does_not_hang_on_grandchild_holding_stdout() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-backend");
        {
            let mut f = std::fs::File::create(&script).unwrap();
            // The direct child exits 0, but backgrounds a `sleep` that INHERITS the
            // stdout pipe — the classic case that would block a naive `join` on the
            // reader. The runner must still return promptly (via the recv grace).
            f.write_all(b"#!/bin/sh\nsleep 30 &\necho hi\nexit 0\n")
                .unwrap();
        }
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let runner = SystemRunner::new(script);
        let start = Instant::now();
        let out = runner
            .run(&CaptureIntent::Status {
                repo: dir.path().to_path_buf(),
            })
            .unwrap();
        // Clean exit is reported, and we returned in well under the 30s grandchild
        // sleep (the reader's pipe never EOFs, so we proceed after the recv grace).
        assert_eq!(out.exit, ExitClass::Ok);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "must not block on a grandchild holding the pipe"
        );
    }

    #[cfg(unix)]
    #[test]
    fn system_runner_hard_kills_on_timeout() {
        use std::io::Write as _;
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake-backend");
        {
            let mut f = std::fs::File::create(&script).unwrap();
            // `exec` so the killed child IS the single process holding the pipe —
            // killing it closes stdout promptly (no orphaned writer to wait on).
            f.write_all(b"#!/bin/sh\nexec sleep 30\n").unwrap();
        }
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let runner = SystemRunner {
            binary: script,
            limits: SpawnLimits {
                wall: Duration::from_millis(150),
                max_output_bytes: 1 << 20,
            },
        };
        let start = Instant::now();
        let res = runner.run(&CaptureIntent::Status {
            repo: dir.path().to_path_buf(),
        });
        assert!(matches!(res, Err(SubstrateError::Timeout)));
        // Returned near the 150ms budget, not after the 30s sleep.
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "must hard-kill near the wall budget"
        );
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
