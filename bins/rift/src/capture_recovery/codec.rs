//! One bounded adapter around the existing recursively unique queue codec.

use std::ffi::OsStr;
use std::path::Path;

use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{PinError, PinResult};

pub(super) const MAX_BYTES: usize = 64 * 1024;

#[derive(Clone, PartialEq)]
pub(super) struct Decoded {
    pub full: Value,
    pub context: Value,
    pub attempts: u32,
}

pub(super) fn basename(name: &OsStr) -> PinResult<()> {
    use std::path::Component;
    let bytes = name.as_encoded_bytes();
    if bytes.is_empty() || bytes.len() > 256 || !bytes.is_ascii() || bytes.contains(&0) {
        return Err(PinError);
    }
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(actual)), None) if actual == name => Ok(()),
        _ => Err(PinError),
    }
}

pub(super) fn decode(name: &OsStr, bytes: &[u8]) -> PinResult<Decoded> {
    basename(name)?;
    if bytes.len() > MAX_BYTES {
        return Err(PinError);
    }
    let full = crate::capture_queue::delivery::decode(bytes).map_err(|_| PinError)?;
    let job: crate::agent_queue::AgentJob =
        serde_json::from_value(full.clone()).map_err(|_| PinError)?;
    let stem = crate::agent_queue::logical_stem(&job).ok_or(PinError)?;
    if !crate::capture_queue::delivery::matches_name(Path::new(name), &stem, &full)
        .map_err(|_| PinError)?
    {
        return Err(PinError);
    }
    let attempts = crate::capture_queue::delivery::attempts(&full).map_err(|_| PinError)?;
    // The queue's own strip, never a local copy: a second definition is how
    // the recovery codec once kept retry members the queue had stopped naming.
    let context = crate::capture_queue::delivery::context(&full).map_err(|_| PinError)?;
    Ok(Decoded {
        full,
        context,
        attempts,
    })
}

/// `sha256(delivery::encode(delivery::context(full)))`, lowercase hex: the one
/// context-digest formula the inventory freezes and every pin is checked by.
pub(super) fn context_sha256(decoded: &Decoded) -> PinResult<String> {
    let context = crate::capture_queue::delivery::context(&decoded.full)
        .and_then(|context| crate::capture_queue::delivery::encode(&context))
        .map_err(|_| PinError)?;
    Ok(format!("{:x}", Sha256::digest(&context)))
}

pub(super) fn wire_decode<T: serde::de::DeserializeOwned>(
    bytes: &[u8],
    cap: usize,
) -> super::WireResult<T> {
    if cap > MAX_BYTES || bytes.len() > cap {
        return Err(super::WireError);
    }
    let unique = crate::capture_queue::delivery::decode(bytes).map_err(|_| super::WireError)?;
    serde_json::from_value(unique).map_err(|_| super::WireError)
}

pub(super) fn wire_encode<T: serde::Serialize>(
    value: &T,
    cap: usize,
) -> super::WireResult<Vec<u8>> {
    if cap > MAX_BYTES {
        return Err(super::WireError);
    }
    let value = serde_json::to_value(value).map_err(|_| super::WireError)?;
    let bytes = crate::capture_queue::delivery::encode(&value).map_err(|_| super::WireError)?;
    if bytes.len() > cap {
        return Err(super::WireError);
    }
    Ok(bytes)
}

/// What the closed recovery gate found a decoded queue ticket to be.
pub(super) enum Shape {
    /// A boundary-snapshot ticket of the exact closed shape, with its facts.
    Boundary(BoundaryFacts),
    /// A job the queue understands that is not a boundary snapshot: owed to
    /// its own executor, never gapped and never run here.
    Other,
}

/// The facts a boundary ticket commits, typed; the raw value is not kept.
pub(super) struct BoundaryFacts {
    pub(super) snapshot: crate::agent_queue::BoundarySnapshot,
    pub(super) queued_at_ms: i64,
    pub(super) attempts: u32,
}

/// The outer keys a boundary ticket may carry: the four AgentJob fields, the
/// tagged kind, and S8's two retry members (`next_due_ms`, `parked`), which
/// are tolerated as siblings of `attempts` and never part of eligibility.
const OUTER_KEYS: [&str; 7] = [
    "attempts",
    "kind",
    "next_due_ms",
    "parked",
    "queued_at_ms",
    "schema_version",
    "work_tree",
];
/// The nested kind keys, exactly: what the real enqueue serializes.
const BOUNDARY_KEYS: [&str; 7] = [
    "agent",
    "boundary",
    "captured_at_ms",
    "head_oid",
    "kind",
    "ordinal",
    "session_id",
];

/// The pure closed recovery gate over the full decoded value of one ticket.
/// Any key outside the closed sets, an unsafe native id, a work tree other
/// than the Plan's, a negative time, a malformed head or a mistyped retry
/// member makes the ticket an unknown executable shape, which refuses.
pub(super) fn recovery_gate(decoded: &Decoded, work_tree: &Path) -> PinResult<Shape> {
    let job: crate::agent_queue::AgentJob =
        serde_json::from_value(decoded.full.clone()).map_err(|_| PinError)?;
    let crate::agent_queue::JobKind::BoundarySnapshot(snapshot) = job.kind else {
        return Ok(Shape::Other);
    };
    let outer = decoded.full.as_object().ok_or(PinError)?;
    if outer.keys().any(|key| !OUTER_KEYS.contains(&key.as_str())) {
        return Err(PinError);
    }
    let nested = outer
        .get("kind")
        .and_then(Value::as_object)
        .ok_or(PinError)?;
    let nested_keys: std::collections::BTreeSet<&str> = nested.keys().map(String::as_str).collect();
    if nested_keys != BOUNDARY_KEYS.iter().copied().collect() {
        return Err(PinError);
    }
    let Some(Value::String(tree)) = outer.get("work_tree") else {
        return Err(PinError);
    };
    super::plan::lexical_path(tree).map_err(|_| PinError)?;
    if Path::new(tree) != work_tree
        || job.queued_at_ms < 0
        || snapshot.captured_at_ms < 0
        || !rift_shadow::hooks::is_safe_session_id(&snapshot.session_id)
        || !snapshot.head_oid.as_deref().is_none_or(is_git_oid)
    {
        return Err(PinError);
    }
    match outer.get("next_due_ms") {
        None => (),
        Some(Value::Number(due)) if due.as_i64().is_some_and(|ms| ms >= 0) => (),
        Some(_) => return Err(PinError),
    }
    match outer.get("parked") {
        None | Some(Value::Bool(_)) => (),
        Some(_) => return Err(PinError),
    }
    Ok(Shape::Boundary(BoundaryFacts {
        snapshot,
        queued_at_ms: job.queued_at_ms,
        attempts: decoded.attempts,
    }))
}

pub(super) fn is_git_oid(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests;
