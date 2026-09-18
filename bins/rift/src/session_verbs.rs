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

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use rift_core::Result;
use rift_store::CanonicalStore as _;

use crate::cli::exit;
use crate::cli::Outcome;
use crate::provenance::{commit_claims, Label};

/// The checkpoint branch every rung of the fetch ladder targets — the same
/// ref the walker reads and the backend writes (append-only; fetches are
/// non-forced, so a rewritten remote history is refused rather than
/// absorbed).
const CHECKPOINTS_REF: &str = rift_shadow::backend::NATIVE_CHECKPOINT_REF;

/// How deep the branch-unique trailer walk looks (the pinned tag's cap).
const MAX_WALK: usize = 100;

/// One resumable session row: the journal fact plus where it lives.
struct ResumableRow {
    session_id: String,
    agent: String,
    state: String,
    updated_at_ms: i64,
    worktree: PathBuf,
    branch: Option<String>,
}

/// The hook-refusal rail: attach and import must never run
/// from inside a git hook or a rift hook child. The live coverage is the
/// variables git exports to every hook it runs (`GIT_DIR` /
/// `GIT_INDEX_FILE`); `RIFT_HOOK` is a **reserved** marker — tested here,
/// honored if any rift-spawned context ever exports it, but nothing in
/// production sets it today (P84Bv wording note). The refusal is typed and
/// quiet — never a panic, never a hang.
pub(crate) fn refuse_in_hooks(verb: &str) -> Option<Outcome> {
    let hook_env = ["GIT_DIR", "GIT_INDEX_FILE", "RIFT_HOOK"]
        .iter()
        .find(|k| std::env::var_os(k).is_some());
    hook_env.map(|k| {
        Outcome::coded(
            exit::PRECONDITION,
            format!(
                "rift session {verb} must not run from a hook ({k} is set); \
                 run it from your own terminal"
            ),
            serde_json::json!({ "error": "refused_in_hook", "verb": verb, "env": k }),
        )
    })
}

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
#[allow(clippy::too_many_lines)] // one linear adopt -> archive -> trailer flow
pub(crate) fn attach(
    store: &rift_store::SqliteStore,
    rt: &tokio::runtime::Runtime,
    root: &Path,
    session_id: &str,
    agent: &str,
    force: bool,
) -> Result<Outcome> {
    use rift_shadow::hooks::session_cache::{read_cache, write_cache, SessionCache};
    use rift_shadow::hooks::session_id::is_safe_session_id;

    if let Some(refusal) = refuse_in_hooks("attach") {
        return Ok(refusal);
    }
    if !is_safe_session_id(session_id) {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "that session id is not usable (unsafe as a path component)",
            serde_json::json!({ "error": "unsafe_session_id" }),
        ));
    }
    let repo = rift_git::discover(root)?;
    let Some(head) = rift_git::resolve_commit(&repo, "HEAD")? else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            "no commit to attach to (HEAD is unborn)",
            serde_json::json!({ "error": "unborn_head" }),
        ));
    };

    // The native transcript: journal-recorded address first, the agent's own
    // default location second. Refused, not guessed, when neither exists.
    let journal = read_cache(&repo.git_dir, session_id).ok().flatten();
    let recorded = journal.as_ref().and_then(|c| c.transcript_path.clone());
    let candidates: Vec<PathBuf> = recorded
        .iter()
        .map(PathBuf::from)
        .chain(default_transcript_path(agent, &repo.work_tree, session_id))
        .collect();
    let Some(transcript_path) = candidates.iter().find(|p| p.exists()).cloned() else {
        return Ok(Outcome::coded(
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
        ));
    };

    // Adopt: the journal entry is what the archive reads the transcript
    // address from, and what makes the session visible to the lifecycle
    // surfaces. `ended` — an attached session is finished work, and the
    // live-session surfaces (trailer injection, sweeps) must not see it.
    let now = crate::hooks::now_ms();
    let mut cache = journal.unwrap_or_else(|| SessionCache::new(session_id, agent, "ended", now));
    "ended".clone_into(&mut cache.state);
    agent.clone_into(&mut cache.agent);
    cache.updated_at_ms = now;
    cache.transcript_path = Some(transcript_path.display().to_string());
    write_cache(&repo.git_dir, &cache)?;

    // Attach archives the WHOLE transcript every time: the compaction-
    // progress sidecar (`<git_dir>/rift-sessions/<id>.transcript`) is
    // worker-transient state for LIVE hook flows, and honoring it here would
    // make a re-attach compact from a shifted boundary — an empty or
    // truncated pair, a different content-derived id — instead of no-oping
    // on identical content. Removing it is safe (derived, non-canonical) and
    // is what makes attach idempotent BY CONTENT; the e2e pins the ref tip
    // across a re-attach.
    let _ = std::fs::remove_file(
        repo.git_dir
            .join("rift-sessions")
            .join(format!("{session_id}.transcript")),
    );
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
    let mut amended_commit = None;
    let amend_allowed = force || confirm_amend(&checkpoint_id);
    if !already && amend_allowed {
        let message = String::from_utf8_lossy(&rift_git::run_git_capture(
            &repo,
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
            &repo,
            &[
                "commit".to_owned(),
                "--amend".to_owned(),
                "-q".to_owned(),
                "-F".to_owned(),
                msg_file.display().to_string(),
            ],
        )?;
        let _ = std::fs::remove_file(&msg_file);
        amended_commit = rift_git::resolve_commit(&repo, "HEAD")?;
    }

    let mut human = format!(
        "attached session {} ({}) to checkpoint {}\n  transcript: {}\n",
        safe(session_id),
        safe(agent),
        safe(&checkpoint_id),
        crate::provenance::safe_path(&transcript_path.display().to_string()),
    );
    if already {
        human.push_str("  the last commit already claims this checkpoint\n");
    } else if let Some(new_head) = &amended_commit {
        human.push_str(&format!(
            "  amended {} -> {} to add the trailer\n",
            short(&head),
            short(new_head)
        ));
    } else {
        human.push_str(&format!(
            "  trailer NOT added (explicit confirm required; --force in a pipe):\n    {trailer}\n"
        ));
    }
    Ok(Outcome::ok(
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
    ))
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
#[allow(clippy::too_many_lines)] // one linear enumerate -> archive -> report flow
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
    // Source: an explicit --path wins; otherwise the agent's own per-repo
    // transcript directory. Refused, not guessed, when neither resolves.
    let dir = match source_dir {
        Some(d) => d.to_path_buf(),
        None => match default_transcript_dir(agent, &repo.work_tree) {
            Some(d) => d,
            None => {
                return Ok(Outcome::coded(
                    exit::PRECONDITION,
                    format!(
                        "no known transcript store for agent {} — pass --path DIR",
                        safe(agent)
                    ),
                    serde_json::json!({ "error": "no_source", "agent": agent }),
                ))
            }
        },
    };
    // Enumerate `<session>.jsonl` files, name-sorted for a deterministic
    // report and a deterministic chain order.
    let mut sources: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
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
    if sources.is_empty() {
        return Ok(Outcome::ok(
            format!(
                "nothing to import: no session transcripts under {}",
                crate::provenance::safe_path(&dir.display().to_string())
            ),
            serde_json::json!({
                "agent": agent, "source": dir.display().to_string(),
                "sessions": [], "imported": 0, "dry_run": dry_run,
            }),
        ));
    }
    if dry_run {
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
        return Ok(Outcome::ok(
            human,
            serde_json::json!({
                "agent": agent, "source": dir.display().to_string(),
                "sessions": rows, "imported": 0, "dry_run": true,
            }),
        ));
    }

    let chain_before = rift_git::chain::read_order(&repo.git_dir)?.len();
    let now = crate::hooks::now_ms();
    let mut rows = Vec::new();
    let mut imported = 0usize;
    let mut already = 0usize;
    for (sid, path) in &sources {
        let Ok(bytes) = std::fs::read(path) else {
            rows.push(serde_json::json!({ "session_id": sid, "status": "unreadable" }));
            continue;
        };
        let before = rift_git::chain::read_order(&repo.git_dir)?.len();
        let checkpoint = crate::hooks::import_archive(
            &repo,
            store,
            rt,
            &head,
            sid,
            rift_core::SecretBytes::new(bytes),
            now,
        )?;
        let Some(id) = checkpoint else {
            rows.push(serde_json::json!({ "session_id": sid, "status": "declined" }));
            continue;
        };
        // The chain dedups by id, so whether it grew is the idempotency
        // verdict for this shard.
        let order = rift_git::chain::read_order(&repo.git_dir)?;
        let status = if order.len() > before {
            imported += 1;
            "imported"
        } else {
            already += 1;
            "already-imported"
        };
        // The `import` provenance basis, per imported session. The row's id is
        // content-addressed over the observed state, so re-importing an
        // unchanged transcript store writes no second row — the same
        // idempotence the import itself has.
        rt.block_on(crate::provenance::record_worktree_observation(
            store,
            &repo,
            &rift_core::capture_session_row_id(agent, sid),
            "import",
            now,
        ));
        rows.push(serde_json::json!({
            "session_id": sid, "status": status, "checkpoint_id": id,
            "chain_position": order.iter().position(|c| *c == id),
        }));
    }
    let chain_after = rift_git::chain::read_order(&repo.git_dir)?.len();
    let _ = crate::hooks::level_projection(&repo.work_tree);

    let epoch = (chain_after > chain_before).then(
        || serde_json::json!({ "start_position": chain_before, "end_position": chain_after - 1 }),
    );
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
            "sessions": rows,
            "imported": imported,
            "already_imported": already,
            "dry_run": false,
            "import_epoch": epoch,
        }),
    ))
}

