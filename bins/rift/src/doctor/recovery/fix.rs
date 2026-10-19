//! `rift doctor --fix` for capture recovery: take the owner lock, begin or
//! resume the job, admit its hold, drive the stages, and release the hold on
//! every exit. The plan directory and every stage record stay behind as the
//! receipt; a later run takes a new hold and the stages recheck what changed.
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};

use rift_recall_substrate::GixSubstrate;
use rift_shadow::backend::NativeBackend;
use rift_shadow::hooks::AdapterRegistry;

use crate::capture_queue::QueueLock;
use crate::capture_recovery::hold::{admit_own_hold, HOLD_NAME};
use crate::capture_recovery::job::{begin, release, Job, Member};
use crate::capture_recovery::runner::{recover_naming_historical, Context, Recovered};
use crate::hooks::Historical;

/// The same words the walk ends with when its inventory is empty.
const NOTHING_OWED: &str = "capture recovery: nothing to recover";

/// Why no recovery ran, or why its hold could not be released.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    NotSetUp,
    Busy,
    PlanUnwritten,
    HoldUnadmitted,
    StoreUnavailable,
    NativeUnavailable,
    HoldKept,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Refusal::NotSetUp => "capture recovery not run: this repository is not set up here",
            Refusal::Busy => "capture recovery not run: another capture holds the queue; retry",
            Refusal::PlanUnwritten => {
                "capture recovery not run: the recovery plan could not be written"
            }
            Refusal::HoldUnadmitted => "capture recovery stopped: its hold could not be admitted",
            Refusal::StoreUnavailable => {
                "capture recovery stopped: the projection could not be opened"
            }
            Refusal::NativeUnavailable => {
                "capture recovery stopped: the native tree could not be read"
            }
            Refusal::HoldKept => {
                "capture recovery hold could not be removed; capture waits until a later --fix"
            }
        })
    }
}

/// The walk's words and the boundary tickets the rest of the run leaves to it.
pub(super) fn run(
    root: &Path,
    repo: &rift_git::RepoPaths,
) -> Result<(String, Historical), Refusal> {
    if !crate::family::provisioned_here(root) {
        return Err(Refusal::NotSetUp);
    }
    let family = crate::family::resolve_at(root).map_err(|_| Refusal::NotSetUp)?;
    // A run with nothing owed writes nothing: no lock, no plan, no hold.
    if !owed(&repo.git_dir) {
        return Ok((
            NOTHING_OWED.to_owned(),
            Historical::Named(std::collections::BTreeSet::default()),
        ));
    }
    let family_dir = family_dir(&family.family_id)?;
    let owner = crate::capture_queue::acquire_owner(&repo.git_dir).map_err(|_| Refusal::Busy)?;
    let member = Member {
        repo_id: &family.family_id,
        work_tree: &repo.work_tree,
        family_dir: &family_dir,
    };
    let now = crate::hooks::now_ms();
    let job = begin(&owner, &member, &format!("fix-{now}")).map_err(|_| Refusal::PlanUnwritten)?;
    // The hold is released before any result is reported, refusal included.
    let walked = walk(root, repo, &owner, &job, now);
    release(&owner, &job).map_err(|_| Refusal::HoldKept)?;
    walked.map(|recovered| (recovered.words, recovered.historical))
}

/// Whether anything is owed, read without a lock or a write: a hold left
/// to resume, or a `*.json` ticket in the member's agent journal. With
/// neither, the inventory is empty and the walk could only say so, so no
/// plan is minted for it. A ticket that arrives after this read waits for
/// the next run.
fn owed(git_dir: &Path) -> bool {
    if git_dir.join(HOLD_NAME).symlink_metadata().is_ok() {
        return true;
    }
    crate::family::run_state::agent_queue_read_dirs(git_dir)
        .iter()
        .any(|dir| holds_ticket(dir))
}

fn holds_ticket(dir: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries
        .flatten()
        .any(|entry| Path::new(&entry.file_name()).extension() == Some(OsStr::new("json")))
}

fn family_dir(family_id: &rift_core::RepoId) -> Result<PathBuf, Refusal> {
    let cache = crate::family::cache_root()
        .canonicalize()
        .map_err(|_| Refusal::NotSetUp)?;
    Ok(rift_store::projection_dir_under(&cache, family_id))
}

fn walk(
    root: &Path,
    repo: &rift_git::RepoPaths,
    owner: &QueueLock,
    job: &Job,
    now: i64,
) -> Result<Recovered, Refusal> {
    let hold = admit_own_hold(owner, &job.evidence, &job.operation)
        .map_err(|_| Refusal::HoldUnadmitted)?;
    let store = crate::projection::open_projection(root).map_err(|_| Refusal::StoreUnavailable)?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| Refusal::StoreUnavailable)?;
    let reader = GixSubstrate::open(&repo.work_tree).map_err(|_| Refusal::NativeUnavailable)?;
    let backend = NativeBackend::new(rift_git::PlumbingGitWrite::new(repo.clone()));
    let registry = AdapterRegistry::with_builtin();
    let cx = Context {
        rt: &rt,
        store: &store,
        registry: &registry,
        repo,
        backend: &backend,
        reader: &reader,
        now,
    };
    Ok(recover_naming_historical(cx, &hold))
}
