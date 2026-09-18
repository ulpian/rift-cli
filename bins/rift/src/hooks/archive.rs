//! Condensing a queued commit into a checkpoint and archiving it: the native
//! archive run, the chain entry, attaching and importing an archive, the
//! projection levelling and soak accrual that keep the binary of record
//! honest. Separate because it is the one place a checkpoint is minted.

use std::path::{Path, PathBuf};

use rift_core::Result;

use super::git::{effective_config, session_raw_capture, state_from_token, strategy_kind};
use super::{now_ms, projection_db_at};

/// Run one live archive. Capture never blocks the git op.
///
/// There is one archive path: the native one, which writes the shard itself.
/// This used to consult a per-capability route table read from
/// `.rift/rewrite.toml` and, on two of its three answers, shell out to a pinned
/// foreign binary — deferring when that binary was not installed. The route
/// table, its wrapper leg and its kill-switch were deleted on 2026-08-11; what
/// is left is what every repository was already doing.
///
/// Returns **the checkpoint that was written**, not whether the call ran. The id
/// is carried rather than a flag because a caller that closes a session
/// lifecycle on the strength of an archive must know there was one, and the only
/// honest evidence of that is the id `run_decision` minted. A hand-written
/// `Ok(true)` beside a `return` is exactly how an earlier silent-loss defect
/// was written — the flag said "archived" where no checkpoint existed;
/// threading the `Option<CheckpointId>` makes that spelling unavailable.
#[allow(clippy::too_many_arguments)]
pub(super) fn archive_now(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    work_tree: &Path,
    commit: &str,
    session: &str,
    agent: &str,
    decision: rift_shadow::hooks::strategy::ArchiveDecision,
    now: i64,
) -> Result<Option<rift_shadow::CheckpointId>> {
    use rift_shadow::backend::NativeBackend;

    let repo = rift_git::discover(work_tree)?;
    let native = NativeBackend::new(rift_git::PlumbingGitWrite::new(repo.clone()));
    let raw = session_raw_capture(&repo.work_tree);
    let facts = read_facts_for_archive(rt, store, agent, session);
    let (packs, notes) = crate::redactor_packs::capture_rule_packs(&repo.work_tree);
    report_pack_notes(&notes);
    // A session-end/sweep condense archives the transcript too — same source,
    // same boundary discipline as the post-commit worker.
    let transcript = session_transcript(&repo.git_dir, session, &packs);
    let (transcript, transcript_raw_lines) = match transcript {
        Some((pair, raw_lines)) => (Some(pair), Some(raw_lines)),
        None => (None, None),
    };

    let archived = rt.block_on(rift_shadow::run_decision(
        &native,
        store,
        work_tree,
        session,
        &raw,
        decision,
        commit,
        now,
        facts,
        &packs,
        transcript,
        // No hook payload carries subagent task artifacts yet: the SubagentStop
        // wire reaches the event stream (GitFact), but nothing collects a
        // per-task transcript address for the worker to read. A known gap,
        // deliberately left open — the archive layer beneath is complete.
        Vec::new(),
    ));
    if let (Ok(Some(_)), Some(lines)) = (&archived, transcript_raw_lines) {
        let progress = rift_shadow::hooks::TranscriptProgress {
            schema: rift_shadow::hooks::session_cache::SCHEMA,
            session_id: session.to_owned(),
            raw_lines: lines,
            updated_at_ms: now,
        };
        if let Err(e) = rift_shadow::hooks::write_transcript_progress(&repo.git_dir, &progress) {
            tracing::warn!(error = %e, "transcript progress not recorded");
        }
    }
    // The shard is in git; its POSITION is not, until this runs — the same
    // discipline the post-commit worker keeps. Before this call existed the two
    // other archive roads (the session-end hook and the idle sweep, both of
    // which land here) recorded no entry, so every `session stop` and every
    // sweep-finalize left a chain TAIL gap for the next commit or a doctor
    // `--fix` to heal. Loud-never-fatal, like every chain write.
    if let Ok(Some(id)) = &archived {
        let t = crate::hooktime::HookTimer::from_env("archive-now");
        record_chain_entry(&t, &repo, Some(id.as_str()), now);
    }
    archived
}

/// The session's captured read facts, mapped for the shard's
/// `tool_calls.jsonl`. Best-effort: a query failure archives an empty
/// set rather than blocking the checkpoint.
pub(super) fn read_facts_for_archive(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    agent: &str,
    session: &str,
) -> Vec<rift_shadow::backend::ArchivedReadFact> {
    let row_id = rift_shadow::ids::session_row_id(agent, session);
    rt.block_on(rift_store::session_read_facts(store, &row_id))
        .unwrap_or_default()
        .into_iter()
        .map(|f| rift_shadow::backend::ArchivedReadFact {
            tool_use_id: f.tool_call_id,
            tool_name: f.tool_name,
            path: f.path,
            content_sha256: f.content_sha256,
            input_tokens: f.input_tokens,
        })
        .collect()
}