/// The agent's per-repo transcript DIRECTORY (the bulk-import source).
/// Stated only where the convention is known — claude-code today.
fn default_transcript_dir(agent: &str, work_tree: &Path) -> Option<PathBuf> {
    let probe = default_transcript_path(agent, work_tree, "probe")?;
    probe.parent().map(Path::to_path_buf)
}

/// `rift session list`: the session-STATE surface. The
/// journal (live and ended sessions across **every member of the repository
/// family**, with ages) is the primary; the projection — the family's one
/// store — adds agents/models/token totals per session and
/// contributes sessions the journal no longer holds (historical, imported).
///
/// The enumeration is [`family_journal`], so this reads the same set from any
/// member: run from a linked worktree it used to list that worktree and its
/// siblings and silently omit the main one, which is where most sessions are
/// where most sessions are.
#[allow(clippy::too_many_lines)] // one linear journal-union-projection render
pub(crate) async fn list(store: &rift_store::SqliteStore, root: &Path) -> Result<Outcome> {
    let repo = rift_git::discover(root)?;
    let now = crate::hooks::now_ms();
    let mut rows = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for member in family_journal(&repo) {
        for s in member.sessions {
            seen.insert(s.session_id.clone());
            let row_id = rift_core::capture_session_row_id(&s.agent, &s.session_id);
            rows.push(
                session_row_json(
                    store,
                    &s.session_id,
                    &row_id,
                    Some(&s.agent),
                    &s.state,
                    Some(now.saturating_sub(s.updated_at_ms)),
                    s.updated_at_ms,
                )
                .await,
            );
        }
    }
    // Projection-only sessions (their journal entry is gone, or they were
    // imported): the identity map recovers the native id the operator knows
    // them by; a session it never bound falls back to its row id.
    let projected = store
        .query(
            "SELECT s.session_id, s.agent, COALESCE(ci.native_session_id, s.session_id), \
             s.started_at \
             FROM session s \
             LEFT JOIN capture_session_identity ci ON ci.session_id = s.session_id \
             ORDER BY s.started_at DESC LIMIT 50",
            &[],
        )
        .await?;
    for r in &projected {
        let row_id = r.get_text(0)?;
        let agent = r.get_text(1)?;
        let native = r.get_text(2).unwrap_or_else(|_| row_id.clone());
        if seen.contains(&native) {
            continue;
        }
        let started = r.get_i64(3).unwrap_or(0);
        rows.push(
            session_row_json(
                store,
                &native,
                &row_id,
                Some(&agent),
                "archived",
                None,
                started,
            )
            .await,
        );
    }
    rows.sort_by_key(|r| {
        std::cmp::Reverse(
            r.get("last_at_ms")
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0),
        )
    });
    let mut human = String::from("sessions:\n");
    if rows.is_empty() {
        human.push_str("  (none)");
    }
    for r in &rows {
        let text = |k: &str| {
            r.get(k)
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown")
                .to_owned()
        };
        let age = r
            .get("age_ms")
            .and_then(serde_json::Value::as_i64)
            .map_or(String::new(), |ms| format!("  age={}", human_age(ms)));
        let tokens = r
            .pointer("/tokens/output")
            .and_then(serde_json::Value::as_i64)
            .map_or(String::new(), |out| format!("  out-tokens={out}"));
        human.push_str(&format!(
            "  {}  {}  {}  {}  worktree={}{age}{tokens}\n",
            Label::of(&text("session_id")),
            Label::of(&text("agent")),
            Label::of(&text("model")),
            text("state"),
            // Absence renders through the shared constant, so this column and
            // the dashboard's badge say `unknown` with the same word (D6).
            r.get("worktree")
                .and_then(serde_json::Value::as_str)
                .map_or_else(
                    || crate::provenance::UNKNOWN_WORKTREE.to_owned(),
                    str::to_owned
                ),
        ));
    }
    Ok(Outcome::ok(
        human.trim_end().to_owned(),
        serde_json::json!({ "sessions": rows }),
    ))
}

/// One list/info row: journal facts + the projection's agent/model/token
/// totals, resolved through the derived session row id.
async fn session_row_json(
    store: &rift_store::SqliteStore,
    native_id: &str,
    row_id: &str,
    agent: Option<&str>,
    state: &str,
    age_ms: Option<i64>,
    last_at_ms: i64,
) -> serde_json::Value {
    let meta = store
        .query(
            "SELECT agent, COALESCE(model_primary, 'unknown') FROM session WHERE session_id = ?",
            &[rift_store::Value::text(row_id.to_owned())],
        )
        .await
        .ok();
    let (proj_agent, model) = meta
        .as_ref()
        .and_then(|rows| rows.first())
        .map_or((None, None), |r| (r.get_text(0).ok(), r.get_text(1).ok()));
    let tokens = crate::provenance::session_cost(store, row_id)
        .await
        .ok()
        .flatten();
    // The session's CURRENT worktree — its latest observation.
    // `null` is UNKNOWN, which is every session captured before worktree
    // provenance existed, and the human column below spells it rather than
    // defaulting to `main`.
    let worktree = crate::provenance::session_worktree(store, row_id).await;
    serde_json::json!({
        "session_id": native_id,
        "row_id": row_id,
        "worktree": worktree,
        "agent": agent.map(str::to_owned).or(proj_agent),
        "model": model,
        "state": state,
        "age_ms": age_ms,
        "last_at_ms": last_at_ms,
        "tokens": tokens.as_ref().map(|c| serde_json::json!({
            "uncached_input": c.uncached_input,
            "cached_input": c.cached_input,
            "cache_creation": c.cache_creation,
            "output": c.output,
        })),
        "amount_micros": tokens.as_ref().and_then(|c| c.amount_micros),
    })
}

/// A coarse human age: seconds under a minute, then minutes/hours/days.
fn human_age(ms: i64) -> String {
    let s = ms / 1000;
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else if s < 86_400 {
        format!("{}h", s / 3600)
    } else {
        format!("{}d", s / 86_400)
    }
}

