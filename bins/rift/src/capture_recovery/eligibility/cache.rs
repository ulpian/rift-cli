//! The member's current session cache, read descriptor-relative under the
//! recovery read cap and projected to the few semantic facts eligibility uses.
//!
//! Only the current sessions tree is read: a missing or malformed current
//! cache is unavailable, never a reason to read the legacy tree, and only a
//! true `NotFound` is "absent" — a symlink, a directory, a permission failure
//! or a torn file is unavailable with its own reason. Nothing here repairs,
//! moves or writes.
use std::ffi::OsStr;

use rift_shadow::hooks::session_cache::SessionCache;

use super::{Budget, CacheProjection, Reason};
use crate::capture_recovery::codec::MAX_BYTES;
use crate::capture_recovery::files::{self, Namespace};

/// Read and project `<native>.json` from the admitted sessions tree, charging
/// the pass's cache budget for every byte the attempt cost, sentinel included.
pub(super) fn read_projection(
    sessions: &Namespace,
    native: &str,
    budget: &mut Budget,
) -> Result<CacheProjection, Reason> {
    if !rift_shadow::hooks::is_safe_session_id(native) {
        return Err(Reason::NativeUnsafe);
    }
    let name = format!("{native}.json");
    sessions.recheck().map_err(|_| Reason::CacheUnreadable)?;
    let before = match sessions.dir.symlink_metadata(&name) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(Reason::CacheAbsent)
        }
        Err(_) => return Err(Reason::CacheUnreadable),
    };
    if !before.is_file() {
        return Err(Reason::CacheNotRegular);
    }
    if before.len() > MAX_BYTES as u64 {
        budget.charge_cache(MAX_BYTES as u64 + 1)?;
        return Err(Reason::CacheOversize);
    }
    let Ok(read) = files::read_regular(sessions, OsStr::new(&name), MAX_BYTES) else {
        // The bounded read charges its sentinel whatever stopped it.
        budget.charge_cache(MAX_BYTES as u64 + 1)?;
        return Err(Reason::CacheUnreadable);
    };
    budget.charge_cache(read.bytes.len() as u64)?;
    project(&read.bytes)
}

/// The structural parse the queue shares, then the cache's own shape; the
/// transcript address, the id map and the memo are discarded here.
fn project(bytes: &[u8]) -> Result<CacheProjection, Reason> {
    use sha2::{Digest as _, Sha256};
    let value = crate::capture_queue::delivery::decode_unique_value(bytes)
        .map_err(|_| Reason::CacheMalformed)?;
    let cache: SessionCache = serde_json::from_value(value).map_err(|_| Reason::CacheMalformed)?;
    Ok(CacheProjection {
        cache_sha256: format!("{:x}", Sha256::digest(bytes)),
        schema: cache.schema,
        session_id: cache.session_id,
        agent: cache.agent,
        state: cache.state,
        ordinal: cache.current_turn_ordinal,
        turn_open: cache.turn_open,
        work_tree: cache.work_tree,
    })
}
