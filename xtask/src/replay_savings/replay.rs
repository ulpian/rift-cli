//! The walk: every session tree, every window, every call, in one order.
//!
//! This is where the four substitutions a replay makes are applied — the
//! synthetic revision, the worktree's line count, the larger slice reading and
//! the whole-file write's coverage — and where a call is routed to the channel
//! it belongs to.

use std::collections::{BTreeMap, BTreeSet};

use rift_agent::respond::Channel;
use rift_agent::window_journal::{EvictReason, Interval, Journal, Limits, Line};
use rift_agent::{AgentAdapter, EditKind};
use rift_agent_claude_code::ClaudeCodeAdapter;
use rift_core::Sha256Id;
use serde_json::{json, Value};

use super::bucket::Bucket;
use super::classify::classify_reads;
use super::options::Options;
use super::report::{finish, land, Report};
use super::transcript::{load_window, tree_stems, tree_windows, Call, Event};
use super::worktree::{path_key, FileFacts, Worktree};

/// The command words that mean a shell call was probably reading a file.
///
/// Used for one thing only: telling a shell read the pinned grammar did not
/// recognise from a shell call that was never a read at all. It is a heuristic
/// and is published as one — it decides nothing, and its only effect on the
/// report is which informational line a Bash call is counted on.
const SHELL_READ_WORDS: [&str; 17] = [
    "awk", "bat", "batcat", "cat", "egrep", "fgrep", "grep", "head", "hexdump", "less", "more",
    "nl", "od", "rg", "sed", "tail", "xxd",
];

/// How many lines a stretch of text spans, counted the way the gate counts a
/// file's.
fn line_span(text: &str) -> u32 {
    u32::try_from(text.split_inclusive('\n').count()).unwrap_or(u32::MAX)
}

/// The synthetic revision hash: a file's name and how many times it has been
/// changed in this session tree.
pub(super) fn revision_hash(key: &str, revision: u32) -> Sha256Id {
    rift_agent::content_sha256(format!("{key}\u{1}{revision}").as_bytes())
}

/// What one session tree carries across its windows.
#[derive(Default)]
pub(super) struct TreeState {
    /// How many times each path has been changed.
    pub(super) revisions: BTreeMap<String, u32>,
    /// The line count a whole-file write established, where one did.
    pub(super) written_lines: BTreeMap<String, u32>,
    /// Which windows read each `(path, revision)` first, and what that first
    /// read cost them.
    pub(super) seen: BTreeMap<(String, u32), Vec<(usize, u64)>>,
}

/// The settings every call in one replay is decided under, spelled once.
pub(super) struct Replay<'a> {
    adapter: &'a ClaudeCodeAdapter,
    pub(super) limits: Limits,
    pub(super) channel: Option<Channel>,
}

/// One context window's replay state.
#[derive(Default)]
pub(super) struct WindowState {
    /// The pure journal this window's decisions are taken over.
    pub(super) journal: Journal,
    /// The turn the next call is made in.
    pub(super) turn: u32,
    /// Paths this window changed with something other than a whole-file write
    /// and has not read since.
    pub(super) awaiting_verification: BTreeSet<String>,
}

/// Everything one call is replayed against: the settings it is decided under,
/// which window made it, and the four states it folds into.
///
/// One parameter rather than seven, so each phase of a call can be a function
/// of its own and still have a signature a reader gets through.
pub(super) struct Ctx<'a> {
    /// The settings the decision is taken under.
    pub(super) replay: &'a Replay<'a>,
    /// Which window of the session tree made the call.
    pub(super) window: usize,
    /// That window's own journal and turn.
    pub(super) state: &'a mut WindowState,
    /// What the session tree carries across its windows.
    pub(super) tree: &'a mut TreeState,
    /// The checkout the recorded paths are resolved against.
    pub(super) worktree: &'a mut Worktree,
    /// Everything the replay has concluded so far.
    pub(super) report: &'a mut Report,
}

