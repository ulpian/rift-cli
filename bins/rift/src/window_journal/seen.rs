//! The turn a window last read each revision it still holds.
//!
//! Asked by the pull channel, never by a hook: a `known_unchanged` entry is
//! the caller's own claim that it holds a revision, and this is the evidence
//! the answer may cite beside it. A separate module from [`denies`](super::denies)
//! and [`edits`](super::edits) because those feed a decision and this feeds
//! nothing but a label; like them it is asked of a journal already read, and
//! opens nothing.

use std::collections::BTreeMap;

use rift_agent::window_journal::{Journal, Line};

/// For each revision, keyed `(path, content sha256 hex)`, the latest turn the
/// window read it in since it was last emptied.
///
/// Only reads count: a line Rift announced, or a read it declined, put no
/// bytes in front of the model. Only reads since the last emptying count
/// either — an eviction means the window no longer holds those bytes, so a
/// turn from before it would label a claim with evidence that has lapsed, and
/// the conservative answer is no turn at all until the window reads the
/// revision again.
///
/// A folded history keeps one row per revision at the earliest turn of the
/// reads it merged, so after a fold the turn named here can be earlier than
/// the latest read; it is still a turn the window read exactly those bytes in,
/// which is all a citation claims.
pub(crate) fn last_read_turns(journal: &Journal) -> BTreeMap<(String, String), u64> {
    let mut turns = BTreeMap::new();
    for line in journal.live() {
        let Line::Read {
            turn, path, hash, ..
        } = line
        else {
            continue;
        };
        let latest = turns
            .entry((path.clone(), hash.to_hex()))
            .or_insert(u64::from(*turn));
        *latest = (*latest).max(u64::from(*turn));
    }
    turns
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::{EvictReason, Line};

    use super::super::tests::{hash, read};
    use super::*;

    fn turn_of(journal: &Journal, path: &str, revision: u8) -> Option<u64> {
        last_read_turns(journal)
            .get(&(path.to_owned(), hash(revision).to_hex()))
            .copied()
    }

    fn evict(turn: u32) -> Line {
        Line::Evict {
            turn,
            reason: EvictReason::Compact,
            at_ms: u64::from(turn),
        }
    }

    #[test]
    fn two_reads_of_one_revision_cite_the_later_turn() {
        let journal = Journal::from_lines(vec![
            read(3, "src/a.rs", 1, 1, 40),
            read(7, "src/a.rs", 1, 41, 80),
        ]);
        assert_eq!(turn_of(&journal, "src/a.rs", 1), Some(7));
    }

    #[test]
    fn a_later_read_of_another_revision_does_not_count_for_the_old_one() {
        let journal = Journal::from_lines(vec![
            read(3, "src/a.rs", 1, 1, 40),
            read(9, "src/a.rs", 2, 1, 40),
        ]);
        assert_eq!(turn_of(&journal, "src/a.rs", 1), Some(3));
        assert_eq!(turn_of(&journal, "src/a.rs", 2), Some(9));
    }

    #[test]
    fn a_read_is_cited_under_its_own_path_only() {
        let journal = Journal::from_lines(vec![read(3, "src/a.rs", 1, 1, 40)]);
        assert_eq!(turn_of(&journal, "src/b.rs", 1), None);
    }

    /// The window no longer holds bytes it read before an emptying, so the
    /// turn it read them in is not evidence of anything it holds now.
    #[test]
    fn an_eviction_after_the_read_leaves_no_turn_until_the_window_reads_again() {
        let mut journal = Journal::from_lines(vec![read(3, "src/a.rs", 1, 1, 40), evict(5)]);
        assert_eq!(turn_of(&journal, "src/a.rs", 1), None);
        journal.push(read(8, "src/a.rs", 1, 1, 40));
        assert_eq!(turn_of(&journal, "src/a.rs", 1), Some(8));
    }

    #[test]
    fn announced_and_declined_lines_are_not_reads() {
        let announced = Line::Hold {
            turn: 4,
            path: "src/a.rs".to_owned(),
            hash: hash(1),
            range: rift_agent::window_journal::Interval::new(1, 40).unwrap(),
            at_ms: 4,
        };
        let declined = Line::Deny {
            turn: 6,
            path: "src/a.rs".to_owned(),
            hash: hash(1),
            range: rift_agent::window_journal::Interval::new(1, 40).unwrap(),
            at_ms: 6,
        };
        let journal = Journal::from_lines(vec![announced, declined]);
        assert!(last_read_turns(&journal).is_empty());
    }
}
