//! The window journal on disk: one append-only file per context window, under
//! this checkout's `.rift/state/`.
//!
//! The decision — what a window has been handed, and what may be declined
//! because of it — is pure and lives in the agent SDK. This module is the
//! other half: where those lines are kept, how two processes may append to
//! them at the same time, and how a file that has grown long is folded into a
//! smaller equivalent one without a reader ever seeing a gap.
//!
//! **Local run state, never a record of anything.** Losing every file in this
//! directory costs a re-read that would have been declined. Nothing here is
//! joined to a canonical table, synced, or trusted to be well-formed: the
//! directory sits in a work tree an agent can write to, so every row is
//! validated on the way in and a row that fails is dropped rather than
//! interpreted.
//!
//! **Every failure leans one way.** A file that cannot be read, a snapshot
//! that will not parse, a row longer than one atomic write, a directory that
//! cannot be created — each ends as *less* coverage than really exists, which
//! costs a saving and can never produce a decline of lines the window does not
//! hold. The one direction that would be unsafe — keeping coverage while
//! losing the line that says the window was emptied — is why an unreadable
//! snapshot is read as an emptying rather than as nothing at all.
//!
//! ## The files
//!
//! ```text
//! .rift/state/reads/<key>.jsonl                 the live file: appended to
//! .rift/state/reads/<key>.<n>.rolled.jsonl      a live file that filled up
//! .rift/state/reads/<key>.<n>.snapshot.json     the state of everything ≤ n
//! ```
//!
//! `<key>` is one **context window**, not one session: a subagent runs under
//! its parent's session id and carries its own agent id, and the two windows
//! hold different bytes, so the key is the hash of both together.
//!
//! ## Appending, and two processes doing it at once
//!
//! A client may run its hooks in parallel and a model may issue parallel tool
//! calls, so two processes appending to one window's file is ordinary, not
//! exceptional. Each row is one `write` of at most [`MAX_APPEND_BYTES`] to a
//! file opened for append — atomic per call on POSIX below `PIPE_BUF` and on
//! Windows through `FILE_APPEND_DATA` — so rows never interleave. A reader
//! arriving mid-append sees a final row without its newline and drops it,
//! because a row that does not parse is a row that never happened.
//!
//! ## Reading a folded history
//!
//! A live file that fills up is *renamed* aside and a snapshot of the merged
//! state is written beside it; the writing half lands next. What a reader must
//! do with the result is here: the newest snapshot stands for every rolled
//! file below it, and each of those is read past the byte the snapshot
//! accounts for — which is where an append from a process still holding the
//! old file open lands, however many folds ago that file was rolled — while
//! rolled files above it belong to a fold that did not finish and are read
//! whole, and the live file comes last. Nothing is ever truncated, so no
//! concurrent append can be lost, and a snapshot that cannot be read is taken
//! as the window having been emptied rather than as nothing at all.
//!
//! Those bytes are measured **once**, immediately after the rename and before
//! a byte of any rolled file is read, and the fold reads no further than them.
//! That ordering is what makes "no concurrent append can be lost" true rather
//! than nearly true: an accounted byte measured after the reading would count
//! rows the snapshot had not taken in, and a row that landed in that gap would
//! be in neither place. The account names every rolled file rather than only
//! the newest, because a straggler writes into the file it opened for as long
//! as it lives, and a row it adds to a file two folds old has to stay
//! reachable too.
//!
//! ## The modules
//!
//! This module names the files — the directory, the key one context window
//! is spelled with, and the two caps a caller outside this plane has to
//! respect. [`journal`] is the place on disk: the append, the fold and the
//! walk that reads a history back. [`fold`] is the rows themselves, read and
//! condensed, and never touches a path. [`denies`] and [`edits`] are the
//! questions a decision asks of a journal already in memory. [`housekeeping`]
//! counts a checkout's journals and sweeps the ones it has finished with.
//!

mod denies;
mod edits;
mod fold;
mod housekeeping;
mod journal;

use std::path::{Path, PathBuf};

