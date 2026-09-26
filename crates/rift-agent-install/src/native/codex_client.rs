//! Which Codex client runs in a checkout, read from the newest Codex
//! session Rift has recorded there.
//!
//! Rift keeps a small cache per session under the git dir, and a Codex
//! session's cache names the session file Codex itself reported on its hook
//! payloads. That file's first line is Codex's own record of the program
//! that started the session and the Codex version inside it, which the Codex
//! adapter parses. Nothing else is scanned: no directory of Codex's is
//! walked, and a checkout with no recorded Codex session names no client.
//!
//! A linked worktree's own sessions are read first, then the family's shared
//! git dir, because the Codex installed on the machine is the same for every
//! member and a fresh member has no session of its own yet.

use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};

use rift_shadow_hooks::{codex_rollout, session_cache};

use crate::ClientFamily;

use super::codex_trust::{CodexClient, CodexSurface};

/// How much of a session file is read to find its first line. Codex's
/// `session_meta` line carries the session's base instructions and runs to
/// tens of kilobytes; a line longer than this is not read at all.
const MAX_FIRST_LINE_BYTES: u64 = 1024 * 1024;

/// The client the checkout's newest recorded Codex session names, or `None`
/// when there is no such session or its record cannot be read.
pub(super) fn observed_codex_client(repo: &Path) -> Option<CodexClient> {
    let git_dir = git_dir_of(repo)?;
    let mut candidates = newest_codex_session(&git_dir)
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(common) = common_dir_of(&git_dir) {
        candidates.extend(newest_codex_session(&common));
    }
    let (_, transcript) = candidates.into_iter().max_by_key(|(updated, _)| *updated)?;
    let line = first_line(&transcript)?;
    let client = codex_rollout::session_client(&line)?;
    let surface = if client.is_vscode() {
        CodexSurface::VsCode
    } else {
        CodexSurface::Terminal
    };
    Some(CodexClient {
        surface,
        version: client.cli_version,
    })
}

/// The newest Codex session cached under `git_dir` that names its session
/// file: `(updated_at_ms, file)`.
fn newest_codex_session(git_dir: &Path) -> Option<(i64, PathBuf)> {
    let entries = std::fs::read_dir(session_cache::sessions_dir(git_dir)).ok()?;
    entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let session_id = name.strip_suffix(".json")?.to_owned();
            session_cache::read_cache(git_dir, &session_id)
                .ok()
                .flatten()
        })
        .filter(|cache| cache.agent == ClientFamily::Codex.token())
        .filter_map(|cache| {
            let transcript = cache.transcript_path?;
            Some((cache.updated_at_ms, PathBuf::from(transcript)))
        })
        .max_by_key(|(updated, _)| *updated)
}

fn first_line(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file.take(MAX_FIRST_LINE_BYTES));
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    if !line.ends_with('\n') {
        return None;
    }
    Some(line)
}

/// The git dir of the checkout at `repo`: `.git` itself, or the directory a
/// linked worktree's `.git` file points at.
fn git_dir_of(repo: &Path) -> Option<PathBuf> {
    let dot_git = repo.join(".git");
    if dot_git.is_dir() {
        return Some(dot_git);
    }
    let text = std::fs::read_to_string(&dot_git).ok()?;
    let target = text.trim().strip_prefix("gitdir:")?.trim();
    Some(repo.join(target))
}

/// The shared git dir a linked worktree's admin dir names, if it names one.
fn common_dir_of(git_dir: &Path) -> Option<PathBuf> {
    let text = std::fs::read_to_string(git_dir.join("commondir")).ok()?;
    Some(git_dir.join(text.trim()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(git_dir: &Path, id: &str, agent: &str, updated: i64, transcript: &Path) {
        let mut cache = session_cache::SessionCache::new(id, agent, "ended", updated);
        cache.transcript_path = Some(transcript.display().to_string());
        session_cache::write_cache(git_dir, &cache).unwrap();
    }

    fn rollout(path: &Path, originator: &str, version: &str) {
        let meta = serde_json::json!({
            "type": "session_meta",
            "payload": {"originator": originator, "source": "vscode", "cli_version": version},
        });
        std::fs::write(path, format!("{meta}\n{{\"type\":\"event_msg\"}}\n")).unwrap();
    }

    /// The newest Codex session wins, a Claude Code session is never read as
    /// Codex's, and a fresh linked worktree borrows the family's record.
    #[test]
    fn the_client_is_read_from_the_newest_recorded_codex_session() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("repo");
        let git_dir = repo.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        assert_eq!(observed_codex_client(&repo), None, "no session, no client");

        let (old, new, other) = (
            tmp.path().join("a"),
            tmp.path().join("b"),
            tmp.path().join("c"),
        );
        rollout(&old, "codex_exec", "0.153.1");
        rollout(&new, "codex_vscode", "0.155.0-alpha.16.3");
        rollout(&other, "codex-tui", "9.9.9");
        session(&git_dir, "s-old", "codex", 10, &old);
        session(&git_dir, "s-new", "codex", 20, &new);
        session(&git_dir, "s-claude", "claude-code", 30, &other);
        let expected = CodexClient {
            surface: CodexSurface::VsCode,
            version: Some("0.155.0-alpha.16.3".to_owned()),
        };
        assert_eq!(observed_codex_client(&repo), Some(expected.clone()));

        let member = tmp.path().join("member");
        let admin = git_dir.join("worktrees/member");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::create_dir_all(&member).unwrap();
        std::fs::write(
            member.join(".git"),
            format!("gitdir: {}\n", admin.display()),
        )
        .unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        assert_eq!(observed_codex_client(&member), Some(expected));
    }

    #[test]
    fn a_session_file_without_a_whole_first_line_names_no_client() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let torn = tmp.path().join("torn.jsonl");
        std::fs::write(
            &torn,
            r#"{"type":"session_meta","payload":{"originator":"codex_vscode""#,
        )
        .unwrap();
        session(&git_dir, "s", "codex", 1, &torn);
        assert_eq!(observed_codex_client(tmp.path()), None);
    }
}
