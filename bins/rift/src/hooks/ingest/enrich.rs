//! The facts a payload is given before it is captured: the model an agent's
//! own hook does not name, and the context snapshot only its transcript holds.
//!
//! Separate because every function here is best-effort and honest — a fact it
//! cannot read is left absent rather than fabricated — and because the reading
//! is bounded on purpose, never a walk of a private transcript format.

use std::io::Read as _;
use std::path::{Path, PathBuf};

/// Antigravity's documented hook payload does not expose the selected model.
/// When its ordinary settings file names one concrete model, attach that fact
/// with explicit configured provenance. `auto`/`default` remain unavailable;
/// private transcript/database formats are never inspected.
pub(super) fn enrich_antigravity_model(raw: &mut rift_shadow::hooks::RawHook) {
    if raw.agent != rift_core::AgentId::ANTIGRAVITY || raw.payload.get("model").is_some() {
        return;
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let candidates = [
        home.join(".gemini/antigravity-cli/settings.json"),
        home.join(".gemini/antigravity/settings.json"),
    ];
    let model = candidates.into_iter().find_map(|path| {
        let bytes = std::fs::read(path).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
        value
            .get("model")
            .and_then(serde_json::Value::as_str)
            .filter(|model| {
                !model.is_empty()
                    && !model.eq_ignore_ascii_case("auto")
                    && !model.eq_ignore_ascii_case("default")
            })
            .map(str::to_owned)
    });
    if let (Some(model), Some(payload)) = (model, raw.payload.as_object_mut()) {
        payload.insert(
            "_rift_configured_model".to_owned(),
            serde_json::Value::String(model),
        );
    }
}

/// Native Claude Code hooks carry no usage fields; the transcript named by
/// the payload's `transcript_path` holds the provider-reported numbers. At a
/// mid-turn boundary (`PreToolUse`) read a small bounded tail and inject one
/// call-grain **context** snapshot (`context_usage` + `model`) — the Canopy
/// nic-gauge's pressure reading. A turn's **spend** is deliberately NOT read
/// here any more: a `Stop` fires while the agent is still flushing the
/// transcript, and the inline settle that made that safe cost the hook the
/// whole 500 ms quiet window. Spend is settled by the next hook for the
/// session from the `.usage` sidecar's offset instead
/// (`rift_shadow::bridge::settle_closed_turn_usage`), at the same ids.
/// Best-effort and honest: a missing, unreadable, or usage-free transcript
/// leaves the fields absent, never fabricated.
pub(in crate::hooks) fn enrich_native_context(raw: &mut rift_shadow::hooks::RawHook) {
    use rift_shadow::hooks::transcript::{latest_main_context, CONTEXT_TAIL_BYTES};
    use rift_shadow::hooks::AgentId;

    if raw.agent != AgentId::CLAUDE_CODE || raw.verb != "PreToolUse" {
        return;
    }
    let Some(obj) = raw.payload.as_object_mut() else {
        return;
    };
    let Some(path) = obj
        .get("transcript_path")
        .and_then(serde_json::Value::as_str)
    else {
        return;
    };
    let Some(tail) = read_tail(Path::new(path), CONTEXT_TAIL_BYTES) else {
        return;
    };
    let Some(context) = latest_main_context(&tail) else {
        return;
    };
    if !obj.contains_key("context_usage") {
        let mut usage = serde_json::Map::new();
        for (key, count) in [
            ("input_tokens", context.usage.input),
            ("output_tokens", context.usage.output),
            ("cache_read_input_tokens", context.usage.cache_read),
            ("cache_creation_input_tokens", context.usage.cache_write),
        ] {
            if let Some(n) = count {
                usage.insert(key.to_owned(), n.into());
            }
        }
        obj.insert("context_usage".to_owned(), usage.into());
    }
    if let Some(model) = context.model {
        obj.entry("model".to_owned())
            .or_insert_with(|| model.into());
    }
}

/// Read at most `cap` bytes from the end of `path`. `None` on any I/O failure.
fn read_tail(path: &Path, cap: u64) -> Option<Vec<u8>> {
    use std::io::{Seek as _, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len > cap {
        f.seek(SeekFrom::Start(len.saturating_sub(cap))).ok()?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}
