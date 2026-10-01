//! `cargo run -p xtask -- replay-savings --transcripts <dir>` — what this
//! build's read gate would have decided about sessions that already happened.
//!
//! The savings figures Rift publishes are booked from decisions it took while
//! an agent waited. Before a change to the read grammar or the window journal
//! can be argued for, somebody has to be able to say what the change is worth
//! on real traffic, and the only traffic that is not a benchmark is the
//! owner's own recorded sessions. This replays them: no model is called, no
//! store is opened, nothing is written, and every number comes out the same on
//! two runs of the same input.
//!
//! **It reads private transcripts and prints aggregates.** The files under a
//! client's project directory hold the owner's prompts, their paths and their
//! tool output. Nothing here renders any of that: the report is counts, token
//! totals and ratios, and a path never reaches an output line. That is a
//! property a test asserts rather than a habit — see
//! `no_prompt_text_reaches_the_output`.
//!
//! ## What it is measuring against
//!
//! The recogniser and the decision are the shipped ones, reached through the
//! real Claude Code adapter and the pure window journal, so the harness
//! reflects whatever grammar the checkout it is built from has. That is how a
//! before and an after are produced: run it on `main`, run it on the branch,
//! compare the two tables. Nothing about the buckets below is a second opinion
//! about what a read is.
//!
//! ## The four places a replay has to stand in for something
//!
//! A hook sees a file at the instant of the call. A replay does not, so four
//! substitutions are made, each chosen so an error costs a saving rather than
//! producing one:
//!
//! 1. **Revisions are synthetic.** A file's revision is a counter that starts
//!    at zero and advances every time a mutating tool call names it; the hash
//!    a decision is keyed by is `sha256(path, counter)`. Two reads of one
//!    counter value are two reads of one revision, which is the only thing
//!    the journal asks of a hash.
//! 2. **Line counts come from the worktree**, or from a whole-file write's own
//!    payload when the window created the file. A path with neither is left
//!    undecided rather than guessed at. Given `--history <git-dir>`, the
//!    worktree's place is taken by the newest commit, across the first-parent
//!    chains of the `--history-ref`s (`HEAD` when none is named), whose
//!    committer time is at or before the window's first event; the worktree
//!    answers only for a path that commit does not have and the worktree does,
//!    and for a window older than every commit. Both are counted and printed,
//!    and the header names which mode ran.
//! 3. **A slice is measured at the larger** of what the worktree (or the
//!    window's commit) holds for those lines and what the result actually
//!    returned. Both readings are
//!    imperfect — the worktree has moved on, the result carries the read tool's
//!    line-number decoration — and the larger of the two is the one that
//!    pushes a call over a ceiling rather than under it. Past that ceiling the
//!    gate credits nothing from the bytes, and the client's own statement is
//!    the only thing that can stand in — see below.
//! 4. **Only a whole-file write carries coverage forward.** The guide's edit
//!    echo locates a replacement's text in the new bytes; a replay has no new
//!    bytes, so a replacement edit records no coverage here even where the
//!    shipped gate would have found the site. The reads that lands in the
//!    first-sight pool are counted and published on their own line, so the
//!    size of what this harness declines to claim is visible rather than
//!    silently absent.
//!
//! ## Two levers a call is more than one decision under
//!
//! A tool call used to be one read and one decision. It is neither any more,
//! and a replay that modelled it as both would report an after-figure smaller
//! than the build actually achieves.
//!
//! **A call performs every read its command line spells.** `cd <dir> && cat a
//! && cat b` is one call and three facts, so the harness replays every request
//! [`AgentAdapter::read_requests`](rift_agent::AgentAdapter::read_requests) yields
//! and applies the binary's own
//! all-or-nothing rule: the call is declined only when every member of it
//! resolved and every member was held. One member the window does not hold —
//! or one nobody could resolve at all — and the whole line runs, because a
//! decline answers the command line and the agent would lose the reads that
//! were never in question. What that costs is published as
//! `declines lost to an unheld sibling read` rather than absorbed.
//!
//! **A call is decided before it runs and reconciled after it.** At the result
//! the client states how much of the file it handed over, and
//! [`window_journal::reconcile_delivery`](rift_agent::window_journal::reconcile_delivery)
//! moves the window's coverage to agree
//! with it — forward, which is the only route by which a delivery too large
//! for the byte ceiling to vouch for ever becomes coverage, and back, where
//! the result says it was cut short. The harness reads that statement off each
//! result record's `toolUseResult` and applies the same rule, so a whole-file
//! read past the ceiling now leaves coverage behind and the next read of those
//! lines has something to be decided against. The effect has its own bucket.
//!
//! ## The buckets
//!
//! Every repository-content call lands in exactly one [`Bucket`], and the
//! buckets are what the share is computed over. Reads of files outside the
//! work tree are out of scope and leave the denominator altogether, the way
//! scope containment takes them out of every other figure.
//!
//! ## What it sizes beyond this build
//!
//! Four sections measure the pools later levers would work on rather than
//! anything this build decides: the client's own declines beside this
//! build's; the sibling re-reads split by the route a window could have
//! inherited the bytes through ([`Inheritance`]), one count per later
//! window's first read of a revision a sibling had read first; the reads of machine-written
//! files by path shape ([`MachineFormat`]); and the first sights by the size of
//! the file read, with the first-contact floor of 400 lines or 16,384 bytes.
//! `--artifact <dir>` writes the table and the document with the build, the
//! facts mode, the corpus and the run's times attached.
//!
//! ## What narrowing would have done
//!
//! The buckets are decided with narrowing off, so every figure above reads the
//! same before and after it: the shipped decision with
//! [`Limits::narrow_mode`](rift_agent::window_journal::Limits::narrow_mode) at
//! `off` is the build without it. The two repeat pools a narrowing works on
//! are split beneath by how each request stands against what is held, and one
//! line asks the same decision, against the same journal, with narrowing
//! enforced: what it would have declined, sized as the held lines' share of
//! the result net of the frames.