/// Replay every transcript under `opts.transcripts`.
///
/// # Errors
/// If the transcripts directory cannot be read.
pub fn replay(opts: &Options) -> Result<Report, String> {
    let adapter = ClaudeCodeAdapter;
    let settings = Replay {
        adapter: &adapter,
        limits: Limits::default(),
        channel: deny_channel(),
    };
    let mut worktree = Worktree::new(&opts.repo);
    let mut report = Report {
        tokenizer: rift_tokens::provenance(),
        ..Report::default()
    };
    for stem in tree_stems(&opts.transcripts)? {
        let paths = tree_windows(&opts.transcripts, &stem);
        if paths.is_empty() {
            continue;
        }
        report.trees = report.trees.saturating_add(1);
        let mut tree = TreeState::default();
        let mut windows: Vec<WindowState> = Vec::new();
        // One order over every window of the tree, so a revision advanced in
        // one window is the revision the next window's read is keyed by.
        let mut merged: Vec<(String, usize, usize, Event)> = Vec::new();
        for (index, path) in paths.iter().enumerate() {
            let window = load_window(path)?;
            windows.push(WindowState::default());
            report.windows = report.windows.saturating_add(1);
            for (stamp, at, event) in window.events {
                merged.push((stamp, index, at, event));
            }
        }
        merged.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
        for (_, index, _, event) in merged {
            let Some(state) = windows.get_mut(index) else {
                continue;
            };
            match event {
                Event::Turn => state.turn = state.turn.saturating_add(1),
                Event::Evict => {
                    report.evictions = report.evictions.saturating_add(1);
                    state.journal.push(Line::Evict {
                        turn: state.turn,
                        reason: EvictReason::Compact,
                        at_ms: 0,
                    });
                }
                Event::Call(call) => replay_call(
                    &mut Ctx {
                        replay: &settings,
                        window: index,
                        state,
                        tree: &mut tree,
                        worktree: &mut worktree,
                        report: &mut report,
                    },
                    &call,
                ),
            }
        }
        fold_cross_window(&tree, &mut report);
    }
    finish(&mut report);
    Ok(report)
}

/// The channel a decline travels on for this family, read from the pinned
/// capability table rather than restated here.
fn deny_channel() -> Option<Channel> {
    rift_agent::respond::CAPABILITIES
        .iter()
        .find(|(agent, verb, channel)| {
            *agent == ClaudeCodeAdapter::AGENT_ID
                && *verb == "PreToolUse"
                && matches!(channel, Channel::DenyWithReason { .. })
        })
        .map(|(_, _, channel)| *channel)
}

/// Fold one tool call into the report, on the one channel it belongs to.
fn replay_call(ctx: &mut Ctx<'_>, call: &Call) {
    ctx.report.tool_calls = ctx.report.tool_calls.saturating_add(1);
    let native = json!({ "tool_name": call.name, "tool_input": call.input });
    let text = call.result.clone().unwrap_or_default();
    let tokens = u64::from(rift_tokens::count(&text));
    if call.result.is_some() {
        ctx.report.all_results.add(tokens);
    }
    if call.name == "Bash" {
        count_shell_call(ctx.report, call);
    }
    // A change first: a read in the same call is not a thing, and the revision
    // a later read is keyed by has to have moved before the read arrives.
    if let Some(edit) = ctx.replay.adapter.edit_request("PreToolUse", &native) {
        apply_edit(ctx, &edit, call);
        return;
    }
    // Every read the call performs, not just its first. One shell command
    // line is routinely several — `cd <dir> && cat a && cat b` is one call and
    // three facts — and a member left out here is a read the window is never
    // credited with, and one the all-or-nothing rule below would never see.
    let requests = ctx.replay.adapter.read_requests("PreToolUse", &native);
    if !requests.is_empty() {
        classify_reads(ctx, call, &requests, &text, tokens);
        return;
    }
    if ctx
        .replay
        .adapter
        .search_request("PreToolUse", &native)
        .is_some()
    {
        land(ctx.report, Bucket::Search, tokens);
        return;
    }
    // A read tool whose arguments step outside the pin is still a read of
    // repository content; it is simply one no decision may be taken about.
    if call.name == "Read" || call.name == "NotebookRead" {
        land(ctx.report, Bucket::NotReadShaped, tokens);
        return;
    }
    if call.name == "Bash" && note_an_invisible_shell_read(ctx, call, tokens) {
        return;
    }
    let seen = ctx.report.other_tools.entry(call.name.clone()).or_default();
    *seen = seen.saturating_add(1);
}

