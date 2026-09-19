//! What a window was told it holds, what it held when it last lost its
//! context, and the key a decline is remembered under.
//!
//! A separate module because these are the shapes the journal's queries
//! answer with, and the merge that ranks a lost generation is arithmetic
//! over rows rather than a row.

use rift_core::Sha256Id;

use super::{Interval, IntervalSet, Line};

/// The exact request a decline was recorded against: this file, at this
/// revision, for these lines.
///
/// A decline is refused a second time for the same key, which is what bounds
/// a model that answers a decline by asking again to one extra exchange. A
/// neighbouring range is a different key and gets its own single decline.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DenyKey {
    /// The file, in the one spelling coverage is keyed by.
    pub path: String,
    /// The revision asked for.
    pub hash: Sha256Id,
    /// The lines asked for.
    pub range: Interval,
}

impl DenyKey {
    /// Whether a line about this file, revision and run of lines is a line
    /// about this key — the equality every question about one decline is
    /// asked with, in one place rather than at each of its callers.
    #[must_use]
    pub fn matches(&self, path: &str, hash: Sha256Id, range: Interval) -> bool {
        self.path == path && self.hash == hash && self.range == range
    }
}

/// An announcement Rift pushed that nothing has answered: the revision it was
/// about, and the lines it said the window held.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StandingHold {
    /// The revision the announcement named.
    pub hash: Sha256Id,
    /// The lines it said were held — [`Interval::is_whole_file`] when the
    /// announcement was about the whole of the file.
    pub range: Interval,
}

/// One file a context window held when it last stopped holding what it was
/// given: the revision, the lines, and when it last saw them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeldFile {
    /// The file, in the one spelling coverage is keyed by.
    pub path: String,
    /// The revision the window read.
    pub hash: Sha256Id,
    /// The lines it was handed, merged.
    pub held: IntervalSet,
    /// The millisecond of its newest read of that revision.
    pub last_read_at_ms: u64,
}

/// Every revision one stretch of lines was handed, merged and ranked.
pub(super) fn held_of(stretch: &[Line]) -> Vec<HeldFile> {
    let mut out: Vec<HeldFile> = Vec::new();
    for line in stretch {
        let Line::Read {
            path,
            hash,
            range,
            at_ms,
            ..
        } = line
        else {
            continue;
        };
        match out
            .iter_mut()
            .find(|held| held.path == *path && held.hash == *hash)
        {
            Some(held) => {
                held.held.insert(*range);
                held.last_read_at_ms = held.last_read_at_ms.max(*at_ms);
            }
            None => out.push(HeldFile {
                path: path.clone(),
                hash: *hash,
                held: {
                    let mut set = IntervalSet::new();
                    set.insert(*range);
                    set
                },
                last_read_at_ms: *at_ms,
            }),
        }
    }
    out.sort_by(|a, b| {
        b.last_read_at_ms
            .cmp(&a.last_read_at_ms)
            .then_with(|| a.path.cmp(&b.path))
    });
    out
}
