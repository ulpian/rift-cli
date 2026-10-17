//! The walk: every session tree, every window, every call, in one order.
//!
//! This is where the four substitutions a replay makes are applied — the
//! synthetic revision, the worktree's line count, the larger slice reading and
//! the whole-file write's coverage — and where a call is routed to the channel
//! it belongs to.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rift_agent::respond::Channel;
use rift_agent::window_journal::{EvictReason, Interval, Journal, Limits, Line, Mode};
use rift_agent::{AgentAdapter, EditKind};
use rift_agent_claude_code::ClaudeCodeAdapter;
use rift_core::Sha256Id;
use serde_json::{json, Value};

use super::bucket::Bucket;
use super::by_name_split::ByNameOrigin;
use super::classify::classify_reads;
use super::family_held::{case_keys, fold_family, fold_uncommitted_family, FamilyLog};
use super::file_order::FileOrder;
use super::history::History;
use super::inheritance::{
    inheritance_of, leads, opening_section, Inheritance, WindowLead, SPAWNING_TOOLS,
};
use super::no_line_count_split::WorkingTree;
use super::options::Options;
use super::output_recognition::{fold_the_output, match_the_output, Before, OutputHoldings};
use super::pull_answers::pull_invocations;
use super::report::{finish, land, Report};
use super::shell_shape::count_shape;
use super::spawn_link::{note_the_spawn, open_the_window, openings, Opening, PendingSpawn};
use super::transcript::{load_window, tree_stems, tree_windows, Call, Event, Window};
use super::uncommitted_declines::note_an_edit;
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
    /// The byte count the same write established.
    pub(super) written_bytes: BTreeMap<String, u64>,
    /// Which windows read each `(path, revision)` first, and what that first
    /// read cost them.
    pub(super) seen: BTreeMap<(String, u32), Vec<(usize, u64)>>,
    /// What each window inherited from the one that started it.
    pub(super) leads: Vec<WindowLead>,
    /// The windows that spawned another: only their first reads' text is kept,
    /// because only a spawning window's read can have been pasted onward.
    pub(super) spawners: BTreeSet<usize>,
    /// What a spawning window's first read of a `(path, revision)` returned.
    pub(super) first_texts: BTreeMap<(String, u32, usize), String>,
    /// The windows each spawning call opened, by the call's id.
    pub(super) spawned_by: BTreeMap<String, Vec<usize>>,
    /// What the section each spawned window opens with would have named.
    pub(super) opening_named: BTreeMap<usize, BTreeSet<(String, String)>>,
    /// The spawn lines the session's main journal would hold.
    pub(super) spawns: Vec<PendingSpawn>,
    /// What each window's opening hook is handed, and whether it has opened.
    pub(super) openings: Vec<Opening>,
    /// What the family-held rows are folded from.
    pub(super) family: FamilyLog,
    /// The same, over the journal of files no commit holds.
    pub(super) uncommitted_family: FamilyLog,
    /// What the spawner handed down for each first read, by the window that
    /// made it, the path and the revision.
    pub(super) by_name_origin: BTreeMap<(usize, String, u32), ByNameOrigin>,
    /// The windows in file order, and what was noted of their calls.
    pub(super) order: FileOrder,
    /// The working tree the transcript proves.
    pub(super) working: WorkingTree,
    /// What the output match would have shown each window.
    pub(super) output: OutputHoldings,
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
    /// The commit this window's path facts come from, when the run was given
    /// a history and the window began after one.
    pub(super) commit: Option<usize>,
    /// Files this window's first contact would have been answered with an
    /// outline for, by the class it was counted in, until it reads them again.
    pub(super) outlined: BTreeMap<String, usize>,
    /// The journal of files no commit holds, kept apart from the one above so
    /// the rows built on that one read the same: it is fed only by what the
    /// transcript proves of a file's bytes.
    pub(super) uncommitted: Journal,
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
    /// When the call was made, as the transcript stamped it.
    pub(super) stamp: &'a str,
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
/// If the transcripts directory cannot be read, or the history given cannot
/// be opened or read.
pub fn replay(opts: &Options) -> Result<Report, String> {
    let adapter = ClaudeCodeAdapter;
    let settings = Replay {
        adapter: &adapter,
        // The buckets are the build without narrowing, so every figure they
        // carry reads the same before and after it; what narrowing would
        // have done is asked of the same decision on its own line.
        limits: Limits {
            narrow_mode: Mode::Off,
            ..Limits::default()
        },
        channel: deny_channel(),
    };
    let history = match &opts.history {
        Some(git_dir) => Some(History::open(git_dir, &opts.history_refs)?),
        None => None,
    };
    let mut worktree = Worktree::new(&opts.repo, history);
    let mut report = Report {
        tokenizer: rift_tokens::provenance(),
        ..Report::default()
    };
    for stem in tree_stems(&opts.transcripts)? {
        replay_tree(
            &settings,
            &mut worktree,
            &mut report,
            &opts.transcripts,
            &stem,
        )?;
    }
    report.facts = worktree.source()?;
    finish(&mut report);
    Ok(report)
}