/// The deferred half of `post-commit`: bind identities, diff the commit, replay
/// turn-boundary attribution, decide, archive, and persist the durable C-diff
/// facts. This is verbatim the work that used to run inside the hook; only
/// *when* it runs changed, and the clock it reads is the entry's queue time so a
/// deferred run neither backdates nor advances the archive decision.
pub(super) fn condense_queued(
    t: &crate::hooktime::HookTimer,
    git_dir: &Path,
    entry: &crate::capture_queue::QueueEntry,
    path: &Path,
) -> Result<()> {
    use rift_shadow::hooks::strategy::{decide, GitEvent, TriggerCtx};

    // The checkout this commit was made in is gone: a subagent's
    // temporary worktree removed before the detached worker ran, or a
    // `git worktree remove` between the commit and the drain. The commit is
    // still in the family's shared object store, so it condenses commit-only
    // rather than failing until the retry budget drops it.
    if !entry.work_tree.is_dir() {
        return condense_commit_only(t, git_dir, entry);
    }
    let repo = rift_git::discover(&entry.work_tree)?;
    note_stale_rewrite_toml(&repo.git_dir, &repo.work_tree);
    let reader = t.phase("gix_open", || {
        rift_git::GixBlobReader::open(&repo.work_tree)
    })?;
    let store = t.phase("open_projection", || {
        rift_store::open_capture_projection(projection_db_at(&repo.work_tree)?)
    })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let now = entry.queued_at_ms;

    // Bind any unbound sessions to the local developer identity (best-effort).
    let _ = t.phase("bind_sessions", || {
        rt.block_on(crate::identity::bind_new_sessions(
            &store,
            &repo.work_tree,
            crate::identity::install_salt,
            now,
        ))
    });

    // The committed diff, computed once: it feeds the sensitive-path trigger,
    // the turn-boundary attribution replay, and the durable C-diff facts.
    let mut capture = t.phase("capture_commit", || {
        rift_shadow::attribution::capture_commit(&reader, &entry.commit)
    })?;
    let cfg = effective_config(&repo.work_tree);
    let sensitive_path_touched = capture.files.iter().any(|f| cfg.is_sensitive_path(&f.path));

    if let (Some(session_id), Some(agent)) = (&entry.session_id, &entry.agent) {
        // Turn-boundary attribution: replay the session's H_i/A_i edit
        // stack through the LIFO engine so committed lines resolve to
        // agent/human instead of all-unknown. Best-effort — a replay failure
        // leaves the honest unknown counts.
        let _ = t.phase("enrich_attribution", || {
            rt.block_on(rift_shadow::enrich_attribution(
                &store,
                &reader,
                agent,
                session_id,
                &mut capture,
            ))
        });
        let last = rt.block_on(rift_store::latest_archive_ms(&store, session_id))?;
        let decision = decide(
            strategy_kind(cfg.checkpoint_strategy()),
            &TriggerCtx {
                git_event: Some(GitEvent::PostCommit),
                session_state: state_from_token(entry.session_state.as_deref().unwrap_or("")),
                sensitive_path_touched,
                clock_ms: now,
                last_archive_ms: last,
                schedule_interval_ms: cfg.schedule_interval_ms(),
            },
        );
        if decision.archive {
            let checkpoint = run_archive(
                t,
                &rt,
                &store,
                &repo,
                &capture,
                &entry.commit,
                session_id,
                agent,
                decision,
                now,
            )?;
            // The shard is in git; its POSITION is not, until this runs. Same
            // worker op as the archive, and before the soak's verify — which now
            // refuses a checkpoint that has no entry, so recording it later
            // would mean a red run in between.
            record_chain_entry(t, &repo, checkpoint.as_deref(), now);
            // The soak accrues against the checkpoint that was just written, and
            // there is no arm here that accrues without one. `run_archive`
            // inside `decision.archive` returns `Ok(Some(_))` or `Err`, so this
            // is the type's shape being consumed, not a case being guarded —
            // said plainly because the mechanism this replaced was an
            // `Option` gate at this very call whose `None` arm could not fire.
            if let Some(id) = checkpoint.as_deref() {
                if let Some(reason) =
                    accrue_soak(t, &repo.work_tree, &crate::verify::Witness::of(id))
                {
                    crate::capture_queue::mark_error(path, &reason);
                }
            }
            return Ok(());
        }
    }

    // No live session: still record the durable C-diff facts
    // (file_revision + changed_hunk); attribution waits for a checkpoint.
    t.phase("persist_capture", || {
        rt.block_on(rift_shadow::persist_commit_capture(&store, &capture, None))
    })?;
    Ok(())
}

