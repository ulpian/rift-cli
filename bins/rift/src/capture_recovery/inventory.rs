//! Bounded, descriptor-relative inventory of the admitted member's agent-queue
//! journal: every direct entry counted, every ticket read whole under the
//! caps and gated, one retained original descriptor per boundary source.
//!
//! The inventory is a finite observed list, not a quiescence claim: a file a
//! producer adds after enumeration is outside it. Any entry the closed
//! classification cannot place, any cap crossed and any read that fails makes
//! the whole inventory non-actionable with its reasons counted — never a
//! partial list that quietly selects what it could read.
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;

use cap_std::fs::File;
use sha2::{Digest as _, Sha256};

use super::carrier::QueuedCondense;
use super::codec::{self, Shape, MAX_BYTES};
use super::files::{self, EntryKind, PhysicalIdentity};
use super::hold::ReadyQueue;
use crate::agent_queue::BoundarySnapshot;

/// Why an inventory is not actionable, as a static identifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Refusal {
    QueueUnavailable,
    EntriesOverflow,
    DuplicateName,
    NameNotAscii,
    Stage,
    Symlink,
    Directory,
    NotRegular,
    UnknownName,
    TicketOversize,
    TicketUnreadable,
    AggregateOverflow,
    TicketMalformed,
    TicketShape,
    SourceAlias,
    Resource,
}

/// What one pass over the directory counted; the refused map is empty for an
/// actionable inventory.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Counts {
    pub(crate) entries_counted: u32,
    pub(crate) tickets: u32,
    pub(crate) understood_other: u32,
    pub(crate) ancillary: u32,
    pub(crate) refused: BTreeMap<Refusal, u32>,
}

/// One boundary source with its derived facts and its retained original
/// descriptor; the raw bytes and the decoded value are gone.
pub(crate) struct Source {
    pub(crate) name: String,
    pub(crate) identity: PhysicalIdentity,
    pub(crate) raw_length: u64,
    pub(crate) ticket_sha256: String,
    pub(crate) context_sha256: String,
    pub(crate) context_id: String,
    pub(crate) attempts: u32,
    pub(crate) queued_at_ms: i64,
    pub(crate) snapshot: BoundarySnapshot,
    pub(crate) file: File,
}

/// A complete, actionable inventory.
pub(crate) struct Inventory {
    pub(crate) counts: Counts,
    pub(crate) sources: Vec<Source>,
    /// The queued Condense tickets of this work tree, still owed to their own
    /// executor: read here only so a carrier can be chosen among them.
    pub(crate) condenses: Vec<CondenseTicket>,
}

/// One queued Condense ticket: its name, its delivery identity and its facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CondenseTicket {
    pub(crate) name: String,
    pub(crate) context_id: String,
    pub(crate) job: QueuedCondense,
}

/// One direct entry after classification by type and name.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Entry {
    Ticket(String),
    Ancillary,
    Refused(Refusal),
}

/// The seam a test observes: the moment before one source is retained.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Phase {
    BeforeRetain(u32),
}

impl Refusal {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::QueueUnavailable => "queue-unavailable",
            Self::EntriesOverflow => "entries-overflow",
            Self::DuplicateName => "duplicate-name",
            Self::NameNotAscii => "name-not-ascii",
            Self::Stage => "stage",
            Self::Symlink => "symlink",
            Self::Directory => "directory",
            Self::NotRegular => "not-regular",
            Self::UnknownName => "unknown-name",
            Self::TicketOversize => "ticket-oversize",
            Self::TicketUnreadable => "ticket-unreadable",
            Self::AggregateOverflow => "aggregate-overflow",
            Self::TicketMalformed => "ticket-malformed",
            Self::TicketShape => "ticket-shape",
            Self::SourceAlias => "source-alias",
            Self::Resource => "resource",
        }
    }
}

impl Counts {
    fn refuse(&mut self, reason: Refusal) {
        *self.refused.entry(reason).or_insert(0) += 1;
    }

    pub(crate) fn is_actionable(&self) -> bool {
        self.refused.is_empty()
    }
}

