//! Where an agent keeps its own transcripts, and writing one back there.
//!
//! The convention is stated only where it is known — Claude Code today —
//! because a guessed location is worse than no answer: `attach` would adopt
//! the wrong file and `resume` would write beside the checkout.

use std::path::{Path, PathBuf};

/// The agent's per-repo transcript DIRECTORY (the bulk-import source).
/// Stated only where the convention is known — claude-code today.
pub(super) fn default_transcript_dir(agent: &str, work_tree: &Path) -> Option<PathBuf> {
    let probe = default_transcript_path(agent, work_tree, "probe")?;
    probe.parent().map(Path::to_path_buf)
}

/// Write the stored redacted transcript bytes to the agent's native location.
pub(super) fn write_transcript(path: &Path, bytes: &[u8]) -> bool {
    if let Some(parent) = path.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            return false;
        }
    }
    std::fs::write(path, bytes).is_ok()
}

/// The agent's own default transcript location, for the journal-free path
/// (fresh clone). Stated only where the convention is known; `None` is the
/// honest answer everywhere else — the caller says so instead of guessing.
pub(super) fn default_transcript_path(
    agent: &str,
    work_tree: &Path,
    session_id: &str,
) -> Option<PathBuf> {
    if agent != "claude-code" {
        return None;
    }
    let home = claude_code_home()?;
    // Claude Code keys its per-project transcript dir by the absolute worktree
    // path with every separator folded to `-`: `/` and `.` everywhere, and on
    // Windows `\` and the drive's `:` as well (observed: a working tree
    // `c:\Users\…\rift-cli` keeps its transcripts under
    // `c--Users-…-rift-cli`). The spelling folded is the plain one, never the
    // verbatim `\\?\` form the family plane resolves, so the name stays
    // relative and the join below stays under the projects directory.
    let plain = rift_git::child_process_path(work_tree);
    let munged: String = plain
        .display()
        .to_string()
        .chars()
        .map(|c| {
            if TRANSCRIPT_DIR_FOLDED.contains(&c) {
                '-'
            } else {
                c
            }
        })
        .collect();
    Some(
        home.join(".claude")
            .join("projects")
            .join(munged)
            .join(format!("{session_id}.jsonl")),
    )
}

/// The characters Claude Code folds to `-` in a project directory name.
#[cfg(windows)]
const TRANSCRIPT_DIR_FOLDED: [char; 4] = ['/', '.', '\\', ':'];

#[cfg(not(windows))]
const TRANSCRIPT_DIR_FOLDED: [char; 2] = ['/', '.'];

/// Where Claude Code keeps `.claude`: `HOME` where it is set (every Unix
/// shell, and the redirected home a test hands the binary), otherwise the
/// Windows profile directory, which is what Claude Code itself reads there.
fn claude_code_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("USERPROFILE")
            } else {
                None
            }
        })
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default transcript location is stated only where the convention is
    /// known. `None` everywhere else is the honest answer, not a fallback.
    #[test]
    fn only_the_agent_whose_convention_is_known_gets_a_default_transcript_path() {
        let work_tree = Path::new("/repos/my.project/sub");
        for agent in ["codex", "gemini", "copilot", "opencode", ""] {
            assert!(
                default_transcript_path(agent, work_tree, "sid").is_none(),
                "{agent} has no known transcript convention"
            );
            assert!(
                default_transcript_dir(agent, work_tree).is_none(),
                "{agent}"
            );
        }

        // Claude Code keys its per-project dir by the worktree path with `/`
        // and `.` folded to `-`; the answer exists exactly when HOME does.
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let path = default_transcript_path("claude-code", work_tree, "sid");
        assert_eq!(
            path.is_some(),
            home.is_some(),
            "the path is knowable exactly when HOME is"
        );
        if let (Some(home), Some(path)) = (home, path) {
            assert_eq!(
                path,
                home.join(".claude")
                    .join("projects")
                    .join("-repos-my-project-sub")
                    .join("sid.jsonl")
            );
            assert_eq!(
                default_transcript_dir("claude-code", work_tree),
                Some(
                    home.join(".claude")
                        .join("projects")
                        .join("-repos-my-project-sub")
                ),
                "the dir is the path's parent, from the same convention"
            );
        }
    }

    /// The Windows convention, observed on a machine running Claude Code:
    /// `\` and the drive's `:` fold to `-` like `/` and `.`, and the
    /// verbatim spelling the family plane resolves is folded in its plain
    /// form. Folding only `/` and `.` left an absolute path, and `join`
    /// discarded the projects directory: `session attach` never found a
    /// Windows transcript, and `resume` on a fresh clone would have written
    /// one beside the checkout in `%TEMP%`.
    #[cfg(windows)]
    #[test]
    fn a_windows_work_tree_stays_under_the_projects_directory() {
        let home = claude_code_home().expect("HOME or USERPROFILE is set on Windows");
        let expected = home
            .join(".claude")
            .join("projects")
            .join("C--repos-my-project-sub")
            .join("sid.jsonl");
        for spelling in [r"C:\repos\my.project\sub", r"\\?\C:\repos\my.project\sub"] {
            let path = default_transcript_path("claude-code", Path::new(spelling), "sid")
                .expect("the convention is known");
            assert_eq!(path, expected, "{spelling}");
            assert!(
                path.starts_with(home.join(".claude").join("projects")),
                "the folded name is relative: {}",
                path.display()
            );
        }
    }

    /// A transcript write reports failure instead of pretending: an
    /// unwritable parent is `false`, and a fresh nested path is created.
    #[test]
    fn a_transcript_write_reports_its_own_failure() {
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("t.jsonl");
        assert!(write_transcript(&nested, b"line\n"));
        assert_eq!(std::fs::read(&nested).unwrap(), b"line\n");

        // The parent exists as a FILE, so `create_dir_all` fails and the
        // write is refused rather than clobbering it.
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a dir").unwrap();
        assert!(!write_transcript(&blocker.join("t.jsonl"), b"x"));
        assert_eq!(std::fs::read(&blocker).unwrap(), b"not a dir");
    }
}
