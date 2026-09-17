//! `cargo run -p xtask -- replay-savings --transcripts <dir>` — what this
//! build's read gate would have decided about sessions that already happened.
//!
//! The savings figures Rift publishes are booked from decisions it took while
//! an agent waited. Before a change to the read grammar or the read ledger can
//! be argued for, somebody has to be able to say what the change is worth on
//! real traffic, and the only traffic that is not a benchmark is the owner's
//! own recorded sessions. This replays them: no model is called, no store is
//! opened, nothing is written, and every number comes out the same on two runs
//! of the same input.
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
//! real Claude Code adapter and the pure read ledger, so the harness reflects
//! whatever grammar the checkout it is built from has. That is how a before
//! and an after are produced: run it on `main`, run it on the branch, compare
//! the two tables. Nothing about the buckets below is a second opinion about
//! what a read is.
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
//!    the ledger asks of a hash.
//! 2. **Line counts come from the worktree**, or from a whole-file write's own
//!    payload when the window created the file. A path with neither is left
//!    undecided rather than guessed at.
//! 3. **A slice is measured at the larger** of what the current worktree holds
//!    for those lines and what the result actually returned. Both readings are
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
//! [`AgentAdapter::read_requests`] yields and applies the binary's own
//! all-or-nothing rule: the call is declined only when every member of it
//! resolved and every member was held. One member the window does not hold —
//! or one nobody could resolve at all — and the whole line runs, because a
//! decline answers the command line and the agent would lose the reads that
//! were never in question. What that costs is published as
//! `declines lost to an unheld sibling read` rather than absorbed.
//!
//! **A call is decided before it runs and reconciled after it.** At the result
//! the client states how much of the file it handed over, and
//! [`read_ledger::reconcile_delivery`] moves the window's coverage to agree
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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rift_agent::read_ledger::{
    self, Decision, DenyShape, EditEcho, EvictReason, Interval, Ledger, Limits, Line, Mode,
    Reconcile, ResolvedRead, StatedDelivery,
};
use rift_agent::respond::Channel;
use rift_agent::{AgentAdapter, EditKind, ReadRequest};
use rift_agent_claude_code::ClaudeCodeAdapter;
use rift_core::Sha256Id;
use serde_json::{json, Map, Value};

/// The version of the machine-readable document this command writes. A later
/// shape gets a later number rather than a reader that tolerates both.
pub const SCHEMA_VERSION: u32 = 2;

/// The marker a Claude Code result carries when the client wrote the output to
/// a file and handed the window a pointer and a short preview instead.
///
/// It is the exact string the client writes, and it is the one unambiguous
/// sighting of the case the shell delivery ceiling exists for: the command ran,
/// the lines exist, and what reached the context is a filename.
const PERSISTED_OUTPUT: &str = "<persisted-output>";

/// The sentences Claude Code's own identical-repeat guard answers a read with.
///
/// A result carrying one of these is a read the client itself declined, so the
/// tokens were never spent and there is nothing left for Rift to save. They are
/// counted on their own line rather than folded into the declinable pool, which
/// would credit Rift with a saving another mechanism made.
const CLIENT_DEDUP_MARKERS: [&str; 2] = ["Wasted call", "File unchanged since last read"];

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

/// The largest file the gate will hash inline, and so the largest file a
/// decision is ever reached about. Mirrors the shipped default.
const MAX_INLINE_HASH_BYTES: u64 = 1024 * 1024;

// ------------------------------------------------------------------ options ---

/// What one run was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Options {
    /// The client project directory holding `<session>.jsonl` files and their
    /// `<session>/subagents/` windows.
    pub transcripts: PathBuf,
    /// The checkout the recorded paths are resolved against, for line counts
    /// and slice sizes.
    pub repo: PathBuf,
    /// Write the machine-readable document instead of the table.
    pub json: bool,
}

/// Parse this command's arguments.
///
/// `default_repo` is used when `--repo` is absent, so a caller that knows the
/// workspace root does not have to spell it.
///
/// # Errors
/// An unknown argument, a flag with no value, or a missing `--transcripts`.
pub fn parse_args(args: &[String], default_repo: &Path) -> Result<Options, String> {
    const USAGE: &str = "usage: xtask replay-savings --transcripts <dir> [--repo <dir>] [--json]";
    let mut transcripts = None;
    let mut repo = None;
    let mut json = false;
    let mut at = 0usize;
    while let Some(arg) = args.get(at) {
        let value = |name: &str| -> Result<String, String> {
            args.get(at + 1)
                .cloned()
                .ok_or_else(|| format!("replay-savings: {name} needs a value\n{USAGE}"))
        };
        match arg.as_str() {
            "--transcripts" => {
                transcripts = Some(PathBuf::from(value("--transcripts")?));
                at += 2;
            }
            "--repo" => {
                repo = Some(PathBuf::from(value("--repo")?));
                at += 2;
            }
            "--json" => {
                json = true;
                at += 1;
            }
            other => {
                return Err(format!(
                    "replay-savings: unknown argument `{other}`\n{USAGE}"
                ))
            }
        }
    }
    Ok(Options {
        transcripts: transcripts
            .ok_or_else(|| format!("replay-savings: --transcripts is required\n{USAGE}"))?,
        repo: repo.unwrap_or_else(|| default_repo.to_path_buf()),
        json,
    })
}

/// Replay the transcripts and print the report.
///
/// # Errors
/// If the transcripts directory cannot be walked.
pub fn run(opts: &Options) -> Result<(), String> {
    let report = replay(opts)?;
    if opts.json {
        println!("{}", render_json(&report));
    } else {
        println!("{}", render_table(&report));
    }
    Ok(())
}

// ------------------------------------------------------------------ buckets ---

/// Where one repository-content call landed.
///
/// Exactly one per call, and the order below is the order the table prints
/// them: what the gate would have acted on first, then what it would have
/// reached and left alone, then the reasons it would never have reached a
/// decision at all.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bucket {
    /// First sight of this `(path, revision)` in this context window. The
    /// read that can never be declined — the floor of any design that does
    /// not lie about what a window holds.
    FirstSight,
    /// A repeat wholly inside held coverage, which this build would decline.
    Declined,
    /// A repeat wholly inside coverage, under the line floor: decided and
    /// recorded, never acted on, because the reason frame would cost more
    /// than the lines it replaces.
    BelowFloor,
    /// A repeat wholly inside coverage the build reached a decision about and
    /// did not act on for any other reason.
    ObservedNotDeclined,
    /// The one allowed repeat of a call this window already declined — the
    /// rule that bounds a decline to a single extra round trip.
    RepeatAfterDecline,
    /// A repeat inside coverage the gate reached and had nothing exact to say
    /// about: it asks for exactly the lines an edit changed, or the frame
    /// would have had to name a site that did not survive the clamp. Either
    /// way the read runs.
    NoHonestFrame,
    /// A repeat of a revision this window has read, overlapping held coverage
    /// without being inside it.
    RepeatOverlapping,
    /// A repeat of a revision this window has read, over lines it does not
    /// hold.
    RepeatDisjoint,
    /// The slice is past the size a delivery can still be vouched for, and
    /// the result stated nothing that could stand in for density, so the gate
    /// records nothing and decides nothing.
    OverCreditBound,
    /// The slice is past that size and the client said outright how much of
    /// the file it handed over, so the window is credited from the statement
    /// one hook after the call. The read still runs — nothing past the
    /// ceiling is ever declined — but it now leaves coverage behind for the
    /// next read of the same revision to be decided against.
    CreditedFromStatement,
    /// The client wrote the result to a file; the window holds a pointer and a
    /// preview, never the lines.
    SpilledToAFile,
    /// A recognised read whose span no artifact pins — a decorated whole-file
    /// print, a default-window `head`.
    UnknownSpan,
    /// A read tool call outside the pinned argument set — pages of a document,
    /// cells of a notebook, an argument added after the pin was taken.
    NotReadShaped,
    /// No file on disk and no whole-file write to take a line count from.
    NoLineCount,
    /// The client's own identical-repeat guard answered the call.
    ClientDedup,
    /// The transcript holds the call and no result for it.
    NoResultRecorded,
    /// A name or content search: a repository-content channel the read ledger
    /// never decides about.
    Search,
}

impl Bucket {
    /// Every bucket, in the order the table prints them.
    pub const ALL: [Self; 17] = [
        Self::FirstSight,
        Self::Declined,
        Self::BelowFloor,
        Self::ObservedNotDeclined,
        Self::RepeatAfterDecline,
        Self::NoHonestFrame,
        Self::RepeatOverlapping,
        Self::RepeatDisjoint,
        Self::OverCreditBound,
        Self::CreditedFromStatement,
        Self::SpilledToAFile,
        Self::UnknownSpan,
        Self::NotReadShaped,
        Self::NoLineCount,
        Self::ClientDedup,
        Self::NoResultRecorded,
        Self::Search,
    ];

