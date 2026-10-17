//! The family-held share of the unchanged re-reads of files no commit holds.
//!
//! The gate model credits an unchanged re-read of such a file only to the
//! window that already held its bytes. The largest class it lets through is a
//! window's first read of bytes another window of the same session tree
//! held, which is the family-held decline's ground, not a re-read. The
//! family-held rows count a first sight only where the read resolved a line
//! count from a commit or from a write the replay saw, so a file no commit
//! holds never reaches them. This module sizes that pool on the working-tree
//! model: the same cases, routes, frames and follow-ups, over the journal of
//! uncommitted files, and the reasons the rest are no case.
//!
//! Every figure is on rows of its own, so every row before them reads the
//! same with or without them.

use serde_json::{json, Value};

use super::family_held::{FamilyHeldPool, NoCase, RouteFigures, ROUTES};
use super::render::{commas, row};
use super::report::{Report, Tally};

/// What the first reads of uncommitted bytes another window held come to.
#[derive(Clone, Debug, Default)]
pub struct UncommittedFamilyHeld {
    /// The first reads a live window held, as family-held cases.
    pub(super) pool: FamilyHeldPool,
    /// Of every first read, the ones today's family-held rows count.
    pub(super) counted_today: Tally,
    /// Held only by a subagent whose journal had been idle past the floor.
    quiet_holder: Tally,
    /// A mid-file range, or a first page under the outline floor.
    shape: Tally,
    /// No other window's journal holds all the lines asked for.
    no_holder: Tally,
}

impl UncommittedFamilyHeld {
    pub(super) fn note_no_case(&mut self, why: NoCase, tokens: u64) {
        let tally = match why {
            NoCase::Shape => &mut self.shape,
            NoCase::QuietHolder => &mut self.quiet_holder,
            NoCase::NoHolder => &mut self.no_holder,
        };
        tally.add(tokens);
    }

    /// The cases over every route.
    fn cases(&self) -> Tally {
        let figures = self.pool.route_figures();
        sum(figures.iter().map(|f| f.pool))
    }

    /// What every route nets: the reads a decline answers, less the frames
    /// and the follow-ups. The route that runs the read nets nothing.
    fn net(&self) -> Tally {
        let figures = self.pool.route_figures();
        sum(figures
            .iter()
            .filter(|f| f.frames > 0)
            .map(|f| f.net_kept()))
    }

    /// Every first read the pool is made of: the cases and the rest.
    fn first_reads(&self) -> Tally {
        sum([self.cases(), self.quiet_holder, self.shape, self.no_holder])
    }
}

fn sum(parts: impl IntoIterator<Item = Tally>) -> Tally {
    let mut total = Tally::default();
    for part in parts {
        total.calls = total.calls.saturating_add(part.calls);
        total.tokens = total.tokens.saturating_add(part.tokens);
    }
    total
}

/// The table's rows for the section.
pub(super) fn family_held_uncommitted_rows(report: &Report, denominator: u64) -> String {
    let held = &report.family_held_uncommitted;
    let mut out = String::from(
        "\n  family held, files no commit holds: first reads of bytes another window held \
         (observed; working-tree model)\n",
    );
    out.push_str(&row(
        "  first reads of bytes another window held",
        held.first_reads(),
        denominator,
    ));
    out.push_str(&row(
        "    counted by the family-held rows above",
        held.counted_today,
        denominator,
    ));
    out.push_str(&row(
        "    a case: a live window held the bytes",
        held.cases(),
        denominator,
    ));
    for ((_, label), f) in ROUTES.iter().zip(held.pool.route_figures()) {
        out.push_str(&row(&format!("      {label}"), f.pool, denominator));
        if f.frames > 0 {
            let net_label = format!(
                "        net of frames (max {} tok) and follow-ups",
                commas(f.frame_max)
            );
            out.push_str(&row(&net_label, f.net_kept(), denominator));
        }
        out.push_str(&row(
            "        followed by another read",
            f.follow_ups,
            denominator,
        ));
    }
    out.push_str(&row(
        "    net of every route (an upper bound)",
        held.net(),
        denominator,
    ));
    out.push_str(&row(
        "    no case: held only by a quiet subagent",
        held.quiet_holder,
        denominator,
    ));
    out.push_str(&row(
        "    no case: a mid-file range or a page under the floor",
        held.shape,
        denominator,
    ));
    out.push_str(&row(
        "    no case: no other window holds the whole of it",
        held.no_holder,
        denominator,
    ));
    out
}

/// The same rows for the machine-readable document.
pub(super) fn family_held_uncommitted_json(report: &Report) -> Value {
    let held = &report.family_held_uncommitted;
    let tally = |t: Tally| json!({ "calls": t.calls, "tokens": t.tokens });
    let route = |f: RouteFigures| {
        json!({
            "calls": f.pool.calls,
            "tokens": f.pool.tokens,
            "frame_tokens": f.frames,
            "frame_tokens_max": f.frame_max,
            "follow_up_calls": f.follow_ups.calls,
            "follow_up_tokens": f.follow_ups.tokens,
            "net_tokens": if f.frames > 0 { f.net_kept().tokens } else { 0 },
        })
    };
    let mut routes = serde_json::Map::new();
    for ((key, _), f) in ROUTES.iter().zip(held.pool.route_figures()) {
        routes.insert((*key).to_owned(), route(f));
    }
    json!({
        "first_reads": tally(held.first_reads()),
        "counted_by_family_held_rows": tally(held.counted_today),
        "cases": tally(held.cases()),
        "routes": routes,
        "net_tokens": held.net().tokens,
        "no_case": {
            "quiet_subagent_holder": tally(held.quiet_holder),
            "shape": tally(held.shape),
            "no_holder": tally(held.no_holder),
        },
    })
}
