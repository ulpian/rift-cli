//! Output-side recognition, replayed: what the shipped output match would
//! have booked from every recorded `Bash` result, by member class, and how
//! much of what it booked the window, or another window of its session tree,
//! already held.
//!
//! A module of its own because it is the one part of the replay that reads a
//! call's *output*. It runs the shipped pure match — the command cut into
//! members, the delivered lines normalised, placed and aligned — over the
//! bytes a window would have found on disk: what the transcript proves of the
//! working tree first, the window's commit after it. It books nothing into
//! any journal the other rows are built on, so every figure above it reads
//! the same with it or without it.
//!
//! Every count is at the shipped grounding rule:
//! [`GROUNDING_RUN`](rift_agent::window_journal::output_match::GROUNDING_RUN)
//! distinct lines, a unique line, or a verified printed prefix.

use std::collections::{BTreeMap, HashMap, HashSet};

use rift_agent::window_journal::output_match::{
    align, chain_members, delivered_readings, file_line_hashes, place_lines, printed_paths,
    Booking, ChainMember, Owners, Placement,
};

use super::bucket::Bucket;
use super::delivery::PERSISTED_OUTPUT;
use super::replay::Ctx;
use super::report::{Report, Tally};
use super::transcript::Call;
use super::worktree::path_key;

/// The size past which the client hands a terminal result over as a file.
const MAX_DELIVERED_CHARS: usize = 30_000;

/// The most files one delivery is matched against, as in the hook.
const MAX_CANDIDATES: usize = 16;

/// The most distinct printed prefixes resolved, as in the hook.
const MAX_PRINTED_PREFIXES: usize = 64;

/// What the output match would have booked, over the whole corpus.
#[derive(Clone, Debug, Default)]
pub struct OutputRecognition {
    /// Every `Bash` result replayed.
    pub bash: Tally,
    /// Calls with a booked line, and the tokens of the booked lines.
    pub covered: Tally,
    /// The same, by the class of the member each booked line was aligned to.
    pub by_class: BTreeMap<&'static str, Tally>,
    /// Calls with a grounded line no member could own, and those lines'
    /// tokens: output that books nothing.
    pub unaligned: Tally,
    /// The `Bash` calls of the "no name or line count" bucket, and of them
    /// the ones with a booked line.
    pub no_line_count: [Tally; 2],
    /// Shell calls the grammar refused that look like reads, and the covered.
    pub invisible: [Tally; 2],
    /// Of those, the ones naming a checkout file, and the covered.
    pub invisible_named: [Tally; 2],
    /// Shell calls the grammar recognised, and the covered.
    pub recognised: [Tally; 2],
    /// Booked lines this window had never held, by call and tokens.
    pub first_sight: Tally,
    /// Booked lines this window still held: a repeat in the same window.
    pub same_window: Tally,
    /// Booked lines another window of the session tree held and this one
    /// did not: a sibling re-read.
    pub sibling: Tally,
}

/// What one session tree's windows were shown, line by line.
#[derive(Default)]
pub(super) struct OutputHoldings {
    /// The line hashes a `(commit, key)` resolves to on disk.
    on_disk: HashMap<(Option<usize>, String), Option<FileView>>,
    /// The lines each window holds now, as `(key, revision, line)`.
    window: HashMap<usize, HashSet<(String, u64, u32)>>,
    /// The windows that were ever shown each line.
    tree: HashMap<(String, u64, u32), Vec<usize>>,
    /// The `(cwd, key)`s a change moved away from any commit without the
    /// transcript proving the new bytes: matched against nothing.
    changed: HashSet<(String, String)>,
}

/// One file as a window would have found it.
#[derive(Clone)]
struct FileView {
    key: String,
    hashes: Vec<u64>,
    revision: u64,
}

/// The report's counters before a call, so its classes can be read off what
/// the call moved.
pub(super) struct Before {
    counts: [u64; 4],
}

/// What a call's output was matched to, worked out before the call folds its
/// own evidence into the working tree.
pub(super) struct Pending {
    files: Vec<FileView>,
    members: Vec<ChainMember>,
    placed: Vec<Option<Placement>>,
    bookings: Vec<Booking>,
}