/// `rift session info ID` — one session in full: journal state + age,
/// projection identity, four-class token totals, and its checkpoints.
pub(crate) async fn info(
    store: &rift_store::SqliteStore,
    root: &Path,
    reference: &str,
) -> Result<Outcome> {
    use rift_shadow::hooks::session_cache::read_cache;
    let repo = rift_git::discover(root)?;
    let now = crate::hooks::now_ms();
    let journal = read_cache(&repo.git_dir, reference).ok().flatten();
    // Resolve the projection row: through the journal's agent when present,
    // else by the recorded native id.
    let row_id = match &journal {
        Some(c) => Some(rift_core::capture_session_row_id(&c.agent, reference)),
        None => store
            .query(
                "SELECT session_id FROM capture_session_identity \
                 WHERE native_session_id = ? LIMIT 1",
                &[rift_store::Value::text(reference.to_owned())],
            )
            .await?
            .first()
            .and_then(|r| r.get_text(0).ok()),
    };
    let Some(row_id) = row_id else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            format!(
                "no session {} in the journal or the projection",
                safe(reference)
            ),
            serde_json::json!({ "error": "unknown_session", "session_id": reference }),
        ));
    };
    let state = journal.as_ref().map_or("archived", |c| c.state.as_str());
    let age_ms = journal
        .as_ref()
        .map(|c| now.saturating_sub(c.updated_at_ms));
    let last = journal.as_ref().map_or(0, |c| c.updated_at_ms);
    let mut row = session_row_json(store, reference, &row_id, None, state, age_ms, last).await;
    // The session's checkpoints, newest first.
    let checkpoints = store
        .query(
            "SELECT sic.checkpoint_id, COALESCE(c.created_at, 0) \
             FROM session_in_checkpoint sic \
             LEFT JOIN checkpoint c ON c.checkpoint_id = sic.checkpoint_id \
             WHERE sic.session_id = ? ORDER BY COALESCE(c.created_at, 0) DESC",
            &[rift_store::Value::text(row_id.clone())],
        )
        .await?;
    let checkpoint_rows: Vec<serde_json::Value> = checkpoints
        .iter()
        .filter_map(|r| {
            let id = r.get_text(0).ok()?;
            let at = r.get_i64(1).unwrap_or(0);
            Some(serde_json::json!({ "checkpoint_id": id, "created_at_ms": at }))
        })
        .collect();
    // `info` is the surface that carries the HISTORY — the ordered
    // set of every worktree the session was observed in. One session id can be
    // observed in several checkouts over its life (a worktree-isolated subagent
    // runs under its parent's id), and the ordered set is what an auditor
    // reads; the scalar `worktree` above is its last element.
    let history = crate::provenance::session_worktree_history(store, &row_id).await;
    if let Some(obj) = row.as_object_mut() {
        obj.insert("worktree_history".into(), serde_json::json!(history));
        obj.insert("checkpoints".into(), serde_json::json!(checkpoint_rows));
        obj.insert(
            "transcript_path".into(),
            journal
                .as_ref()
                .and_then(|c| c.transcript_path.clone())
                .map_or(serde_json::Value::Null, serde_json::Value::from),
        );
    }
    let mut human = format!(
        "session {}\n  state: {}{}\n  worktree: {}\n  checkpoints: {}",
        safe(reference),
        state,
        age_ms.map_or(String::new(), |a| format!("  age: {}", human_age(a))),
        crate::provenance::worktree_label(history.last().map(|o| &o.name)),
        checkpoint_rows.len(),
    );
    for o in &history {
        human.push_str(&format!(
            "\n  observed in {} ({}) on {} — basis {} @{}",
            o.name,
            if o.linked { "linked" } else { "main" },
            o.branch
                .as_ref()
                .map_or_else(|| "(detached)".to_owned(), ToString::to_string),
            o.basis,
            o.first_observed_at,
        ));
    }
    human.push_str("\n  (full detail in --json)");
    Ok(Outcome::ok(human, row))
}

/// `rift session stop [ID]` — finalize through the state machine's
/// IdleTimeout → condense → CondenseDone path (the crashed-session sweep's
/// exact machinery, timeout zero: "stale" is now). With an ID, only that
/// session is even looked at.
pub(crate) fn stop(root: &Path, target: Option<&str>) -> Result<Outcome> {
    use rift_shadow::hooks::session_cache::read_cache;
    let repo = rift_git::discover(root)?;
    if let Some(id) = target {
        match read_cache(&repo.git_dir, id).ok().flatten() {
            None => {
                return Ok(Outcome::coded(
                    exit::PRECONDITION,
                    format!("no session {} in this worktree's journal", safe(id)),
                    serde_json::json!({ "error": "unknown_session", "session_id": id }),
                ))
            }
            Some(c) if c.state == "ended" => {
                return Ok(Outcome::ok(
                    format!("session {} is already ended", safe(id)),
                    serde_json::json!({ "finalized": [], "deferred": [], "already_ended": [id] }),
                ))
            }
            Some(_) => {}
        }
    }
    let now = crate::hooks::now_ms();
    let sweep = crate::hooks::sweep_stale_sessions(root, now, 0, target)?;
    // The machine's sweep reaches only its live states (active / compacting /
    // ending). A cache parked in `idle` is in the machine's PRE-SESSION state:
    // `(Idle, Start)` is the only edge out and `(Idle, End)` is deliberately
    // illegal — there is no open phase to condense. Five such caches sat on
    // this repository for 3–57 days, counted STUCK by doctor and reachable by
    // nothing. The explicit stop verb closes them at the
    // journal level: state → `ended`, no transition invented, and it says so.
    // Stop-all keeps a one-hour guard so a cache an agent wrote moments before
    // its `Start` is never closed out from under it; a targeted stop is the
    // operator naming the session, so it closes regardless of age.
    let closed_idle = close_idle_caches(root, now, target)?;
    let mut human = String::new();
    for id in &sweep.finalized {
        human.push_str(&format!(
            "finalized {} (condensed to a checkpoint)\n",
            Label::of(id)
        ));
    }
    for id in &sweep.condense_deferred {
        human.push_str(&format!(
            "{}: stop deferred (no commit to anchor to yet — the next sweep retries)\n",
            Label::of(id)
        ));
    }
    for id in &closed_idle {
        human.push_str(&format!(
            "closed {} (idle: the machine's pre-session state — nothing to condense)\n",
            Label::of(id)
        ));
    }
    if sweep.finalized.is_empty() && sweep.condense_deferred.is_empty() && closed_idle.is_empty() {
        human.push_str("nothing to stop");
    }
    Ok(Outcome::ok(
        human.trim_end().to_owned(),
        serde_json::json!({
            "finalized": sweep.finalized,
            "deferred": sweep.condense_deferred,
            "closed_idle": closed_idle,
            "already_ended": [],
        }),
    ))
}

/// How long an `idle` cache must have gone untouched before stop-all closes
/// it. Generous for the one legitimate gap (an agent between writing its
/// state file and its `Start` hook), unreachable for every stuck cache the
/// verb exists to clear — doctor's own STUCK floor is 24 hours.
const IDLE_CLOSE_GUARD_MS: i64 = 3_600_000;

/// Close journal caches parked in the machine's `idle` state (see the comment
/// at the call site). Returns the ids closed, oldest first.
fn close_idle_caches(root: &Path, now: i64, target: Option<&str>) -> Result<Vec<String>> {
    use rift_shadow::hooks::session_cache::{is_idle_past, write_cache};
    let repo = rift_git::discover(root)?;
    let mut caches: Vec<_> = journal_sessions(&repo.git_dir)
        .into_iter()
        .filter(|c| c.state == "idle")
        .filter(|c| match target {
            Some(id) => c.session_id == id,
            None => is_idle_past(c, now, IDLE_CLOSE_GUARD_MS),
        })
        .collect();
    caches.sort_by_key(|c| c.updated_at_ms);
    let mut closed = Vec::new();
    for mut cache in caches {
        "ended".clone_into(&mut cache.state);
        write_cache(&repo.git_dir, &cache)?;
        closed.push(cache.session_id);
    }
    Ok(closed)
}

/// The previous tool's ephemeral per-base-commit checkpoint refs — the
/// "orphaned shadow refs" doctor reports and `clean` prunes. ONLY the
/// ephemeral namespace: the legacy checkpoint metadata (the read-compat
/// import source) and every rift ref are out of scope by construction.
pub(crate) fn shadow_refs(repo: &rift_git::RepoPaths) -> Vec<(String, i64)> {
    let args = vec![
        "for-each-ref".to_owned(),
        "--format=%(refname) %(committerdate:unix)".to_owned(),
        concat!("refs/", "ent", "ire", "/shadow/").to_owned(),
    ];
    let Ok((true, out)) = rift_git::run_git_status(repo, &args, &[], &[]) else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out)
        .lines()
        .filter_map(|l| {
            let (name, date) = l.split_once(' ')?;
            Some((name.to_owned(), date.trim().parse::<i64>().ok()?))
        })
        .collect()
}

