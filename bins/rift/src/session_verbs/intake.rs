//! `rift session attach` and `rift import`: the two verbs that take a
//! transcript an agent already wrote and adopt it into the ledger.
//!
//! They share one shape — find the native transcript, archive it through the
//! same machinery a hook archive uses, report what landed — and differ in
//! what the result may be used for: an attached session is claimed by a
//! commit and so resumable, an imported one never is.

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use rift_core::Result;

use super::transcripts::{default_transcript_dir, default_transcript_path};
use super::{exit, read_yes, refuse_in_hooks, safe, short, Outcome};
use crate::provenance::commit_claims;

/// `rift session attach SESSION_ID --agent A [--force]`:
/// link an existing native transcript to the last commit. The transcript is
/// found at the journal-recorded address (or the agent's default location),
/// the session is adopted into the journal as `ended`, the shard is archived
/// through the same machinery as a hook archive (idempotent: unchanged
/// content re-derives the same checkpoint id and the archive no-ops), and —
/// only on an explicit yes — the last commit is amended to carry the
/// trailer. The amend NEVER auto-fires: a TTY asks, a pipe requires
/// `--force`, and without either the trailer line is printed for the
/// operator to add.
///
/// Sequencing note, recorded: the archive anchors on the PRE-amend commit
/// (the amended commit has the identical tree; the checkpoint id derives
/// from capture content, never the anchor oid, so the trailer written by the
/// amend addresses the landed shard exactly). Both oids ride the `--json`
/// document.
pub(crate) fn attach(
    store: &rift_store::SqliteStore,
    rt: &tokio::runtime::Runtime,
    root: &Path,
    session_id: &str,
    agent: &str,
    force: bool,
) -> Result<Outcome> {
    use rift_shadow::hooks::session_cache::read_cache;
    if let Some(refusal) = refuse_unattachable(session_id) {
        return Ok(refusal);
    }
    let repo = rift_git::discover(root)?;
    let Some(head) = rift_git::resolve_commit(&repo, "HEAD")? else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "no commit to attach to (HEAD is unborn)",
            serde_json::json!({ "error": "unborn_head" }),
        ));
    };
    let journal = read_cache(&repo.git_dir, session_id).ok().flatten();
    let candidates = transcript_candidates(journal.as_ref(), agent, &repo.work_tree, session_id);
    let Some(transcript_path) = candidates.iter().find(|p| p.exists()).cloned() else {
        return Ok(refuse_missing_transcript(session_id, &candidates));
    };

    let now = crate::hooks::now_ms();
    adopt_as_ended(&repo, journal, session_id, agent, &transcript_path, now)?;
    forget_compaction_progress(&repo.git_dir, session_id);
    // Archive through the one machinery. Idempotent by construction: the id
    // derives from the capture content, so an unchanged re-attach lands on
    // the same shard and the write is a no-op.
    let checkpoint = crate::hooks::attach_archive(&repo, store, rt, &head, session_id, agent, now)?;
    // The `attach` provenance basis: the session is
    // recorded as observed in the checkout the operator attached it from.
    // AFTER the archive, so the row exists only for a session the projection
    // now has — the observation is provenance about a captured session, not a
    // note about an attempt.
    rt.block_on(crate::provenance::record_worktree_observation(
        store,
        &repo,
        &rift_core::capture_session_row_id(agent, session_id),
        "attach",
        now,
    ));
    let Some(checkpoint_id) = checkpoint else {
        return Ok(Outcome::coded(
            exit::FAILURE,
            "the archive declined to write a checkpoint",
            serde_json::json!({ "error": "archive_declined" }),
        ));
    };

    // The trailer: already claimed, amended on an explicit yes, or printed.
    let claims = commit_claims(&repo, &head)?;
    let already = claims.ids().iter().any(|id| *id == checkpoint_id);
    let trailer = format!("Rift-Checkpoint: {checkpoint_id}");
    let amend_allowed = force || confirm_amend(&checkpoint_id);
    let amended_commit = if already || !amend_allowed {
        None
    } else {
        amend_with_trailer(&repo, &trailer)?
    };
    Ok(attached_outcome(&Attached {
        session_id,
        agent,
        checkpoint_id: &checkpoint_id,
        head: &head,
        transcript_path: &transcript_path,
        trailer: &trailer,
        already,
        amended_commit,
    }))
}

/// The two refusals that precede any work: attach is an interactive verb and
/// a hook must never run it, and a session id that is unsafe as a path
/// component cannot address a transcript.
fn refuse_unattachable(session_id: &str) -> Option<Outcome> {
    use rift_shadow::hooks::session_id::is_safe_session_id;
    if let Some(refusal) = refuse_in_hooks("attach") {
        return Some(refusal);
    }
    if is_safe_session_id(session_id) {
        return None;
    }
    Some(Outcome::coded(
        exit::PRECONDITION,
        "that session id is not usable (unsafe as a path component)",
        serde_json::json!({ "error": "unsafe_session_id" }),
    ))
}