/// The `archive_decision.reason` a commit-only degrade records.
///
/// A new **value** in an existing relation, deliberately: the design review
/// established that `capture_completeness` is a computed attribution factor
/// and not a column anywhere, so the fact that the tree was unavailable is
/// carried by this token plus the facts the row's siblings do and do not hold —
/// the replay lowers confidence from those on its own. No schema moves for it.
pub(crate) const WORKTREE_MISSING_COMMIT_ONLY: &str = "worktree-missing:commit-only";

/// Condense a queued commit whose working tree is **gone**.
///
/// Commit-only, because that is exactly what survives. The commit object is in
/// the family's shared store, so its diff, its file revisions and its hunks are
/// all still derivable — through any surviving member, since the object store
/// is the thing worktrees share. The tree is not, so nothing that reads a
/// checkout runs: no `capture_document`, no `.worktreeinclude` (absent, which
/// is not the same as empty), no session raw capture, no transcript.
///
/// **And no checkpoint.** Archiving from a surviving member would write a shard
/// under that member's worktree token for a session that lived in another one,
/// which is precisely the misattributed credit the provenance rules forbid: a
/// worktree may never be credited with another worktree's work. So the archive is not
/// attempted, and the `archive_decision` says `archived = false` with
/// [`WORKTREE_MISSING_COMMIT_ONLY`]: the gap becomes a row instead of a
/// silence.
///
/// Settles the entry either way — a retry can only find the same missing
/// directory — except when there is no surviving member to record against at
/// all, which is the one case worth retrying and the one the caller's bounded
/// retry budget already handles.
///
/// # Errors
/// [`rift_core::RiftError`] when no member of the family survives to record
/// under, or the projection refuses the audit row.
fn condense_commit_only(
    t: &crate::hooktime::HookTimer,
    git_dir: &Path,
    entry: &crate::capture_queue::QueueEntry,
) -> Result<()> {
    let Some(main) = surviving_main_work_tree(git_dir) else {
        return Err(rift_core::RiftError::NotFound(format!(
            "no surviving member of this family to condense {} against",
            entry.commit
        )));
    };
    let store = t.phase("open_projection", || {
        rift_store::open_capture_projection(projection_db_at(&main)?)
    })?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let now = entry.queued_at_ms;

    // The commit's own facts, read through a member that still has a checkout.
    // Best-effort: a repository that has since had the object pruned still owes
    // the audit row below, and losing the diff is not a reason to lose that too.
    let captured = t.phase("capture_commit", || {
        let reader = rift_git::GixBlobReader::open(&main)?;
        rift_shadow::attribution::capture_commit(&reader, &entry.commit)
    });
    match captured {
        Ok(capture) => {
            if let Err(e) = t.phase("persist_capture", || {
                rt.block_on(rift_shadow::persist_commit_capture(&store, &capture, None))
            }) {
                tracing::warn!(error = %e, "commit-only capture facts not persisted");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, commit = entry.commit, "commit-only: no diff available");
        }
    }

    let strategy = strategy_kind(effective_config(&main).checkpoint_strategy());
    let session = entry.session_id.clone().unwrap_or_default();
    let row = rift_store::ArchiveDecisionRow {
        // The same derivation `rift_shadow`'s own decisions use — session,
        // strategy, reason, clock, checkpoint — so a re-drained entry at the
        // same queue time is one row, not two.
        decision_id: rift_shadow::sha256_hex(&[
            session.as_bytes(),
            b"\0",
            strategy.as_str().as_bytes(),
            b"\0",
            WORKTREE_MISSING_COMMIT_ONLY.as_bytes(),
            b"\0",
            now.to_string().as_bytes(),
            b"\0",
            b"",
        ]),
        session_id: entry.session_id.clone(),
        strategy: strategy.as_str().to_owned(),
        reason: WORKTREE_MISSING_COMMIT_ONLY.to_owned(),
        archived: false,
        checkpoint_id: None,
        decided_at: now,
        schema_version: 1,
    };
    t.phase("archive_decision", || {
        rt.block_on(rift_store::record_archive_decision(&store, &row))
    })?;
    Ok(())
}

/// The family member a drain can still read a gone checkout's commit through:
/// the main work tree, when it is there.
///
/// `git_dir` is the gitdir being drained, so the walk that resolves the family
/// starts from a gitdir rather than a checkout — and a walk can land somewhere
/// unrelated (a `--separate-git-dir` gitdir kept under another repository, say).
/// It is therefore proved rather than assumed: the gitdir being drained has to
/// be one of the family's own members, or nothing is written under it.
fn surviving_main_work_tree(git_dir: &Path) -> Option<PathBuf> {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let paths = rift_git::discover(git_dir).ok()?;
    let members = rift_git::family_members(&paths);
    if !members.iter().any(|m| canon(&m.git_dir) == canon(git_dir)) {
        return None;
    }
    members
        .into_iter()
        .find(|m| m.is_main && !m.prunable)
        .map(|m| m.work_tree)
}

