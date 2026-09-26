//! What a Codex session's own record says about the client that ran it.
//!
//! Codex opens every session file with one `session_meta` line naming the
//! program that started the session and the Codex version inside it. Two
//! facts on the install side depend on exactly that: the remedy for a hook
//! Codex stopped trusting differs between the terminal and the VS Code
//! extension, and Codex's trust hash is only reproducible for versions whose
//! hashing Rift has been proven against. This module reads the line and
//! nothing else; the file is found and opened by the caller, because an
//! adapter has no filesystem authority.

/// The `originator` the VS Code extension stamps on the sessions it starts.
const VSCODE_ORIGINATOR: &str = "codex_vscode";

/// The client that started one Codex session, as its record names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionClient {
    /// The program that started the session (`codex_vscode`, `codex-tui`,
    /// `codex_exec`, …).
    pub originator: String,
    /// The Codex version the session ran, when the record carries one.
    pub cli_version: Option<String>,
}

impl SessionClient {
    /// Whether the VS Code extension started the session.
    ///
    /// Read from `originator` alone. The record's `source` field says
    /// `vscode` for the terminal interface too (a Codex 0.157.0 session
    /// started by `codex-tui` records it), so it cannot tell the two apart.
    #[must_use]
    pub fn is_vscode(&self) -> bool {
        self.originator == VSCODE_ORIGINATOR
    }
}

/// The client named by a session file's first line, or `None` when the line
/// is not a `session_meta` record with an `originator`.
#[must_use]
pub fn session_client(first_line: &str) -> Option<SessionClient> {
    let record: serde_json::Value = serde_json::from_str(first_line).ok()?;
    if record.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
        return None;
    }
    let payload = record.get("payload")?;
    let originator = payload.get("originator")?.as_str()?.to_owned();
    let cli_version = payload
        .get("cli_version")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned);
    Some(SessionClient {
        originator,
        cli_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(originator: &str, source: &str, version: &str) -> String {
        serde_json::json!({
            "type": "session_meta",
            "payload": {
                "originator": originator,
                "source": source,
                "cli_version": version,
                "cwd": "/repo",
            }
        })
        .to_string()
    }

    #[test]
    fn the_vscode_extension_is_named_by_its_originator() {
        let client = session_client(&meta("codex_vscode", "vscode", "0.155.0-alpha.16.3")).unwrap();
        assert!(client.is_vscode());
        assert_eq!(client.cli_version.as_deref(), Some("0.155.0-alpha.16.3"));
    }

    #[test]
    fn a_terminal_session_that_records_a_vscode_source_is_still_the_terminal() {
        let tui = session_client(&meta("codex-tui", "vscode", "0.157.0")).unwrap();
        assert!(!tui.is_vscode());
        let exec = session_client(&meta("codex_exec", "exec", "0.153.1")).unwrap();
        assert!(!exec.is_vscode());
        assert_eq!(exec.cli_version.as_deref(), Some("0.153.1"));
    }

    #[test]
    fn a_line_that_is_not_a_session_record_names_no_client() {
        assert_eq!(session_client("not json"), None);
        assert_eq!(
            session_client(r#"{"type":"response_item","payload":{"originator":"codex_vscode"}}"#),
            None
        );
        assert_eq!(
            session_client(r#"{"type":"session_meta","payload":{}}"#),
            None
        );
    }
}