    /// The key this bucket is published under, which a script may depend on.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::FirstSight => "first_sight",
            Self::Declined => "declined",
            Self::BelowFloor => "below_floor",
            Self::ObservedNotDeclined => "observed_not_declined",
            Self::RepeatAfterDecline => "repeat_after_decline",
            Self::NoHonestFrame => "no_honest_frame",
            Self::RepeatOverlapping => "repeat_overlapping",
            Self::RepeatDisjoint => "repeat_disjoint",
            Self::OverCreditBound => "over_credit_bound",
            Self::CreditedFromStatement => "credited_from_statement",
            Self::SpilledToAFile => "spilled_to_a_file",
            Self::UnknownSpan => "unknown_span",
            Self::NotReadShaped => "not_read_shaped",
            Self::NoLineCount => "no_line_count",
            Self::ClientDedup => "client_dedup",
            Self::NoResultRecorded => "no_result_recorded",
            Self::Search => "search",
        }
    }

    /// The sentence the table prints for this bucket.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::FirstSight => "first sight of (path, revision) in this window",
            Self::Declined => "repeat inside coverage — DECLINED",
            Self::BelowFloor => "repeat inside coverage, under the line floor",
            Self::ObservedNotDeclined => "repeat inside coverage, decided not acted on",
            Self::RepeatAfterDecline => "the one allowed repeat of a declined call",
            Self::NoHonestFrame => "repeat inside coverage with nothing exact to say",
            Self::RepeatOverlapping => "repeat overlapping held coverage",
            Self::RepeatDisjoint => "repeat of a read revision, over unheld lines",
            Self::OverCreditBound => "slice past the credit bound; nothing recorded",
            Self::CreditedFromStatement => "past the bound, credited from the client's statement",
            Self::SpilledToAFile => "result written to a file; a pointer reached the window",
            Self::UnknownSpan => "recognised read with no usable line range",
            Self::NotReadShaped => "read tool call outside the pinned argument set",
            Self::NoLineCount => "no name or line count for the path",
            Self::ClientDedup => "the client's own repeat guard answered it",
            Self::NoResultRecorded => "no result for the call in the transcript",
            Self::Search => "name or content search",
        }
    }

    /// Whether the read ledger reached a decision that the range was already
    /// held — whether or not it acted on it.
    #[must_use]
    pub const fn declinable(self) -> bool {
        matches!(
            self,
            Self::Declined | Self::BelowFloor | Self::ObservedNotDeclined
        )
    }
}

// ------------------------------------------------------------------- report ---

/// How many calls landed somewhere, and the tokens their results returned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// How many calls.
    pub calls: u64,
    /// The tokens their results returned, counted with the real tokenizer.
    pub tokens: u64,
}

impl Tally {
    /// Fold one call in.
    fn add(&mut self, tokens: u64) {
        self.calls = self.calls.saturating_add(1);
        self.tokens = self.tokens.saturating_add(tokens);
    }

    /// Fold another tally in.
    fn merge(&mut self, other: Self) {
        self.calls = self.calls.saturating_add(other.calls);
        self.tokens = self.tokens.saturating_add(other.tokens);
    }
}

/// Everything one replay concluded.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// How many session trees were walked.
    pub trees: u64,
    /// How many context windows — a main session, or one subagent file.
    pub windows: u64,
    /// How many tool calls were replayed, of every tool.
    pub tool_calls: u64,
    /// Every tool result in the corpus, of every tool: the informational
    /// ceiling everything else is a part of.
    pub all_results: Tally,
    /// Per-bucket totals, keyed by [`Bucket::key`].
    pub buckets: BTreeMap<&'static str, Tally>,
    /// The read channels: a read tool, or a recognised shell read.
    pub reads: Tally,
    /// The search channel.
    pub searches: Tally,
    /// Reads plus searches: the denominator the share is taken over.
    pub repo_content: Tally,
    /// What the ledger found already held, acted on or not.
    pub declinable: Tally,
    /// What this build would actually have declined.
    pub declined: Tally,
    /// What the declines' own reason frames cost, counted with the same
    /// tokenizer over the sealed renderer's output.
    pub frame_tokens: u64,
    /// The reads that can never be declined: the first sight of a revision in
    /// a window.
    pub first_read_floor: Tally,
    /// `(path, revision)` pairs a session tree read in two or more windows.
    pub cross_window_pairs: u64,
    /// What the second and later windows' first reads of those pairs cost.
    pub cross_window: Tally,
    /// Shell calls that look like reads and that the grammar does not
    /// recognise: invisible to the gate, and outside every figure above.
    ///
    /// The loose end of a bound. A read word at a command position catches
    /// `… | tail -20` over a build log as readily as `sed -n '1,40p' <file>`,
    /// so this figure is an over-count by construction and is published beside
    /// the tight end rather than on its own.
    pub invisible_shell: Tally,
    /// The tight end of the same bound: of those, the calls that also name a
    /// file this checkout actually has. A command that names a work-tree file
    /// and prints part of it is a read whatever else it does.
    pub invisible_shell_named_file: Tally,
    /// Every call to the shell tool, recognised or not.
    pub shell_calls: u64,
    /// How many of them the pinned grammar recognised at all.
    pub shell_recognised: u64,
    /// Calls that perform more than one read — a chained command line.
    pub calls_with_several_reads: u64,
    /// Every read every repository-content call performs, members included.
    pub read_members: u64,
    /// Members the ledger would have declined, allowed because a sibling read
    /// on the same command line was not held. The price of the all-or-nothing
    /// rule, stated rather than absorbed.
    pub declines_lost_to_a_sibling: u64,
    /// Repository-content calls whose result carried the client's own result
    /// object at all.
    pub results_with_a_response: u64,
    /// Of those, the ones the pinned rule could read a statement out of. The
    /// gap between the two is the corpus saying nothing rather than the rule
    /// refusing it, and a zero credit means one of those two things.
    pub statements_read: u64,
    /// Members the client's own statement credited coverage for after the
    /// call, which density could not vouch for before it.
    pub deliveries_credited: u64,
    /// Members whose result stated less than the window had been credited
    /// with, each of them an emptying and a re-record.
    pub deliveries_taken_back: u64,
    /// Reads of files outside the work tree, which scope containment takes out
    /// of the denominator.
    pub outside_work_tree: Tally,
    /// Reads of a file the window had just edited with something other than a
    /// whole-file write — the pool this harness declines to model as coverage.
    pub verification_after_edit: Tally,
    /// How many declines rest on the window's own whole-file write rather than
    /// on a read, so a reader can take them out.
    pub declines_resting_on_a_write: u64,
    /// How many context-window emptyings were replayed.
    pub evictions: u64,
    /// How many mutating tool calls advanced a revision.
    pub edits: u64,
    /// The basis of every token count in this report.
    pub tokenizer: &'static str,
    /// Tool names whose calls are none of the shapes above, and how many.
    pub other_tools: BTreeMap<String, u64>,
}

impl Report {
    /// One bucket's totals.
    #[must_use]
    pub fn bucket(&self, bucket: Bucket) -> Tally {
        self.buckets.get(bucket.key()).copied().unwrap_or_default()
    }

    /// What a decline is worth once its own reason frame is paid for.
    #[must_use]
    pub fn declined_net_tokens(&self) -> u64 {
        self.declined.tokens.saturating_sub(self.frame_tokens)
    }

    /// The tokens no channel above accounts for.
    #[must_use]
    pub fn remainder_tokens(&self) -> u64 {
        self.all_results
            .tokens
            .saturating_sub(self.repo_content.tokens)
    }
}

/// A percentage, with `0 / 0` defined as `0` — the same guard every other
/// rate in this tree divides behind.
#[must_use]
pub fn share_percent(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 * 100.0 / denominator as f64
    }
}

// -------------------------------------------------------------- the transcript ---

/// One tool call, with the result that came back for it.
struct Call {
    /// The tool's own name, as the client spelled it.
    name: String,
    /// The tool's arguments.
    input: Value,
    /// The working directory the record states, which a relative path is
    /// resolved against and against nothing else.
    cwd: Option<String>,
    /// The result text, or `None` when the transcript holds no result.
    result: Option<String>,
    /// The client's own result object for this call — `toolUseResult` in a
    /// transcript, `tool_response` on the wire the hook reads. It is what the
    /// delivery statement of [`stated_delivery`] is built from.
    response: Option<Value>,
}