/// The one thing left that reads `.rift/rewrite.toml`: a check that it is not
/// there, so a repository that still has one is TOLD rather than quietly obeyed
/// in the negative.
///
/// **Why this exists at all.** The standing rule is that tripping the
/// kill-switch for a capability whose fallback leg cannot run must be *fail-loud
/// capture-off*, never a silent revert. Removing the retired wrapper route turned
/// `kill_switch = true` into a no-op — precisely the silence that rule
/// forbids. The clause is honoured by saying so out loud instead of by keeping
/// a dial that moves nothing.
///
/// **Presence is the whole decision; nothing is parsed.** Not "does it mention
/// `kill_switch`", not "does it name a wrapper route" — the file's *legacy*
/// content is inert now, so every sentence this note says is true of every
/// `rewrite.toml` there can be, including one that only ever said
/// `archive = "native"`. Parsing to decide would mean a repository pinned to
/// the route that happens to match reality keeps a file it believes configures
/// something. It does not. (Judgement recorded per the deletion brief: the
/// alternative — note only on `kill_switch`/wrapper content — was rejected
/// because it leaves the benign-looking cases silently lying.)
///
/// Never blocks git: it runs in the detached worker, after git has returned,
/// and every step is best-effort. `mark_note` overwrites by key, so a
/// repository that ignores it pays one line per commit until the file goes,
/// not one line per commit forever.
fn note_stale_rewrite_toml(git_dir: &Path, work_tree: &Path) {
    if work_tree.join(".rift").join("rewrite.toml").exists() {
        crate::capture_queue::mark_note(git_dir, "rewrite-toml", STALE_REWRITE_TOML);
    }
}

/// What a leftover `.rift/rewrite.toml` is told. One sentence of history, one
/// of consequence, one of remedy — a note that says only "remove it" leaves the
/// reader wondering what they are losing.
pub(crate) const STALE_REWRITE_TOML: &str =
    ".rift/rewrite.toml: the wrapper route was deleted; this file does nothing; remove it";

/// Ingest any checkpoint shard that reached git but not the projection.
///
/// The same walk `rift rebuild` runs, and idempotent for the same reason: every
/// row id is content-addressed, so re-running is a set-union no-op.
///
/// # Errors
/// Propagated to the caller, which swallows them — a levelling failure must
/// never fail a drain.
pub(crate) fn level_projection(work_tree: &Path) -> Result<()> {
    let family = crate::family::resolve_at(work_tree)?;
    let repo_id = family.family_id.clone();
    let store = rift_store::open_capture_projection(rift_store::projection_db_for_family(&family))?;
    let reader = rift_recall_substrate::GixSubstrate::open(work_tree)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    rt.block_on(rift_shadow::ingest_from_substrate(
        &store,
        &reader,
        &repo_id.to_string(),
        "claude-code",
        now_ms(),
    ))?;
    Ok(())
}

/// The attach verb's archive: the `run_archive` composition
/// callable **outside** the hook flow — attach refuses to run from hooks, and
/// this path runs on the operator's terminal with no latency budget. The
/// audit reason rides `Commit` (the anchor IS the last commit); a distinct
/// `Attach` token would be an ArchiveReason ripple across rift-shadow-hooks,
/// Worth doing if a reader ever needs it distinguishable in the audit row.
pub(crate) fn attach_archive(
    repo: &rift_git::RepoPaths,
    store: &rift_store::SqliteStore,
    rt: &tokio::runtime::Runtime,
    commit: &str,
    session_id: &str,
    agent: &str,
    now: i64,
) -> Result<Option<String>> {
    use rift_shadow::hooks::strategy::{ArchiveDecision, ArchiveReason, StrategyKind};
    let t = crate::hooktime::HookTimer::from_env("attach");
    let reader = rift_git::GixBlobReader::open(&repo.work_tree)?;
    let mut capture = rift_shadow::attribution::capture_commit(&reader, commit)?;
    // Best-effort like the worker: a replay failure leaves honest unknowns.
    let _ = rt.block_on(rift_shadow::enrich_attribution(
        store,
        &reader,
        agent,
        session_id,
        &mut capture,
    ));
    let decision = ArchiveDecision {
        archive: true,
        reason: ArchiveReason::Commit,
        strategy: StrategyKind::ManualCommit,
    };
    let checkpoint = run_archive(
        &t, rt, store, repo, &capture, commit, session_id, agent, decision, now,
    )?;
    record_chain_entry(&t, repo, checkpoint.as_deref(), now);
    if let Some(id) = checkpoint.as_deref() {
        // The explicit `level_projection` that used to stand here is GONE, not
        // moved: `accrue_soak` below runs a verify, and `verify::run_at_with`
        // now levels before it compares. The shard's rows
        // (`checkpoint`, `session_in_checkpoint`) still land, so explain/blame
        // resolve the attached checkpoint immediately — one levelling instead of
        // two, and the comment no longer describes a call the worker stopped
        // making.
        //
        // Attach IS a condensation — the same `run_archive` the worker calls,
        // over a real transcript and a real commit — and it used to accrue
        // nothing at all, while twenty bare `rift verify` runs would
        // have filled the soak in its place. The marker rides the repository
        // channel rather than a queue entry, because this path has none: attach
        // runs on the operator's terminal, not out of the drain.
        if let Some(reason) = accrue_soak(&t, &repo.work_tree, &crate::verify::Witness::of(id)) {
            crate::capture_queue::mark_note(&repo.git_dir, "soak", &reason);
        }
    }
    Ok(checkpoint)
}

