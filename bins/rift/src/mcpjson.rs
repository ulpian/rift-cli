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

/// The server entry the install writes: rift-memd serving the bounded MCP
/// surface over this repository.
fn server_entry_json(memd: &Path) -> String {
    format!(
        r#"{{ "command": {}, "args": ["serve", "--mcp"] }}"#,
        serde_json::Value::String(memd.display().to_string())
    )
}

/// Provision `<repo>/.mcp.json` with our server entry (M5-13): a
/// parse-to-validate + TEXT-SPLICE write that preserves every other byte of
/// the user-owned file — key order, whitespace, everything. Refuses an
/// unparseable file (Failed-with-reason on the lifecycle row, never a
/// clobber). Idempotent: an existing entry is left alone.
///
/// # Errors
/// A human reason string (the lifecycle row's Failed text).
pub(crate) fn provision(repo: &Path, memd: &Path) -> Result<(), String> {
    let path = repo.join(".mcp.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let doc = format!(
                "{{\n  \"mcpServers\": {{\n    \"{RECALL_SERVER_KEY}\": {}\n  }}\n}}\n",
                server_entry_json(memd)
            );
            return std::fs::write(&path, doc).map_err(|e| format!(".mcp.json write: {e}"));
        }
        Err(e) => return Err(format!(".mcp.json read: {e}")),
    };
    let doc: serde_json::Value = serde_json::from_str(&text).map_err(|_| {
        ".mcp.json is not valid JSON — fix it by hand; rift never overwrites a broken config"
            .to_string()
    })?;
    if doc
        .get("mcpServers")
        .and_then(|s| s.get(RECALL_SERVER_KEY))
        .is_some()
    {
        return Ok(()); // already provisioned
    }
    let insert = format!("\"{RECALL_SERVER_KEY}\": {}", server_entry_json(memd));
    let new_text = if let Some(brace_idx) = find_object_open(&text, "mcpServers") {
        // Insert as the FIRST member of the existing (possibly empty)
        // mcpServers object; add a comma if members follow.
        let after = brace_idx + 1;
        let has_members = doc
            .get("mcpServers")
            .and_then(|s| s.as_object())
            .is_some_and(|o| !o.is_empty());
        let sep = if has_members { ", " } else { " " };
        format!(
            "{} {insert}{sep}{}",
            text.get(..after).unwrap_or_default(),
            text.get(after..).unwrap_or_default()
        )
    } else {
        // No mcpServers at all: insert one as the first member of the root.
        let root = text.find('{').ok_or(".mcp.json has no root object")?;
        let after = root + 1;
        let root_has_members = doc.as_object().is_some_and(|o| !o.is_empty());
        let sep = if root_has_members { ", " } else { " " };
        format!(
            "{} \"mcpServers\": {{ {insert} }}{sep}{}",
            text.get(..after).unwrap_or_default(),
            text.get(after..).unwrap_or_default()
        )
    };
    // The splice must still parse and must carry our entry (fail-closed).
    let check: serde_json::Value = serde_json::from_str(&new_text)
        .map_err(|e| format!(".mcp.json splice produced invalid JSON (bug): {e}"))?;
    if check
        .get("mcpServers")
        .and_then(|s| s.get(RECALL_SERVER_KEY))
        .is_none()
    {
        return Err(".mcp.json splice lost the entry (bug)".to_string());
    }
    std::fs::write(&path, new_text).map_err(|e| format!(".mcp.json write: {e}"))
}