/// The native transcript's possible addresses, in the order attach tries
/// them: the journal-recorded one first, the agent's own default location
/// second. Refused, not guessed, when neither exists.
fn transcript_candidates(
    journal: Option<&rift_shadow::hooks::session_cache::SessionCache>,
    agent: &str,
    work_tree: &Path,
    session_id: &str,
) -> Vec<PathBuf> {
    let recorded = journal.and_then(|c| c.transcript_path.clone());
    recorded
        .iter()
        .map(PathBuf::from)
        .chain(default_transcript_path(agent, work_tree, session_id))
        .collect()
}

/// The refusal when no candidate address holds a file: the count is named so
/// an operator can tell "no journal entry" from "the file is gone".
fn refuse_missing_transcript(session_id: &str, candidates: &[PathBuf]) -> Outcome {
    Outcome::coded(
        exit::PRECONDITION,
        format!(
            "no native transcript found for session {} (looked at {} location(s)); \
             nothing to attach",
            safe(session_id),
            candidates.len()
        ),
        serde_json::json!({
            "error": "transcript_not_found",
            "session_id": session_id,
            "looked_at": candidates.iter().map(|p| p.display().to_string())
                .collect::<Vec<_>>(),
        }),
    )
}

/// Adopt the session into the journal as `ended`.
///
/// The journal entry is what the archive reads the transcript address from,
/// and what makes the session visible to the lifecycle surfaces. `ended` —
/// an attached session is finished work, and the live-session surfaces
/// (trailer injection, sweeps) must not see it.
fn adopt_as_ended(
    repo: &rift_git::RepoPaths,
    journal: Option<rift_shadow::hooks::session_cache::SessionCache>,
    session_id: &str,
    agent: &str,
    transcript_path: &Path,
    now: i64,
) -> Result<()> {
    use rift_shadow::hooks::session_cache::{write_cache, SessionCache};
    let mut cache = journal.unwrap_or_else(|| SessionCache::new(session_id, agent, "ended", now));
    "ended".clone_into(&mut cache.state);
    agent.clone_into(&mut cache.agent);
    cache.updated_at_ms = now;
    cache.transcript_path = Some(transcript_path.display().to_string());
    write_cache(&repo.git_dir, &cache)?;
    Ok(())
}

/// Drop the compaction-progress sidecar, so attach archives the WHOLE
/// transcript every time.
///
/// The sidecar (`<git_dir>/rift-sessions/<id>.transcript`) is
/// worker-transient state for LIVE hook flows, and honoring it here would
/// make a re-attach compact from a shifted boundary — an empty or
/// truncated pair, a different content-derived id — instead of no-ooping
/// on identical content. Removing it is safe (derived, non-canonical) and
/// is what makes attach idempotent BY CONTENT; the e2e pins the ref tip
/// across a re-attach.
fn forget_compaction_progress(git_dir: &Path, session_id: &str) {
    let _ = std::fs::remove_file(
        git_dir
            .join("rift-sessions")
            .join(format!("{session_id}.transcript")),
    );
}

/// Amend the last commit to carry the trailer, and answer with the new head.
fn amend_with_trailer(repo: &rift_git::RepoPaths, trailer: &str) -> Result<Option<String>> {
    let message = String::from_utf8_lossy(&rift_git::run_git_capture(
        repo,
        &[
            "log".to_owned(),
            "-1".to_owned(),
            "--format=%B".to_owned(),
            "HEAD".to_owned(),
        ],
        &[],
    )?)
    .into_owned();
    let msg_file = repo.git_dir.join("RIFT_ATTACH_MSG");
    std::fs::write(&msg_file, format!("{}\n{trailer}\n", message.trim_end()))?;
    rift_git::run_git(
        repo,
        &[
            "commit".to_owned(),
            "--amend".to_owned(),
            "-q".to_owned(),
            "-F".to_owned(),
            msg_file.display().to_string(),
        ],
    )?;
    let _ = std::fs::remove_file(&msg_file);
    rift_git::resolve_commit(repo, "HEAD")
}

/// What an attach landed, as the report reads it.
struct Attached<'a> {
    session_id: &'a str,
    agent: &'a str,
    checkpoint_id: &'a str,
    head: &'a str,
    transcript_path: &'a Path,
    trailer: &'a str,
    already: bool,
    amended_commit: Option<String>,
}

