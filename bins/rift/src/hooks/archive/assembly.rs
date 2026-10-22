//! The one checkpoint assembly every archive road builds from: the raw
//! capture, the session facts, the repository's redactor packs, the sealed
//! transcript and the session's tasks with their orphaned originals attached.
//!
//! The post-commit worker, the session-end and sweep archive, and the
//! recovery wrapper each hand these same inputs to `run_decision`, so no road
//! can archive a checkpoint whose bytes another road would have assembled
//! differently. Pack notes and transcript progress stay with the caller:
//! where a note is said and whether progress is recorded is a road's policy.

use rift_core::{Result, SecretBytes};
use rift_shadow::backend::{ArchivedTask, ArchivedTranscript};
use rift_shadow::redact::RulePackSet;

use super::carry::{self, TurnCarry};
use super::tasks::{self, TaskSources};
use super::{originals, session_facts_for_archive, session_raw_capture, session_transcript};
use crate::hooks::turn_end::frontier::SealRead;
use crate::hooks::turn_end::{SealEnd, TurnInputs};
use crate::hooktime::HookTimer;

/// Everything one checkpoint of one session is built from.
pub(crate) struct SessionAssembly {
    pub(crate) raw: SecretBytes,
    pub(crate) facts: rift_shadow::SessionFacts,
    pub(crate) packs: RulePackSet,
    /// Redactor-pack notes the caller says or records.
    pub(crate) notes: Vec<String>,
    pub(crate) transcript: Option<ArchivedTranscript>,
    /// Non-empty raw transcript lines read, for the caller's progress record.
    pub(crate) transcript_raw_lines: Option<u64>,
    pub(crate) tasks: Vec<ArchivedTask>,
    /// What a turn-end seal read, for the frontier record once the shard
    /// lands; `None` on every other road.
    pub(crate) turn_read: Option<SealRead>,
}

/// Assemble one session's checkpoint inputs, the main transcript sealed whole:
/// the commit, session-end, sweep and recovery roads.
pub(crate) fn assemble_session(
    t: &HookTimer,
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    agent: &str,
    session: &str,
) -> Result<SessionAssembly> {
    assemble(t, rt, store, repo, agent, session, None)
}

/// Assemble a turn-end checkpoint's inputs. Everything is as
/// [`assemble_session`] builds it except the main transcript, whose earlier
/// lines are carried from the previous shard and never pass the gate again
/// (re-redacting them each turn would make a turn's cost grow with the
/// transcript).
pub(crate) fn assemble_turn_end(
    t: &HookTimer,
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    agent: &str,
    session: &str,
) -> Result<SessionAssembly> {
    assemble(t, rt, store, repo, agent, session, Some(SealEnd::Turn))
}

/// Assemble a session-end checkpoint's inputs. When the session's turn ends
/// left segmented parts, the main transcript's final body carries them by oid
/// and seals only the tail past them; a refused chain (a truncated file, a
/// moved part, changed packs) reseals the whole file and the frontier record
/// names the break for doctor. A session that never sealed a turn is
/// assembled exactly as [`assemble_session`] does it.
pub(crate) fn assemble_session_end(
    t: &HookTimer,
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    agent: &str,
    session: &str,
) -> Result<SessionAssembly> {
    assemble(t, rt, store, repo, agent, session, Some(SealEnd::Session))
}

/// The one assembly every entry shares; `end` changes only how the main
/// transcript is sealed, `None` sealing it whole.
fn assemble(
    t: &HookTimer,
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    agent: &str,
    session: &str,
    end: Option<SealEnd>,
) -> Result<SessionAssembly> {
    let raw = t.phase("archive_raw_capture", || {
        session_raw_capture(&repo.work_tree)
    });
    let facts = t.phase("archive_read_facts", || {
        session_facts_for_archive(rt, store, &repo.work_tree, agent, session)
    })?;
    // The repository's own redactor packs, folded in beside the built-ins.
    let (packs, notes) = t.phase("rule_packs", || {
        crate::redactor_packs::capture_rule_packs(&repo.work_tree)
    });
    // The session transcript: its ADDRESS was remembered off the hook payload
    // in the session cache; the bytes are read here, compacted on the raw side
    // and sealed once per artifact. Best-effort like the capture itself: a
    // missing or unreadable file archives a transcript-less checkpoint, never
    // an error, because the agent owns that file and may have moved it.
    let carry = end.map(|_| turn_carry(rt, store, repo, session));
    let spool = originals::Spool::load_carrying(&repo.common_dir, session, &packs, carry.as_ref());
    let inputs = end.map(|end| TurnInputs {
        rt,
        store,
        repo,
        session,
        packs: &packs,
        end,
    });
    let turn = inputs.as_ref();
    // The main transcript is read once here; the seal and the subagents'
    // spawn index both take these bytes.
    let main = tasks::main_transcript_path(&repo.git_dir, session);
    let main_raw = main
        .as_deref()
        .and_then(|p| std::fs::read(p).ok())
        .map(SecretBytes::new);
    let transcript = t.phase("archive_transcript", || {
        let read = (main.as_deref()?, main_raw.as_ref()?);
        session_transcript(&repo.git_dir, session, read, &packs, &spool, turn)
    });
    let (mut transcript, transcript_raw_lines, mut turn_read) = match transcript {
        Some((pair, raw_lines, read)) => (Some(pair), Some(raw_lines), read),
        None => (None, None, None),
    };
    let sources = TaskSources {
        spool: Some(&spool),
        main_bytes: main_raw.as_ref().map(SecretBytes::as_slice),
        carry: carry.as_ref(),
        ..TaskSources::of(&repo.git_dir, session, &packs, &facts)
    };
    let mut carried = Vec::new();
    let mut tasks = t.phase("archive_tasks", || {
        tasks::session_tasks_noting(&sources, &mut carried)
    });
    spool.attach_orphans(transcript.as_mut(), &mut tasks);
    if let Some(read) = turn_read.as_mut() {
        carried.extend(carry::original_entries(&spool, transcript.as_ref(), &tasks));
        read.carried = carried;
    }
    Ok(SessionAssembly {
        raw,
        facts,
        packs,
        notes,
        transcript,
        transcript_raw_lines,
        tasks,
        turn_read,
    })
}

/// The previous turn-end shard's subagent pairs and originals, read back by
/// the oids the sidecar recorded for the session's newest checkpoint.
fn turn_carry(
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    session: &str,
) -> TurnCarry {
    let last = rt.block_on(crate::hooks::turn_end::last_checkpoint(store, session));
    let (Some(last), Ok(walker)) = (
        last,
        rift_recall_substrate::ShardWalker::open(&repo.work_tree),
    ) else {
        return TurnCarry::default();
    };
    let read = |path: &str| walker.read_native_entry(&last, path).ok().flatten();
    TurnCarry::after(&repo.git_dir, session, &last, &read)
}
