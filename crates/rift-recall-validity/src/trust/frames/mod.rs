//! Every sentence a model reads from Rift: the frames' fixed wording, and the
//! renderers that compose it around spans the chokepoint has judged.
//!
//! One file on purpose. A frame is refused if it can be forged, counted honestly,
//! and held to its own bytes by goldens, and the intercept gate reads *this* file
//! for the phrases no other file may open a line with — so the openers, the
//! clauses and every `render_*` live here and nowhere else.
//!
//! A manifest is pushed at a turn boundary. The frames below are pushed in the
//! position of a call the agent made and Rift declined — the tool result itself
//! on the one family whose channel was measured returning it byte-identical. They
//! obey the manifest's discipline exactly: fixed wording this module authors,
//! every variable span through the chokepoint, counts instead of quotations, no
//! content, and no `<placeholder>` (the alphabet has no `<`, which is why every
//! hint below is built from values the caller actually holds).
//!
//! One rule is stricter here, and the reason is position rather than taste. A
//! manifest entry is a bullet on a line of its own, so a span that *starts* like
//! the frame is the only forgery worth refusing and prose about the frame renders
//! (the 2026-08-06 decision, at `opens_like_the_frame`). A reason frame is one
//! line with the span inside it, so a span reproducing the frame's wording
//! anywhere is not prose about the frame — it is a second clause of the frame, in
//! the position where the agent reads Rift's own explanation of why its call was
//! declined. `neutral_inline_span` is that rule.

use core::fmt::Write as _;

use super::manifest::{EntryDetail, ManifestEntry};
use super::pushed::{neutral_pushed_span, normalize_pushed};
use crate::outline::{DefinitionKind, OutlineShape};

/// What Rift can say about a held file at the moment the context lost it.
///
/// Three states and no fourth, because the map's value is that a model can act
/// on each one differently: skip the file, pull only the named lines, or ask.
/// A verdict is never inferred from the absence of another — an unreachable
/// basis is [`HeldVerdict::Unknown`], not "unchanged".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeldVerdict<'a> {
    /// The worktree bytes still hash to what the session read.
    Unchanged,
    /// The file changed, at these ranges in current numbering.
    ChangedAt(&'a [(u32, u32)]),
    /// No basis was reachable within the caller's caps, so nothing is claimed.
    Unknown,
}

/// One file the session held when its context was compacted.
///
/// Plain borrowed data on purpose: this renderer sits below the ledger and the
/// diff, and taking their types here would put an edge into the crate graph for
/// the sake of five fields.
#[derive(Clone, Copy, Debug)]
pub struct HeldEntry<'a> {
    /// The path as the session read it.
    pub path: &'a str,
    /// The content hash the session holds — 12 hex renders, 16 is the argument
    /// form of the pull verb.
    pub hash: &'a str,
    /// The line ranges the session held, in the numbering it read.
    pub held: &'a [(u32, u32)],
    /// What is true of the file now.
    pub verdict: HeldVerdict<'a>,
    /// The shape of the revision the checkout holds now, from a valid summary
    /// of it; `None` when there is none to state.
    pub shape: Option<OutlineShape>,
}

/// One revision a subagent's coordinator had read when the subagent's window
/// opened: borrowed, like [`HeldEntry`], so this renderer takes no edge to the
/// journal that produced it.
#[derive(Clone, Copy, Debug)]
pub struct CoordinatorRead<'a> {
    /// The path as the coordinator's journal spells it.
    pub path: &'a str,
    /// The content hash the coordinator read, in hex.
    pub hash: &'a str,
}

/// What git says about a path the answer ranks, as a fixed word.
///
/// An enum rather than a caller's string, because this is the one place a
/// frame would otherwise render a label somebody else composed. The ranking
/// puts the warm end first, and the tag is what tells the model
/// why — so it has to be Rift's own word or it is content in a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathStatus {
    /// Nothing to say; the path renders bare.
    Tracked,
    /// Modified in the working tree.
    Modified,
    /// Staged for the next commit.
    Staged,
    /// Not tracked by git.
    Untracked,
}

/// One ranked path in a search answer.
#[derive(Clone, Copy, Debug)]
pub struct SearchHit<'a> {
    /// The path, relative to the work tree.
    pub path: &'a str,
    /// What git says about it.
    pub status: PathStatus,
}

/// One definition of an outline a first-contact frame names: plain borrowed
/// data, for [`HeldEntry`]'s reason.
#[derive(Clone, Copy, Debug)]
pub struct OutlineEntry<'a> {
    /// Its kind, from the closed set: Rift's own word, never the source's.
    pub kind: DefinitionKind,
    /// Its name, a span the guard judges before the frame quotes it.
    pub name: &'a str,
    /// The first line of its span.
    pub first_line: u32,
    /// The last line of its span, inclusive.
    pub last_line: u32,
    /// The stored token count of its span, where one was stored.
    pub tokens: Option<u32>,
}

/// A first read of a large file, and the bound outline that may answer it.
#[derive(Clone, Copy, Debug)]
pub struct OutlinedRead<'a> {
    /// The path as the call named it.
    pub path: &'a str,
    /// The content hash of the revision outlined.
    pub hash: &'a str,
    /// The file's line count.
    pub lines: u32,
    /// The file's byte count.
    pub bytes: u64,
    /// The whole file's stored token count, where one was stored.
    pub whole_tokens: Option<u32>,
    /// How many definitions the outline lists in all.
    pub definitions: u32,
    /// Whether the outline is by heading.
    pub markdown: bool,
    /// The definitions the caller holds, in file order.
    pub entries: &'a [OutlineEntry<'a>],
    /// The last line a first-page read asked for; `None` for the whole file.
    pub page_end: Option<u32>,
}

/// One definition line a frame names, and the span it names.
type OutlineLine = (String, (u32, u32));

/// The frame's own wording, one const per line it authors, so the renderer and
/// the mimicry guard read from the same source and cannot drift apart.
///
/// Why openers rather than whole lines: an attacker does not need to reproduce a
/// frame line to be read as one, only to start like it.
const FRAME_HEADER_OPENER: &str = "[rift] Recorded delta";
/// The list header's fixed part; the line is `{count} {this}`.
const FRAME_LIST_OPENER: &str = "file(s) you have read changed since then:";
/// The omitted clause's fixed part; the clause is `{count} {this}`.
const FRAME_OMITTED_OPENER: &str = "more not shown";
/// The withheld clause's fixed part; the clause is `{count} {this}`.
const FRAME_WITHHELD_OPENER: &str = "withheld as unsafe to inject";
/// The hint line's fixed part; the line is `{this} {hint}`.
const FRAME_HINT_OPENER: &str = "Detail on demand:";

/// The head of both reason frames: an unchanged re-read, and a re-read of a
/// file whose changes Rift can name exactly.
///
/// One opener for both, because they are the same act — a read Rift declined
/// once — and a model that has learned to read one has learned to read the
/// other.
const FRAME_DENY_OPENER: &str = "[rift] Read skipped:";

/// The head of the frame for a read that is partly held: the held run is
/// named, and so is the one run of lines the window still lacks.
///
/// An opener of its own rather than [`FRAME_DENY_OPENER`], because it is a
/// different act. A skipped read hands the model nothing; a narrowed one
/// tells it which smaller read to make instead, and a model has to be able to
/// tell the two apart from the first words.
const FRAME_NARROW_OPENER: &str = "[rift] Read narrowed:";

