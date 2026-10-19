//! The recovery job's plan and hold: one chokepoint writes them, resumes an
//! existing hold, and releases it. The hold stops all ordinary capture while
//! it exists, so the job that writes it is the one that removes it; the plan
//! directory stays behind as the receipt of the operation.
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use rift_core::RepoId;

use super::hold::{Hold, HOLD_NAME, MAX_HOLD_BYTES};
use super::plan::{member_key, Plan};
use super::{EvidenceError, EvidenceResult};
use crate::capture_queue::QueueLock;

/// The Plan maxima for one ticket and for all tickets together.
const TICKET_BUDGETS: (u64, u64) = (65_536, 67_108_864);

/// One recovery operation this process may drive: its evidence directory and
/// the exact hold bytes it wrote or resumed.
pub(crate) struct Job {
    pub(crate) operation: String,
    pub(crate) evidence: PathBuf,
    pub(crate) resumed: bool,
    hold_bytes: Vec<u8>,
}

/// What the member is and where its cache lives; the caller resolves both.
pub(crate) struct Member<'a> {
    pub(crate) repo_id: &'a RepoId,
    pub(crate) work_tree: &'a Path,
    pub(crate) family_dir: &'a Path,
}

/// Resume the hold already in the member's git dir, or write a new plan and
/// then the hold, both no-clobber. Holding the owner lock is the proof that
/// the process which wrote an existing hold is gone, so resuming it is the
/// takeover the ruling allows.
pub(crate) fn begin(
    owner: &QueueLock,
    member: &Member<'_>,
    operation: &str,
) -> EvidenceResult<Job> {
    owner.check_current().map_err(|_| EvidenceError)?;
    let hold_path = owner.git_dir().join(HOLD_NAME);
    if hold_path.symlink_metadata().is_ok() {
        return resume(&hold_path, member);
    }
    let evidence = recovery_dir(member.family_dir, operation);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&evidence)
        .map_err(|_| EvidenceError)?;
    let evidence = evidence.canonicalize().map_err(|_| EvidenceError)?;
    let plan = plan_for(owner, member, operation, &evidence)?;
    write_new(
        &evidence.join("plan.json"),
        &plan.encode().map_err(|_| EvidenceError)?,
    )?;
    let digest = plan.sha256().map_err(|_| EvidenceError)?;
    let hold = Hold::new(operation, plan.member(), digest.as_str()).map_err(|_| EvidenceError)?;
    let hold_bytes = hold.encode().map_err(|_| EvidenceError)?;
    write_new(&hold_path, &hold_bytes)?;
    Ok(Job {
        operation: operation.to_owned(),
        evidence,
        resumed: false,
        hold_bytes,
    })
}

/// Remove the hold this job wrote or resumed, and nothing else: a hold whose
/// bytes changed belongs to someone else and stays.
pub(crate) fn release(owner: &QueueLock, job: &Job) -> EvidenceResult<()> {
    owner.check_current().map_err(|_| EvidenceError)?;
    let hold_path = owner.git_dir().join(HOLD_NAME);
    match read_hold(&hold_path) {
        Ok(bytes) if bytes == job.hold_bytes => {
            std::fs::remove_file(&hold_path).map_err(|_| EvidenceError)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(EvidenceError),
    }
}

/// The report-only words for a hold in `git_dir`: which operation and for how
/// long, read without opening a lock or writing anything.
pub(crate) fn held_words(git_dir: &Path, now: std::time::SystemTime) -> Option<String> {
    let hold_path = git_dir.join(HOLD_NAME);
    let metadata = hold_path.symlink_metadata().ok()?;
    let age = metadata
        .modified()
        .ok()
        .and_then(|at| now.duration_since(at).ok())
        .map_or(0, |age| age.as_secs());
    let operation = read_hold(&hold_path)
        .ok()
        .and_then(|bytes| Hold::decode(&bytes).ok())
        .map(|hold| hold.operation().to_owned());
    Some(match operation {
        Some(operation) => format!(
            "capture recovery held: operation {operation}, held for {age}s; ordinary capture waits until `rift doctor --fix` finishes it"
        ),
        None => format!(
            "capture recovery held: an unreadable hold, held for {age}s; ordinary capture waits"
        ),
    })
}

fn resume(hold_path: &Path, member: &Member<'_>) -> EvidenceResult<Job> {
    let hold_bytes = read_hold(hold_path).map_err(|_| EvidenceError)?;
    let hold = Hold::decode(&hold_bytes).map_err(|_| EvidenceError)?;
    let evidence = recovery_dir(member.family_dir, hold.operation())
        .canonicalize()
        .map_err(|_| EvidenceError)?;
    Ok(Job {
        operation: hold.operation().to_owned(),
        evidence,
        resumed: true,
        hold_bytes,
    })
}

fn recovery_dir(family_dir: &Path, operation: &str) -> PathBuf {
    family_dir.join("recovery").join(operation)
}

fn plan_for(
    owner: &QueueLock,
    member: &Member<'_>,
    operation: &str,
    evidence: &Path,
) -> EvidenceResult<Plan> {
    let git_dir = owner.git_dir().to_str().ok_or(EvidenceError)?;
    let key = member_key(member.repo_id, git_dir).map_err(|_| EvidenceError)?;
    let (max_ticket_bytes, max_total_ticket_bytes) = TICKET_BUDGETS;
    let value = serde_json::json!({
        "schema_version": 1,
        "operation_id": operation,
        "repo_id": member.repo_id.as_str(),
        "member_key": key.to_hex(),
        "canonical_git_dir": git_dir,
        "canonical_work_tree": member.work_tree,
        "evidence_dir": evidence,
        "selector": {
            "kind": "boundary-snapshots-only",
            "schema_version": 1,
            "unknown_fields": "refuse",
            "inventory": "complete-bounded",
            "reservation": "one-eligible-explicit-current-boundary@1",
        },
        "budgets": {
            "max_inventory_entries": 8192,
            "max_selected_tickets": 8192,
            "max_ticket_bytes": max_ticket_bytes,
            "max_total_ticket_bytes": max_total_ticket_bytes,
            "max_cut_bytes": 8_388_608,
            "max_artifact_bytes": 8_388_608,
        },
    });
    let bytes = serde_json::to_vec(&value).map_err(|_| EvidenceError)?;
    Plan::decode(&bytes).map_err(|_| EvidenceError)
}

fn read_hold(path: &Path) -> std::io::Result<Vec<u8>> {
    let bytes = std::fs::read(path)?;
    if bytes.len() > MAX_HOLD_BYTES {
        return Err(std::io::Error::other("hold over its cap"));
    }
    Ok(bytes)
}

/// Created 0600 and never over an existing name, then synced.
fn write_new(path: &Path, bytes: &[u8]) -> EvidenceResult<()> {
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| EvidenceError)?;
    file.write_all(bytes).map_err(|_| EvidenceError)?;
    file.sync_all().map_err(|_| EvidenceError)
}

#[cfg(test)]
mod tests;
