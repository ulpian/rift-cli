//! The three clients a dogfood run can drive, and how each is driven.
//!
//! The argument vectors are the load-bearing part: they are the least
//! permission that still lets a client read, edit, glob and grep in a scratch
//! checkout, and each one asks for that client's own structured event stream.

use std::path::Path;

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
}