/// `rift clean [--dry-run] [--ttl-days N]`: GC the
/// transients and nothing else.
///
/// In scope: orphaned ephemeral shadow refs past TTL
/// (the previous tool's per-base-commit shadow namespace
/// checkpoints, which its own GC pruned the same way), capture-queue entries
/// past TTL (their C-diff facts are already durable; what a dropped entry
/// costs is a checkpoint that was never going to land), stale queue temp
/// files, the `.rift/state/` run caches (manifest baselines + injection
/// markers — losing them costs one bootstrap turn, never correctness), the
/// per-window journals past their own TTL (losing one costs a re-read that
/// would have been declined), and the once-per-eviction recovery markers past
/// the same TTL (losing one costs a recovery map handed over twice).
///
/// NEVER in scope, by construction: the canonical checkpoint branch, the
/// chain ref, the legacy checkpoint metadata refs (read-compat import
/// source), the projection, and the session journal. The e2e pins the
/// canonical refs byte-identical across a clean.
pub(crate) fn clean(root: &Path, dry_run: bool, ttl_days: u32) -> Result<Outcome> {
    let repo = rift_git::discover(root)?;
    let now = crate::hooks::now_ms();
    let ttl_ms = i64::from(ttl_days) * 86_400_000;
    let cutoff_unix = (now - ttl_ms) / 1000;

    // Ephemeral shadow refs past TTL (by their tip's committer time).
    let shadow = shadow_refs(&repo);
    let (prunable, kept): (Vec<_>, Vec<_>) = shadow
        .iter()
        .partition(|(_, tip_unix)| *tip_unix < cutoff_unix);
    let mut pruned_refs = Vec::new();
    for (name, _) in &prunable {
        // Belt and braces: the enumeration is already namespace-scoped, but a
        // deletion never trusts a list it did not build this line.
        if !name.starts_with(concat!("refs/", "ent", "ire", "/shadow/")) {
            continue;
        }
        if !dry_run
            && rift_git::run_git(
                &repo,
                &["update-ref".to_owned(), "-d".to_owned(), name.clone()],
            )
            .is_ok()
        {
            pruned_refs.push(name.clone());
        }
        if dry_run {
            pruned_refs.push(name.clone());
        }
    }

    // Queue entries past TTL, plus the stale temp files every drain sweeps.
    let mut pruned_queue = 0usize;
    for (path, entry) in crate::capture_queue::entries(&repo.git_dir) {
        if now.saturating_sub(entry.queued_at_ms) > ttl_ms {
            if !dry_run {
                crate::capture_queue::remove(&path);
            }
            pruned_queue += 1;
        }
    }
    let swept_tmps = if dry_run {
        0
    } else {
        crate::capture_queue::sweep_stale_tmps(&repo.git_dir, now)
    };

    // The per-window journals, by their own age: a window still being
    // written to keeps its file, so a live conversation is never disarmed
    // mid-sentence, and nothing in the directory that this tool did not write
    // is touched however it got there.
    let expired_window_journals =
        crate::window_journal::sweep_expired(&repo.work_tree, now, ttl_ms, dry_run);

    // Run-state caches: bootstrap-cost transients only.
    let dropped_state = drop_run_state_caches(&repo.work_tree, dry_run);

    // What ended conversations left in this member's run state.
    let (expired_recovery_markers, search_windows, search_answers) =
        sweep_window_state(&repo.work_tree, now, ttl_ms, dry_run);

    // The family's document sidecars, and this member's views of them.
    //
    // The TTL is the more conservative of the two that have a claim: the
    // repository's own `[doc] ttl_days`, which is its statement about how long
    // a converted document is worth keeping, and this command's flag. A
    // sweep may be asked to keep things longer than the policy promised;
    // it is never asked to collect a sidecar the repository still expects to
    // find, which would cost a parse the next time an agent opened the file.
    let documents = swept_documents(&repo, ttl_days, now, dry_run);
    let sidecars = documents.map_or(0, |report| report.expired);
    let orphan_links = documents.map_or(0, |report| report.orphan_links);
    let sidecar_tmps = documents.map_or(0, |report| report.swept_tmps);
    // What one conversation was told about documents, which a conversation
    // that ended a month ago will not be told any more of.
    let doc_windows = documents.map_or(0, |report| report.swept_windows);

    // The family's read-time basis snapshots, under the same
    // more-conservative-of-two rule the sidecars get: the repository's own
    // `[interception] basis_ttl_days` against this command's flag. Losing one
    // costs a delta frame and never an answer — the blob route is untouched
    // and the file is still in the checkout.
    let bases = swept_bases(&repo, ttl_days, now, dry_run).unwrap_or_default();
    let (basis_snapshots, orphan_basis_metas, basis_tmps) =
        (bases.expired, bases.orphan_metas, bases.swept_tmps);

    let mode = if dry_run { "dry-run" } else { "clean" };
    let human = format!(
        "clean ({mode}): {} shadow ref(s) pruned ({} kept inside TTL), \
         {pruned_queue} stale queue entr{} removed, {swept_tmps} temp file(s) swept, \
         {dropped_state} run-state cache dir(s) dropped, \
         {expired_window_journals} expired window-journal file(s) removed, \
         {expired_recovery_markers} expired recovery marker(s) removed, \
         {sidecars} expired document sidecar(s), {orphan_links} orphan link(s) \
         and {doc_windows} ended document conversation(s) removed, \
         {search_windows} ended search conversation(s) and {search_answers} \
         unpriced answer(s) removed, \
         {basis_snapshots} expired basis snapshot(s) and {orphan_basis_metas} \
         orphan snapshot record(s) removed\n  \
         canonical refs and the chain: untouched",
        pruned_refs.len(),
        kept.len(),
        if pruned_queue == 1 { "y" } else { "ies" },
    );
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "mode": mode,
            "pruned_shadow_refs": pruned_refs,
            "kept_shadow_refs": kept.len(),
            "pruned_queue_entries": pruned_queue,
            "swept_tmp_files": swept_tmps,
            "dropped_state_dirs": dropped_state,
            "expired_window_journals": expired_window_journals,
            "expired_recovery_markers": expired_recovery_markers,
            "expired_doc_sidecars": sidecars,
            "orphan_doc_links": orphan_links,
            "swept_doc_tmp_files": sidecar_tmps,
            "swept_doc_windows": doc_windows,
            "swept_search_windows": search_windows,
            "swept_search_answers": search_answers,
            "expired_basis_snapshots": basis_snapshots,
            "orphan_basis_metas": orphan_basis_metas,
            "swept_basis_tmp_files": basis_tmps,
            "ttl_days": ttl_days,
        }),
    ))
}

/// Drop the whole-directory run-state caches, and say how many went.
///
/// The two here are **bootstrap cost and nothing else**: the manifest cache is
/// a stamp that lets a turn answer "nothing to say" without opening the
/// projection, and the injection markers say a pointer has already been sent.
/// Losing either costs one recomputation or one duplicate pointer, which is
/// why they go whole rather than by age — unlike everything else this sweep
/// touches, where dropping a fresh file would disarm a live conversation.
fn drop_run_state_caches(work_tree: &Path, dry_run: bool) -> usize {
    let mut dropped = 0usize;
    for sub in ["manifest", "injection"] {
        let dir = work_tree.join(".rift").join("state").join(sub);
        if dir.is_dir() {
            dropped += 1;
            if !dry_run {
                let _ = std::fs::remove_dir_all(&dir);
            }
        }
    }
    dropped
}

/// The family's read-time basis snapshots, collected under the more
/// conservative of the two TTLs that have a claim.
///
/// Same rule as [`swept_documents`], for the same reason: a sweep may be asked
/// to keep things longer than the policy promised, and is never asked to
/// collect a basis the repository still expects to find.
fn swept_bases(
    repo: &rift_git::RepoPaths,
    ttl_days: u32,
    now_ms: i64,
    dry_run: bool,
) -> Option<rift_recall_serve::basis_store::SweepReport> {
    let family = crate::family::try_at(&repo.work_tree)?;
    let ttl_days = ttl_days.max(
        rift_recall_policy::ServePolicy::load(&repo.work_tree.join(".rift"))
            .interception
            .basis_ttl_days,
    );
    Some(
        rift_recall_serve::basis_store::BasisStore::for_family(&family.common_dir).sweep(
            now_ms,
            i64::from(ttl_days) * 86_400_000,
            dry_run,
        ),
    )
}

