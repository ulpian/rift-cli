//! The production [`IdentitySource`]: git config + a per-OS machine
//! fingerprint.
//!
//! Reads are strictly **local** — git config via an argv-array `git` shell-out
//! (no shell, no network), the machine fingerprint from the platform's local
//! identifier store — honoring the trait's no-network contract. The raw
//! fingerprint is held in a [`Zeroizing`] buffer for the source's lifetime and
//! wiped when it drops (each caller gets its own zeroizing copy); only the
//! salted hash ever leaves `rift-core`'s derivation. The per-install salt is
//! injected by the caller (custodied in the OS keychain by `bins/rift`),
//! keeping this crate keychain-blind.
//!
//! # One instance, one observation
//!
//! Each of the three signals is read **at most once per
//! [`GitConfigIdentity`]**, then memoized. That is not a cache with an
//! invalidation story: an instance exists for the length of a single identity
//! derivation (one `bind_new_sessions` call in one short-lived hook process),
//! and `rift-core`'s derivation already stamps the whole observation at one
//! `at` timestamp. Re-reading mid-derivation could only produce an internally
//! inconsistent snapshot — a developer id from a new git config beside a basis
//! computed from the old one. A genuinely changed config is picked up by the
//! next instance, which in production is the next hook process.
//!
//! What it buys: `rift-core`'s derivation asks for the three signals eight
//! times over (`snapshot` reads all three, `binding_evidence` calls `snapshot`
//! again and then `binding_basis` re-reads two), and every read is a child
//! process — two `git config` spawns and, on macOS, an `ioreg`. Eight spawns
//! become three, on the first hook of every session.

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use rift_core::identity::{IdentitySource, MachineId};
use zeroize::Zeroizing;

/// Environment variable for an explicit identity override (e.g. CI bots).
pub const ENV_DEVELOPER_OVERRIDE: &str = "RIFT_DEVELOPER";

/// Production identity inputs: git `user.name`/`user.email` scoped to a
/// worktree, the platform machine id, and an injected install salt.
///
/// Each signal is observed at most once per instance (see the module docs).
pub struct GitConfigIdentity {
    work_tree: PathBuf,
    salt: [u8; 32],
    /// `user.name`, observed lazily and at most once.
    author_name: OnceLock<Option<String>>,
    /// `user.email`, observed lazily and at most once.
    email: OnceLock<Option<String>>,
    /// The platform fingerprint, observed lazily and at most once. Held
    /// zeroizing, so the raw value is wiped when this source drops.
    fingerprint: OnceLock<Option<Zeroizing<String>>>,
    /// A salted machine id the caller already holds (its cache of an earlier
    /// derivation under the same salt). Present, the fingerprint is never
    /// read: on macOS that read is an `ioreg` child process on every
    /// session's first hook, and the digest it feeds does not change.
    cached_machine_id: Option<MachineId>,
    /// Signals actually read from the machine — the instrument behind the
    /// "one instance, one observation" tests. Every one of them is a child
    /// process except the Linux fingerprint, which is a file read.
    #[cfg(test)]
    observations: std::sync::atomic::AtomicUsize,
}