impl Before {
    /// The counters the classes are read from.
    pub(super) fn of(report: &Report) -> Self {
        Self {
            counts: [
                report.bucket(Bucket::NoLineCount).calls,
                report.invisible_shell.calls,
                report.invisible_shell_named_file.calls,
                report.shell_recognised,
            ],
        }
    }

    /// Which of the four the call landed in.
    fn moved(&self, report: &Report) -> [bool; 4] {
        let now = Self::of(report).counts;
        let mut out = [false; 4];
        for ((slot, after), before) in out.iter_mut().zip(now).zip(self.counts) {
            *slot = after > before;
        }
        out
    }
}

impl OutputHoldings {
    /// The window lost what it held.
    pub(super) fn evict(&mut self, window: usize) {
        self.window.remove(&window);
    }
}

/// Match a call's output before the call is replayed, or `None` for a call
/// with nothing to match.
pub(super) fn match_the_output(ctx: &mut Ctx<'_>, call: &Call) -> Option<Pending> {
    let text = call.result.as_deref()?;
    if text.contains(PERSISTED_OUTPUT) || text.chars().count() >= MAX_DELIVERED_CHARS {
        return None;
    }
    let members = match call.name.as_str() {
        "Bash" => chain_members(call.input.get("command")?.as_str()?)?,
        "Read" => vec![read_member(call)?],
        _ => return None,
    };
    let names = member_names(ctx, call, &members);
    let mut files: Vec<FileView> = Vec::new();
    for key in names.iter().flat_map(|(files, _)| files) {
        add_candidate(ctx, call, &mut files, key);
    }
    let printed = printed_candidates(ctx, call, &members, &names, &mut files, text);
    let named_by: Vec<Vec<usize>> = files
        .iter()
        .map(|file| owners_of(&names, &file.key))
        .collect();
    let named_files: HashSet<&String> = names.iter().flat_map(|(f, _)| f).collect();
    let sole_file = match named_files.into_iter().collect::<Vec<_>>().as_slice() {
        [only] => files.iter().position(|f| &f.key == *only),
        _ => None,
    };
    let lines = delivered_readings(text, sole_file, &|p| printed.get(p).copied());
    let slices: Vec<&[u64]> = files.iter().map(|f| f.hashes.as_slice()).collect();
    let placed = place_lines(&slices, &lines);
    let cd: Vec<bool> = members.iter().map(|m| m.cd).collect();
    let bookings = align(
        &Owners {
            named_by: &named_by,
            cd: &cd,
        },
        &placed,
    );
    Some(Pending {
        files,
        members,
        placed,
        bookings,
    })
}

/// Fold what a call's output was matched to into the report and the tree's
/// holdings, once the call has been replayed.
pub(super) fn fold_the_output(
    ctx: &mut Ctx<'_>,
    call: &Call,
    before: &Before,
    pending: Option<Pending>,
) {
    note_a_change(ctx, call);
    let classes = before.moved(ctx.report);
    let is_bash = call.name == "Bash";
    let text = call.result.as_deref().unwrap_or_default();
    let tokens = if is_bash { count(text) } else { 0 };
    if is_bash {
        let rows = &mut ctx.report.output_recognition;
        rows.bash.add(tokens);
        for (row, landed) in class_rows(rows).into_iter().zip(classes) {
            if landed {
                row.add(tokens);
            }
        }
    }
    let Some(pending) = pending else {
        return;
    };
    let raw: Vec<&str> = text.lines().collect();
    let booked = booked_lines(&pending);
    if is_bash {
        tally_the_call(ctx, &pending, &raw, &booked, classes);
    }
    for (_, file, line) in booked.values() {
        let Some(view) = pending.files.get(*file) else {
            continue;
        };
        hold(
            &mut ctx.tree.output,
            ctx.window,
            (view.key.clone(), view.revision, *line),
        );
    }
}

/// After a change, whether the transcript proves the file's new bytes: where
/// it does they are matched against, and where it does not the file is
/// matched against nothing until it does.
fn note_a_change(ctx: &mut Ctx<'_>, call: &Call) {
    if !matches!(call.name.as_str(), "Write" | "Edit" | "MultiEdit") {
        return;
    }
    let Some(key) = call
        .input
        .get("file_path")
        .and_then(|p| p.as_str())
        .and_then(|p| path_key(p, call.cwd.as_deref()))
    else {
        return;
    };
    let cwd = call.cwd.clone().unwrap_or_default();
    let proven = ctx.tree.working.proven_text(&cwd, &key).is_some();
    let slot = (cwd, key);
    if proven {
        ctx.tree.output.changed.remove(&slot);
    } else {
        ctx.tree.output.changed.insert(slot);
    }
}