/// One replayable event in a window, in the order it happened.
enum Event {
    /// A tool call.
    Call(Box<Call>),
    /// The window stopped holding what it had been given.
    Evict,
    /// A new turn started.
    Turn,
}

/// One context window's events, with the sort key that merges it with its
/// siblings into one tree-wide order.
struct Window {
    /// The window's own events.
    events: Vec<(String, usize, Event)>,
}

/// The text of one `tool_result` block's content, which is a string or a list
/// of typed parts.
fn result_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => {
            let mut out = String::new();
            for part in parts {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// The blocks of one record's message, when it has any.
fn blocks(record: &Value) -> &[Value] {
    record
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Whether this record is a context-window boundary the client announced.
fn is_eviction(record: &Value) -> bool {
    let field = |key: &str| record.get(key).and_then(Value::as_str);
    record.get("isCompactSummary") == Some(&Value::Bool(true))
        || (field("type") == Some("system") && field("subtype") == Some("compact_boundary"))
}

/// Whether this record is a person's own prompt, which starts a turn.
fn is_prompt(record: &Value) -> bool {
    if record.get("type").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let content = record.get("message").and_then(|m| m.get("content"));
    match content {
        Some(Value::String(_)) => true,
        Some(Value::Array(parts)) => !parts
            .iter()
            .any(|part| part.get("type").and_then(Value::as_str) == Some("tool_result")),
        _ => false,
    }
}

/// Read one window file into the events it replays.
///
/// Results are collected first and then paired to their calls by id, because a
/// client may write a result many records after the call and a replay must not
/// depend on how far.
fn load_window(path: &Path) -> Result<Window, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("replay-savings: read a transcript: {e}"))?;
    let mut records: Vec<(String, usize, Value)> = Vec::new();
    let mut last_stamp = String::new();
    for (at, row) in text.lines().enumerate() {
        let Ok(record) = serde_json::from_str::<Value>(row) else {
            continue;
        };
        if let Some(stamp) = record.get("timestamp").and_then(Value::as_str) {
            stamp.clone_into(&mut last_stamp);
        }
        records.push((last_stamp.clone(), at, record));
    }
    let mut results: BTreeMap<String, String> = BTreeMap::new();
    // The client's own object for the same call, off the record the result
    // block arrived on. It is the transcript's spelling of the field the
    // result hook reads, and it is the only place a delivery larger than
    // density can vouch for is ever stated.
    let mut responses: BTreeMap<String, Value> = BTreeMap::new();
    for (_, _, record) in &records {
        for block in blocks(record) {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(id) = block.get("tool_use_id").and_then(Value::as_str) else {
                continue;
            };
            if let Some(response) = record.get("toolUseResult") {
                responses.insert(id.to_owned(), response.clone());
            }
            let Some(content) = block.get("content") else {
                continue;
            };
            results.insert(id.to_owned(), result_text(content));
        }
    }
    let mut events = Vec::new();
    for (stamp, at, record) in &records {
        if is_eviction(record) {
            events.push((stamp.clone(), *at, Event::Evict));
            continue;
        }
        if is_prompt(record) {
            events.push((stamp.clone(), *at, Event::Turn));
            continue;
        }
        let cwd = record.get("cwd").and_then(Value::as_str).map(str::to_owned);
        for block in blocks(record) {
            if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                continue;
            }
            let (Some(name), Some(id)) = (
                block.get("name").and_then(Value::as_str),
                block.get("id").and_then(Value::as_str),
            ) else {
                continue;
            };
            events.push((
                stamp.clone(),
                *at,
                Event::Call(Box::new(Call {
                    name: name.to_owned(),
                    input: block.get("input").cloned().unwrap_or(Value::Null),
                    cwd: cwd.clone(),
                    result: results.get(id).cloned(),
                    response: responses.get(id).cloned(),
                })),
            ));
        }
    }
    Ok(Window { events })
}

// ------------------------------------------------- the client's own statement ---

/// The field a Claude Code `Read` result states its own truncation in, and the
/// fields that say how much of the file it handed over. Spelled here exactly as
/// `bins/rift/src/hooks.rs` spells them, from the same pinned observations of
/// `crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md`.
const TRUNCATED_BY_TOKEN_CAP: &str = "truncatedByTokenCap";
/// The first line handed over, when the result names it.
const DELIVERY_START_LINE: &str = "startLine";
/// How many lines followed it.
const DELIVERY_NUM_LINES: &str = "numLines";
/// How many lines the file has, as the client counts them.
const DELIVERY_TOTAL_LINES: &str = "totalLines";
/// The same truncation fact as a sentence, for a result that carries text.
const TRUNCATION_NOTICE: &str = "[Truncated: PARTIAL view";
/// The clause of that sentence carrying the numbers, spelled `A-B`.
const TRUNCATION_NOTICE_LINES: &str = "showing lines ";
/// The field a command result names the file its output was kept in.
const PERSISTED_OUTPUT_PATH: &str = "persistedOutputPath";

/// What one result states about the delivery it answered.
///
/// **A restatement, not a second opinion.** The rule lives in
/// `bins/rift/src/hooks.rs::stated_delivery`, which is private to the binary
/// and reads a hook payload rather than a transcript record, so a replay
/// cannot call it. Every clause below is the same clause in the same order,
/// against the same field names, and the two are meant to be read side by
/// side: a cut-short reading first, so a result that both sets the flag and
/// carries a contradictory count is read the way that can only cost a saving;
/// a complete reading only where the client says so outright or the run
/// reaches the client's own total; and silence answering `None`, never
/// "complete".
fn stated_delivery(response: &Value) -> Option<StatedDelivery> {
    // A command's output is not cut short in place the way a read is: past a
    // threshold in characters the client writes the whole of it to a file and
    // hands the model the path, so nothing states how far the output got.
    let persisted = response
        .get(PERSISTED_OUTPUT_PATH)
        .and_then(Value::as_str)
        .is_some_and(|path| !path.is_empty())
        || response
            .as_str()
            .is_some_and(|text| text.starts_with(PERSISTED_OUTPUT));
    if persisted {
        return Some(StatedDelivery::Unstated);
    }
    let file = response.get("file");
    let stated =
        |file: &Value, name: &str| -> Option<u32> { u32::try_from(file.get(name)?.as_u64()?).ok() };
    let flag = file
        .and_then(|file| file.get(TRUNCATED_BY_TOKEN_CAP))
        .and_then(Value::as_bool);
    let notice = response
        .as_str()
        .or_else(|| response.get("content")?.as_str())
        .filter(|text| text.contains(TRUNCATION_NOTICE));
    let from_object = file.and_then(|file| {
        let lines = stated(file, DELIVERY_NUM_LINES)?;
        let lo = stated(file, DELIVERY_START_LINE).unwrap_or(1);
        Interval::new(lo, lo.checked_add(lines.checked_sub(1)?)?)
    });
    if flag == Some(true) || notice.is_some() {
        let run = from_object.or_else(|| notice.and_then(notice_lines));
        return Some(run.map_or(StatedDelivery::Unstated, StatedDelivery::Truncated));
    }
    let run = from_object?;
    let reaches_the_end = file
        .and_then(|file| stated(file, DELIVERY_TOTAL_LINES))
        .is_some_and(|total| run.hi >= total);
    (flag == Some(false) || reaches_the_end).then_some(StatedDelivery::Complete(run))
}

/// The run of lines a truncation notice names, from its `showing lines A-B`
/// clause. Anything it cannot read outright is no run at all rather than a
/// guess.
fn notice_lines(notice: &str) -> Option<Interval> {
    let clause = notice.split_once(TRUNCATION_NOTICE_LINES)?.1;
    let digits = |text: &str| text.trim().parse::<u32>().ok();
    let (lo, rest) = clause.split_once('-')?;
    let hi = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    Interval::new(digits(lo)?, digits(hi)?)
}

/// The window files of one session tree: the main session, then each subagent.
fn tree_windows(dir: &Path, stem: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let main = dir.join(format!("{stem}.jsonl"));
    if main.is_file() {
        out.push(main);
    }
    let subagents = dir.join(stem).join("subagents");
    if let Ok(entries) = std::fs::read_dir(&subagents) {
        let mut agents: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "jsonl"))
            .collect();
        agents.sort();
        out.extend(agents);
    }
    out
}

/// Every session tree under a client project directory, by stem, sorted.
fn tree_stems(dir: &Path) -> Result<Vec<String>, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("replay-savings: read {}: {e}", dir.display()))?;
    let mut stems = BTreeSet::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "jsonl") {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                stems.insert(stem.to_owned());
            }
        } else if path.join("subagents").is_dir() {
            if let Some(stem) = path.file_name().and_then(|s| s.to_str()) {
                stems.insert(stem.to_owned());
            }
        }
    }
    Ok(stems.into_iter().collect())
}

