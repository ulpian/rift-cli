//! The section a subagent's window opens with: the revisions its coordinator
//! held at that instant, which the subagent does not.
//!
//! A module of its own because it is the one composer that speaks about a
//! window other than the one the hook belongs to. It names revisions and
//! nothing more. It never writes to the subagent's window journal and never
//! touches the read gate, so a hint can never turn into a decline: the gate
//! keeps deciding from the subagent's own reads alone.
//!
//! Spoken once, at the boundary that opens the window, inside the claim that
//! hands the window its pointer: a retried opening finds the claim taken and
//! says nothing, so no marker of its own is needed.

use std::path::Path;

use rift_core::AgentId;

use super::agent::{boundary_budget, boundary_ceiling};
use crate::window_journal::{coordinator_reads, CoordinatorRead};

/// The boundary a subagent's window opens at, gathered so the composer takes
/// one argument rather than five.
pub(super) struct Opening<'a> {
    pub(super) repo: &'a Path,
    pub(super) agent: &'a AgentId,
    pub(super) verb: &'a str,
    pub(super) session: &'a str,
    /// The subagent whose window opens here.
    pub(super) window: &'a str,
}

/// The coordinator section, or `None` for silence.
///
/// It rides in the opening boundary's own budget, after the text `already`
/// pushed there (the pointer), so the whole push stays inside the number the
/// operator set; injection switched off is switched off here too.
///
/// Newest first: what the coordinator read last before the spawn is what the
/// task was most likely briefed from, and the fit drops from the tail.
pub(super) fn coordinator_section(at: &Opening<'_>, already: &str) -> Option<String> {
    let budget = boundary_budget(
        rift_recall_policy::ServePolicy::load(&at.repo.join(".rift")).injection_turn_budget,
        boundary_ceiling(at.agent, at.verb),
    );
    if budget == 0 {
        return None;
    }
    let mut held = coordinator_reads(at.repo, at.session, at.window, None);
    held.reverse();
    let room = budget.saturating_sub(rift_tokens::count(already));
    fitted_section(&held, room)
}

/// The longest section that fits `room` tokens, dropping named revisions from
/// the tail into the count, or `None` when not even the count fits.
fn fitted_section(held: &[CoordinatorRead], room: u32) -> Option<String> {
    let hexes: Vec<String> = held.iter().map(|read| read.hash.to_hex()).collect();
    let reads: Vec<rift_recall_serve::CoordinatorRead<'_>> = held
        .iter()
        .zip(&hexes)
        .map(|(read, hash)| rift_recall_serve::CoordinatorRead {
            path: &read.path,
            hash,
        })
        .collect();
    let mut keep = reads.len();
    loop {
        let shown = reads.get(..keep).unwrap_or_default();
        let omitted = reads.len().saturating_sub(keep);
        let (text, named) = rift_recall_serve::render_coordinator_reads_naming(shown, omitted)?;
        if rift_tokens::count(&text) <= room {
            return Some(text);
        }
        // Shrink from what the frame actually named, so a capped or partly
        // withheld list loses a name on every pass rather than an unnamed row.
        keep = *named.last()?;
    }
}

#[cfg(test)]
mod tests;
