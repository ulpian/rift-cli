//! The other live windows of this repository family, and their journals.
//!
//! The one question about windows that are not the caller's that a decision
//! may lean on, which is why it is a module apart from
//! [`coordinator`](super::coordinator): that one names another window's reads
//! as a hint, and this one hands a decline the journals it rests on. Both read
//! files only — a journal, the marker that says which session a journal
//! belongs to, and that session's cache — and never a store, the git object
//! database or the tokenizer.
//!
//! **Live is decided from files.** A window is live when the session it
//! belongs to is: its cache says `active` or `compacting` and was written
//! within the family's idle-sweep floor — the same floor past which the sweep
//! ends the session — and its own journal was written within that floor too.
//! The second test is what makes the answer a window's rather than a
//! session's: a session that started a hundred subagents is live for as long
//! as any of them works, and a subagent that finished an hour ago holds
//! nothing anyone may rest a decline on. A window with no marker, a marker
//! that does not name it, a session swept or ended — each is not live.
//!
//! **Bounded before anything is read.** The journals are filtered by their
//! modification time first, newest first, and at most
//! [`MAX_SIBLING_JOURNALS`] of them and [`MAX_SIBLING_BYTES`] of their bytes
//! are read on one decision: the hook has a latency budget and the directory
//! is writable by whatever runs in the checkout.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use rift_agent::window_journal::Journal;

use super::{is_journal_key, journal_dir, key, WindowJournal};

/// The most journals of other windows one decision reads.
pub(crate) const MAX_SIBLING_JOURNALS: usize = 16;

/// The most journal bytes one decision reads, across those windows.
pub(crate) const MAX_SIBLING_BYTES: u64 = 1024 * 1024;

/// The largest marker file read: one is a short JSON object.
const MAX_MARKER_BYTES: u64 = 4_096;

/// What a decision about a read asks for: the caller's own checkout and
/// window, the moment it asks at and the floor past which a window is not
/// live.
pub(crate) struct FamilyQuery<'a> {
    /// The caller's checkout.
    pub(crate) work_tree: &'a Path,
    /// The caller's git dir, from which the family's members are found.
    pub(crate) git_dir: &'a Path,
    /// The caller's own window key, which is never one of its siblings.
    pub(crate) own_key: &'a str,
    /// The moment of the decision, in milliseconds since the epoch.
    pub(crate) now_ms: u64,
    /// The family's idle-sweep floor, in milliseconds.
    pub(crate) floor_ms: u64,
}

/// The journals of the live windows a decision read, and what reading them
/// cost.
#[derive(Debug, Default)]
pub(crate) struct SiblingJournals {
    /// One per live window, newest first.
    pub(crate) journals: Vec<Journal>,
    /// The journal bytes they were read from.
    pub(crate) bytes: u64,
}

/// One checkout of the family: where its journals and its sessions are.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Checkout {
    /// The checkout directory, under which its journals live.
    work_tree: PathBuf,
    /// Its git dir, under which its sessions' caches live.
    git_dir: PathBuf,
}

/// A journal recent enough to be a live window's, not yet read.
struct Candidate {
    /// Which checkout it is in.
    checkout: usize,
    /// Its window key.
    key: String,
    /// When its live file was last written.
    modified_ms: u64,
    /// Every byte its files hold.
    bytes: u64,
}

/// The journals of every live window of the family but the caller's own,
/// within the bounds.
///
/// The [`MAX_SIBLING_JOURNALS`] most recently written journals within the
/// floor are examined, newest first; each live one is read until the next
/// would pass [`MAX_SIBLING_BYTES`]. The newest are the ones most likely to
/// hold the revision being asked for now, so a bound that cuts is a bound that
/// cuts the coldest end.
pub(crate) fn live_sibling_journals(query: &FamilyQuery<'_>) -> SiblingJournals {
    let checkouts = family_checkouts(query.work_tree, query.git_dir);
    let mut candidates: Vec<Candidate> = checkouts
        .iter()
        .enumerate()
        .flat_map(|(at, checkout)| recent_journals(at, checkout, query))
        .collect();
    candidates.sort_by(|a, b| {
        b.modified_ms
            .cmp(&a.modified_ms)
            .then_with(|| a.key.cmp(&b.key))
    });
    let mut out = SiblingJournals::default();
    let mut sessions: BTreeMap<(usize, String), bool> = BTreeMap::new();
    for candidate in candidates.into_iter().take(MAX_SIBLING_JOURNALS) {
        let bytes = out.bytes.saturating_add(candidate.bytes);
        if bytes > MAX_SIBLING_BYTES {
            break;
        }
        let Some(checkout) = checkouts.get(candidate.checkout) else {
            continue;
        };
        if !window_is_live(checkout, &candidate, query, &mut sessions) {
            continue;
        }
        let journal = WindowJournal {
            dir: journal_dir(&checkout.work_tree),
            key: candidate.key,
        };
        out.bytes = bytes;
        out.journals.push(journal.load().journal);
    }
    out
}

