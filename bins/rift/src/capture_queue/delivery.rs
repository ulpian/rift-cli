//! Context-addressed queue publication. Attempts are the sole mutable field.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use rift_core::{Result, RiftError};
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;

struct UniqueValue(Value);
struct UniqueVisitor;
struct Canonical<'a>(&'a Value);

struct Stage(PathBuf);

#[derive(Clone, Copy)]
enum Existing {
    Context,
    Complete,
}

impl Drop for Stage {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(UniqueVisitor).map(Self)
    }
}

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JSON without duplicate keys")
    }

    fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> std::result::Result<Value, M::Error> {
        let mut keys = BTreeSet::new();
        let mut values = serde_json::Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if !keys.insert(key.clone()) {
                return Err(serde::de::Error::custom("duplicate JSON key"));
            }
            values.insert(key, map.next_value::<UniqueValue>()?.0);
        }
        Ok(Value::Object(values))
    }

    fn visit_seq<S: SeqAccess<'de>>(self, mut seq: S) -> std::result::Result<Value, S::Error> {
        let mut values = Vec::new();
        while let Some(value) = seq.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(Value::Array(values))
    }

    fn visit_bool<E: serde::de::Error>(self, value: bool) -> std::result::Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E: serde::de::Error>(self, value: i64) -> std::result::Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: serde::de::Error>(self, value: u64) -> std::result::Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: serde::de::Error>(self, value: f64) -> std::result::Result<Value, E> {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("invalid JSON number"))
    }

    fn visit_str<E: serde::de::Error>(self, value: &str) -> std::result::Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }

    fn visit_string<E: serde::de::Error>(self, value: String) -> std::result::Result<Value, E> {
        Ok(Value::String(value))
    }

    fn visit_unit<E: serde::de::Error>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }
}

impl Serialize for Canonical<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        match self.0 {
            Value::Object(object) => {
                let mut keys: Vec<_> = object.keys().collect();
                keys.sort_unstable();
                let mut map = serializer.serialize_map(Some(keys.len()))?;
                for key in keys {
                    if let Some(value) = object.get(key) {
                        map.serialize_entry(key, &Canonical(value))?;
                    }
                }
                map.end()
            }
            Value::Array(values) => {
                let mut seq = serializer.serialize_seq(Some(values.len()))?;
                for value in values {
                    seq.serialize_element(&Canonical(value))?;
                }
                seq.end()
            }
            value => value.serialize(serializer),
        }
    }
}

fn invalid() -> RiftError {
    RiftError::validity("queue delivery: unsupported, malformed, or conflicting identity")
}

/// The structural half of [`decode`]: one JSON value with unique keys at every
/// depth and nothing after it. The recovery cache reader shares it — a session
/// cache is `schema: 1`, not a queue delivery — and validates its own closed
/// shape afterwards; the queue's object, schema and attempts checks stay in
/// [`decode`], so nothing a queue refuses is admitted by the split.
pub(crate) fn decode_unique_value(bytes: &[u8]) -> Result<Value> {
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = UniqueValue::deserialize(&mut decoder)
        .map_err(|_| invalid())?
        .0;
    decoder.end().map_err(|_| invalid())?;
    Ok(value)
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Value> {
    let value = decode_unique_value(bytes)?;
    if !value.is_object() || value.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(invalid());
    }
    attempts(&value)?;
    Ok(value)
}

pub(crate) fn attempts(value: &Value) -> Result<u32> {
    match value.get("attempts") {
        None => Ok(0),
        Some(value) => value
            .as_u64()
            .and_then(|n| u32::try_from(n).ok())
            .ok_or_else(invalid),
    }
}

pub(crate) fn encode(value: &Value) -> Result<Vec<u8>> {
    serde_json::to_vec(&Canonical(value)).map_err(|_| invalid())
}

pub(crate) fn context(value: &Value) -> Result<Value> {
    let mut value = value.clone();
    value
        .as_object_mut()
        .ok_or_else(invalid)?
        .remove("attempts");
    Ok(value)
}

pub(crate) fn context_id(value: &Value) -> Result<String> {
    Ok(blake3::hash(&encode(&context(value)?)?)
        .to_hex()
        .to_string())
}

pub(crate) fn matches_name(path: &Path, stem: &str, value: &Value) -> Result<bool> {
    let Some(name) = path.file_stem().and_then(|name| name.to_str()) else {
        return Ok(false);
    };
    Ok(name == stem || name == format!("{stem}--{}", context_id(value)?))
}

pub(crate) fn is_versioned(path: &Path, stem: &str, value: &Value) -> Result<bool> {
    Ok(path.file_stem().and_then(|name| name.to_str())
        == Some(format!("{stem}--{}", context_id(value)?).as_str()))
}