/// The import verb's archive: one **read-only** checkpoint
/// out of a foreign transcript file. No journal entry, no session events, no
/// commit-diff persistence — the capture is empty because attribution has no
/// basis in a bulk-imported transcript, and an empty capture persists
/// nothing. The transcript reaches the shard through the one redaction gate
/// (`archived_transcript` seals via `rift-shadow-redact`, the single seal
/// path). The projection is NOT leveled here — the import loop levels once
/// at its end.
pub(crate) fn import_archive(
    repo: &rift_git::RepoPaths,
    store: &rift_store::SqliteStore,
    rt: &tokio::runtime::Runtime,
    commit: &str,
    session_id: &str,
    raw_transcript: rift_core::SecretBytes,
    now: i64,
) -> Result<Option<String>> {
    use rift_shadow::backend::NativeBackend;
    use rift_shadow::hooks::strategy::{ArchiveDecision, ArchiveReason, StrategyKind};

    let t = crate::hooktime::HookTimer::from_env("import");
    let (packs, notes) = crate::redactor_packs::capture_rule_packs(&repo.work_tree);
    record_pack_notes(&repo.git_dir, &notes);
    let (pair, _raw_lines) =
        rift_shadow::transcript::archived_transcript(&raw_transcript, 0, &packs)?;
    let capture = rift_shadow::attribution::CommitCapture {
        commit_id: commit.to_owned(),
        files: Vec::new(),
    };
    let raw_doc = session_raw_capture(&repo.work_tree);
    // The import marker: the shard's `metadata.json` and the
    // projection row both carry `import`, so blame/why/explain can say the
    // checkpoint is an imported transcript rather than a live archive.
    // Imports landed before 2026-08-29 stay `manual-commit` forever — the
    // checkpoint table is append-only and their metadata is content-addressed.
    let decision = ArchiveDecision {
        archive: true,
        reason: ArchiveReason::Commit,
        strategy: StrategyKind::Import,
    };
    let native = NativeBackend::new(rift_git::PlumbingGitWrite::new(repo.clone()));
    let checkpoint = rt.block_on(rift_shadow::archive_and_capture_with(
        &capture,
        &native,
        store,
        &repo.work_tree,
        commit,
        session_id,
        &raw_doc,
        decision,
        now,
        Vec::new(),
        &packs,
        Some(pair),
        Vec::new(),
    ))?;
    record_chain_entry(&t, repo, checkpoint.as_deref(), now);
    Ok(checkpoint)
}

/// Record the archived checkpoint's position in `refs/rift/chain/v1`.
///
/// **Loud, never fatal** — the same trade the soak makes. A chain-maintenance
/// refusal is written beside the queue entry so the next hook says it out loud;
/// it never fails the drain, because refusing to drain would strand capture
/// input over an ordering problem `verify` is about to report anyway.
fn record_chain_entry(
    t: &crate::hooktime::HookTimer,
    repo: &rift_git::RepoPaths,
    checkpoint: Option<&str>,
    now: i64,
) {
    // No checkpoint means the archive decided not to write one: there is no
    // position to record, and inventing one would be worse than nothing.
    let Some(id) = checkpoint else { return };
    // The whole `RepoPaths`, because the chain write and the push-queue
    // enqueue it triggers live in two different directories now: git reads the
    // chain through this checkout's gitdir, and the queue that names the refs
    // belongs to the family's common dir.
    let git_dir = repo.git_dir.as_path();
    // Chain markers ride the repository channel (`mark_note`, key "chain"),
    // NOT the queue entry's own `.error` file: `accrue_soak` runs moments
    // later on the same entry path, and a chain refusal makes that verify
    // unclean BY CONSTRUCTION — so an entry-path marker written here was
    // always clobbered before anyone read it. An adversarial review planted
    // this twice: both refusal texts arrived as "soak verify is not clean: …".
    match t.phase("chain_append", || {
        crate::chain::record_archived(git_dir, &repo.common_dir, id, now)
    }) {
        Ok(step) => {
            if let Some(marker) = step.marker() {
                crate::capture_queue::mark_note(git_dir, "chain", &marker);
            } else {
                tracing::debug!(checkpoint = id, "chain position recorded");
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "chain entry could not be recorded");
            crate::capture_queue::mark_note(
                git_dir,
                "chain",
                &format!("the chain entry was NOT recorded: {e}"),
            );
        }
    }
}

