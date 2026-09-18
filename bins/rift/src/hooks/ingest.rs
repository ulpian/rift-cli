//! Ingesting one agent hook into the per-repo projection: the edit record,
//! worktree observation, the enrichment a payload gets before capture, the
//! work a hook leaves owed to the worker, and the post-ingest drive. Separate
//! because this is the capture side of the agent hook, after the gate has
//! answered.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use rift_core::Result;
use rift_shadow::bridge::ingest_hook;
use rift_shadow::hooks::{build_raw_hook, AdapterRegistry};

use super::agent::arm_agent_replacement;
use super::archive::archive_now;
use super::doc_gate::DocAnswer;
use super::gate::{
    gate_file, gate_rel_path, gate_step, gate_turn, payload_cwd, read_ledger_step, GateStep,
};
use super::git::{effective_config, spawn_capture_worker, strategy_kind, CAPTURE_SYNC_ENV};
use super::worker::{drain_capture_queue, owe_session_start_passes};
use super::{capture_repo, current_dir_or_dot, now_ms};

/// What this hook does about a change to a file, if anything.
///
/// The two moments are one mechanism. Before a whole-file write lands, the
/// hash of the bytes the payload carries is written down; after it lands, the
/// file is hashed and the two are compared. That equality is what separates
/// "the agent wrote these bytes" from "something reformatted them on the way
/// in", and it is the only reason a window that never read a file may be said
/// to hold it.
enum EditStep {
    /// A whole-file write is about to happen: record the hash its own bytes
    /// have, so that afterwards there is something to check against.
    Expect,
    /// A change has happened: hash the file and record what follows.
    Record,
}

/// Which moment of an edit a `(family, verb)` is, if it is one.
///
/// `PostToolUse` is the *successful* completion of a tool call on every family
/// whose event list this tree pins — a call that failed announces itself under
/// its own name and never reaches here — so a change is recorded only where
/// one really happened. A payload that names no edit at all is the adapter's
/// answer, not this function's.
fn edit_step(verb: &str) -> Option<EditStep> {
    match verb {
        "PreToolUse" => Some(EditStep::Expect),
        "PostToolUse" => Some(EditStep::Record),
        _ => None,
    }
}

/// The edit record itself, guarded and timed by the step that calls it.
///
/// It opens no store, hashes at most the policy's inline cap, and every
/// refusal below leaves the ledger knowing less than really happened — which
/// costs a saving and can never claim bytes a window does not hold.
pub(super) fn edit_record(
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) {
    // The same lever, so the same switch: a repository that turned the read
    // ledger off keeps no ledger, and what a window wrote is a line of one.
    if effective.read_deny == rift_recall_policy::Mode::Off || raw.session.is_empty() {
        return;
    }
    let Some(adapter) = registry.get(&raw.agent) else {
        return;
    };
    let Some(step) = edit_step(&raw.verb) else {
        return;
    };
    let Some(request) = adapter.edit_request(&raw.verb, &raw.payload) else {
        return;
    };
    let facts = adapter.facts(&raw.verb, &raw.payload);
    let cwd = payload_cwd(&facts);
    let window = facts.child_session;
    let ledger = crate::read_ledger::ReadLedger::open(work_tree, &raw.session, window.as_deref());
    let turn = gate_turn(raw, git_dir);
    let at_ms = u64::try_from(raw.occurred_at_ms).unwrap_or_default();
    if matches!(step, EditStep::Expect) {
        // Only a whole-file write has an expectation to record: it is the one
        // payload that carries the file it is about to make.
        let rift_agent::EditKind::Write { content_sha256 } = request.kind else {
            return;
        };
        let Some(root) = work_tree.canonicalize().ok() else {
            return;
        };
        let Some(path) = gate_rel_path(&root, &request.path, cwd.as_deref()) else {
            return;
        };
        let _ = ledger.append(&rift_agent::read_ledger::Line::Expect {
            turn,
            path,
            hash: content_sha256,
            at_ms,
        });
        return;
    }
    // The bytes the change left behind: this checkout's own spelling of the
    // path, the revision it is at now, and how long it is. A file this hook
    // cannot stand behind — outside the checkout, past the inline cap, not
    // text — leaves no line at all.
    let Some(file) = gate_file(
        work_tree,
        &request.path,
        cwd.as_deref(),
        effective.max_inline_hash_bytes,
    ) else {
        return;
    };
    let held = ledger.load().ledger;
    let prior = crate::read_ledger::prior_revision(&held, &file.rel_path);
    let facts = crate::read_ledger::edit_facts(
        &request.kind,
        file.text(),
        file.total_lines,
        file.hash,
        crate::read_ledger::expected_write(&held, &file.rel_path),
        prior,
    );
    if facts.is_silent() && prior.is_some_and(|p| p.hash == file.hash) {
        // Nothing about the file changed and nothing was learned: a call that
        // did not land leaves the ledger exactly as it found it.
        return;
    }
    let _ = ledger.append(&rift_agent::read_ledger::Line::Edit {
        turn,
        path: file.rel_path,
        hash_old: prior.map(|p| p.hash),
        hash_new: file.hash,
        sites: facts.sites,
        shift: facts.shift,
        full: facts.full,
        at_ms,
    });
}

