//! `rift session resume`: continue prior agent work with
//! full context, offline-first.
//!
//! Ported behavior (pinned tag, cited as `upstream: resume.go`, `resume_picker.go`,
//! `agent/resume_command.go` — behavior, never code): branch checkout with a
//! dirty-tree refusal, the branch-unique trailer walk capped at the merge-base
//! (merge commits never count as newer work), the metadata fetch ladder, the
//! keep-existing-transcript default with `--force` to overwrite, and the
//! printed native resume command. Rift extras: the restored transcript is the
//! shard's **stored redacted** bytes (never a live re-read), and resume seeds
//! a recall known-state cursor so the first injected manifest is a
//! delta against the resumed context instead of a silent bootstrap.
//!
//! Two deliberate divergences from the retired reference implementation,
//! recorded here:
//! * **No `git reset`, ever.** the legacy backend's help text says resume "will reset your
//!   Git status" past un-checkpointed commits; its tag code does not, and Rift
//!   does not either. The safety rail (PRD item 6) is a warning + confirm
//!   (`--force` in non-interactive runs): newer commits stay exactly where
//!   they are.
//! * **The picker never hangs a pipe.** Without a TTY it prints the resumable
//!   list (the same rows `--json` carries) and the branch-mode teaching line.
//!
//! The surface itself is three steps: the picker, the branch checkout, and
//! the restore of one session's transcript from the shard a commit claims.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use rift_core::Result;
use rift_store::CanonicalStore as _;

use super::journals::family_journal;
use super::shard::{
    anchored_checkpoints, ensure_shard_local, latest_by_created_at, read_shard_transcript,
    shard_read_fact_hashes, walk_branch_checkpoints, FoundCheckpoint,
};
use super::transcripts::{default_transcript_path, write_transcript};
use super::{exit, read_yes, safe, short, Outcome, CHECKPOINTS_REF};

/// One resumable session row: the journal fact plus where it lives.
struct ResumableRow {
    session_id: String,
    agent: String,
    state: String,
    updated_at_ms: i64,
    worktree: PathBuf,
    branch: Option<String>,
}

/// `rift session resume [BRANCH] [--force]`.
pub(crate) async fn resume(
    store: &rift_store::SqliteStore,
    root: &Path,
    branch: Option<&str>,
    force: bool,
) -> Result<Outcome> {
    let repo = rift_git::discover(root)?;
    match branch {
        Some(b) => resume_branch(store, &repo, b, force).await,
        None => Ok(picker(&repo)),
    }
}

/// The no-arg surface: enumerate stopped sessions across worktrees. Without a
/// TTY (CI, pipes, `--json`, agent subprocesses) this **lists** and teaches
/// the branch form instead of hanging on a prompt the caller cannot answer.
fn picker(repo: &rift_git::RepoPaths) -> Outcome {
    let rows = resumable_sessions(repo);
    let mut human = String::from("resumable sessions:\n");
    let mut json_rows = Vec::new();
    for r in &rows {
        human.push_str(&format!(
            "  {} · {} · {} · {}\n",
            r.branch.as_deref().unwrap_or("(no branch)"),
            crate::provenance::Label::of(&r.session_id),
            crate::provenance::Label::of(&r.agent),
            r.state,
        ));
        json_rows.push(serde_json::json!({
            "session_id": r.session_id,
            "agent": r.agent,
            "state": r.state,
            "updated_at_ms": r.updated_at_ms,
            "worktree": r.worktree.display().to_string(),
            "branch": r.branch,
        }));
    }
    if rows.is_empty() {
        human = String::from("no resumable sessions found\n");
    }
    human.push_str("resume one: rift session resume <branch>");
    Outcome::ok(human, serde_json::json!({ "resumable": json_rows }))
}