// ------------------------------------------------------------------- paths ---

/// Whether a spelling names a root of its own rather than a name under some
/// working directory.
fn is_rooted(path: &str) -> bool {
    if path.starts_with('/') {
        return true;
    }
    let mut chars = path.chars();
    matches!(
        (chars.next(), chars.next(), chars.next()),
        (Some(drive), Some(':'), Some('/')) if drive.is_ascii_alphabetic()
    )
}

/// The one spelling coverage is keyed by, or `None` when the path is outside
/// the working directory the record states.
///
/// Separators are folded to one form and a drive letter's case is folded with
/// them, because a client spells the same file two ways in one session and two
/// spellings of one file must not become two claims about two files. Everything
/// after that is left verbatim: folding a name's case would merge two files on
/// a platform that keeps them apart.
fn path_key(raw: &str, cwd: Option<&str>) -> Option<String> {
    let fold = |s: &str| {
        let mut out = s.replace('\\', "/");
        if is_rooted(&out) && !out.starts_with('/') {
            out.replace_range(0..1, &out[0..1].to_ascii_lowercase());
        }
        out
    };
    let path = fold(raw);
    let Some(cwd) = cwd.map(fold) else {
        return (!is_rooted(&path)).then_some(path);
    };
    let cwd = cwd.trim_end_matches('/').to_owned();
    if !is_rooted(&path) {
        return Some(path);
    }
    let rest = path.strip_prefix(&cwd)?;
    let rest = rest.strip_prefix('/')?;
    (!rest.is_empty()).then(|| rest.to_owned())
}

// -------------------------------------------------------------- file facts ---

/// The bytes of one file, reduced to what a decision needs.
#[derive(Clone, Debug)]
struct FileFacts {
    /// The byte length of each line, the newline included.
    line_bytes: Vec<u32>,
}

impl FileFacts {
    /// How many lines the file has, the way the gate counts them: a terminated
    /// run and an unterminated tail are each one line, and an empty file has
    /// none.
    fn total_lines(&self) -> u32 {
        u32::try_from(self.line_bytes.len()).unwrap_or(u32::MAX)
    }

    /// How many bytes the lines `lo..=hi` amount to.
    fn slice_bytes(&self, run: Interval) -> usize {
        let lo = (run.lo as usize).saturating_sub(1);
        let hi = run.hi as usize;
        self.line_bytes
            .iter()
            .skip(lo)
            .take(hi.saturating_sub(lo))
            .map(|n| *n as usize)
            .sum()
    }
}

/// The worktree's facts about one path, cached for the whole replay.
struct Worktree {
    /// The checkout every relative key is resolved under.
    root: PathBuf,
    /// What each key resolved to, or that it resolved to nothing.
    cache: BTreeMap<String, Option<FileFacts>>,
}

impl Worktree {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            cache: BTreeMap::new(),
        }
    }

    /// The file a key names, or `None` when this checkout has nothing to say
    /// about it — no such file, too large to hash inline, or not text.
    fn facts(&mut self, key: &str) -> Option<&FileFacts> {
        if !self.cache.contains_key(key) {
            self.cache
                .insert(key.to_owned(), read_facts(&self.root, key));
        }
        self.cache.get(key).and_then(Option::as_ref)
    }
}

/// Read one file's facts, applying the same two refusals the gate applies.
fn read_facts(root: &Path, key: &str) -> Option<FileFacts> {
    let path = if is_rooted(key) {
        PathBuf::from(key)
    } else {
        root.join(key)
    };
    let meta = std::fs::metadata(&path).ok()?;
    if !meta.is_file() || meta.len() > MAX_INLINE_HASH_BYTES {
        return None;
    }
    let bytes = std::fs::read(&path).ok()?;
    if bytes.contains(&0) {
        return None;
    }
    Some(FileFacts {
        line_bytes: bytes
            .split_inclusive(|byte| *byte == b'\n')
            .map(|line| u32::try_from(line.len()).unwrap_or(u32::MAX))
            .collect(),
    })
}

/// How many lines a stretch of text spans, counted the way the gate counts a
/// file's.
fn line_span(text: &str) -> u32 {
    u32::try_from(text.split_inclusive('\n').count()).unwrap_or(u32::MAX)
}

/// The synthetic revision hash: a file's name and how many times it has been
/// changed in this session tree.
fn revision_hash(key: &str, revision: u32) -> Sha256Id {
    rift_agent::content_sha256(format!("{key}\u{1}{revision}").as_bytes())
}

// ------------------------------------------------------------------ replay ---

/// What one session tree carries across its windows.
#[derive(Default)]
struct TreeState {
    /// How many times each path has been changed.
    revisions: BTreeMap<String, u32>,
    /// The line count a whole-file write established, where one did.
    written_lines: BTreeMap<String, u32>,
    /// Which windows read each `(path, revision)` first, and what that first
    /// read cost them.
    seen: BTreeMap<(String, u32), Vec<(usize, u64)>>,
}

/// One context window's replay state.
#[derive(Default)]
struct WindowState {
    /// The pure ledger this window's decisions are taken over.
    ledger: Ledger,
    /// The turn the next call is made in.
    turn: u32,
    /// Paths this window changed with something other than a whole-file write
    /// and has not read since.
    awaiting_verification: BTreeSet<String>,
}

/// Replay every transcript under `opts.transcripts`.
///
/// # Errors
/// If the transcripts directory cannot be read.
pub fn replay(opts: &Options) -> Result<Report, String> {
    let adapter = ClaudeCodeAdapter;
    let limits = Limits::default();
    let channel = deny_channel();
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
                    state.ledger.push(Line::Evict {
                        turn: state.turn,
                        reason: EvictReason::Compact,
                        at_ms: 0,
                    });
                }
                Event::Call(call) => {
                    replay_call(
                        &Replay {
                            adapter: &adapter,
                            limits,
                            channel,
                        },
                        &call,
                        index,
                        state,
                        &mut tree,
                        &mut worktree,
                        &mut report,
                    );
                }
            }
        }
        fold_cross_window(&tree, &mut report);
    }
    finish(&mut report);
    Ok(report)
}

/// The settings every call in one replay is decided under, spelled once.
struct Replay<'a> {
    adapter: &'a ClaudeCodeAdapter,
    limits: Limits,
    channel: Option<Channel>,
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

/// Fold one tool call into the report.
fn replay_call(
    replay: &Replay<'_>,
    call: &Call,
    window: usize,
    state: &mut WindowState,
    tree: &mut TreeState,
    worktree: &mut Worktree,
    report: &mut Report,
) {
    report.tool_calls = report.tool_calls.saturating_add(1);
    let native = json!({ "tool_name": call.name, "tool_input": call.input });
    let text = call.result.clone().unwrap_or_default();
    let tokens = u64::from(rift_tokens::count(&text));
    if call.result.is_some() {
        report.all_results.add(tokens);
    }
    // Counted before anything else routes the call away: how much of the
    // shell the pinned grammar sees at all is the first thing a change to
    // that grammar moves, and a recognised read leaves through the read path
    // below rather than through the shell branch at the end.
    if call.name == "Bash" {
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
    // A change first: a read in the same call is not a thing, and the revision
    // a later read is keyed by has to have moved before the read arrives.
    if let Some(edit) = replay.adapter.edit_request("PreToolUse", &native) {
        apply_edit(&edit, call, state, tree, worktree, report);
        return;
    }
    // Every read the call performs, not just its first. One shell command
    // line is routinely several — `cd <dir> && cat a && cat b` is one call and
    // three facts — and a member left out here is a read the window is never
    // credited with, and one the all-or-nothing rule below would never see.
    let requests = replay.adapter.read_requests("PreToolUse", &native);
    if !requests.is_empty() {
        classify_reads(
            replay, call, &requests, &text, tokens, window, state, tree, worktree, report,
        );
        return;
    }
    if replay
        .adapter
        .search_request("PreToolUse", &native)
        .is_some()
    {
        land(report, Bucket::Search, tokens);
        return;
    }
    // A read tool whose arguments step outside the pin is still a read of
    // repository content; it is simply one no decision may be taken about.
    if call.name == "Read" || call.name == "NotebookRead" {
        land(report, Bucket::NotReadShaped, tokens);
        return;
    }
    if call.name == "Bash" {
        // Whatever the grammar recognises as reads left through the read path
        // above, members with no derivable range included — those arrive there
        // as an unpinned span. What is left here is a command line the grammar
        // said nothing about at all.
        let command = call.input.get("command").and_then(Value::as_str);
        let recognised = command.and_then(rift_agent::shell_read::recognise);
        if recognised.is_none() && looks_like_a_shell_read(&call.input) {
            report.invisible_shell.add(tokens);
            if command.is_some_and(|command| names_a_work_tree_file(command, call, worktree)) {
                report.invisible_shell_named_file.add(tokens);
            }
            return;
        }
    }
    let seen = report.other_tools.entry(call.name.clone()).or_default();
    *seen = seen.saturating_add(1);
}

/// Advance a path's revision, and carry coverage forward where — and only
/// where — a whole-file write proves the window holds the new bytes.
///
/// The new file's line count is taken from the payload the call carried, which
/// is the one moment a replay knows a file's shape better than the checkout
/// does. Everything else records the change and no coverage: the guide's edit
/// echo needs the new bytes to find a replacement's site in, and a replay has
/// only the bytes the checkout has today.
fn apply_edit(
    edit: &rift_agent::EditRequest,
    call: &Call,
    state: &mut WindowState,
    tree: &mut TreeState,
    worktree: &mut Worktree,
    report: &mut Report,
) {
    report.edits = report.edits.saturating_add(1);
    let Some(key) = path_key(&edit.path, call.cwd.as_deref()) else {
        return;
    };
    let before = tree
        .written_lines
        .get(&key)
        .copied()
        .or_else(|| worktree.facts(&key).map(FileFacts::total_lines));
    let revision = tree.revisions.entry(key.clone()).or_default();
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
            tree.written_lines.insert(key.clone(), lines);
        }
        None => {
            tree.written_lines.remove(&key);
        }
    }
    let sites: Vec<Interval> = written
        .and_then(|lines| Interval::new(1, lines))
        .into_iter()
        .collect();
    state.ledger.push(Line::Edit {
        turn: state.turn,
        path: key.clone(),
        hash_old: Some(hash_old),
        hash_new,
        sites,
        shift: i64::from(written.unwrap_or_default()) - i64::from(before.unwrap_or_default()),
        full: written.is_some(),
        at_ms: 0,
    });
    if written.is_some() {
        state.awaiting_verification.remove(&key);
    } else {
        state.awaiting_verification.insert(key);
    }
}

