//! What the window has already been told it may not have.
//!
//! Both questions here are asked of a journal that has already been read, so
//! nothing in this module opens a file. They are separate from the edit
//! arithmetic next door because a decline is a fact about one call, and an
//! edit is arithmetic about a file.

use std::collections::BTreeMap;

use rift_agent::window_journal::{CallFact, DenyKey, Journal, Line};

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

/// What became of the declines one window planned, joined call by call on
/// the id the client gave each call.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct DeclineCalls {
    /// Declines planned for a call that carried a native id.
    pub(crate) planned: usize,
    /// Of those, the ones whose response was written and flushed.
    pub(crate) emitted: usize,
    /// Of those, the ones whose response never reached stdout.
    pub(crate) unwritten: usize,
    /// Emitted declines the client ran the call in spite of.
    pub(crate) ineffective: usize,
    /// Declines planned for a call with no native id, which nothing pairs.
    pub(crate) uncorrelatable: usize,
}

impl DeclineCalls {
    /// Fold another window's counts into this one.
    pub(crate) fn add(&mut self, other: &Self) {
        self.planned += other.planned;
        self.emitted += other.emitted;
        self.unwritten += other.unwritten;
        self.ineffective += other.ineffective;
        self.uncorrelatable += other.uncorrelatable;
    }
}

/// Declines a client did not act on, keyed by the native call id: a planned
/// decline, a response written for it, and then a result for the same call.
///
/// Only the id joins the three facts. A result that merely names the same
/// lines is not counted, because a later call may read them for its own
/// reasons, and a call that carried no id is counted apart rather than paired
/// by time. A decline whose response was never written is not the client's
/// failure, so it is never counted as ignored.
pub(crate) fn ineffective_denies(journal: &Journal) -> DeclineCalls {
    let mut calls = DeclineCalls::default();
    // Per id: whether the response was written, and whether a result came.
    let mut seen: BTreeMap<&str, (Option<bool>, bool)> = BTreeMap::new();
    for line in journal.lines() {
        let Line::Call { call, fact, .. } = line else {
            continue;
        };
        match (call.as_deref(), fact) {
            (None, CallFact::Planned) => calls.uncorrelatable += 1,
            (None, _) => {}
            (Some(id), CallFact::Planned) => {
                seen.entry(id).or_default();
            }
            (Some(id), CallFact::Emitted { wrote, .. }) => {
                if let Some(entry) = seen.get_mut(id) {
                    entry.0 = Some(*wrote);
                }
            }
            (Some(id), CallFact::Applied) => {
                if let Some(entry) = seen.get_mut(id) {
                    entry.1 = true;
                }
            }
        }
    }
    calls.planned = seen.len();
    for (wrote, applied) in seen.values() {
        match wrote {
            Some(true) if *applied => {
                calls.emitted += 1;
                calls.ineffective += 1;
            }
            Some(true) => calls.emitted += 1,
            Some(false) => calls.unwritten += 1,
            None => {}
        }
    }
    calls
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::{CallFact, Journal, Line};

    use super::*;

    fn call(id: Option<&str>, fact: CallFact) -> Line {
        Line::Call {
            turn: 1,
            call: id.map(str::to_owned),
            fact,
            at_ms: 1,
        }
    }

    const WROTE: CallFact = CallFact::Emitted {
        wrote: true,
        exit: 0,
    };
    const FAILED: CallFact = CallFact::Emitted {
        wrote: false,
        exit: 0,
    };

    #[test]
    fn a_decline_is_ignored_only_when_the_same_call_was_emitted_and_then_ran() {
        let journal = Journal::from_lines(vec![
            call(Some("ran"), CallFact::Planned),
            call(Some("ran"), WROTE),
            call(Some("held"), CallFact::Planned),
            call(Some("held"), WROTE),
            call(Some("lost"), CallFact::Planned),
            call(Some("lost"), FAILED),
            call(Some("lost"), CallFact::Applied),
            call(Some("ran"), CallFact::Applied),
            call(Some("stranger"), CallFact::Applied),
            call(None, CallFact::Planned),
            call(None, WROTE),
        ]);
        assert_eq!(
            ineffective_denies(&journal),
            DeclineCalls {
                planned: 3,
                emitted: 2,
                unwritten: 1,
                ineffective: 1,
                uncorrelatable: 1,
            }
        );
    }
}