/// Count one shell call, and whether the pinned grammar recognised it at all.
///
/// Counted before anything else routes the call away: how much of the shell
/// the pinned grammar sees is the first thing a change to that grammar moves,
/// and a recognised read leaves through the read path rather than through the
/// shell branch at the end of [`replay_call`].
fn count_shell_call(report: &mut Report, call: &Call) {
    report.shell_calls = report.shell_calls.saturating_add(1);
    if call
        .input
        .get("command")
        .and_then(Value::as_str)
        .and_then(rift_agent::shell_read::recognise)
        .is_some()
    {
        report.shell_recognised = report.shell_recognised.saturating_add(1);
    }
}

/// Count a shell call the grammar said nothing about that looks like a read,
/// answering whether it was counted here.
///
/// Whatever the grammar recognises as reads left through the read path,
/// members with no derivable range included — those arrive there as an
/// unpinned span. What reaches here is a command line the grammar said nothing
/// about at all.
fn note_an_invisible_shell_read(ctx: &mut Ctx<'_>, call: &Call, tokens: u64) -> bool {
    let command = call.input.get("command").and_then(Value::as_str);
    if command
        .and_then(rift_agent::shell_read::recognise)
        .is_some()
        || !looks_like_a_shell_read(&call.input)
    {
        return false;
    }
    ctx.report.invisible_shell.add(tokens);
    if command.is_some_and(|command| names_a_work_tree_file(command, call, ctx.worktree)) {
        ctx.report.invisible_shell_named_file.add(tokens);
    }
    true
}

/// Advance a path's revision, and carry coverage forward where — and only
/// where — a whole-file write proves the window holds the new bytes.
///
/// The new file's line count is taken from the payload the call carried, which
/// is the one moment a replay knows a file's shape better than the checkout
/// does. Everything else records the change and no coverage: the guide's edit
/// echo needs the new bytes to find a replacement's site in, and a replay has
/// only the bytes the checkout has today.
fn apply_edit(ctx: &mut Ctx<'_>, edit: &rift_agent::EditRequest, call: &Call) {
    ctx.report.edits = ctx.report.edits.saturating_add(1);
    let Some(key) = path_key(&edit.path, call.cwd.as_deref()) else {
        return;
    };
    let before = ctx
        .tree
        .written_lines
        .get(&key)
        .copied()
        .or_else(|| ctx.worktree.facts(&key).map(FileFacts::total_lines));
    let revision = ctx.tree.revisions.entry(key.clone()).or_default();
    let hash_old = revision_hash(&key, *revision);
    *revision = revision.saturating_add(1);
    let hash_new = revision_hash(&key, *revision);
    let written = match &edit.kind {
        EditKind::Write { .. } => call
            .input
            .get("content")
            .and_then(Value::as_str)
            .map(line_span)
            .filter(|lines| *lines > 0),
        _ => None,
    };
    match written {
        Some(lines) => {
            ctx.tree.written_lines.insert(key.clone(), lines);
        }
        None => {
            ctx.tree.written_lines.remove(&key);
        }
    }
    let sites: Vec<Interval> = written
        .and_then(|lines| Interval::new(1, lines))
        .into_iter()
        .collect();
    ctx.state.journal.push(Line::Edit {
        turn: ctx.state.turn,
        path: key.clone(),
        hash_old: Some(hash_old),
        hash_new,
        sites,
        shift: i64::from(written.unwrap_or_default()) - i64::from(before.unwrap_or_default()),
        full: written.is_some(),
        at_ms: 0,
    });
    if written.is_some() {
        ctx.state.awaiting_verification.remove(&key);
    } else {
        ctx.state.awaiting_verification.insert(key);
    }
}

