//! The recovery's one archive call: the carrier session built by the same
//! assembly the normal archive roads use, with the frozen gap artifact set on
//! its facts before `run_decision` derives the id, through the wrapper.
//!
//! Nothing here records transcript progress, pack notes, a marker or a chain
//! entry: those are the normal roads' sidecars, and a recovery attempt owns
//! only its intent until a later receipt proves the publication.

use rift_core::Result;
use rift_shadow::backend::ShadowBackend;
use rift_shadow::hooks::strategy::ArchiveDecision;
use rift_shadow::CheckpointId;

use super::RecoveryBackend;
use crate::hooktime::HookTimer;

/// Where and when the recovery archives.
pub(crate) struct RecoveryRun<'a> {
    pub(crate) rt: &'a tokio::runtime::Runtime,
    pub(crate) store: &'a rift_store::SqliteStore,
    pub(crate) repo: &'a rift_git::RepoPaths,
    pub(crate) commit: Option<&'a str>,
    pub(crate) now: i64,
}

/// Assemble the carrier's checkpoint and hand it to the wrapper, which
/// delegates it once or refuses; the caller reads `delegation()` for which.
pub(crate) fn archive_recovered<B: ShadowBackend>(
    run: &RecoveryRun<'_>,
    recovery: &RecoveryBackend<'_, B>,
    decision: ArchiveDecision,
) -> Result<Option<CheckpointId>> {
    let carrier = recovery.carrier();
    let session = carrier.native_session();
    let untimed = HookTimer::new("recovery-archive", false);
    let mut assembly = crate::hooks::assemble_session(
        &untimed,
        run.rt,
        run.store,
        run.repo,
        carrier.agent(),
        session,
    )?;
    // Set before the id is derived: the gap bytes are part of the root.
    assembly.facts.boundary_gaps = Some(recovery.artifact().clone());
    run.rt.block_on(rift_shadow::run_decision(
        recovery,
        run.store,
        &run.repo.work_tree,
        session,
        &assembly.raw,
        decision,
        run.commit,
        run.now,
        assembly.facts,
        &assembly.packs,
        assembly.transcript,
        assembly.tasks,
    ))
}