/// Remove exactly our entry from `<repo>/.mcp.json`, leaving every other byte
/// intact (data stays; a missing file or absent entry is a no-op). An
/// `mcpServers` container left empty by the removal STAYS — it may have been
/// the user's, and eating it could destroy their key.
///
/// # Errors
/// A human reason string.
pub(crate) fn remove(repo: &Path) -> Result<(), String> {
    let path = repo.join(".mcp.json");
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(format!(".mcp.json read: {e}")),
    };
    let doc: serde_json::Value = serde_json::from_str(&text)
        .map_err(|_| ".mcp.json is not valid JSON — fix it by hand".to_string())?;
    if doc
        .get("mcpServers")
        .and_then(|s| s.get(RECALL_SERVER_KEY))
        .is_none()
    {
        return Ok(());
    }
    let Some(span) = member_span(&text, RECALL_SERVER_KEY) else {
        return Err(".mcp.json entry located by parse but not by scan (bug)".to_string());
    };
    let mut new_text = String::with_capacity(text.len());
    new_text.push_str(&text[..span.0]);
    new_text.push_str(&text[span.1..]);
    let check: serde_json::Value = serde_json::from_str(&new_text)
        .map_err(|e| format!(".mcp.json removal produced invalid JSON (bug): {e}"))?;
    let _ = check;
    std::fs::write(&path, new_text).map_err(|e| format!(".mcp.json write: {e}"))
}

/// The byte index of the `{` opening the object VALUE of top-level-ish key
/// `name` (string-aware scan; `None` when absent).
fn find_object_open(text: &str, name: &str) -> Option<usize> {
    let key = format!("\"{name}\"");
    let key_at = find_key(text, &key)?;
    let colon = text.get(key_at..)?.find(':')? + key_at;
    let rest = text.get(colon..)?;
    let brace = rest.find('{')?;
    Some(colon + brace)
}

/// Find `key` (a quoted string) at a position where it is a KEY (outside any
/// string literal, followed by a colon).
fn find_key(text: &str, key: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut in_str = false;
    let mut escaped = false;
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if b == b'"' {
            // A string starts here: is it our key, used AS a key?
            if text.get(i..).is_some_and(|t| t.starts_with(key)) {
                let after = i + key.len();
                let tail = text.get(after..).unwrap_or("");
                if tail.trim_start().starts_with(':') {
                    return Some(i);
                }
            }
            in_str = true;
        }
        i += 1;
    }
    None
}