/// This member's view of its family's document store, when the family has an
/// identity to look it up by.
///
/// `None` is not a failure: a checkout whose family cannot be resolved from
/// here has no shared store to collect, and nothing about that is this
/// command's to repair.
pub(crate) fn document_store(repo: &rift_git::RepoPaths) -> Option<crate::doc_store::DocStore> {
    let family = crate::family::try_at(&repo.work_tree)?;
    Some(crate::doc_store::DocStore::for_member(
        &family.common_dir,
        &repo.work_tree,
    ))
}

/// Remove the recovery markers older than the TTL, and say how many went.
///
/// A marker is an empty file named for the fingerprint of one recovery map's
/// input, written `create_new` so a retried boundary hook stays silent. It is
/// spent the moment the map it names has been handed over, and after that it
/// is disk — but only after that, which is why age decides rather than the
/// directory as a whole.
///
/// Total and quiet, like every other leg of this sweep: a file whose age
/// cannot be read is kept (removing on an unknown age would disarm a live
/// window on a filesystem that reports no times), a name this tool did not
/// write is left alone, and a failed removal is simply not counted.
fn sweep_recovery_markers(work_tree: &Path, now_ms: i64, ttl_ms: i64, dry_run: bool) -> usize {
    let dir = work_tree.join(".rift").join("state").join("recovery");
    let cutoff = now_ms.saturating_sub(ttl_ms);
    let mut removed = 0usize;
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return 0;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        // The one shape this tool writes there: a 64-hex fingerprint, `.json`.
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        if stem.len() != 64 || !stem.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let path = entry.path();
        let modified_ms = std::fs::metadata(&path)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok());
        let Some(modified_ms) = modified_ms else {
            continue;
        };
        if modified_ms >= cutoff {
            continue;
        }
        if dry_run || std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// The family's sidecars and this member's views of them, collected under the
/// more conservative of the two TTLs that have a claim.
///
/// A sweep may be asked to keep things longer than the policy promised; it is
/// never asked to collect a sidecar the repository still expects to find,
/// which would cost a parse the next time an agent opened the file.
fn swept_documents(
    repo: &rift_git::RepoPaths,
    ttl_days: u32,
    now_ms: i64,
    dry_run: bool,
) -> Option<crate::doc_store::SweepReport> {
    let store = document_store(repo)?;
    let ttl_days = ttl_days.max(
        rift_recall_policy::ServePolicy::load(&repo.work_tree.join(".rift"))
            .doc
            .ttl_days,
    );
    Some(store.sweep(now_ms, i64::from(ttl_days) * 86_400_000, dry_run))
}

/// The per-window marks ended conversations leave behind, collected by their
/// own age.
///
/// Three kinds, one rule: a marker is what stops a live window being handed
/// the same thing twice — a recovery map after a compaction, an answer to a
/// search it already asked — so dropping a fresh one costs a duplicate in a
/// conversation that is still open, which is exactly the spend this program
/// exists to remove. Age, never the whole directory, and nothing that this
/// tool did not write is touched however it got there.
fn sweep_window_state(
    work_tree: &Path,
    now_ms: i64,
    ttl_ms: i64,
    dry_run: bool,
) -> (usize, usize, usize) {
    let recovery = sweep_recovery_markers(work_tree, now_ms, ttl_ms, dry_run);
    let (windows, answers) = crate::search_gate::sweep(work_tree, now_ms, ttl_ms, dry_run);
    (recovery, windows, answers)
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

/// Every session the journal knows in one worktree — including `ended` ones,
/// which `live_sessions` deliberately filters and which are exactly what a
/// picker must show (a stopped session is the thing you resume).
pub(crate) fn journal_sessions(
    git_dir: &Path,
) -> Vec<rift_shadow::hooks::session_cache::SessionCache> {
    use rift_shadow::hooks::session_cache::read_cache;
    let dir = git_dir.join("rift-sessions");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(stem) = name
            .to_string_lossy()
            .strip_suffix(".json")
            .map(str::to_owned)
        else {
            continue;
        };
        if let Ok(Some(cache)) = read_cache(git_dir, &stem) {
            out.push(cache);
        }
    }
    out
}

// --- the family-wide journal reader -----------------------------------------

/// One family member and the sessions its journal is credited with.
pub(crate) struct MemberJournal {
    /// The member: the main work tree first, then each linked worktree by
    /// administrative name.
    pub(crate) member: rift_git::FamilyMember,
    /// The caches credited to this member, deduplicated family-wide.
    pub(crate) sessions: Vec<rift_shadow::hooks::session_cache::SessionCache>,
}

/// Every member of the family `paths` belongs to, with the sessions its
/// journal holds — the one enumeration `session list`, the resume picker and
/// `rift doctor`'s session sweep share.
///
/// **Main is always in it.** Session caches stay per gitdir by design — they
/// are a session's private working state, scoped to the checkout that produced
/// it — so a family-wide question has to be asked of every member's
/// gitdir. The readers here asked `repo.work_tree` + `list_worktrees`, which
/// from a linked member enumerates that member and its siblings and omits the
/// main work tree — the member that holds nearly every session in a real
/// repository. `family_members` proves main rather than assuming it
/// (never the lexical `common_dir.parent()`), so it is in the list
/// from any vantage.
///
/// **A session is credited once, to the member it names.** A cache
/// carries the canonical work tree it was observed in; where it names a member
/// of this family, that member gets it, whatever registry the file happens to
/// sit in — a session driven in worktree A is never credited to worktree B. A
/// cache with no claim (every file written before the claim existed) belongs to the
/// registry it was found in, which is where it has always been usable.
///
/// **Total, and never fewer members than the caller can see.** A family whose
/// members cannot be enumerated at all — an unreadable common dir, an
/// administrative entry with no `gitdir` file — still yields the vantage's own
/// journal rather than an empty list, because "this checkout has no sessions"
/// and "the family could not be read" are different answers and only one of
/// them is true.
pub(crate) fn family_journal(paths: &rift_git::RepoPaths) -> Vec<MemberJournal> {
    let members = with_vantage(paths, rift_git::family_members(paths));
    let mut out: Vec<MemberJournal> = members
        .iter()
        .cloned()
        .map(|member| MemberJournal {
            member,
            sessions: Vec::new(),
        })
        .collect();
    let mut seen = std::collections::BTreeSet::new();
    for (index, m) in members.iter().enumerate() {
        for s in journal_sessions(&m.git_dir) {
            if !seen.insert(s.session_id.clone()) {
                continue; // one credit per session, family-wide
            }
            let owner = claimed_member(&members, s.work_tree.as_deref()).unwrap_or(index);
            if let Some(journal) = out.get_mut(owner) {
                journal.sessions.push(s);
            }
        }
    }
    out
}

/// `members`, with the vantage itself prepended when the enumeration did not
/// find it. Cheap insurance for a family that cannot be read: the reader must
/// never return less than the single-checkout reader it replaced.
fn with_vantage(
    paths: &rift_git::RepoPaths,
    mut members: Vec<rift_git::FamilyMember>,
) -> Vec<rift_git::FamilyMember> {
    let here = canonical_or_lexical(&paths.git_dir);
    if members
        .iter()
        .any(|m| canonical_or_lexical(&m.git_dir) == here)
    {
        return members;
    }
    members.insert(
        0,
        rift_git::FamilyMember {
            name: paths.work_tree.file_name().map_or_else(
                || paths.work_tree.to_string_lossy().into_owned(),
                |n| n.to_string_lossy().into_owned(),
            ),
            work_tree: paths.work_tree.clone(),
            git_dir: paths.git_dir.clone(),
            is_main: paths.git_dir == paths.common_dir,
            prunable: false,
        },
    );
    members
}

/// The index of the member a cache's `work_tree` claims, or `None` when it
/// claims nothing (files written before the claim existed) or names a tree
/// outside this family.
fn claimed_member(members: &[rift_git::FamilyMember], claim: Option<&str>) -> Option<usize> {
    let claimed = canonical_or_lexical(Path::new(claim?));
    members
        .iter()
        .position(|m| canonical_or_lexical(&m.work_tree) == claimed)
}