/// Best-effort agent-hook ingest; any error is the caller's to swallow.
///
/// Phase-timed under `RIFT_HOOK_TIMING=1` (see [`crate::hooktime`]) so the
/// agent-hook budget can be split the way the git-hook budget
/// already is: the report line names where an agent's hook spent its
/// milliseconds, from the shipped binary, and prints nothing when unset.
pub(super) fn try_ingest_agent_hook(
    agent: &str,
    verb: &str,
    body: &[u8],
    decline: &mut Option<String>,
    document: &mut Option<DocAnswer>,
    search: &mut Option<crate::search_gate::SearchAnswer>,
) -> Result<()> {
    let t = crate::hooktime::HookTimer::from_env("agent-hook");
    let verb = verb.to_owned();
    // Build the hook **totally** from the untrusted stdin envelope (bounded, no
    // panic). A malformed/oversized payload or agent token defers (no write).
    let Ok(mut raw) = t.phase("build_raw", || build_raw_hook(agent, &verb, body, now_ms())) else {
        return Ok(()); // malformed/oversized/agent token → deferred
    };
    t.phase("enrich", || {
        enrich_native_context(&mut raw);
        enrich_antigravity_model(&mut raw);
    });
    let raw = raw;

    // Resolve the repository the way git resolves it — including a LINKED
    // worktree, whose `.git` is a file, not a directory (see [`capture_repo`]).
    // Outside any repository this hook records NOTHING and still exits 0: no
    // session cache, no projection, no `.git` minted under whatever directory
    // the agent's shell happened to be in. The timer keeps its
    // line, so an agent hook that captured nothing is still visible to the
    // instrument that measures the budget rather than silently absent.
    let Some(repo) = t.phase("discover", || capture_repo(&current_dir_or_dot())) else {
        t.phase("outside_repository", || ());
        return Ok(());
    };
    let (git_dir, work_tree) = (repo.git_dir.clone(), repo.work_tree.clone());
    // WHAT THIS CONTEXT WINDOW IS BEING HANDED AND WHAT IT HAS WRITTEN, and
    // the one registry the rest of this hook maps its payload through.
    let (registry, gate_journaled) =
        read_ledger_step(&t, &raw, &git_dir, &work_tree, decline, document, search);
    arm_agent_replacement(&work_tree, &raw, decline.as_deref());
    // One identity for the whole repository family: a hook
    // fired in a linked worktree lands in the projection its main work tree
    // already owns, and the rows it writes carry the family id — not this
    // checkout's path hash, which would file them where no family-scoped reader
    // looks. Resolved ONCE here; the injector below reads the same memoized
    // record, so the whole hook pays for one manifest read.
    //
    // `None` is an identity that cannot be proven from this vantage (an
    // unminted family whose main work tree this checkout cannot reach, or a
    // manifest this reader will not guess at). The hook records nothing and
    // still exits 0, and the timer keeps its line so the budget instrument can
    // still see the hook that captured nothing.
    let Some(family) = t.phase("family", || crate::family::try_at(&work_tree)) else {
        t.phase("no_family_identity", || ());
        return Ok(());
    };
    let store = t.phase("open_store", || {
        rift_store::open_capture_projection(rift_store::projection_db_for_family(&family))
    })?;
    let repo_id = t.phase("repo_id", || family.family_id.to_string());

    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let read_hasher = worktree_read_hasher(&work_tree);
    let outcome = t.phase("ingest", || {
        rt.block_on(ingest_hook(
            &raw,
            &registry,
            &store,
            &git_dir,
            agent,
            &repo_id,
            now_ms(),
            Some(&read_hasher),
        ))
    })?;
    let owed = owe_deferred_measurement(&t, &git_dir, &work_tree, &registry, &raw, &outcome);
    let sweep_owed = owe_session_start_passes(&t, &raw, &git_dir, &work_tree);
    // Bind any newly-appeared sessions to the local developer identity
    // (best-effort: identity failure must never fail the agent's hook).
    // A hook that named no session created none, so there is nothing to
    // bind — and binding spawns child processes, which a hook that
    // captured nothing must not pay for.
    if outcome.receipt != rift_shadow::HookReceipt::DeferredNoSession {
        bind_hook_identity(&t, &rt, &store, &work_tree);
    }
    // The receipt is the whole of a session-less ingest; every step below
    // addresses a session by id, and this hook has none.
    if outcome.receipt == rift_shadow::HookReceipt::DeferredNoSession {
        return Ok(());
    }
    // Every post-ingest step is addressed by the session's *owner*,
    // not by the agent that fired this hook: when two agents drive one session
    // id, rows written under the firing agent would belong to a session row
    // that does not exist.
    let owner = outcome.owner_agent.as_str();
    // WHICH WORKTREE THIS HOOK WAS FIRED IN.
    observe_hook_worktree(&t, &rt, &store, &repo, &registry, &raw, owner);
    // The deferred settle: the turn that
    // has CLOSED by now — never the one this hook closed, whose transcript the
    // agent is still flushing — gets its provider-reported spend recorded at
    // its own Stop event's ids, read forward from the `.usage` sidecar's
    // offset. This replaces the 500 ms inline wait a Stop used to pay.
    // Best-effort — never fails the agent's hook.
    let _ = t.phase("settle_usage", || {
        rt.block_on(rift_shadow::bridge::settle_closed_turn_usage(
            &store,
            &git_dir,
            &raw.session,
            owner,
            &repo_id,
            rift_shadow::bridge::SettleMode::AfterHook {
                turn_closed_here: outcome
                    .boundaries
                    .iter()
                    .any(|(_, b)| *b == rift_shadow::Boundary::End),
            },
            now_ms(),
        ))
    });
    // Turn-boundary working-tree snapshots: close the H_i/A_i windows
    // this hook implied. Best-effort — never fails the agent's hook.
    let _ = t.phase("snapshot_boundaries", || {
        snapshot_boundaries(
            &rt,
            &store,
            &work_tree,
            owner,
            &raw.session,
            &outcome.boundaries,
        )
    });
    // Post-ingest strategy drivers: the session-end condense effect
    // (journaled for the detached worker) and the scheduled interval.
    // Best-effort — never fails the agent's hook.
    let condense_queued = t.phase("post_ingest", || {
        drive_post_ingest(
            &rt,
            &store,
            &git_dir,
            &repo_id,
            &raw.session,
            owner,
            outcome.state,
            &outcome.effects,
        )
    });
    // One worker for everything this hook journaled: the condense (if the
    // session ended) and the token counts. A condense that was enqueued
    // already dispatched; otherwise the owed counts dispatch here.
    if (owed > 0 || gate_journaled || sweep_owed) && !matches!(condense_queued, Ok(true)) {
        t.phase("spawn_worker", || dispatch_agent_worker(&git_dir));
    }
    Ok(())
}

fn bind_hook_identity(
    t: &crate::hooktime::HookTimer,
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    work_tree: &Path,
) {
    let _ = t.phase("bind_identity", || {
        rt.block_on(crate::identity::bind_new_sessions(
            store,
            work_tree,
            crate::identity::install_salt,
            now_ms(),
        ))
    });
}

/// The worktree-provenance step of one agent hook: read the
/// payload's own facts through the adapter that mapped this hook, and record
/// the observation under the session's OWNER — the label every other
/// post-ingest step is addressed by.
///
/// Best-effort and infallible by signature: a provenance label is never a
/// reason to fail an agent's hook, and the timer keeps its phase either way so
/// the budget instrument sees what this step cost.
fn observe_hook_worktree(
    t: &crate::hooktime::HookTimer,
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    owner: &str,
) {
    let _ = t.phase("observe_worktree", || {
        let facts = registry
            .get(&raw.agent)
            .map(|adapter| adapter.facts(&raw.verb, &raw.payload))
            .unwrap_or_default();
        observe_worktree(
            rt,
            store,
            repo,
            facts.cwd.as_ref(),
            &raw.session,
            &rift_shadow::ids::session_row_id(owner, &raw.session),
            facts.child_session.as_deref(),
            now_ms(),
        )
    });
}

