//! The scripted sessions: the tracked prompt files a run plays.
//!
//! The prompts live in the tree beside this module so two developers who run
//! the harness play the same sessions.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The tracked directory the session prompts live in, relative to the root.
pub const SESSIONS_DIR: &str = "xtask/dogfood/sessions";

/// The first line of a session file: its one-line statement of intent.
const INTENT_OPEN: &str = "<!-- intent:";
/// How that line closes.
const INTENT_CLOSE: &str = "-->";

/// One scripted session: a file name, an intent, and the prompt itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// The two-digit ordinal from the file name.
    pub number: u32,
    /// The file's stem, which is also the log file's name.
    pub name: String,
    /// The one-line header saying what this session is shaped like.
    pub intent: String,
    /// The prompt handed to the client, verbatim.
    pub prompt: String,
}

/// Parse one session file.
///
/// # Errors
/// A file name that is not `NN-name.md`, a missing intent header, or an empty
/// prompt body — each of which would otherwise play as a session that measures
/// nothing.
pub fn parse_session(file_name: &str, text: &str) -> Result<Session, String> {
    let stem = file_name
        .strip_suffix(".md")
        .ok_or_else(|| format!("dogfood: {file_name}: a session file must end in .md"))?;
    let (digits, rest) = stem.split_at(stem.len().min(2));
    let number: u32 = digits
        .parse()
        .map_err(|_| format!("dogfood: {file_name}: must start with a two-digit ordinal"))?;
    if !rest.starts_with('-') || rest.len() < 2 {
        return Err(format!(
            "dogfood: {file_name}: the ordinal must be followed by `-` and a name"
        ));
    }
    let mut lines = text.lines();
    let header = lines
        .next()
        .unwrap_or_default()
        .trim()
        .strip_prefix(INTENT_OPEN)
        .and_then(|r| r.strip_suffix(INTENT_CLOSE))
        .map(str::trim)
        .ok_or_else(|| {
            format!("dogfood: {file_name}: the first line must be `{INTENT_OPEN} … {INTENT_CLOSE}`")
        })?;
    if header.is_empty() {
        return Err(format!("dogfood: {file_name}: the intent header is empty"));
    }
    let prompt = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    if prompt.is_empty() {
        return Err(format!("dogfood: {file_name}: the prompt body is empty"));
    }
    Ok(Session {
        number,
        name: stem.to_owned(),
        intent: header.to_owned(),
        prompt,
    })
}

/// Load every session in `dir`, ordered by file name.
///
/// # Errors
/// An unreadable directory, an unparsable file, or a duplicate ordinal.
pub fn load_sessions(dir: &Path) -> Result<Vec<Session>, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("dogfood: read {}: {e}", dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.sort();
    let mut out = Vec::with_capacity(files.len());
    let mut seen: BTreeMap<u32, String> = BTreeMap::new();
    for path in &files {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("dogfood: read {}: {e}", path.display()))?;
        let session = parse_session(&name, &text)?;
        if let Some(first) = seen.insert(session.number, name.clone()) {
            return Err(format!(
                "dogfood: {name}: ordinal {} is already used by {first}",
                session.number
            ));
        }
        out.push(session);
    }
    if out.is_empty() {
        return Err(format!(
            "dogfood: no session files in {} — the harness would measure nothing",
            dir.display()
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace_root;

    /// Every session that ships parses, and the set is the one the command
    /// promises: ten, numbered from one, each with an intent of its own.
    #[test]
    fn the_shipped_sessions_load() {
        let sessions = load_sessions(&workspace_root().join(SESSIONS_DIR)).unwrap();
        assert_eq!(sessions.len(), 10, "ten scripted sessions ship");
        for (at, s) in sessions.iter().enumerate() {
            assert_eq!(s.number as usize, at + 1, "{} is out of order", s.name);
            assert!(!s.intent.is_empty());
            assert!(s.prompt.len() > 40, "{} has a stub prompt", s.name);
        }
    }
    /// The prompts are ordinary work. Two things they must never do: ask the
    /// client to write history, or tell it what is being measured — a client
    /// told it is in a test stops behaving like a client.
    #[test]
    fn the_prompts_ask_for_ordinary_work_only() {
        let sessions = load_sessions(&workspace_root().join(SESSIONS_DIR)).unwrap();
        for s in &sessions {
            let lower = s.prompt.to_lowercase();
            for banned in ["commit", "push", " hook", "measure", "token"] {
                assert!(
                    !lower.contains(banned),
                    "{}: the prompt says `{banned}`",
                    s.name
                );
            }
        }
    }
    /// The whole shape of a session file, and each way it can be wrong.
    #[test]
    fn a_session_file_states_its_intent_and_its_prompt() {
        let ok = parse_session("03-a-name.md", "<!-- intent: read a file -->\nRead x.\n").unwrap();
        assert_eq!(ok.number, 3);
        assert_eq!(ok.name, "03-a-name");
        assert_eq!(ok.intent, "read a file");
        assert_eq!(ok.prompt, "Read x.");
        assert!(parse_session("03-a-name.txt", "x").is_err(), "not markdown");
        assert!(parse_session("x-name.md", "x").is_err(), "no ordinal");
        assert!(parse_session("03name.md", "x").is_err(), "no separator");
        assert!(
            parse_session("03-a.md", "Read x.\n").is_err(),
            "no intent header"
        );
        assert!(
            parse_session("03-a.md", "<!-- intent: x -->\n\n").is_err(),
            "no prompt"
        );
        assert!(
            parse_session("03-a.md", "<!-- intent:  -->\nRead x.").is_err(),
            "empty intent"
        );
    }
}
