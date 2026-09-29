//! The tracked form of one run: the table and the document, each carrying the
//! provenance a later reader needs to know what they are comparing.
//!
//! A figure copied out of a terminal loses the build it came from, the corpus
//! it was taken over and the moment it was taken, and a before and an after
//! are only comparable when all three are known. The artifact is written with
//! them attached, and with nothing else from the corpus: aggregates only, like
//! every surface of this command.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::history::utc_stamp;
use super::render::{document, render_table};
use super::report::Report;

/// What one run was, apart from what it measured.
///
/// Every field is handed in: the command's edge reads the clock and asks git,
/// and a test injects both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Provenance {
    /// `git describe --always --dirty` of the source tree the command was
    /// built from.
    pub describe: String,
    /// The short id of that tree's `HEAD`.
    pub head: String,
    /// How many files the transcripts directory held, sidecars and spilled
    /// results included: the corpus described by its size, never its path.
    pub corpus_files: u64,
    /// The refs the history was read from, empty when the facts came from the
    /// worktree.
    pub history_refs: Vec<String>,
    /// When the run started, in seconds since the Unix epoch.
    pub started: i64,
    /// When it finished.
    pub finished: i64,
}

/// Write `replay-savings-<started>.md` and `.json` into `dir`, answering the
/// two paths written.
///
/// # Errors
/// If the directory cannot be made or a file cannot be written.
pub fn write_artifact(
    dir: &Path,
    report: &Report,
    provenance: &Provenance,
) -> Result<(PathBuf, PathBuf), String> {
    std::fs::create_dir_all(dir)
        .map_err(|e| format!("replay-savings: make {}: {e}", dir.display()))?;
    let stem = format!("replay-savings-{}", provenance.started);
    let table = dir.join(format!("{stem}.md"));
    let document = dir.join(format!("{stem}.json"));
    let write = |path: &Path, body: String| {
        std::fs::write(path, body)
            .map_err(|e| format!("replay-savings: write {}: {e}", path.display()))
    };
    write(&table, artifact_markdown(report, provenance))?;
    write(&document, artifact_json(report, provenance))?;
    Ok((table, document))
}

/// The table, under a provenance list.
fn artifact_markdown(report: &Report, provenance: &Provenance) -> String {
    let mut out = String::new();
    out.push_str("# replay-savings\n\n");
    for (name, value) in provenance_lines(report, provenance) {
        out.push_str(&format!("- {name}: {value}\n"));
    }
    out.push_str("\n```text\n");
    out.push_str(&render_table(report));
    out.push_str("```\n");
    out
}

/// The document, with its provenance under a key of its own.
fn artifact_json(report: &Report, provenance: &Provenance) -> String {
    let mut document = document(report);
    if let Some(map) = document.as_object_mut() {
        map.insert("provenance".to_owned(), provenance_json(report, provenance));
    }
    let mut text = serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_owned());
    text.push('\n');
    text
}

/// The provenance as the markdown list spells it.
fn provenance_lines(report: &Report, provenance: &Provenance) -> Vec<(&'static str, String)> {
    vec![
        (
            "build",
            format!("`{}` (HEAD `{}`)", provenance.describe, provenance.head),
        ),
        ("facts from", facts_from(provenance)),
        (
            "corpus",
            format!(
                "{} transcript files, {} session trees, {} context windows, {} tool calls",
                provenance.corpus_files, report.trees, report.windows, report.tool_calls
            ),
        ),
        ("tokenizer", report.tokenizer.to_owned()),
        ("started", stamp(provenance.started)),
        ("finished", stamp(provenance.finished)),
    ]
}

/// The provenance as the document spells it.
fn provenance_json(report: &Report, provenance: &Provenance) -> Value {
    json!({
        "build_describe": provenance.describe,
        "build_head": provenance.head,
        "corpus": {
            "context_windows": report.windows,
            "session_trees": report.trees,
            "tool_calls_replayed": report.tool_calls,
            "transcript_files": provenance.corpus_files,
        },
        "facts_from": if provenance.history_refs.is_empty() { "worktree" } else { "history" },
        "finished_unix_seconds": provenance.finished,
        "history_refs": provenance.history_refs,
        "started_unix_seconds": provenance.started,
        "tokenizer": report.tokenizer,
    })
}

/// Where the facts came from, with the refs when they came from history.
fn facts_from(provenance: &Provenance) -> String {
    if provenance.history_refs.is_empty() {
        return "worktree".to_owned();
    }
    format!(
        "history at each window's commit (refs: {})",
        provenance.history_refs.join(", ")
    )
}

/// A moment, as seconds and as a UTC stamp.
fn stamp(seconds: i64) -> String {
    format!("{} ({seconds})", utc_stamp(seconds))
}