fn attached_outcome(attached: &Attached<'_>) -> Outcome {
    let Attached {
        session_id,
        agent,
        checkpoint_id,
        head,
        transcript_path,
        trailer,
        already,
        amended_commit,
    } = attached;
    let mut human = format!(
        "attached session {} ({}) to checkpoint {}\n  transcript: {}\n",
        safe(session_id),
        safe(agent),
        safe(checkpoint_id),
        crate::provenance::safe_path(&transcript_path.display().to_string()),
    );
    match (already, amended_commit) {
        (true, _) => human.push_str("  the last commit already claims this checkpoint\n"),
        (false, Some(new_head)) => human.push_str(&format!(
            "  amended {} -> {} to add the trailer\n",
            short(head),
            short(new_head)
        )),
        (false, None) => human.push_str(&format!(
            "  trailer NOT added (explicit confirm required; --force in a pipe):\n    {trailer}\n"
        )),
    }
    Outcome::ok(
        human,
        serde_json::json!({
            "session_id": session_id,
            "agent": agent,
            "checkpoint_id": checkpoint_id,
            "anchor_commit": head,
            "already_claimed": already,
            "amended": amended_commit.is_some(),
            "amended_commit": amended_commit,
            "transcript_path": transcript_path.display().to_string(),
            "trailer": trailer,
        }),
    )
}

/// `rift import <agent> [--dry-run --session ID --path DIR]`:
/// bulk-read the agent's local transcript store for this repository
/// into **read-only** checkpoints — searchable and explainable, never
/// resumable (no commit ever claims them, so the branch walk cannot select
/// them and no trailer resolves to them). Idempotent by content: an
/// unchanged re-import re-derives the same checkpoint ids, the shard writes
/// no-op, and the chain answers `AlreadyRecorded` per id. The chain
/// positions appended by one run are its **import epoch**, reported as a
/// position range.
pub(crate) fn import(
    store: &rift_store::SqliteStore,
    rt: &tokio::runtime::Runtime,
    root: &Path,
    agent: &str,
    dry_run: bool,
    only_session: Option<&str>,
    source_dir: Option<&Path>,
) -> Result<Outcome> {
    if let Some(refusal) = refuse_in_hooks("import") {
        return Ok(refusal);
    }
    let repo = rift_git::discover(root)?;
    let Some(head) = rift_git::resolve_commit(&repo, "HEAD")? else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "no commit to anchor imported checkpoints to (HEAD is unborn)",
            serde_json::json!({ "error": "unborn_head" }),
        ));
    };
    let Some(dir) = import_source_dir(agent, &repo.work_tree, source_dir) else {
        return Ok(refuse_no_source(agent));
    };
    let sources = import_sources(&dir, only_session);
    if sources.is_empty() {
        return Ok(nothing_to_import(agent, &dir, dry_run));
    }
    if dry_run {
        return Ok(would_import(agent, &dir, &sources));
    }

    let chain_before = rift_git::chain::read_order(&repo.git_dir)?.len();
    let run = ImportRun {
        store,
        rt,
        repo: &repo,
        head: &head,
        agent,
        now: crate::hooks::now_ms(),
    };
    let tally = archived_transcripts(&run, &sources)?;
    let chain_after = rift_git::chain::read_order(&repo.git_dir)?.len();
    let _ = crate::hooks::level_projection(&repo.work_tree);

    let epoch = (chain_after > chain_before).then(
        || serde_json::json!({ "start_position": chain_before, "end_position": chain_after - 1 }),
    );
    let imported = tally.imported;
    let already = tally.already;
    let human = format!(
        "imported {imported} session(s) from {} ({already} already imported); \
         read-only: searchable and explainable, never resumable",
        crate::provenance::safe_path(&dir.display().to_string()),
    );
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "agent": agent,
            "source": dir.display().to_string(),
            "sessions": tally.rows,
            "imported": imported,
            "already_imported": already,
            "dry_run": false,
            "import_epoch": epoch,
        }),
    ))
}

/// Where a bulk import reads from: an explicit `--path` wins; otherwise the
/// agent's own per-repo transcript directory.
fn import_source_dir(agent: &str, work_tree: &Path, source_dir: Option<&Path>) -> Option<PathBuf> {
    source_dir
        .map(Path::to_path_buf)
        .or_else(|| default_transcript_dir(agent, work_tree))
}

/// The refusal for an agent whose transcript convention is unknown: the
/// source is refused, never guessed, and the flag that supplies it is named.
fn refuse_no_source(agent: &str) -> Outcome {
    Outcome::coded(
        exit::PRECONDITION,
        format!(
            "no known transcript store for agent {} — pass --path DIR",
            safe(agent)
        ),
        serde_json::json!({ "error": "no_source", "agent": agent }),
    )
}