/// The report rows a call's four classes are counted under, in the order
/// [`Before`] reads them.
fn class_rows(rows: &mut OutputRecognition) -> [&mut Tally; 4] {
    let OutputRecognition {
        no_line_count,
        invisible,
        invisible_named,
        recognised,
        ..
    } = rows;
    let [nlc, _] = no_line_count;
    let [inv, _] = invisible;
    let [named, _] = invisible_named;
    let [rec, _] = recognised;
    [nlc, inv, named, rec]
}

/// The covered rows, in the same order.
fn covered_rows(rows: &mut OutputRecognition) -> [&mut Tally; 4] {
    let OutputRecognition {
        no_line_count,
        invisible,
        invisible_named,
        recognised,
        ..
    } = rows;
    let [_, nlc] = no_line_count;
    let [_, inv] = invisible;
    let [_, named] = invisible_named;
    let [_, rec] = recognised;
    [nlc, inv, named, rec]
}

/// Count one `Bash` call's booked and unaligned lines, by member class and
/// by what the window and its tree already held.
fn tally_the_call(
    ctx: &mut Ctx<'_>,
    pending: &Pending,
    raw: &[&str],
    booked: &BTreeMap<usize, (usize, usize, u32)>,
    classes: [bool; 4],
) {
    let mut pools: [Vec<&str>; 3] = [Vec::new(), Vec::new(), Vec::new()];
    let mut by_class: BTreeMap<&'static str, Vec<&str>> = BTreeMap::new();
    for (at, (member, file, line)) in booked {
        let (Some(text), Some(view)) = (raw.get(*at), pending.files.get(*file)) else {
            continue;
        };
        let slot = (view.key.clone(), view.revision, *line);
        let pool = held_by(&ctx.tree.output, ctx.window, &slot);
        if let Some(lines) = pools.get_mut(pool) {
            lines.push(text);
        }
        let class = member_class(pending.members.get(*member));
        by_class.entry(class).or_default().push(text);
    }
    let unaligned: Vec<&str> = pending
        .placed
        .iter()
        .enumerate()
        .filter(|(at, p)| p.is_some_and(|p| p.grounded) && !booked.contains_key(at))
        .filter_map(|(at, _)| raw.get(at).copied())
        .collect();
    let rows = &mut ctx.report.output_recognition;
    add_lines(&mut rows.unaligned, &unaligned);
    let all: Vec<&str> = pools.iter().flatten().copied().collect();
    if all.is_empty() {
        return;
    }
    let covered = count(&all.join("\n"));
    rows.covered.add(covered);
    for (row, landed) in covered_rows(rows).into_iter().zip(classes) {
        if landed {
            row.add(covered);
        }
    }
    let [first, same, sibling] = &pools;
    add_lines(&mut rows.first_sight, first);
    add_lines(&mut rows.same_window, same);
    add_lines(&mut rows.sibling, sibling);
    for (class, lines) in by_class {
        add_lines(rows.by_class.entry(class).or_default(), &lines);
    }
}

/// Count a call under `row` with these lines' tokens, when there are any.
fn add_lines(row: &mut Tally, lines: &[&str]) {
    if !lines.is_empty() {
        row.add(count(&lines.join("\n")));
    }
}

/// The tokens of `text`, with the replay's tokenizer.
fn count(text: &str) -> u64 {
    u64::from(rift_tokens::count(text))
}

/// Each booked output line, by its index: the member it was booked to, the
/// file and the line.
fn booked_lines(pending: &Pending) -> BTreeMap<usize, (usize, usize, u32)> {
    let mut out = BTreeMap::new();
    for (at, place) in pending.placed.iter().enumerate() {
        let Some(place) = place.filter(|p| p.grounded) else {
            continue;
        };
        let owner = pending.bookings.iter().find(|b| {
            b.file == place.file
                && b.runs
                    .iter()
                    .any(|r| r.lo <= place.line && place.line <= r.hi)
        });
        if let Some(booking) = owner {
            out.insert(at, (booking.member, place.file, place.line));
        }
    }
    out
}

