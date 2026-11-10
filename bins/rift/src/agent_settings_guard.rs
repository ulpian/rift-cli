//! Keep agents' machine-local settings files out of git.
//!
//! When a person answers "Yes, and don't ask again", Claude Code saves the
//! exact command it approved — tokens, passwords and connection strings
//! included — into `.claude/settings.local.json` in the project. Rift writes
//! its own Claude Code hooks into that same file, so on a project where Rift
//! created it, nothing else may have made git ignore it, and the next
//! `git add .` publishes whatever was approved. (Apiiro, November 2026, found
//! thousands of live secrets committed this way.)
//!
//! [`ensure`] makes the file ignored through the repository's
//! `info/exclude`: machine-local, like the file itself, and never an edit to
//! a tracked `.gitignore` the team owns. A file that is already committed
//! cannot be fixed by ignoring it, so that case is reported, never rewritten:
//! untracking it and rotating what it held are the owner's decisions. The
//! shared `.claude/settings.json` is meant to be committed, so it is only
//! scanned: an approved command in it that the redaction engine would mask is
//! reported.

use std::path::Path;

use rift_core::SecretBytes;
use rift_git::RepoPaths;

/// Machine-local agent settings that must never be committed.
const LOCAL_SETTINGS: [&str; 1] = [".claude/settings.local.json"];
/// Shared agent settings whose approved commands are scanned for secrets.
const SHARED_SETTINGS: &str = ".claude/settings.json";
/// What `ensure` appends to `info/exclude`, after a comment saying why.
const EXCLUDE_NOTE: &str =
    "# Added by rift: Claude Code saves approved commands here, secrets included.";

