//! The three clients a dogfood run can drive, and how each is driven.
//!
//! The argument vectors are the load-bearing part: they are the least
//! permission that still lets a client read, edit, glob and grep in a scratch
//! checkout, and each one asks for that client's own structured event stream.
//! The shell arm's vector differs from the on arm's in the tools alone, and
//! which clients play that arm is decided in one place, [`Client::shell_arm`].

use std::path::Path;

use crate::dogfood_compare::Arm;

/// The three clients this harness can drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Client {
    /// Claude Code (`claude`).
    ClaudeCode,
    /// Codex (`codex`).
    Codex,
    /// Antigravity (`agy`).
    Antigravity,
}

/// Whether a client plays the shell arm, and why not when it does not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellArm {
    /// The client has a read tool a pinned flag can withhold, so the arm
    /// plays.
    Plays,
    /// The arm is not played, and the sentence the run prints says why.
    Skipped(&'static str),
}

/// Every client, in the order the table prints them.
pub const ALL_CLIENTS: [Client; 3] = [Client::ClaudeCode, Client::Codex, Client::Antigravity];

impl Client {
    /// The name on the command line, and the name `rift enable --agent` wants.
    #[must_use]
    pub const fn slug(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude-code",
            Self::Codex => "codex",
            Self::Antigravity => "antigravity",
        }
    }

    /// The executable this client is driven through.
    #[must_use]
    pub const fn program(self) -> &'static str {
        match self {
            Self::ClaudeCode => "claude",
            Self::Codex => "codex",
            Self::Antigravity => "agy",
        }
    }

    /// Parse the `--client` value: one slug, or `all`.
    ///
    /// # Errors
    /// A value that is neither `all` nor one of the three slugs.
    pub fn parse(value: &str) -> Result<Vec<Self>, String> {
        if value == "all" {
            return Ok(ALL_CLIENTS.to_vec());
        }
        ALL_CLIENTS
            .iter()
            .find(|c| c.slug() == value)
            .map(|c| vec![*c])
            .ok_or_else(|| {
                format!("dogfood: unknown client `{value}` — one of claude-code, codex, antigravity, all")
            })
    }

    /// Whether this client plays the shell arm.
    ///
    /// This is the one place the per-client rule lives:
    ///
    /// * Claude Code has a read tool and a pinned flag that withholds it, so
    ///   its shell arm is the on arm with `Read`, `Glob` and `Grep` withheld.
    /// * Codex has no read tool: it already reads through its shell, so its
    ///   on arm *is* the shell shape, and a third arm would play the same
    ///   sessions twice for the same reading.
    /// * Antigravity has a read tool but no pinned flag that withholds it. A
    ///   guessed flag that the client ignored would leave the tool in place
    ///   and print an on arm under the shell arm's name.
    #[must_use]
    pub const fn shell_arm(self) -> ShellArm {
        match self {
            Self::ClaudeCode => ShellArm::Plays,
            Self::Codex => ShellArm::Skipped(
                "codex plays no shell arm: it has no read tool and already reads through its \
                 shell, so its on arm is the shell shape and a third arm would repeat it",
            ),
            Self::Antigravity => ShellArm::Skipped(
                "antigravity plays no shell arm: no pinned flag withholds its read tool, and a \
                 guessed one could leave the tool in place and measure nothing",
            ),
        }
    }

    /// The argument vector that plays one session of `arm`.
    ///
    /// Every arm but the shell arm plays [`Client::argv`] unchanged — the off
    /// arm differs in its checkout's policy, not in how the client is driven.
    /// The harness asks for the shell arm only of a client whose
    /// [`Client::shell_arm`] plays, so no other client has a shell argv.
    #[must_use]
    pub fn argv_for(self, arm: Option<Arm>, prompt: &str, repo: &Path) -> Vec<String> {
        match (self, arm) {
            (Self::ClaudeCode, Some(Arm::Shell)) => claude_code_shell_argv(prompt),
            _ => self.argv(prompt, repo),
        }
    }

    /// The argument vector that plays one session headlessly.
    ///
    /// The flags are the least-permissive set that still lets the client read,
    /// edit, glob and grep in the scratch checkout, and each one asks for the
    /// client's own structured event stream so the log can be parsed
    /// afterwards. `repo` is the scratch checkout, absolute.
    #[must_use]
    pub fn argv(self, prompt: &str, repo: &Path) -> Vec<String> {
        let repo = repo.display().to_string();
        match self {
            // `--setting-sources project,local` confines settings to the
            // scratch checkout — `rift enable` writes the wiring into
            // `.claude/settings.local.json`, and the developer's own user
            // settings stay out of the run.
            Self::ClaudeCode => vec![
                "-p".to_owned(),
                prompt.to_owned(),
                "--setting-sources".to_owned(),
                "project,local".to_owned(),
                "--allowedTools".to_owned(),
                "Read".to_owned(),
                "Edit".to_owned(),
                "Glob".to_owned(),
                "Grep".to_owned(),
                "--permission-mode".to_owned(),
                "acceptEdits".to_owned(),
                "--output-format".to_owned(),
                "stream-json".to_owned(),
                "--verbose".to_owned(),
            ],
            // `--dangerously-bypass-hook-trust` runs the checkout's hook file
            // for this invocation and persists nothing; the approvals-and-
            // sandbox bypass is deliberately NOT used, because that one writes
            // a trust row into the developer's global client configuration.
            // The bypass is also why no dogfood run ever sees a hook Codex
            // stopped trusting: a handler Rift rewrote runs here and is skipped
            // in a real session. The per-hook trust check at every write and
            // status read (`rift_agent_install::native::codex_trust`) is what
            // covers that case.
            Self::Codex => vec![
                "exec".to_owned(),
                "--skip-git-repo-check".to_owned(),
                "--dangerously-bypass-hook-trust".to_owned(),
                "--sandbox".to_owned(),
                "workspace-write".to_owned(),
                "--json".to_owned(),
                "--color".to_owned(),
                "never".to_owned(),
                prompt.to_owned(),
            ],
            // `--new-project` is required rather than tidy: without it this
            // client continues the previous conversation and the sessions stop
            // being independent.
            Self::Antigravity => vec![
                "-p".to_owned(),
                prompt.to_owned(),
                "--output-format".to_owned(),
                "stream-json".to_owned(),
                "--dangerously-skip-permissions".to_owned(),
                "--add-dir".to_owned(),
                repo,
                "--new-project".to_owned(),
            ],
        }
    }
}