/// One read of a call that may perform several: resolved, and decided where
/// the gate reaches a decision at all.
///
/// The shape mirrors the binary's `GateMember`, and for the same reason: a
/// member's decision is reached against the ledger as it stood when the call
/// arrived, and only once every member has one does the call as a whole get
/// to say whether a decline travels.
struct Member {
    /// The file, in the one spelling coverage is keyed by.
    key: String,
    /// The synthetic revision the decision was keyed by.
    revision: u32,
    /// The request resolved against that file.
    read: ResolvedRead,
    /// How many bytes the run of lines it asked for holds.
    slice_bytes: usize,
    /// What the pre-call step credited before any result arrived.
    recorded: Interval,
    /// What was decided, or `None` where the slice was past the ceiling and
    /// the gate reached no decision and wrote nothing.
    decision: Option<Decision>,
    /// The sentence that decision would be answered with.
    frame: Option<String>,
    /// Whether a decline here rests on the window's own whole-file write.
    rests_on_a_write: bool,
    /// The bucket this member puts the call in, were it the first to block a
    /// decline.
    bucket: Bucket,
}

/// One slot of a call, in the order the command line performs it.
enum Slot {
    /// This checkout could say nothing about the member at all.
    Blocked(Bucket),
    /// The member resolved.
    Member(Box<Member>),
}

impl Slot {
    /// The bucket this slot puts the call in when it is the first blocker.
    fn bucket(&self) -> Bucket {
        match self {
            Self::Blocked(bucket) => *bucket,
            Self::Member(member) => member.bucket,
        }
    }

    /// Whether this slot would let the call be declined as a whole.
    fn declines(&self) -> bool {
        matches!(
            self,
            Self::Member(member) if matches!(member.decision, Some(Decision::Deny(_)))
        )
    }
}

/// Take every read one repository-content call performs through the shipped
/// decision, and then through the client's own statement about what it
/// delivered.
///
/// Five phases, the first three of them the binary's own: resolve and decide
/// every member against ONE snapshot of the ledger; settle whether the call as
/// a whole is declined, which it is only when declining every part of it would
/// be right; record what each member decided; reconcile each member that was
/// not declined against what the result states it was handed; and only then
/// choose the one bucket the call is counted in.
#[allow(clippy::too_many_lines)]
fn classify_reads(
    replay: &Replay<'_>,
    call: &Call,
    requests: &[ReadRequest],
    text: &str,
    tokens: u64,
    window: usize,
    state: &mut WindowState,
    tree: &mut TreeState,
    worktree: &mut Worktree,
    report: &mut Report,
) {
    // A call none of whose reads this checkout has a name for is out of scope,
    // the way scope containment takes an out-of-tree read out of every other
    // figure. One member inside the tree makes the call repository content.
    if requests
        .iter()
        .all(|request| path_key(&request.path, call.cwd.as_deref()).is_none())
    {
        report.outside_work_tree.add(tokens);
        return;
    }
    if requests.len() > 1 {
        report.calls_with_several_reads = report.calls_with_several_reads.saturating_add(1);
    }
    report.read_members = report
        .read_members
        .saturating_add(requests.len().try_into().unwrap_or(u64::MAX));
    let mut verified = false;
    for request in requests {
        if let Some(key) = path_key(&request.path, call.cwd.as_deref()) {
            verified |= state.awaiting_verification.remove(&key);
        }
    }
    if verified {
        report.verification_after_edit.add(tokens);
    }
    if call.result.is_none() {
        land(report, Bucket::NoResultRecorded, tokens);
        return;
    }
    // The client answered the call itself, so no bytes arrived and there is
    // nothing to credit a window with. Checked before the ledger is touched:
    // recording a delivery that never happened is the one error a decline
    // cannot survive.
    if CLIENT_DEDUP_MARKERS
        .iter()
        .any(|marker| text.contains(marker))
    {
        land(report, Bucket::ClientDedup, tokens);
        return;
    }

    // Phase one.
    let mut slots: Vec<Slot> = Vec::new();
    for request in requests {
        slots.push(resolve_member(
            replay, call, request, text, state, tree, worktree,
        ));
    }
    // Phase two: the call is one act, so it is declined only when declining
    // every part of it would be right. One member the window does not hold —
    // or one nobody here could resolve at all — and the whole line runs.
    let wholly_declined = !slots.is_empty() && slots.iter().all(Slot::declines);
    // Which member stopped the call being declined, taken before the downgrade
    // below turns the held members into allowed ones — afterwards nothing on
    // the call declines and the question has no answer.
    let blocker = slots.iter().position(|slot| !slot.declines());
    if !wholly_declined {
        for slot in &mut slots {
            if let Slot::Member(member) = slot {
                if matches!(member.decision, Some(Decision::Deny(_))) {
                    member.decision = Some(Decision::Allow);
                    member.frame = None;
                    report.declines_lost_to_a_sibling =
                        report.declines_lost_to_a_sibling.saturating_add(1);
                }
            }
        }
    }
    // Phase three: write each member's decision into the window's ledger.
    for slot in &slots {
        if let Slot::Member(member) = slot {
            if let Some(decision) = &member.decision {
                state.ledger.push(read_ledger::record(
                    &member.read,
                    decision,
                    member.slice_bytes,
                    replay.limits,
                    state.turn,
                    0,
                ));
            }
        }
    }
    // Phase four: the result's own statement, which is the only route by which
    // a delivery too large for the size rule to vouch for ever becomes
    // coverage — and the only one that takes coverage back.
    let stated = call.response.as_ref().and_then(stated_delivery);
    if call.response.is_some() {
        report.results_with_a_response = report.results_with_a_response.saturating_add(1);
    }
    if stated.is_some() {
        report.statements_read = report.statements_read.saturating_add(1);
    }
    if let Some(stated) = stated {
        for slot in &mut slots {
            let Slot::Member(member) = slot else { continue };
            if matches!(member.decision, Some(Decision::Deny(_))) {
                // A decline stands over these lines, so the result is the
                // bytes arriving anyway rather than a statement about a
                // delivery this window is owed coverage for.
                continue;
            }
            match read_ledger::reconcile_delivery(&member.read, member.recorded, stated) {
                Reconcile::Unchanged => {}
                Reconcile::Credit(range) => {
                    // The whole of what this lever is worth, in one line: a
                    // read the ceiling refused to credit now leaves coverage
                    // behind, so the NEXT read of these lines has something to
                    // be decided against.
                    if member.bucket == Bucket::OverCreditBound {
                        member.bucket = Bucket::CreditedFromStatement;
                    }
                    report.deliveries_credited = report.deliveries_credited.saturating_add(1);
                    credit_line(&mut state.ledger, &member.read, range, state.turn);
                }
                Reconcile::TakeBack(kept) => {
                    report.deliveries_taken_back = report.deliveries_taken_back.saturating_add(1);
                    state.ledger.push(Line::Evict {
                        turn: state.turn,
                        reason: EvictReason::Truncate,
                        at_ms: 0,
                    });
                    if let Some(range) = kept {
                        credit_line(&mut state.ledger, &member.read, range, state.turn);
                    }
                }
            }
        }
    }
    // Phase five: the one bucket this call is counted in.
    let spilled = text.contains(PERSISTED_OUTPUT);
    let blocker = blocker.and_then(|at| slots.get(at));
    let bucket = if wholly_declined {
        Bucket::Declined
    } else if spilled {
        Bucket::SpilledToAFile
    } else {
        blocker.map_or(Bucket::NoLineCount, Slot::bucket)
    };
    if bucket == Bucket::Declined {
        report.declined.add(tokens);
        for slot in &slots {
            if let Slot::Member(member) = slot {
                if let Some(frame) = &member.frame {
                    report.frame_tokens = report
                        .frame_tokens
                        .saturating_add(u64::from(rift_tokens::count(frame)));
                }
                if member.rests_on_a_write {
                    report.declines_resting_on_a_write =
                        report.declines_resting_on_a_write.saturating_add(1);
                }
            }
        }
    }
    if bucket.declinable() {
        report.declinable.add(tokens);
    }
    // A first read is a first read whichever ceiling it arrived past: both
    // buckets are reads the window had to be handed, and both are what a
    // sibling window then reads again.
    if matches!(bucket, Bucket::FirstSight | Bucket::CreditedFromStatement) {
        report.first_read_floor.add(tokens);
        if let Some(Slot::Member(member)) = blocker {
            tree.seen
                .entry((member.key.clone(), member.revision))
                .or_default()
                .push((window, tokens));
        }
    }
    land(report, bucket, tokens);
}

