//! The store and hook every bridge test module starts from.

use rift_shadow_hooks::{AgentId, RawHook};
use rift_store::SqliteStore;

pub(super) fn store() -> SqliteStore {
    let s = SqliteStore::open_in_memory().unwrap();
    s.create_canonical_schema().unwrap();
    s
}

pub(super) fn raw(verb: &str, turn: u32) -> RawHook {
    RawHook {
        agent: AgentId::CLAUDE_CODE,
        verb: verb.into(),
        session: "01JSESSION".into(),
        turn: Some(turn),
        occurred_at_ms: 1,
        payload: serde_json::json!({"tool_name": "Bash"}),
    }
}