/// Stopped sessions across **every member of the repository family**, main
/// included and whichever member this runs in, newest first. "Stopped" is
/// anything not visibly mid-turn right now — the walked-away idle session is
/// the common resume case, so only `active` sessions with an open turn are
/// excluded (the pinned picker's PhaseActive rule).
///
/// The first sentence used to say "the main worktree and every linked one"
/// while enumerating `repo.work_tree` + `list_worktrees`, which from a linked
/// member is *this* member and its siblings: main's stopped sessions — the
/// ones a developer most often wants back — were the only ones the picker
/// could not show. [`family_journal`] is the enumeration that makes the
/// sentence true.
fn resumable_sessions(repo: &rift_git::RepoPaths) -> Vec<ResumableRow> {
    let mut rows = Vec::new();
    for member in family_journal(repo) {
        if member.sessions.is_empty() {
            continue; // no row to label: do not spawn a git for its branch
        }
        let branch = member_branch(&member.member);
        for s in member.sessions {
            if s.state == "active" && s.turn_open {
                continue; // mid-turn right now — not stopped
            }
            rows.push(ResumableRow {
                session_id: s.session_id,
                agent: s.agent,
                state: s.state,
                updated_at_ms: s.updated_at_ms,
                worktree: member.member.work_tree.clone(),
                branch: branch.clone(),
            });
        }
    }
    rows.sort_by_key(|r| std::cmp::Reverse(r.updated_at_ms));
    rows
}

/// A member's checked-out branch, or `None` when there is no checkout left to
/// ask: a `prunable` member's sessions are still real history and still worth
/// listing, but the branch its admin entry last pointed at is not a fact this
/// can read back.
fn member_branch(member: &rift_git::FamilyMember) -> Option<String> {
    if member.prunable {
        return None;
    }
    current_branch(&rift_git::discover(&member.work_tree).ok()?)
}

/// The worktree's checked-out branch, `None` when detached/unreadable.
fn current_branch(repo: &rift_git::RepoPaths) -> Option<String> {
    let args = vec![
        "rev-parse".to_owned(),
        "--abbrev-ref".to_owned(),
        "HEAD".to_owned(),
    ];
    let (ok, out) = rift_git::run_git_status(repo, &args, &[], &[]).ok()?;
    if !ok {
        return None;
    }
    let name = String::from_utf8_lossy(&out).trim().to_owned();
    (!name.is_empty() && name != "HEAD").then_some(name)
}

/// The branch form: checkout → trailer walk → fetch ladder → transcript
/// restore → resume command + recall cursor.
async fn resume_branch(
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    branch: &str,
    force: bool,
) -> Result<Outcome> {
    rift_git::validate_refspec(branch)?;
    if let Some(refusal) = switch_to_branch(repo, branch, force)? {
        return Ok(refusal);
    }

    // Latest branch-unique commit carrying a `Rift-Checkpoint` trailer.
    let walk = walk_branch_checkpoints(repo, branch)?;
    let Some(found) = walk.found else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            format!("no Rift checkpoint found on branch '{}'", safe(branch)),
            serde_json::json!({ "error": "no_checkpoint", "branch": branch }),
        ));
    };
    // Safety rail (PRD item 6): un-checkpointed work above the checkpoint is
    // named, and the resume proceeds only on an explicit yes. Nothing is ever
    // reset — the newer commits stay.
    if walk.newer_work > 0 && !force && !confirm_older_checkpoint(walk.newer_work, &found.commit) {
        return Ok(refuse_older_checkpoint(&found, walk.newer_work));
    }
    let checkpoint_id = claimed_checkpoint(store, &found).await;

    // The fetch ladder — only when the shard is not already readable locally
    // (an id that resolves locally IS the shard: ids are content-derived and
    // shards are append-only, so local presence cannot be stale).
    let Some(meta) = ensure_shard_local(repo, &checkpoint_id) else {
        return Ok(refuse_unavailable_shard(&checkpoint_id, &found.commit));
    };
    let shard = shard_contents(repo, &checkpoint_id, &meta);

    let mut human = format!(
        "resuming branch '{}' from checkpoint {} (commit {})\n",
        safe(branch),
        safe(&checkpoint_id),
        short(&found.commit)
    );
    if walk.newer_work > 0 {
        human.push_str(&format!(
            "  note: {} newer commit(s) without checkpoints stay in place (nothing was reset)\n",
            walk.newer_work
        ));
    }
    let (restored, session_rows) = restored_sessions(store, repo, &shard, force).await;
    human.push_str(&restored);
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "branch": branch,
            "checkpoint_id": checkpoint_id,
            "commit": found.commit,
            "newer_commits": walk.newer_work,
            "sessions": session_rows,
            "recall_cursor": { "seeded": true, "hashes": shard.read_hashes.len() },
        }),
    ))
}

