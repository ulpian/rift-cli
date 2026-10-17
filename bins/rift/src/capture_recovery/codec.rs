//! One bounded adapter around the existing recursively unique queue codec.

use std::ffi::OsStr;
use std::path::Path;

use serde_json::Value;

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
    let mut context = full.clone();
    context.as_object_mut().ok_or(PinError)?.remove("attempts");
    Ok(Decoded {
        full,
        context,
        attempts,
    })
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

#[cfg(test)]
mod tests;
