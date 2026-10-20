//! Codex's trust identity for one hook handler, reproduced.
//!
//! Codex runs a project hook only while the hash it recorded when the user
//! trusted the hook equals the hash of the hook as it now stands, and it
//! drops a mismatched hook without a word. To say which of Rift's hooks
//! Codex is skipping, Rift computes the same hash. It is SHA-256 because it
//! has to equal a value another program wrote.
//!
//! The identity is not the handler's bytes. Codex normalises the handler
//! first — the timeout it will actually apply, the context limit only where
//! the event can emit context and only when it differs from Codex's default,
//! the matcher only where the event dispatches on one — and hashes the
//! canonical JSON of the result: keys sorted, no whitespace. Every rule here
//! is pinned by the fixtures in `fixtures/codex-trust/`, recorded for the
//! Codex versions in [`PROVEN_CODEX_VERSIONS`]; for any other version the
//! caller must not use this hash at all.

use serde_json::{Map, Value};
use sha2::{Digest as _, Sha256};

/// The Codex versions whose hook hashing the fixtures were recorded against.
/// A version outside this list may hash differently, so a hash computed for
/// it proves nothing and the trust verdict is "unverifiable" instead, or
/// "unsupported version" when it is newer than every version here.
///
/// `fixtures/codex-trust/config.toml` pins 0.153.1 and 0.155.0-alpha.16.3:
/// a record taken before the user re-trusted two modified handlers, so it
/// carries two mismatches. `fixtures/codex-trust/config-0.157.0.toml` pins
/// 0.157.0: the record taken after that re-trust, all ten hashes current.
/// `fixtures/codex-trust/config-0.163.0.toml` pins 0.163.0: its re-trust of
/// the release-binary handlers in `hooks-0.163.0.json`, all ten reproduced.
pub(super) const PROVEN_CODEX_VERSIONS: [&str; 4] =
    ["0.153.1", "0.155.0-alpha.16.3", "0.157.0", "0.163.0"];

/// The newest of [`PROVEN_CODEX_VERSIONS`], the one a newer client is
/// measured against.
pub(crate) fn newest_proven_codex_version() -> &'static str {
    PROVEN_CODEX_VERSIONS
        .iter()
        .copied()
        .max_by_key(|version| version_order(version))
        .unwrap_or_default()
}

/// Whether `version` is newer than every proven version. A version Rift
/// cannot read as `major.minor.patch` is never called newer, so it stays
/// merely unverifiable.
pub(super) fn is_newer_than_every_proven(version: &str) -> bool {
    let newest = version_order(newest_proven_codex_version());
    version_order(version).is_some_and(|order| Some(order) > newest)
}

/// A sortable form of a Codex version: the three numbers, then whether it is
/// a full release, since `0.155.0-alpha.1` comes before `0.155.0`.
fn version_order(version: &str) -> Option<(u64, u64, u64, bool)> {
    let (core, pre) = match version.split_once('-') {
        Some((core, pre)) => (core, Some(pre)),
        None => (version, None),
    };
    let mut parts = core.split('.').map(str::parse::<u64>);
    let (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch)), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };
    Some((major, minor, patch, pre.is_none()))
}

/// The context limit Codex applies when a handler declares none; a handler
/// that declares exactly this hashes as if it declared nothing.
const DEFAULT_CONTEXT_LIMIT: u64 = 2500;

/// The timeout Codex applies to a handler that declares none, outside the
/// two short-lived events.
const DEFAULT_TIMEOUT_SEC: u64 = 600;

/// The ceiling (and the default) for `SessionEnd` and `Interrupt` handlers.
const SHORT_EVENT_MAX_TIMEOUT_SEC: u64 = 3;
const SHORT_EVENT_DEFAULT_TIMEOUT_SEC: u64 = 1;

/// The label Codex keys an event's trust record with, for a hook-file event
/// name; `None` for an event Codex does not know.
pub(super) fn event_key_label(event: &str) -> Option<&'static str> {
    Some(match event {
        "PreToolUse" => "pre_tool_use",
        "PermissionRequest" => "permission_request",
        "PostToolUse" => "post_tool_use",
        "PreCompact" => "pre_compact",
        "PostCompact" => "post_compact",
        "SessionStart" => "session_start",
        "SessionEnd" => "session_end",
        "UserPromptSubmit" => "user_prompt_submit",
        "SubagentStart" => "subagent_start",
        "SubagentStop" => "subagent_stop",
        "Stop" => "stop",
        "Interrupt" => "interrupt",
        _ => return None,
    })
}

/// The `sha256:<hex>` identity Codex records for one command handler of
/// `event` inside `group`, or `None` when the handler is not a command
/// handler Codex would load.
pub(super) fn handler_identity_hash(
    event: &str,
    group: &Map<String, Value>,
    handler: &Map<String, Value>,
) -> Option<String> {
    let label = event_key_label(event)?;
    let normalized = normalized_handler(event, handler)?;
    let mut identity = Map::new();
    identity.insert("event_name".to_owned(), Value::from(label));
    match group.get("matcher").and_then(Value::as_str) {
        Some(matcher) if dispatches_on_matcher(event) => {
            identity.insert("matcher".to_owned(), Value::from(matcher));
        }
        _ => (),
    }
    identity.insert("hooks".to_owned(), Value::Array(vec![normalized]));
    let bytes = serde_json::to_vec(&canonical(&Value::Object(identity))).ok()?;
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    Some(format!(
        "sha256:{}",
        rift_core::Sha256Id::from_bytes(digest).to_hex()
    ))
}