/// Run one soak observation after a successful condense.
///
/// For a capability with no comparison leg to run against — the archive — the
/// soak is defined as *N consecutive verify-green real native executions
/// recorded in `verification_run`*, and accruing them was called an operational
/// job rather than a coding one. This is that job, mechanized:
/// every real commit that condenses now contributes one row, so the counter
/// moves on its own instead of waiting for someone to remember.
///
/// **It runs in the detached worker, after git has already returned**, so it
/// costs the hook budget nothing — the 100 ms ceiling is measured on git
/// latency and this is off that path fully.
///
/// **A verify failure never fails the drain.** The row is recorded either way
/// (that is what makes the soak honest — a streak that silently skipped its
/// failures is not a streak), and the caller places the non-clean verdict on
/// whichever channel it has: the queue entry for the worker, the repository
/// note for `attach`, doctor's own output for the sweep. Loud, not fatal:
/// refusing to drain would strand capture input over a *reporting* problem,
/// which is the wrong half of the loud-versus-available trade.
///
/// **It is `pub(crate)` because every site that TAKES an observation must take
/// it the same way, and that was measured rather than assumed.** Doctor's sweep
/// used to call `soak_verify` directly and so skipped the levelling below — so
/// every soak row it wrote was recorded against a projection still behind git,
/// which `verify` correctly reads as tamper. The sweep therefore accrued a
/// *dirty* run every time, and the counter it then printed was reset by the act
/// of taking it. The old assertion counted rows and could not see it; asserting
/// that the run was CLEAN and witnessed is what surfaced it.
///
/// **THREE CONDENSE SITES TAKE NO OBSERVATION AT ALL. Enumerated, because "every
/// condense site" was written here while this list existed, and a summary is how
/// an unowned gap hides.** Each mints a real `CheckpointId`
/// through `archive_now` and calls nothing below:
///
/// * `on_pr_archive`'s `archive_now` call, reached from the `pre-push` arm of
///   `hook_body` — the `on-pr` checkpoint strategy;
/// * `drive_post_ingest`'s SessionEnd condense (the `Effect::RequestCondense`
///   arm), reached from `ingest_agent_hook`;
/// * `drive_post_ingest`'s scheduled-interval condense (the `decision.archive`
///   arm below it), same caller.
///
/// Cited by NAME and not by line: the first version of this list gave line
/// numbers that were ~32 lines stale by the time it was read. Names survive an
/// edit above them; numbers do not.
///
/// **The direction is UNDERCOUNT, never inflation**, which is why this is a
/// a declared, owned gap and not a defect: an unwitnessed run is not soak
/// evidence, so these sites make the counter move slower than the work does —
/// they can never clear the bar early. None of them accrued anything under the
/// older design either; this widens no hole, it only fails to close one.
///
/// Whether they SHOULD witness is a question about the soak's population, which
/// was settled only for the sites named at the time. Adding three more is an
/// owner call, not an implementer's — so it is written down here rather than
/// decided in passing.
///
/// `import_archive` (`:983`) is a fourth `CheckpointId` producer and is
/// deliberately NOT on that list, because its
/// capture is empty by construction and twenty `rift import` runs would
/// otherwise clear the soak — the `rift verify` inflation route wearing a
/// different verb.
pub(crate) fn accrue_soak(
    t: &crate::hooktime::HookTimer,
    work_tree: &Path,
    witness: &crate::verify::Witness,
) -> Option<String> {
    // Bring the projection level with git BEFORE verifying it.
    //
    // The archive writes the shard to git; nothing wrote the matching chain row
    // to the projection, so every condensed commit widened a gap that `verify`
    // correctly calls tamper ("projection has N, canonical source has N+1").
    // The soak could therefore never be clean on a repository anyone was
    // actually committing to — the counter was measuring the gap, not the code.
    //
    // This is the levelling an earlier attempt reverted, back with the thing it was
    // waiting for. It was correct about the problem and premature about the
    // cure: until the order was durable, a levelled projection only exposed the
    // sorted-hash fork underneath. Now the order comes from the chain ref, the
    // bridge's derivation is INDEPENDENT of the checker's, and their agreement
    // is the check — which is exactly what `verify` refusing to seed makes
    // load-bearing rather than decorative.
    //
    // The bridge is an idempotent set-union, so this is a no-op once level; and
    // it runs HERE, in the detached worker after git has returned, precisely
    // because it must not touch the hook's latency budget.
    //
    // Best-effort by design: a failed levelling leaves the verify to report the
    // gap it was going to report anyway. It never fails the drain.
    // The levelling this used to do here moved INTO `verify::run_at_with`, so
    // every path into verify gets it and no caller can
    // skip it — which is exactly what doctor's sweep did when it called
    // `soak_verify` directly and recorded a dirty run every time. The
    // `soak_verify` phase below now absorbs that time; the ordering is unchanged
    // and is no longer any caller's responsibility to remember.
    let verdict = t.phase("soak_verify", || {
        crate::verify::soak_verify(work_tree, witness)
    });
    match verdict {
        Ok(s) if s.is_clean() => {
            tracing::debug!(checkpoints = s.checkpoints, "soak verify clean");
            None
        }
        Ok(s) => Some(format!("soak verify is not clean: {}", s.verdict_token())),
        // An internal failure (projection unopenable, migration refused) is not a
        // verdict and must not be counted as one; it is still said out loud.
        Err(e) => {
            tracing::warn!(error = %e, "soak verify could not run");
            Some(format!("soak verify could not run: {e}"))
        }
    }
}

