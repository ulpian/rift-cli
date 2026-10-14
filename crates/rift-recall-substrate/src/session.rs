//! The out-of-tree session phase machine (`<sessions>/*.json`, the member's
//! sessions directory in the family's run state).
//!
//! The local-only, non-canonical half of the session view. This state lives
//! **outside** the working tree so file watchers never fire and capture never
//! blocks a commit. It is a *hint* for `status`/`doctor`, never a
//! canonical fact — the authoritative lifecycle is reconstructed from immutable
//! events at ingest. Reading is lenient (skip-and-report), bounded to the
//! sessions directory, and **never follows a symlink** out of it. Which
//! directory that is belongs to the caller, which can resolve the family;
//! this crate cannot.

use std::path::Path;

use serde::Deserialize;

/// A session lifecycle phase. Unknown strings degrade to [`Phase::Unknown`] for
/// forward-compat rather than failing the parse.
#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// Session opened.
    Started,
    /// Actively capturing turns.
    Capturing,
    /// Running the redaction pass.
    Redacting,
    /// Writing the durable checkpoint.
    Checkpointing,
    /// Archived (terminal).
    Archived,
    /// Unrecognized/forward-compat phase.
    #[serde(other)]
    #[default]
    Unknown,
}

/// One `<sessions>/<id>.json` file. Informational only.
#[derive(Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct SessionPhaseFile {
    /// The session id.
    pub session_id: String,
    /// Current phase (informational; reconciled against events at ingest).
    #[serde(default)]
    pub phase: Phase,
    /// The agent that produced the session, if recorded.
    #[serde(default)]
    pub agent: Option<String>,
    /// Base commit the session started from, if recorded.
    #[serde(default)]
    pub base_commit: Option<String>,
}

/// A skipped/unreadable session file (surfaced to `doctor`). No payload bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionIssue {
    /// The offending filename (not a full path; no payload).
    pub file: String,
    /// Position/category reason.
    pub detail: String,
}

/// Read every `<sessions>/*.json` phase file. Missing dir → empty.
/// Symlinks (and anything not a regular file) are skipped and reported — never
/// followed (prevents reading outside the sessions dir). Corrupt JSON is one
/// `SessionIssue`, not an abort.
#[must_use]
pub fn read_session_phases(sessions: &Path) -> (Vec<SessionPhaseFile>, Vec<SessionIssue>) {
    let mut ok = Vec::new();
    let mut issues = Vec::new();
    let Ok(entries) = std::fs::read_dir(sessions) else {
        return (ok, issues); // no sessions yet → empty, not error
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        // Regular files only — never follow a symlink out of the sessions dir.
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                issues.push(SessionIssue {
                    file: name,
                    detail: "symlink skipped (never followed)".to_string(),
                });
                continue;
            }
            Ok(meta) if !meta.is_file() => continue,
            Ok(_) => {}
            Err(_) => continue,
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        match serde_json::from_slice::<SessionPhaseFile>(&bytes) {
            Ok(v) => ok.push(v),
            Err(e) => issues.push(SessionIssue {
                file: name,
                detail: crate::parse::redact_err(&e),
            }),
        }
    }
    ok.sort_by(|a, b| a.session_id.cmp(&b.session_id));
    (ok, issues)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sessions_dir() -> (tempfile::TempDir, std::path::PathBuf) {
        let git_dir = tempfile::tempdir().unwrap();
        let sdir = git_dir.path().join("rift-sessions");
        std::fs::create_dir_all(&sdir).unwrap();
        let root = git_dir.path().to_path_buf();
        (git_dir, root)
    }

    #[test]
    fn missing_dir_yields_empty() {
        let d = tempfile::tempdir().unwrap();
        let (ok, issues) = read_session_phases(&d.path().join("sessions"));
        assert!(ok.is_empty() && issues.is_empty());
    }

    #[test]
    fn reads_and_sorts_phase_files() {
        let (_g, root) = sessions_dir();
        std::fs::write(
            root.join("rift-sessions/s2.json"),
            br#"{"session_id":"s2","phase":"capturing"}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("rift-sessions/s1.json"),
            br#"{"session_id":"s1","phase":"weird_future_phase"}"#,
        )
        .unwrap();
        let (ok, issues) = read_session_phases(&root.join("rift-sessions"));
        assert!(issues.is_empty());
        assert_eq!(ok.len(), 2);
        assert_eq!(ok[0].session_id, "s1");
        assert_eq!(
            ok[0].phase,
            Phase::Unknown,
            "unknown phase degrades, not errors"
        );
        assert_eq!(ok[1].phase, Phase::Capturing);
    }

    #[test]
    fn corrupt_file_is_skipped_and_reported() {
        let (_g, root) = sessions_dir();
        std::fs::write(root.join("rift-sessions/bad.json"), b"not json").unwrap();
        std::fs::write(
            root.join("rift-sessions/good.json"),
            br#"{"session_id":"ok"}"#,
        )
        .unwrap();
        let (ok, issues) = read_session_phases(&root.join("rift-sessions"));
        assert_eq!(ok.len(), 1);
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].file, "bad.json");
    }

    #[cfg(unix)]
    #[test]
    fn phase_reader_rejects_symlink_escape() {
        let (_g, root) = sessions_dir();
        // A secret file outside the sessions dir.
        let secret = root.join("secret.json");
        std::fs::write(&secret, br#"{"session_id":"LEAK"}"#).unwrap();
        // A symlink inside the sessions dir pointing at it.
        std::os::unix::fs::symlink(&secret, root.join("rift-sessions/eviltrap.json")).unwrap();
        let (ok, issues) = read_session_phases(&root.join("rift-sessions"));
        assert!(
            ok.iter().all(|s| s.session_id != "LEAK"),
            "symlinked file outside the dir must never be read"
        );
        assert!(issues.iter().any(|i| i.file == "eviltrap.json"));
    }
}
