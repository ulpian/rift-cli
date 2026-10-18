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

use super::tasks::{self, TaskSources};
use super::{originals, session_facts_for_archive, session_raw_capture, session_transcript};
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
}

/// Assemble one session's checkpoint inputs.
pub(crate) fn assemble_session(
    t: &HookTimer,
    rt: &tokio::runtime::Runtime,
    store: &rift_store::SqliteStore,
    repo: &rift_git::RepoPaths,
    agent: &str,
    session: &str,
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
    let spool = originals::Spool::load(&repo.common_dir, session, &packs);
    let transcript = t.phase("archive_transcript", || {
        session_transcript(&repo.git_dir, session, &packs, &spool)
    });
    let (mut transcript, transcript_raw_lines) = match transcript {
        Some((pair, raw_lines)) => (Some(pair), Some(raw_lines)),
        None => (None, None),
    };
    let sources = TaskSources {
        spool: Some(&spool),
        ..TaskSources::of(&repo.git_dir, session, &packs, &facts)
    };
    let mut tasks = t.phase("archive_tasks", || tasks::session_tasks(&sources));
    spool.attach_orphans(transcript.as_mut(), &mut tasks);
    Ok(SessionAssembly {
        raw,
        facts,
        packs,
        notes,
        transcript,
        transcript_raw_lines,
        tasks,
    })
}