/// The refusal when un-checkpointed work sits above the checkpoint and the
/// operator has neither passed `--force` nor said yes: the commits are named,
/// and nothing is reset.
fn refuse_older_checkpoint(found: &FoundCheckpoint, newer_work: usize) -> Outcome {
    Outcome::coded(
        exit::PRECONDITION,
        format!(
            "checkpoint {} is {} commit(s) behind the branch tip; \
             re-run with --force to resume from it (nothing is reset)",
            short(&found.commit),
            newer_work
        ),
        serde_json::json!({
            "error": "newer_commits",
            "commit": found.commit,
            "newer_commits": newer_work,
        }),
    )
}

/// The refusal when every rung of the fetch ladder came back empty: the
/// commit claims a checkpoint whose bytes are not on this machine.
fn refuse_unavailable_shard(checkpoint_id: &str, commit: &str) -> Outcome {
    Outcome::coded(
        exit::PRECONDITION,
        format!(
            "checkpoint {} is claimed by commit {} but its shard is not \
             available locally, and no remote rung could fetch it \
             (tried: checkpoint remote, origin). Fetch {CHECKPOINTS_REF} and retry.",
            safe(checkpoint_id),
            short(commit)
        ),
        serde_json::json!({ "error": "shard_unavailable", "checkpoint_id": checkpoint_id }),
    )
}

/// The one checkpoint this commit resumes from.
///
/// The trailer is a CLAIM, not the landed id (settled 2026-08-29):
/// `prepare-commit-msg` predicts the checkpoint id before the
/// commit exists, and the shard the post-commit archive lands — the one
/// that carries the transcript this verb restores — can and does derive a
/// different id. A trailer-only resume then walks to a shard that never
/// landed and refuses with `shard_unavailable` on exactly the sessions
/// with the richest provenance. So the claims are unioned with the
/// checkpoints the projection ANCHORED to this commit (`checkpoint_commit`,
/// written by the archive that landed), the same union `blame`/`why`
/// resolve through; where trailer and anchor agree it is a no-op.
///
/// A squash claims several checkpoints: the latest by created-at wins (the
/// ranking rule), falling back to claim order for ids the projection has
/// never seen.
async fn claimed_checkpoint(store: &rift_store::SqliteStore, found: &FoundCheckpoint) -> String {
    let mut claims = found.claims.clone();
    for id in anchored_checkpoints(store, &found.commit).await {
        if !claims.contains(&id) {
            claims.push(id);
        }
    }
    latest_by_created_at(store, &claims).await
}

/// What the shard gives a resume: the sessions it records (canon-3
/// single-session shards record exactly one), the stored redacted transcript
/// that lives at the shard root, and the read facts that seed the cursor.
struct ShardContents {
    sessions: Vec<String>,
    transcript: Option<Vec<u8>>,
    read_hashes: Vec<String>,
}

fn shard_contents(
    repo: &rift_git::RepoPaths,
    checkpoint_id: &str,
    meta: &serde_json::Value,
) -> ShardContents {
    let sessions = meta
        .get("sessions")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    ShardContents {
        sessions,
        transcript: read_shard_transcript(repo, checkpoint_id),
        read_hashes: shard_read_fact_hashes(repo, checkpoint_id),
    }
}

