//! The hermetic process environment every harness subprocess runs in.
//!
//! A benchmark number is only comparable if the environment is. Both the fixture
//! generator and the scenario driver launch children through [`Hermetic`], which:
//!
//! * points `git` at **stub config files this module writes** (never the
//!   developer's `~/.gitconfig` or `/etc/gitconfig`), pinning identity, line
//!   endings, `gc.auto`, and the default branch;
//! * scrubs the inherited `GIT_*` variables that silently re-target a repository
//!   (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, alternates, …);
//! * sets `protocol.allow = never` and `GIT_TERMINAL_PROMPT=0`, so nothing in the
//!   fixture plane can reach the network or block on a prompt;
//! * fixes `TZ`/`LC_ALL` so timestamps and sorting cannot drift between machines.
//!
//! Git hooks are disabled for fixture generation ([`Hooks::Disabled`]) and left
//! enabled for scenario runs ([`Hooks::Enabled`]) — condensation-at-commit *is*
//! one of the measures, and it rides the repository's own hooks.

use std::{
    ffi::OsStr,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::{BenchError, Result};

/// Author/committer identity baked into every generated commit.
pub const FIXTURE_IDENTITY: (&str, &str) = ("Rift Bench", "bench@rift.invalid");

/// `GIT_*` variables that would re-target or re-configure a child git.
const SCRUBBED: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CEILING_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_COMMON_DIR",
    "GIT_CONFIG",
    "GIT_ATTR_NOSYSTEM",
    "GIT_EXTERNAL_DIFF",
    "GIT_SSH_COMMAND",
];

/// Whether the child may run repository git hooks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hooks {
    /// Hooks are routed to an empty directory — fixture generation must be inert.
    Disabled,
    /// Repository hooks run normally — the scenario driver measures them.
    Enabled,
}

/// A prepared hermetic sandbox: stub config files plus an empty template and
/// hooks directory, all under one root.
#[derive(Debug, Clone)]
pub struct Hermetic {
    root: PathBuf,
}

impl Hermetic {
    /// Materialize the sandbox under `root` (idempotent; safe to call per run).
    ///
    /// # Errors
    /// [`BenchError::Io`] if the stub files cannot be written.
    pub fn prepare(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        let me = Self { root };
        fs::create_dir_all(me.template_dir())
            .map_err(|e| BenchError::io("hermetic template", e))?;
        fs::create_dir_all(me.empty_hooks_dir())
            .map_err(|e| BenchError::io("hermetic hooks", e))?;
        for hooks in [Hooks::Disabled, Hooks::Enabled] {
            let path = me.config_path(hooks);
            fs::write(&path, me.config_body(hooks))
                .map_err(|e| BenchError::io("hermetic gitconfig", e))?;
        }
        Ok(me)
    }

    /// The sandbox root.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// An empty `git init --template=` directory (no user templates, no hooks).
    #[must_use]
    pub fn template_dir(&self) -> PathBuf {
        self.root.join("template")
    }

    fn empty_hooks_dir(&self) -> PathBuf {
        self.root.join("no-hooks")
    }

    fn config_path(&self, hooks: Hooks) -> PathBuf {
        match hooks {
            Hooks::Disabled => self.root.join("gitconfig-inert"),
            Hooks::Enabled => self.root.join("gitconfig-live"),
        }
    }

    fn config_body(&self, hooks: Hooks) -> String {
        let (name, email) = FIXTURE_IDENTITY;
        let mut body = format!(
            "[user]\n\tname = {name}\n\temail = {email}\n\
             [core]\n\tautocrlf = false\n\tignorecase = false\n\tsymlinks = false\n\
             [commit]\n\tgpgsign = false\n[tag]\n\tgpgsign = false\n\
             [gc]\n\tauto = 0\n[init]\n\tdefaultBranch = main\n\
             [protocol]\n\tallow = never\n[advice]\n\tdetachedHead = false\n"
        );
        if hooks == Hooks::Disabled {
            let dir = self.empty_hooks_dir();
            body.push_str(&format!("[core]\n\thooksPath = {}\n", dir.display()));
        }
        body
    }

    /// Deterministic environment pairs for a child process.
    #[must_use]
    pub fn env(&self, hooks: Hooks) -> Vec<(String, String)> {
        let (name, email) = FIXTURE_IDENTITY;
        let cfg = self.config_path(hooks).display().to_string();
        [
            ("GIT_CONFIG_GLOBAL", cfg.as_str()),
            ("GIT_CONFIG_SYSTEM", cfg.as_str()),
            ("GIT_CONFIG_NOSYSTEM", "1"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_ASKPASS", ""),
            ("GIT_EDITOR", "true"),
            ("GIT_AUTHOR_NAME", name),
            ("GIT_AUTHOR_EMAIL", email),
            ("GIT_COMMITTER_NAME", name),
            ("GIT_COMMITTER_EMAIL", email),
            ("TZ", "UTC"),
            ("LC_ALL", "C"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect()
    }

    /// A [`Command`] for `program` in `cwd`, scrubbed and configured.
    ///
    /// The parent `PATH` is inherited on purpose: the harness measures binaries as
    /// a developer would invoke them.
    #[must_use]
    pub fn command(&self, program: impl AsRef<OsStr>, cwd: &Path, hooks: Hooks) -> Command {
        let mut cmd = Command::new(program);
        cmd.current_dir(cwd);
        for key in SCRUBBED {
            cmd.env_remove(key);
        }
        for (k, v) in self.env(hooks) {
            cmd.env(k, v);
        }
        cmd
    }
}

/// Format `epoch_secs` as a fixed-offset git date (`"<secs> +0000"`).
#[must_use]
pub fn git_date(epoch_secs: i64) -> String {
    format!("{epoch_secs} +0000")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_writes_both_config_stubs_and_only_one_pins_hooks() {
        let tmp = tempfile::tempdir().unwrap();
        let h = Hermetic::prepare(tmp.path()).unwrap();
        let inert = fs::read_to_string(h.config_path(Hooks::Disabled)).unwrap();
        let live = fs::read_to_string(h.config_path(Hooks::Enabled)).unwrap();
        assert!(inert.contains("hooksPath"));
        assert!(!live.contains("hooksPath"));
        for body in [&inert, &live] {
            assert!(body.contains("allow = never"), "no network transports");
            assert!(body.contains("bench@rift.invalid"));
        }
        assert!(h.template_dir().is_dir());
    }

    #[test]
    fn env_pins_identity_and_locale() {
        let tmp = tempfile::tempdir().unwrap();
        let h = Hermetic::prepare(tmp.path()).unwrap();
        let env = h.env(Hooks::Enabled);
        let get = |k: &str| {
            env.iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.clone())
                .unwrap()
        };
        assert_eq!(get("GIT_COMMITTER_NAME"), "Rift Bench");
        assert_eq!(get("TZ"), "UTC");
        assert_eq!(get("LC_ALL"), "C");
        assert_eq!(get("GIT_TERMINAL_PROMPT"), "0");
        assert_eq!(get("GIT_CONFIG_GLOBAL"), get("GIT_CONFIG_SYSTEM"));
    }

    #[test]
    fn git_date_is_fixed_offset() {
        assert_eq!(git_date(1_700_000_000), "1700000000 +0000");
    }
}