/// The canonical path where it resolves, the given one where it does not —
/// both sides of every comparison here go through it, because a claim written
/// through a symlinked path still has to match the tree it names, and a gone
/// checkout still has to be nameable.
fn canonical_or_lexical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
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
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            format!(
                "checkpoint {} is {} commit(s) behind the branch tip; \
                 re-run with --force to resume from it (nothing is reset)",
                short(&found.commit),
                walk.newer_work
            ),
            serde_json::json!({
                "error": "newer_commits",
                "commit": found.commit,
                "newer_commits": walk.newer_work,
            }),
        ));
    }

    // The trailer is a CLAIM, not the landed id (settled 2026-08-29):
    // `prepare-commit-msg` predicts the checkpoint id before the
    // commit exists, and the shard the post-commit archive lands — the one
    // that carries the transcript this verb restores — can and does derive a
    // different id. A trailer-only resume then walks to a shard that never
    // landed and refuses with `shard_unavailable` on exactly the sessions
    // with the richest provenance. So the claims are unioned with the
    // checkpoints the projection ANCHORED to this commit (`checkpoint_commit`,
    // written by the archive that landed), the same union `blame`/`why`
    // resolve through; where trailer and anchor agree it is a no-op.
    let mut claims = found.claims.clone();
    for id in anchored_checkpoints(store, &found.commit).await {
        if !claims.contains(&id) {
            claims.push(id);
        }
    }
    // A squash claims several checkpoints: resolve the latest by created-at
    // (the ranking rule), falling back to claim order for ids the
    // projection has never seen.
    let checkpoint_id = latest_by_created_at(store, &claims).await;

    // The fetch ladder — only when the shard is not already readable locally
    // (an id that resolves locally IS the shard: ids are content-derived and
    // shards are append-only, so local presence cannot be stale).
    let Some(meta) = ensure_shard_local(repo, &checkpoint_id) else {
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            format!(
                "checkpoint {} is claimed by commit {} but its shard is not \
                 available locally, and no remote rung could fetch it \
                 (tried: checkpoint remote, origin). Fetch {CHECKPOINTS_REF} and retry.",
                safe(&checkpoint_id),
                short(&found.commit)
            ),
            serde_json::json!({ "error": "shard_unavailable", "checkpoint_id": checkpoint_id }),
        ));
    };

    // Restore each session the shard records (canon-3 single-session shards
    // record exactly one; the transcript pair lives at the shard root).
    let sessions: Vec<String> = meta
        .get("sessions")
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let transcript = read_shard_transcript(repo, &checkpoint_id);
    let read_hashes = shard_read_fact_hashes(repo, &checkpoint_id);

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
    let mut session_rows = Vec::new();
    for session_id in &sessions {
        let row = restore_session(store, repo, session_id, transcript.as_deref(), force).await;
        // The recall known-state cursor (Rift extra): the shard's read facts
        // are what this session had read when it was archived — seed the
        // manifest baseline so the next turn injects the delta since then.
        crate::hooks::seed_manifest_baseline(&repo.work_tree, session_id, &read_hashes);
        human.push_str(&row.human);
        session_rows.push(row.json);
    }
    if sessions.is_empty() {
        human.push_str("  shard records no sessions — nothing to restore\n");
    }
    human.push_str(&format!(
        "  recall cursor seeded ({} known read fact(s))",
        read_hashes.len()
    ));
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "branch": branch,
            "checkpoint_id": checkpoint_id,
            "commit": found.commit,
            "newer_commits": walk.newer_work,
            "sessions": session_rows,
            "recall_cursor": { "seeded": true, "hashes": read_hashes.len() },
        }),
    ))
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

