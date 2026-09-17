//! The LIFO edit-stack replay: turn-boundary `H_i`/`A_i` hunks →
//! per-line writer histories.
//!
//! The boundary snapshotter records, per turn, the human's between-turn hunks
//! (`H_i`, closed at `turn.start`) and the agent's within-turn hunks (`A_i`,
//! closed at `turn.end`) — coordinates only, ordered `[H_0, A_0, H_1, A_1, …]`.
//! This module replays that stack over a line-ownership model of the file and
//! hands the committed lines' writer histories to [`crate::attribute`] (the
//! LIFO scorer). Pure and total: coordinates that do not fit the model mark the
//! file **diverged** (a capture gap), which the caller scores as `unknown`
//! lines — never silently credited.

use crate::diff::Hunk;
use crate::Actor;

/// One replayed turn edit: an `H_i` or `A_i` hunk for one file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TurnEdit {
    /// Who made the edit (`H_i` → human, `A_i` → agent).
    pub actor: Actor,
    /// The hunk coordinates (0-based, half-open — `crate::diff` convention).
    pub hunk: Hunk,
}

/// The replayed state of one file after the whole edit stack.
#[derive(Clone, Debug, Default)]
pub struct ReplayedFile {
    /// Per surviving line (0-based), the ordered writers that produced it.
    /// Pre-session lines carry an empty history.
    pub histories: Vec<Vec<Actor>>,
    /// Agent-authored lines a later human edit deleted.
    pub agent_removed_by_human: u32,
    /// Whether any edit failed to fit the model (a capture gap: hooks missed an
    /// edit, or the session spans states the boundaries did not observe).
    pub diverged: bool,
}

/// Replay one file's ordered turn edits over a base of `base_len` pre-session
/// lines. Never panics; an out-of-bounds edit sets `diverged` and is skipped.
#[must_use]
pub fn replay(base_len: u32, edits: &[TurnEdit]) -> ReplayedFile {
    let mut out = ReplayedFile {
        histories: vec![Vec::new(); base_len as usize],
        ..ReplayedFile::default()
    };
    for e in edits {
        let start = e.hunk.old_start as usize;
        let old_len = e.hunk.old_len as usize;
        let new_len = e.hunk.new_len as usize;
        let Some(end) = start.checked_add(old_len) else {
            out.diverged = true;
            continue;
        };
        if end > out.histories.len() {
            out.diverged = true;
            continue;
        }

        // Modified prefix: min(old,new) lines keep their history, the new
        // writer appended (LIFO takeover).
        let keep = old_len.min(new_len);
        for h in out.histories.iter_mut().skip(start).take(keep) {
            h.push(e.actor);
        }
        if new_len > old_len {
            // Net insertion: fresh lines written solely by this actor.
            let fresh = new_len - old_len;
            for i in 0..fresh {
                out.histories.insert(start + keep + i, vec![e.actor]);
            }
        } else if old_len > new_len {
            // Net deletion: agent-authored lines removed by a human edit are an
            // acceptance signal.
            for h in out.histories.drain(start + keep..end) {
                if e.actor == Actor::Human && h.contains(&Actor::Agent) {
                    out.agent_removed_by_human += 1;
                }
            }
        }
    }
    out
}