/// Record the `session_worktree` observation this hook implies, when it is a
/// state the session has not been seen in before.
///
/// **Content-addressed over the state, never the time**, so a redelivered hook,
/// a re-run and a rebuild all derive the same `observation_id` and the insert
/// collapses; a session that MOVES — a worktree-isolated subagent is the case
/// this exists for — gets a second row, and the ordered set is the history an
/// auditor reads.
///
/// **Two layers of idempotence, on purpose.** The database layer is `INSERT OR
/// IGNORE` on the state's own id and is what makes the record correct. The
/// session cache's `last_worktree` is a *memo* in front of it and is what makes
/// it cheap: the overwhelmingly common hook observes exactly what the previous
/// hook observed, and on that path this function reads three small files and
/// touches no database at all. A lost, stale or absent memo costs one redundant
/// no-op insert and never a row — which is why the memo is written only AFTER
/// the row lands, and why a session with no cache file still records.
///
/// Returns whether a row was written (the tests' oracle; the caller ignores it).
pub(super) fn observe_worktree(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    cwd: Option<&rift_agent::CwdFact>,
    native_session: &str,
    session_row_id: &str,
    subagent_id: Option<&str>,
    now: i64,
) -> Result<bool> {
    use rift_shadow::hooks::session_cache;

    let (observed, basis) = observed_worktree(repo, cwd);
    let head = rift_git::head_state(&observed);
    let seen = rift_shadow::hooks::WorktreeObservation {
        token: observed.worktree_token(),
        name: observed.member_name(),
        linked: observed.is_linked(),
        branch: head.branch,
        head: head.oid,
        subagent: subagent_id.map(str::to_owned),
    };

    // The memo lives beside the session's own state, under the gitdir the
    // ingest just wrote the cache to — never under the OBSERVED worktree's,
    // which may be a different member of the same family.
    let cached = session_cache::read_cache(&repo.git_dir, native_session)
        .ok()
        .flatten();
    if cached
        .as_ref()
        .is_some_and(|c| c.last_worktree.as_ref() == Some(&seen))
    {
        return Ok(false);
    }

    let row = rift_store::SessionWorktreeRow {
        observation_id: rift_shadow::ids::session_worktree_observation_id(
            session_row_id,
            &seen.token,
            seen.branch.as_deref(),
            seen.head.as_deref(),
            seen.subagent.as_deref(),
        ),
        session_id: session_row_id.to_owned(),
        worktree_token: seen.token.clone(),
        worktree_name: seen.name.clone(),
        is_linked: i64::from(seen.linked),
        // A LOCAL ADDRESS, canonicalized so the same checkout observed through
        // `/tmp/x` and `/private/tmp/x` is one path. This column is why the
        // relation is projection-only: it never enters a shard.
        work_tree: canonical_display(&observed.work_tree),
        branch: seen.branch.clone(),
        head_oid: seen.head.clone(),
        subagent_id: seen.subagent.clone(),
        basis: basis.to_owned(),
        first_observed_at: now,
        schema_version: 1,
    };
    let wrote = rt.block_on(rift_store::record_session_worktree(store, &row))?;

    // Memoize only what landed. A cache that does not exist is not invented
    // here: the ingest owns that file's existence, and a session it declined to
    // cache is not one this path should conjure state for.
    if let Some(mut cache) = cached {
        cache.last_worktree = Some(seen);
        session_cache::write_cache(&repo.git_dir, &cache)?;
    }
    Ok(wrote)
}

/// Which checkout the observation names, and the `basis` token that says how it
/// was decided (`rift_store::schema::SESSION_WORKTREE_BASES`).
///
/// The payload's directory wins when the adapter carries one — it is the agent
/// speaking about itself, while the hook process's cwd is wherever the shell
/// that spawned it happened to be — but only when it resolves to a checkout of
/// **this same family**. A payload naming a directory in another repository (or
/// none at all) falls back to the discovered capture repo with `process_cwd`,
/// because filing one family's session under another family's worktree is the
/// misattributed credit the provenance rules forbid, and a basis token that
/// lied about where the value
/// came from would hide it.
fn observed_worktree(
    repo: &rift_git::RepoPaths,
    cwd: Option<&rift_agent::CwdFact>,
) -> (rift_git::RepoPaths, &'static str) {
    let claimed = cwd
        .and_then(|fact| {
            rift_git::discover(Path::new(&fact.path))
                .ok()
                .map(|found| (found, fact.source.as_str()))
        })
        .filter(|(found, _)| {
            canonical_display(&found.common_dir) == canonical_display(&repo.common_dir)
        });
    claimed.unwrap_or_else(|| (repo.clone(), "process_cwd"))
}

