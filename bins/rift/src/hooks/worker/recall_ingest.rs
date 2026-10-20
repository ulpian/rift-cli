//! Keeping the family's recall ingest level with the checkpoint branch.
//!
//! Before this, recall ingested only when Canopy launched, when a scoped
//! `rift recall` pull asked, or when `rift-memd` ran, so a family worked on
//! without the dashboard open served recall from an ever older checkpoint. A
//! pass of the worker that wrote a checkpoint now asks for one ingest, and the
//! worker runs it like any other job. No hook decides with it or waits on it.

use std::path::Path;

use rift_core::Result;

use crate::agent_queue::{AgentJob, JobKind};

#[cfg(test)]
thread_local! {
    /// How many recall ingests this thread's worker ran.
    pub(super) static INGESTS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Ask for one recall ingest of the family behind `git_dir`, and say whether
/// the request was journaled.
///
/// The job names nothing but the family's queue, so every pass that asks
/// while one is pending is answered by that one. A request that cannot be
/// journaled is an ingest taken by the next pass that writes a checkpoint, so
/// failing here fails nothing.
pub(super) fn owe_recall_ingest(git_dir: &Path) -> bool {
    let Some(work_tree) = rift_shadow::hooks::session_cache::work_tree_of(git_dir) else {
        return false;
    };
    let job = AgentJob {
        schema_version: 1,
        work_tree,
        queued_at_ms: super::now_ms(),
        attempts: 0,
        kind: JobKind::RecallIngest,
    };
    crate::agent_queue::enqueue(git_dir, &job).is_ok()
}

/// Run the one production ingest pass for the job's checkout.
///
/// It goes through the same guarded entry the dashboard's launch uses: a
/// family with recall written off, or one not set up on this machine, is left
/// untouched. The ingest stamps its watermark with the system clock and only
/// adds rows, so a pass after the branch was rewritten catches up in one go.
pub(super) fn recall_ingest_job(job: &AgentJob) -> Result<()> {
    #[cfg(test)]
    INGESTS.with(|ingests| ingests.set(ingests.get() + 1));
    crate::memory::launch_ingest(&job.work_tree).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::worker::{drain_queues, LockWait};

    fn recall_has_a_watermark(checkout: &Path) -> bool {
        let db = crate::family::recall_db(checkout).unwrap();
        let store = rift_store::SqliteStore::open_read_only(&db).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        rt.block_on(rift_store::CanonicalStore::count(
            &store,
            "ingest_watermark",
        ))
        .unwrap()
            >= 1
    }

    #[test]
    fn a_pass_that_wrote_a_checkpoint_runs_one_recall_ingest_that_advances_the_watermark() {
        let tmp = tempfile::tempdir().unwrap();
        let (repo, _) = super::super::tests::queue_session_end(tmp.path(), "recall-fresh");
        INGESTS.with(|ingests| ingests.set(0));

        let condensed = drain_queues(repo.git_dir.to_str(), LockWait::None).unwrap();

        assert_eq!(condensed.len(), 1);
        assert_eq!(INGESTS.with(std::cell::Cell::get), 1);
        assert_eq!(crate::agent_queue::job_file_count(&repo.git_dir), 0);
        assert!(recall_has_a_watermark(tmp.path()));
    }

    #[test]
    fn a_pass_that_wrote_nothing_asks_for_no_recall_ingest() {
        let tmp = tempfile::tempdir().unwrap();
        super::super::tests::fixture_git(tmp.path(), &["init", "--quiet"]);
        let repo = rift_git::discover(tmp.path()).unwrap();
        assert!(crate::hooks::worker::owe_search_index(
            &repo.git_dir,
            tmp.path()
        ));
        INGESTS.with(|ingests| ingests.set(0));

        drain_queues(repo.git_dir.to_str(), LockWait::None).unwrap();

        assert_eq!(INGESTS.with(std::cell::Cell::get), 0);
        assert_eq!(crate::agent_queue::job_file_count(&repo.git_dir), 0);
    }

    #[test]
    fn two_asks_while_one_is_pending_are_one_recall_ingest_job() {
        let tmp = tempfile::tempdir().unwrap();
        super::super::tests::fixture_git(tmp.path(), &["init", "--quiet"]);
        let repo = rift_git::discover(tmp.path()).unwrap();

        assert!(owe_recall_ingest(&repo.git_dir));
        assert!(owe_recall_ingest(&repo.git_dir));

        let jobs = crate::agent_queue::jobs(&repo.git_dir);
        assert_eq!(jobs.len(), 1);
        assert!(jobs
            .iter()
            .all(|(_, job)| job.kind == JobKind::RecallIngest));
    }
}
