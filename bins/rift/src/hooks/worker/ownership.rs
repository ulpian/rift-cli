//! Mutation admission under a borrowed checkout owner. Deferred input remains owed.

use std::path::Path;

use rift_core::Result;
use rift_store::CanonicalStore as _;

use crate::agent_queue::AgentJob;
use crate::capture_queue::{deferred, QueueLock};

pub(super) fn validate_job(
    owner: &QueueLock,
    path: &Path,
    job: &AgentJob,
) -> Result<serde_json::Value> {
    owner
        .check_path(path)
        .map_err(|_| deferred("queue ownership changed or is unavailable"))?;
    let work_tree = std::fs::canonicalize(&job.work_tree)
        .map_err(|_| deferred("job repository is unavailable"))?;
    if !work_tree.is_dir() {
        return Err(deferred("job repository is unavailable"));
    }
    let repo =
        rift_git::discover(&work_tree).map_err(|_| deferred("job repository is unavailable"))?;
    if !owner.matches_git_dir(&repo.git_dir) {
        return Err(deferred("job repository differs from queue owner"));
    }
    let (value, retained) = crate::agent_queue::read_job(path)?;
    if retained != *job {
        return Err(deferred("job delivery changed before execution"));
    }
    crate::capture_queue::delivery::context(&value)
}

pub(super) fn validate_capture(
    owner: &QueueLock,
    path: &Path,
    entry: &crate::capture_queue::QueueEntry,
) -> Result<()> {
    owner
        .check_path(path)
        .map_err(|_| deferred("queue ownership changed or is unavailable"))?;
    let work_tree = std::fs::canonicalize(&entry.work_tree)
        .map_err(|_| deferred("capture repository is unavailable"))?;
    if !work_tree.is_dir() {
        return Err(deferred("capture repository is unavailable"));
    }
    let repo = rift_git::discover(&work_tree)
        .map_err(|_| deferred("capture repository is unavailable"))?;
    if !owner.matches_git_dir(&repo.git_dir) {
        return Err(deferred("capture repository differs from queue owner"));
    }
    let (_, retained) = crate::capture_queue::read_entry(path)?;
    if retained != *entry {
        return Err(deferred("capture delivery changed before execution"));
    }
    Ok(())
}

pub(super) use crate::capture_queue::proof_read;

/// Existing rows omit source inputs such as line ranges and search descriptions.
/// Their counts cannot certify a replay, including an equal-count replay.
pub(super) fn require_new_token_key(
    store: &rift_store::SqliteStore,
    rt: &tokio::runtime::Runtime,
    sql: &str,
    key: &str,
) -> Result<()> {
    let rows = rt
        .block_on(store.query(sql, &[rift_store::Value::text(key)]))
        .map_err(|error| proof_read(error, "token prior-key proof is unavailable"))?;
    if !rows.is_empty() {
        return Err(deferred(
            "token input cannot be proven from the existing row",
        ));
    }
    Ok(())
}

/// What a read's token delivery finds at the key it would write.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ReadTokenKey {
    /// No row stands at the key: the delivery is owed its count.
    Fresh,
    /// The row this delivery would write already stands: the redelivery is
    /// discharged, and its queue file goes.
    Recorded,
}

/// Whether a read's token row already stands at `tool_call_id`, and if so
/// whether it is this delivery's own.
///
/// The key is `read_call_row_id(session, turn, path, content_sha256)`, minted
/// over the read's content hash, so a row at it counts this delivery's bytes
/// only when the read the key names read this delivery's path at this
/// delivery's hash. That is proven from the read's own `tool_reads_file` and
/// `file_revision` rows, never from the job's word: a row at the key whose
/// read names other bytes, or no read at all, stays owed as before.
pub(super) fn read_token_key(
    store: &rift_store::SqliteStore,
    rt: &tokio::runtime::Runtime,
    tool_call_id: &str,
    rel_path: &str,
    content_sha256: &str,
) -> Result<ReadTokenKey> {
    let key = [rift_store::Value::text(tool_call_id)];
    let counted = rt
        .block_on(store.query(
            "SELECT tool_call_id FROM file_read_tokens WHERE tool_call_id = ?1",
            &key,
        ))
        .map_err(|error| proof_read(error, "token prior-key proof is unavailable"))?;
    if counted.is_empty() {
        return Ok(ReadTokenKey::Fresh);
    }
    let read = rt
        .block_on(store.query(
            "SELECT r.path, r.content_sha256 FROM tool_reads_file f \
             JOIN file_revision r ON r.file_revision_id = f.file_revision_id \
             WHERE f.tool_call_id = ?1",
            &key,
        ))
        .map_err(|error| proof_read(error, "token prior-key proof is unavailable"))?;
    let bound = read.iter().any(|row| {
        row.get_text(0).is_ok_and(|path| path == rel_path)
            && row.get_text(1).is_ok_and(|sha| sha == content_sha256)
    });
    if !bound {
        return Err(deferred(
            "token input cannot be proven from the existing row",
        ));
    }
    Ok(ReadTokenKey::Recorded)
}