/// The writer histories of a commit's changed lines: for each `C` hunk, the
/// model's histories over the hunk's new-side range. A model that does not
/// match the committed file's length (or was already diverged) yields empty
/// histories for every changed line — `unknown`, never silently credited.
#[must_use]
pub fn committed_writers(
    replayed: &ReplayedFile,
    c_hunks: &[Hunk],
    committed_len: u32,
) -> Vec<Vec<Actor>> {
    let changed: usize = c_hunks.iter().map(|h| h.new_len as usize).sum();
    if replayed.diverged || replayed.histories.len() != committed_len as usize {
        return vec![Vec::new(); changed];
    }
    let mut writers = Vec::with_capacity(changed);
    for h in c_hunks {
        let start = h.new_start as usize;
        for i in 0..h.new_len as usize {
            writers.push(
                replayed
                    .histories
                    .get(start + i)
                    .cloned()
                    .unwrap_or_default(),
            );
        }
    }
    writers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attribute;
    use Actor::{Agent, Human};

    fn hunk(old_start: u32, old_len: u32, new_start: u32, new_len: u32) -> Hunk {
        Hunk {
            old_start,
            old_len,
            new_start,
            new_len,
        }
    }

    #[test]
    fn agent_insertion_then_human_takeover() {
        // Base: 2 pre-session lines. A_0 inserts 2 lines at position 1.
        let edits = [
            TurnEdit {
                actor: Agent,
                hunk: hunk(1, 0, 1, 2),
            },
            // H_1: the human rewrites one of the agent's lines (position 1).
            TurnEdit {
                actor: Human,
                hunk: hunk(1, 1, 1, 1),
            },
        ];
        let r = replay(2, &edits);
        assert_eq!(r.histories.len(), 4);
        assert!(!r.diverged);
        // line 1: [Agent, Human] — human took it over; line 2: [Agent].
        let writers = committed_writers(&r, &[hunk(0, 4, 0, 4)], 4);
        let a = attribute(&writers, r.agent_removed_by_human);
        assert_eq!(a.agent_lines, 1);
        assert_eq!(a.human_lines, 1);
        assert_eq!(a.human_modified_agent_lines, 1);
        assert_eq!(a.unknown_lines, 2, "pre-session lines are unattributed");
    }

    #[test]
    fn human_deleting_agent_lines_counts_removal() {
        let edits = [
            TurnEdit {
                actor: Agent,
                hunk: hunk(0, 0, 0, 3),
            },
            TurnEdit {
                actor: Human,
                hunk: hunk(0, 3, 0, 1),
            },
        ];
        let r = replay(0, &edits);
        assert_eq!(r.histories.len(), 1);
        assert_eq!(r.agent_removed_by_human, 2);
    }

    #[test]
    fn out_of_bounds_edit_marks_divergence_and_unknowns() {
        let edits = [TurnEdit {
            actor: Agent,
            hunk: hunk(10, 5, 10, 5),
        }];
        let r = replay(2, &edits);
        assert!(r.diverged);
        let writers = committed_writers(&r, &[hunk(0, 2, 0, 2)], 2);
        assert!(writers.iter().all(Vec::is_empty), "diverged → unknown");
    }

    #[test]
    fn length_mismatch_yields_unknowns() {
        let r = replay(3, &[]);
        // committed file has 5 lines but the model has 3 → capture gap.
        let writers = committed_writers(&r, &[hunk(0, 1, 0, 2)], 5);
        assert_eq!(writers.len(), 2);
        assert!(writers.iter().all(Vec::is_empty));
    }

    proptest::proptest! {
        /// Replay is total and conserves: histories length equals base plus the
        /// net insertion of every applied (in-bounds) edit; never panics.
        #[test]
        fn replay_is_total(base in 0u32..40, raw_edits in proptest::collection::vec(
            (proptest::bool::ANY, 0u32..50, 0u32..6, 0u32..6), 0..12)) {
            let edits: Vec<TurnEdit> = raw_edits.iter().map(|(agent, start, ol, nl)| TurnEdit {
                actor: if *agent { Agent } else { Human },
                hunk: hunk(*start, *ol, *start, *nl),
            }).collect();
            let r = replay(base, &edits);
            // total: no panic, and the model is never larger than base + all insertions
            let max_growth: usize = edits.iter().map(|e| e.hunk.new_len as usize).sum();
            proptest::prop_assert!(r.histories.len() <= base as usize + max_growth);
        }
    }
    /// **A consumed hunk replayed over its own parent over-lengthens the
    /// model** — the mechanism behind the all-unknown blame.
    /// Parent = 3 lines. Replaying the hunk an EARLIER commit already
    /// consumed (`+2` at line 1) together with the two hunks that are really
    /// new (`+1` at 3, `+1` at 4) grows the model to 7 lines against a 5-line
    /// commit, and `committed_writers` degrades every committed line to
    /// `unknown`. The same replay with the consumed hunk dropped lands on 5
    /// and names both writers.
    #[test]
    fn a_consumed_hunk_replayed_over_its_own_parent_over_lengthens_the_model() {
        let consumed = TurnEdit {
            actor: Agent,
            hunk: hunk(1, 0, 1, 2),
        };
        let fresh = [
            TurnEdit {
                actor: Human,
                hunk: hunk(3, 0, 3, 1),
            },
            TurnEdit {
                actor: Agent,
                hunk: hunk(4, 0, 4, 1),
            },
        ];
        let c_hunks = [hunk(3, 0, 3, 2)];

        let mut all = vec![consumed];
        all.extend(fresh.iter().copied());
        let over = replay(3, &all);
        assert_eq!(
            over.histories.len(),
            7,
            "the consumed hunk is applied twice"
        );
        let writers = committed_writers(&over, &c_hunks, 5);
        assert!(writers.iter().all(Vec::is_empty), "{writers:?}");

        let exact = replay(3, &fresh);
        assert_eq!(exact.histories.len(), 5);
        let writers = committed_writers(&exact, &c_hunks, 5);
        assert_eq!(writers, vec![vec![Human], vec![Agent]]);
    }
}