/// Whether a shell command line has a read word where a command goes.
///
/// A heuristic, and the only one in this command: it decides nothing and only
/// chooses which informational line an unrecognised Bash call is counted on.
fn looks_like_a_shell_read(input: &Value) -> bool {
    let Some(command) = input.get("command").and_then(Value::as_str) else {
        return false;
    };
    command
        .split(['|', ';', '\n', '&'])
        .filter_map(|segment| segment.split_whitespace().next())
        .map(|word| word.rsplit('/').next().unwrap_or(word))
        .any(|word| SHELL_READ_WORDS.contains(&word))
}

/// Whether any word of a command line names a file this checkout has.
///
/// A directory does not count. `cargo test -p xtask` names a directory of this
/// workspace and read no file; `grep -n fn xtask/src/lib.rs` names a file and
/// printed part of it. The difference is the whole value of the tighter bound,
/// so the looser reading is not allowed to creep into it.
fn names_a_work_tree_file(command: &str, call: &Call, worktree: &mut Worktree) -> bool {
    command
        .split_whitespace()
        .map(|word| word.trim_matches(['\'', '"', '`', ',', ';']))
        .filter(|word| !word.is_empty() && !word.starts_with('-'))
        .filter_map(|word| path_key(word, call.cwd.as_deref()))
        .any(|key| worktree.facts(&key).is_some())
}