/// The head of the frame that answers a first read of a large file with the
/// file's outline: its shape, its definitions and the cheaper read to make.
///
/// An opener of its own, because it is a different act again: the window
/// does not hold the file, and the frame hands it a map of it rather than
/// telling it what it already has.
const FRAME_OUTLINE_OPENER: &str = "[rift] Read outlined:";

/// The head of the frame that answers a read of bytes another window of the
/// repository holds, while no outline of them is bound: the runs that window
/// holds, as the reads to make instead.
///
/// An opener of its own, because the act is different from every other: the
/// window asking holds nothing of the file, and what it is handed is where
/// another window found the bytes worth reading.
const FRAME_HELD_OPENER: &str = "[rift] Read held by another window:";

/// The head of the line that stands in a terminal result for file lines the
/// window already holds at the same revision.
const FRAME_HELD_OUTPUT_OPENER: &str = "[rift] Output held:";

/// The head of the recovery map, injected at the boundary where the context
/// lost the bytes it held.
const FRAME_RECOVERY_OPENER: &str = "[rift] Context was compacted.";

/// The head of the answer that stands in for a listing the agent asked for.
const FRAME_SEARCH_OPENER: &str = "[rift] Search answered:";
/// The head of the redirect: a document Rift already holds as Markdown.
const FRAME_REDIRECT_OPENER: &str = "[rift] Read redirected:";
/// The head of the section a subagent's window opens with: revisions its
/// coordinator held at that instant, which the subagent does not.
const FRAME_COORDINATOR_OPENER: &str = "[rift] Held by your coordinator at your start";
/// The head of the prepared result Antigravity's persistent-message channel
/// hands back for a read Rift declined: the reason frame, re-announced.
///
/// Its own opener and not [`FRAME_DENY_OPENER`], because the model meets it in
/// a different position: not as the tool result of the declined call but as a
/// message of its own, so the line has to say what it is a message about.
const FRAME_PREPARED_OPENER: &str = "[rift] Prepared result for the declined unchanged read:";
/// The head of the bounded read plan that answers an Antigravity
/// `ContentOffset` probe, whose coverage semantics Rift cannot verify: the
/// landmarks to read by line span instead.
const FRAME_PLAN_OPENER: &str = "[rift] Bounded structural read plan for";

/// What a frame renders where it may not render a path.
///
/// A withheld path still leaves a true sentence: the agent made the call, so it
/// knows which file it asked for. The manifest's count-not-quotation rule
/// becomes a stand-in-not-quotation rule here, for the same reason — the frame
/// is honest that a file is involved and never repeats its name.
const WITHHELD_PATH: &str = "a file you read";

/// The fixed clause naming what the ledger proved: the requested lines are in
/// the context already.
const CLAUSE_UNCHANGED: &str = "unchanged since your read";
/// The fixed clause that says where those bytes are.
const CLAUSE_IN_CONTEXT: &str = "are already in your context";
/// The same fact as the narrowing frame says it, where it shares its verb
/// with [`CLAUSE_UNCHANGED`]. A clause of its own so the guard refuses a span
/// that spells it, which the longer clause would not catch.
const CLAUSE_ALREADY_IN_CONTEXT: &str = "already in your context";
/// The escape hatch, which is what makes a decline something other than an
/// unexplained failure: one deny per key, so the identical call succeeds (A3).
const CLAUSE_REPEAT: &str = "Repeat this exact call";
/// The outline frame's cheaper route: the lines of one definition.
const CLAUSE_READ_ONLY: &str = "Read only the lines you need";
/// The cheaper route, named in the plain frame.
const CLAUSE_PULL: &str = "To see only what changed since then:";
/// The delta frame's fact: the file changed, and Rift knows exactly where.
const CLAUSE_CHANGED_ONLY: &str = "changed only at lines";
/// The other half of that fact, which is what makes the read skippable.
const CLAUSE_OTHERS_UNCHANGED: &str = "every other line is unchanged";
/// The delta frame's suggested window.
const CLAUSE_IN_CONTEXT_WINDOW: &str = "to see the change in context";
/// The cheaper route, named in the delta frame.
const CLAUSE_FULL_DIFF: &str = "Full diff on demand:";
/// What the recovery map is a list of.
const CLAUSE_HELD_LIST: &str = "Files this session had read";
/// The recovery map's other verdict, beside [`CLAUSE_UNCHANGED`].
const CLAUSE_CHANGED_AT: &str = "changed at lines";
/// What the recovery map's line under a changed file states.
const CLAUSE_SHAPE_NOW: &str = "shape of the current revision:";
/// What the search answer's order means, said rather than implied.
const CLAUSE_RANKED: &str = "ranked by relevance to this session";
/// The search answer's escape hatch: the hidden tail is one call away.
const CLAUSE_FULL_LIST: &str = "for the full list";
/// What the redirect claims, and the whole of what it claims.
const CLAUSE_CONVERTED: &str = "is a document Rift has already converted to Markdown";
/// The handle to the story behind the change, named by both reason frames.
///
/// The frames say *what* moved; this says where *when, who and what it cost*
/// are kept, as a command rather than a promise. A **phrase** and not the bare
/// word, for the reason the clause list states: the list is matched with
/// `contains`, and `provenance` alone would withhold every path in
/// `bins/rift/src/provenance.rs`'s neighbourhood.
const CLAUSE_PROVENANCE: &str = "Provenance: rift why";
/// Why an outline was sent in a read's stead when the cause is not a first
/// contact: another window holds these bytes. The holder is named as nothing
/// more than this — never a session, a key, a checkout or a turn.
const CLAUSE_HELD_ELSEWHERE: &str = "is held at this revision by another window of this repository";
/// The held-lines frame's fact, in the same words.
const CLAUSE_HELD_BY: &str = "another window of this repository holds";

/// The same phrases in the form a pushed span is judged in: projected onto the
/// pushed alphabet (a span can contain nothing else, so nothing else can be
/// compared) and then normalized.
///
/// Written out rather than computed, because a guard on the hot path should not
/// allocate five strings to answer a question whose answer is fixed at compile
/// time. Drift is prevented by `the_frame_openers_are_the_frame`, which checks
/// the correspondence in **both** directions and the count, so the two lists are
/// a bijection: every [`FRAME_OPENERS`] phrase normalizes onto an entry here
/// (else the guard defends less than the frame writes), and every entry here has
/// a phrase it came from (else the guard refuses spans on behalf of wording the
/// renderer no longer has, which is over-refusal that the first direction cannot
/// see). Each opener must additionally open a line of a real rendered manifest.
pub(super) const FRAME_OPENERS_NORMALIZED: &[&str] = &[
    "riftrecordeddelta",
    "riftreadskipped:",
    "riftreadnarrowed:",
    "riftreadoutlined:",
    "riftreadheldbyanotherwindow:",
    "riftcontextwascompacted",
    "riftsearchanswered:",
    "riftreadredirected:",
    "riftheldbyyourcoordinatoratyourstart",
    "riftpreparedresultforthedeclinedunchangedread:",
    "riftboundedstructuralreadplanfor",
    "filesyouhavereadchangedsincethen:",
    "morenotshown",
    "withheldasunsafetoinject",
    "detailondemand:",
];