/// Replay one session tree: every window of it, in one order.
fn replay_tree(
    settings: &Replay<'_>,
    worktree: &mut Worktree,
    report: &mut Report,
    dir: &Path,
    stem: &str,
) -> Result<(), String> {
    let paths = tree_windows(dir, stem);
    if paths.is_empty() {
        return Ok(());
    }
    report.trees = report.trees.saturating_add(1);
    let mut loaded: Vec<Window> = Vec::new();
    for path in &paths {
        loaded.push(load_window(path)?);
        report.windows = report.windows.saturating_add(1);
    }
    let path_refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
    let mut tree = TreeState {
        leads: leads(&path_refs, &loaded),
        spawned_by: spawned_by(&loaded),
        openings: openings(&loaded),
        order: FileOrder::of(&loaded),
        ..TreeState::default()
    };
    tree.spawners = tree.leads.iter().filter_map(|lead| lead.parent).collect();
    let parents: Vec<Option<usize>> = tree.leads.iter().map(|lead| lead.parent).collect();
    tree.family = FamilyLog::new(&path_refs, &parents);
    tree.uncommitted_family = FamilyLog::for_uncommitted(&path_refs, &parents);
    let mut windows: Vec<WindowState> = Vec::new();
    // One order over every window of the tree, so a revision advanced in
    // one window is the revision the next window's read is keyed by.
    let mut merged: Vec<(String, usize, usize, Event)> = Vec::new();
    for (index, window) in loaded.into_iter().enumerate() {
        let first_stamp = window
            .events
            .iter()
            .map(|(stamp, _, _)| stamp.as_str())
            .find(|stamp| !stamp.is_empty());
        windows.push(WindowState {
            commit: worktree.commit_for(first_stamp),
            ..WindowState::default()
        });
        for (stamp, at, event) in window.events {
            merged.push((stamp, index, at, event));
        }
    }
    merged.sort_by(|a, b| (&a.0, a.1, a.2).cmp(&(&b.0, b.1, b.2)));
    for (stamp, index, _, event) in merged {
        let Some(state) = windows.get_mut(index) else {
            continue;
        };
        open_the_window(state, &mut tree, report, index);
        let ctx = &mut Ctx {
            replay: settings,
            window: index,
            stamp: &stamp,
            state,
            tree: &mut tree,
            worktree,
            report,
        };
        replay_event(ctx, event);
        note_the_journals(&mut tree, &windows, index, &stamp);
    }
    fold_cross_window(&tree, settings.limits, report);
    tree.order.fold(report);
    fold_the_family(&tree, &windows, report);
    Ok(())
}

