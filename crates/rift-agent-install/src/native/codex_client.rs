//! Which Codex client runs in a checkout, read from the newest Codex
//! session Rift has recorded there.
//!
//! Rift keeps a small cache per session in the checkout's run state, and a Codex
//! session's cache names the session file Codex itself reported on its hook
//! payloads. That file's first line is Codex's own record of the program
//! that started the session and the Codex version inside it, which the Codex
//! adapter parses. Nothing else is scanned: no directory of Codex's is
//! walked, and a checkout with no recorded Codex session names no client.
//!
//! A linked worktree's own sessions are read first, then the main work
//! tree's, because the Codex installed on the machine is the same for every
//! member and a fresh member has no session of its own yet.

use std::path::{Path, PathBuf};

use rift_shadow_hooks::run_state::{self, RunRoot};
use rift_shadow_hooks::{codex_rollout, session_cache};

use crate::health::surface::{first_in_head, newest_transcript};
use crate::ClientFamily;

use super::codex_trust::{CodexClient, CodexSurface};

/// The client the checkout's newest recorded Codex session names, or `None`
/// when there is no such session or its record cannot be read.
pub(super) fn observed_codex_client(repo: &Path) -> Option<CodexClient> {
    let cache_root = rift_store::resolve_cache_root(|key| std::env::var(key).ok());
    observed_codex_client_among(&cache_root.join("rift"), repo)
}