/// Every direct entry of the queue counted toward `max_entries`, reading at
/// most one past it; the sentinel, an iterator error, a repeated name and a
/// non-ASCII name each refuse. Pure over the iterator, so a listing failure
/// is testable without a failing disk.
fn list_entries(
    listed: impl Iterator<Item = Result<(std::ffi::OsString, EntryKind), ()>>,
    max_entries: u32,
    counts: &mut Counts,
) -> Vec<Entry> {
    let mut seen = BTreeSet::new();
    let mut entries = Vec::new();
    for item in listed {
        let Ok((name, kind)) = item else {
            counts.refuse(Refusal::QueueUnavailable);
            break;
        };
        if counts.entries_counted >= max_entries {
            counts.refuse(Refusal::EntriesOverflow);
            break;
        }
        counts.entries_counted += 1;
        let Some(ascii) = name.to_str().filter(|s| s.is_ascii()) else {
            counts.refuse(Refusal::NameNotAscii);
            continue;
        };
        if !seen.insert(ascii.to_owned()) {
            counts.refuse(Refusal::DuplicateName);
            continue;
        }
        let entry = classify_entry(ascii, kind);
        match &entry {
            Entry::Refused(reason) => counts.refuse(*reason),
            Entry::Ancillary => counts.ancillary += 1,
            Entry::Ticket(_) => (),
        }
        entries.push(entry);
    }
    entries
}