/// Every fixed phrase a frame writes **inside** a line, in the form a span is
/// judged in: projected onto the pushed alphabet and normalized.
///
/// Written out for the same reason as [`FRAME_OPENERS_NORMALIZED`] — a guard on
/// the hook path should not build strings to answer a question fixed at compile
/// time — and held to the consts above by `the_frame_clauses_are_the_frame`,
/// which checks both directions and that each clause really appears in a
/// rendered frame.
///
/// Every entry is a phrase rather than a word, deliberately: this list is
/// checked with `contains` rather than `starts_with`, so a short entry would
/// eat ordinary paths. `readlines` would have cost `src/read_lines.rs`;
/// `unchangedsinceyourread` costs nothing anyone will name a file.
const FRAME_CLAUSES_NORMALIZED: &[&str] = &[
    "unchangedsinceyourread",
    "arealreadyinyourcontext",
    "alreadyinyourcontext",
    "repeatthisexactcall",
    "readonlythelinesyouneed",
    "toseeonlywhatchangedsincethen:",
    "changedonlyatlines",
    "everyotherlineisunchanged",
    "toseethechangeincontext",
    "fulldiffondemand:",
    "filesthissessionhadread",
    "changedatlines",
    "shapeofthecurrentrevision:",
    "rankedbyrelevancetothissession",
    "forthefulllist",
    "isadocumentrifthasalreadyconvertedtomarkdown",
    "provenance:riftwhy",
    "isheldatthisrevisionbyanotherwindowofthisrepository",
    "anotherwindowofthisrepositoryholds",
];

/// The most changed sites a frame names before it starts counting.
///
/// Eight, the same cap a manifest entry puts on its ranges: past that a list
/// of line numbers stops being a place to look and becomes the diff, which is
/// what the pull verb is for.
const MAX_FRAME_SITES: usize = 8;

/// The most definitions a first-contact frame names before it counts the
/// rest: the costliest, so the lines worth reading on their own are the ones
/// named, listed in file order.
///
/// Twelve keeps the frame under four hundred tokens on every first read the
/// replay corpus holds; the floor test in the evaluation crate measures it.
pub const MAX_OUTLINE_FRAME_DEFINITIONS: usize = 12;

/// The same cap for the outline sent because another window holds the
/// bytes: one fewer, because the clause that says so costs about what a
/// definition line does, and the frame keeps the same four-hundred-token
/// ceiling on the replay corpus's largest outlines.
const MAX_HELD_OUTLINE_FRAME_DEFINITIONS: usize = MAX_OUTLINE_FRAME_DEFINITIONS - 1;

/// The fewest lines a first page must ask for before its outline is worth
/// sending in its stead: the outline frame's break-even at four tokens a line,
/// the rule every read floor is derived by.
///
/// Measured rather than quoted: the evaluation crate, which has this renderer
/// and the real tokenizer, counts the frame at the definition cap on its
/// fixture, and a reword that grows the frame reds that test.
pub const OUTLINE_FRAME_MIN_PAGE_LINES: u32 = 95;

/// The most revisions the coordinator section names before it counts the
/// rest.
///
/// The section is pushed unasked into a window that has its own task: a
/// handful of names is a hint it can act on, and past that the list is the
/// coordinator's whole reading, which `rift recall known-state` answers
/// better on request.
const MAX_COORDINATOR_READS: usize = 8;

impl PathStatus {
    /// The word this status renders as, or nothing.
    fn tag(self) -> &'static str {
        match self {
            Self::Tracked => "",
            Self::Modified => " (modified)",
            Self::Staged => " (staged)",
            Self::Untracked => " (untracked)",
        }
    }
}