/// Archive the session's checkpoint and return the id it wrote (`None` when the
/// decision produced no checkpoint).
///
/// The return used to be a three-way `ArchiveLeg` because there were three
/// routes and one of them could decline for want of an installed binary. There
/// is one route now, so the honest return is the id itself — and it stays an id
/// rather than a `bool` for the reason it became one: a caller that closes a
/// session lifecycle on the strength of an archive must be able to name the
/// checkpoint, and the chain ref needs exactly that id. A shard in git with no
/// position in `refs/rift/chain/v1` is a checkpoint outside the order.
///
/// An archive failure is **returned**, not printed. It used to be an `eprintln!`
/// on the commit that lost it — loud where the loss happened — but this now
/// runs in a detached worker whose stderr is `/dev/null`, so printing would be
/// the very silence that property exists to prevent. The error travels back to the drain loop, which
/// writes it beside the queue entry, and the next commit's hook says it out
/// loud. The C-diff facts are durable either way: `archive_and_capture_with`
/// persists them before it surfaces the archive error.
#[allow(clippy::too_many_arguments)]
fn run_archive(
    t: &crate::hooktime::HookTimer,
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    capture: &rift_shadow::attribution::CommitCapture,
    commit: &str,
    session_id: &str,
    agent: &str,
    decision: rift_shadow::hooks::strategy::ArchiveDecision,
    now: i64,
) -> Result<Option<String>> {
    use rift_shadow::backend::NativeBackend;

    let native = NativeBackend::new(rift_git::PlumbingGitWrite::new(repo.clone()));
    let raw = t.phase("archive_raw_capture", || {
        session_raw_capture(&repo.work_tree)
    });
    let facts = t.phase("archive_read_facts", || {
        read_facts_for_archive(rt, store, agent, session_id)
    });
    // The repository's own redactor packs, folded in beside the built-ins. Any
    // note is recorded as a queue marker rather than printed — the worker's
    // stderr is /dev/null, so the next hook is what says it. (This comment used
    // to describe the intent while the code called `report_pack_notes`, i.e.
    // printed into the void; see `record_pack_notes`.)
    let (packs, notes) = t.phase("rule_packs", || {
        crate::redactor_packs::capture_rule_packs(&repo.work_tree)
    });
    // The session transcript: its ADDRESS was remembered off
    // the hook payload in the session cache; the bytes are read here — in the
    // detached worker, off git's latency budget — compacted on the raw side and
    // sealed once per artifact. Best-effort like the capture itself: a missing
    // or unreadable file archives a transcript-less checkpoint, never an error,
    // because the agent owns that file and may have moved or pruned it.
    let transcript = t.phase("archive_transcript", || {
        session_transcript(&repo.git_dir, session_id, &packs)
    });
    let (transcript, transcript_raw_lines) = match transcript {
        Some((pair, raw_lines)) => (Some(pair), Some(raw_lines)),
        None => (None, None),
    };

    let archived = t.phase("archive_native", || {
        rt.block_on(rift_shadow::archive_and_capture_with(
            capture,
            &native,
            store,
            &repo.work_tree,
            commit,
            session_id,
            &raw,
            decision,
            now,
            facts,
            &packs,
            transcript,
            Vec::new(),
        ))
    });
    // Record how far the archived transcript reached (non-empty RAW lines, the
    // compactor's own count) so the NEXT checkpoint's compaction starts at the
    // boundary this one ended at. Worker-owned sidecar, written only after the
    // archive landed — a failed archive leaves the old boundary so nothing is
    // skipped on retry.
    if let (Ok(Some(_)), Some(lines)) = (&archived, transcript_raw_lines) {
        let progress = rift_shadow::hooks::TranscriptProgress {
            schema: rift_shadow::hooks::session_cache::SCHEMA,
            session_id: session_id.to_owned(),
            raw_lines: lines,
            updated_at_ms: now,
        };
        if let Err(e) = rift_shadow::hooks::write_transcript_progress(&repo.git_dir, &progress) {
            tracing::warn!(error = %e, "transcript progress not recorded");
        }
    }
    // The pack notes are owed whether or not the archive landed: a pack that
    // would not compile degraded THIS commit's coverage, and the archive error
    // below must not swallow the reason.
    record_pack_notes(&repo.git_dir, &notes);
    archived
}