mod artifact;
mod bucket;
mod census;
#[cfg(test)]
mod census_fixture;
mod classify;
mod delivery;
mod history;
mod inheritance;
#[cfg(test)]
mod narrow_fixture;
mod options;
mod render;
mod replay;
mod report;
mod sections;
mod transcript;
mod worktree;

pub use artifact::{write_artifact, Provenance};
pub use bucket::Bucket;
pub use census::{CensusRow, FirstSightSizes, MachineFormat};
pub use inheritance::Inheritance;
pub use options::{parse_args, run, Options};
pub use render::{render_json, render_table, SCHEMA_VERSION};
pub use replay::replay;
pub use report::{share_percent, Report, Tally};
pub use worktree::FactsSource;

#[cfg(test)]
pub(super) mod tests {
    use std::path::Path;

    use super::Options;

    /// The synthetic corpus, and the checkout it is replayed against.
    pub(super) fn fixture(tmp: &Path) -> Options {
        let repo = tmp.join("repo");
        let write = |rel: &str, body: &str| {
            let path = repo.join(rel);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(path, body).unwrap();
        };
        let lines = |n: usize| -> String {
            use std::fmt::Write as _;
            (1..=n).fold(String::new(), |mut out, i| {
                let _ = writeln!(out, "line {i} of plain prose text");
                out
            })
        };
        write("src/lib.rs", &lines(60));
        write("src/util.rs", &lines(50));
        write("src/extra.rs", &lines(80));
        write("src/small.rs", &lines(10));
        write("docs/notes.md", &lines(100));
        // One file past the read tool's credit bound, generated rather than
        // committed: a fixture does not have to carry fifty kilobytes to pin
        // a ceiling.
        write("src/wide.rs", &lines(2600));
        Options {
            transcripts: Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("fixtures")
                .join("replay-savings"),
            repo,
            json: false,
            history: None,
            history_refs: vec!["HEAD".to_owned()],
            artifact: None,
        }
    }

    /// The sentinel the committed fixture's prompts carry.
    pub(super) const SENTINEL: &str = "REPLAYSENTINELdoNOTprintTHIS";
}
