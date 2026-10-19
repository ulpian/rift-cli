//! Ingesting one agent hook into the per-repo projection: the edit record,
//! worktree observation, the enrichment a payload gets before capture, the
//! work a hook leaves owed to the worker, and the post-ingest drive. Separate
//! because this is the capture side of the agent hook, after the gate has
//! answered.

mod cli_serve;
mod edit;
mod enrich;
pub(in crate::hooks) mod owed;
mod post;
pub(in crate::hooks) mod worktree;

use std::path::Path;

use rift_core::Result;
use rift_shadow::bridge::ingest_hook;
use rift_shadow::hooks::{build_raw_hook, AdapterRegistry};

use enrich::enrich_antigravity_model;
use owed::owe_deferred_measurement;
use post::{drive_post_ingest, snapshot_boundaries};
use worktree::{observe_hook_worktree, worktree_read_hasher};

use super::agent::arm_agent_replacement;
use super::doc_gate::DocAnswer;
use super::gate::window_journal_step;
use super::git::{spawn_capture_worker, CAPTURE_SYNC_ENV};
use super::worker::{drain_capture_queue, owe_session_start_passes};
use super::{capture_repo, current_dir_or_dot, now_ms};

pub(super) use edit::edit_record;
pub(super) use enrich::enrich_native_context;

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
    held_output: &mut Option<serde_json::Value>,
) -> Result<()> {
    let t = crate::hooktime::HookTimer::from_env("agent-hook");
    let verb = verb.to_owned();
    let Some(raw) = enriched_raw_hook(&t, agent, &verb, body) else {
        return Ok(()); // malformed/oversized/agent token → deferred
    };
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
    // WHAT THIS CONTEXT WINDOW IS BEING HANDED AND WHAT IT HAS WRITTEN, and
    // the one registry the rest of this hook maps its payload through.
    let (registry, gate_journaled, held) = window_journal_step(
        &t,
        &raw,
        &repo.git_dir,
        &repo.work_tree,
        decline,
        document,
        search,
    );
    *held_output = held;
    arm_agent_replacement(&repo.work_tree, &raw, decline.as_deref());
    capture_into_projection(&t, &repo, &registry, &raw, agent, gate_journaled)
}

/// The hook as the rest of this module reads one: built **totally** from the
/// untrusted stdin envelope (bounded, no panic), then given the two facts the
/// envelope itself does not carry.
///
/// `None` is a malformed or oversized payload, or an agent token this build
/// does not know: the hook defers and writes nothing.
fn enriched_raw_hook(
    t: &crate::hooktime::HookTimer,
    agent: &str,
    verb: &str,
    body: &[u8],
) -> Option<rift_shadow::hooks::RawHook> {
    let mut raw = t
        .phase("build_raw", || build_raw_hook(agent, verb, body, now_ms()))
        .ok()?;
    t.phase("enrich", || {
        enrich_native_context(&mut raw);
        enrich_antigravity_model(&mut raw);
    });
    Some(raw)
}

/// Everything this hook captured, and the one identity it writes under.
///
/// A context struct rather than seven arguments repeated down the post-ingest
/// steps: every one of them addresses the same projection, the same runtime
/// and the same family, and a step that took its own would be a step that
/// could disagree.
struct CapturedHook<'a> {
    t: &'a crate::hooktime::HookTimer,
    rt: &'a tokio::runtime::Runtime,
    store: &'a rift_store::SqliteStore,
    repo: &'a rift_git::RepoPaths,
    registry: &'a AdapterRegistry,
    raw: &'a rift_shadow::hooks::RawHook,
    repo_id: &'a str,
}