/// Append the one line a credit is: a read of exactly the run the statement
/// named.
fn credit_line(ledger: &mut Ledger, read: &ResolvedRead, range: Interval, turn: u32) {
    ledger.push(Line::Read {
        turn,
        path: read.path.clone(),
        hash: read.hash,
        range,
        total_lines: read.total_lines,
        at_ms: 0,
    });
}

/// Resolve one member of a call and decide about it, writing nothing.
fn resolve_member(
    replay: &Replay<'_>,
    call: &Call,
    request: &ReadRequest,
    text: &str,
    state: &WindowState,
    tree: &TreeState,
    worktree: &mut Worktree,
) -> Slot {
    let Some(key) = path_key(&request.path, call.cwd.as_deref()) else {
        return Slot::Blocked(Bucket::NoLineCount);
    };
    let revision = tree.revisions.get(&key).copied().unwrap_or_default();
    let hash = revision_hash(&key, revision);
    let facts = worktree.facts(&key).cloned();
    let total_lines = tree
        .written_lines
        .get(&key)
        .copied()
        .or_else(|| facts.as_ref().map(FileFacts::total_lines));
    let Some(total_lines) = total_lines.filter(|lines| *lines > 0) else {
        return Slot::Blocked(Bucket::NoLineCount);
    };
    let Some(read) = ResolvedRead::new(
        &ReadRequest {
            path: key.clone(),
            ..request.clone()
        },
        hash,
        total_lines,
    ) else {
        // A span no artifact pins, or a start past the end of the file.
        return Slot::Blocked(Bucket::UnknownSpan);
    };
    // The larger of the two readings of the slice: the checkout's own bytes
    // for those lines, and what the result actually returned.
    let slice_bytes = facts
        .as_ref()
        .map_or(0, |facts| facts.slice_bytes(read.range))
        .max(text.len());
    let recorded = read.range_delivered(slice_bytes, replay.limits);
    let member = |decision: Option<Decision>, frame, rests_on_a_write, bucket| {
        Slot::Member(Box::new(Member {
            key: key.clone(),
            revision,
            read: read.clone(),
            slice_bytes,
            recorded,
            decision,
            frame,
            rests_on_a_write,
            bucket,
        }))
    };
    // Past the size a delivery can be vouched for: the gate reaches no
    // decision and writes nothing, and the member's only route to coverage is
    // the client's own statement, one hook later.
    if slice_bytes > replay.limits.max_slice_bytes
        || !read.delivery_is_complete(slice_bytes, replay.limits)
    {
        return member(None, None, false, Bucket::OverCreditBound);
    }
    let read_coverage = state.ledger.read_coverage(&key, hash);
    let held_before = state.ledger.coverage(&key, hash).contains(read.range);
    let read_before = !read_coverage.is_empty();
    let already_declined = state.ledger.denied(&read.deny_key());
    let rests_on_a_write = state.ledger.wrote_in_full(&key, hash).is_some();
    let decision = read_ledger::decide(
        &read,
        &state.ledger,
        Mode::Enforce,
        replay.channel,
        EditEcho::Pinned,
        slice_bytes,
        replay.limits,
    );
    // The same fallback the shipped gate takes: a decline whose frame has
    // nothing exact left to say is not a decline.
    let frame = frame_for(&state.ledger, &read, &decision, replay.limits.context_lines);
    let decision = match (&decision, &frame) {
        (Decision::Deny(_), None) => Decision::Allow,
        _ => decision,
    };
    let bucket = bucket_of(
        &decision,
        held_before,
        read_before,
        already_declined,
        &read_coverage,
        read.range,
        replay.limits.min_slice_lines,
    );
    member(Some(decision), frame, rests_on_a_write, bucket)
}

/// Which bucket a decided call belongs in.
fn bucket_of(
    decision: &Decision,
    held_before: bool,
    read_before: bool,
    already_declined: bool,
    coverage: &read_ledger::IntervalSet,
    range: Interval,
    floor: u32,
) -> Bucket {
    match decision {
        Decision::Deny(_) => Bucket::Declined,
        Decision::Observe(_) => {
            if range.lines() < floor {
                Bucket::BelowFloor
            } else {
                Bucket::ObservedNotDeclined
            }
        }
        Decision::Allow => {
            if held_before && already_declined {
                Bucket::RepeatAfterDecline
            } else if held_before {
                Bucket::NoHonestFrame
            } else if !read_before {
                Bucket::FirstSight
            } else if coverage.runs().iter().any(|run| run.intersects(range)) {
                Bucket::RepeatOverlapping
            } else {
                Bucket::RepeatDisjoint
            }
        }
    }
}

/// The sentence a decline would carry, through the one sealed renderer.
fn frame_for(
    held: &Ledger,
    read: &ResolvedRead,
    decision: &Decision,
    context_lines: u32,
) -> Option<String> {
    let Decision::Deny(shape) = decision else {
        return None;
    };
    let hash = read.hash.to_hex();
    let since = held_since(held, read);
    match shape {
        DenyShape::Plain => Some(rift_recall_validity::trust::render_deny_frame(
            &read.path,
            &hash,
            read.range.lo,
            read.range.hi,
            since,
        )),
        DenyShape::Delta { sites, shift, .. } => {
            let sites: Vec<(u32, u32)> = sites.iter().map(|site| (site.lo, site.hi)).collect();
            rift_recall_validity::trust::render_delta_frame(
                &read.path,
                &hash,
                &sites,
                *shift,
                read.total_lines,
                context_lines,
                since,
            )
        }
    }
}

/// The turn this window was last handed — or last authored — these bytes.
fn held_since(held: &Ledger, read: &ResolvedRead) -> Option<u64> {
    held.live()
        .iter()
        .rev()
        .find_map(|line| match line {
            Line::Read {
                turn, path, hash, ..
            } if path == &read.path && *hash == read.hash => Some(*turn),
            Line::Edit {
                turn,
                path,
                hash_new,
                ..
            } if path == &read.path && *hash_new == read.hash => Some(*turn),
            _ => None,
        })
        .map(u64::from)
}

/// Fold one call into its bucket.
fn land(report: &mut Report, bucket: Bucket, tokens: u64) {
    report.buckets.entry(bucket.key()).or_default().add(tokens);
}

/// Whether a shell command line has a read word where a command goes.
///
/// A heuristic, and the only one in this file: it decides nothing and only
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

/// The totals every other figure is a part of.
fn finish(report: &mut Report) {
    let mut reads = Tally::default();
    let mut searches = Tally::default();
    for bucket in Bucket::ALL {
        let tally = report.bucket(bucket);
        if bucket == Bucket::Search {
            searches.merge(tally);
        } else {
            reads.merge(tally);
        }
    }
    report.reads = reads;
    report.searches = searches;
    let mut total = reads;
    total.merge(searches);
    report.repo_content = total;
}

