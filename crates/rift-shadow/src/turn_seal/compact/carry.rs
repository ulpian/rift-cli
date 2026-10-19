//! The compactor's state carried from one turn-end seal to the next, so a
//! turn compacts only the lines it added instead of the whole prefix twice
//! (4.78 s on a 28 MB transcript, measured in L-5 slice 8).
//!
//! A record can grow whenever a later line repeats its message id, so the
//! carry keeps, for each record the last turn added or grew, where its raw
//! fragments sit in the transcript: replaying them rebuilds the record's
//! bytes exactly. Every other record is closed and kept only as a digest of
//! its id. A line that grows a closed record cannot be placed from the carry,
//! so the turn starts over from a carry rebuilt out of the whole prefix; a
//! digest collision can only cause that slow road, never a wrong record.
//!
//! The carry holds offsets and digests, never transcript bytes, so it can sit
//! beside the session cache without passing the redaction gate.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::transcript::{compact_fragment, merge_content, record_line};

/// What the compactor knows of a transcript's first `lines` lines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompactCarry {
    /// Document lines covered.
    lines: u64,
    /// Bytes those lines span.
    bytes: u64,
    /// Compact records those lines produce.
    records: u64,
    /// Digest of the last covered line, so a rewritten transcript reads as
    /// a carry that no longer fits.
    last_line: u64,
    /// Id digests of the records no later line is expected to grow, sorted.
    closed: Vec<u64>,
    open: Vec<OpenRecord>,
    /// Blob oids of the compact parts sealed beside this carry: parts read
    /// back under the same oids hold `records` records without a recount.
    parts: Vec<String>,
}

/// A record a later line may still grow: its position and its fragments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenRecord {
    key: u64,
    at: u64,
    /// Byte ranges of the record's raw lines, without their `\n`.
    fragments: Vec<[u64; 2]>,
}

/// The records one advance added or grew, by position.
type Touched = Vec<(u64, Vec<[u64; 2]>)>;

impl CompactCarry {
    /// The carry of `raw`'s first `lines` lines, built from nothing, with
    /// every record open so the next advance can grow any of them.
    pub(super) fn rebuilt(raw: &[u8], lines: u64) -> Self {
        let (mut carry, touched) = Self::default().scan(raw, lines).unwrap_or_default();
        carry.open = touched
            .into_iter()
            .filter_map(|(at, fragments)| open_record(raw, at, fragments))
            .collect();
        carry
    }

    /// Compact records the carried lines produce.
    pub(super) fn records(&self) -> usize {
        usize::try_from(self.records).unwrap_or(usize::MAX)
    }

    /// Whether `parts` are, oid for oid, the parts sealed beside this carry.
    pub(super) fn vouches_for(&self, parts: &[super::PriorPart]) -> bool {
        !self.parts.is_empty()
            && self.parts.len() == parts.len()
            && self.parts.iter().zip(parts).all(|(oid, p)| *oid == p.oid)
    }

    /// Name the compact parts sealed beside this carry.
    pub(super) fn with_parts(mut self, parts: Vec<String>) -> Self {
        self.parts = parts;
        self
    }

    /// Whether this carry describes `raw`'s first `lines` lines.
    pub(super) fn fits(&self, raw: &[u8], lines: u64) -> bool {
        let Ok(end) = usize::try_from(self.bytes) else {
            return false;
        };
        let Some(covered) = raw.get(..end) else {
            return false;
        };
        if end == 0 {
            return self.lines == lines && lines == 0;
        }
        self.lines == lines
            && covered.ends_with(b"\n")
            && digest(last_line(covered)) == self.last_line
    }

    /// The raw part for the lines past this carry up to `now` — every record
    /// they added or grew, in position order — and the carry after them;
    /// `None` when a line grows a closed record.
    pub(super) fn advance(&self, raw: &[u8], now: u64) -> Option<(Vec<u8>, Self)> {
        let (mut next, touched) = self.scan(raw, now)?;
        let mut part = Vec::new();
        let mut open = Vec::new();
        for (at, fragments) in touched {
            part.extend_from_slice(&replay(raw, &fragments)?);
            part.push(b'\n');
            open.extend(open_record(raw, at, fragments));
        }
        let still_open: Vec<u64> = open.iter().map(|r| r.key).collect();
        next.closed.extend(
            self.open
                .iter()
                .map(|r| r.key)
                .filter(|k| !still_open.contains(k)),
        );
        next.closed.sort_unstable();
        next.closed.dedup();
        next.open = open;
        Some((part, next))
    }

