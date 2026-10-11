//! A read another live window of the repository holds: declined once, with a
//! cheaper route to the same bytes — the file's outline where one is bound,
//! the runs the other windows hold while none is — and every later read of
//! them admitted and charged against the decline.
//!
//! Separate because it is the gate's one decision that rests on a window other
//! than the caller's. What it reads to make it is files only: the other
//! windows' journals, bounded, through the window-journal plane, and the
//! outline cache through its one admitted reader. Neither found, and the read
//! runs — a decline here is never a bare refusal.

use std::path::Path;

use rift_agent::window_journal::{
    decide_family_held, family_coverage, held_runs, is_family_candidate, Decision, FamilyRoute,
    Interval, IntervalSet, ResolvedRead, MAX_FAMILY_RUNS,
};

use super::first_contact::with_outlined_read;
use super::gate_mode;
use super::members::{deny_channel, GatePass};
use super::outline_cache::load_outline_cache;

/// The route a decline would carry, rendered: the route, its frame, and the
/// runs the frame offers as follow-ups.
struct Offer {
    route: FamilyRoute,
    frame: String,
    offered: Vec<Interval>,
}

/// What a read the shipped decision allowed becomes when another live window
/// holds its bytes and a cheaper route exists: the decision, and the frame
/// that travels only with an answered one. `None` leaves the read as it was.
///
/// The pure gates run first, so an ordinary read never reads another window's
/// journal; then the live windows' coverage, then the route. The frame is
/// rendered either way, because an observed decline records its size.
pub(super) fn family_answer(
    raw: &rift_shadow::hooks::RawHook,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
    pass: &GatePass<'_>,
    read: &ResolvedRead,
) -> Option<(Decision, Option<String>)> {
    let mode = gate_mode(effective.decline_family_held);
    let floor = rift_recall_serve::OUTLINE_FRAME_MIN_PAGE_LINES;
    if !is_family_candidate(read, pass.held, mode, floor) {
        return None;
    }
    let holders = live_family_coverage(work_tree, pass, read);
    if holders.is_empty() {
        return None;
    }
    let route = family_route(pass.git_dir, read, &holders)?;
    let frame_bytes = u32::try_from(route.frame.len()).unwrap_or(u32::MAX);
    let decision = decide_family_held(
        route.route,
        route.offered,
        frame_bytes,
        mode,
        deny_channel(&raw.agent, &raw.verb),
    );
    let travels = matches!(decision, Decision::Deny(_)).then_some(route.frame);
    Some((decision, travels))
}

/// What the other live windows of the family hold of this revision, read
/// within the bounds the window-journal plane keeps.
fn live_family_coverage(work_tree: &Path, pass: &GatePass<'_>, read: &ResolvedRead) -> IntervalSet {
    let floor_ms = rift_config::idle_sweep_ms(&work_tree.join(".rift"));
    let siblings =
        crate::window_journal::live_sibling_journals(&crate::window_journal::FamilyQuery {
            work_tree,
            git_dir: pass.git_dir,
            own_key: pass.window_key,
            now_ms: pass.at_ms,
            floor_ms: u64::try_from(floor_ms).unwrap_or_default(),
        });
    family_coverage(&siblings.journals, &read.path, read.hash)
}

/// The cheaper route, or `None` when there is none: the outline bound for
/// these exact bytes first, and the runs the other windows hold only while no
/// outline is.
fn family_route(git_dir: &Path, read: &ResolvedRead, holders: &IntervalSet) -> Option<Offer> {
    if let Some(cached) = load_outline_cache(git_dir, &read.path, read.hash) {
        let (frame, spans) =
            with_outlined_read(&cached, read, rift_recall_serve::render_held_outline_frame)?;
        let offered = spans
            .into_iter()
            .filter_map(|(lo, hi)| Interval::new(lo, hi))
            .take(MAX_FAMILY_RUNS)
            .collect();
        return Some(Offer {
            route: FamilyRoute::Outline,
            frame,
            offered,
        });
    }
    let runs = held_runs(read, holders)?;
    let pairs: Vec<(u32, u32)> = runs.iter().map(|run| (run.lo, run.hi)).collect();
    let page_end = (read.range.hi < read.total_lines).then_some(read.range.hi);
    let frame = rift_recall_serve::render_held_lines_frame(
        &read.path,
        &read.hash.to_hex(),
        &pairs,
        page_end,
    )?;
    Some(Offer {
        route: FamilyRoute::Held,
        frame,
        offered: runs,
    })
}
