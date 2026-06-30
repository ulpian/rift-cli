//! Minimal in-memory ingest: turn a checkpoint's artifacts into the file-read
//! facts the delta ledger measures (Stage 3 Phase 2 Step 2/5, scoped to the
//! metric slice — no projection DB).
//!
//! This is the glue that makes the substrate seam *observable*: it joins
//! `tool_calls.jsonl` (path@hash read facts) with `token_estimates` (input
//! tokens by `tool_use_id`) into [`FileRead`]s, which
//! [`rift_recall_ledger::RecallMetrics`] turns into `repeated`/`avoidable` rates
//! and the headline `tokens_avoided`.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_recall_ledger::FileRead;

use crate::parse::{self, ParseIssue};
use crate::SubstrateReader;

/// Extract the file-read facts from a single checkpoint's artifacts, plus any
/// parse issues encountered. Only read-shaped tool calls that carry both a path
/// and a content hash become `FileRead`s; tokens are taken inline or joined from
/// `token_estimates` by `tool_use_id` (defaulting to 0 when unknown).
///
/// # Errors
/// Propagates [`SubstrateReader::read_artifact`] read errors.
pub fn read_facts_for(
    reader: &impl SubstrateReader,
    checkpoint_id: &str,
) -> Result<(Vec<FileRead>, Vec<ParseIssue>)> {
    let mut issues = Vec::new();

    let tool_calls = match reader.read_artifact(checkpoint_id, "tool_calls.jsonl")? {
        Some(bytes) => {
            let out = parse::parse_tool_calls(&bytes, checkpoint_id);
            issues.extend(out.issues);
            out.value
        }
        None => Vec::new(),
    };
    let tokens = match reader.read_artifact(checkpoint_id, "token_estimates.json")? {
        Some(bytes) => {
            let out = parse::parse_token_estimates(&bytes, checkpoint_id);
            issues.extend(out.issues);
            out.value
        }
        None => Vec::new(),
    };
    let tok_by_id: BTreeMap<&str, u32> = tokens
        .iter()
        .map(|t| (t.tool_use_id.as_str(), t.input_tokens))
        .collect();

    let mut reads = Vec::new();
    for tc in &tool_calls {
        if !tc.is_read() {
            continue;
        }
        let (Some(path), Some(hash)) = (tc.path.as_ref(), tc.content_sha256.as_ref()) else {
            continue;
        };
        let input_tokens = tc
            .input_tokens
            .or_else(|| tok_by_id.get(tc.tool_use_id.as_str()).copied())
            .unwrap_or(0);
        reads.push(FileRead {
            path: path.clone(),
            content_sha256: hash.clone(),
            input_tokens,
        });
    }
    Ok((reads, issues))
}

/// Extract file-read facts across every checkpoint of a reader, in walk order.
///
/// # Errors
/// Propagates substrate read errors.
pub fn read_facts_all(reader: &impl SubstrateReader) -> Result<(Vec<FileRead>, Vec<ParseIssue>)> {
    let mut reads = Vec::new();
    let mut issues = Vec::new();
    for shard in reader.checkpoints()? {
        let (r, i) = read_facts_for(reader, &shard.checkpoint_id)?;
        reads.extend(r);
        issues.extend(i);
    }
    Ok((reads, issues))
}

/// The distinct repo-relative paths that appear in `reads` (the only paths the
/// worktree hasher needs to hash for the avoidable-context metric).
#[must_use]
pub fn read_paths(reads: &[FileRead]) -> std::collections::BTreeSet<String> {
    reads.iter().map(|r| r.path.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemorySubstrate;

    #[test]
    fn joins_tool_calls_with_token_estimates() {
        let tool_calls =
            b"{\"id\":\"t1\",\"name\":\"Read\",\"file_path\":\"a.rs\",\"sha256\":\"h1\"}\n";
        let tokens = b"{\"id\":\"t1\",\"input\":120}\n";
        let sub = MemorySubstrate::local()
            .with_artifact("ab12", "tool_calls.jsonl", tool_calls)
            .with_artifact("ab12", "token_estimates.json", tokens);
        let (reads, issues) = read_facts_for(&sub, "ab12").unwrap();
        assert!(issues.is_empty());
        assert_eq!(reads.len(), 1);
        assert_eq!(reads[0].path, "a.rs");
        assert_eq!(reads[0].content_sha256, "h1");
        assert_eq!(reads[0].input_tokens, 120);
    }

    #[test]
    fn non_read_and_incomplete_calls_are_dropped() {
        let tool_calls = concat!(
            r#"{"id":"t1","name":"Bash","file_path":"a.rs","sha256":"h1"}"#,
            "\n",
            r#"{"id":"t2","name":"Read","file_path":"b.rs"}"#, // no hash → dropped
            "\n",
        )
        .as_bytes();
        let sub = MemorySubstrate::local().with_artifact("ab12", "tool_calls.jsonl", tool_calls);
        let (reads, _) = read_facts_for(&sub, "ab12").unwrap();
        assert!(reads.is_empty());
    }
}