/// [`observed_codex_client`] over an explicit families directory
/// (`<cache_root>/rift`), the seam its tests inject a root through.
fn observed_codex_client_among(families: &Path, repo: &Path) -> Option<CodexClient> {
    let git_dir = git_dir_of(repo)?;
    let mut candidates = sessions_of(families, repo, &git_dir)
        .and_then(|sessions| newest_codex_session(&sessions))
        .into_iter()
        .collect::<Vec<_>>();
    if let Some(common) = common_dir_of(&git_dir) {
        // Canonical first: the common dir arrives as `…/worktrees/x/../..`,
        // whose lexical parent is not the main work tree.
        let common = std::fs::canonicalize(&common).unwrap_or(common);
        let main = common.parent().unwrap_or(&common).to_path_buf();
        candidates.extend(
            sessions_of(families, &main, &common)
                .and_then(|sessions| newest_codex_session(&sessions)),
        );
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

/// The sessions directory of the checkout at `work_tree`: its member's run
/// state once moved, its git dir's before (the one tree a reader that moves
/// nothing may read). `None` when two families hold the checkout as a member:
/// which one is live is not this reader's to guess, and reading either could
/// name the client of a removed clone, so it reads neither.
fn sessions_of(families: &Path, work_tree: &Path, git_dir: &Path) -> Option<PathBuf> {
    match run_state::find_member(families, work_tree) {
        Ok(Some(member)) => Some(run_state::dir_for_read(RunRoot::Sessions, &member, git_dir)),
        Ok(None) => Some(RunRoot::Sessions.legacy_under(git_dir)),
        // The trust verdict then names no client, which it reports as
        // unverifiable rather than as any one client's.
        Err(_ambiguous) => None,
    }
}

/// The newest Codex session cached in `sessions` that names its session
/// file: `(updated_at_ms, file)`.
fn newest_codex_session(sessions: &Path) -> Option<(i64, PathBuf)> {
    let caches: Vec<_> = run_state::list_entries(sessions)
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            let session_id = name.strip_suffix(".json")?.to_owned();
            session_cache::read_cache(sessions, &session_id)
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

    fn session(sessions: &Path, id: &str, agent: &str, updated: i64, transcript: &Path) {
        let mut cache = session_cache::SessionCache::new(id, agent, "ended", updated);
        cache.transcript_path = Some(transcript.display().to_string());
        session_cache::write_cache(sessions, &cache).unwrap();
    }

    /// The sessions directory of `work_tree`'s member in family `family`,
    /// marked moved.
    fn moved_sessions(families: &Path, family: &str, work_tree: &Path) -> PathBuf {
        let member = run_state::member_dir(&families.join(family), work_tree);
        run_state::create_private_dir(&member).unwrap();
        std::fs::write(member.join(run_state::MOVED_MARKER), b"").unwrap();
        RunRoot::Sessions.under(&member)
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
        let families = tmp.path().join("cache").join("rift");
        let repo = tmp.path().join("repo");
        let git_dir = repo.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let sessions = moved_sessions(&families, "fam", &repo);
        let observed = |checkout: &Path| observed_codex_client_among(&families, checkout);
        assert_eq!(observed(&repo), None, "no session, no client");

        let (old, new, other) = (
            tmp.path().join("a"),
            tmp.path().join("b"),
            tmp.path().join("c"),
        );
        rollout(&old, "codex_exec", "0.153.1");
        rollout(&new, "codex_vscode", "0.155.0-alpha.16.3");
        rollout(&other, "codex-tui", "9.9.9");
        session(&sessions, "s-old", "codex", 10, &old);
        session(&sessions, "s-new", "codex", 20, &new);
        session(&sessions, "s-claude", "claude-code", 30, &other);
        let expected = CodexClient {
            surface: CodexSurface::VsCode,
            version: Some("0.155.0-alpha.16.3".to_owned()),
        };
        assert_eq!(observed(&repo), Some(expected.clone()));

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
        assert_eq!(observed(&member), Some(expected));
    }

    /// A member not moved yet is read in its git dir, the one tree it has.
    #[test]
    fn a_member_not_moved_yet_is_read_in_its_git_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let families = tmp.path().join("cache").join("rift");
        let git_dir = tmp.path().join(".git");
        let rollout_file = tmp.path().join("r.jsonl");
        rollout(&rollout_file, "codex_vscode", "0.1.0");
        session(
            &RunRoot::Sessions.legacy_under(&git_dir),
            "s",
            "codex",
            1,
            &rollout_file,
        );
        assert!(observed_codex_client_among(&families, tmp.path()).is_some());
    }

    /// One checkout held by two families is not guessed between: neither
    /// family's sessions are read.
    #[test]
    fn a_checkout_held_by_two_families_names_no_client() {
        let tmp = tempfile::tempdir().unwrap();
        let families = tmp.path().join("cache").join("rift");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let rollout_file = tmp.path().join("r.jsonl");
        rollout(&rollout_file, "codex_vscode", "0.1.0");
        session(
            &moved_sessions(&families, "fam", &repo),
            "s",
            "codex",
            1,
            &rollout_file,
        );
        assert!(observed_codex_client_among(&families, &repo).is_some());
        session(
            &moved_sessions(&families, "fam2", &repo),
            "t",
            "codex",
            2,
            &rollout_file,
        );
        assert_eq!(observed_codex_client_among(&families, &repo), None);
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
        let families = tmp.path().join("cache").join("rift");
        session(
            &moved_sessions(&families, "fam", tmp.path()),
            "s",
            "codex",
            1,
            &torn,
        );
        assert_eq!(observed_codex_client_among(&families, tmp.path()), None);
    }

    #[test]
    fn an_unmoved_split_registry_selects_the_newest_client_with_new_name_precedence() {
        let tmp = tempfile::tempdir().unwrap();
        let families = tmp.path().join("families");
        let repo = tmp.path().join("repo");
        let git_dir = repo.join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let member = run_state::member_dir(&families.join("family"), &repo);
        run_state::record_git_dir(&member, &git_dir).unwrap();
        run_state::record_work_tree(&member, &repo).unwrap();
        let new = RunRoot::Sessions.under(&member);
        let old = RunRoot::Sessions.legacy_under(&git_dir);
        let terminal = tmp.path().join("terminal.jsonl");
        let editor = tmp.path().join("editor.jsonl");
        rollout(&terminal, "codex_exec", "0.1.0");
        rollout(&editor, "codex_vscode", "0.2.0");
        session(&new, "new", "codex", 10, &editor);
        session(&old, "old", "codex", 20, &terminal);
        assert!(!run_state::is_moved(&member));
        assert_eq!(
            observed_codex_client_among(&families, &repo),
            Some(CodexClient {
                surface: CodexSurface::Terminal,
                version: Some("0.1.0".into()),
            }),
            "newest distinct old-tree session remains visible"
        );
        session(&old, "same", "codex", 100, &terminal);
        session(&new, "same", "codex", 30, &editor);
        assert_eq!(
            observed_codex_client_among(&families, &repo),
            Some(CodexClient {
                surface: CodexSurface::VsCode,
                version: Some("0.2.0".into()),
            }),
            "new copy shadows the same old filename before newest selection"
        );
    }
}