/// A path as an address: canonical where the filesystem can say so, verbatim
/// where it cannot (a checkout removed between the hook and this read).
fn canonical_display(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

/// Journal a token-count job for every read fact this hook recorded without
/// a count. Returns how many were journaled.
fn owe_read_tokens(git_dir: &Path, work_tree: &Path, reads: &[rift_shadow::RecordedRead]) -> usize {
    let now = now_ms();
    reads
        .iter()
        .filter(|r| !r.tokens_recorded)
        .filter(|r| {
            crate::agent_queue::enqueue(
                git_dir,
                &crate::agent_queue::AgentJob {
                    schema_version: 1,
                    work_tree: work_tree.to_path_buf(),
                    queued_at_ms: now,
                    attempts: 0,
                    kind: crate::agent_queue::JobKind::ReadTokens {
                        tool_call_id: r.tool_call_id.clone(),
                        rel_path: r.rel_path.clone(),
                        content_sha256: r.content_sha256.clone(),
                        blob_oid: r.blob_oid.clone(),
                        lines: r.lines,
                        schema_version: r.schema_version,
                    },
                },
            )
            .is_ok()
        })
        .count()
}

/// Journal a snapshot job for every revision this hook's read facts named.
/// Returns how many were journaled.
///
/// Every recorded read, not only the unmeasured ones: the count a read owes
/// and the bytes a *later* read will be diffed against are different debts,
/// and a revision whose tokens were counted inline is exactly as likely to be
/// the one an agent still holds. The journal keys by the revision, so a file
/// read ten times in a turn owes one snapshot.
pub(super) fn owe_basis_snapshots(
    git_dir: &Path,
    work_tree: &Path,
    reads: &[rift_shadow::RecordedRead],
) -> usize {
    let now = now_ms();
    let mut seen = std::collections::BTreeSet::new();
    reads
        .iter()
        .filter(|r| seen.insert(r.content_sha256.clone()))
        .filter(|r| {
            crate::agent_queue::enqueue(
                git_dir,
                &crate::agent_queue::AgentJob {
                    schema_version: 1,
                    work_tree: work_tree.to_path_buf(),
                    queued_at_ms: now,
                    attempts: 0,
                    kind: crate::agent_queue::JobKind::BasisSnapshot {
                        rel_path: r.rel_path.clone(),
                        content_sha256: r.content_sha256.clone(),
                        blob_oid: r.blob_oid.clone(),
                    },
                },
            )
            .is_ok()
        })
        .count()
}

/// Everything this hook's ingest owes a later, off-path writer, journaled in
/// one place and counted as one number.
///
/// Three debts, all of them identifiers only and all of them drained by the
/// same worker. **The token count** of each read fact, which needs the
/// tokenizer. **The bytes** of each revision a read recorded, so the delta
/// against it survives the edit that follows — which needs the redaction gate.
/// And **the measurement of a search**, which needs a process. Not one of them
/// fits inside a hook, and every one of them is best-effort: a journal that
/// cannot be written leaves a figure unmeasured, never the hook failed.
fn owe_deferred_measurement(
    t: &crate::hooktime::HookTimer,
    git_dir: &Path,
    work_tree: &Path,
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    outcome: &rift_shadow::IngestOutcome,
) -> usize {
    let reads = t.phase("owe_tokens", || {
        owe_read_tokens(git_dir, work_tree, &outcome.read_calls)
    });
    let bases = t.phase("owe_basis", || {
        owe_basis_snapshots(git_dir, work_tree, &outcome.read_calls)
    });
    let results = t.phase("owe_result_tokens", || {
        owe_tool_result_tokens(
            git_dir,
            work_tree,
            registry,
            raw,
            outcome.owner_agent.as_str(),
        )
    });
    reads.saturating_add(bases).saturating_add(results)
}

/// Journal the measurement one **search result** owes, and say whether the
/// worker was left anything.
///
/// The denominator's other half. A read resolves to a file and is measured
/// through `file_read_tokens`; a search resolves to no file, has no key there
/// and has never been measured at all — while returning repository content on
/// every call. This is the row that makes the pool the whole pool.
///
/// The hook does none of the work: it classifies the call from the arguments
/// the family's pinned row names, reads the tree key out of the saved index's
/// header for one small read, and writes a file. The re-execution, the
/// tokenizer and the projection all belong to the worker.
///
/// Two calls are deliberately **not** measured here. A shell read — `cat`,
/// `sed -n`, a `grep` over one named path — is already in the read
/// denominator through its own read fact, and counting it again would double
/// it; the recogniser is asked, and a command line it reads as a read is left
/// alone. And a command whose head word is not an allow-listed verb is not a
/// search Rift can see at all, so nothing is claimed about it.
fn owe_tool_result_tokens(
    git_dir: &Path,
    work_tree: &Path,
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    owner_agent: &str,
) -> usize {
    if !matches!(
        gate_step(&raw.agent, &raw.verb, &raw.payload),
        Some(GateStep::CrossCheck)
    ) {
        return 0;
    }
    let Some(adapter) = registry.get(&raw.agent) else {
        return 0;
    };
    let facts = adapter.facts(&raw.verb, &raw.payload);
    let Some(tool) = facts.tool_name.as_deref() else {
        return 0;
    };
    let Some(row) = rift_agent::tool_fields::tool_field_row(&raw.agent, tool) else {
        return 0;
    };
    let Some(args) = search_result_args(row, &raw.payload) else {
        return 0;
    };
    let (channel, search) = match row.shape {
        rift_agent::tool_fields::ToolShape::Search => (
            "search",
            crate::search_measure::classify_tool(
                row.fact("kind").unwrap_or_default(),
                args,
                row.fact("pattern").unwrap_or("pattern"),
                row.fact("dir").unwrap_or("path"),
            ),
        ),
        rift_agent::tool_fields::ToolShape::Shell => {
            let command = args
                .get(row.fact("command").unwrap_or("command"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            // A command line the read grammar reads as a READ is already in
            // the read denominator; measuring it here would count it twice.
            if matches!(
                rift_agent::shell_read::recognise(command),
                Some(rift_agent::shell_read::ShellCall::Read(_))
            ) {
                return 0;
            }
            ("search", crate::search_measure::classify_command(command))
        }
        _ => return 0,
    };
    let Some(search) = search else { return 0 };
    let payload_sha256 = rift_shadow::sha256_hex(&[serde_json::Value::Object(args.clone())
        .to_string()
        .as_bytes()]);
    let session_row = rift_shadow::ids::session_row_id(owner_agent, &raw.session);
    let job = crate::agent_queue::AgentJob {
        schema_version: 1,
        work_tree: work_tree.to_path_buf(),
        queued_at_ms: raw.occurred_at_ms,
        attempts: 0,
        kind: crate::agent_queue::JobKind::ToolResultTokens {
            tool_result_id: rift_shadow::ids::tool_result_row_id(
                &session_row,
                gate_turn(raw, git_dir),
                tool,
                &payload_sha256,
            ),
            session_id: session_row,
            tool_name: tool.to_owned(),
            channel: channel.to_owned(),
            search,
            tree_key: saved_tree_key(work_tree),
        },
    };
    usize::from(crate::agent_queue::enqueue(git_dir, &job).is_ok())
}

/// The arguments object a family's pinned row says a call carries them in.
///
/// Read off the row rather than known here, exactly as the search gate reads
/// it: two families put them flat under the envelope field beside `tool_name`
/// and one nests them, and the row is where that fact is already pinned to an
/// artifact.
fn search_result_args(
    row: rift_agent::tool_fields::ToolFieldRow,
    payload: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    let mut node = payload;
    for segment in row.fact("args").unwrap_or("tool_input").split('.') {
        node = node.get(segment)?;
    }
    node.as_object()
}

/// The tree state this member's path index was last taken at, read from the
/// saved index's header alone.
///
/// One small read and no walk: a hook may not stat a tree's skeleton, and the
/// index's own key is the closest honest answer to "what state was this
/// checkout in when the search ran". `None` where the index is cold, which
/// leaves the worker with its own bracket and nothing earlier to compare.
#[cfg(feature = "search")]
fn saved_tree_key(work_tree: &Path) -> Option<crate::agent_queue::TreeKey> {
    rift_recall_search::PathIndex::saved_key(&rift_recall_search::index_path(work_tree))
        .as_ref()
        .map(crate::agent_queue::TreeKey::of)
}

/// A build with no search seam keeps no index and therefore no key.
#[cfg(not(feature = "search"))]
fn saved_tree_key(_work_tree: &Path) -> Option<crate::agent_queue::TreeKey> {
    None
}

/// Hand the journaled agent-hook jobs to the detached worker — or, under
/// [`CAPTURE_SYNC_ENV`], drain them here before returning, for the caller that
/// must observe the result (an end-to-end test, a CI run with nowhere to put a
/// background child). Same variable, same two arms, same code either way as
/// the post-commit path: it chooses **who** drains and **when**, never
/// **whether**.
fn dispatch_agent_worker(git_dir: &Path) {
    if std::env::var_os(CAPTURE_SYNC_ENV).is_some() {
        let _ = drain_capture_queue(git_dir.to_str());
    } else {
        spawn_capture_worker(git_dir);
    }
}

/// The worktree read-hasher: resolves a read-shaped tool call's
/// payload path (absolute or repo-relative) to a repo-relative path plus the
/// current bytes' identity, using the SAME hashing the recall plane's
/// blob-oid map uses. A path outside the repo, vanished, or unsafe resolves
/// to no fact — capture never guesses.
pub(super) fn worktree_read_hasher(
    work_tree: &Path,
) -> impl Fn(&str) -> Option<rift_shadow::ReadContent> {
    let hash_root = Some(
        work_tree
            .canonicalize()
            .unwrap_or_else(|_| work_tree.to_path_buf()),
    );
    move |raw_path: &str| -> Option<rift_shadow::ReadContent> {
        let wt = hash_root.as_ref()?;
        let p = Path::new(raw_path);
        // Canonicalize an absolute payload path so symlinked prefixes (macOS
        // `/var` -> `/private/var`) still resolve under the worktree root.
        // Spelled repository-relative with `/` on every platform: this string
        // becomes `ReadContent.rel_path` and part of the content-addressed
        // `tool_call_id`, and every lookup is keyed with `/`.
        let rel_norm = if p.is_absolute() {
            let canon = p.canonicalize().ok()?;
            rift_recall_substrate::worktree::repo_relative_slash(wt, &canon)?
        } else {
            rift_recall_substrate::worktree::repo_relative_slash(Path::new(""), p)?
        };
        let (blob, bytes) = rift_recall_substrate::worktree::hash_blob_bytes(wt, &rel_norm)?;
        // The token measurement is NOT taken here any more, at any
        // size. It used to be, up to 1 MiB, and `hook-read-tool` measured
        // p50 102.39 ms against a 100 ms budget on a 120-line file — because
        // the cost is not the count but the tokenizer: `cl100k_base`
        // initialises in 234 ms (release, this machine) before the first
        // byte, then 7 ms for 5 KiB, 89 ms for 64 KiB, 1.38 s for 1 MiB. No
        // inline cap keeps a hook inside the budget when the first count
        // alone is over it, so the cap is zero and the count is owed to the
        // detached worker (`count_read_tokens_job`), which appends the same
        // `file_read_tokens` row at the same read identity. Only real BPE
        // figures persist ("measured or absent"): the chars/4 fallback never
        // lands a durable number, here or there.
        let _ = bytes;
        Some(rift_shadow::ReadContent {
            rel_path: blob.path,
            blob_oid: blob.blob_oid,
            content_sha256: blob.content_sha256,
            tokens: None,
        })
    }
}

/// Antigravity's documented hook payload does not expose the selected model.
/// When its ordinary settings file names one concrete model, attach that fact
/// with explicit configured provenance. `auto`/`default` remain unavailable;
/// private transcript/database formats are never inspected.
fn enrich_antigravity_model(raw: &mut rift_shadow::hooks::RawHook) {
    if raw.agent != rift_core::AgentId::ANTIGRAVITY || raw.payload.get("model").is_some() {
        return;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let candidates = [
        home.join(".gemini/antigravity-cli/settings.json"),
        home.join(".gemini/antigravity/settings.json"),
    ];
    let model = candidates.into_iter().find_map(|path| {
        let bytes = std::fs::read(path).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        value
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|model| {
                !model.is_empty()
                    && !model.eq_ignore_ascii_case("auto")
                    && !model.eq_ignore_ascii_case("default")
            })
            .map(str::to_owned)
    });
    if let (Some(model), Some(payload)) = (model, raw.payload.as_object_mut()) {
        payload.insert(
            "_rift_configured_model".to_owned(),
            serde_json::Value::String(model),
        );
    }
}

/// Native Claude Code hooks carry no usage fields; the transcript named by
/// the payload's `transcript_path` holds the provider-reported numbers. At a
/// mid-turn boundary (`PreToolUse`) read a small bounded tail and inject one
/// call-grain **context** snapshot (`context_usage` + `model`) — the Canopy
/// nic-gauge's pressure reading. A turn's **spend** is deliberately NOT read
/// here any more: a `Stop` fires while the agent is still flushing the
/// transcript, and the inline settle that made that safe cost the hook the
/// whole 500 ms quiet window. Spend is settled by the next hook for the
/// session from the `.usage` sidecar's offset instead
/// (`rift_shadow::bridge::settle_closed_turn_usage`), at the same ids.
/// Best-effort and honest: a missing, unreadable, or usage-free transcript
/// leaves the fields absent, never fabricated.
pub(super) fn enrich_native_context(raw: &mut rift_shadow::hooks::RawHook) {
    use rift_shadow::hooks::transcript::{latest_main_context, CONTEXT_TAIL_BYTES};
    use rift_shadow::hooks::AgentId;

    if raw.agent != AgentId::CLAUDE_CODE || raw.verb != "PreToolUse" {
        return;
    }
    let Some(obj) = raw.payload.as_object_mut() else {
        return;
    };
    let Some(path) = obj
        .get("transcript_path")
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    let Some(tail) = read_tail(Path::new(path), CONTEXT_TAIL_BYTES) else {
        return;
    };
    let Some(context) = latest_main_context(&tail) else {
        return;
    };
    if !obj.contains_key("context_usage") {
        let mut usage = serde_json::Map::new();
        for (key, count) in [
            ("input_tokens", context.usage.input),
            ("output_tokens", context.usage.output),
            ("cache_read_input_tokens", context.usage.cache_read),
            ("cache_creation_input_tokens", context.usage.cache_write),
        ] {
            if let Some(n) = count {
                usage.insert(key.to_owned(), n.into());
            }
        }
        obj.insert("context_usage".to_owned(), usage.into());
    }
    if let Some(model) = context.model {
        obj.entry("model".to_owned())
            .or_insert_with(|| model.into());
    }
}

/// Read at most `cap` bytes from the end of `path`. `None` on any I/O failure.
fn read_tail(path: &Path, cap: u64) -> Option<Vec<u8>> {
    use std::io::{Seek as _, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len > cap {
        f.seek(SeekFrom::Start(len.saturating_sub(cap))).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// Capture the working-tree snapshot for each boundary this hook closed.
/// Takes the RESOLVED work tree (never the process cwd: a hook's cwd is not
/// guaranteed to be the repository). A failure is **said once** rather than
/// swallowed (cause E of `turn_boundary`'s register, 2026-08-29): it lands
/// on the capture queue's note channel under `boundary-<sha16(session)>`, so
/// re-noticing overwrites instead of accumulating, and `rift doctor` reports
/// it without consuming it. Never blocks — the caller ignores the `Err`.
fn snapshot_boundaries(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    work_tree: &Path,
    agent: &str,
    session: &str,
    boundaries: &[(u32, rift_shadow::Boundary)],
) -> Result<()> {
    if boundaries.is_empty() {
        return Ok(());
    }
    let repo = rift_git::discover(work_tree)?;
    let result = (|| -> Result<()> {
        let reader = rift_git::GixBlobReader::open(&repo.work_tree)?;
        let head = reader.head_hex()?;
        for (ordinal, boundary) in boundaries {
            rt.block_on(rift_shadow::capture_boundary(
                store,
                &repo,
                &reader,
                head.as_deref(),
                agent,
                session,
                *ordinal,
                *boundary,
                now_ms(),
            ))?;
        }
        Ok(())
    })();
    if let Err(e) = &result {
        let digest = rift_shadow::sha256_hex(&[session.as_bytes()]);
        let key = format!("boundary-{}", digest.get(..16).unwrap_or_default());
        crate::capture_queue::mark_note(
            &repo.git_dir,
            &key,
            &format!(
                "turn-boundary snapshot failed; the session's H_i/A_i windows are incomplete ({e})"
            ),
        );
    }
    result
}

/// Run the archive work an agent hook may have triggered: the state machine's
/// `RequestCondense` effect (session end — journaled for the detached worker),
/// and the `scheduled` strategy's interval check. A missing repo/HEAD/backend
/// defers silently. Returns whether a condense was journaled — its worker is
/// then already dispatched, so the caller need not spawn a second.
#[allow(clippy::too_many_arguments)]
fn drive_post_ingest(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    git_dir: &Path,
    repo_id: &str,
    session: &str,
    agent: &str,
    state: rift_shadow::hooks::SessionState,
    effects: &[rift_shadow::hooks::Effect],
) -> Result<bool> {
    use rift_shadow::hooks::strategy::{decide, TriggerCtx};
    use rift_shadow::hooks::Effect;

    if effects.contains(&Effect::RequestCondense) {
        // The session ended: the state machine's explicit condense request.
        // The condense itself — redact, shard, chain append, then Ending →
        // Ended — measured p50 647.80 ms with a transcript against a 100 ms
        // agent-hook budget, so it is JOURNALED here and performed by the
        // detached worker (`condense_agent_job`), exactly as `post-commit`
        // defers its own. The job is durable before the spawn: a worker that
        // never starts leaves it for the next worker or `rift doctor`.
        //
        // Everything the old inline branch said about closure still holds
        // in the worker: `ended` is claimed only by a checkpoint that exists,
        // and a skipped or failed archive leaves the session `ending` for
        // `sweep_stale_sessions` to retry (settled 2026-08-07).
        let cwd = std::env::current_dir()?;
        crate::agent_queue::enqueue(
            git_dir,
            &crate::agent_queue::AgentJob {
                schema_version: 1,
                work_tree: rift_git::discover(&cwd)?.work_tree,
                queued_at_ms: now_ms(),
                attempts: 0,
                kind: crate::agent_queue::JobKind::Condense {
                    session_id: session.to_owned(),
                    agent: agent.to_owned(),
                    repo_id: repo_id.to_owned(),
                },
            },
        )?;
        dispatch_agent_worker(git_dir);
        return Ok(true);
    }

    let cwd = std::env::current_dir()?;
    let repo = rift_git::discover(&cwd)?;
    let reader = rift_git::GixBlobReader::open(&repo.work_tree)?;
    let Some(commit) = reader.head_hex()? else {
        return Ok(false);
    };
    let cfg = effective_config(&repo.work_tree);
    let base = strategy_kind(cfg.checkpoint_strategy());
    let now = now_ms();

    // The scheduled strategy fires from agent activity too — a long session
    // without commits still checkpoints on its interval.
    let last = rt.block_on(rift_store::latest_archive_ms(store, session))?;
    let decision = decide(
        base,
        &TriggerCtx {
            git_event: None,
            session_state: state,
            sensitive_path_touched: false,
            clock_ms: now,
            last_archive_ms: last,
            schedule_interval_ms: cfg.schedule_interval_ms(),
        },
    );
    if decision.archive {
        // A deferred interval archive changes no lifecycle: the session stays
        // live and the next activity asks again.
        let _checkpoint = archive_now(
            rt,
            store,
            &repo.work_tree,
            &commit,
            session,
            agent,
            decision,
            now,
        )?;
    }
    Ok(false)
}

/// What a change to a file leaves behind: the hash it promised, the hash it
/// has, and whether the window that made it holds the result.
#[cfg(test)]
mod edit_record_tests {
    use rift_agent::read_ledger::{Interval, Line};
    use rift_shadow::hooks::RawHook;

    use super::*;
    use crate::hooks::gate::read_gate;

    /// A hook as the record reads one.
    fn hook(agent: &str, verb: &str, session: &str, payload: serde_json::Value) -> RawHook {
        RawHook {
            agent: rift_core::AgentId::new(agent),
            verb: verb.to_owned(),
            session: session.to_owned(),
            turn: Some(7),
            occurred_at_ms: 1_700_000_000_000,
            payload,
        }
    }

    /// A whole-file write, as the pinned fields spell one.
    fn write_of(path: &Path, content: &str) -> serde_json::Value {
        serde_json::json!({
            "tool_name": "Write",
            "tool_input": {
                "file_path": path.display().to_string(),
                "content": content,
            },
        })
    }

    /// A work tree, with nothing in it yet.
    fn checkout() -> tempfile::TempDir {
        tempfile::tempdir().expect("a work tree")
    }

    fn lines_of(work_tree: &Path, session: &str) -> Vec<Line> {
        crate::read_ledger::ReadLedger::open(work_tree, session, None)
            .load()
            .ledger
            .lines()
            .to_vec()
    }

    /// The levers as this repository ships them: observe, with the defaults.
    fn observing() -> rift_recall_policy::Effective {
        rift_recall_policy::resolve(
            &rift_recall_policy::ServePolicy::default(),
            "claude-code",
            crate::recall::family_capability(&rift_core::AgentId::CLAUDE_CODE),
        )
    }

    fn record(raw: &RawHook, work_tree: &Path) {
        edit_record(
            &AdapterRegistry::with_builtin(),
            raw,
            &work_tree.join(".git"),
            work_tree,
            &observing(),
        );
    }

    /// The whole mechanism in one pass: the hash the payload promised is
    /// written down before the write, the file is hashed after it, and the two
    /// agreeing is what says the window holds every line of a file it never
    /// read.
    #[test]
    fn a_write_whose_bytes_survive_grants_coverage_of_every_line() {
        let tmp = checkout();
        let file = tmp.path().join("new.rs");
        let body = "one\ntwo\nthree\n";
        record(
            &hook("claude-code", "PreToolUse", "s-1", write_of(&file, body)),
            tmp.path(),
        );
        std::fs::write(&file, body).expect("the write lands");
        record(
            &hook("claude-code", "PostToolUse", "s-1", write_of(&file, body)),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-1");
        let [Line::Expect { path, .. }, Line::Edit {
            path: edited,
            hash_old,
            sites,
            full,
            ..
        }] = lines.as_slice()
        else {
            panic!("an expectation and the change it was for, got {lines:?}");
        };
        assert_eq!(path, "new.rs", "the checkout's own spelling of a new file");
        assert_eq!(edited, "new.rs");
        assert_eq!(*hash_old, None, "the file did not exist to have a revision");
        assert_eq!(sites.as_slice(), [Interval::new(1, 3).expect("a run")]);
        assert!(
            *full,
            "the agent authored every byte and they are all there"
        );

        let ledger = crate::read_ledger::ReadLedger::open(tmp.path(), "s-1", None)
            .load()
            .ledger;
        let hash = match lines.get(1) {
            Some(Line::Edit { hash_new, .. }) => *hash_new,
            _ => panic!("the change names a revision"),
        };
        assert!(
            ledger
                .coverage("new.rs", hash)
                .contains(Interval::new(1, 3).expect("a run")),
            "writing a file in full is holding it in full"
        );
    }

    /// ★ A formatter that rewrites the bytes on their way to disk breaks the
    /// equality the whole claim rests on, so the window is credited with
    /// nothing — the case that separates a correct mechanism from a plausible
    /// one.
    #[test]
    fn a_write_whose_bytes_a_formatter_changed_grants_no_coverage() {
        let tmp = checkout();
        let file = tmp.path().join("fmt.rs");
        let payload = write_of(&file, "fn  f( ) {}\n");
        record(
            &hook("claude-code", "PreToolUse", "s-2", payload.clone()),
            tmp.path(),
        );
        // What a format-on-save hook left there instead.
        std::fs::write(&file, "fn f() {}\n").expect("the formatter wins");
        record(
            &hook("claude-code", "PostToolUse", "s-2", payload),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-2");
        let [Line::Expect { .. }, Line::Edit {
            hash_new,
            sites,
            full,
            ..
        }] = lines.as_slice()
        else {
            panic!("an expectation and the change it was for, got {lines:?}");
        };
        assert!(sites.is_empty(), "no place to point at");
        assert!(!*full, "the file is not the bytes the payload carried");
        let ledger = crate::read_ledger::ReadLedger::open(tmp.path(), "s-2", None)
            .load()
            .ledger;
        assert!(
            ledger.coverage("fmt.rs", *hash_new).is_empty(),
            "a revision nobody read and nobody wrote is held by nobody"
        );
    }

    /// A replacement, as the pinned fields spell one.
    fn edit_of(path: &Path, old: &str, new: &str, all: bool) -> serde_json::Value {
        serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {
                "file_path": path.display().to_string(),
                "old_string": old,
                "new_string": new,
                "replace_all": all,
            },
        })
    }

    /// A file this window has read every line of, so that what follows an edit
    /// of it is the question of whether it still holds all of it.
    fn read_in_full(work_tree: &Path, session: &str, path: &str, body: &str) {
        let file = work_tree.join(path);
        std::fs::write(&file, body).expect("the file");
        read_gate(
            &AdapterRegistry::with_builtin(),
            &hook(
                "claude-code",
                "PreToolUse",
                session,
                serde_json::json!({
                    "tool_name": "Read",
                    "tool_input": {
                        "file_path": file.display().to_string(),
                        "offset": 1,
                        "limit": 1_000,
                    },
                }),
            ),
            &work_tree.join(".git"),
            work_tree,
            &observing(),
        );
    }

    /// The site is where the text the agent wrote actually sits in the bytes
    /// it left behind, in the numbering a reader of those bytes will see — not
    /// an older line number moved along by an offset.
    #[test]
    fn sites_are_in_new_numbering_with_the_shift() {
        let tmp = checkout();
        read_in_full(tmp.path(), "s-10", "shift.rs", "a\nb\nc\nd\n");
        let file = tmp.path().join("shift.rs");
        std::fs::write(&file, "a\nb\nB2\nB3\nc\nd\n").expect("the edit lands");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-10",
                edit_of(&file, "b\n", "b\nB2\nB3\n", false),
            ),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-10");
        let [_, Line::Edit {
            sites, shift, full, ..
        }] = lines.as_slice()
        else {
            panic!("a read and the change that followed it, got {lines:?}");
        };
        assert_eq!(
            sites.as_slice(),
            [Interval::new(2, 4).expect("a run")],
            "lines 2-4 of the file as it is now, where line 2 was all there was"
        );
        assert_eq!(*shift, 2, "the file is two lines longer than it was");
        assert!(
            *full,
            "it held every line before, and it wrote the difference"
        );
    }

    /// ★ The same text in two places is two places it could have gone, and a
    /// mechanism that picks one is guessing — so it picks neither and the
    /// window is credited with nothing.
    #[test]
    fn an_edit_whose_new_string_occurs_twice_grants_no_coverage() {
        let tmp = checkout();
        read_in_full(tmp.path(), "s-11", "twice.rs", "keep\nx\n");
        let file = tmp.path().join("twice.rs");
        std::fs::write(&file, "keep\nkeep\n").expect("the edit lands");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-11",
                edit_of(&file, "x\n", "keep\n", false),
            ),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-11");
        let [_, Line::Edit {
            hash_new,
            sites,
            full,
            ..
        }] = lines.as_slice()
        else {
            panic!("a read and the change that followed it, got {lines:?}");
        };
        assert!(sites.is_empty(), "two candidates are no site at all");
        assert!(!*full);
        let ledger = crate::read_ledger::ReadLedger::open(tmp.path(), "s-11", None)
            .load()
            .ledger;
        assert!(ledger.coverage("twice.rs", *hash_new).is_empty());
    }

    /// Every occurrence when every occurrence was asked for — and none at all
    /// when the file's own growth says these replacements were not the only
    /// change in it.
    #[test]
    fn replace_all_records_every_site_or_none() {
        let tmp = checkout();
        read_in_full(tmp.path(), "s-12", "all.rs", "a\nold\nb\nold\n");
        let file = tmp.path().join("all.rs");
        std::fs::write(&file, "a\nnew\nb\nnew\n").expect("the edit lands");
        let call = edit_of(&file, "old\n", "new\n", true);
        record(
            &hook("claude-code", "PostToolUse", "s-12", call.clone()),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-12");
        let [_, Line::Edit { sites, full, .. }] = lines.as_slice() else {
            panic!("a read and the change that followed it, got {lines:?}");
        };
        assert_eq!(
            sites.as_slice(),
            [
                Interval::new(2, 2).expect("a run"),
                Interval::new(4, 4).expect("a run"),
            ]
        );
        assert!(*full);

        // The same call, against bytes something else also added a line to.
        read_in_full(tmp.path(), "s-13", "drift.rs", "a\nold\nb\nold\n");
        let drifted = tmp.path().join("drift.rs");
        std::fs::write(&drifted, "a\nnew\nb\nnew\nextra\n").expect("and a formatter");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-13",
                edit_of(&drifted, "old\n", "new\n", true),
            ),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-13");
        let [_, Line::Edit { sites, full, .. }] = lines.as_slice() else {
            panic!("a read and the change that followed it, got {lines:?}");
        };
        assert!(
            sites.is_empty(),
            "two replacements of one line by one line cannot make a file longer"
        );
        assert!(!*full);
    }

    /// A list of changed places long enough to threaten the one atomic write a
    /// row is has to stop being a list, because a row that cannot be written
    /// leaves the ledger nothing at all — and because "the file changed in
    /// thirty-three places" is "the file changed".
    ///
    /// The cap is pinned here against the row it exists to protect: raising it
    /// past what one append can carry turns a recorded change into a silently
    /// dropped one, and this is what says so.
    #[test]
    fn a_change_in_more_places_than_a_row_can_name_names_none_of_them() {
        let tmp = checkout();
        let file = tmp.path().join("wide.rs");
        let body = "x\n".repeat(200);
        std::fs::write(&file, &body).expect("a file of two hundred identical lines");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-17",
                edit_of(&file, "y\n", "x\n", true),
            ),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-17");
        let [Line::Edit { sites, full, .. }] = lines.as_slice() else {
            panic!("one change, got {lines:?}");
        };
        assert!(sites.is_empty(), "two hundred places are no place to name");
        assert!(!*full);

        // And a row that does name the most places it may is still one write.
        let widest = Line::Edit {
            turn: u32::MAX,
            path: "a".repeat(120),
            hash_old: Some(rift_core::Sha256Id::from_bytes([0xab; 32])),
            hash_new: rift_core::Sha256Id::from_bytes([0xcd; 32]),
            sites: (0..crate::read_ledger::MAX_EDIT_SITES)
                .filter_map(|n| {
                    let lo = u32::try_from(n).ok()? * 1_000 + 900_000;
                    Interval::new(lo, lo + 40)
                })
                .collect(),
            shift: i64::MIN,
            full: true,
            at_ms: u64::MAX,
        };
        assert!(
            widest.to_json_line().len() < crate::read_ledger::MAX_APPEND_BYTES,
            "the fullest row this writes, and its newline, still fit one write"
        );
    }

    /// A replacement written with one line ending, in a file that keeps the
    /// other, is the same replacement.
    #[test]
    fn a_replacement_is_found_whichever_line_ending_the_file_keeps() {
        let tmp = checkout();
        let file = tmp.path().join("crlf.rs");
        std::fs::write(&file, "a\r\nnew\r\nb\r\n").expect("a file with the other ending");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-14",
                edit_of(&file, "old\n", "new\n", false),
            ),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-14");
        let [Line::Edit { sites, .. }] = lines.as_slice() else {
            panic!("one change, got {lines:?}");
        };
        assert_eq!(sites.as_slice(), [Interval::new(2, 2).expect("a run")]);
    }

    /// A deletion has nothing to look for, so it names no place — and a line
    /// naming no place is one nothing is ever declined on.
    #[test]
    fn a_deletion_names_no_place_and_still_says_the_file_shrank() {
        let tmp = checkout();
        read_in_full(tmp.path(), "s-15", "cut.rs", "a\ngone\nb\n");
        let file = tmp.path().join("cut.rs");
        std::fs::write(&file, "a\nb\n").expect("the deletion lands");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-15",
                edit_of(&file, "gone\n", "", false),
            ),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-15");
        let [_, Line::Edit { sites, shift, .. }] = lines.as_slice() else {
            panic!("a read and the change that followed it, got {lines:?}");
        };
        assert!(sites.is_empty());
        assert_eq!(*shift, -1);
    }

    /// A window that had not read the whole file before the edit does not hold
    /// the whole file after it: the lines it never saw are still lines it never
    /// saw. The place it changed is recorded all the same.
    #[test]
    fn an_edit_by_a_window_that_held_only_part_of_the_file_grants_no_coverage() {
        let tmp = checkout();
        let file = tmp.path().join("part.rs");
        std::fs::write(&file, "a\nold\nc\n").expect("a file");
        std::fs::write(&file, "a\nnew\nc\n").expect("the edit lands");
        record(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-16",
                edit_of(&file, "old\n", "new\n", false),
            ),
            tmp.path(),
        );
        let lines = lines_of(tmp.path(), "s-16");
        let [Line::Edit { sites, full, .. }] = lines.as_slice() else {
            panic!("one change, got {lines:?}");
        };
        assert_eq!(sites.as_slice(), [Interval::new(2, 2).expect("a run")]);
        assert!(!*full, "nothing here says this window ever read the file");
    }

    /// Several edits in one call: the change is recorded, because the old
    /// bytes must stop counting, and no coverage follows from it.
    #[test]
    fn a_change_whose_places_do_not_follow_from_its_payload_grants_no_coverage() {
        let tmp = checkout();
        let file = tmp.path().join("many.rs");
        std::fs::write(&file, "a\nb\n").expect("a file to change");
        let payload = serde_json::json!({
            "tool_name": "MultiEdit",
            "tool_input": { "file_path": file.display().to_string() },
        });
        record(
            &hook("claude-code", "PostToolUse", "s-3", payload),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-3");
        let [Line::Edit { sites, full, .. }] = lines.as_slice() else {
            panic!("one change, got {lines:?}");
        };
        assert!(sites.is_empty());
        assert!(!*full);
    }

    /// A call that changed nothing leaves nothing: a tool that failed and a
    /// tool that was never run are the same file on disk, and the ledger has
    /// nothing to say about either.
    #[test]
    fn a_change_that_did_not_land_records_nothing() {
        let tmp = checkout();
        let file = tmp.path().join("same.rs");
        std::fs::write(&file, "a\nb\n").expect("a file to change");
        let payload = serde_json::json!({
            "tool_name": "MultiEdit",
            "tool_input": { "file_path": file.display().to_string() },
        });
        record(
            &hook("claude-code", "PostToolUse", "s-4", payload.clone()),
            tmp.path(),
        );
        record(
            &hook("claude-code", "PostToolUse", "s-4", payload),
            tmp.path(),
        );
        assert_eq!(
            lines_of(tmp.path(), "s-4").len(),
            1,
            "the second call found the file exactly as the first left it"
        );
    }

    /// The refusals, each of which leaves the ledger knowing less than really
    /// happened and never more.
    #[test]
    fn a_change_this_checkout_cannot_stand_behind_records_nothing() {
        let tmp = checkout();
        let outside = checkout();
        let elsewhere = outside.path().join("theirs.rs");
        std::fs::write(&elsewhere, "a\n").expect("someone else's file");
        let cases = [
            ("another checkout's file", write_of(&elsewhere, "a\n")),
            (
                "a path relative to a working directory no payload states",
                serde_json::json!({
                    "tool_name": "Write",
                    "tool_input": { "file_path": "src.rs", "content": "a\n" },
                }),
            ),
            (
                "a call that is not edit-shaped at all",
                serde_json::json!({
                    "tool_name": "Bash",
                    "tool_input": { "command": "ls" },
                }),
            ),
        ];
        for (why, payload) in cases {
            record(
                &hook("claude-code", "PostToolUse", "s-5", payload.clone()),
                tmp.path(),
            );
            record(
                &hook("claude-code", "PreToolUse", "s-5", payload),
                tmp.path(),
            );
            assert!(lines_of(tmp.path(), "s-5").is_empty(), "{why}");
        }
    }

    /// A window that names no session has no ledger to write to, and a family
    /// this build does not carry has no fields to read.
    #[test]
    fn a_hook_with_no_window_to_speak_of_records_nothing() {
        let tmp = checkout();
        let file = tmp.path().join("x.rs");
        std::fs::write(&file, "a\n").expect("a file");
        record(
            &hook("claude-code", "PostToolUse", "", write_of(&file, "a\n")),
            tmp.path(),
        );
        record(
            &hook("nobody", "PostToolUse", "s-6", write_of(&file, "a\n")),
            tmp.path(),
        );
        assert!(lines_of(tmp.path(), "s-6").is_empty());
        assert!(lines_of(tmp.path(), "").is_empty());
    }
}
