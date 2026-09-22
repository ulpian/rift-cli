//! `rift blame <file>` — which checkpoint, session and agent explain each line.
//!
//! The chain is: `git blame --line-porcelain` gives a commit per line; the
//! commit's message gives every checkpoint it *claims*
//! (`rift_shadow_attribution::provenance::parse_checkpoint_claims`); the
//! projection resolves each claim into a candidate; and `select_for_file`
//! chooses the one that explains this file, carrying the basis for the choice
//! so a guess is never printed as evidence.
//!
//! # Two things this module is careful about
//!
//! **Untrusted input.** A commit message is free text written by anyone who has
//! ever pushed to the repository, `git blame`'s porcelain hands us the author
//! name, mail and subject alongside the oid, and **a filename is
//! attacker-controlled in any repository you clone** — a path is an argument
//! here, but it is an argument the operator copied from a directory listing.
//! What reaches a terminal is exactly: line numbers, oids, checkpoint ids (all
//! validated lowercase hex by `parse_checkpoint_claims`), counts, agent/model
//! labels — which are [`Label`]s, a type with no constructor that does not
//! sanitize — and the requested path through [`safe_path`]. Author, mail and
//! subject are dropped at the *parse boundary* rather than filtered
//! downstream, and `ClaimSet` is structurally incapable of carrying a rejected
//! value — it counts them.
//!
//! The property behind that list, stated as narrowly as it is true: **no byte
//! an attacker chose reaches an operator as anything but text inside a line
//! this module wrote.** A path holding a newline and a plausible blame row
//! would otherwise print exactly such a row, asserting authorship that only
//! recorded evidence is allowed to assert. `safe_path` and `Label` are the two
//! places that is enforced, and both are pinned by tests that fail on the
//! rendered output rather than on the sanitizer alone.
//!
//! The blanket version of that sentence — "in a form indistinguishable from a
//! fact this module recorded" — is stronger than anything here can deliver, and
//! saying so is the point, because [`safe_path`] already admitted it one level
//! down and the two must not disagree. **A terminal narrower than the header
//! wraps it, and a wrapped continuation is a line this module did not write.**
//! With [`safe_text::MAX_PATH`] at 160 against the 12-character `rift blame "`
//! prefix, an 80-column terminal leaves 68 attacker-chosen characters on the first line
//! and a full 80 on the second — room for a plausible row. No constant closes
//! that: a bound that defeated wrapping would have to be smaller than a width
//! this program cannot know, and the largest that never wraps at 80 columns
//! (66) would truncate ordinary paths in this workspace, whose deepest is 100
//! characters.
//!
//! `--json` is a different surface with a different guarantee. It carries the
//! caller's path and the repository's spelling **byte-exact**, so what it
//! promises is that an attacker's bytes cannot change the document's shape —
//! not that they are absent from it. What survives raw is named and asserted in
//! `the_json_document_cannot_be_given_a_shape_the_renderer_did_not_build`.
//!
//! **Work per line vs work per commit.** A file has thousands of lines and a
//! handful of commits. Every message parse and every projection read is
//! memoised by commit oid and checkpoint id, and adjacent lines with the same
//! answer collapse into one span before anything is rendered — so the cost is
//! O(distinct commits), not O(lines). Doing it per line is the specific
//! weakness this verb exists to beat.
//!
//! # The submodules, and the line each of them holds
//!
//! [`safe_text`] owns what may reach a terminal; [`mod@blame`] owns the git side
//! and the parse boundary where untrusted output stops being untrusted;
//! [`resolve`] owns the projection reads and the caching that makes them
//! affordable; [`render`] and [`mod@why`] own the two documents and their two
//! separately versioned contracts; [`worktree`] owns the member labels, which
//! are total over a projection that predates the relations; and [`capture`]
//! owns the one reader that opens a projection of its own.

mod blame;
mod capture;
mod render;
mod resolve;
mod safe_text;
mod why;
mod worktree;

pub(crate) use blame::blame;
pub(crate) use capture::CaptureProvenance;
pub(crate) use resolve::commit_claims;
pub(crate) use safe_text::{safe_excerpt, safe_path, Label};
pub(crate) use why::{prompt_from_shard, session_cost, why, CostView, PromptView, ShardPrompt};
pub(crate) use worktree::{
    checkpoint_worktree, record_worktree_observation, session_worktree, session_worktree_history,
    spread_text, worktree_label, worktree_spread, UNKNOWN_WORKTREE,
};

#[cfg(test)]
mod tests {
    //! Seeders several of this module's test modules share.

    use std::collections::HashMap;

    use rift_shadow::attribution::provenance::ClaimSet;

    use super::render::render;
    use super::resolve::{CheckpointFacts, Reads, Resolved};
    use crate::cli::Outcome;

    /// A 40-character oid made of one repeated character, so a test's commits
    /// are told apart by eye.
    pub(super) fn oid(c: char) -> String {
        std::iter::repeat_n(c, 40).collect()
    }

    /// [`render`] with no `--line` range — what every pre-range test meant.
    pub(super) fn render_whole(
        file: &str,
        resolved: &[Resolved],
        claims: &HashMap<String, ClaimSet>,
        facts: &HashMap<String, CheckpointFacts>,
        reads: Reads,
    ) -> Outcome {
        render(file, None, resolved, claims, facts, reads)
    }
}
