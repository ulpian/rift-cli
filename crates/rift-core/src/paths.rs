//! Well-known paths and the `entire/checkpoints/v1` shard layout.

use std::path::{Path, PathBuf};

/// The orphan branch `entire` commits redacted checkpoint metadata to.
pub const CHECKPOINT_BRANCH: &str = "entire/checkpoints/v1";

/// Project-level, team-shared settings file inside a repo.
pub const SETTINGS_FILE: &str = ".entire/settings.json";

/// Personal, gitignored settings overrides.
pub const SETTINGS_LOCAL_FILE: &str = ".entire/settings.local.json";

/// Directory under `.git/` holding the per-session state machine.
pub const SESSIONS_DIR: &str = "entire-sessions";

/// Files that live inside a single checkpoint's per-session directory.
pub mod session_files {
    pub const TRANSCRIPT: &str = "full.jsonl";
    pub const PROMPT: &str = "prompt.txt";
    pub const CONTEXT: &str = "context.md";
    pub const TOOL_CALLS: &str = "tool_calls.jsonl";
    pub const TOKEN_ESTIMATES: &str = "token_estimates.json";
}

/// Build the sharded directory for a checkpoint id: `<id[:2]>/<id[2:]>/`.
///
/// Mirrors `entire`'s 256-way shard so cross-tool reads line up. Ids shorter
/// than two characters are placed under their full value with an empty tail.
#[must_use]
pub fn shard_dir(checkpoint_id: &str) -> PathBuf {
    let (head, tail) = checkpoint_id.split_at(checkpoint_id.len().min(2));
    Path::new(head).join(tail)
}

/// Path to a specific file within a checkpoint's session subdirectory.
#[must_use]
pub fn session_file(checkpoint_id: &str, session_index: usize, file: &str) -> PathBuf {
    shard_dir(checkpoint_id)
        .join(session_index.to_string())
        .join(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shards_split_after_two_chars() {
        assert_eq!(shard_dir("a3b2c4d5"), Path::new("a3").join("b2c4d5"));
    }

    #[test]
    fn shards_tolerate_short_ids() {
        assert_eq!(shard_dir("a"), Path::new("a").join(""));
    }

    #[test]
    fn session_file_joins_index_and_name() {
        let p = session_file("a3b2c4d5", 0, session_files::PROMPT);
        assert_eq!(
            p,
            Path::new("a3").join("b2c4d5").join("0").join("prompt.txt")
        );
    }
}