/// Every checkout of the family the caller's checkout belongs to — the main
/// work tree and each linked worktree whose checkout still exists — found from
/// the git dir's own files without running git. The caller's checkout alone
/// when the family cannot be read.
fn family_checkouts(work_tree: &Path, git_dir: &Path) -> Vec<Checkout> {
    let own = Checkout {
        work_tree: work_tree.to_path_buf(),
        git_dir: git_dir.to_path_buf(),
    };
    let Ok(paths) = rift_git::discover(work_tree) else {
        return vec![own];
    };
    let members: Vec<Checkout> = rift_git::family_members(&paths)
        .into_iter()
        .filter(|member| !member.prunable)
        .map(|member| Checkout {
            work_tree: member.work_tree,
            git_dir: member.git_dir,
        })
        .collect();
    if members.is_empty() {
        return vec![own];
    }
    members
}

/// The journals of one checkout whose live file was written within the floor,
/// each with every byte its files hold.
fn recent_journals(at: usize, checkout: &Checkout, query: &FamilyQuery<'_>) -> Vec<Candidate> {
    let mut files: BTreeMap<String, (Option<u64>, u64)> = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(journal_dir(&checkout.work_tree)) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some((window, rest)) = name.split_once('.') else {
            continue;
        };
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !is_journal_key(window) || window == query.own_key || !meta.is_file() {
            continue;
        }
        let slot = files.entry(window.to_owned()).or_default();
        slot.1 = slot.1.saturating_add(meta.len());
        if rest == "jsonl" {
            slot.0 = journal_written_ms(&meta);
        }
    }
    files
        .into_iter()
        .filter_map(|(window, (modified, bytes))| {
            let modified_ms = modified?;
            if query.now_ms.saturating_sub(modified_ms) > query.floor_ms {
                return None;
            }
            Some(Candidate {
                checkout: at,
                key: window,
                modified_ms,
                bytes,
            })
        })
        .collect()
}

/// A file's modification time in milliseconds since the epoch.
fn journal_written_ms(meta: &std::fs::Metadata) -> Option<u64> {
    let since = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    u64::try_from(since.as_millis()).ok()
}

/// Whether the window a recent journal belongs to is live: its marker names
/// it, and the session it names is live in that checkout. A session asked
/// about twice is read once.
fn window_is_live(
    checkout: &Checkout,
    candidate: &Candidate,
    query: &FamilyQuery<'_>,
    sessions: &mut BTreeMap<(usize, String), bool>,
) -> bool {
    let Some(session) = marker_session(&checkout.work_tree, &candidate.key) else {
        return false;
    };
    *sessions
        .entry((candidate.checkout, session))
        .or_insert_with_key(|(_, session)| session_is_live(&checkout.git_dir, session, query))
}

/// The session the window `window_key` belongs to, from the marker its
/// opening left — or `None` when there is none, or it names some other
/// window. The marker is a file an agent can write, so its claim is checked
/// against the key the journal is named by.
fn marker_session(work_tree: &Path, window_key: &str) -> Option<String> {
    let path = work_tree
        .join(".rift")
        .join("state")
        .join("injection")
        .join(format!("{window_key}.json"));
    if std::fs::metadata(&path).ok()?.len() > MAX_MARKER_BYTES {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let session = value.get("session")?.as_str()?;
    let agent = match value.get("agent_id") {
        None | Some(serde_json::Value::Null) => None,
        Some(agent) => Some(agent.as_str()?),
    };
    if key(session, agent) != window_key {
        return None;
    }
    Some(session.to_owned())
}

/// Whether `session`'s cache in this git dir says it is working and was
/// written within the floor. A cache stamped in the future reads as age zero.
fn session_is_live(git_dir: &Path, session: &str, query: &FamilyQuery<'_>) -> bool {
    let Ok(Some(cache)) = rift_shadow::hooks::session_cache::read_cache(git_dir, session) else {
        return false;
    };
    let working = matches!(cache.state.as_str(), "active" | "compacting");
    let updated = u64::try_from(cache.updated_at_ms).unwrap_or_default();
    working && query.now_ms.saturating_sub(updated) <= query.floor_ms
}

#[cfg(test)]
mod tests;
