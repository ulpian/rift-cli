//! What the window has already been told it may not have.
//!
//! Both questions here are asked of a journal that has already been read, so
//! nothing in this module opens a file. They are separate from the edit
//! arithmetic next door because a decline is a fact about one call, and an
//! edit is arithmetic about a file.

use rift_agent::window_journal::{DenyKey, Journal, Line};

/// Whether this window's last word on exactly these lines is a decline that
/// nothing has answered since.
///
/// The question a tool's *result* asks: a call the journal declined has no
/// result, so one arriving means the client ran the call anyway. Read from the
/// newest line back, and only since the window was last emptied — a decline
/// before an emptying is spent, and the read that follows it is a legitimate
/// first read of bytes the window no longer holds.
pub(crate) fn deny_stands(journal: &Journal, key: &DenyKey) -> bool {
    journal.live().iter().rev().find_map(|line| match line {
        Line::Deny {
            path, hash, range, ..
        }
        | Line::Outline {
            path, hash, range, ..
        } if key.matches(path, *hash, *range) => Some(true),
        Line::Read {
            path, hash, range, ..
        } if key.matches(path, *hash, *range) => Some(false),
        _ => None,
    }) == Some(true)
}

/// Declines a client did not act on: a decline, and then a read of exactly
/// those lines, with no emptying between them.
///
/// A client too old to understand a decision on the channel it was sent on
/// runs the call regardless, and the read that follows is how that shows. It
/// is not a failure of the decision — the bytes did reach the window and the
/// journal records that they did — but a count of them is the difference
/// between "the model asked again" and "the client never asked the model".
pub(crate) fn ineffective_denies(journal: &Journal) -> usize {
    let mut standing: Vec<DenyKey> = Vec::new();
    let mut ineffective = 0usize;
    for line in journal.lines() {
        match line {
            Line::Deny {
                path, hash, range, ..
            }
            | Line::Outline {
                path, hash, range, ..
            } => standing.push(DenyKey {
                path: path.clone(),
                hash: *hash,
                range: *range,
            }),
            Line::Read {
                path, hash, range, ..
            } => {
                if let Some(at) = standing
                    .iter()
                    .position(|key| key.matches(path, *hash, *range))
                {
                    standing.remove(at);
                    ineffective += 1;
                }
            }
            // An emptying spends every decline before it: the window no longer
            // holds what it was declined for, so the next read of those lines
            // is a first read and not a decision ignored.
            Line::Evict { .. } => standing.clear(),
            _ => {}
        }
    }
    ineffective
}