/// Old lifecycle shortcuts cannot certify the exact current delivery.
pub(super) fn require_condense_input(git_dir: &Path, sessions: &Path, session: &str) -> Result<()> {
    if crate::agent_queue::is_condensed(git_dir, session) {
        return Err(deferred("legacy condense marker lacks delivery proof"));
    }
    if rift_shadow::hooks::session_cache::read_cache(sessions, session)
        .map_err(|error| proof_read(error, "condense session input is unreadable"))?
        .is_some_and(|cache| cache.state == rift_shadow::hooks::SessionState::Ended.as_str())
    {
        return Err(deferred("ended cache lacks exact condense delivery proof"));
    }
    Ok(())
}

/// The marker certifies only this exact validated delivery and existing shard.
pub(super) fn completed_condense(
    owner: &QueueLock,
    context: &serde_json::Value,
    work_tree: &Path,
) -> Result<bool> {
    let name = format!(
        "completed-{}.done",
        crate::capture_queue::delivery::context_id(context)?
    );
    let path = crate::agent_queue::queue_dir(owner.git_dir()).join(name);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(_) => return Err(deferred("condense completion is unreadable")),
    };
    let record = crate::capture_queue::delivery::decode(&bytes)
        .map_err(|_| deferred("condense completion format is unsupported"))?;
    if record.get("context") != Some(context) {
        return Err(deferred("condense completion context conflicts"));
    }
    let id = record
        .get("checkpoint_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| deferred("condense checkpoint is unavailable"))?;
    checkpoint_exists(work_tree, id)?;
    Ok(true)
}

fn checkpoint_exists(work_tree: &Path, id: &str) -> Result<()> {
    use rift_recall_substrate::SubstrateReader as _;
    if id.len() != 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(deferred("condense checkpoint identity is unsupported"));
    }
    let reader = rift_recall_substrate::GixSubstrate::open(work_tree)
        .map_err(|_| deferred("condense checkpoint repository is unreadable"))?;
    if reader
        .read_artifact(id, "metadata.json")
        .map_err(|_| deferred("condense checkpoint proof is unreadable"))?
        .is_none()
    {
        return Err(deferred("condense checkpoint is absent"));
    }
    Ok(())
}

pub(super) fn record_condense(
    owner: &QueueLock,
    context: &serde_json::Value,
    work_tree: &Path,
    checkpoint: &str,
) -> Result<()> {
    owner
        .check_current()
        .map_err(|_| deferred("condense owner changed"))?;
    checkpoint_exists(work_tree, checkpoint)?;
    let record =
        serde_json::json!({"schema_version":1, "context":context, "checkpoint_id":checkpoint});
    let name = format!(
        "completed-{}.done",
        crate::capture_queue::delivery::context_id(context)?
    );
    crate::capture_queue::delivery::publish_completion(
        &crate::agent_queue::queue_dir(owner.git_dir()),
        &name,
        &record,
    )
    .map_err(|_| deferred("condense completion could not be recorded"))
}

/// Old snapshots do not retain the complete queued delivery context.
pub(super) fn require_new_boundary(
    job: &AgentJob,
    snapshot: &crate::agent_queue::BoundarySnapshot,
) -> Result<()> {
    let session = rift_core::capture_session_row_id(&snapshot.agent, &snapshot.session_id);
    let id = rift_shadow::ids::turn_boundary_snapshot_id(
        &session,
        snapshot.ordinal,
        snapshot.boundary.boundary().as_str(),
    );
    let store = rift_store::open_capture_projection(
        crate::hooks::projection_db_at(&job.work_tree)
            .map_err(|error| proof_read(error, "boundary projection location is unavailable"))?,
    )
    .map_err(|error| proof_read(error, "boundary prior-key proof is unavailable"))?;
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(rift_core::RiftError::from)?;
    let rows = rt
        .block_on(store.query(
            "SELECT snapshot_id FROM turn_boundary_snapshot WHERE snapshot_id = ?1",
            &[rift_store::Value::text(id)],
        ))
        .map_err(|error| proof_read(error, "boundary prior-key proof is unavailable"))?;
    if !rows.is_empty() {
        return Err(deferred("boundary snapshot lacks exact queued input proof"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
