//! Read checkpoints laid out in `entire`'s sharded directory structure.
//!
//! A [`CheckpointStore`] is rooted at a directory that contains the
//! `<id[:2]>/<id[2:]>/` shards — typically a checked-out worktree of the
//! `entire/checkpoints/v1` branch, or a fixture directory in tests. Resolving
//! and reading files for a given checkpoint is what the redaction targets and
//! the TUI metrics both build on.

use std::path::{Path, PathBuf};

use crate::model::{CheckpointSummary, TokenEstimates};
use crate::paths::{self, session_files};

/// Errors raised while reading the checkpoint store.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

/// A view over a directory holding `entire`-style checkpoint shards.
pub struct CheckpointStore {
    root: PathBuf,
}

impl CheckpointStore {
    #[must_use]
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Absolute path to a file within a checkpoint's session subdirectory.
    #[must_use]
    pub fn session_file(&self, id: &str, session_index: usize, file: &str) -> PathBuf {
        self.root.join(paths::session_file(id, session_index, file))
    }

    /// Read and parse a checkpoint's top-level `metadata.json`.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the file is missing or malformed.
    pub fn summary(&self, id: &str) -> Result<CheckpointSummary, StoreError> {
        let path = self.root.join(paths::shard_dir(id)).join("metadata.json");
        self.read_json(&path)
    }

    /// Read a session's `token_estimates.json`.
    ///
    /// # Errors
    /// Returns [`StoreError`] if the file is missing or malformed.
    pub fn token_estimates(
        &self,
        id: &str,
        session_index: usize,
    ) -> Result<TokenEstimates, StoreError> {
        let path = self.session_file(id, session_index, session_files::TOKEN_ESTIMATES);
        self.read_json(&path)
    }

    /// Discover every checkpoint id under the store root (shards that contain a
    /// `metadata.json`). Order is unspecified.
    ///
    /// # Errors
    /// Returns [`StoreError`] on an unreadable root directory.
    pub fn list_ids(&self) -> Result<Vec<String>, StoreError> {
        let mut ids = Vec::new();
        if !self.root.is_dir() {
            return Ok(ids);
        }
        for head in std::fs::read_dir(&self.root)? {
            let head = head?;
            if !head.file_type()?.is_dir() {
                continue;
            }
            let head_name = head.file_name();
            for tail in std::fs::read_dir(head.path())? {
                let tail = tail?;
                if tail.path().join("metadata.json").is_file() {
                    let id = format!(
                        "{}{}",
                        head_name.to_string_lossy(),
                        tail.file_name().to_string_lossy()
                    );
                    ids.push(id);
                }
            }
        }
        Ok(ids)
    }

    fn read_json<T: serde::de::DeserializeOwned>(&self, path: &Path) -> Result<T, StoreError> {
        let bytes = std::fs::read(path)?;
        serde_json::from_slice(&bytes).map_err(|source| StoreError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    #[test]
    fn lists_ids_and_reads_summary() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        write(
            &root.join("a3").join("b2c4").join("metadata.json"),
            r#"{"id":"a3b2c4","session_ids":["s1"]}"#,
        );
        let store = CheckpointStore::new(root);

        let ids = store.list_ids().unwrap();
        assert_eq!(ids, vec!["a3b2c4".to_string()]);

        let summary = store.summary("a3b2c4").unwrap();
        assert_eq!(summary.session_ids, vec!["s1".to_string()]);
    }

    #[test]
    fn missing_root_lists_nothing() {
        let store = CheckpointStore::new("/nonexistent/rift/store");
        assert!(store.list_ids().unwrap().is_empty());
    }
}