/// The handler as Codex hashes it: the fields it keeps, with the values it
/// would apply rather than the ones written.
fn normalized_handler(event: &str, handler: &Map<String, Value>) -> Option<Value> {
    if handler.get("type").and_then(Value::as_str) != Some("command") {
        return None;
    }
    let command = platform_command(handler)?;
    if command.trim().is_empty() {
        return None;
    }
    let mut normalized = Map::new();
    normalized.insert("type".to_owned(), Value::from("command"));
    normalized.insert("command".to_owned(), Value::from(command));
    let timeout = handler.get("timeout").and_then(Value::as_u64);
    normalized.insert(
        "timeout".to_owned(),
        Value::from(effective_timeout(event, timeout)),
    );
    let runs_async = handler
        .get("async")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    normalized.insert("async".to_owned(), Value::from(runs_async));
    if let Some(status) = handler.get("statusMessage").and_then(Value::as_str) {
        normalized.insert("statusMessage".to_owned(), Value::from(status));
    }
    match handler
        .get("additionalContextLimit")
        .and_then(Value::as_u64)
    {
        Some(limit) if emits_context(event) && limit != DEFAULT_CONTEXT_LIMIT => {
            normalized.insert("additionalContextLimit".to_owned(), Value::from(limit));
        }
        _ => (),
    }
    Some(Value::Object(normalized))
}

/// The command Codex runs on this platform: the Windows override where one
/// is written and this is Windows, the plain command everywhere else.
fn platform_command(handler: &Map<String, Value>) -> Option<&str> {
    let windows = handler.get("commandWindows").and_then(Value::as_str);
    match windows {
        Some(command) if cfg!(windows) => Some(command),
        _ => handler.get("command").and_then(Value::as_str),
    }
}

fn effective_timeout(event: &str, timeout: Option<u64>) -> u64 {
    match event {
        "SessionEnd" | "Interrupt" => timeout
            .unwrap_or(SHORT_EVENT_DEFAULT_TIMEOUT_SEC)
            .clamp(1, SHORT_EVENT_MAX_TIMEOUT_SEC),
        _ => timeout.unwrap_or(DEFAULT_TIMEOUT_SEC).max(1),
    }
}

/// The events that can return `additionalContext`, the only ones whose
/// context limit is part of the identity.
fn emits_context(event: &str) -> bool {
    matches!(
        event,
        "PreToolUse" | "PostToolUse" | "SessionStart" | "UserPromptSubmit" | "SubagentStart"
    )
}

/// The events Codex dispatches against a matcher; on the others it ignores
/// a written matcher, and so does the identity.
fn dispatches_on_matcher(event: &str) -> bool {
    !matches!(event, "UserPromptSubmit" | "Stop" | "Interrupt")
}

/// `value` with every object's keys in sorted order, whatever order the map
/// type keeps.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let mut sorted = Map::new();
            for key in keys {
                if let Some(inner) = map.get(key) {
                    sorted.insert(key.clone(), canonical(inner));
                }
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(value: &Value) -> &Map<String, Value> {
        value.as_object().unwrap()
    }

    /// The normalisation rules one by one, each against the hash of the
    /// handler it must equal: a rule that stopped applying would change one
    /// side and not the other.
    #[test]
    fn the_identity_hashes_what_codex_would_apply_not_what_was_written() {
        let group = serde_json::json!({});
        let hash = |event: &str, handler: Value| {
            handler_identity_hash(event, object(&group), object(&handler)).unwrap()
        };
        let plain = serde_json::json!({"type": "command", "command": "run"});
        assert_eq!(
            hash("Stop", plain.clone()),
            hash(
                "Stop",
                serde_json::json!({"type": "command", "command": "run", "timeout": 600})
            ),
            "an absent timeout is Codex's default"
        );
        assert_eq!(
            hash(
                "SessionEnd",
                serde_json::json!({"type": "command", "command": "run", "timeout": 10})
            ),
            hash(
                "SessionEnd",
                serde_json::json!({"type": "command", "command": "run", "timeout": 3})
            ),
            "a SessionEnd timeout is clamped to three seconds"
        );
        let limited = |limit: u64| serde_json::json!({"type": "command", "command": "run", "additionalContextLimit": limit});
        assert_eq!(
            hash("SessionStart", limited(2500)),
            hash("SessionStart", plain.clone())
        );
        assert_ne!(
            hash("SessionStart", limited(1200)),
            hash("SessionStart", plain.clone())
        );
        assert_eq!(
            hash("Stop", limited(1200)),
            hash("Stop", plain),
            "an event that cannot emit context drops the limit"
        );
    }

    #[test]
    fn the_matcher_counts_only_where_codex_dispatches_on_it() {
        let handler = serde_json::json!({"type": "command", "command": "run"});
        let matched = serde_json::json!({"matcher": "*"});
        let bare = serde_json::json!({});
        let hash = |event: &str, group: &Value| {
            handler_identity_hash(event, object(group), object(&handler)).unwrap()
        };
        assert_ne!(hash("PreToolUse", &matched), hash("PreToolUse", &bare));
        assert_eq!(
            hash("UserPromptSubmit", &matched),
            hash("UserPromptSubmit", &bare)
        );
    }

    #[test]
    fn a_handler_codex_would_not_load_has_no_identity() {
        let group = serde_json::json!({});
        for handler in [
            serde_json::json!({"type": "prompt"}),
            serde_json::json!({"type": "command", "command": "  "}),
            serde_json::json!({"command": "run"}),
        ] {
            assert_eq!(
                handler_identity_hash("Stop", object(&group), object(&handler)),
                None
            );
        }
        let handler = serde_json::json!({"type": "command", "command": "run"});
        assert_eq!(
            handler_identity_hash("NoSuchEvent", object(&group), object(&handler)),
            None
        );
    }
}