/// The state of one settings file, as [`ensure`] left it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Finding {
    /// Ignored by git already (by any rule).
    Ignored(&'static str),
    /// Was not ignored; now listed in `info/exclude`.
    Excluded(&'static str),
    /// Committed to the repository: ignoring cannot help.
    Tracked(&'static str),
    /// The shared settings file approves commands holding what looks like a
    /// credential (the count, never the text).
    SharedSecrets(u32),
}

impl Finding {
    /// Whether the finding needs the person to act.
    pub(crate) fn needs_action(&self) -> bool {
        matches!(self, Self::Tracked(_) | Self::SharedSecrets(_))
    }

    /// One sentence for a terminal.
    pub(crate) fn sentence(&self) -> String {
        match self {
            Self::Ignored(path) => format!("{path}: ignored by git"),
            Self::Excluded(path) => format!(
                "{path}: was not ignored by git; added to .git/info/exclude so approved \
                 commands saved there are never committed"
            ),
            Self::Tracked(path) => format!(
                "{path} is committed to this repository. It can hold commands you approved, \
                 secrets included: run `git rm --cached {path}`, commit, and rotate any \
                 credential it held (it stays in git history)"
            ),
            Self::SharedSecrets(count) => format!(
                "{SHARED_SETTINGS} approves {count} command(s) holding what looks like a \
                 credential; this file is shared through git. Remove those entries and rotate \
                 the credentials"
            ),
        }
    }
}

/// Make every machine-local settings file in the repository at `start` ignored
/// by git, and report what needs the person's attention. Never fails the
/// caller: a repository git cannot read yields no findings.
pub(crate) fn ensure(start: &Path) -> Vec<Finding> {
    let Ok(repo) = rift_git::discover(start) else {
        return Vec::new();
    };
    let mut findings: Vec<Finding> = LOCAL_SETTINGS
        .iter()
        .filter_map(|path| guard_local(&repo, path))
        .collect();
    let shared = shared_secret_count(&repo.work_tree.join(SHARED_SETTINGS));
    if shared > 0 {
        findings.push(Finding::SharedSecrets(shared));
    }
    findings
}

/// The findings worth printing: everything except "already ignored".
pub(crate) fn notable(findings: &[Finding]) -> Vec<String> {
    findings
        .iter()
        .filter(|finding| !matches!(finding, Finding::Ignored(_)))
        .map(Finding::sentence)
        .collect()
}

fn guard_local(repo: &RepoPaths, path: &'static str) -> Option<Finding> {
    let git = |args: &[&str]| {
        let argv: Vec<String> = args.iter().map(|a| (*a).to_owned()).collect();
        rift_git::run_git_status(repo, &argv, &[], &[]).ok()
    };
    if git(&["ls-files", "--error-unmatch", "--", path])?.0 {
        return Some(Finding::Tracked(path));
    }
    if git(&["check-ignore", "-q", "--no-index", "--", path])?.0 {
        return Some(Finding::Ignored(path));
    }
    let exclude = repo.common_dir.join("info").join("exclude");
    append_exclude(&exclude, path).ok()?;
    Some(Finding::Excluded(path))
}

/// Append `/path` to `info/exclude`, creating it if needed. Appending never
/// rewrites what the person keeps there.
fn append_exclude(exclude: &Path, path: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let existing = std::fs::read_to_string(exclude).unwrap_or_default();
    let line = format!("/{path}");
    if existing.lines().any(|l| l.trim() == line) {
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(exclude)?;
    let separator = if existing.is_empty() || existing.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    write!(file, "{separator}{EXCLUDE_NOTE}\n{line}\n")
}

/// How many of the shared settings' approved commands the redaction engine
/// would mask. Zero when the file is absent or unreadable.
fn shared_secret_count(path: &Path) -> u32 {
    let Ok(text) = std::fs::read_to_string(path) else {
        return 0;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        return 0;
    };
    let Some(allowed) = json
        .get("permissions")
        .and_then(|p| p.get("allow"))
        .and_then(serde_json::Value::as_array)
    else {
        return 0;
    };
    let commands = allowed.iter().filter_map(serde_json::Value::as_str).fold(
        String::new(),
        |mut all, command| {
            all.push_str(command);
            all.push('\n');
            all
        },
    );
    rift_shadow::redact::redact(&SecretBytes::new(commands.into_bytes()))
        .map_or(0, |output| output.report.total_spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        assert!(status.status.success(), "git {args:?}");
    }

    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        git(dir.path(), &["init", "-q"]);
        // The machine's global excludes (Claude Code adds this file to them)
        // must not decide the test: the repository's own setting wins.
        git(dir.path(), &["config", "core.excludesFile", "/dev/null"]);
        std::fs::create_dir_all(dir.path().join(".claude")).unwrap();
        dir
    }

    #[test]
    fn an_unignored_local_settings_file_is_excluded_once_and_then_reads_as_ignored() {
        let dir = repo();
        std::fs::write(dir.path().join(".claude/settings.local.json"), "{}").unwrap();
        assert_eq!(
            ensure(dir.path()),
            vec![Finding::Excluded(".claude/settings.local.json")]
        );
        assert_eq!(
            ensure(dir.path()),
            vec![Finding::Ignored(".claude/settings.local.json")]
        );
        let exclude = std::fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap();
        assert_eq!(exclude.matches("/.claude/settings.local.json").count(), 1);
    }

    #[test]
    fn a_file_the_project_already_ignores_is_left_alone() {
        let dir = repo();
        std::fs::write(dir.path().join(".gitignore"), "settings.local.json\n").unwrap();
        assert_eq!(
            ensure(dir.path()),
            vec![Finding::Ignored(".claude/settings.local.json")]
        );
        let exclude =
            std::fs::read_to_string(dir.path().join(".git/info/exclude")).unwrap_or_default();
        assert!(!exclude.contains("settings.local.json"));
    }

    #[test]
    fn a_committed_local_settings_file_is_reported_not_rewritten() {
        let dir = repo();
        let file = dir.path().join(".claude/settings.local.json");
        std::fs::write(&file, "{}").unwrap();
        git(dir.path(), &["add", "-f", ".claude/settings.local.json"]);
        let findings = ensure(dir.path());
        assert_eq!(
            findings,
            vec![Finding::Tracked(".claude/settings.local.json")]
        );
        assert!(findings[0].needs_action());
        assert!(findings[0].sentence().contains("git rm --cached"));
    }

    #[test]
    fn a_shared_settings_file_approving_a_credential_is_reported_by_count() {
        let dir = repo();
        std::fs::write(
            dir.path().join(".gitignore"),
            ".claude/settings.local.json\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            r#"{"permissions":{"allow":["Bash(cargo test:*)","Bash(curl -H 'Authorization: token ghp_aB3dE5fG7hJ9kL1mN3pQ5rS7tU9vW1xY3zA5' https://api.github.com)"]}}"#,
        )
        .unwrap();
        let findings = ensure(dir.path());
        assert!(
            findings
                .iter()
                .any(|f| matches!(f, Finding::SharedSecrets(n) if *n >= 1)),
            "{findings:?}"
        );
        let text = notable(&findings).join("\n");
        assert!(!text.contains("ghp_"), "a finding never quotes the secret");
    }

    #[test]
    fn outside_a_git_repository_nothing_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        assert!(ensure(dir.path()).is_empty());
    }
}
