//! The first contact: a first read of a large file in this window, answered —
//! once the lever acts — with the file's bound outline instead of its lines.
//!
//! Separate because it is the gate's one decision about a read the window does
//! not hold, and the one that reads a file of its own: the outline cache,
//! named by the bytes' hash, which is how a hook learns an outline is bound
//! without opening a store or starting a tokenizer. A miss is a read that
//! runs.

use rift_agent::window_journal::{Decision, FirstContactFloor, ResolvedRead};

use super::file::GateFile;
use super::gate_mode;
use super::members::{deny_channel, GatePass};
use super::outline_cache::{load_outline_cache, CachedOutline};

/// What a read the shipped decision allowed becomes when it is a first
/// contact with a bound outline: the decision, and the frame that travels
/// only with an answered one. `None` leaves the read as it was.
///
/// The pure half decides first, so the cache is read only for a read that is
/// a candidate; then the outline must be bound for these exact bytes and its
/// frame must render. Either missing, and the read runs with nothing recorded
/// about an outline.
pub(super) fn first_contact_answer(
    raw: &rift_shadow::hooks::RawHook,
    effective: &rift_recall_policy::Effective,
    pass: &GatePass<'_>,
    file: &GateFile,
    read: &ResolvedRead,
) -> Option<(Decision, Option<String>)> {
    if effective.first_contact == rift_recall_policy::Mode::Off
        || !rift_recall_serve::is_outlinable_path(&read.path)
    {
        return None;
    }
    let floor = FirstContactFloor {
        min_lines: effective.first_contact_min_lines,
        min_bytes: effective.first_contact_min_bytes,
        min_page_lines: rift_recall_serve::OUTLINE_FRAME_MIN_PAGE_LINES,
    };
    let decision = rift_agent::window_journal::decide_first_contact(
        read,
        pass.held,
        gate_mode(effective.first_contact),
        deny_channel(&raw.agent, &raw.verb),
        floor,
        u64::try_from(file.bytes.len()).unwrap_or(u64::MAX),
    );
    if decision == Decision::Allow {
        return None;
    }
    let cached = load_outline_cache(pass.git_dir, &read.path, read.hash)?;
    let frame = with_outlined_read(&cached, read, rift_recall_serve::render_outline_frame)?;
    let travels = matches!(decision, Decision::Deny(_)).then_some(frame);
    Some((decision, travels))
}

/// What `render` makes of this read's cached outline, as the sealed renderer
/// is handed it: one construction for every frame an outline is sent in.
pub(super) fn with_outlined_read<T>(
    cached: &CachedOutline,
    read: &ResolvedRead,
    render: impl FnOnce(&rift_recall_serve::OutlinedRead<'_>) -> T,
) -> T {
    let entries: Vec<rift_recall_serve::OutlineEntry<'_>> = cached
        .entries
        .iter()
        .map(|entry| rift_recall_serve::OutlineEntry {
            kind: entry.kind,
            name: &entry.name,
            first_line: entry.first_line,
            last_line: entry.last_line,
            tokens: entry.tokens,
        })
        .collect();
    let hash = read.hash.to_hex();
    render(&rift_recall_serve::OutlinedRead {
        path: &read.path,
        hash: &hash,
        lines: cached.lines,
        bytes: cached.bytes,
        whole_tokens: cached.whole_tokens,
        definitions: cached.definitions,
        markdown: cached.markdown,
        entries: &entries,
        page_end: (read.range.hi < read.total_lines).then_some(read.range.hi),
    })
}