/// The delta manifest Rift injects at the start of a turn,
/// rendered from `entries` (changed files, in the ledger's rank order) plus the
/// `omitted` count its caller's budget already dropped.
///
/// `None` is the **silence property**, and it is narrower than it first looks.
/// Silence is authorized for one state only: *nothing changed*. So
/// `None` means exactly that — no entry to consider and nothing the caller's
/// budget dropped. Every other state renders.
///
/// In particular a manifest whose entries were **all** withheld still renders:
/// the frame plus the honest counter, naming no path. The earlier reading —
/// that saying "N withheld" announces something hostile in the ledger — traded a
/// true statement for a false one, because on a repository whose paths are
/// simply not ASCII (a CJK tree, an accented filename) nothing hostile is
/// involved at all, and staying silent would tell the agent its state is current
/// when it is not. A count is not a quotation: what was withheld is never named,
/// so the counter carries no attacker-controlled bytes either way.
///
/// The frame is authored here and never interpolated from data. Every entry's
/// path is [`neutral_pushed_span`]'d — by the private entry renderer this
/// function delegates each line to — and withheld entries are counted into the
/// closing line, never quoted. The result is a
/// fact sheet: the caller supplies the command an agent runs to get detail,
/// because only the caller knows which spelling of the verb surface this build
/// ships — and, since an entry now carries the revision the session holds, the
/// caller can spell that command with a hash the agent can actually pass.
///
/// **What an entry carries, and why it is worth its tokens.** A path alone
/// tells a model that a file moved and leaves it to re-read the file to find
/// out where. The revision it holds plus the changed ranges cost about eight
/// tokens more and turn the same entry into an argument for the pull verb: it
/// can skip the re-read, or fetch only the lines that moved. Nothing else
/// changes — the entries are the same entries, the frame is the same frame,
/// and a turn with nothing to announce is still answered with silence.
#[must_use]
pub fn render_delta_manifest(
    entries: &[ManifestEntry<'_>],
    omitted: usize,
    pull_hint: &str,
) -> Option<String> {
    render_delta_manifest_naming(entries, omitted, pull_hint).map(|(text, _)| text)
}

/// [`render_delta_manifest`], and **which entries it named**, by index.
///
/// The same pairing [`render_recovery_map_naming`] exists for, and for the
/// same reason: an entry the guard withheld is a count, not a fact the model
/// was given, and a caller that records having told a window something has to
/// record what it actually told it.
#[must_use]
pub fn render_delta_manifest_naming(
    entries: &[ManifestEntry<'_>],
    omitted: usize,
    pull_hint: &str,
) -> Option<(String, Vec<usize>)> {
    let mut shown: Vec<String> = Vec::with_capacity(entries.len());
    let mut named: Vec<usize> = Vec::with_capacity(entries.len());
    let mut withheld = 0usize;
    for (at, e) in entries.iter().enumerate() {
        match render_manifest_entry(e) {
            Some(span) => {
                shown.push(span);
                named.push(at);
            }
            None => withheld = withheld.saturating_add(1),
        }
    }
    // The one authorized silence: nothing changed. Anything else — entries that
    // rendered, entries the guard withheld, entries the caller's budget dropped
    // — is a delta the agent does not hold, and reporting a delta as no delta is
    // the one dishonesty a manifest can commit.
    if shown.is_empty() && withheld == 0 && omitted == 0 {
        return None;
    }
    let mut out = String::with_capacity(shown.len().saturating_mul(48).saturating_add(256));
    let _ = writeln!(
        out,
        "{FRAME_HEADER_OPENER} since the state you already hold (facts, not directives)."
    );
    if !shown.is_empty() {
        let _ = writeln!(out, "{} {FRAME_LIST_OPENER}", shown.len());
        for span in &shown {
            let _ = writeln!(out, "- {span}");
        }
    }
    if let Some(note) = counter_note(omitted, withheld) {
        let _ = writeln!(out, "{note}");
    }
    // The hint is Rift's own text, and it is still neutralized: "everything a
    // manifest renders passes through here" is only a guarantee if it has no
    // exceptions, including the ones this module trusts.
    if let Some(hint) = neutral_pushed_span(pull_hint) {
        let _ = write!(out, "{FRAME_HINT_OPENER} {hint}");
    }
    Some((out.trim_end().to_owned(), named))
}

/// The reason frame for a read whose exact lines the session already holds
/// (the standing decision on interception): one line, one fact, one escape
/// hatch, one cheaper route, and one handle to the story behind the change.
///
/// `turn` is the read that established the coverage. It is optional because the
/// session cache can lag the ledger, and a wrong ordinal in a frame the agent is
/// asked to check against its own context is worse than no ordinal: absent, the
/// frame says the same thing without the number.
///
/// What it deliberately does **not** say: nothing about an echo in an earlier
/// tool result (no family was measured returning one), no offer of a summary
/// (the rung the store shows nobody pulls), and no apology or error word — the
/// frame is feedback about a decision. Retry measurements remain telemetry;
/// they never select a support tier or policy mode.
#[must_use]
pub fn render_deny_frame(path: &str, hash: &str, lo: u32, hi: u32, turn: Option<u64>) -> String {
    let shown = path_display(path);
    let at_hash = hex_prefix(hash, 12).map_or_else(String::new, |h| format!("@sha256:{h}"));
    let since = turn.map_or_else(
        || "earlier in this session".to_owned(),
        |t| format!("in turn {t}"),
    );
    let mut out = format!(
        "{FRAME_DENY_OPENER} {shown}{at_hash} lines {lo}-{hi} are {CLAUSE_UNCHANGED} {since} \
         and {CLAUSE_IN_CONTEXT}. {CLAUSE_REPEAT} if you still need it (it is allowed once)."
    );
    if let Some(hint) = pull_hint(path, hash) {
        let _ = write!(out, " {CLAUSE_PULL} {hint}");
    }
    if let Some(why) = why_hint(path, lo) {
        let _ = write!(out, " {why}");
    }
    out
}

/// The one line that stands in a terminal result for lines `lo`-`hi` of
/// `path` at `hash`, which the window was handed in turn `turn` and still
/// holds unchanged.
///
/// A fact the model can check against its own context, nothing more: the
/// lines, the file, the revision and the turn. Every line the window does not
/// hold stays in the result verbatim around it, so the line never has to say
/// what else the command printed.
#[must_use]
pub fn render_held_output_frame(path: &str, hash: &str, lo: u32, hi: u32, turn: u32) -> String {
    let shown = path_display(path);
    let at_hash = hex_prefix(hash, 12).map_or_else(String::new, |h| format!(" at sha256:{h}"));
    format!("{FRAME_HELD_OUTPUT_OPENER} lines {lo}-{hi} of {shown}{at_hash}, unchanged since turn {turn}.")
}

/// The reason frame for a read of a file the session holds at an older hash,
/// whose changes Rift can name exactly.
///
/// `None` when no site survives the clamp: a delta frame without exact sites is
/// the plain frame with a false claim attached, so the caller allows the read
/// instead. That is the program's conservative direction, expressed as a return
/// type rather than as a caller's discipline.
///
/// `shift` is the net line movement in the new numbering, rendered only when it
/// is non-zero — `(+0 lines)` is noise, and a counter that is noise gets skimmed
/// past.
#[must_use]
pub fn render_delta_frame(
    path: &str,
    hash: &str,
    sites: &[(u32, u32)],
    shift: i64,
    line_count: u32,
    context_lines: u32,
    turn: Option<u64>,
) -> Option<String> {
    let sites = clamped_sites(sites, line_count);
    if sites.is_empty() {
        return None;
    }
    let (min_lo, max_hi) = sites.iter().fold((u32::MAX, 0u32), |(lo, hi), (a, b)| {
        (lo.min(*a), hi.max(*b))
    });
    let window_lo = min_lo.saturating_sub(context_lines).max(1);
    let window_hi = {
        let raw = max_hi.saturating_add(context_lines);
        if line_count == 0 {
            raw
        } else {
            raw.min(line_count)
        }
    };
    let shown = path_display(path);
    let shift_clause = if shift == 0 {
        String::new()
    } else {
        format!(" ({shift:+} lines)")
    };
    let from_turn = turn.map_or_else(String::new, |t| format!(" from turn {t}"));
    let mut out = format!(
        "{FRAME_DENY_OPENER} {shown} {CLAUSE_CHANGED_ONLY} {}{shift_clause} since the revision \
         you hold{from_turn}; {CLAUSE_OTHERS_UNCHANGED}. Read lines {window_lo}-{window_hi} \
         {CLAUSE_IN_CONTEXT_WINDOW}, or repeat this exact call for the whole file (it is allowed \
         once).",
        render_sites(&sites)
    );
    if let Some(hint) = pull_hint(path, hash) {
        let _ = write!(out, " {CLAUSE_FULL_DIFF} {hint}");
    }
    // The line the frame is about: the first site it named, which is where the
    // change the agent is being asked to trust actually starts. The plain
    // frame's is the first line it declined. Both are a line the reader already
    // has in front of it, so the command is one it can run unedited.
    if let Some(why) = sites.first().and_then(|(lo, _)| why_hint(path, *lo)) {
        let _ = write!(out, " {why}");
    }
    Some(out)
}

/// The reason frame for a read whose lines the session holds only in part:
/// the held run, the turn it was read in, and the one run still to read.
///
/// `held` and `unheld` are `(lo, hi)` pairs in the numbering of the bytes the
/// read is about, and together they are exactly the lines the call asked for.
/// `turn` is the read the held run came from; where several reads made up the
/// run, the caller names the latest, which is the one the window saw last.
/// Absent, the frame says the same thing without the number, as the plain
/// frame does.
///
/// It carries no pull hint and no provenance handle. Both are routes to a
/// change, and this frame is about lines that did not change; every token it
/// spends raises the number of held lines below which narrowing costs more
/// than it spares.
#[must_use]
pub fn render_narrow_frame(
    path: &str,
    hash: &str,
    held: (u32, u32),
    unheld: (u32, u32),
    turn: Option<u64>,
) -> String {
    let shown = path_display(path);
    let at_hash = hex_prefix(hash, 12).map_or_else(String::new, |h| format!("@{h}"));
    let since = turn.map_or_else(
        || "earlier in this session".to_owned(),
        |t| format!("in turn {t}"),
    );
    let (held_lo, held_hi) = held;
    let (read_lo, read_hi) = unheld;
    format!(
        "{FRAME_NARROW_OPENER} {shown}{at_hash} lines {held_lo}-{held_hi} are {CLAUSE_UNCHANGED} \
         {since} and {CLAUSE_ALREADY_IN_CONTEXT}. Read lines {read_lo}-{read_hi} for the rest, or \
         repeat this exact call for the whole range (it is allowed once)."
    )
}

/// The frame that answers a first read of a large file with the file's bound
/// outline: the shape with the whole file's cost, the costliest definitions
/// with their lines and costs, the cheaper read, the identical repeat, and
/// the provenance handle.
///
/// Every number is the caller's, read from the outline the worker bound — no
/// count is taken here, and a figure the binding did not store is left out
/// rather than estimated. Every name goes through the inline guard; a name it
/// withholds is counted, never quoted.
///
/// `None` when no definition renders: an outline with nothing to route a read
/// to is not worth the call it would stand in for, and the caller lets the
/// read run.
#[must_use]
pub fn render_outline_frame(read: &OutlinedRead<'_>) -> Option<String> {
    render_outline(read, OutlineCause::FirstContact).map(|(text, _)| text)
}

/// The same frame, sent because another window of the repository holds these
/// bytes: one clause in the first sentence says so, and every other line is
/// the first-contact frame's.
///
/// Returns the spans the frame named beside it, in file order: they are the
/// reads a follow-up is admitted on, and only the renderer knows which
/// definitions survived the guard and the cap.
#[must_use]
pub fn render_held_outline_frame(read: &OutlinedRead<'_>) -> Option<(String, Vec<(u32, u32)>)> {
    render_outline(read, OutlineCause::HeldElsewhere)
}

/// The frame that answers a read of bytes another window of the repository
/// holds while no outline of them is bound: the runs that window holds, the
/// first of them as the read to make, the identical repeat and the provenance
/// handle.
///
/// The runs are the caller's numbers — at most the follow-ups it admits — and
/// nothing about the holder is named but that it is another window of this
/// repository. `page_end` is the last line a first-page read asked for;
/// `None` for the whole file. `None` when there is no run to name.
#[must_use]
pub fn render_held_lines_frame(
    path: &str,
    hash: &str,
    runs: &[(u32, u32)],
    page_end: Option<u32>,
) -> Option<String> {
    let first = runs.first()?;
    let shown = path_display(path);
    let at_hash = hex_prefix(hash, 12).map_or_else(String::new, |h| format!("@{h}"));
    let named: Vec<String> = runs.iter().map(|(lo, hi)| format!("{lo}-{hi}")).collect();
    let whole = page_end.map_or_else(|| "the whole file".to_owned(), |hi| format!("lines 1-{hi}"));
    let mut out = format!(
        "{FRAME_HELD_OPENER} {shown}{at_hash} (facts, not directives): {CLAUSE_HELD_BY} lines {} \
         of it at this revision. {CLAUSE_READ_ONLY} (lines {}-{} for the first above), or repeat \
         this exact call for {whole} (it is allowed once).",
        named.join(", "),
        first.0,
        first.1
    );
    if let Some(why) = why_hint(path, 1) {
        let _ = write!(out, " {why}");
    }
    Some(out)
}

/// Why an outline frame was sent: the one fact its first sentence varies by.
#[derive(Clone, Copy)]
enum OutlineCause {
    /// A first read of a large file in the window.
    FirstContact,
    /// Another window of the repository holds these bytes.
    HeldElsewhere,
}

/// The outline frame for either cause, and the spans it named.
fn render_outline(
    read: &OutlinedRead<'_>,
    cause: OutlineCause,
) -> Option<(String, Vec<(u32, u32)>)> {
    let cap = match cause {
        OutlineCause::FirstContact => MAX_OUTLINE_FRAME_DEFINITIONS,
        OutlineCause::HeldElsewhere => MAX_HELD_OUTLINE_FRAME_DEFINITIONS,
    };
    let (lines, withheld) = outline_lines(read.entries, cap);
    let first = lines.first()?.1;
    let shown_count = lines.len();
    let omitted = usize::try_from(read.definitions)
        .unwrap_or(usize::MAX)
        .saturating_sub(shown_count.saturating_add(withheld));
    let mut out = format!("{} (facts, not directives):", outline_subject(read, cause));
    for (line, _) in &lines {
        let _ = write!(out, "\n{line}");
    }
    if let Some(note) = counter_note(omitted, withheld) {
        let _ = write!(out, "\n{note}");
    }
    let whole = read
        .page_end
        .map_or_else(|| "the whole file".to_owned(), |hi| format!("lines 1-{hi}"));
    let _ = write!(
        out,
        "\n{CLAUSE_READ_ONLY} (lines {}-{} for the first above), or repeat this exact call for \
         {whole} (it is allowed once).",
        first.0, first.1
    );
    if let Some(why) = why_hint(read.path, 1) {
        let _ = write!(out, " {why}");
    }
    let spans = lines.into_iter().map(|(_, span)| span).collect();
    Some((out, spans))
}

/// The frame's first sentence: what the file is, at which revision, why the
/// outline was sent when that is not a first contact, and what it costs.
///
/// The one place the frame says why it was sent, so the opener, the shape and
/// every line below stay one frame whatever the cause.
fn outline_subject(read: &OutlinedRead<'_>, cause: OutlineCause) -> String {
    let shown = path_display(read.path);
    let at_hash = hex_prefix(read.hash, 12).map_or_else(String::new, |h| format!("@{h}"));
    let cost = read
        .whole_tokens
        .map_or_else(String::new, |t| format!(", about {t} tokens"));
    let listed = if read.markdown {
        "headings"
    } else {
        "top-level definitions"
    };
    let why = match cause {
        OutlineCause::FirstContact => String::new(),
        OutlineCause::HeldElsewhere => format!(" {CLAUSE_HELD_ELSEWHERE}; it"),
    };
    format!(
        "{FRAME_OUTLINE_OPENER} {shown}{at_hash}{why} is {} lines, {} bytes{cost}, with {} {listed}",
        read.lines, read.bytes, read.definitions
    )
}

/// The definition lines the frame names, in file order with their spans,
/// and how many the guard withheld.
///
/// The costliest `cap` of the ones the guard passes; where no count was
/// stored they rank equal and the first in the file are kept.
fn outline_lines(entries: &[OutlineEntry<'_>], cap: usize) -> (Vec<OutlineLine>, usize) {
    let mut passed: Vec<(usize, &OutlineEntry<'_>, String)> = Vec::new();
    let mut withheld = 0usize;
    for (at, entry) in entries.iter().enumerate() {
        match neutral_inline_span(entry.name) {
            Some(name) if name == entry.name.trim() => passed.push((at, entry, name)),
            _ => withheld = withheld.saturating_add(1),
        }
    }
    passed.sort_by(|a, b| b.1.tokens.cmp(&a.1.tokens).then(a.0.cmp(&b.0)));
    passed.truncate(cap);
    passed.sort_by_key(|(at, _, _)| *at);
    let lines = passed
        .into_iter()
        .map(|(_, entry, name)| {
            let cost = entry
                .tokens
                .map_or_else(String::new, |t| format!(", about {t} tokens"));
            let span = (entry.first_line, entry.last_line);
            let line = format!(
                "- {} {name} lines {}-{}{cost}",
                entry.kind.as_str(),
                span.0,
                span.1
            );
            (line, span)
        })
        .collect();
    (lines, withheld)
}

/// The held set, injected once at the boundary where the context lost it
/// with a verdict per entry.
///
/// This is the one moment a full held-state listing pays for itself, and the
/// verdict is why: a compacted context is about to re-read wholesale, and an
/// entry that says `unchanged since your read` is a re-read the model can skip
/// outright, while `changed at lines …` is one it can narrow to a pull. The
/// standing decision on quiet turns says why the same listing pays nowhere else
/// — it costs hundreds of tokens a turn and breaks cache-optimal silence — and
/// this renderer is not reachable from that path.
///
/// `budget_chars` fits the map to the caller's injection budget by dropping
/// **whole entries from the tail** — the list is ranked by recency, so the tail
/// is the coldest end — and counting them into the same clause the caller's own
/// `omitted` feeds. Characters rather than tokens because the tokenizer is not
/// this crate's, and the caller converts once (a `recovery_budget` in tokens is
/// four characters a token, near enough for a bound whose only job is to stop a
/// listing from eating a turn).
///
/// `None` is silence, and it means the same thing as it does for the manifest:
/// there was nothing to say. An entry the guard withheld, or the budget
/// dropped, is not nothing — it renders as a count.
#[must_use]
pub fn render_recovery_map(
    entries: &[HeldEntry<'_>],
    omitted: usize,
    budget_chars: usize,
) -> Option<String> {
    render_recovery_map_naming(entries, omitted, budget_chars).map(|(text, _)| text)
}

/// [`render_recovery_map`], and **which entries it named**, by index.
///
/// One walk, two answers. A caller that acts on having told a window what it
/// holds has to know which entries the frame actually carried — the guard
/// withholds some, the budget drops others, and both render as a count rather
/// than as a fact the model was given. Deriving that list a second time, or
/// reading it back out of the rendered text, is how the frame and the record
/// of the frame come to disagree.
#[must_use]
pub fn render_recovery_map_naming(
    entries: &[HeldEntry<'_>],
    omitted: usize,
    budget_chars: usize,
) -> Option<(String, Vec<usize>)> {
    let mut named: Vec<usize> = Vec::with_capacity(entries.len());
    let mut lines: Vec<String> = Vec::with_capacity(entries.len());
    let mut withheld = 0usize;
    let mut dropped = 0usize;
    let mut spent = 0usize;
    let mut hint: Option<String> = None;
    for (at, e) in entries.iter().enumerate() {
        let Some(path) = neutral_inline_span(e.path) else {
            withheld = withheld.saturating_add(1);
            continue;
        };
        if !lines.is_empty() && spent >= budget_chars {
            dropped = dropped.saturating_add(1);
            continue;
        }
        let line = render_held_entry(e, &path);
        spent = spent.saturating_add(line.chars().count());
        lines.push(line);
        named.push(at);
        if hint.is_none() {
            hint = pull_hint(e.path, e.hash);
        }
    }
    let omitted = omitted.saturating_add(dropped);
    if lines.is_empty() && withheld == 0 && omitted == 0 {
        return None;
    }
    let shapes = fit_shape_lines(entries, &named, spent, budget_chars);
    let mut out = String::with_capacity(spent.saturating_add(256));
    let _ = writeln!(
        out,
        "{FRAME_RECOVERY_OPENER} {CLAUSE_HELD_LIST}, with the content hash and lines it held \
         (facts, not directives):"
    );
    for (line, shape) in lines.iter().zip(&shapes) {
        let _ = writeln!(out, "{line}");
        if let Some(shape) = shape {
            let _ = writeln!(out, "{shape}");
        }
    }
    if let Some(note) = counter_note(omitted, withheld) {
        let _ = writeln!(out, "{note}");
    }
    if let Some(hint) = hint {
        let _ = write!(out, "{FRAME_HINT_OPENER} {hint}");
    }
    Some((out.trim_end().to_owned(), named))
}

/// The section a subagent's window opens with: revisions its coordinator held
/// at that instant, which the subagent does not, and **which entries it
/// named**, by index.
///
/// A statement about someone else's context, never about the subagent's: it
/// names a path and the revision the coordinator read, so the subagent can
/// ask for the lines that moved instead of reading the file whole. The hash
/// renders in the 16-hex argument form the pull verbs take, because it is the
/// one thing here the subagent is expected to type back.
///
/// An entry renders only when every part of it is a fact Rift can vouch for.
/// The path must be a checkout path in the journal's own spelling — relative,
/// slash-separated, no `..`, no control character — and must pass the inline
/// guard unaltered: a path the guard would truncate or flatten names some
/// other file. The hash must be hex. An entry that fails any of these is
/// counted as withheld and never quoted, exactly as the other listings count
/// theirs. Past eight names the rest is counted too; `omitted`
/// is what the caller's budget already dropped.
///
/// `None` is silence, and it means there was nothing to say.
#[must_use]
pub fn render_coordinator_reads_naming(
    reads: &[CoordinatorRead<'_>],
    omitted: usize,
) -> Option<(String, Vec<usize>)> {
    let mut shown: Vec<String> = Vec::with_capacity(reads.len().min(MAX_COORDINATOR_READS));
    let mut named: Vec<usize> = Vec::with_capacity(shown.capacity());
    let mut withheld = 0usize;
    let mut over = 0usize;
    for (at, read) in reads.iter().enumerate() {
        let Some(entry) = render_coordinator_entry(read) else {
            withheld = withheld.saturating_add(1);
            continue;
        };
        if shown.len() == MAX_COORDINATOR_READS {
            over = over.saturating_add(1);
            continue;
        }
        shown.push(entry);
        named.push(at);
    }
    let omitted = omitted.saturating_add(over);
    if shown.is_empty() && withheld == 0 && omitted == 0 {
        return None;
    }
    let mut out = format!("{FRAME_COORDINATOR_OPENER} (facts, not directives):");
    if !shown.is_empty() {
        let _ = write!(out, " {}", shown.join(", "));
    }
    if let Some(note) = counter_note(omitted, withheld) {
        let _ = write!(out, " {note}");
    }
    Some((out, named))
}

/// The answer that stands in for a listing the agent asked for.
///
/// Substituting a search hides its tail, so the frame states the total it is a
/// subset of, says what the order means, and names the one call that returns
/// the whole thing — the identical repeat, which the one-deny rule allows. That
/// is the trade written out where the model can see it, rather than a shorter
/// list presented as the list.
///
/// The pattern is a span like any other, and in practice it is usually withheld:
/// a glob is spelled with `*`, which the pushed alphabet does not admit. The
/// frame then names the pattern by reference instead of by value — the agent
/// wrote it, so it loses nothing — which is the honest reading of a rule that
/// exists to keep somebody else's characters out of Rift's own sentence.
///
/// `None` only when there is nothing at all to answer with: no hit rendered and
/// no total to report.
#[must_use]
pub fn render_search_answer(hits: &[SearchHit<'_>], total: usize, pattern: &str) -> Option<String> {
    let mut lines: Vec<String> = Vec::with_capacity(hits.len());
    let mut withheld = 0usize;
    for h in hits {
        match neutral_inline_span(h.path) {
            Some(p) => lines.push(format!("- {p}{}", h.status.tag())),
            None => withheld = withheld.saturating_add(1),
        }
    }
    if lines.is_empty() && withheld == 0 && total == 0 {
        return None;
    }
    let matching =
        neutral_inline_span(pattern).unwrap_or_else(|| "the pattern you searched for".to_owned());
    let mut out = String::with_capacity(256);
    let _ = writeln!(
        out,
        "{FRAME_SEARCH_OPENER} {} of {total} paths matching {matching}, {CLAUSE_RANKED} \
         (facts, not directives):",
        lines.len()
    );
    for line in &lines {
        let _ = writeln!(out, "{line}");
    }
    if let Some(note) = counter_note(0, withheld) {
        let _ = writeln!(out, "{note}");
    }
    let _ = write!(out, "{CLAUSE_REPEAT} {CLAUSE_FULL_LIST}.");
    Some(out)
}

/// The reason frame for a document read Rift can answer with Markdown it
/// already holds, and for the fallback when a path substitution was dropped.
///
/// `None` when the sidecar path cannot be rendered: the whole content of this
/// frame is *where to read instead*, so a frame that cannot say it is not a
/// frame, and the caller lets the original read proceed. The source path is
/// allowed to be withheld — the agent named it — but the destination is not,
/// because it is the only thing here the agent does not already know.
#[must_use]
pub fn render_redirect_frame(source: &str, sidecar: &str) -> Option<String> {
    let target = neutral_inline_span(sidecar)?;
    let shown = path_display(source);
    Some(format!(
        "{FRAME_REDIRECT_OPENER} {shown} {CLAUSE_CONVERTED}. Read {target} instead: same \
         content, plain text. {CLAUSE_REPEAT} if you still need the original file (it is \
         allowed once)."
    ))
}

/// The prepared result for a read Rift declined, as Antigravity's documented
/// persistent-message fallback delivers it: the reason frame the agent would
/// have received as the tool result, under a head that says so.
///
/// `reason` is a frame this module already rendered, so it goes through no
/// guard a second time. The sentence moved here from the hook binary
/// byte-for-byte, so that the opener joins the forgery guard and no other
/// file spells it; the goldens pin the bytes.
#[must_use]
pub fn render_prepared_result(reason: &str) -> String {
    format!("{FRAME_PREPARED_OPENER}\n{reason}")
}

/// The bounded plan that answers an Antigravity `ContentOffset` probe: the
/// raw call ran, because what an offset covers is unverified, and the plan
/// names the landmarks (heading lines, or fixed spans when a file has none) to
/// read by explicit line span instead of probing further.
///
/// Moved here byte-for-byte from the hook binary for the same reason as
/// [`render_prepared_result`]: one module writes the sentences a model reads.
/// The path and the landmarks render as the caller hands them, as the sentence
/// did before the move; routing them through the chokepoint is a behaviour
/// change and is not this one.
#[must_use]
pub fn render_structural_read_plan(rel_path: &str, landmarks: &[String]) -> String {
    format!(
        "{FRAME_PLAN_OPENER} `{rel_path}`. ContentOffset request/result semantics are \
         unverified, so the raw call ran untouched. Use explicit line spans around these \
         landmarks instead of probing more offsets:\n{}",
        landmarks.join("\n")
    )
}

/// Neutralize one span for rendering **inside** a frame line, or withhold it.
///
/// [`neutral_pushed_span`] first — alphabet, role markers and directive
/// vocabulary are the same wherever a span lands — and then the positional
/// rule: a span rendered inside a frame's own line may not contain the frame's
/// wording anywhere, not merely open with it.
///
/// The containment test is sound in the same way the initial one is. It runs on
/// the normal form, in which every spelling of a phrase in the pushed alphabet
/// collapses onto the phrase itself (see `normalize_pushed`), and it runs on
/// the span as it will render, after truncation, so it judges exactly the text
/// the reader will see.
#[must_use]
pub fn neutral_inline_span(raw: &str) -> Option<String> {
    let span = neutral_pushed_span(raw)?;
    let n = normalize_pushed(&span);
    let forges_a_clause = FRAME_OPENERS_NORMALIZED
        .iter()
        .chain(FRAME_CLAUSES_NORMALIZED.iter())
        .any(|p| n.contains(p));
    (!forges_a_clause).then_some(span)
}

/// One entry's line, or `None` when its path may not be rendered at all.
///
/// The path is the only span here that came from outside Rift, so it is the
/// only one that goes through the guard; the revision is hex or it is not
/// rendered, and the ranges are integers this crate formats. A path the guard
/// withholds takes its hash and its ranges with it — an entry is a sentence
/// about a file, and half of one is not a fact.
///
/// More changed places than [`MAX_FRAME_SITES`] renders as `(changed)` rather
/// than as a capped list with a remainder. The recovery map counts its
/// remainder because its entries are what a session *held*, a set Rift knows
/// in full; a changed-range list is a claim that everything unnamed is
/// unchanged, and that claim cannot be made about a list that was cut short.
fn render_manifest_entry(entry: &ManifestEntry<'_>) -> Option<String> {
    let mut line = neutral_pushed_span(entry.path)?;
    if let Some(hash) = hex_prefix(entry.hash, 12) {
        let _ = write!(line, "@{hash}");
    }
    match entry.detail {
        EntryDetail::ChangedAt { ranges, shift }
            if !ranges.is_empty() && ranges.len() <= MAX_FRAME_SITES =>
        {
            let _ = write!(line, " {}", render_line_ranges(ranges, ","));
            if shift != 0 {
                let _ = write!(line, " ({shift:+})");
            }
        }
        EntryDetail::ChangedAt { .. } | EntryDetail::Changed => line.push_str(" (changed)"),
        EntryDetail::Binary => line.push_str(" (binary)"),
        EntryDetail::Removed => line.push_str(" (removed)"),
        EntryDetail::Unstated => {}
    }
    Some(line)
}

/// The shape line under each named entry that has one, in the named order,
/// for as many as fit in what the entries left of the budget.
///
/// The entries were fitted first, so a shape line never displaces an entry:
/// when the budget binds, shape lines are what go, and they go from the tail.
fn fit_shape_lines(
    entries: &[HeldEntry<'_>],
    named: &[usize],
    spent: usize,
    budget_chars: usize,
) -> Vec<Option<String>> {
    let mut spent = spent;
    named
        .iter()
        .map(|at| {
            let shape = entries.get(*at)?.shape?;
            let line = render_shape_line(shape);
            let after = spent.saturating_add(line.chars().count());
            if after > budget_chars {
                spent = usize::MAX;
                return None;
            }
            spent = after;
            Some(line)
        })
        .collect()
}

/// The line under a changed held file: the size of the revision the checkout
/// holds now, as counts only.
fn render_shape_line(shape: OutlineShape) -> String {
    format!(
        "  {CLAUSE_SHAPE_NOW} {} lines, {} bytes, {} top-level definitions",
        shape.lines, shape.bytes, shape.definitions
    )
}

/// One line of the recovery map: the path as the guard rendered it, the
/// revision, the lines held and the verdict — each part only when it has
/// something to say, so an entry Rift knows nothing more about is the path
/// alone.
fn render_held_entry(entry: &HeldEntry<'_>, path: &str) -> String {
    let at_hash = hex_prefix(entry.hash, 12).map_or_else(String::new, |h| format!("@{h}"));
    let held = render_held_ranges(entry.held);
    let verdict = match entry.verdict {
        HeldVerdict::Unchanged => format!(" {CLAUSE_UNCHANGED}"),
        HeldVerdict::ChangedAt(sites) if !sites.is_empty() => {
            format!(" {CLAUSE_CHANGED_AT} {}", render_sites(sites))
        }
        HeldVerdict::ChangedAt(_) | HeldVerdict::Unknown => String::new(),
    };
    let mut line = format!("- {path}{at_hash}");
    if !held.is_empty() {
        let _ = write!(line, " {held}");
    }
    line.push_str(&verdict);
    line
}

/// The counter a listing closes with, or `None` when there is nothing to
/// count.
///
/// Each clause appears only when it has something to say: "(0 more not shown)"
/// beside a real withheld count reads as noise, and a counter that is noise
/// gets skimmed past — which is how honest counts stop being read at all.
/// Three listings close with it, and one spelling is what keeps their counts
/// reading as the same fact.
fn counter_note(omitted: usize, withheld: usize) -> Option<String> {
    let mut clauses: Vec<String> = Vec::with_capacity(2);
    if omitted > 0 {
        clauses.push(format!("{omitted} {FRAME_OMITTED_OPENER}"));
    }
    if withheld > 0 {
        clauses.push(format!("{withheld} {FRAME_WITHHELD_OPENER}"));
    }
    if clauses.is_empty() {
        return None;
    }
    Some(format!("({})", clauses.join("; ")))
}

/// One revision of the coordinator section, or `None` when it may not render.
fn render_coordinator_entry(read: &CoordinatorRead<'_>) -> Option<String> {
    if !is_checkout_spelling(read.path) {
        return None;
    }
    let path = neutral_inline_span(read.path).filter(|span| span == read.path)?;
    let hash = hex_prefix(read.hash, 16)?;
    Some(format!("{path}@{hash}"))
}

/// Whether a path is spelled the way the window journal keys a checkout file:
/// relative, slash-separated, and naming nothing above the checkout.
///
/// The journal is a file an agent can write to, so a row is not trusted to
/// hold the spelling the gate wrote. An absolute path or a `..` names a file
/// outside the checkout — never something a coordinator in this checkout
/// could have read through the gate — and a backslash or a control character
/// is a second spelling of a path the guard would otherwise rewrite.
fn is_checkout_spelling(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !path.split('/').any(|part| part.is_empty() || part == "..")
}

/// The display form of a path inside a frame: the span, or the stand-in.
fn path_display(path: &str) -> String {
    neutral_inline_span(path).unwrap_or_else(|| WITHHELD_PATH.to_owned())
}

/// The first `n` hex characters of a content hash, or `None`.
///
/// `None` for anything that is not hex, or shorter than the form asks for. A
/// hash is the one span in a frame the agent is expected to type back into a
/// pull verb, so a guess is worse than an omission: 12 is the display form and
/// 16 the argument form the pull verbs accept, and nothing shorter is a hash
/// at all.
/// Uppercase input is lowered rather than refused — the value is the same and
/// the store's own spelling is lowercase.
fn hex_prefix(hash: &str, n: usize) -> Option<String> {
    if hash.len() < n || !hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(
        hash.chars()
            .take(n)
            .map(|c| c.to_ascii_lowercase())
            .collect(),
    )
}

/// The runnable pull command for one file, or `None` when it would not be
/// runnable.
///
/// Three ways it is not, each dropping the whole hint rather than rendering a
/// broken one: the path is withheld (there is nothing to name), the hash is not
/// a 16-hex basis (the verb would answer "unknown" and the agent would have
/// spent a turn learning that), or the path contains a space (the command as
/// written would parse as two arguments, and this module owns no quoting rule —
/// `'` and `"` are outside the pushed alphabet).
fn pull_hint(path: &str, hash: &str) -> Option<String> {
    let p = neutral_inline_span(path)?;
    if p.contains(' ') {
        return None;
    }
    let h = hex_prefix(hash, 16)?;
    Some(format!(
        "rift recall changed-hunks {p} --known-hash {h} --json"
    ))
}

/// The runnable provenance command for one line of one file, or `None` when it
/// would not be runnable.
///
/// The same two ways a hint is dropped rather than rendered broken as
/// [`pull_hint`]'s first two: a withheld path has nothing to name, and a path
/// with a space in it would parse as two arguments and this module owns no
/// quoting rule. There is no hash arm — `rift why` addresses a line, not a
/// revision, so nothing here can be a stale or truncated address.
///
/// **Fixed text plus two spans, and that is the whole of it.** This is the one
/// thing a frame may add on the gate path: no store is opened to compose it, no
/// projection is consulted, and no tokenizer runs. The facts the command
/// *fetches* are joined in the pull verb, where a process may afford them.
fn why_hint(path: &str, line: u32) -> Option<String> {
    let p = neutral_inline_span(path)?;
    if p.contains(' ') {
        return None;
    }
    Some(format!("{CLAUSE_PROVENANCE} {p}:{line} --json"))
}

/// Render line ranges as `lo-hi`, comma-joined, capped at [`MAX_FRAME_SITES`].
///
/// The separators are the frame's own punctuation over integers the caller
/// computed, not spans: `,` is outside the pushed alphabet precisely so no
/// *data* can spell one, and this module writing one between two numbers takes
/// nothing back from that.
fn render_sites(sites: &[(u32, u32)]) -> String {
    let mut out = String::new();
    for (i, (lo, hi)) in sites.iter().take(MAX_FRAME_SITES).enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        let _ = write!(out, "{lo}-{hi}");
    }
    let rest = sites.len().saturating_sub(MAX_FRAME_SITES);
    if rest > 0 {
        let _ = write!(out, " and {rest} more");
    }
    out
}

/// Keep the sites that are real line ranges of a file of `line_count` lines
/// (`0` means the count is unknown and only the range's own shape is checked).
///
/// The clamp is the frame's half of the property that authorized denying on an
/// edit at all: the numbers
/// a deny frame names are computed from the new bytes, so they are exact or
/// absent. A site past the end of the file is not exact, and a frame naming it
/// teaches the agent a line that is not there.
fn clamped_sites(sites: &[(u32, u32)], line_count: u32) -> Vec<(u32, u32)> {
    sites
        .iter()
        .copied()
        .filter(|(lo, hi)| *lo >= 1 && lo <= hi)
        .filter(|(lo, _)| line_count == 0 || *lo <= line_count)
        .map(|(lo, hi)| {
            (
                lo,
                if line_count == 0 {
                    hi
                } else {
                    hi.min(line_count)
                },
            )
        })
        .collect()
}

/// Render the ranges a session held, as `L<lo>-<hi>`, comma-joined.
fn render_held_ranges(ranges: &[(u32, u32)]) -> String {
    render_line_ranges(ranges, ", ")
}

/// `L<lo>-<hi>` line ranges joined by `sep`, capped at [`MAX_FRAME_SITES`].
///
/// Two separators, one implementation: a map entry sits in a sentence and
/// reads better spaced, a manifest entry is a dense line inside a turn budget
/// and pays for every space. The numbers, the `L`, the cap and the way the
/// remainder is counted are the same fact in both places, so they are written
/// once — a second copy is how two surfaces come to disagree about what a
/// range is.
fn render_line_ranges(ranges: &[(u32, u32)], sep: &str) -> String {
    let mut out = String::new();
    for (i, (lo, hi)) in ranges.iter().take(MAX_FRAME_SITES).enumerate() {
        if i > 0 {
            out.push_str(sep);
        }
        let _ = write!(out, "L{lo}-{hi}");
    }
    let rest = ranges.len().saturating_sub(MAX_FRAME_SITES);
    if rest > 0 {
        let _ = write!(out, " and {rest} more");
    }
    out
}

#[cfg(test)]
mod tests;