/// Claude Code's shell arm: `Bash` in, the read and search tools out.
///
/// Withholding is explicit (`--disallowedTools`) rather than left to the
/// allow list, so a tool the settings allow elsewhere cannot come back. The
/// settings-source confinement, the permission mode and the stream are the
/// on arm's, so the tools are the one thing that differs.
fn claude_code_shell_argv(prompt: &str) -> Vec<String> {
    [
        "-p",
        prompt,
        "--setting-sources",
        "project,local",
        "--allowedTools",
        "Bash",
        "Edit",
        "--disallowedTools",
        "Read",
        "Glob",
        "Grep",
        "--permission-mode",
        "acceptEdits",
        "--output-format",
        "stream-json",
        "--verbose",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each client is driven headlessly, with the least permission that lets
    /// it read, edit, glob and grep — and Codex is never handed the flag that
    /// writes a trust row into the developer's global configuration.
    #[test]
    fn each_client_is_driven_headlessly() {
        let repo = Path::new("/scratch/repo");
        let claude = Client::ClaudeCode.argv("hello", repo);
        assert!(claude.contains(&"-p".to_owned()));
        assert!(claude.contains(&"hello".to_owned()));
        assert!(claude.contains(&"project,local".to_owned()));
        assert!(claude.contains(&"acceptEdits".to_owned()));
        assert!(!claude.contains(&"--dangerously-skip-permissions".to_owned()));
        let codex = Client::Codex.argv("hello", repo);
        assert_eq!(codex.first().map(String::as_str), Some("exec"));
        assert!(codex.contains(&"--dangerously-bypass-hook-trust".to_owned()));
        assert!(codex.contains(&"--skip-git-repo-check".to_owned()));
        assert!(
            !codex.contains(&"--dangerously-bypass-approvals-and-sandbox".to_owned()),
            "that flag persists a trust row in the developer's own client configuration"
        );
        let agy = Client::Antigravity.argv("hello", repo);
        assert!(
            agy.contains(&"--new-project".to_owned()),
            "a fresh conversation each time"
        );
        assert!(agy.contains(&"/scratch/repo".to_owned()));
    }
    /// The slugs are the ones `rift enable --agent` accepts, and `all` is all
    /// three.
    #[test]
    fn the_client_names_are_the_adapter_names() {
        assert_eq!(Client::parse("all").unwrap().len(), 3);
        assert_eq!(
            Client::parse("claude-code").unwrap(),
            vec![Client::ClaudeCode]
        );
        assert_eq!(
            Client::parse("antigravity").unwrap(),
            vec![Client::Antigravity]
        );
        assert!(
            Client::parse("gemini").is_err(),
            "only three can be enabled"
        );
        assert_eq!(Client::ClaudeCode.program(), "claude");
        assert_eq!(Client::Codex.program(), "codex");
        assert_eq!(Client::Antigravity.program(), "agy");
    }

    /// Claude Code's shell arm has its shell and withholds the read and search
    /// tools, and keeps everything else the on arm is driven with.
    #[test]
    fn the_shell_arm_withholds_the_read_tools_from_claude_code() {
        let repo = Path::new("/scratch/repo");
        let shell = Client::ClaudeCode.argv_for(Some(Arm::Shell), "hello", repo);
        let after = |flag: &str| -> Vec<&str> {
            shell
                .iter()
                .skip_while(|a| *a != flag)
                .skip(1)
                .take_while(|a| !a.starts_with("--"))
                .map(String::as_str)
                .collect()
        };
        assert_eq!(after("--allowedTools"), ["Bash", "Edit"]);
        assert_eq!(after("--disallowedTools"), ["Read", "Glob", "Grep"]);
        assert_eq!(after("--setting-sources"), ["project,local"]);
        assert_eq!(after("--permission-mode"), ["acceptEdits"]);
        assert_eq!(after("--output-format"), ["stream-json"]);
        assert!(shell.contains(&"hello".to_owned()));
        assert!(!shell.contains(&"--dangerously-skip-permissions".to_owned()));
    }

    /// Every other arm is driven exactly as before, whatever the client.
    #[test]
    fn only_the_shell_arm_changes_how_a_client_is_driven() {
        let repo = Path::new("/scratch/repo");
        for client in crate::dogfood::ALL_CLIENTS {
            let plain = client.argv("hello", repo);
            assert_eq!(client.argv_for(None, "hello", repo), plain);
            assert_eq!(client.argv_for(Some(Arm::Off), "hello", repo), plain);
            assert_eq!(client.argv_for(Some(Arm::On), "hello", repo), plain);
        }
        let codex = Client::Codex.argv("hello", repo);
        assert!(
            !codex.contains(&"--dangerously-bypass-approvals-and-sandbox".to_owned()),
            "the trust-writing flag stays out"
        );
    }

    /// Claude Code plays the shell arm; Codex and Antigravity do not, and each
    /// says why in a sentence of its own.
    #[test]
    fn the_shell_arm_is_played_where_a_flag_can_withhold_the_read_tool() {
        assert_eq!(Client::ClaudeCode.shell_arm(), ShellArm::Plays);
        let ShellArm::Skipped(codex) = Client::Codex.shell_arm() else {
            panic!("codex already reads through its shell");
        };
        assert!(
            codex.starts_with("codex ") && codex.contains("no read tool"),
            "{codex}"
        );
        let ShellArm::Skipped(agy) = Client::Antigravity.shell_arm() else {
            panic!("no pinned flag withholds antigravity's read tool");
        };
        assert!(
            agy.starts_with("antigravity ") && agy.contains("no pinned flag"),
            "{agy}"
        );
    }
}