    /// Read the lines past this carry up to `now`: the carry after them with
    /// no open records yet, and every record they added or grew.
    fn scan(&self, raw: &[u8], now: u64) -> Option<(Self, Touched)> {
        let mut offset = usize::try_from(self.bytes).ok()?;
        let tail = raw.get(offset..)?;
        let mut next = Self {
            open: Vec::new(),
            parts: Vec::new(),
            ..self.clone()
        };
        let mut touched: Touched = Vec::new();
        let mut index: HashMap<u64, usize> = HashMap::new();
        for open in &self.open {
            index.insert(open.key, touched.len());
            touched.push((open.at, open.fragments.clone()));
        }
        let mut grown = vec![false; touched.len()];
        let wanted = usize::try_from(now.saturating_sub(self.lines)).ok()?;
        for line in tail.split_inclusive(|b| *b == b'\n').take(wanted) {
            let Some(body) = line.strip_suffix(b"\n") else {
                break;
            };
            let range = [as_u64(offset), as_u64(offset + body.len())];
            offset += line.len();
            next.lines += 1;
            next.bytes = as_u64(offset);
            next.last_line = digest(body);
            let Some((_, id)) = compact_fragment(body) else {
                continue;
            };
            let key = id.as_deref().map(|i| digest(i.as_bytes()));
            if let Some(&slot) = key.and_then(|k| index.get(&k)) {
                touched.get_mut(slot)?.1.push(range);
                *grown.get_mut(slot)? = true;
                continue;
            }
            if key.is_some_and(|k| self.closed.binary_search(&k).is_ok()) {
                return None;
            }
            if let Some(k) = key {
                index.insert(k, touched.len());
            }
            touched.push((next.records, vec![range]));
            grown.push(true);
            next.records += 1;
        }
        let mut touched: Touched = touched
            .into_iter()
            .zip(grown)
            .filter_map(|(record, grown)| grown.then_some(record))
            .collect();
        touched.sort_by_key(|(at, _)| *at);
        Some((next, touched))
    }
}

/// The open record a later line may grow, or `None` for one with no id,
/// which nothing can grow.
fn open_record(raw: &[u8], at: u64, fragments: Vec<[u64; 2]>) -> Option<OpenRecord> {
    let first = fragments.first().and_then(|r| fragment(raw, *r))?;
    let (_, id) = compact_fragment(first)?;
    Some(OpenRecord {
        key: digest(id?.as_bytes()),
        at,
        fragments,
    })
}

/// A record's canonical line rebuilt from its raw fragments, merged in order
/// as the one-shot compactor merges them; `None` if a fragment is unreadable
/// or names another id (a digest collision).
fn replay(raw: &[u8], fragments: &[[u64; 2]]) -> Option<Vec<u8>> {
    let mut lines = fragments.iter().map(|r| fragment(raw, *r));
    let (mut record, id) = compact_fragment(lines.next()??)?;
    for line in lines {
        let (next, next_id) = compact_fragment(line?)?;
        if next_id != id {
            return None;
        }
        merge_content(&mut record, &next);
    }
    Some(record_line(record))
}

fn fragment(raw: &[u8], [start, end]: [u64; 2]) -> Option<&[u8]> {
    raw.get(usize::try_from(start).ok()?..usize::try_from(end).ok()?)
}

/// The last line of `covered`, without its `\n`; empty for no lines.
fn last_line(covered: &[u8]) -> &[u8] {
    let body = covered.strip_suffix(b"\n").unwrap_or(covered);
    let start = body
        .iter()
        .rposition(|b| *b == b'\n')
        .map_or(0, |at| at + 1);
    body.get(start..).unwrap_or_default()
}

/// The first eight bytes of a SHA-256: stable across builds, which the
/// standard hasher does not promise.
fn digest(bytes: &[u8]) -> u64 {
    let hash = Sha256::digest(bytes);
    let mut head = [0u8; 8];
    head.copy_from_slice(hash.get(..8).unwrap_or(&[0; 8]));
    u64::from_be_bytes(head)
}

fn as_u64(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}