/// Restore every session the shard records, and say what happened to each.
async fn restored_sessions(
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    shard: &ShardContents,
    force: bool,
) -> (String, Vec<serde_json::Value>) {
    let mut human = String::new();
    let mut session_rows = Vec::new();
    for session_id in &shard.sessions {
        let row =
            restore_session(store, repo, session_id, shard.transcript.as_deref(), force).await;
        // The recall known-state cursor (Rift extra): the shard's read facts
        // are what this session had read when it was archived — seed the
        // manifest baseline so the next turn injects the delta since then.
        crate::hooks::seed_manifest_baseline(&repo.work_tree, session_id, &shard.read_hashes);
        human.push_str(&row.human);
        session_rows.push(row.json);
    }
    if shard.sessions.is_empty() {
        human.push_str("  shard records no sessions — nothing to restore\n");
    }
    human.push_str(&format!(
        "  recall cursor seeded ({} known read fact(s))",
        shard.read_hashes.len()
    ));
    (human, session_rows)
}

/// One session's restore: journal-recorded address first, adapter default
/// second; an existing local log is kept unless `--force` (the pinned
/// resume.go rule — the on-disk transcript may be the live session).
struct RestoredSession {
    human: String,
    json: serde_json::Value,
}

async fn restore_session(
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    session_id: &str,
    transcript: Option<&[u8]>,
    force: bool,
) -> RestoredSession {
    use rift_shadow::hooks::session_cache::read_cache;
    let journal = read_cache(&repo.git_dir, session_id).ok().flatten();
    let agent = match &journal {
        Some(c) => c.agent.clone(),
        None => projection_session_agent(store, session_id)
            .await
            .unwrap_or_else(|| "unknown".to_owned()),
    };
    let target = journal
        .as_ref()
        .and_then(|c| c.transcript_path.clone())
        .map(PathBuf::from)
        .or_else(|| default_transcript_path(&agent, &repo.work_tree, session_id));

    let (status, path_shown) = match (&target, transcript) {
        (Some(path), Some(bytes)) => {
            if path.exists() && !force {
                ("kept-existing", Some(path.clone()))
            } else if write_transcript(path, bytes) {
                ("restored", Some(path.clone()))
            } else {
                ("write-failed", Some(path.clone()))
            }
        }
        (Some(path), None) => ("no-archived-transcript", Some(path.clone())),
        (None, _) => ("no-native-path", None),
    };
    let command = resume_command(&agent, session_id);
    let mut human = format!(
        "  session {} ({}): {}\n",
        crate::provenance::Label::of(session_id),
        crate::provenance::Label::of(&agent),
        match status {
            "kept-existing" => "existing local transcript kept (use --force to overwrite)",
            "restored" => "transcript restored from the checkpoint shard",
            "write-failed" => "transcript restore FAILED (path not writable)",
            "no-archived-transcript" =>
                "the shard archived no transcript for this session (pre-transcript era)",
            _ =>
                "no native transcript location is known for this agent — \
                  the stored transcript stays in the shard",
        }
    );
    if let (Some(p), true) = (
        &path_shown,
        status == "restored" || status == "kept-existing",
    ) {
        human.push_str(&format!(
            "    at {}\n",
            crate::provenance::safe_path(&p.display().to_string())
        ));
    }
    match &command {
        Some(c) => human.push_str(&format!("    continue with: {c}\n")),
        None => human.push_str("    no native resume command is recorded for this agent\n"),
    }
    RestoredSession {
        human,
        json: serde_json::json!({
            "session_id": session_id,
            "agent": agent,
            "restored": status,
            "transcript_path": path_shown.map(|p| p.display().to_string()),
            "resume_command": command,
        }),
    }
}

/// The projection's recorded agent for a session, when it was ever ingested.
async fn projection_session_agent(
    store: &rift_store::SqliteStore,
    native_session: &str,
) -> Option<String> {
    // Sessions are keyed by the derived row id; the journal-free path cannot
    // know the agent half of that derivation, so ask the identity map that
    // records the (agent, native id) → row binding.
    let rows = store
        .query(
            "SELECT agent FROM capture_session_identity WHERE native_session_id = ? LIMIT 1",
            &[rift_store::Value::text(native_session.to_owned())],
        )
        .await
        .ok()?;
    rows.first().and_then(|r| r.get_text(0).ok())
}

