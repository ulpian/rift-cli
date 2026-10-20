//! One context window's call sidecar: the planned, emitted and applied
//! outcomes of the calls it declined, keyed by the client's own call id.
//!
//! A file of its own beside the window's journal, never rows in it. The
//! journal models what a context window holds — reads, edits, declines,
//! emptyings — and every gate decision folds it; a fact about whether a
//! response left the hook says nothing about what the window holds, and a
//! row of it in the journal is a row every decision and every reader of
//! that history would have to step over. The sidecar is written the same
//! store-free way: one bounded, atomic append per fact.

use std::path::{Path, PathBuf};

use rift_agent::window_journal::{Journal, Line};

use super::fold::absorb_file;
use super::journal::write_row;
use super::{journal_dir, key, MAX_APPEND_BYTES};

/// The suffix a sidecar's file name carries after its window's key.
pub(super) const CALLS_SUFFIX: &str = ".calls.jsonl";

/// One context window's call sidecar, as a place on disk.
pub(crate) struct CallSidecar {
    dir: PathBuf,
    key: String,
}

impl CallSidecar {
    /// The sidecar of one context window in one checkout, beside its
    /// journal. Opens nothing and creates nothing; the first append makes
    /// the directory.
    pub(crate) fn open(work_tree: &Path, session_id: &str, agent_id: Option<&str>) -> Self {
        Self::at(journal_dir(work_tree), key(session_id, agent_id))
    }

    /// The sidecar of the window `key` in the journal directory `dir`.
    pub(super) fn at(dir: PathBuf, key: String) -> Self {
        Self { dir, key }
    }

    /// The one file this sidecar is.
    pub(crate) fn path(&self) -> PathBuf {
        self.dir.join(format!("{}{CALLS_SUFFIX}", self.key))
    }

    /// Every call fact this window recorded, oldest first. A row that does
    /// not parse is dropped, which costs a pairing and nothing else.
    pub(crate) fn load(&self) -> Journal {
        let (mut lines, mut dropped) = (Vec::new(), 0usize);
        absorb_file(&self.path(), 0, None, &mut lines, &mut dropped);
        lines.retain(|line| matches!(line, Line::Call { .. }));
        Journal::from_lines(lines)
    }

    /// Append one call fact as one atomic write.
    ///
    /// Never folded: a row is written only for a declined call, so the file
    /// stays as short as the declines a window was given.
    pub(crate) fn append(&self, line: &Line) -> std::io::Result<()> {
        let mut row = line.to_json_line();
        row.push('\n');
        if row.len() > MAX_APPEND_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "a call row longer than one atomic append is refused",
            ));
        }
        std::fs::create_dir_all(&self.dir)?;
        write_row(&self.path(), &row)
    }
}
