//! The repo's `.mcp.json` agent MCP config — Rift's one entry in it (M5-06/13).
//!
//! `.mcp.json` is **user-owned working-tree config**, not a canonical table:
//! the recall lifecycle install is the single writer (parse → modify →
//! serialize preserving every other key, refusing an unparseable file), and
//! doctor is the warner when recall is On and the entry is absent. This module
//! owns the shared constants and the read side; the lifecycle write side lands
//! with the recall install (M5-13).

use std::path::Path;

/// The server key Rift provisions under `mcpServers`.
pub(crate) const RECALL_SERVER_KEY: &str = "rift-recall";

/// How the provisioning state of `.mcp.json` reads for a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum McpProvisionState {
    /// No `.mcp.json` at all (agents have no MCP config here).
    Missing,
    /// `.mcp.json` parses and carries our `mcpServers.rift-recall` entry.
    Provisioned,
    /// `.mcp.json` parses but has no rift entry.
    Absent,
    /// `.mcp.json` exists but is not valid JSON — never write over it.
    Unparseable,
}

impl McpProvisionState {
    /// One lowercase token for JSON output.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Provisioned => "provisioned",
            Self::Absent => "absent",
            Self::Unparseable => "unparseable",
        }
    }
}

/// Read the provisioning state of `<repo>/.mcp.json`.
pub(crate) fn provision_state(repo: &Path) -> McpProvisionState {
    let path = repo.join(".mcp.json");
    let Ok(bytes) = std::fs::read(&path) else {
        return McpProvisionState::Missing;
    };
    let Ok(doc) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return McpProvisionState::Unparseable;
    };
    let has_entry = doc
        .get("mcpServers")
        .and_then(|s| s.get(RECALL_SERVER_KEY))
        .is_some();
    if has_entry {
        McpProvisionState::Provisioned
    } else {
        McpProvisionState::Absent
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provision_states_read_honestly() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        assert_eq!(provision_state(repo), McpProvisionState::Missing);

        std::fs::write(repo.join(".mcp.json"), "{not json").unwrap();
        assert_eq!(provision_state(repo), McpProvisionState::Unparseable);

        std::fs::write(repo.join(".mcp.json"), r#"{"mcpServers":{}}"#).unwrap();
        assert_eq!(provision_state(repo), McpProvisionState::Absent);

        std::fs::write(
            repo.join(".mcp.json"),
            r#"{"mcpServers":{"rift-recall":{"command":"rift-memd"}}}"#,
        )
        .unwrap();
        assert_eq!(provision_state(repo), McpProvisionState::Provisioned);
    }
}