/// Read, compact and seal the session's transcript for the archive, or `None`
/// when there is nothing to archive: no cached `transcript_path` (the agent
/// never reported one), or the file is gone/unreadable (the agent owns it).
///
/// Returns the sealed artifact pair plus the compactor's own non-empty
/// raw-line count — the value the caller records as the next checkpoint's
/// `start_line` once the archive lands. The starting boundary comes from the
/// worker's transcript-progress sidecar; a missing or unreadable sidecar reads
/// as 0, which re-covers the whole session (self-contained shards make that a
/// correctness no-op — only the recorded boundary widens).
fn session_transcript(
    git_dir: &Path,
    session_id: &str,
    packs: &rift_shadow::redact::RulePackSet,
) -> Option<(rift_shadow::backend::ArchivedTranscript, u64)> {
    let path = rift_shadow::hooks::read_cache(git_dir, session_id)
        .ok()
        .flatten()?
        .transcript_path?;
    let bytes = std::fs::read(path).ok()?;
    let raw = rift_core::SecretBytes::new(bytes);
    let start_line = rift_shadow::hooks::read_transcript_progress(git_dir, session_id)
        .ok()
        .flatten()
        .map_or(0usize, |p| usize::try_from(p.raw_lines).unwrap_or(0));
    match rift_shadow::archived_transcript(&raw, start_line, packs) {
        Ok((pair, raw_lines)) => Some((pair, u64::try_from(raw_lines).unwrap_or(u64::MAX))),
        Err(e) => {
            // A redaction failure must not lose the checkpoint: the capture
            // still archives; only the transcript is withheld, loudly.
            tracing::warn!(error = %e, "transcript could not be sealed; archiving without it");
            None
        }
    }
}

/// Name anything the repository's redactor packs could not be used for.
///
/// A pack that failed to load costs its own rule and nothing else — the
/// built-ins are always present — so this is a note, never an error. It goes to
/// stderr where a terminal exists (the pre-push path) and is swallowed where one
/// does not (the detached worker), which is why the loader returns the notes
/// rather than printing them itself.
fn report_pack_notes(notes: &[String]) {
    for note in notes {
        tracing::warn!(note = %note, "redactor pack");
        eprintln!("rift: {note}");
    }
}

/// How many pack notes get their own marker. A repository may hold arbitrarily
/// many packs; the cost of noticing them is bounded here rather than left to
/// the repository to decide, because it is paid on every commit.
const MAX_PACK_NOTE_MARKERS: usize = 4;

/// Record pack notes where the next sweep will **say** them, for the paths that
/// cannot say them themselves.
///
/// [`report_pack_notes`] prints, which is right where a terminal exists (the
/// pre-push leg) and useless where one does not. Neither of the two paths a
/// commit actually goes through has one: `prepare-commit-msg` discarded these
/// notes outright, and the detached worker's stderr is `/dev/null`. So a
/// mistyped pack regex was silent for exactly the commit cycle it degraded
/// — the developer's coverage quietly fell back to the built-ins with
/// nothing said anywhere, which a review of this path caught.
///
/// The marker is the queue's existing said-once channel, which also means
/// `rift doctor` reports it. Never blocks git: every write is best-effort, and
/// a pack note is a note — the built-ins are always present, so the worst case
/// a broken pack can produce is the coverage the repository already had.
pub(super) fn record_pack_notes(git_dir: &Path, notes: &[String]) {
    for (i, note) in notes.iter().take(MAX_PACK_NOTE_MARKERS).enumerate() {
        crate::capture_queue::mark_note(git_dir, &format!("redactor-pack-{i}"), note);
    }
    if let Some(extra) = notes
        .len()
        .checked_sub(MAX_PACK_NOTE_MARKERS)
        .filter(|n| *n > 0)
    {
        crate::capture_queue::mark_note(
            git_dir,
            "redactor-pack-more",
            &format!("and {extra} more redactor pack note(s)"),
        );
    }
}