// ------------------------------------------------------------------ output ---

/// A number with thousands separators, so a seven-figure token count can be
/// read at a glance.
fn commas(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (at, ch) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// One line of the table: a label, a call count, a token count and a share.
fn row(label: &str, tally: Tally, denominator: u64) -> String {
    format!(
        "  {label:<48}{:>9}{:>15}{:>11.2} %\n",
        commas(tally.calls),
        commas(tally.tokens),
        share_percent(tally.tokens, denominator)
    )
}

/// The human table.
///
/// Aggregates only: no path, no prompt, no command line and no result text
/// reaches this string.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn render_table(report: &Report) -> String {
    let denominator = report.repo_content.tokens;
    let mut out = String::new();
    out.push_str("replay-savings — what this build's read gate would have decided\n");
    out.push_str(&format!(
        "  schema {SCHEMA_VERSION} · tokenizer {} · aggregates only\n\n",
        report.tokenizer
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "session trees",
        commas(report.trees)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "context windows",
        commas(report.windows)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "tool calls replayed",
        commas(report.tool_calls)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "every tool result (informational ceiling)",
        commas(report.all_results.calls),
        commas(report.all_results.tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "revisions advanced / windows emptied",
        commas(report.edits),
        commas(report.evictions)
    ));

    out.push_str("\n  repository-content calls, one bucket each");
    out.push_str(&format!(
        "\n  {:<48}{:>9}{:>15}{:>13}\n",
        "", "calls", "tokens", "of content"
    ));
    for bucket in Bucket::ALL {
        let tally = report.bucket(bucket);
        if tally.calls == 0 {
            continue;
        }
        out.push_str(&row(bucket.label(), tally, denominator));
    }
    out.push_str(&row("read channels", report.reads, denominator));
    out.push_str(&row("search channel", report.searches, denominator));
    out.push_str(&row(
        "repository content (the denominator)",
        report.repo_content,
        denominator,
    ));

    out.push_str("\n  what the share is\n");
    out.push_str(&row(
        "declinable by the ledger",
        report.declinable,
        denominator,
    ));
    out.push_str(&row("declined by this build", report.declined, denominator));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}{:>11.2} %\n",
        "declined, net of the reason frames",
        commas(report.declined.calls),
        commas(report.declined_net_tokens()),
        share_percent(report.declined_net_tokens(), denominator)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "reason frames cost",
        "",
        commas(report.frame_tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "declines resting on the window's own write",
        commas(report.declines_resting_on_a_write)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "declines lost to an unheld sibling read",
        commas(report.declines_lost_to_a_sibling)
    ));

    out.push_str("\n  the two levers a replay has to model on purpose\n");
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "results with a response object / stating a delivery",
        commas(report.results_with_a_response),
        commas(report.statements_read)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "reads credited from the client's statement",
        commas(report.deliveries_credited),
        commas(report.bucket(Bucket::CreditedFromStatement).tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "deliveries the statement took back",
        commas(report.deliveries_taken_back)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "calls performing several reads / members",
        commas(report.calls_with_several_reads),
        commas(report.read_members)
    ));

    out.push_str("\n  what is left, and why\n");
    out.push_str(&row(
        "first-read floor (never declinable)",
        report.first_read_floor,
        denominator,
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}{:>11.2} %\n",
        "  of it, read again by a sibling window",
        commas(report.cross_window_pairs),
        commas(report.cross_window.tokens),
        share_percent(report.cross_window.tokens, denominator)
    ));
    out.push_str(&row(
        "reads after a non-write edit (not modelled)",
        report.verification_after_edit,
        denominator,
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "invisible shell reads, loose bound",
        commas(report.invisible_shell.calls),
        commas(report.invisible_shell.tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "  of them, naming a file this checkout has",
        commas(report.invisible_shell_named_file.calls),
        commas(report.invisible_shell_named_file.tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "shell calls / of them recognised by the grammar",
        commas(report.shell_calls),
        commas(report.shell_recognised)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "reads outside the work tree (out of scope)",
        commas(report.outside_work_tree.calls),
        commas(report.outside_work_tree.tokens)
    ));
    out.push_str(&format!(
        "  {:<48}{:>9}{:>15}\n",
        "unmeasured remainder (every other tool)",
        "",
        commas(report.remainder_tokens())
    ));

    out.push_str("\n  the constants this build decided with\n");
    for (name, value) in constants() {
        out.push_str(&format!("  {name:<48}{value:>9}\n"));
    }
    out.push_str(&format!(
        "  {:<48}{:>9}\n",
        "other tool names seen",
        commas(report.other_tools.len() as u64)
    ));
    out.push_str(
        "\n  Reading it: the declined share is an estimated upper bound on what this\n  \
         build would have kept out of context, never a claim that the reads would\n  \
         have happened. Searches are in the denominator and the ledger never\n  \
         declines one, so the read-only share is the larger of the two rates.\n",
    );
    out
}

/// The numbers the decision was taken with, as a list a reader can check.
fn constants() -> Vec<(&'static str, String)> {
    let limits = Limits::default();
    vec![
        (
            "max slice bytes (decline ceiling)",
            limits.max_slice_bytes.to_string(),
        ),
        (
            "min slice lines (decline floor)",
            limits.min_slice_lines.to_string(),
        ),
        (
            "max delivered bytes, read tool",
            limits.max_delivered_bytes.to_string(),
        ),
        (
            "max delivered bytes, terminal",
            limits.max_shell_delivered_bytes.to_string(),
        ),
        (
            "context lines around a changed site",
            limits.context_lines.to_string(),
        ),
        ("max inline hash bytes", MAX_INLINE_HASH_BYTES.to_string()),
        ("mode", "enforce".to_owned()),
        ("edit echo", "pinned".to_owned()),
    ]
}

/// The machine-readable document: sorted keys, versioned, no free text from
/// the corpus anywhere in it.
#[must_use]
pub fn render_json(report: &Report) -> String {
    let denominator = report.repo_content.tokens;
    let tally = |t: Tally| json!({ "calls": t.calls, "tokens": t.tokens });
    let mut buckets = Map::new();
    for bucket in Bucket::ALL {
        let t = report.bucket(bucket);
        buckets.insert(
            bucket.key().to_owned(),
            json!({
                "calls": t.calls,
                "declinable": bucket.declinable(),
                "share_percent_of_repository_content": share_percent(t.tokens, denominator),
                "tokens": t.tokens,
            }),
        );
    }
    let mut constants_map = Map::new();
    for (name, value) in constants() {
        constants_map.insert(
            name.replace(' ', "_").replace([',', '(', ')'], ""),
            json!(value),
        );
    }
    let mut others = Map::new();
    for (name, count) in &report.other_tools {
        others.insert(name.clone(), json!(count));
    }
    let document = json!({
        "buckets": Value::Object(buckets),
        "constants": Value::Object(constants_map),
        "corpus": {
            "context_windows": report.windows,
            "revisions_advanced": report.edits,
            "calls_performing_several_reads": report.calls_with_several_reads,
            "context_windows_read_members": report.read_members,
            "session_trees": report.trees,
            "shell_calls": report.shell_calls,
            "shell_calls_the_grammar_recognised": report.shell_recognised,
            "tool_calls_replayed": report.tool_calls,
            "windows_emptied": report.evictions,
        },
        "delivery_statement": {
            "credited": report.deliveries_credited,
            "credited_tokens": report.bucket(Bucket::CreditedFromStatement).tokens,
            "results_with_a_response_object": report.results_with_a_response,
            "results_stating_a_delivery": report.statements_read,
            "taken_back": report.deliveries_taken_back,
        },
        "cross_window_repeats": {
            "pairs": report.cross_window_pairs,
            "tokens": report.cross_window.tokens,
        },
        "declined": {
            "calls": report.declined.calls,
            "frame_tokens": report.frame_tokens,
            "net_tokens": report.declined_net_tokens(),
            "lost_to_an_unheld_sibling": report.declines_lost_to_a_sibling,
            "resting_on_a_write": report.declines_resting_on_a_write,
            "tokens": report.declined.tokens,
        },
        "declinable": tally(report.declinable),
        "first_read_floor": tally(report.first_read_floor),
        "invisible_shell_reads": {
            "loose_bound": tally(report.invisible_shell),
            "naming_a_work_tree_file": tally(report.invisible_shell_named_file),
        },
        "other_tools": Value::Object(others),
        "outside_the_work_tree": tally(report.outside_work_tree),
        "reads_after_a_non_write_edit": tally(report.verification_after_edit),
        "repository_content": tally(report.repo_content),
        "repository_content_reads": tally(report.reads),
        "repository_content_searches": tally(report.searches),
        "schema_version": SCHEMA_VERSION,
        "share_estimated_upper_bound_percent": {
            "declinable_of_repository_content": share_percent(
                report.declinable.tokens, denominator),
            "declined_net_of_frames_of_reads": share_percent(
                report.declined_net_tokens(), report.reads.tokens),
            "declined_net_of_frames_of_repository_content": share_percent(
                report.declined_net_tokens(), denominator),
            "declined_of_repository_content": share_percent(
                report.declined.tokens, denominator),
        },
        "tokenizer": report.tokenizer,
        "tool_results_all": tally(report.all_results),
        "unmeasured_remainder_tokens": report.remainder_tokens(),
    });
    serde_json::to_string_pretty(&document).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The synthetic corpus, and the checkout it is replayed against.
    fn fixture(tmp: &Path) -> Options {
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
        }
    }

    /// The sentinel the committed fixture's prompts carry.
    const SENTINEL: &str = "REPLAYSENTINELdoNOTprintTHIS";

    #[test]
    fn the_fixture_pins_every_classification_bucket() {
        let tmp = tempfile::tempdir().unwrap();
        let report = replay(&fixture(tmp.path())).unwrap();
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
        assert_eq!(report.invisible_shell.calls, 2, "two unrecognised reads");
        assert_eq!(
            report.invisible_shell_named_file.calls, 1,
            "one of them names a file this checkout has"
        );
        assert_eq!(report.shell_calls, 8, "every shell call");
        assert_eq!(report.shell_recognised, 6, "what the grammar recognised");
        assert_eq!(report.outside_work_tree.calls, 1, "out of scope");
        assert_eq!(report.evictions, 1, "one compaction");
        assert_eq!(report.edits, 3, "a write, a replacement and a multi-edit");
        assert_eq!(report.tool_calls, 37, "every tool call replayed");
        assert_eq!(report.all_results.calls, 36, "one call returned nothing");
        // The two levers a call is more than one decision under.
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

    /// The client's own statement, read the way `hooks.rs::stated_delivery`
    /// reads it: silence is not "complete", the cut-short reading wins a
    /// contradiction, and a run that stops short of the file with no flag
    /// beside it says nothing at all.
    #[test]
    fn a_delivery_statement_is_read_the_way_the_gate_reads_it() {
        let file = |body: Value| json!({ "type": "text", "file": body });
        assert_eq!(
            stated_delivery(&file(
                json!({"startLine": 1, "numLines": 574, "totalLines": 574})
            )),
            Some(StatedDelivery::Complete(Interval::new(1, 574).unwrap())),
            "a run reaching the client's own total is a complete delivery"
        );
        assert_eq!(
            stated_delivery(&file(
                json!({"startLine": 1, "numLines": 230, "totalLines": 275})
            )),
            None,
            "short of the total with no flag says nothing"
        );
        assert_eq!(
            stated_delivery(&file(json!({
                "startLine": 1, "numLines": 40, "totalLines": 120, "truncatedByTokenCap": true
            }))),
            Some(StatedDelivery::Truncated(Interval::new(1, 40).unwrap())),
            "the flag names how far it got"
        );
        assert_eq!(
            stated_delivery(&file(
                json!({"startLine": 1, "numLines": 120, "totalLines": 120,
                       "truncatedByTokenCap": true})
            )),
            Some(StatedDelivery::Truncated(Interval::new(1, 120).unwrap())),
            "a contradiction is read the way that can only cost a saving"
        );
        assert_eq!(
            stated_delivery(&json!({"truncatedByTokenCap": true})),
            None,
            "a statement with no file object is no statement"
        );
        assert_eq!(
            stated_delivery(&json!({"persistedOutputPath": "/tmp/out.txt"})),
            Some(StatedDelivery::Unstated),
            "output kept in a file states nothing about lines"
        );
        assert_eq!(
            stated_delivery(&Value::String(format!("{PERSISTED_OUTPUT}\nPreview:"))),
            Some(StatedDelivery::Unstated),
            "the same fact as the envelope the model reads"
        );
        assert_eq!(
            stated_delivery(&Value::String(
                "[Truncated: PARTIAL view showing lines 5-44 of 900]".to_owned()
            )),
            Some(StatedDelivery::Truncated(Interval::new(5, 44).unwrap())),
            "the sentence carries the numbers where the object does not"
        );
        assert_eq!(
            notice_lines("showing lines 12-"),
            None,
            "half a clause is none"
        );
    }

    #[test]
    fn the_share_is_the_declined_tokens_over_the_repository_content() {
        let tmp = tempfile::tempdir().unwrap();
        let report = replay(&fixture(tmp.path())).unwrap();
        // Every bucket is part of the denominator and nothing else is.
        let summed: u64 = Bucket::ALL.iter().map(|b| report.bucket(*b).tokens).sum();
        assert_eq!(summed, report.repo_content.tokens, "buckets add up");
        assert_eq!(
            report.reads.tokens + report.searches.tokens,
            report.repo_content.tokens,
            "the two channels are the whole denominator"
        );
        assert!(report.declined.tokens > 0, "something was declined");
        assert!(report.frame_tokens > 0, "the frames cost something");
        assert!(
            report.declined.tokens > report.frame_tokens,
            "the declines pay for their own frames: {} vs {}",
            report.declined.tokens,
            report.frame_tokens
        );
        assert_eq!(
            report.declined_net_tokens(),
            report.declined.tokens - report.frame_tokens,
            "net is gross less the frames"
        );
        assert!(
            report.declinable.tokens > report.declined.tokens,
            "the floor keeps something out of the declined pool"
        );
        let share = share_percent(report.declined.tokens, report.repo_content.tokens);
        assert!(share > 0.0 && share < 100.0, "a real share: {share}");
        assert_eq!(share_percent(1, 0), 0.0, "zero over zero is zero");
    }

    #[test]
    fn no_prompt_text_reaches_the_output() {
        let tmp = tempfile::tempdir().unwrap();
        let options = fixture(tmp.path());
        let corpus = std::fs::read_to_string(options.transcripts.join("sess-0001.jsonl")).unwrap();
        assert!(
            corpus.contains(SENTINEL),
            "the fixture carries the sentinel"
        );
        let report = replay(&options).unwrap();
        let table = render_table(&report);
        let document = render_json(&report);
        for rendered in [&table, &document] {
            assert!(!rendered.contains(SENTINEL), "a prompt reached the output");
            assert!(!rendered.contains("lib.rs"), "a path reached the output");
            assert!(!rendered.contains("notes.md"), "a path reached the output");
            assert!(
                !rendered.contains("/repo"),
                "a directory reached the output"
            );
        }
    }

    #[test]
    fn the_document_is_stable_and_versioned() {
        let tmp = tempfile::tempdir().unwrap();
        let options = fixture(tmp.path());
        let first = render_json(&replay(&options).unwrap());
        let second = render_json(&replay(&options).unwrap());
        assert_eq!(first, second, "two replays of one corpus agree");
        let parsed: Value = serde_json::from_str(&first).unwrap();
        assert_eq!(parsed["schema_version"], json!(SCHEMA_VERSION));
        // serde_json's map is ordered, so the document's keys are sorted as
        // written: assert the property a script would depend on.
        let keys: Vec<&String> = parsed.as_object().unwrap().keys().collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "top-level keys are sorted");
    }

    #[test]
    fn a_path_outside_the_working_directory_has_no_key() {
        assert_eq!(
            path_key("C:\\work\\repo\\src\\lib.rs", Some("c:\\work\\repo")),
            Some("src/lib.rs".to_owned())
        );
        assert_eq!(
            path_key("src/lib.rs", Some("/repo")),
            Some("src/lib.rs".to_owned())
        );
        assert_eq!(path_key("/elsewhere/x.txt", Some("/repo")), None);
        assert_eq!(path_key("/repo", Some("/repo")), None);
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

    #[test]
    fn arguments_are_parsed_and_checked() {
        let root = Path::new("/root");
        let args: Vec<String> = ["--transcripts", "/t", "--json"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
        let opts = parse_args(&args, root).unwrap();
        assert!(opts.json);
        assert_eq!(opts.repo, root);
        assert!(parse_args(&[], root).is_err(), "transcripts is required");
        assert!(
            parse_args(&["--nope".to_owned()], root).is_err(),
            "unknown argument"
        );
        assert!(
            parse_args(&["--repo".to_owned()], root).is_err(),
            "a flag with no value"
        );
    }

    #[test]
    fn a_number_is_readable_and_a_tally_saturates() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1_234_567), "1,234,567");
        let mut tally = Tally {
            calls: u64::MAX,
            tokens: u64::MAX,
        };
        tally.add(5);
        assert_eq!(tally.calls, u64::MAX);
    }
}
