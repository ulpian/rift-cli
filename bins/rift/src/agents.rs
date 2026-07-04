//! `rift enable/disable --agent <x>` (P8 S3): wire an agent's hooks to
//! `rift hooks <agent> <verb>` and record the enable/disable as an append-only
//! event.
//!
//! Wiring is **config, not code injection**: `enable` writes a rift-owned wiring
//! descriptor under `.rift/agents/<agent>.json` naming the **absolute** `rift`
//! binary and the exact `rift hooks <agent> <verb>` command for each native verb
//! (mirroring the git-hook absolute-path discipline, P3 S1). It is idempotent and
//! reversible (`disable` removes the descriptor), dry-run-verified (a synthetic
//! event must map through the adapter before success), and **never** names the
//! upstream backend.

use std::path::PathBuf;

use rift_core::{AgentId, Result, RiftError};
use rift_hashchain::leaf_hash;
use rift_shadow::hooks::{dispatch, AdapterRegistry, RawHook};
use rift_store::{record_agent_enablement, AgentEnablementRow, SqliteStore};

use crate::cli::{exit, Outcome};

/// `.rift/agents/<agent>.json` under the current repo.
fn descriptor_path(agent: &str) -> Result<PathBuf> {
    Ok(std::env::current_dir()?
        .join(".rift")
        .join("agents")
        .join(format!("{agent}.json")))
}

/// Hex content fingerprint over `bytes` (domain-separated leaf hash, hex).
fn fingerprint(bytes: &[u8]) -> String {
    leaf_hash(bytes).to_hex()
}

/// `rift enable --agent <x>`.
pub(crate) async fn enable(store: &SqliteStore, agent_token: &str, now_ms: i64) -> Result<Outcome> {
    let agent = AgentId::new(agent_token);
    if !agent.is_well_formed() {
        return Ok(Outcome::coded(
            exit::USAGE,
            format!("invalid agent token {agent_token:?}"),
            serde_json::json!({ "error": "invalid_agent", "agent": agent_token }),
        ));
    }
    let reg = AdapterRegistry::with_builtin();
    let Some(adapter) = reg.get(&agent) else {
        let known: Vec<String> = reg.agents().iter().map(|a| a.as_str().to_owned()).collect();
        // A precondition, not a generic failure: the command is well-formed but
        // no built-in adapter exists for this agent (exit contract, P8 S1).
        return Ok(Outcome::coded(
            exit::PRECONDITION,
            format!(
                "no adapter for agent {agent_token:?}; known: {}",
                known.join(", ")
            ),
            serde_json::json!({ "error": "unknown_agent", "known": known }),
        ));
    };
    let verbs: Vec<&str> = adapter.verbs().to_vec();
    let notes = adapter.compat_notes();
    let rift_bin = std::env::current_exe()?;

    // The wiring descriptor: absolute rift path + the exact command per verb.
    let hooks: Vec<serde_json::Value> = verbs
        .iter()
        .map(|v| {
            serde_json::json!({
                "verb": v,
                "command": format!("{} hooks {agent_token} {v}", rift_bin.display()),
            })
        })
        .collect();
    let descriptor = serde_json::json!({
        "agent": agent_token,
        "rift_bin": rift_bin.display().to_string(),
        "verbs": verbs,
        "hooks": hooks,
        "compat": {
            "best_effort": notes.best_effort,
            "surfaces_token_usage": notes.surfaces_token_usage,
            "lifecycle_gaps": notes.lifecycle_gaps,
        },
    });
    let bytes = serde_json::to_vec_pretty(&descriptor).unwrap_or_default();
    let wired_hash = fingerprint(&bytes);

    // Dry-run: a synthetic event must map through the adapter before we declare success.
    verify_wiring(&reg, &agent, &verbs)?;

    // Write the descriptor (idempotent — same content re-writes identically).
    let path = descriptor_path(agent_token)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, &bytes)?;

    record_event(store, agent_token, "enable", now_ms, &wired_hash).await?;

    // Agent-native settings snippet (M2-15): for Claude Code, emit the exact
    // hooks block to paste into `.claude/settings.local.json` — plain
    // commands, no shim (the wire parser accepts native hook stdin directly).
    let snippet_path = if agent == AgentId::CLAUDE_CODE {
        let sp = descriptor_path(agent_token)?.with_file_name("claude-code.claude-settings.json");
        let snippet = claude_settings_snippet(&rift_bin);
        std::fs::write(&sp, serde_json::to_vec_pretty(&snippet).unwrap_or_default())?;
        Some(sp)
    } else {
        None
    };

    let note_line = if notes.best_effort {
        format!("\n  note: best-effort capture ({})", notes.note)
    } else {
        String::new()
    };
    let snippet_line = snippet_path.as_ref().map_or_else(String::new, |sp| {
        format!(
            "\n  paste into .claude/settings.local.json: {}",
            sp.display()
        )
    });
    let human = format!(
        "enabled {agent_token}: wired {} hook(s) via {}\n  config: {}{snippet_line}{note_line}",
        verbs.len(),
        rift_bin.display(),
        path.display(),
    );
    let json = serde_json::json!({
        "agent": agent_token,
        "wired_hooks": verbs,
        "config": path.display().to_string(),
        "wired_hooks_hash": wired_hash,
        "best_effort": notes.best_effort,
        "settings_snippet": snippet_path.map(|p| p.display().to_string()),
    });
    Ok(Outcome::ok(human, json))
}

