//! Serde models for the `entire` data Rift reads.
//!
//! These are intentionally lenient: every struct uses `#[serde(default)]` and
//! captures unknown fields so that Rift keeps working when `entire` evolves its
//! schema. They cover only what Rift needs today (scanning, redaction targets,
//! and TUI metrics), not the full `entire` schema.

use serde::{Deserialize, Serialize};

/// Subset of `.entire/settings.json` Rift cares about.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub org_id: Option<String>,
    /// Agents that have been enabled in this repo (e.g. `claude-code`, `codex`).
    #[serde(default)]
    pub agents: Vec<String>,
    /// Anything else `entire` writes is preserved but not interpreted.
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// `metadata.json` at the root of a checkpoint shard.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CheckpointSummary {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub session_ids: Vec<String>,
    #[serde(default)]
    pub strategy: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub files_touched: Vec<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// `token_estimates.json` inside a checkpoint's session directory.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TokenEstimates {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_input_tokens: u64,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

/// Per-session state stored under `.git/entire-sessions/<id>.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SessionState {
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub current_session_id: Option<String>,
    #[serde(default)]
    pub current_agent: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_tolerates_unknown_fields() {
        let json = r#"{"project_id":"p1","future_field":42}"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.project_id.as_deref(), Some("p1"));
        assert!(s.extra.contains_key("future_field"));
    }

    #[test]
    fn token_estimates_default_to_zero() {
        let t: TokenEstimates = serde_json::from_str("{}").unwrap();
        assert_eq!(t.input_tokens, 0);
    }
}