/// Write the stored redacted transcript bytes to the agent's native location.
fn write_transcript(path: &Path, bytes: &[u8]) -> bool {
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
fn default_transcript_path(agent: &str, work_tree: &Path, session_id: &str) -> Option<PathBuf> {
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
    if !rift_git::changed_worktree_paths(repo)?.is_empty() {
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

/// What the branch walk found.
struct WalkResult {
    /// The newest branch-unique commit carrying at least one claim.
    found: Option<FoundCheckpoint>,
    /// Non-merge commits above it that carry no checkpoint (real newer work).
    newer_work: usize,
}

struct FoundCheckpoint {
    commit: String,
    claims: Vec<String>,
}

/// Walk first-parent history from the branch tip, capped at the merge-base
/// with the default branch (so a merged-in main never claims the resume) and
/// at [`MAX_WALK`] commits. Merge commits are skipped silently — they are not
/// "newer work" (the pinned tag's rule).
fn walk_branch_checkpoints(repo: &rift_git::RepoPaths, branch: &str) -> Result<WalkResult> {
    let base = default_branch(repo)
        .filter(|d| d != branch)
        .and_then(|d| merge_base(repo, branch, &d));
    let mut args = vec![
        "rev-list".to_owned(),
        "--first-parent".to_owned(),
        "--parents".to_owned(),
        format!("--max-count={MAX_WALK}"),
        branch.to_owned(),
    ];
    if let Some(b) = &base {
        args.push(format!("^{b}"));
    }
    args.push("--".to_owned());
    let (ok, out) = rift_git::run_git_status(repo, &args, &[], &[])?;
    if !ok {
        return Ok(WalkResult {
            found: None,
            newer_work: 0,
        });
    }
    let mut newer_work = 0usize;
    for line in String::from_utf8_lossy(&out).lines() {
        let mut fields = line.split_whitespace();
        let Some(oid) = fields.next() else { continue };
        let parents = fields.count();
        let claims = commit_claims(repo, oid)?;
        if !claims.claims.is_empty() {
            return Ok(WalkResult {
                found: Some(FoundCheckpoint {
                    commit: oid.to_owned(),
                    claims: claims.ids().into_iter().map(str::to_owned).collect(),
                }),
                newer_work,
            });
        }
        if parents <= 1 {
            newer_work += 1;
        }
    }
    Ok(WalkResult {
        found: None,
        newer_work: 0,
    })
}

/// The repository's default branch: origin's HEAD when recorded, else the
/// first of `main`/`master` that exists locally.
fn default_branch(repo: &rift_git::RepoPaths) -> Option<String> {
    let args = vec![
        "symbolic-ref".to_owned(),
        "--short".to_owned(),
        "refs/remotes/origin/HEAD".to_owned(),
    ];
    if let Ok((true, out)) = rift_git::run_git_status(repo, &args, &[], &[]) {
        let name = String::from_utf8_lossy(&out).trim().to_owned();
        if let Some(short) = name.strip_prefix("origin/") {
            return Some(short.to_owned());
        }
    }
    for name in ["main", "master"] {
        if matches!(
            rift_git::resolve_commit(repo, &format!("refs/heads/{name}")),
            Ok(Some(_))
        ) {
            return Some(name.to_owned());
        }
    }
    None
}

fn merge_base(repo: &rift_git::RepoPaths, a: &str, b: &str) -> Option<String> {
    let args = vec!["merge-base".to_owned(), a.to_owned(), b.to_owned()];
    let (ok, out) = rift_git::run_git_status(repo, &args, &[], &[]).ok()?;
    if !ok {
        return None;
    }
    let oid = String::from_utf8_lossy(&out).trim().to_owned();
    (!oid.is_empty()).then_some(oid)
}

/// The latest claim by projection `created_at` (the ranking rule); claims
/// the projection has never ingested fall back to the LAST trailer in message
/// order (trailers accumulate downward through rewrites).
async fn latest_by_created_at(store: &rift_store::SqliteStore, claims: &[String]) -> String {
    let mut best: Option<(i64, &String)> = None;
    for id in claims {
        let created = store
            .query_opt_i64(
                "SELECT created_at FROM checkpoint WHERE checkpoint_id = ?",
                &[rift_store::Value::text(id.clone())],
            )
            .await
            .ok()
            .flatten();
        if let Some(t) = created {
            if best.is_none_or(|(bt, _)| t >= bt) {
                best = Some((t, id));
            }
        }
    }
    best.map_or_else(
        || claims.last().cloned().unwrap_or_default(),
        |(_, id)| id.clone(),
    )
}

/// The checkpoints the projection recorded as archived AT `commit_oid` — the
/// ids that really landed, as opposed to the ones a trailer predicted.
async fn anchored_checkpoints(store: &rift_store::SqliteStore, commit_oid: &str) -> Vec<String> {
    store
        .query(
            "SELECT checkpoint_id FROM checkpoint_commit WHERE commit_oid = ? ORDER BY checkpoint_id",
            &[rift_store::Value::text(commit_oid.to_owned())],
        )
        .await
        .map(|rows| rows.iter().filter_map(|r| r.get_text(0).ok()).collect())
        .unwrap_or_default()
}

/// The fetch ladder (PRD item 1): local read → checkpoint remote → origin
/// full — re-reading after each rung, stopping at the first that answers.
/// Every remote rung skips gracefully when its remote is not configured, so
/// a local-only repository resolves fully offline.
///
/// **There is deliberately NO treeless rung** (P84Bv blocker 2, reproduced
/// on git 2.50.1). The retired reference implementation's tip-only treeless
/// fetch works because
/// go-git lazy-fetches missing objects on read; gix does not, so a treeless
/// fetch can only ever probe. But `git fetch --filter=…` against a
/// filter-capable server durably writes `remote.<name>.promisor=true` +
/// `remote.<name>.partialclonefilter` into the repo config — after which
/// EVERY later fetch from that remote is silently tree-filtered, including
/// this ladder's own full rung: the checkpoint ref then advances WITHOUT its
/// objects and the next resume dead-ends on a shard whose blobs never
/// arrive (and un-setting the config alone does not recover, because the
/// ref has already advanced). Nothing reads between a probe and the full
/// rung, so the probe defended nothing; it is gone, and the regression e2e
/// (`resume_twice_from_a_filter_capable_remote_stays_whole`) pins a second
/// resume of a new checkpoint against an `uploadpack.allowFilter=true`
/// remote — the exact sequence that died.
fn ensure_shard_local(
    repo: &rift_git::RepoPaths,
    checkpoint_id: &str,
) -> Option<serde_json::Value> {
    if let Some(meta) = read_shard_metadata(repo, checkpoint_id) {
        return Some(meta);
    }
    // Rung: the configured checkpoint remote (`.rift/config.toml`,
    // `[checkpoint] remote = …`) — where checkpoints live when one is set up.
    if let Some(remote) = rift_config::checkpoint_remote(&repo.work_tree)
        .ok()
        .flatten()
    {
        fetch_checkpoints(repo, &remote);
        if let Some(meta) = read_shard_metadata(repo, checkpoint_id) {
            return Some(meta);
        }
    }
    // Rung: full fetch from origin (fast-forward only into the real ref).
    fetch_checkpoints(repo, "origin");
    read_shard_metadata(repo, checkpoint_id)
}

/// One non-forced, **always-full** fetch of the checkpoints branch from
/// `remote` (fast-forward only — a rewritten remote history is refused,
/// matching append-only shards). No filter flag can reach this argv: a
/// filtered fetch poisons the remote's config durably (see
/// [`ensure_shard_local`]). Best-effort: an absent remote, an offline
/// machine, or a refused update is a skipped rung, never an error.
fn fetch_checkpoints(repo: &rift_git::RepoPaths, remote: &str) {
    let args = vec![
        "fetch".to_owned(),
        "--no-tags".to_owned(),
        remote.to_owned(),
        format!("{CHECKPOINTS_REF}:{CHECKPOINTS_REF}"),
    ];
    let _ = rift_git::run_git_status(repo, &args, &[], &[]);
}

/// The shard's `metadata.json`, parsed — the "is it local yet" probe every
/// ladder rung re-runs.
fn read_shard_metadata(
    repo: &rift_git::RepoPaths,
    checkpoint_id: &str,
) -> Option<serde_json::Value> {
    let walker = rift_recall_substrate::ShardWalker::open(&repo.work_tree).ok()?;
    let bytes = walker
        .read_artifact_any(checkpoint_id, "metadata.json")
        .ok()
        .flatten()?;
    serde_json::from_slice(&bytes).ok()
}

/// The stored redacted transcript out of the shard — canon-3 name first,
/// legacy names for older shards (the same resolution `prompt_from_shard`
/// serves the prompt through).
fn read_shard_transcript(repo: &rift_git::RepoPaths, checkpoint_id: &str) -> Option<Vec<u8>> {
    let walker = rift_recall_substrate::ShardWalker::open(&repo.work_tree).ok()?;
    ["transcript.jsonl", "full.jsonl", "sessions/0/full.jsonl"]
        .into_iter()
        .find_map(|name| walker.read_artifact_any(checkpoint_id, name).ok().flatten())
}

/// The shard's read-fact content hashes (`tool_calls.jsonl`) — what the
/// session had read when it was archived; the seed of the recall cursor.
fn shard_read_fact_hashes(repo: &rift_git::RepoPaths, checkpoint_id: &str) -> Vec<String> {
    let Ok(walker) = rift_recall_substrate::ShardWalker::open(&repo.work_tree) else {
        return Vec::new();
    };
    let Ok(Some(bytes)) = walker.read_artifact_any(checkpoint_id, "tool_calls.jsonl") else {
        return Vec::new();
    };
    bytes
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_slice(l).ok()?;
            v.get("sha256")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
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

fn read_yes() -> bool {
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim(), "y" | "Y" | "yes")
}

/// Terminal-safe rendering of caller-supplied names (branch names and ids are
/// data; the label charset is the existing neutralizer).
fn safe(s: &str) -> crate::provenance::Label {
    crate::provenance::Label::of(s)
}

fn short(oid: &str) -> &str {
    oid.get(..7).unwrap_or(oid)
}

#[cfg(test)]
mod tests {
    /// An `idle` journal cache is the machine's pre-session
    /// state — no live-state sweep reaches it and no legal transition ends it,
    /// so five sat STUCK for weeks. The explicit verb closes them at the
    /// journal level; a fresh idle cache (an agent between its state file and
    /// its `Start`) survives stop-all and falls only to a targeted stop.
    #[test]
    fn stop_closes_stuck_idle_caches_and_spares_fresh_ones() {
        use rift_shadow::hooks::session_cache::{read_cache, write_cache, SessionCache};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("repo");
        let git_dir = rift_git::test_support::fake_repo(&root).unwrap();
        let cache = |id: &str, updated_at_ms: i64| {
            SessionCache::new(id, "claude-code", "idle", updated_at_ms)
        };
        write_cache(&git_dir, &cache("stuck-idle", 1_000)).unwrap();
        let fresh_at = crate::hooks::now_ms();
        write_cache(&git_dir, &cache("fresh-idle", fresh_at)).unwrap();

        let out = stop(&root, None).unwrap();
        assert!(out.human.contains("closed stuck-idle"), "{}", out.human);
        assert_eq!(
            read_cache(&git_dir, "stuck-idle").unwrap().unwrap().state,
            "ended"
        );
        assert_eq!(
            read_cache(&git_dir, "fresh-idle").unwrap().unwrap().state,
            "idle",
            "stop-all must not race an agent that just opened its state file"
        );

        // The operator naming the session overrides the freshness guard.
        let out = stop(&root, Some("fresh-idle")).unwrap();
        assert!(out.human.contains("closed fresh-idle"), "{}", out.human);
        assert_eq!(
            read_cache(&git_dir, "fresh-idle").unwrap().unwrap().state,
            "ended"
        );
    }

    use super::*;

    /// Every rung of the age ladder, at both sides of each boundary. The
    /// coarsening is the point: a session list must not print `3600s`.
    #[test]
    fn the_age_ladder_coarsens_at_each_boundary() {
        for (ms, expected) in [
            (0_i64, "0s"),
            (999, "0s"),
            (1_000, "1s"),
            (59_000, "59s"),
            (60_000, "1m"),
            (3_599_000, "59m"),
            (3_600_000, "1h"),
            (86_399_000, "23h"),
            (86_400_000, "1d"),
            (864_000_000, "10d"),
        ] {
            assert_eq!(human_age(ms), expected, "human_age({ms})");
        }
    }

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

    /// Object ids are abbreviated for display, and a short or empty id is
    /// returned whole rather than panicking on a byte range.
    #[test]
    fn abbreviation_never_slices_past_the_end() {
        assert_eq!(short("0123456789abcdef"), "0123456");
        assert_eq!(short("0123456"), "0123456");
        assert_eq!(short("012"), "012");
        assert_eq!(short(""), "");
        // A multi-byte id is never sliced THROUGH a char boundary: byte 7 is
        // a boundary in the first (h é l l o - = 7 bytes) so it truncates, and
        // is mid-character in the second, where the whole id is returned
        // rather than panicking on the range.
        assert_eq!(short("héllo-世界"), "héllo-");
        assert_eq!(short("héllo世界"), "héllo世界");
    }

    /// Plant a session cache file directly under `git_dir` — the shape the
    /// hook writes, and (with an explicit `work_tree`) the shape only a
    /// hand-written file has, which is exactly what the credit discriminator
    /// exists to adjudicate.
    fn plant(git_dir: &Path, id: &str, state: &str, work_tree: Option<&Path>, updated_at_ms: i64) {
        let dir = git_dir.join("rift-sessions");
        std::fs::create_dir_all(&dir).unwrap();
        let cache = rift_shadow::hooks::session_cache::SessionCache {
            schema: rift_shadow::hooks::session_cache::SCHEMA,
            session_id: id.to_owned(),
            state: state.to_owned(),
            agent: "claude-code".to_owned(),
            current_turn_ordinal: 1,
            turn_open: false,
            compaction_boundary: None,
            updated_at_ms,
            native_id_map: std::collections::BTreeMap::new(),
            transcript_path: None,
            work_tree: work_tree.map(|w| w.display().to_string()),
            // A planted cache has observed no worktree: the memo is UNKNOWN,
            // which is what a cache file written before worktree provenance
            // existed reads as too.
            last_worktree: None,
        };
        std::fs::write(
            dir.join(format!("{id}.json")),
            serde_json::to_vec(&cache).unwrap(),
        )
        .unwrap();
    }

    /// A family, hermetically: a main work tree and one linked member, in the
    /// exact file shapes `git worktree add` writes.
    fn family(root: &Path) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let main = root.join("main");
        std::fs::create_dir_all(&main).unwrap();
        let main_git = rift_git::test_support::fake_repo(&main).unwrap();
        let member = root.join("member");
        let admin = rift_git::test_support::fake_worktree(&main_git, &member, "member").unwrap();
        (main, main_git, member, admin)
    }

    /// **From a member, the main work tree's journal is in the list.** The
    /// readers enumerated `repo.work_tree` + `list_worktrees`, which from a
    /// linked worktree is that worktree and its siblings — never main, where
    /// nearly every session of a real repository lives.
    #[test]
    fn the_family_journal_reads_every_member_main_included() {
        let tmp = tempfile::tempdir().unwrap();
        let (_main, main_git, member, admin) = family(tmp.path());
        plant(&main_git, "on-main", "ended", None, 10);
        plant(&admin, "on-member", "ended", None, 20);

        // The vantage is the MEMBER: the case that was broken.
        let paths = rift_git::discover(&member).unwrap();
        let journal = family_journal(&paths);

        assert_eq!(journal.len(), 2, "main and the member");
        assert!(journal[0].member.is_main, "main is first");
        assert_eq!(
            journal[0]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["on-main"],
            "main's own journal, read from the member"
        );
        assert_eq!(
            journal[1]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["on-member"]
        );
    }

    /// **The cache's own claim decides the credit, not the directory.** A cache sitting in
    /// the shared registry that NAMES a member is that member's; one that
    /// names nothing belongs to the registry it was found in. Neither is ever
    /// credited to two members, and a session driven in one worktree is never
    /// attributed to another.
    #[test]
    fn a_cache_is_credited_to_the_member_it_names_not_to_the_registry_it_sits_in() {
        let tmp = tempfile::tempdir().unwrap();
        let (main, main_git, member, _admin) = family(tmp.path());
        plant(&main_git, "claims-the-member", "ended", Some(&member), 30);
        plant(&main_git, "claims-nothing", "ended", None, 20);
        plant(
            &main_git,
            "claims-elsewhere",
            "ended",
            Some(Path::new("/nowhere")),
            10,
        );

        let paths = rift_git::discover(&main).unwrap();
        let journal = family_journal(&paths);

        let ids = |i: usize| {
            journal[i]
                .sessions
                .iter()
                .map(|s| s.session_id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(0),
            vec!["claims-elsewhere".to_owned(), "claims-nothing".to_owned()],
            "a claim on no member of this family falls back to the registry's owner"
        );
        assert_eq!(ids(1), vec!["claims-the-member".to_owned()]);
    }

    /// A family the enumeration cannot see all of still answers for the
    /// vantage. The member's administrative `gitdir` file is unreadable, so
    /// `family_members` skips the entry (a row without a path would be an
    /// invented one) — the reader must still be that member's own journal
    /// plus what it can enumerate, never an empty list.
    #[test]
    fn a_family_that_cannot_be_enumerated_still_reads_the_vantages_own_journal() {
        let tmp = tempfile::tempdir().unwrap();
        let (_main, main_git, member, admin) = family(tmp.path());
        plant(&main_git, "on-main", "ended", None, 10);
        plant(&admin, "on-member", "ended", None, 20);
        std::fs::remove_file(admin.join("gitdir")).unwrap();

        let paths = rift_git::discover(&member).unwrap();
        let journal = family_journal(&paths);

        assert_eq!(journal.len(), 2, "the vantage, and the main it could prove");
        assert!(!journal[0].member.is_main, "the vantage is prepended");
        assert_eq!(journal[0].member.git_dir, paths.git_dir);
        assert_eq!(
            journal[0]
                .sessions
                .iter()
                .map(|s| s.session_id.as_str())
                .collect::<Vec<_>>(),
            vec!["on-member"]
        );
        assert_eq!(journal[1].sessions.len(), 1, "and main is still read");
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

    /// ★ **The recovery markers are swept by their own age, and only they
    /// are.** One marker per compaction generation per window accumulated
    /// with nothing to remove it; a fresh one stays, because dropping it hands
    /// a live window the same recovery map a second time, and a name this tool
    /// did not write is never touched however it got there.
    #[test]
    fn the_recovery_markers_expire_and_a_fresh_one_and_a_stranger_do_not() {
        let tmp = tempfile::tempdir().unwrap();
        let work_tree = tmp.path();
        let dir = work_tree.join(".rift").join("state").join("recovery");
        std::fs::create_dir_all(&dir).unwrap();
        let marker = |name: &str| {
            let path = dir.join(name);
            std::fs::write(&path, b"").unwrap();
            path
        };
        let old = marker(&format!("{}.json", "a".repeat(64)));
        let fresh = marker(&format!("{}.json", "b".repeat(64)));
        let stranger = marker("notes.txt");
        let ttl_ms = 7 * 86_400_000;
        let now = 1_800_000_000_000;
        // The two ages, set outright rather than waited for.
        let at = |path: &Path, ms: i64| {
            let when = std::time::UNIX_EPOCH
                + std::time::Duration::from_millis(u64::try_from(ms).unwrap());
            std::fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap()
                .set_modified(when)
                .unwrap();
        };
        at(&old, now - ttl_ms - 1);
        at(&fresh, now - 1);
        at(&stranger, now - ttl_ms - 1);

        assert_eq!(
            sweep_recovery_markers(work_tree, now, ttl_ms, true),
            1,
            "a dry run counts the one marker past its TTL and removes nothing"
        );
        assert!(old.exists(), "and it is still there afterwards");

        assert_eq!(sweep_recovery_markers(work_tree, now, ttl_ms, false), 1);
        assert!(!old.exists(), "the spent marker goes");
        assert!(fresh.exists(), "the live window keeps its own");
        assert!(
            stranger.exists(),
            "and a file this tool did not write stays"
        );
    }

    /// A checkout with no recovery directory at all sweeps nothing and says
    /// so, rather than treating absence as a failure.
    #[test]
    fn a_checkout_that_has_never_recovered_sweeps_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(sweep_recovery_markers(tmp.path(), 1_000, 1, false), 0);
    }
}
