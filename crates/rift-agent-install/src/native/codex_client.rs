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

use std::path::{Path, PathBuf};

use rift_shadow_hooks::{codex_rollout, session_cache};

use crate::health::surface::{first_in_head, newest_transcript};
use crate::ClientFamily;

use super::codex_trust::{CodexClient, CodexSurface};

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
    codex_client_at(&transcript)
}

/// The client a Codex session file's first line names, read through the
/// health module's bounded head reader so both surface readers share one
/// bound and one error rule: a first line that is not whole within the bound
/// names no client.
pub(crate) fn codex_client_at(transcript: &Path) -> Option<CodexClient> {
    let client = first_in_head(transcript, 1, codex_rollout::session_client)?;
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
    let caches: Vec<_> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            let session_id = name.strip_suffix(".json")?.to_owned();
            session_cache::read_cache(git_dir, &session_id)
                .ok()
                .flatten()
        })
        .collect();
    newest_transcript(&caches, ClientFamily::Codex.token())
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