/// The agent's native resume command (print-only — Rift never launches the
/// agent). The table is the pinned tag's `ResumeCommandSpecFor` set; agents
/// absent there answer `None` and the caller says so.
fn resume_command(agent: &str, session_id: &str) -> Option<String> {
    use rift_shadow::hooks::session_id::is_safe_session_id;
    if !is_safe_session_id(session_id) {
        return None;
    }
    let argv = match agent {
        "claude-code" => format!("claude -r {session_id}"),
        "codex" => format!("codex resume {session_id}"),
        "copilot" => format!("copilot --resume {session_id}"),
        "factory-droid" => format!("droid --session-id {session_id}"),
        "gemini" => format!("gemini --resume {session_id}"),
        "opencode" => format!("opencode -s {session_id}"),
        _ => return None,
    };
    Some(argv)
}

/// Ensure the worktree is on `branch`. `Ok(Some(outcome))` is a refusal the
/// caller returns as-is; `Ok(None)` means proceed.
fn switch_to_branch(
    repo: &rift_git::RepoPaths,
    branch: &str,
    force: bool,
) -> Result<Option<Outcome>> {
    if current_branch(repo).as_deref() == Some(branch) {
        return Ok(None);
    }
    // Never checkout over uncommitted work.
    if has_uncommitted_work(repo)? {
        return Ok(Some(Outcome::coded(
            exit::PRECONDITION,
            "you have uncommitted changes — commit or stash them first",
            serde_json::json!({ "error": "dirty_worktree" }),
        )));
    }
    let local = rift_git::resolve_commit(repo, &format!("refs/heads/{branch}"))?;
    if local.is_none() {
        let tracking = rift_git::resolve_commit(repo, &format!("refs/remotes/origin/{branch}"))?;
        if tracking.is_none() {
            return Ok(Some(Outcome::coded(
                exit::PRECONDITION,
                format!(
                    "branch '{}' not found locally or as origin/{0}",
                    safe(branch)
                ),
                serde_json::json!({ "error": "unknown_branch", "branch": branch }),
            )));
        }
        // A remote-only branch is fetched on an explicit yes (--force, or an
        // interactive confirm); a pipe without --force is refused with the
        // exact command instead of a hang.
        if !force && !confirm_fetch_branch(branch) {
            return Ok(Some(Outcome::coded(
                exit::PRECONDITION,
                format!(
                    "branch '{}' exists only as origin/{0}; \
                     re-run with --force to check it out",
                    safe(branch)
                ),
                serde_json::json!({ "error": "remote_only_branch", "branch": branch }),
            )));
        }
        let args = vec![
            "checkout".to_owned(),
            "-b".to_owned(),
            branch.to_owned(),
            format!("origin/{branch}"),
        ];
        rift_git::run_git(repo, &args)?;
        return Ok(None);
    }
    rift_git::git_checkout(repo, branch)?;
    Ok(None)
}

/// Whether the work tree holds changes a checkout could lose. Rift's own
/// state under `.rift/` is not work: the `.rift/.gitignore` `init` writes
/// keeps it out of git's status, but one the repository authored itself may
/// not, and a refusal over Rift's search index would be Rift blocking itself.
fn has_uncommitted_work(repo: &rift_git::RepoPaths) -> Result<bool> {
    Ok(!rift_git::changed_worktree_paths_outside_rift_state(repo)?.is_empty())
}

/// Interactive confirm for resuming from an older checkpoint. Only ever asked
/// on a real terminal; every non-interactive caller takes the `--force` path.
fn confirm_older_checkpoint(newer: usize, commit: &str) -> bool {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return false;
    }
    println!(
        "Found a checkpoint on commit {} with {newer} newer commit(s) above it.\n\
         Resume from this older checkpoint? Nothing will be reset. [y/N]",
        short(commit)
    );
    read_yes()
}