/// Every `<session>.jsonl` under `dir`, name-sorted for a deterministic
/// report and a deterministic chain order.
fn import_sources(dir: &Path, only_session: Option<&str>) -> Vec<(String, PathBuf)> {
    let mut sources: Vec<(String, PathBuf)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let stem = p.file_name()?.to_str()?.strip_suffix(".jsonl")?.to_owned();
            Some((stem, p))
        })
        .filter(|(sid, _)| only_session.is_none_or(|want| want == sid))
        .collect();
    sources.sort();
    sources
}

/// The answer for a source directory with nothing in it: success, not a
/// refusal — an agent this repository was never driven with has no
/// transcripts and that is not an error.
fn nothing_to_import(agent: &str, dir: &Path, dry_run: bool) -> Outcome {
    Outcome::ok(
        format!(
            "nothing to import: no session transcripts under {}",
            crate::provenance::safe_path(&dir.display().to_string())
        ),
        serde_json::json!({
            "agent": agent, "source": dir.display().to_string(),
            "sessions": [], "imported": 0, "dry_run": dry_run,
        }),
    )
}

/// The `--dry-run` report: what a real run would read, and its size.
fn would_import(agent: &str, dir: &Path, sources: &[(String, PathBuf)]) -> Outcome {
    let rows: Vec<serde_json::Value> = sources
        .iter()
        .map(|(sid, p)| {
            let bytes = std::fs::metadata(p).map_or(0, |m| m.len());
            serde_json::json!({ "session_id": sid, "path": p.display().to_string(), "bytes": bytes })
        })
        .collect();
    let human = format!(
        "would import {} session(s) from {} (dry run — nothing written)",
        rows.len(),
        crate::provenance::safe_path(&dir.display().to_string())
    );
    Outcome::ok(
        human,
        serde_json::json!({
            "agent": agent, "source": dir.display().to_string(),
            "sessions": rows, "imported": 0, "dry_run": true,
        }),
    )
}

/// The fixed context one import run archives every transcript against.
struct ImportRun<'a> {
    store: &'a rift_store::SqliteStore,
    rt: &'a tokio::runtime::Runtime,
    repo: &'a rift_git::RepoPaths,
    head: &'a str,
    agent: &'a str,
    now: i64,
}

/// What one import run landed, per session and in total.
struct ImportTally {
    rows: Vec<serde_json::Value>,
    imported: usize,
    already: usize,
}

/// Archive each source in turn, counting what the chain accepted.
fn archived_transcripts(run: &ImportRun<'_>, sources: &[(String, PathBuf)]) -> Result<ImportTally> {
    let mut tally = ImportTally {
        rows: Vec::new(),
        imported: 0,
        already: 0,
    };
    for (sid, path) in sources {
        let Ok(bytes) = std::fs::read(path) else {
            tally
                .rows
                .push(serde_json::json!({ "session_id": sid, "status": "unreadable" }));
            continue;
        };
        let before = rift_git::chain::read_order(&run.repo.git_dir)?.len();
        let checkpoint = crate::hooks::import_archive(
            run.repo,
            run.store,
            run.rt,
            run.head,
            sid,
            rift_core::SecretBytes::new(bytes),
            run.now,
        )?;
        let Some(id) = checkpoint else {
            tally
                .rows
                .push(serde_json::json!({ "session_id": sid, "status": "declined" }));
            continue;
        };
        // The chain dedups by id, so whether it grew is the idempotency
        // verdict for this shard.
        let order = rift_git::chain::read_order(&run.repo.git_dir)?;
        let status = if order.len() > before {
            tally.imported += 1;
            "imported"
        } else {
            tally.already += 1;
            "already-imported"
        };
        // The `import` provenance basis, per imported session. The row's id is
        // content-addressed over the observed state, so re-importing an
        // unchanged transcript store writes no second row — the same
        // idempotence the import itself has.
        run.rt
            .block_on(crate::provenance::record_worktree_observation(
                run.store,
                run.repo,
                &rift_core::capture_session_row_id(run.agent, sid),
                "import",
                run.now,
            ));
        tally.rows.push(serde_json::json!({
            "session_id": sid, "status": status, "checkpoint_id": id,
            "chain_position": order.iter().position(|c| *c == id),
        }));
    }
    Ok(tally)
}

/// Interactive confirm for the amend — the only way it fires without
/// `--force`, and only on a real terminal.
fn confirm_amend(checkpoint_id: &str) -> bool {
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return false;
    }
    println!(
        "Amend the last commit to add 'Rift-Checkpoint: {}'? \
         This rewrites the commit id. [y/N]",
        safe(checkpoint_id)
    );
    read_yes()
}
