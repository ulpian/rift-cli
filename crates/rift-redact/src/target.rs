//! Applying the pipeline to `entire`'s captured files.
//!
//! Per the project's decision, redaction touches **working-tree copies only**
//! and writes atomically (temp file + rename). `.jsonl` files are redacted
//! field-by-field so their structure is preserved; everything else is redacted
//! as a whole-text buffer.

use std::path::{Path, PathBuf};

use rift_core::checkpoint_store::CheckpointStore;
use rift_core::paths::session_files;
use serde_json::Value;

use crate::pipeline::{Masker, Pipeline};

/// The standard text files Rift redacts within a checkpoint session directory,
/// in a stable order. Paths may not exist; callers should skip missing ones.
#[must_use]
pub fn checkpoint_text_targets(
    store: &CheckpointStore,
    checkpoint_id: &str,
    session_index: usize,
) -> Vec<PathBuf> {
    [
        session_files::PROMPT,
        session_files::TRANSCRIPT,
        session_files::TOOL_CALLS,
    ]
    .into_iter()
    .map(|f| store.session_file(checkpoint_id, session_index, f))
    .collect()
}

/// Errors while redacting a target file.
#[derive(Debug, thiserror::Error)]
pub enum TargetError {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON on line {line} of {path}: {source}")]
    Json {
        path: PathBuf,
        line: usize,
        #[source]
        source: serde_json::Error,
    },
}

/// Result of redacting one file.
#[derive(Debug, Clone)]
pub struct RedactionOutcome {
    pub path: PathBuf,
    /// Number of findings the pipeline produced.
    pub findings: usize,
    /// Whether the file's contents were rewritten (false in dry-run or no-op).
    pub written: bool,
}

/// Redact a single file, dispatching on extension. With `dry_run`, findings are
/// counted but nothing is written.
///
/// # Errors
/// Returns [`TargetError`] on IO failure or malformed JSONL.
pub fn redact_file(
    pipeline: &Pipeline,
    masker: &Masker,
    path: &Path,
    dry_run: bool,
) -> Result<RedactionOutcome, TargetError> {
    let original = std::fs::read_to_string(path).map_err(|source| TargetError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    let is_jsonl = path.extension().is_some_and(|e| e == "jsonl");
    let (redacted, findings) = if is_jsonl {
        redact_jsonl(pipeline, masker, &original, path)?
    } else {
        let (out, fs) = pipeline.redact(&original, masker);
        (out, fs.len())
    };

    let mut written = false;
    if !dry_run && redacted != original {
        atomic_write(path, &redacted)?;
        written = true;
    }

    Ok(RedactionOutcome {
        path: path.to_path_buf(),
        findings,
        written,
    })
}

/// Redact each line of a JSONL document, walking string fields only so the JSON
/// structure stays intact. Returns the rebuilt document and total finding count.
fn redact_jsonl(
    pipeline: &Pipeline,
    masker: &Masker,
    input: &str,
    path: &Path,
) -> Result<(String, usize), TargetError> {
    let mut out = String::with_capacity(input.len());
    let mut total = 0usize;

    for (idx, line) in input.lines().enumerate() {
        if line.trim().is_empty() {
            out.push('\n');
            continue;
        }
        let mut value: Value = serde_json::from_str(line).map_err(|source| TargetError::Json {
            path: path.to_path_buf(),
            line: idx + 1,
            source,
        })?;
        redact_value(pipeline, masker, &mut value, &mut total);
        out.push_str(&value.to_string());
        out.push('\n');
    }
    Ok((out, total))
}

/// Recursively redact every string leaf in a JSON value.
fn redact_value(pipeline: &Pipeline, masker: &Masker, value: &mut Value, total: &mut usize) {
    match value {
        Value::String(s) => {
            let (redacted, findings) = pipeline.redact(s, masker);
            if !findings.is_empty() {
                *s = redacted;
                *total += findings.len();
            }
        }
        Value::Array(items) => {
            for item in items {
                redact_value(pipeline, masker, item, total);
            }
        }
        Value::Object(map) => {
            for v in map.values_mut() {
                redact_value(pipeline, masker, v, total);
            }
        }
        _ => {}
    }
}

/// Write `contents` to `path` atomically via a sibling temp file + rename.
fn atomic_write(path: &Path, contents: &str) -> Result<(), TargetError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "redacted".to_string());
    let tmp = dir.join(format!(".{file_name}.rift-tmp"));

    std::fs::write(&tmp, contents).map_err(|source| TargetError::Io {
        path: tmp.clone(),
        source,
    })?;
    std::fs::rename(&tmp, path).map_err(|source| TargetError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_prompt_text_and_writes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prompt.txt");
        std::fs::write(&path, "use key AKIAIOSFODNN7EXAMPLE now").unwrap();

        let pipeline = Pipeline::builtin();
        let outcome = redact_file(&pipeline, &Masker::default(), &path, false).unwrap();

        assert!(outcome.written);
        assert!(outcome.findings >= 1);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("[REDACTED]"));
        assert!(!after.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn dry_run_does_not_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prompt.txt");
        std::fs::write(&path, "AKIAIOSFODNN7EXAMPLE").unwrap();

        let pipeline = Pipeline::builtin();
        let outcome = redact_file(&pipeline, &Masker::default(), &path, true).unwrap();

        assert!(!outcome.written);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "AKIAIOSFODNN7EXAMPLE"
        );
    }

    #[test]
    fn redacts_jsonl_fields_preserving_structure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("full.jsonl");
        std::fs::write(
            &path,
            "{\"role\":\"user\",\"text\":\"my key AKIAIOSFODNN7EXAMPLE\"}\n",
        )
        .unwrap();

        let pipeline = Pipeline::builtin();
        let outcome = redact_file(&pipeline, &Masker::default(), &path, false).unwrap();
        assert!(outcome.written);

        let after = std::fs::read_to_string(&path).unwrap();
        // Still valid JSON, role intact, secret gone.
        let v: Value = serde_json::from_str(after.trim()).unwrap();
        assert_eq!(v["role"], "user");
        assert!(v["text"].as_str().unwrap().contains("[REDACTED]"));
    }
}