/// What a session tree's windows read of each other.
fn fold_cross_window(tree: &TreeState, report: &mut Report) {
    for readers in tree.seen.values() {
        let distinct: BTreeSet<usize> = readers.iter().map(|(window, _)| *window).collect();
        if distinct.len() < 2 {
            continue;
        }
        report.cross_window_pairs = report.cross_window_pairs.saturating_add(1);
        let mut first = BTreeSet::new();
        for (window, tokens) in readers {
            if first.insert(*window) && first.len() > 1 {
                report.cross_window.add(*tokens);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{looks_like_a_shell_read, replay, Report};
    use crate::replay_savings::bucket::Bucket;
    use crate::replay_savings::tests::fixture;

    /// The committed corpus, replayed. Every test below pins its own subject
    /// against this one reading, so what the buckets separate is not a claim
    /// about one run but a property of the corpus.
    fn replayed() -> Report {
        let tmp = tempfile::tempdir().unwrap();
        replay(&fixture(tmp.path())).unwrap()
    }

    #[test]
    fn the_fixture_pins_every_classification_bucket() {
        let report = replayed();
        let count = |b: Bucket| report.bucket(b).calls;
        assert_eq!(report.trees, 1, "one session tree");
        assert_eq!(report.windows, 2, "a main window and one subagent");
        assert_eq!(count(Bucket::FirstSight), 9, "first sights");
        assert_eq!(count(Bucket::Declined), 5, "declines");
        assert_eq!(count(Bucket::BelowFloor), 2, "under the floor");
        assert_eq!(count(Bucket::RepeatAfterDecline), 1, "the allowed repeat");
        assert_eq!(count(Bucket::RepeatOverlapping), 1, "overlapping repeat");
        assert_eq!(count(Bucket::RepeatDisjoint), 1, "disjoint repeat");
        assert_eq!(count(Bucket::OverCreditBound), 1, "past the credit bound");
        assert_eq!(
            count(Bucket::CreditedFromStatement),
            1,
            "past the bound and credited from the client's own statement"
        );
        assert_eq!(count(Bucket::SpilledToAFile), 1, "spilled to a file");
        assert_eq!(count(Bucket::UnknownSpan), 1, "an unpinned span");
        assert_eq!(count(Bucket::NotReadShaped), 1, "outside the pinned fields");
        assert_eq!(count(Bucket::NoLineCount), 2, "no name or line count");
        assert_eq!(count(Bucket::ClientDedup), 1, "the client's own guard");
        assert_eq!(count(Bucket::NoResultRecorded), 1, "a call with no result");
        assert_eq!(count(Bucket::Search), 2, "a glob and a grep");
        assert_eq!(count(Bucket::NoHonestFrame), 0, "nothing frameless here");
        assert_eq!(
            count(Bucket::ObservedNotDeclined),
            0,
            "the floor is the only observer"
        );
    }

    #[test]
    fn the_fixture_pins_what_the_shell_grammar_sees() {
        let report = replayed();
        assert_eq!(report.invisible_shell.calls, 2, "two unrecognised reads");
        assert_eq!(
            report.invisible_shell_named_file.calls, 1,
            "one of them names a file this checkout has"
        );
        assert_eq!(report.shell_calls, 8, "every shell call");
        assert_eq!(report.shell_recognised, 6, "what the grammar recognised");
        assert_eq!(report.outside_work_tree.calls, 1, "out of scope");
    }

    #[test]
    fn the_fixture_pins_the_corpus_it_was_replayed_from() {
        let report = replayed();
        assert_eq!(report.evictions, 1, "one compaction");
        assert_eq!(report.edits, 3, "a write, a replacement and a multi-edit");
        assert_eq!(report.tool_calls, 37, "every tool call replayed");
        assert_eq!(report.all_results.calls, 36, "one call returned nothing");
    }

    /// The two levers a call is more than one decision under.
    #[test]
    fn the_fixture_pins_the_clients_own_delivery_statement() {
        let report = replayed();
        assert_eq!(
            report.deliveries_credited, 1,
            "one delivery the ceiling refused and the statement vouched for"
        );
        assert_eq!(
            report.deliveries_taken_back, 1,
            "one delivery the statement cut back"
        );
        assert_eq!(
            report.results_with_a_response, 2,
            "two results carry an object"
        );
        assert_eq!(
            report.statements_read, 2,
            "the rule reads a statement out of both"
        );
    }

    #[test]
    fn the_fixture_pins_the_calls_that_perform_several_reads() {
        let report = replayed();
        assert_eq!(
            report.calls_with_several_reads, 2,
            "two chained command lines"
        );
        assert_eq!(
            report.read_members, 29,
            "every read every repository-content call performs"
        );
        assert_eq!(
            report.declines_lost_to_a_sibling, 1,
            "one held member allowed because its sibling was not held"
        );
    }

    #[test]
    fn the_fixture_pins_the_floor_and_what_rests_on_a_write() {
        let report = replayed();
        assert_eq!(report.cross_window_pairs, 1, "one pair read twice");
        assert_eq!(
            report.verification_after_edit.calls, 1,
            "one unmodelled echo"
        );
        assert_eq!(
            report.declines_resting_on_a_write, 1,
            "one decline off a write"
        );
        assert_eq!(
            report.first_read_floor.calls, 10,
            "the floor is every first sight, credited from the bytes or from the statement"
        );
    }

    #[test]
    fn the_shell_read_heuristic_reads_command_positions_only() {
        let command = |text: &str| json!({ "command": text });
        assert!(looks_like_a_shell_read(&command("cargo test | grep error")));
        assert!(looks_like_a_shell_read(&command("/usr/bin/cat a b c")));
        assert!(!looks_like_a_shell_read(&command("cargo test --lib")));
        assert!(!looks_like_a_shell_read(&command(
            "cargo run -- --pattern head"
        )));
        assert!(!looks_like_a_shell_read(&json!({})));
    }
}