/// Which pool a booked line falls in: 0 first sight, 1 this window holds
/// it, 2 another window of the tree was shown it.
fn held_by(holdings: &OutputHoldings, window: usize, slot: &(String, u64, u32)) -> usize {
    if holdings
        .window
        .get(&window)
        .is_some_and(|held| held.contains(slot))
    {
        return 1;
    }
    let sibling = holdings
        .tree
        .get(slot)
        .is_some_and(|windows| windows.iter().any(|w| *w != window));
    if sibling {
        2
    } else {
        0
    }
}

/// Record that `window` was shown one line.
fn hold(holdings: &mut OutputHoldings, window: usize, slot: (String, u64, u32)) {
    let windows = holdings.tree.entry(slot.clone()).or_default();
    if !windows.contains(&window) {
        windows.push(window);
    }
    holdings.window.entry(window).or_default().insert(slot);
}

/// The class of a member, by its command word.
fn member_class(member: Option<&ChainMember>) -> &'static str {
    let head = member
        .and_then(|m| m.words.first())
        .map_or("", |w| w.rsplit('/').next().unwrap_or(w));
    match head {
        "cat" | "sed" | "head" | "tail" | "nl" | "awk" | "bat" | "batcat" | "less" | "more" => {
            "read"
        }
        "grep" | "egrep" | "fgrep" | "rg" => "search",
        "git" => "git",
        "ls" | "find" | "tree" | "wc" | "stat" | "du" => "listing",
        "cargo" | "rustc" | "make" => "build",
        "python" | "python3" | "node" | "jq" | "perl" | "ruby" => "script",
        "for" | "while" | "if" | "do" | "done" | "then" | "fi" => "control",
        "Read" => "read tool",
        _ => "other",
    }
}

/// A Read call as one member naming its file.
fn read_member(call: &Call) -> Option<ChainMember> {
    let path = call.input.get("file_path")?.as_str()?;
    if call.result.as_deref()?.contains("<tool_use_error>") {
        return None;
    }
    Some(ChainMember {
        words: vec!["Read".to_owned(), path.to_owned()],
        ..ChainMember::default()
    })
}

/// What each member names: the keys of files the window would find, and
/// directory prefixes.
fn member_names(
    ctx: &mut Ctx<'_>,
    call: &Call,
    members: &[ChainMember],
) -> Vec<(Vec<String>, Vec<String>)> {
    members
        .iter()
        .map(|member| {
            let mut files = Vec::new();
            let mut dirs = Vec::new();
            if member.cd || member.dir_unknown {
                return (files, dirs);
            }
            for word in member.words.iter().skip(1).filter(|w| !w.starts_with('-')) {
                let Some(key) = member_key(call, member, word) else {
                    continue;
                };
                if view_of(ctx, call, &key).is_some() {
                    files.push(key);
                } else {
                    let key = key.trim_end_matches('/');
                    dirs.push(if key.is_empty() || key == "." {
                        String::new()
                    } else {
                        format!("{key}/")
                    });
                }
            }
            (files, dirs)
        })
        .collect()
}

/// The key a word of a member names, read against the member's directory.
fn member_key(call: &Call, member: &ChainMember, word: &str) -> Option<String> {
    let joined = match &member.dir {
        Some(dir) if !word.starts_with('/') => format!("{}/{word}", dir.trim_end_matches('/')),
        _ => word.to_owned(),
    };
    let key = path_key(&joined, call.cwd.as_deref())?;
    normalised(&key)
}

/// A key with `.` and `..` segments folded, or `None` when it climbs out.
fn normalised(key: &str) -> Option<String> {
    let rooted = key.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for part in key.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    let joined = parts.join("/");
    Some(if rooted { format!("/{joined}") } else { joined })
}

/// The members that name `key`, as a file or under a directory.
fn owners_of(names: &[(Vec<String>, Vec<String>)], key: &str) -> Vec<usize> {
    names
        .iter()
        .enumerate()
        .filter(|(_, (files, dirs))| {
            files.iter().any(|f| f == key) || dirs.iter().any(|d| key.starts_with(d.as_str()))
        })
        .map(|(at, _)| at)
        .collect()
}