/// The closed name and type classes: an executable `*.json` ticket, an
/// understood immutable marker or note, a stage or temp, or a shape this
/// selection does not understand.
fn classify_entry(name: &str, kind: EntryKind) -> Entry {
    match kind {
        EntryKind::Symlink => return Entry::Refused(Refusal::Symlink),
        EntryKind::Directory => return Entry::Refused(Refusal::Directory),
        EntryKind::Other => return Entry::Refused(Refusal::NotRegular),
        EntryKind::Regular => (),
    }
    if name.starts_with('.') || has_extension(name, "stage") || has_extension(name, "tmp") {
        return Entry::Refused(Refusal::Stage);
    }
    if codec::basename(OsStr::new(name)).is_err() {
        return Entry::Refused(Refusal::UnknownName);
    }
    if has_extension(name, "json") {
        return Entry::Ticket(name.to_owned());
    }
    let condensed = name.strip_prefix("condensed-").is_some_and(|key| {
        key.len() == 64
            && key
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    if condensed || has_extension(name, "error") {
        return Entry::Ancillary;
    }
    Entry::Refused(Refusal::UnknownName)
}

/// The queue spells its extensions in lowercase; an upper-case twin is not
/// the same class, so the comparison is deliberately exact.
fn has_extension(name: &str, extension: &str) -> bool {
    std::path::Path::new(name)
        .extension()
        .is_some_and(|actual| actual == extension)
}

/// The whole inventory: the queue rechecked, every entry listed and counted,
/// then every ticket read under the per-ticket and aggregate caps, decoded by
/// the queue's own codec and gated. Refusals are counted, never skipped.
pub(crate) fn enumerate(queue: &ReadyQueue<'_>) -> Result<Inventory, Counts> {
    enumerate_with(queue, &mut |_| Ok(()))
}

pub(crate) fn enumerate_with(
    queue: &ReadyQueue<'_>,
    observe: &mut impl FnMut(Phase) -> Result<(), ()>,
) -> Result<Inventory, Counts> {
    let mut counts = Counts::default();
    if queue.recheck().is_err() {
        counts.refuse(Refusal::QueueUnavailable);
        return Err(counts);
    }
    let budgets = queue.plan().budgets();
    let Ok(listed) = files::read_dir(queue.queue()) else {
        counts.refuse(Refusal::QueueUnavailable);
        return Err(counts);
    };
    let entries = list_entries(
        listed.map(|item| item.map_err(|_| ())),
        budgets.max_inventory_entries,
        &mut counts,
    );
    if !counts.is_actionable() {
        return Err(counts);
    }
    let mut names: Vec<String> = entries
        .into_iter()
        .filter_map(|entry| match entry {
            Entry::Ticket(name) => Some(name),
            _ => None,
        })
        .collect();
    names.sort_unstable();
    let caps = Caps {
        ticket: usize::try_from(budgets.max_ticket_bytes)
            .unwrap_or(MAX_BYTES)
            .min(MAX_BYTES),
        total: budgets.max_total_ticket_bytes,
    };
    let mut reader = Reader {
        queue,
        caps,
        total: 0,
        aliases: BTreeSet::new(),
        sources: Vec::new(),
        condenses: Vec::new(),
    };
    for (index, name) in names.iter().enumerate() {
        let slot = u32::try_from(index).map_err(|_| Refusal::EntriesOverflow);
        let read = slot.and_then(|slot| {
            if observe(Phase::BeforeRetain(slot)).is_err() {
                return Err(Refusal::Resource);
            }
            reader.read_ticket(name)
        });
        match read {
            Ok(Some(source)) => {
                counts.tickets += 1;
                reader.sources.push(source);
            }
            Ok(None) => counts.understood_other += 1,
            Err(reason) => {
                counts.refuse(reason);
                return Err(counts);
            }
        }
    }
    if queue.recheck().is_err() {
        counts.refuse(Refusal::QueueUnavailable);
        return Err(counts);
    }
    Ok(Inventory {
        counts,
        sources: reader.sources,
        condenses: reader.condenses,
    })
}

struct Caps {
    ticket: usize,
    total: u64,
}

struct Reader<'q, 'h> {
    queue: &'q ReadyQueue<'h>,
    caps: Caps,
    total: u64,
    aliases: BTreeSet<(u64, u64)>,
    sources: Vec<Source>,
    condenses: Vec<CondenseTicket>,
}

impl Reader<'_, '_> {
    /// One ticket: its length against the per-ticket cap and the running
    /// aggregate before the read, then the bounded read, the queue codec and
    /// the recovery gate. `Ok(None)` is an understood non-boundary job.
    fn read_ticket(&mut self, name: &str) -> Result<Option<Source>, Refusal> {
        let os_name = OsStr::new(name);
        let before = self
            .queue
            .queue()
            .dir
            .symlink_metadata(os_name)
            .map_err(|_| Refusal::TicketUnreadable)?;
        if before.len() > self.caps.ticket as u64 {
            return Err(Refusal::TicketOversize);
        }
        self.total = self
            .total
            .checked_add(before.len())
            .filter(|total| *total <= self.caps.total)
            .ok_or(Refusal::AggregateOverflow)?;
        let read = files::read_regular(self.queue.queue(), os_name, self.caps.ticket)
            .map_err(|_| Refusal::TicketUnreadable)?;
        let decoded = codec::decode(os_name, &read.bytes).map_err(|_| Refusal::TicketMalformed)?;
        let facts = match codec::recovery_gate(&decoded, self.queue.plan().work_tree()) {
            Ok(Shape::Boundary(facts)) => facts,
            Ok(Shape::Other) => {
                self.retain_condense(name, &read.bytes, &decoded)?;
                return Ok(None);
            }
            Err(_) => return Err(Refusal::TicketShape),
        };
        let identity = files::physical_identity(&read.metadata);
        if !self.aliases.insert((identity.dev, identity.ino)) {
            return Err(Refusal::SourceAlias);
        }
        let context_sha256 =
            codec::context_sha256(&decoded).map_err(|_| Refusal::TicketMalformed)?;
        let context_id = crate::capture_queue::delivery::context_id(&decoded.full)
            .map_err(|_| Refusal::TicketMalformed)?;
        Ok(Some(Source {
            name: name.to_owned(),
            identity,
            raw_length: read.metadata.len(),
            ticket_sha256: format!("{:x}", Sha256::digest(&read.bytes)),
            context_sha256,
            context_id,
            attempts: facts.attempts,
            queued_at_ms: facts.queued_at_ms,
            snapshot: facts.snapshot,
            file: read.file,
        }))
    }

    /// Keep a queued Condense's facts and digests; any other job is skipped.
    fn retain_condense(
        &mut self,
        name: &str,
        bytes: &[u8],
        decoded: &codec::Decoded,
    ) -> Result<(), Refusal> {
        let Some(job) = codec::condense(decoded, self.queue.plan().work_tree()) else {
            return Ok(());
        };
        let context_sha256 =
            codec::context_sha256(decoded).map_err(|_| Refusal::TicketMalformed)?;
        let context_id = crate::capture_queue::delivery::context_id(&decoded.full)
            .map_err(|_| Refusal::TicketMalformed)?;
        self.condenses.push(CondenseTicket {
            name: name.to_owned(),
            context_id,
            job: QueuedCondense {
                agent: job.agent,
                session_id: job.session_id,
                repo_id: job.repo_id,
                context_sha256,
                ticket_sha256: format!("{:x}", Sha256::digest(bytes)),
            },
        });
        Ok(())
    }
}

#[cfg(test)]
pub(super) mod tests;