fn stage(dir: &Path, bytes: &[u8]) -> Result<Stage> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    rift_shadow::hooks::run_state::create_private_dir(dir)?;
    for _ in 0..16 {
        let path = dir.join(format!(
            ".delivery-{}-{}.stage",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        };
        let owned = Stage(path);
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        return Ok(owned);
    }
    Err(RiftError::storage(
        "queue delivery: private staging unavailable",
    ))
}

fn verify_existing(bytes: &[u8], value: &Value, policy: Existing) -> Result<()> {
    let retained = decode(bytes)?;
    let equal = match policy {
        Existing::Context => context(&retained)? == context(value)?,
        Existing::Complete => retained == *value,
    };
    if !equal {
        return Err(invalid());
    }
    Ok(())
}

fn publish_with(
    dir: &Path,
    stem: &str,
    value: &Value,
    policy: Existing,
) -> Result<(PathBuf, bool)> {
    if stem.is_empty()
        || stem.len() > 180
        || !stem
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(invalid());
    }
    let bytes = encode(value)?;
    decode(&bytes)?;
    let path = dir.join(format!("{stem}--{}.json", context_id(value)?));
    match std::fs::read(&path) {
        Ok(bytes) => {
            verify_existing(&bytes, value, policy)?;
            return Ok((path, false));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let staged = stage(dir, &bytes)?;
    #[cfg(test)]
    run_publication_pause();
    match std::fs::hard_link(&staged.0, &path) {
        Ok(()) => Ok((path, true)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_existing(&std::fs::read(&path)?, value, policy)?;
            Ok((path, false))
        }
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn publish(dir: &Path, stem: &str, value: &Value) -> Result<PathBuf> {
    publish_with(dir, stem, value, Existing::Context).map(|(path, _)| path)
}

pub(crate) fn publish_exact(dir: &Path, stem: &str, value: &Value) -> Result<PathBuf> {
    publish_with(dir, stem, value, Existing::Complete).map(|(path, _)| path)
}

pub(crate) fn publish_exact_observed(
    dir: &Path,
    stem: &str,
    value: &Value,
) -> Result<(PathBuf, bool)> {
    publish_with(dir, stem, value, Existing::Complete)
}

/// Publish an owned completion record without replacing another record.
pub(crate) fn publish_completion(dir: &Path, name: &str, value: &Value) -> Result<()> {
    if name.is_empty()
        || name.len() > 100
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'.'
        })
    {
        return Err(invalid());
    }
    let path = dir.join(name);
    match std::fs::read(&path) {
        Ok(bytes) => return verify_existing(&bytes, value, Existing::Complete),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    let staged = stage(dir, &encode(value)?)?;
    match std::fs::hard_link(&staged.0, &path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            verify_existing(&std::fs::read(path)?, value, Existing::Complete)
        }
        Err(error) => Err(error.into()),
    }
}

/// The caller holds the member owner; producers never replace final files.
pub(crate) fn replace_attempts(
    path: &Path,
    stem: &str,
    expected: &Value,
    next: u32,
    owner: &super::QueueLock,
) -> Result<()> {
    owner.check_path(path)?;
    if !is_versioned(path, stem, expected)? {
        return Err(invalid());
    }
    let retained = decode(&std::fs::read(path)?)?;
    if retained != *expected || !matches_name(path, stem, &retained)? {
        return Err(invalid());
    }
    let mut updated = retained;
    updated
        .as_object_mut()
        .ok_or_else(invalid)?
        .insert("attempts".into(), next.into());
    let dir = path.parent().ok_or_else(invalid)?;
    let staged = stage(dir, &encode(&updated)?)?;
    std::fs::rename(&staged.0, path)?;
    Ok(())
}

#[cfg(test)]
thread_local! {
    static PUBLICATION_PAUSE: std::cell::RefCell<Option<Box<dyn FnOnce()>>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(crate) struct PublicationPauseGuard(Option<Box<dyn FnOnce()>>);

#[cfg(test)]
impl Drop for PublicationPauseGuard {
    fn drop(&mut self) {
        PUBLICATION_PAUSE.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

#[cfg(test)]
pub(crate) fn pause_publication(callback: impl FnOnce() + 'static) -> PublicationPauseGuard {
    PublicationPauseGuard(PUBLICATION_PAUSE.with(|slot| slot.replace(Some(Box::new(callback)))))
}

#[cfg(test)]
fn run_publication_pause() {
    let callback = PUBLICATION_PAUSE.with(std::cell::RefCell::take);
    if let Some(callback) = callback {
        callback();
    }
}

#[cfg(test)]
mod tests;