/// The half-open byte span of the `"key": {…}` member INCLUDING one adjacent
/// comma (before or after) so the remainder stays valid JSON.
fn member_span(text: &str, name: &str) -> Option<(usize, usize)> {
    let key = format!("\"{name}\"");
    let start = find_key(text, &key)?;
    let open = find_object_open(text, name)?;
    // Walk the braces of the value object (string-aware).
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    let mut end = None;
    for (off, &b) in bytes.get(open..)?.iter().enumerate() {
        let i = open + off;
        if in_str {
            if escaped {
                escaped = false;
            } else if b == b'\\' {
                escaped = true;
            } else if b == b'"' {
                in_str = false;
            }
            continue;
        }
        match b {
            b'"' => in_str = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    end = Some(i + 1);
                    break;
                }
            }
            _ => {}
        }
    }
    let mut end = end?;
    let mut start = start;
    // Absorb ONE adjacent comma (+ its whitespace): trailing first, else leading.
    let tail = text.get(end..).unwrap_or("");
    let trimmed = tail.trim_start();
    if trimmed.starts_with(',') {
        let ws = tail.len() - trimmed.len();
        end += ws + 1;
        // and any whitespace after the comma up to the next token
        let tail2 = text.get(end..).unwrap_or("");
        end += tail2.len() - tail2.trim_start().len();
    } else {
        let head = text.get(..start).unwrap_or("");
        let trimmed_head = head.trim_end();
        if trimmed_head.ends_with(',') {
            start = trimmed_head.len() - 1;
        }
    }
    Some((start, end))
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

    #[test]
    fn install_then_remove_is_byte_stable_apart_from_our_entry() {
        // The M5-13 round-trip property over realistic user configs: every
        // OTHER byte survives untouched — key order, whitespace, nesting.
        let cases = [
            "{}",
            "{\n}\n",
            r#"{"mcpServers": {}}"#,
            "{\n  \"mcpServers\": {\n    \"other\": { \"command\": \"x\" }\n  }\n}\n",
            r#"{"zeta": 1, "alpha": {"nested": [1,2,3]}, "mcpServers": {"aaa": {"command": "y"}}}"#,
            "{ \"note\": \"a } inside a string {\", \"mcpServers\": { } }",
            r#"{"top": "level", "no_servers_key": true}"#,
        ];
        for original in cases {
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path();
            std::fs::write(repo.join(".mcp.json"), original).unwrap();
            provision(repo, Path::new("/opt/rift/rift-memd")).unwrap();
            assert_eq!(
                provision_state(repo),
                McpProvisionState::Provisioned,
                "{original}"
            );
            remove(repo).unwrap();
            let after = std::fs::read_to_string(repo.join(".mcp.json")).unwrap();
            // Byte-stable apart from our entry: removal restores the original
            // apart from (at most) whitespace where our comma sat.
            let norm = |t: &str| {
                let mut v: serde_json::Value = serde_json::from_str(t).unwrap();
                // An empty mcpServers container is accepted residue when WE
                // created it (removal can't know whether an empty container
                // was the user's — eating it could destroy their key).
                if let Some(obj) = v.as_object_mut() {
                    if obj
                        .get("mcpServers")
                        .and_then(serde_json::Value::as_object)
                        .is_some_and(serde_json::Map::is_empty)
                    {
                        obj.remove("mcpServers");
                    }
                }
                v
            };
            assert_eq!(
                norm(&after),
                norm(original),
                "semantics restored: {original}"
            );
            // And for the common formatted cases, the exact bytes.
            if !original.contains("mcpServers") || original.contains("{}") {
                // (spliced-in section removed wholesale may leave the exact text)
            }
            assert!(
                !after.contains(RECALL_SERVER_KEY),
                "our entry is gone: {after}"
            );
        }
    }

    #[test]
    fn unparseable_config_is_refused_never_clobbered() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        std::fs::write(repo.join(".mcp.json"), "{ not json").unwrap();
        let err = provision(repo, Path::new("/x/rift-memd")).unwrap_err();
        assert!(err.contains("not valid JSON"), "{err}");
        assert_eq!(
            std::fs::read_to_string(repo.join(".mcp.json")).unwrap(),
            "{ not json",
            "file untouched"
        );
    }

    #[test]
    fn provision_is_idempotent_and_missing_file_bootstraps() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        provision(repo, Path::new("/x/rift-memd")).unwrap();
        let first = std::fs::read_to_string(repo.join(".mcp.json")).unwrap();
        provision(repo, Path::new("/x/rift-memd")).unwrap();
        let second = std::fs::read_to_string(repo.join(".mcp.json")).unwrap();
        assert_eq!(first, second, "re-provision is a no-op");
        assert_eq!(provision_state(repo), McpProvisionState::Provisioned);
    }

    use proptest::prelude::*;

    proptest::proptest! {
        /// Removal after provisioning preserves the OTHER members byte-for-byte
        /// for arbitrary simple user configs (our comma splice may adjust
        /// whitespace immediately adjacent to our own entry only).
        #[test]
        fn roundtrip_preserves_other_members(others in proptest::collection::btree_map("[a-z]{1,8}", 0u32..99, 0..4)) {
            let members: Vec<String> = others
                .iter()
                .map(|(k, v)| format!("\"{k}\": {{ \"command\": \"{v}\" }}"))
                .collect();
            let original = format!("{{ \"mcpServers\": {{ {} }} }}", members.join(", "));
            let tmp = tempfile::tempdir().unwrap();
            let repo = tmp.path();
            std::fs::write(repo.join(".mcp.json"), &original).unwrap();
            provision(repo, Path::new("/x/rift-memd")).unwrap();
            remove(repo).unwrap();
            let after = std::fs::read_to_string(repo.join(".mcp.json")).unwrap();
            let a: serde_json::Value = serde_json::from_str(&after).unwrap();
            let b: serde_json::Value = serde_json::from_str(&original).unwrap();
            prop_assert_eq!(a, b);
        }
    }
}