/// Ingest the hook into the family's projection and run everything it implies.
fn capture_into_projection(
    t: &crate::hooktime::HookTimer,
    repo: &rift_git::RepoPaths,
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    agent: &str,
    gate_journaled: bool,
) -> Result<()> {
    let (git_dir, work_tree) = (repo.git_dir.as_path(), repo.work_tree.as_path());
    let Some((store, repo_id)) = family_projection(t, work_tree)? else {
        return Ok(());
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let read_hasher = worktree_read_hasher(work_tree);
    let outcome = t.phase("ingest", || {
        rt.block_on(ingest_hook(
            raw,
            registry,
            &store,
            &crate::family::run_state::sessions_dir(git_dir),
            agent,
            &repo_id,
            now_ms(),
            Some(&read_hasher),
        ))
    })?;
    log_what_the_ingest_repaired_or_refused(raw, &outcome);
    let owed = owe_deferred_measurement(t, git_dir, work_tree, registry, raw, &outcome);
    let sweep_owed = owe_session_start_passes(t, raw, git_dir, work_tree);
    // Bind any newly-appeared sessions to the local developer identity
    // (best-effort: identity failure must never fail the agent's hook).
    // A hook that named no session created none, so there is nothing to
    // bind — and binding spawns child processes, which a hook that
    // captured nothing must not pay for.
    if outcome.receipt == rift_shadow::HookReceipt::DeferredNoSession {
        // The receipt is the whole of a session-less ingest; every step below
        // addresses a session by id, and this hook has none.
        return Ok(());
    }
    bind_hook_identity(t, &rt, &store, work_tree);
    super::subagent_claims::record_claim(git_dir, raw);
    let run = CapturedHook {
        t,
        rt: &rt,
        store: &store,
        repo,
        registry,
        raw,
        repo_id: &repo_id,
    };
    let condense_queued = settle_after_ingest(&run, &outcome);
    // One worker for everything this hook journaled: the condense (if the
    // session ended) or the interval archive, the boundary snapshots, and the
    // token counts. Any of the first three already dispatched; otherwise the
    // owed counts dispatch here.
    if (owed > 0 || gate_journaled || sweep_owed) && !matches!(condense_queued, Ok(true)) {
        t.phase("spawn_worker", || dispatch_agent_worker(git_dir));
    }
    Ok(())
}

/// Say in the log what the ingest already recorded in its rows: each trigger
/// the state machine refused (a `refused-*` transition row), and a session
/// cache it could not read and moved aside (the session re-opened as a
/// `synthesized-start`, the file kept beside the cache as the trace).
fn log_what_the_ingest_repaired_or_refused(
    raw: &rift_shadow::hooks::RawHook,
    outcome: &rift_shadow::IngestOutcome,
) {
    if let Some(aside) = &outcome.cache_moved_aside {
        tracing::warn!(
            session = %raw.session,
            moved_to = %aside.display(),
            "session cache unreadable, moved aside, session re-opened"
        );
    }
    for trigger in &outcome.refused {
        tracing::warn!(
            session = %raw.session,
            trigger = trigger.as_str(),
            "session state machine refused a trigger"
        );
    }
}

/// The projection this hook writes into, and the id every row it writes
/// carries.
///
/// One identity for the whole repository family: a hook
/// fired in a linked worktree lands in the projection its main work tree
/// already owns, and the rows it writes carry the family id — not this
/// checkout's path hash, which would file them where no family-scoped reader
/// looks. Resolved ONCE here; the injector reads the same memoized
/// record, so the whole hook pays for one manifest read.
///
/// `None` is an identity that cannot be proven from this vantage (an
/// unminted family whose main work tree this checkout cannot reach, or a
/// manifest this reader will not guess at). The hook records nothing and
/// still exits 0, and the timer keeps its line so the budget instrument can
/// still see the hook that captured nothing.
fn family_projection(
    t: &crate::hooktime::HookTimer,
    work_tree: &Path,
) -> Result<Option<(rift_store::SqliteStore, String)>> {
    let Some(family) = t.phase("family", || crate::family::try_at(work_tree)) else {
        t.phase("no_family_identity", || ());
        return Ok(None);
    };
    let store = t.phase("open_store", || {
        rift_store::open_capture_projection(crate::family::projection_db_of(&family))
    })?;
    let repo_id = t.phase("repo_id", || family.family_id.to_string());
    Ok(Some((store, repo_id)))
}

/// Everything a session-bearing ingest implies, in the order it implies it.
///
/// Every step is addressed by the session's *owner*, not by the agent that
/// fired this hook: when two agents drive one session id, rows written under
/// the firing agent would belong to a session row that does not exist.
///
/// Returns whether a condense, an interval archive or a boundary snapshot was
/// journaled — its worker is then already dispatched.
fn settle_after_ingest(
    run: &CapturedHook<'_>,
    outcome: &rift_shadow::IngestOutcome,
) -> Result<bool> {
    let owner = outcome.owner_agent.as_str();
    // WHICH WORKTREE THIS HOOK WAS FIRED IN.
    observe_hook_worktree(run, owner);
    // The deferred settle: the turn that
    // has CLOSED by now — never the one this hook closed, whose transcript the
    // agent is still flushing — gets its provider-reported spend recorded at
    // its own Stop event's ids, read forward from the `.usage` sidecar's
    // offset. This replaces the 500 ms inline wait a Stop used to pay.
    // Best-effort — never fails the agent's hook.
    let _ = run.t.phase("settle_usage", || {
        run.rt
            .block_on(rift_shadow::bridge::settle_closed_turn_usage(
                run.store,
                &crate::family::run_state::sessions_dir(&run.repo.git_dir),
                &run.raw.session,
                owner,
                run.repo_id,
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
    // this hook implied, journaled for the detached worker. Best-effort —
    // never fails the agent's hook.
    let snapshots_owed = run.t.phase("snapshot_boundaries", || {
        snapshot_boundaries(run, owner, &outcome.boundaries)
    });
    // Post-ingest strategy drivers: the session-end condense effect and the
    // scheduled interval, both journaled for the detached worker, which is
    // dispatched once for them and the snapshots. Best-effort — never fails
    // the agent's hook.
    run.t.phase("post_ingest", || {
        drive_post_ingest(run, owner, outcome.state, &outcome.effects, snapshots_owed)
    })
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
