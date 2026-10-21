//! The turn end's two scans of the main transcript: the spill references its
//! results name, and the lines recording the results of spooled originals.
//!
//! Both used to read the whole transcript at every turn end, so their cost
//! grew with the session. The turn-frontier sidecar now carries what the
//! earlier bytes held, so a turn end scans only the bytes past that frontier
//! and joins the two, first finding winning as in a whole scan. The carry is
//! trusted only when the file still holds the same last bytes before the
//! frontier, the seal extended an unbroken chain (the caller offers no carry
//! otherwise), and no original was spooled that the earlier scan did not look
//! for. Anything else scans the whole file, as every other road does.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use rift_shadow::backend::{ArchivedOriginal, ArchivedSpill};
use rift_shadow::hooks::turn_frontier::ScanCarry;
use rift_shadow::redact::RulePackSet;

use super::originals::{result_lines, Spool};
use super::spills;

/// The bytes before the frontier whose hash stands for the scanned part: a
/// rewritten file almost always differs there, and hashing them costs nothing
/// next to the scan they spare.
const TAIL_BYTES: usize = 4096;

/// What the scans are made for: the main transcript's session and its spool.
pub(super) struct MainScan<'a> {
    pub(super) path: &'a Path,
    pub(super) session_id: &'a str,
    pub(super) packs: &'a RulePackSet,
    pub(super) spool: &'a Spool,
}

/// What a scan found, and the carry the next turn end resumes from.
pub(super) struct Scanned {
    pub(super) spills: Vec<ArchivedSpill>,
    pub(super) originals: Vec<ArchivedOriginal>,
    pub(super) carry: ScanCarry,
}

/// Where a scan starts and what the bytes before that start held.
#[derive(Default)]
struct Found {
    from: usize,
    lines: u64,
    named: BTreeMap<String, String>,
    claimed: BTreeMap<String, usize>,
}

#[cfg(test)]
thread_local! {
    /// Transcript bytes scanned on this thread, so a test can prove a turn
    /// end read only the bytes past the frontier.
    pub(super) static SCANNED_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Scan `raw` from `prior`'s frontier when the carry still holds, else whole;
/// `sealed` is the same transcript through the gate.
pub(super) fn scan(
    ctx: &MainScan<'_>,
    raw: &[u8],
    sealed: &[u8],
    prior: Option<&ScanCarry>,
) -> Scanned {
    let ids = ctx.spool.ids();
    let mut found = prior.and_then(|p| resume(raw, p, &ids)).unwrap_or_default();
    let new = raw.get(found.from..).unwrap_or_default();
    note_scanned(new.len());
    for (id, address) in spills::named_spills(new) {
        found.named.entry(id).or_insert(address);
    }
    if !ids.is_empty() {
        let wanted: BTreeSet<&str> = ids.iter().map(String::as_str).collect();
        let base = usize::try_from(found.lines).unwrap_or(usize::MAX);
        for (id, line) in result_lines(new, &wanted) {
            found.claimed.entry(id).or_insert(base.saturating_add(line));
        }
    }
    let carry = carry_of(raw, &found, new, ids);
    Scanned {
        originals: ctx.spool.claimed_at(&found.claimed, sealed),
        spills: spills::of_named(ctx.path, ctx.session_id, ctx.packs, found.named),
        carry,
    }
}

/// The earlier scan's findings, when `raw` still holds the bytes it scanned
/// and every spooled id was looked for then.
fn resume(raw: &[u8], prior: &ScanCarry, ids: &BTreeSet<String>) -> Option<Found> {
    let from = usize::try_from(prior.scanned_bytes).ok()?;
    let tail = raw.get(from.saturating_sub(TAIL_BYTES)..from)?;
    if rift_shadow::sha256_hex(&[tail]) != prior.tail_sha256 {
        return None;
    }
    if !ids.iter().all(|id| prior.wanted.contains(id)) {
        return None;
    }
    let claimed = prior
        .claimed
        .iter()
        .map(|(id, line)| Some((id.clone(), usize::try_from(*line).ok()?)))
        .collect::<Option<_>>()?;
    Some(Found {
        from,
        lines: prior.scanned_lines,
        named: prior.spills.iter().cloned().collect(),
        claimed,
    })
}

/// The carry for the next turn end: the frontier is the end of the last
/// complete line, since a line still being written may yet change.
fn carry_of(raw: &[u8], found: &Found, new: &[u8], ids: BTreeSet<String>) -> ScanCarry {
    let complete = new.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
    let frontier = found.from.saturating_add(complete);
    let added = new.get(..complete).unwrap_or_default();
    // `added` ends on a line break, so each inclusive piece is one line.
    let added_lines = added.split_inclusive(|b| *b == b'\n').count();
    let tail = raw
        .get(frontier.saturating_sub(TAIL_BYTES)..frontier)
        .unwrap_or_default();
    ScanCarry {
        scanned_bytes: u64::try_from(frontier).unwrap_or(u64::MAX),
        scanned_lines: found
            .lines
            .saturating_add(u64::try_from(added_lines).unwrap_or(u64::MAX)),
        tail_sha256: rift_shadow::sha256_hex(&[tail]),
        spills: found
            .named
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect(),
        wanted: ids.into_iter().collect(),
        claimed: found
            .claimed
            .iter()
            .map(|(id, line)| (id.clone(), u64::try_from(*line).unwrap_or(u64::MAX)))
            .collect(),
    }
}

#[cfg(test)]
fn note_scanned(bytes: usize) {
    SCANNED_BYTES.with(|n| n.set(n.get() + bytes));
}

#[cfg(not(test))]
fn note_scanned(_bytes: usize) {}