/// Interactive confirm for fetching a remote-only branch.
fn confirm_fetch_branch(branch: &str) -> bool {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return false;
    }
    println!(
        "Branch '{}' exists only on origin. Fetch and check it out? [y/N]",
        safe(branch)
    );
    read_yes()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The resume table is the pinned tag's set and nothing more: an agent
    /// absent from it answers `None` rather than a guessed argv.
    #[test]
    fn the_resume_table_covers_exactly_the_agents_it_names() {
        let id = "01JQ8ZK6M3S9V0N7R2T4W5X6Y7";
        for (agent, expected) in [
            ("claude-code", "claude -r"),
            ("codex", "codex resume"),
            ("copilot", "copilot --resume"),
            ("factory-droid", "droid --session-id"),
            ("gemini", "gemini --resume"),
            ("opencode", "opencode -s"),
        ] {
            let argv = resume_command(agent, id).expect("agent is in the table");
            assert_eq!(argv, format!("{expected} {id}"), "{agent}");
        }
        for absent in ["cursor", "antigravity", "ollama", ""] {
            assert!(
                resume_command(absent, id).is_none(),
                "{absent} is not in the pinned resume table"
            );
        }
    }

    /// The session id reaches a printed command line, so an id the hook layer
    /// would refuse never gets there — the refusal is checked before the
    /// agent match, so it holds for every agent in the table.
    #[test]
    fn an_unsafe_session_id_never_reaches_a_resume_command() {
        for unsafe_id in [
            "",
            ".hidden",
            "a b",
            "a;rm -rf /",
            "$(id)",
            "../escape",
            "id\nsecond",
        ] {
            for agent in ["claude-code", "codex", "gemini"] {
                assert!(
                    resume_command(agent, unsafe_id).is_none(),
                    "{agent} accepted an unsafe id: {unsafe_id:?}"
                );
            }
        }
    }

    /// `git` in `dir` with a fixed identity and no hooks.
    fn git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(["-c", "user.name=Fixture", "-c", "user.email=fixture-id"])
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(dir)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }

    /// A one-commit repository with a file under `.rift/state/` that nothing
    /// tells git to ignore.
    fn repo_with_rift_state() -> (tempfile::TempDir, rift_git::RepoPaths) {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(root, &["init", "-q", "-b", "main"]);
        std::fs::write(root.join("a.txt"), "a\n").unwrap();
        git(root, &["add", "a.txt"]);
        git(root, &["commit", "-qm", "seed"]);
        std::fs::create_dir_all(root.join(".rift/state/injection")).unwrap();
        std::fs::write(root.join(".rift/state/injection/x.json"), "{}").unwrap();
        let paths = rift_git::discover(root).unwrap();
        (tmp, paths)
    }

    /// Rift's own state is not uncommitted work, even where no ignore rule
    /// names it — a repository whose `.rift/.gitignore` is somebody else's,
    /// say — while a real edit beside it still is.
    #[test]
    fn rift_state_alone_never_reads_as_a_dirty_worktree() {
        let (tmp, repo) = repo_with_rift_state();
        assert!(!has_uncommitted_work(&repo).unwrap());
        std::fs::write(tmp.path().join("a.txt"), "edited\n").unwrap();
        assert!(has_uncommitted_work(&repo).unwrap());
    }

    /// The `.rift/.gitignore` `init` writes is enough on its own: git's
    /// status no longer lists the state at all.
    #[test]
    fn the_gitignore_init_writes_hides_rift_state_from_git_status() {
        let (tmp, repo) = repo_with_rift_state();
        assert!(!rift_git::changed_worktree_paths(&repo).unwrap().is_empty());
        crate::init::ignore_rift_state(tmp.path()).unwrap();
        git(tmp.path(), &["add", ".rift/.gitignore"]);
        git(tmp.path(), &["commit", "-qm", "ignore rift state"]);
        assert!(
            rift_git::changed_worktree_paths(&repo).unwrap().is_empty(),
            "the committed ignore file leaves nothing for the status to list"
        );
    }
}