/// The Claude Code hook events Rift captures, and whether the settings entry
/// takes a tool matcher. `SubagentStart` has no Claude Code hook event, so the
/// adapter verb list and this table deliberately differ by that one verb.
const CLAUDE_CODE_HOOK_EVENTS: &[(&str, bool)] = &[
    ("SessionStart", false),
    ("UserPromptSubmit", false),
    ("PreToolUse", true),
    ("PostToolUse", true),
    ("Stop", false),
    ("SubagentStop", false),
    ("PreCompact", false),
    ("SessionEnd", false),
];

/// The ready-to-paste `.claude/settings.local.json` hooks block: one plain
/// `rift hooks claude-code <event>` command per hook event (native stdin is
/// accepted directly — no jq shim, M2-15).
fn claude_settings_snippet(rift_bin: &std::path::Path) -> serde_json::Value {
    let mut hooks = serde_json::Map::new();
    for (event, has_matcher) in CLAUDE_CODE_HOOK_EVENTS {
        let cmd = format!("'{}' hooks claude-code {event}", rift_bin.display());
        let mut entry = serde_json::Map::new();
        if *has_matcher {
            entry.insert("matcher".to_owned(), "*".into());
        }
        entry.insert(
            "hooks".to_owned(),
            serde_json::json!([{ "type": "command", "command": cmd }]),
        );
        hooks.insert((*event).to_owned(), serde_json::json!([entry]));
    }
    serde_json::json!({ "hooks": hooks })
}

/// `rift disable --agent <x>`: remove the wiring descriptor (full round-trip).
pub(crate) async fn disable(
    store: &SqliteStore,
    agent_token: &str,
    now_ms: i64,
) -> Result<Outcome> {
    let path = descriptor_path(agent_token)?;
    let existed = path.exists();
    if existed {
        std::fs::remove_file(&path)?;
    }
    let wired_hash = fingerprint(b"disabled");
    record_event(store, agent_token, "disable", now_ms, &wired_hash).await?;
    let human = if existed {
        format!("disabled {agent_token}: removed {}", path.display())
    } else {
        format!("{agent_token} was not enabled")
    };
    Ok(Outcome::ok(
        human,
        serde_json::json!({ "agent": agent_token, "disabled": existed }),
    ))
}

/// Dry-run the wiring: a synthetic event for the adapter's first verb must map
/// through `dispatch` and produce at least one canonical event.
fn verify_wiring(reg: &AdapterRegistry, agent: &AgentId, verbs: &[&str]) -> Result<()> {
    let verb = verbs.first().copied().unwrap_or("session_start");
    let raw = RawHook {
        agent: agent.clone(),
        verb: verb.to_owned(),
        session: "dry-run".to_owned(),
        turn: Some(0),
        occurred_at_ms: 0,
        payload: serde_json::json!({}),
    };
    let events = dispatch(&raw, reg)?;
    if events.is_empty() {
        return Err(RiftError::validity(
            "wiring dry-run produced no canonical events",
        ));
    }
    Ok(())
}

/// Record an enable/disable event (append-only, idempotent on its content address).
async fn record_event(
    store: &SqliteStore,
    agent: &str,
    action: &str,
    changed_at: i64,
    wired_hash: &str,
) -> Result<()> {
    let mut key = Vec::new();
    key.extend_from_slice(agent.as_bytes());
    key.push(0);
    key.extend_from_slice(action.as_bytes());
    key.push(0);
    key.extend_from_slice(changed_at.to_string().as_bytes());
    key.push(0);
    key.extend_from_slice(wired_hash.as_bytes());
    let change_id = fingerprint(&key);
    record_agent_enablement(
        store,
        &AgentEnablementRow {
            change_id,
            agent: agent.to_owned(),
            action: action.to_owned(),
            changed_at,
            wired_hooks_hash: wired_hash.to_owned(),
            schema_version: 1,
        },
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_settings_snippet_wires_plain_commands_no_shim() {
        let s = claude_settings_snippet(std::path::Path::new("/opt/rift"));
        let hooks = s["hooks"].as_object().unwrap();
        assert_eq!(hooks.len(), CLAUDE_CODE_HOOK_EVENTS.len());
        assert_eq!(
            hooks["Stop"][0]["hooks"][0]["command"],
            "'/opt/rift' hooks claude-code Stop"
        );
        assert_eq!(hooks["PreToolUse"][0]["matcher"], "*");
        assert!(hooks["SessionStart"][0].get("matcher").is_none());
        assert!(
            !s.to_string().contains("jq"),
            "native fallback retires the shim (M2-15)"
        );
        // SubagentStart has no Claude Code hook event — never emitted.
        assert!(hooks.get("SubagentStart").is_none());
    }
}