/// Stamp the lines window `index`'s two journals hold now, for the
/// family-held rows' liveness.
fn note_the_journals(tree: &mut TreeState, windows: &[WindowState], index: usize, stamp: &str) {
    let held = |lines: fn(&WindowState) -> usize| windows.get(index).map_or(0, lines);
    tree.family
        .note_journal_lines(index, held(|w| w.journal.lines().len()), stamp);
    tree.uncommitted_family
        .note_journal_lines(index, held(|w| w.uncommitted.lines().len()), stamp);
}

/// Fold the family-held rows over the journals the tree left, for the files a
/// commit holds and for the ones none does.
fn fold_the_family(tree: &TreeState, windows: &[WindowState], report: &mut Report) {
    let journals: Vec<&Journal> = windows.iter().map(|w| &w.journal).collect();
    fold_family(&tree.family, &journals, &mut report.family_held);
    let counted_today = case_keys(&tree.family, &journals);
    let held: Vec<&Journal> = windows.iter().map(|w| &w.uncommitted).collect();
    fold_uncommitted_family(
        &tree.uncommitted_family,
        &held,
        &counted_today,
        &mut report.family_held_uncommitted,
    );
}

/// Fold one event of one window into the replay.
fn replay_event(ctx: &mut Ctx<'_>, event: Event) {
    match event {
        Event::Turn => ctx.state.turn = ctx.state.turn.saturating_add(1),
        Event::Evict => {
            ctx.report.evictions = ctx.report.evictions.saturating_add(1);
            let line = Line::Evict {
                turn: ctx.state.turn,
                reason: EvictReason::Compact,
                at_ms: 0,
            };
            ctx.state.uncommitted.push(line.clone());
            ctx.state.journal.push(line);
            ctx.tree.output.evict(ctx.window);
        }
        Event::Call(call) => {
            let before = Before::of(ctx.report);
            let matched = match_the_output(ctx, &call);
            replay_call(ctx, &call);
            fold_the_output(ctx, &call, &before, matched);
        }
    }
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
        note_the_pulls(ctx, call, &text, tokens);
    }
    if SPAWNING_TOOLS.contains(&call.name.as_str()) {
        note_the_opening_section(ctx, &call.id);
        note_the_spawn(ctx, call);
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

/// Count the lookup verbs a shell call really ran, and keep the answer of one
/// run in the family for the reads that may follow it.
fn note_the_pulls(ctx: &mut Ctx<'_>, call: &Call, text: &str, tokens: u64) {
    let Some(command) = call.input.get("command").and_then(Value::as_str) else {
        return;
    };
    if !command.contains("recall") && !command.contains("memory") {
        return;
    }
    let found = pull_invocations(command, call.cwd.as_deref());
    if found.is_empty() {
        return;
    }
    if let Some(answer) = ctx.report.pulls.note_call(&found, text, tokens) {
        ctx.tree.order.note_answer(ctx.window, &call.id, answer);
    }
}

/// What the section each window this call spawned opens with would name: the
/// spawning window's journal as it stands at the call.
fn note_the_opening_section(ctx: &mut Ctx<'_>, call_id: &str) {
    let Some(children) = ctx.tree.spawned_by.get(call_id) else {
        return;
    };
    let named = opening_section(&ctx.state.journal);
    for child in children {
        ctx.tree.opening_named.insert(*child, named.clone());
    }
}

/// The windows of a tree by the id of the call that spawned each.
fn spawned_by(windows: &[Window]) -> BTreeMap<String, Vec<usize>> {
    let mut out: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, window) in windows.iter().enumerate() {
        if let Some(call_id) = &window.spawning_call {
            out.entry(call_id.clone()).or_default().push(index);
        }
    }
    out
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
    let Some(command) = command else {
        return true;
    };
    let named = names_a_work_tree_file(command, call, ctx.worktree, ctx.state.commit);
    if named {
        ctx.report.invisible_shell_named_file.add(tokens);
    }
    count_shape(
        &mut ctx.report.invisible_shell_shapes,
        command,
        named,
        tokens,
    );
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
    note_an_edit(ctx, call, &key);
    let before = ctx.tree.written_lines.get(&key).copied().or_else(|| {
        ctx.worktree
            .facts_at(ctx.state.commit, &key)
            .map(FileFacts::total_lines)
    });
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
    remember_the_write(ctx.tree, &key, written, call);
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

/// What a whole-file write establishes about the file's shape — its line and
/// byte counts — or, for any other change, the forgetting of both.
fn remember_the_write(tree: &mut TreeState, key: &str, written: Option<u32>, call: &Call) {
    tree.written_lines.remove(key);
    tree.written_bytes.remove(key);
    let Some(lines) = written else {
        return;
    };
    let bytes = call
        .input
        .get("content")
        .and_then(Value::as_str)
        .map_or(0, str::len);
    tree.written_lines.insert(key.to_owned(), lines);
    tree.written_bytes
        .insert(key.to_owned(), u64::try_from(bytes).unwrap_or(u64::MAX));
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
fn names_a_work_tree_file(
    command: &str,
    call: &Call,
    worktree: &mut Worktree,
    commit: Option<usize>,
) -> bool {
    command
        .split_whitespace()
        .map(|word| word.trim_matches(['\'', '"', '`', ',', ';']))
        .filter(|word| !word.is_empty() && !word.starts_with('-'))
        .filter_map(|word| path_key(word, call.cwd.as_deref()))
        .any(|key| worktree.facts_at(commit, &key).is_some())
}

/// What a session tree's windows read of each other, and the route each
/// later reader could have inherited the read through.
fn fold_cross_window(tree: &TreeState, limits: Limits, report: &mut Report) {
    let min_lines = usize::try_from(limits.min_slice_lines).unwrap_or(usize::MAX);
    for ((key, revision), readers) in &tree.seen {
        let distinct: BTreeSet<usize> = readers.iter().map(|(window, _)| *window).collect();
        if distinct.len() < 2 {
            continue;
        }
        report.cross_window_pairs = report.cross_window_pairs.saturating_add(1);
        let mut first = BTreeSet::new();
        for (window, tokens) in readers {
            if !(first.insert(*window) && first.len() > 1) {
                continue;
            }
            report.cross_window.add(*tokens);
            let Some(lead) = tree.leads.get(*window) else {
                continue;
            };
            let parent_text = lead.parent.and_then(|parent| {
                tree.first_texts
                    .get(&(key.clone(), *revision, parent))
                    .map(String::as_str)
            });
            let class = inheritance_of(lead, parent_text, key, min_lines);
            if class == Inheritance::ByName {
                let origin = tree
                    .by_name_origin
                    .get(&(*window, key.clone(), *revision))
                    .copied()
                    .unwrap_or_default();
                report.by_name_split.note(origin, *tokens);
            }
            if let Some(tally) = report.sibling_by_inheritance.get_mut(class.index()) {
                tally.add(*tokens);
            }
            if named_at_spawn(tree, *window, key, *revision) {
                report.sibling_named_at_spawn.add(*tokens);
            }
        }
    }
}

/// Whether the section window `window` opened with would have named this
/// revision.
fn named_at_spawn(tree: &TreeState, window: usize, key: &str, revision: u32) -> bool {
    let Some(named) = tree.opening_named.get(&window) else {
        return false;
    };
    named.contains(&(key.to_owned(), revision_hash(key, revision).to_hex()))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{looks_like_a_shell_read, replay, Report};
    use crate::replay_savings::bucket::Bucket;
    use crate::replay_savings::inheritance::Inheritance;
    use crate::replay_savings::tests::fixture;
    use crate::replay_savings::worktree::FactsSource;

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

    /// A sibling's re-read of the revision its coordinator read just before
    /// the spawn is one the opening section would have named.
    #[test]
    fn a_reread_the_opening_section_would_have_named_is_counted_under_it() {
        let tmp = tempfile::tempdir().unwrap();
        let transcripts = tmp.path().join("transcripts");
        let subagents = transcripts.join("sess-boot").join("subagents");
        std::fs::create_dir_all(&subagents).unwrap();
        let main = [
            json!({"type": "user", "message": {"role": "user", "content": "survey"}}),
            tool_use("r1", "Read", &json!({"file_path": "/repo/src/lib.rs"})),
            tool_result("r1", &numbered(60)),
            tool_use("t1", "Agent", &json!({"prompt": "look around"})),
        ];
        let child = [
            json!({"type": "user", "message": {"role": "user", "content": "look around"}}),
            tool_use("r2", "Read", &json!({"file_path": "/repo/src/lib.rs"})),
            tool_result("r2", &numbered(60)),
        ];
        write_lines(&transcripts.join("sess-boot.jsonl"), &main, "00:00");
        write_lines(&subagents.join("agent-a.jsonl"), &child, "00:01");
        std::fs::write(
            subagents.join("agent-a.meta.json"),
            json!({"toolUseId": "t1"}).to_string(),
        )
        .unwrap();
        let mut opts = fixture(tmp.path());
        opts.transcripts = transcripts;
        let report = replay(&opts).unwrap();
        assert_eq!(report.cross_window.calls, 1, "one sibling re-read");
        assert_eq!(report.sibling_named_at_spawn, report.cross_window);
    }

    fn tool_use(id: &str, name: &str, input: &serde_json::Value) -> serde_json::Value {
        json!({"type": "assistant", "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": id, "name": name, "input": input}
        ]}})
    }

    fn tool_result(id: &str, content: &str) -> serde_json::Value {
        json!({"type": "user", "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": content}
        ]}})
    }

    fn numbered(n: usize) -> String {
        (1..=n)
            .map(|i| format!("{i:>6}\u{2192}line {i} of plain prose text"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// One record per line, each stamped a second apart from `minute` on and
    /// placed in `/repo`, as the client writes them.
    fn write_lines(path: &std::path::Path, records: &[serde_json::Value], minute: &str) {
        let mut out = String::new();
        for (second, record) in records.iter().enumerate() {
            let mut record = record.clone();
            record["cwd"] = json!("/repo");
            record["timestamp"] = json!(format!("2026-03-01T00:{minute}:{second:02}.000Z"));
            out.push_str(&record.to_string());
            out.push('\n');
        }
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn the_fixture_pins_the_sibling_routes_and_where_its_facts_came_from() {
        let report = replayed();
        let class = |class: Inheritance| report.sibling_by_inheritance[class.index()].calls;
        assert_eq!(
            class(Inheritance::None),
            1,
            "a subagent whose prompt names nothing it read"
        );
        assert_eq!(
            class(Inheritance::Fork),
            0,
            "no window starts from another's"
        );
        assert_eq!(class(Inheritance::Paste), 0, "no prompt carries lines");
        assert_eq!(class(Inheritance::ByName), 0, "no prompt names the path");
        assert_eq!(
            report.sibling_named_at_spawn.calls, 0,
            "no sidecar names the call that spawned the subagent, so no section is known"
        );
        let routes = report.sibling_by_inheritance;
        assert_eq!(
            routes.iter().map(|tally| tally.calls).sum::<u64>(),
            report.cross_window.calls,
            "the routes split the later windows' first reads, call for call"
        );
        assert_eq!(
            routes.iter().map(|tally| tally.tokens).sum::<u64>(),
            report.cross_window.tokens,
            "and token for token"
        );
        assert_eq!(
            report.facts,
            FactsSource::default(),
            "the worktree, no history"
        );
        assert!(
            report.machine_formats.is_empty(),
            "no machine-written file is read"
        );
        let bands: Vec<u64> = report
            .first_sight_sizes
            .by_lines
            .iter()
            .map(|tally| tally.calls)
            .collect();
        assert_eq!(
            bands,
            [6, 3, 0, 0, 0],
            "three first sights of the hundred-line notes, none past four hundred"
        );
        assert_eq!(
            bands.iter().sum::<u64>(),
            report.bucket(Bucket::FirstSight).calls,
            "every first sight is in one band"
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