/// The files the output's printed paths name under a member's name, added
/// to the candidates; the map from a printed prefix to its file.
fn printed_candidates(
    ctx: &mut Ctx<'_>,
    call: &Call,
    members: &[ChainMember],
    names: &[(Vec<String>, Vec<String>)],
    files: &mut Vec<FileView>,
    text: &str,
) -> HashMap<String, usize> {
    let mut printed = HashMap::new();
    let mut tried = HashSet::new();
    for prefix in text.lines().flat_map(printed_paths) {
        if files.len() >= MAX_CANDIDATES || tried.len() >= MAX_PRINTED_PREFIXES {
            break;
        }
        if !tried.insert(prefix) {
            continue;
        }
        let found = members
            .iter()
            .filter(|m| !m.cd && !m.dir_unknown)
            .find_map(|member| {
                let key = member_key(call, member, prefix)?;
                (!owners_of(names, &key).is_empty()).then_some(key)
            });
        if let Some(index) = found.and_then(|key| add_candidate(ctx, call, files, &key)) {
            printed.insert(prefix.to_owned(), index);
        }
    }
    printed
}

/// The index of `key` among the candidates, adding it where the window
/// would find it and the cap allows.
fn add_candidate(
    ctx: &mut Ctx<'_>,
    call: &Call,
    files: &mut Vec<FileView>,
    key: &str,
) -> Option<usize> {
    if let Some(at) = files.iter().position(|f| f.key == key) {
        return Some(at);
    }
    if files.len() >= MAX_CANDIDATES {
        return None;
    }
    files.push(view_of(ctx, call, key)?);
    Some(files.len() - 1)
}

/// The file at `key` as the window would find it: the bytes the transcript
/// proves first, then the window's commit (or the worktree).
fn view_of(ctx: &mut Ctx<'_>, call: &Call, key: &str) -> Option<FileView> {
    let cwd = call.cwd.as_deref().unwrap_or_default();
    if let Some(text) = ctx.tree.working.proven_text(cwd, key) {
        return Some(view(key, text.as_bytes()));
    }
    if ctx
        .tree
        .output
        .changed
        .contains(&(cwd.to_owned(), key.to_owned()))
    {
        return None;
    }
    let slot = (ctx.state.commit, key.to_owned());
    if let Some(found) = ctx.tree.output.on_disk.get(&slot) {
        return found.clone();
    }
    let found = ctx
        .worktree
        .bytes_at(ctx.state.commit, key)
        .map(|bytes| view(key, &bytes));
    ctx.tree.output.on_disk.insert(slot, found.clone());
    found
}

/// A file's line hashes and a revision folded from them.
fn view(key: &str, bytes: &[u8]) -> FileView {
    let hashes = file_line_hashes(bytes);
    let revision = hashes
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64 ^ hashes.len() as u64, |acc, h| {
            let acc = (acc ^ h).wrapping_mul(0x0000_0100_0000_01b3);
            acc ^ (acc >> 29)
        });
    FileView {
        key: key.to_owned(),
        hashes,
        revision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A key's `.` and `..` segments fold, and one that climbs out has none.
    #[test]
    fn a_key_folds_its_dot_segments() {
        assert_eq!(normalised("sub/../a.rs").as_deref(), Some("a.rs"));
        assert_eq!(normalised("./sub/b.rs").as_deref(), Some("sub/b.rs"));
        assert_eq!(normalised("../a.rs"), None);
    }

    /// The member classes the rows are published under.
    #[test]
    fn a_member_is_classed_by_its_command_word() {
        let member = |word: &str| ChainMember {
            words: vec![word.to_owned()],
            ..ChainMember::default()
        };
        assert_eq!(member_class(Some(&member("/usr/bin/grep"))), "search");
        assert_eq!(member_class(Some(&member("sed"))), "read");
        assert_eq!(member_class(Some(&member("git"))), "git");
        assert_eq!(member_class(None), "other");
    }

    /// A line is first sight, a same-window repeat, or a sibling re-read.
    #[test]
    fn a_booked_line_falls_in_one_pool() {
        let mut holdings = OutputHoldings::default();
        let slot = ("a.rs".to_owned(), 7, 3);
        assert_eq!(held_by(&holdings, 0, &slot), 0);
        hold(&mut holdings, 0, slot.clone());
        assert_eq!(held_by(&holdings, 0, &slot), 1);
        assert_eq!(held_by(&holdings, 1, &slot), 2);
        holdings.evict(0);
        assert_eq!(held_by(&holdings, 0, &slot), 0);
    }
}