impl GitConfigIdentity {
    /// An identity source for the repo at `work_tree` with the given
    /// per-install `salt` (provision it from the OS keychain).
    #[must_use]
    pub fn new(work_tree: impl Into<PathBuf>, salt: [u8; 32]) -> Self {
        Self {
            work_tree: work_tree.into(),
            salt,
            author_name: OnceLock::new(),
            email: OnceLock::new(),
            fingerprint: OnceLock::new(),
            cached_machine_id: None,
            #[cfg(test)]
            observations: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Answer [`IdentitySource::machine_id`] with `id` instead of deriving it,
    /// so no fingerprint is read. The caller vouches that `id` is what the
    /// derivation under this source's salt produced (see `bins/rift`'s
    /// machine-id cache, which stores only that digest and only under a tag
    /// of the salt it was made with).
    #[must_use]
    pub fn with_machine_id(mut self, id: MachineId) -> Self {
        self.cached_machine_id = Some(id);
        self
    }

    /// How many signals this source has read from the machine so far.
    #[cfg(test)]
    fn observation_count(&self) -> usize {
        self.observations.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The value of `<key>` in the worktree's effective git config. `None`
    /// on any failure — identity derivation degrades honestly, never errors.
    ///
    /// Read in-process through gix first (the resolved config git itself
    /// would read: local, global, system, includes), so a session's first
    /// hook does not spawn a `git config` process per signal — the two spawns
    /// were most of the `identity-attribution` row. A gix open that fails, or
    /// a key gix does not find, falls back to the `git config --get` spawn
    /// (argv array, no shell), so the answer is never narrower than before.
    ///
    /// Uncached by design: the memo lives on the two trait methods that name a
    /// fixed key, so a caller asking for an arbitrary key still gets a fresh
    /// read and cannot poison either of them.
    fn git_config(&self, key: &str) -> Option<String> {
        #[cfg(test)]
        self.observations
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        #[cfg(feature = "gix")]
        if let Some(value) = self.gix_config(key) {
            return Some(value);
        }
        self.git_config_spawn(key)
    }

    /// The in-process read: `None` when the repository cannot be opened or
    /// the key is absent or empty, and the spawn decides.
    #[cfg(feature = "gix")]
    fn gix_config(&self, key: &str) -> Option<String> {
        let repo = gix::open(&self.work_tree).ok()?;
        let value = repo.config_snapshot().string(key)?;
        let trimmed = value.to_string().trim().to_owned();
        (!trimmed.is_empty()).then_some(trimmed)
    }

    /// `git config --get <key>` in the worktree (argv array, no shell).
    fn git_config_spawn(&self, key: &str) -> Option<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(crate::child_process_path(&self.work_tree))
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
        self.author_name
            .get_or_init(|| self.git_config("user.name"))
            .clone()
    }

    fn git_email(&self) -> Option<String> {
        self.email
            .get_or_init(|| self.git_config("user.email"))
            .clone()
    }

    fn machine_fingerprint(&self) -> Option<Zeroizing<String>> {
        self.fingerprint
            .get_or_init(|| {
                #[cfg(test)]
                self.observations
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                machine_fingerprint_raw()
            })
            // Each caller gets its own zeroizing copy; the memoized original is
            // wiped when this source drops.
            .as_ref()
            .map(|fp| Zeroizing::new(fp.as_str().to_owned()))
    }

    fn install_salt(&self) -> [u8; 32] {
        self.salt
    }

    fn machine_id(&self) -> Option<MachineId> {
        if let Some(id) = self.cached_machine_id {
            return Some(id);
        }
        self.machine_fingerprint()
            .map(|fp| rift_core::identity::derive_machine_id(&self.salt, &fp))
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
    use rift_core::identity::{binding_basis, binding_evidence, snapshot, BindingBasis};
    use rift_core::Timestamp;

    /// A scratch repo with local `user.name`/`user.email`, plus a closure that
    /// runs further `git` commands in it.
    fn scratch_repo() -> (tempfile::TempDir, impl Fn(&[&str])) {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_path_buf();
        let run = move |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(&path)
                .args(args)
                .output()
                .expect("git runs");
        };
        run(&["init", "-q"]);
        run(&["config", "user.name", "Test User"]);
        run(&["config", "user.email", "Test@Example.com"]);
        (tmp, run)
    }

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

    /// The in-process read and the spawn answer the same question the same
    /// way: what git resolves for the key in this worktree. Both are asked
    /// directly, so a gix that read a different file set, trimmed
    /// differently, or missed a value set the way `git config` sets it would
    /// red here rather than change developer ids quietly.
    #[cfg(feature = "gix")]
    #[test]
    fn the_in_process_read_agrees_with_git_config() {
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
        run(&["config", "user.name", "  Ada Lovelace "]);
        run(&["config", "user.email", "ada@example.invalid"]);
        let src = GitConfigIdentity::new(tmp.path(), [3u8; 32]);
        for key in ["user.name", "user.email"] {
            let spawned = src.git_config_spawn(key);
            assert!(spawned.is_some(), "{key} was set");
            assert_eq!(src.gix_config(key), spawned, "{key}");
        }
        assert_eq!(src.gix_config("user.absent"), None);
        assert_eq!(
            src.git_config("user.absent"),
            None,
            "the fallback agrees too"
        );
        assert_eq!(src.git_author_name().as_deref(), Some("Ada Lovelace"));
    }

    #[test]
    fn missing_config_degrades_to_none_not_error() {
        let tmp = tempfile::tempdir().unwrap();
        // Not even a git repo — every git read degrades to None.
        let src = GitConfigIdentity::new(tmp.path().join("nope"), [0u8; 32]);
        assert_eq!(src.git_config("user.email"), None);
    }

    /// The whole derivation reads each signal exactly once, however many times
    /// `rift-core` asks for it. Before the memo this sequence cost eight child
    /// processes, and process spawns are the dominant cost the hook budget is
    /// spent on.
    #[test]
    fn the_whole_derivation_observes_each_signal_once() {
        let (tmp, _run) = scratch_repo();
        let src = GitConfigIdentity::new(tmp.path(), [7u8; 32]);
        let at = Timestamp::from_millis(1);

        // Exactly what `bind_new_sessions` does, in order.
        let _ = snapshot(&src, at);
        let _ = binding_evidence(&src, at);

        // Three signals — unless an override is set in this test environment,
        // in which case `snapshot` never asks for the email and `binding_basis`
        // returns before touching git at all.
        let expected = 3 - usize::from(src.env_developer_override().is_some());
        assert_eq!(src.observation_count(), expected);

        // Asking again costs nothing at all.
        let _ = snapshot(&src, at);
        let _ = binding_basis(&src);
        let _ = src.machine_fingerprint();
        assert_eq!(src.observation_count(), expected);
    }

    /// Memoized ≡ unmemoized: a source that has already observed derives what a
    /// freshly-constructed one derives. A fresh instance *is* the unmemoized
    /// reference — it has read nothing yet.
    #[test]
    fn a_memoized_source_derives_what_a_fresh_one_does() {
        let (tmp, _run) = scratch_repo();
        let at = Timestamp::from_millis(42);
        let warm = GitConfigIdentity::new(tmp.path(), [7u8; 32]);
        let first = snapshot(&warm, at);
        let fresh = || GitConfigIdentity::new(tmp.path(), [7u8; 32]);

        assert_eq!(first, snapshot(&fresh(), at));
        assert_eq!(snapshot(&warm, at), first, "repeat asks are stable");
        assert_eq!(binding_evidence(&warm, at), binding_evidence(&fresh(), at));
        assert_eq!(binding_basis(&warm), binding_basis(&fresh()));
        assert_eq!(
            warm.machine_fingerprint().as_deref(),
            fresh().machine_fingerprint().as_deref()
        );
        assert_eq!(warm.git_email(), fresh().git_email());
        assert_eq!(warm.git_author_name(), fresh().git_author_name());
    }

    /// The memo is scoped to the instance, not to the process. That is what
    /// makes staleness unreachable: production builds one source per
    /// `bind_new_sessions` call, so "the next instance" is the next hook.
    #[test]
    fn a_new_instance_re_observes_a_changed_config() {
        let (tmp, run) = scratch_repo();
        let src = GitConfigIdentity::new(tmp.path(), [7u8; 32]);
        assert_eq!(src.git_email().as_deref(), Some("Test@Example.com"));

        run(&["config", "user.email", "moved@example.com"]);
        assert_eq!(
            src.git_email().as_deref(),
            Some("Test@Example.com"),
            "one observation per instance, even when the config moves under it"
        );
        assert_eq!(
            src.observation_count(),
            1,
            "and no second read was paid for"
        );

        let next = GitConfigIdentity::new(tmp.path(), [7u8; 32]);
        assert_eq!(
            next.git_email().as_deref(),
            Some("moved@example.com"),
            "a process-wide cache would have answered with the stale value"
        );
    }

    /// An injected machine id is answered without a single fingerprint read,
    /// and it is the id the derivation would have produced — the evidence a
    /// session bound before the cache existed is the evidence after it.
    #[test]
    fn an_injected_machine_id_costs_no_observation_and_matches_the_derivation() {
        let (tmp, _run) = scratch_repo();
        let at = Timestamp::from_millis(9);
        let derived = GitConfigIdentity::new(tmp.path(), [7u8; 32]);
        let Some(id) = derived.machine_id() else {
            // A platform with no fingerprint has nothing to cache; the
            // derivation stays honest about that.
            return;
        };
        assert_eq!(derived.observation_count(), 1, "one fingerprint read");

        let cached = GitConfigIdentity::new(tmp.path(), [7u8; 32]).with_machine_id(id);
        assert_eq!(cached.machine_id(), Some(id));
        assert_eq!(cached.observation_count(), 0, "the cache answered");
        assert_eq!(
            binding_evidence(&cached, at),
            binding_evidence(&derived, at)
        );
        assert_eq!(snapshot(&cached, at).machine, id);
        assert_eq!(
            cached.observation_count(),
            2,
            "the whole derivation read user.name and user.email and nothing else"
        );
    }

    /// A source that can read nothing memoizes the absence too — a missing
    /// signal must not be re-probed on every ask.
    #[test]
    fn absent_signals_are_observed_once_as_well() {
        let tmp = tempfile::tempdir().unwrap();
        let src = GitConfigIdentity::new(tmp.path().join("nope"), [0u8; 32]);
        assert_eq!(src.git_email(), None);
        assert_eq!(src.git_author_name(), None);
        assert_eq!(src.git_email(), None);
        assert_eq!(src.git_author_name(), None);
        assert_eq!(src.observation_count(), 2);
    }
}