pub(crate) use denies::{deny_stands, ineffective_denies};
pub(crate) use edits::{edit_facts, expected_write, prior_revision};
pub(crate) use housekeeping::{survey, sweep_expired};
pub(crate) use journal::WindowJournal;

/// The largest row this will write in one call.
///
/// Atomicity is what keeps two appending processes from interleaving, and it
/// is a property of a *single* write of a bounded size. A row that would not
/// fit is refused rather than split, and a refused row is one line of
/// coverage the journal does not have. Ordinary rows are a few hundred bytes;
/// only a pathological path or a long list of edited sites comes near this.
pub(crate) const MAX_APPEND_BYTES: usize = 4_096;

/// The most an edit line will name before it names nothing.
///
/// A row is one atomic append of at most [`MAX_APPEND_BYTES`], and a list of
/// changed places long enough to threaten that is also a list no reader acts
/// on: "the file changed in thirty-three places" is "the file changed". Past
/// the cap the places are dropped and the change is recorded as one whose
/// places do not follow from its payload, which is where every other
/// underivable case ends too.
pub(crate) const MAX_EDIT_SITES: usize = 32;

/// Where one checkout keeps its window journals.
///
/// Per checkout by design: the bytes at a path differ between members of a
/// worktree family, and coverage is a claim about bytes. The directory is
/// under the ignored run-state root, so nothing here is ever committed.
pub(crate) fn dir(work_tree: &Path) -> PathBuf {
    work_tree.join(".rift").join("state").join("reads")
}

/// The file name of one context window: the session it belongs to and the
/// agent inside it, hashed together.
///
/// A subagent's hooks carry the parent's session id and their own agent id.
/// Keying by session alone would let one window's reads decline another
/// window's calls, which is the one mistake that costs an agent bytes it does
/// not hold — so the agent id is part of the key, and its absence (an ordinary
/// main-thread call) is a distinct, empty part rather than a missing one.
pub(crate) fn key(session_id: &str, agent_id: Option<&str>) -> String {
    let agent = agent_id.unwrap_or_default();
    let mut bytes = Vec::with_capacity(session_id.len() + 1 + agent.len());
    bytes.extend_from_slice(session_id.as_bytes());
    bytes.push(0);
    bytes.extend_from_slice(agent.as_bytes());
    rift_hashchain::leaf_hash(&bytes).to_hex()
}

/// A 64-character lowercase-hex key, and nothing else — the shape every file
/// this module owns is named with, and the screen every sweep applies before
/// it removes anything.
pub(super) fn is_key(s: &str) -> bool {
    s.len() == 64
        && s.chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::{Interval, Line};

    use super::*;

    /// A hash to hang a line on; the value is arbitrary, its shape is not.
    pub(super) fn hash(seed: u8) -> rift_core::Sha256Id {
        rift_hashchain::leaf_hash(&[seed])
    }

    pub(super) fn read(turn: u32, path: &str, revision: u8, lo: u32, hi: u32) -> Line {
        Line::Read {
            turn,
            path: path.to_owned(),
            hash: hash(revision),
            range: Interval::new(lo, hi).expect("a run"),
            total_lines: 400,
            at_ms: u64::from(turn),
        }
    }

    /// One context window is a session **and** the agent inside it. A subagent
    /// runs under its parent's session id, so a key that ignored the agent
    /// would let the parent's reads decline the child's calls — bytes the
    /// child was never handed.
    #[test]
    fn a_subagent_gets_its_own_journal_under_its_parents_session() {
        let parent = key("session-1", None);
        let child = key("session-1", Some("agent-7"));
        assert_ne!(parent, child);
        assert_eq!(parent, key("session-1", None), "the key is a function");
        assert_eq!(parent.len(), 64, "a key names a file");
        assert!(is_key(&parent) && is_key(&child));
        // The key is exactly the documented derivation, not merely some hash
        // of the two parts: session, a zero byte, then the agent or nothing.
        assert_eq!(
            child,
            rift_hashchain::leaf_hash(b"session-1\0agent-7").to_hex()
        );
        assert_eq!(parent, rift_hashchain::leaf_hash(b"session-1\0").to_hex());
    }
}
