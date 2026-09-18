//! The read gate: the silence precondition a read must pass, the journal step
//! that decides it, the audit row and journal entry a decision leaves, and the
//! frame a denied read is answered with. Separate because this is the
//! decision path the intercept gate walks: nothing reachable from here opens a
//! store or loads the tokenizer.

use std::path::{Path, PathBuf};

use rift_shadow::hooks::AdapterRegistry;

use super::doc_gate::{doc_gate, DocAnswer};
use super::ingest::edit_record;

/// How far interception may go here, for this family, as the repository's own
/// policy file says.
///
/// Read from the file and resolved by the policy crate's pure `resolve`,
/// because that is the one place a lever's mode is decided: the repository's
/// tables, the per-family override, the pinned capability and the measured
/// caveats are all folded there, and a second reading of any of them here
/// would be a second answer. A file is all this costs — the gate path opens no
/// store, and an unreadable policy resolves every lever to `off`.
fn gate_policy(
    work_tree: &Path,
    agent: &rift_core::AgentId,
) -> (
    rift_recall_policy::ServePolicy,
    rift_recall_policy::Effective,
) {
    let policy = rift_recall_policy::ServePolicy::load(&work_tree.join(".rift"));
    let effective = rift_recall_policy::resolve(
        &policy,
        agent.as_str(),
        crate::recall::family_capability(agent),
    );
    (policy, effective)
}

/// The window journal's mode, as the pure decision names it.
const fn gate_mode(mode: rift_recall_policy::Mode) -> rift_agent::window_journal::Mode {
    use rift_agent::window_journal::Mode as Journal;
    match mode {
        rift_recall_policy::Mode::Off => Journal::Off,
        rift_recall_policy::Mode::Observe => Journal::Observe,
        rift_recall_policy::Mode::Enforce => Journal::Enforce,
    }
}

/// The field a Claude Code `Read` result states its own truncation in, and the
/// two fields that say how much of the file it handed over.
///
/// Pinned by observation O5 of
/// `crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md`:
/// four recorded results across two clients, every one of them carrying the
/// boolean, and only the older client's *content block* carrying the sentence
/// as well. A reader that looked for the sentence alone would have called the
/// newer client's truncated reads complete.
const TRUNCATED_BY_TOKEN_CAP: &str = "truncatedByTokenCap";

/// The first line handed over, when the result names it. Absent, the read's
/// own first line is what the delivery started at.
const DELIVERY_START_LINE: &str = "startLine";

/// How many lines followed [`DELIVERY_START_LINE`].
const DELIVERY_NUM_LINES: &str = "numLines";

/// The same fact as a sentence, for a result that reaches a hook as text
/// rather than as the object O5 pins. Quoted from the same observation, up to
/// the point where the wording differs between clients.
const TRUNCATION_NOTICE: &str = "[Truncated: PARTIAL view";

/// The clause of that sentence carrying the numbers, spelled `A-B`.
const TRUNCATION_NOTICE_LINES: &str = "showing lines ";

/// The field a command result names the file its output was kept in, when the
/// client kept it in one rather than handing it over.
///
/// Pinned by observation O6 of the same fixture: thirty recorded results, every
/// one of them a command result, none of them smaller than the threshold the
/// published table names, and no command result past that threshold without
/// it. The model gets a pointer and a two-kilobyte preview, so whatever the
/// command line asked for, the lines are not what arrived.
const PERSISTED_OUTPUT_PATH: &str = "persistedOutputPath";

/// The same fact as the envelope the model reads, for a result that reaches a
/// hook as text rather than as the object O6 pins. Quoted from the same
/// observation; no recorded result carries it in `tool_response`, and it is
/// read only at the front of a text result, so a command that prints the word
/// cannot be mistaken for the client having said it.
const PERSISTED_OUTPUT_MARKER: &str = "<persisted-output>";

/// How many lines the file has, as the client counts them.
///
/// Pinned by O7 of the same fixture, which records both what it settles and
/// what it costs: a result whose `numLines` reaches this number delivered the
/// file, and the number is one larger than Rift's own count wherever the bytes
/// end in a newline. The second half is why it is only ever read *with* the
/// delivered run and never instead of it.
const DELIVERY_TOTAL_LINES: &str = "totalLines";

/// What this result states about the delivery it answered, in the one shape a
/// recorded run has pinned.
///
/// One family only, because one family's result shape is pinned. Antigravity
/// and Codex publish no such statement that any recorded run has shown, and a
/// missing statement is never read as a complete delivery: it is silence, and
/// silence leaves the recording rule that bounds what a slice may be credited
/// with (`max_delivered_bytes`) as the only guard.
///
/// **Three statements, from two fields and a sentence.**
///
/// *Cut short* is the flag set, or — for a result that reaches a hook as text
/// rather than as the object O5 pins — the notice in the text. It names the
/// run the tool got to, and where nothing names one the delivery is unstated.
/// This is the backstop the size rule cannot see past: text denser than the
/// density the rule is derived at, and a command whose output the client kept
/// in a file when the slice it was asked for looked small enough to survive.
///
/// *Answered in full* is the flag set to `false`, or — the shape O7 records,
/// where an untruncated result carries no flag at all — a run that reaches the
/// client's own `totalLines`. Either way it is the client's own word about its
/// own delivery, and it is the only evidence there is for a slice too large
/// for density to vouch for. A run that stops short of `totalLines` with no
/// flag beside it says nothing: it is neither, and this returns `None` rather
/// than reading an absence as a fact.
fn stated_delivery(
    agent: &rift_core::AgentId,
    payload: &serde_json::Value,
) -> Option<rift_agent::window_journal::StatedDelivery> {
    use rift_agent::window_journal::StatedDelivery;
    if *agent != rift_core::AgentId::CLAUDE_CODE {
        return None;
    }
    let response = payload.get("tool_response")?;
    // A command's output is not cut short in place the way a read is: past a
    // threshold in characters the client writes the whole of it to a file and
    // hands the model the path. Nothing then says how far the output got,
    // because none of it arrived as output — so the delivery is unstated and
    // the read keeps the line it started on.
    let persisted = response
        .get(PERSISTED_OUTPUT_PATH)
        .and_then(serde_json::Value::as_str)
        .is_some_and(|path| !path.is_empty())
        || response
            .as_str()
            .is_some_and(|text| text.starts_with(PERSISTED_OUTPUT_MARKER));
    if persisted {
        return Some(StatedDelivery::Unstated);
    }
    let file = response.get("file");
    let stated = |file: &serde_json::Value, name: &str| -> Option<u32> {
        u32::try_from(file.get(name)?.as_u64()?).ok()
    };
    let flag = file
        .and_then(|file| file.get(TRUNCATED_BY_TOKEN_CAP))
        .and_then(serde_json::Value::as_bool);
    let notice = response
        .as_str()
        .or_else(|| response.get("content")?.as_str())
        .filter(|text| text.contains(TRUNCATION_NOTICE));
    let from_object = file.and_then(|file| {
        let lines = stated(file, DELIVERY_NUM_LINES)?;
        let lo = stated(file, DELIVERY_START_LINE).unwrap_or(1);
        rift_agent::window_journal::Interval::new(lo, lo.checked_add(lines.checked_sub(1)?)?)
    });
    // The cut-short reading comes first, so a result that both sets the flag
    // and carries a contradictory count is read the way that can only cost a
    // saving.
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

/// The run of lines a truncation notice names, from the `showing lines A-B`
/// clause of the sentence O5 quotes.
///
/// Anything it cannot read outright is no run at all rather than a guess: the
/// caller's answer to that is the read's own first line, which is the smallest
/// claim there is.
fn notice_lines(notice: &str) -> Option<rift_agent::window_journal::Interval> {
    let clause = notice.split_once(TRUNCATION_NOTICE_LINES)?.1;
    let digits = |text: &str| text.trim().parse::<u32>().ok();
    let (lo, rest) = clause.split_once('-')?;
    let hi = rest.split(|c: char| !c.is_ascii_digit()).next()?;
    rift_agent::window_journal::Interval::new(digits(lo)?, digits(hi)?)
}

/// Move this window's coverage to what a result says it was handed — in
/// whichever direction the statement points.
///
/// Both directions are the same rule, decided by
/// [`rift_agent::window_journal::reconcile_delivery`], and only the writing is
/// here:
///
/// - **credit**, where the result states more than the journal was able to
///   claim. Coverage is a union of `read` intervals, so the addition is one
///   append and costs the window nothing it already had. This is the only
///   route by which a delivery too large for the size rule to vouch for ever
///   becomes coverage, and it arrives one hook after the call;
/// - **take back**, where the result states less. The correction is an
///   eviction and a re-record, because a union has no subtraction in it: the
///   window loses the savings its other reads had earned and is credited
///   again, immediately, with exactly what this result states arrived. It is
///   the coarse instrument on purpose — the alternative, rewriting the
///   window's live lines with one of them clipped, trades a rare and cheap
///   loss of savings for a rewriting path whose own failure mode is a false
///   claim of coverage, which is the expensive direction.
///
/// Nothing is written where the record and the statement already agree, so the
/// ordinary read — delivered whole, recorded whole before it ever ran — costs a
/// lookup and no line at all.
///
/// Returns whether it wrote anything.
fn reconcile_delivered_lines(
    journal: &crate::window_journal::WindowJournal,
    read: &rift_agent::window_journal::ResolvedRead,
    recorded: rift_agent::window_journal::Interval,
    delivered: rift_agent::window_journal::StatedDelivery,
    turn: u32,
    at_ms: u64,
) -> bool {
    use rift_agent::window_journal::{reconcile_delivery, EvictReason, Line, Reconcile};
    let credit = |range| {
        let _ = journal.append(&Line::Read {
            turn,
            path: read.path.clone(),
            hash: read.hash,
            range,
            total_lines: read.total_lines,
            at_ms,
        });
    };
    match reconcile_delivery(read, recorded, delivered) {
        Reconcile::Unchanged => false,
        Reconcile::Credit(range) => {
            credit(range);
            true
        }
        Reconcile::TakeBack(kept) => {
            let _ = journal.append(&Line::Evict {
                turn,
                reason: EvictReason::Truncate,
                at_ms,
            });
            if let Some(range) = kept {
                credit(range);
            }
            true
        }
    }
}

/// Whether a decline may rest on the window's own edit, for this family, from
/// the same resolved policy.
///
/// Kept apart from the mode because it is a different kind of fact: the mode is
/// what this repository asked for, this is what the family's own tooling is
/// pinned to do. A family whose edit result is not pinned to tell the model it
/// holds what it wrote is never declined on an edit line however this
/// repository is configured.
const fn gate_edit_echo(
    effective: &rift_recall_policy::Effective,
) -> rift_agent::window_journal::EditEcho {
    if effective.pinned_edit_echo {
        rift_agent::window_journal::EditEcho::Pinned
    } else {
        rift_agent::window_journal::EditEcho::Unpinned
    }
}

/// The bounds one decision reads, from the same resolved policy.
fn gate_limits(effective: &rift_recall_policy::Effective) -> rift_agent::window_journal::Limits {
    rift_agent::window_journal::Limits {
        max_slice_bytes: usize::try_from(effective.max_slice_bytes)
            .unwrap_or(rift_agent::DEFAULT_MAX_SLICE_BYTES),
        min_slice_lines: effective.min_slice_lines,
        ..rift_agent::window_journal::Limits::default()
    }
}

/// What this hook does about the window journal, if anything.
pub(crate) enum GateStep {
    /// A tool call that may be read-shaped: resolve, decide, record.
    Decide,
    /// A tool call's *result*, which a call this window declined does not
    /// have: one arriving says the client ran the call regardless.
    CrossCheck,
    /// The context window stopped holding what it was given.
    Evict(rift_agent::window_journal::EvictReason),
}

/// Which step a `(family, verb)` is, from the pinned surfaces alone.
///
/// The eviction verbs are each family's own documented announcement that its
/// context has stopped holding what it was handed: Claude Code says so before
/// it summarises a conversation, and names the reason a session started;
/// Codex says so on both sides of its summary. Antigravity publishes no such
/// signal at all, which is the fact that keeps it in observe — coverage that
/// outlives the context it describes is exactly the state a decline must
/// never rest on.
///
/// A failed tool call is not a result: the family publishes its own verb for
/// one (`PostToolUseFailure`, in the pinned event union), and it is not a step
/// here, so a call that errored records nothing and the window is never
/// credited with bytes it was not handed.
///
/// `startup` and `fork` are deliberately not evictions: a fresh session holds
/// nothing to lose, and a forked one carries the conversation it forked from.
/// An unnamed source is not one either, because what a signal means is the
/// family's to say and not this function's to guess.
pub(crate) fn gate_step(
    agent: &rift_core::AgentId,
    verb: &str,
    payload: &serde_json::Value,
) -> Option<GateStep> {
    use rift_agent::window_journal::EvictReason;
    let claude_code = *agent == rift_core::AgentId::CLAUDE_CODE;
    let codex = *agent == rift_core::AgentId::CODEX;
    let source = || payload.get("source").and_then(serde_json::Value::as_str);
    match verb {
        "PreToolUse" => Some(GateStep::Decide),
        "PostToolUse" => Some(GateStep::CrossCheck),
        "PreCompact" if claude_code || codex => Some(GateStep::Evict(EvictReason::PreCompact)),
        "PostCompact" if codex => Some(GateStep::Evict(EvictReason::PostCompact)),
        "SessionStart" if claude_code => match source()? {
            "compact" => Some(GateStep::Evict(EvictReason::Compact)),
            "clear" => Some(GateStep::Evict(EvictReason::Clear)),
            "resume" => Some(GateStep::Evict(EvictReason::Resume)),
            _ => None,
        },
        _ => None,
    }
}

/// The channel a decline of this `(family, verb)` would travel on, if the
/// family documents one and a recorded run has shown the model receiving it.
///
/// Read straight off the capability table, which is the one place a channel
/// is stated, so a family whose row is unconfirmed cannot be declined on by
/// this path however the rest of it is configured.
fn deny_channel(agent: &rift_core::AgentId, verb: &str) -> Option<rift_agent::respond::Channel> {
    rift_agent::respond::CAPABILITIES
        .iter()
        .find(|(family, on, channel)| {
            family == agent
                && *on == verb
                && matches!(channel, rift_agent::respond::Channel::DenyWithReason { .. })
        })
        .map(|(_, _, channel)| *channel)
}

/// One file's bytes as the gate needs them: which revision, how long, and how
/// far a run of lines reaches into it.
pub(super) struct GateFile {
    /// This checkout's own spelling of the path.
    pub(super) rel_path: String,
    /// The revision the bytes hash to now.
    pub(super) hash: rift_core::Sha256Id,
    /// The same bytes as git names them — the source the worker falls back
    /// to when the working file has moved on by the time it counts.
    blob_oid: String,
    /// How many lines the file has.
    pub(super) total_lines: u32,
    /// The bytes, kept only long enough to measure a slice of them.
    pub(super) bytes: Vec<u8>,
}

impl GateFile {
    /// The bytes as text, or nothing when they are not text this can search.
    ///
    /// An encoding no one here can read is not a reason to claim anything: the
    /// stretch an agent wrote is then looked for in an empty haystack, is not
    /// found, and the change is recorded as one whose places do not follow
    /// from its payload.
    pub(super) fn text(&self) -> &str {
        std::str::from_utf8(&self.bytes).unwrap_or_default()
    }

    /// How many bytes the lines `lo..=hi` amount to.
    fn slice_bytes(&self, run: rift_agent::window_journal::Interval) -> usize {
        self.bytes
            .split_inclusive(|byte| *byte == b'\n')
            .skip(run.lo as usize - 1)
            .take((run.hi - run.lo) as usize + 1)
            .map(<[u8]>::len)
            .sum()
    }
}

/// This checkout's own spelling of a path a payload named, or `None` when the
/// payload named something this checkout has no name for.
///
/// One spelling, in one place, because coverage is keyed by this string: two
/// routes to one file — a symlinked prefix, a relative name resolved against
/// some other directory — must not become two claims about two files.
///
/// A relative name is resolved against the working directory the **payload
/// itself states**, and against nothing else. That is what a shell read is
/// usually written as (`sed -n '1,40p' src/lib.rs`), so refusing one outright
/// dropped a whole class of read the recogniser had already understood. The
/// hook process's own cwd is deliberately not a fallback: a hook runs wherever
/// the client launched it, which is not a statement about where the agent
/// stood, and a guess here is a claim about bytes some other file holds. No
/// stated directory, or one that is itself relative, and the read is let
/// through with nothing recorded.
///
/// A file that does not exist yet is still named: a whole-file write says
/// beforehand which bytes it is about to put somewhere, and the somewhere is a
/// directory that does exist. The parent is canonicalized and the name put
/// back on, so a write that creates a file is keyed exactly as the read that
/// follows it will be.
pub(super) fn gate_rel_path(root: &Path, payload_path: &str, cwd: Option<&Path>) -> Option<String> {
    let named = Path::new(payload_path);
    let resolved;
    let named = if named.is_absolute() {
        named
    } else {
        resolved = cwd.filter(|dir| dir.is_absolute())?.join(named);
        resolved.as_path()
    };
    let canonical = match named.canonicalize() {
        Ok(here) => here,
        Err(_) => named
            .parent()?
            .canonicalize()
            .ok()?
            .join(named.file_name()?),
    };
    rift_recall_substrate::worktree::repo_relative_slash(root, &canonical)
}

/// The directory this payload says the agent was standing in, when it says one
/// in the only field that means that.
///
/// Only [`rift_agent::CwdSource::PayloadCwd`] answers. Antigravity sends no
/// working directory at all and its adapter fills the fact from the IDE's
/// workspace root under its own source token; a workspace root is where a
/// project lives, not where a command ran, so resolving a relative name
/// against it would be inventing a path. That family's own shell arguments
/// carry a `Cwd`, and its adapter has already resolved them by the time a
/// request reaches here.
pub(crate) fn payload_cwd(facts: &rift_agent::HookFacts) -> Option<PathBuf> {
    let stated = facts.cwd.as_ref()?;
    (stated.source == rift_agent::CwdSource::PayloadCwd).then(|| PathBuf::from(&stated.path))
}

/// Resolve a payload path to bytes of **this** checkout, or `None`.
///
/// Every refusal here is a read let through and nothing recorded, and each one
/// closes a way the record could claim something about bytes the agent was
/// never handed:
///
/// * a path this checkout has no name for ([`gate_rel_path`]), or one naming
///   something that is not a file, belongs to no checkout this journal
///   describes;
/// * a file past `max_bytes` — the repository's own inline-hash cap — is not
///   hashed, because the budget is the agent's and a saving is not worth
///   spending it;
/// * bytes holding a zero are not lines, and a range of them is not a range
///   of anything.
pub(super) fn gate_file(
    work_tree: &Path,
    payload_path: &str,
    cwd: Option<&Path>,
    max_bytes: u64,
) -> Option<GateFile> {
    let root = work_tree.canonicalize().ok()?;
    let rel_path = gate_rel_path(&root, payload_path, cwd)?;
    let canonical = root.join(&rel_path);
    let meta = std::fs::metadata(&canonical).ok()?;
    if !meta.is_file() || meta.len() > max_bytes {
        return None;
    }
    // The hasher the ingest already uses on this path, over the same bytes it
    // reads once: one read, one hash, no second visit to the filesystem.
    let (blob, bytes) = rift_recall_substrate::worktree::hash_blob_bytes(&root, &rel_path)?;
    if bytes.contains(&0) {
        return None;
    }
    // Lines as the file has them: a terminated run and an unterminated tail
    // are each one line, and an empty file has none.
    let total_lines = bytes.split_inclusive(|byte| *byte == b'\n').count();
    Some(GateFile {
        hash: rift_core::Sha256Id::parse_hex(&blob.content_sha256).ok()?,
        blob_oid: blob.blob_oid,
        total_lines: u32::try_from(total_lines).ok()?,
        rel_path,
        bytes,
    })
}

/// The window journal's side of one agent hook: what this context window is
/// being handed, and what it has stopped holding.
///
/// Runs before the ingest and opens no store: the journal is a file under this
/// checkout's own run state, the decision is pure, and the whole step is worth
/// a hash of at most a megabyte plus one read and one append. Every failure
/// leaves the journal with less than really happened, which costs a saving and
/// can never produce a decline of lines the window does not hold.
///
/// A hook that names no session records nothing: the journal is one context
/// window's, and there is no window to name.
/// Run the read gate, and hand back the adapter registry it built.
///
/// Both here because the gate is the first thing in an agent hook that needs
/// a registry, and one registry for the whole hook is the point: the gate,
/// the ingest and the worktree observation read the SAME adapter's facts, so
/// the `cwd`, the subagent and the read a row records can never come from two
/// different mappings of one payload.
///
/// The gate runs **before** the ingest, because the ingest is not on its
/// critical path, and before anything opens a store, because it never needs
/// one. It is guarded on its own so that a panic in it costs the journal a
/// line and the capture nothing.
pub(super) fn window_journal_step(
    t: &crate::hooktime::HookTimer,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    decline: &mut Option<String>,
    document: &mut Option<DocAnswer>,
    search: &mut Option<crate::search_gate::SearchAnswer>,
) -> (AdapterRegistry, bool) {
    let registry = AdapterRegistry::with_builtin();
    // What the operator permitted, read once for the whole step: every half
    // writes against one policy file, and a lever that is off writes nothing.
    let (policy, effective) = t.phase("gate_policy", || gate_policy(work_tree, &raw.agent));
    // A DOCUMENT read is answered before a *line* read is decided, because the
    // two are about different bytes: the sidecar substitution replaces the
    // whole call, and a decision about lines of a container nobody can read as
    // lines is not a decision worth reaching. Guarded on its own so that a
    // panic in it costs the substitution and nothing else.
    let documents = t.phase("doc_gate", || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            doc_gate(t, &registry, raw, git_dir, work_tree, &policy, &effective)
        }))
        .unwrap_or_default()
    });
    let answered = documents.answer.is_some();
    *document = documents.answer;
    // A call the document gate answered is not decided a second time as lines
    // of a file, and it grants this window no coverage of them. What such a
    // call delivers is the sidecar, or — when it was declined — nothing at
    // all; either way the document's own lines never reached the model, and a
    // line saying they did would let a later read of the same file be declined
    // as something the window already holds. Two decisions about one call
    // could only contradict each other anyway, and one of them would be the
    // one the client never sees.
    let outcome = if answered {
        GateOutcome::default()
    } else {
        t.phase("read_gate", || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                read_gate(&registry, raw, git_dir, work_tree, &effective)
            }))
            .unwrap_or_default()
        })
    };
    // What the window WROTE is coverage of a revision nobody read: the bytes
    // came out of the agent's own payload. Guarded apart from the gate so that
    // a panic in either costs one line of one journal, and the accounting the
    // gate journaled is answered by the same worker either way.
    t.phase("edit_record", || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            edit_record(&registry, raw, git_dir, work_tree, &effective);
        }));
    });
    // WHAT THE WINDOW DID **INSTEAD** OF READING: an act on a file Rift had
    // already told it that it holds. Guarded apart from the two above, and
    // after them, because it is decided over the lines they have just
    // written — the read that ran, or the edit that landed, is what answers
    // an announcement, and a booking taken before them would claim a read
    // that is happening as one that did not.
    let prevented = t.phase("prevented_record", || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            prevented_record(&registry, raw, git_dir, work_tree, &effective)
        }))
        .unwrap_or_default()
    });
    // A SEARCH is decided apart from both: it names no lines of a file and no
    // document, so nothing above it can have answered it, and a panic in it
    // costs the one bound it would have added. Guarded like its neighbours.
    let searches = t.phase("search_gate", || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::search_gate::search_gate(
                t, &registry, raw, git_dir, work_tree, &policy, &effective,
            )
        }))
        .unwrap_or_default()
    });
    *search = searches.answer;
    // Handed back the moment it is known, and before the ingest below can
    // fail: the decline is already a line of the window's journal, so the
    // answer the agent gets says the same thing whatever becomes of the rest.
    *decline = outcome.deny_reason;
    (
        registry,
        outcome.journaled || documents.journaled || searches.journaled || prevented,
    )
}

/// **Book the read an announcement made unnecessary**, once per announcement.
///
/// Rift pushes statements about held state into a context window — a delta
/// manifest entry, a recovery map entry — and the value of one is the read it
/// replaces. Until now nothing observed that read *not* happening, so the
/// whole of what those frames are worth was outside every figure Rift
/// publishes. This is the observation, and it is deliberately narrow: after
/// Rift has said "you hold this file", the window **acts on that file** —
/// changes it, or searches inside it — without asking for it again.
///
/// Both halves of the evidence are things a hook sees. A citation of the file
/// in the model's own prose is not one of them: no family reports prose to a
/// hook, so nothing here can know it happened and nothing here pretends to.
///
/// Every clause below is a way the booking could have been wrong:
///
/// * the lever is off, or the hook names no window — nothing is decided;
/// * the call is not the moment a call is about to run, so an announcement
///   is not answered twice by one act;
/// * the payload names no file this checkout has a name for;
/// * **nothing has been announced about that file**, or something has
///   answered the announcement since — a read (the bytes came back), a
///   decline (the intercept already books that slice, and booking here too
///   would count one saving twice), an earlier booking, or an emptying;
/// * the file no longer hashes to the revision that was announced, so what
///   the window holds is not what is there and no read of it was avoided.
///
/// It opens no store and counts no tokens: the announced lines are named to
/// the detached worker, which reads exactly those bytes and prices them with
/// the real tokenizer, exactly as a decline's slice is priced.
///
/// Returns whether the worker was left a row.
fn prevented_record(
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) -> bool {
    if effective.read_deny == rift_recall_policy::Mode::Off || raw.session.is_empty() {
        return false;
    }
    let Some(adapter) = registry.get(&raw.agent) else {
        return false;
    };
    if !matches!(
        gate_step(&raw.agent, &raw.verb, &raw.payload),
        Some(GateStep::Decide)
    ) {
        return false;
    }
    // An edit of the file, or a content search inside it. Both are acts on a
    // file whose bytes the window is not asking for, and both are named by a
    // pinned surface rather than inferred from a shape.
    let Some(named) = adapter
        .edit_request(&raw.verb, &raw.payload)
        .map(|request| request.path)
        .or_else(|| adapter.searched_file(&raw.verb, &raw.payload))
    else {
        return false;
    };
    let facts = adapter.facts(&raw.verb, &raw.payload);
    let cwd = payload_cwd(&facts);
    // The context window, not the session — the same key everything else on
    // this path is written under.
    let journal = crate::window_journal::WindowJournal::open(
        work_tree,
        &raw.session,
        facts.child_session.as_deref(),
    );
    let Ok(root) = work_tree.canonicalize() else {
        return false;
    };
    // The journal is consulted BEFORE the file is hashed: the overwhelming
    // majority of edits are of files nothing was announced about, and those
    // must cost one small file read and no hash at all.
    let Some(rel_path) = gate_rel_path(&root, &named, cwd.as_deref()) else {
        return false;
    };
    let Some(hold) = journal.load().journal.standing_hold(&rel_path) else {
        return false;
    };
    let Some(file) = gate_file(
        work_tree,
        &named,
        cwd.as_deref(),
        effective.max_inline_hash_bytes,
    ) else {
        return false;
    };
    if file.hash != hold.hash {
        return false;
    }
    let turn = gate_turn(raw, git_dir);
    let at_ms = u64::try_from(raw.occurred_at_ms).unwrap_or_default();
    // The line first, and the row only if it landed. The line is what bounds
    // the booking to one per announcement, so a row written beside a line that
    // could not be written is a figure a second act would book again.
    if journal
        .append(&rift_agent::window_journal::Line::Prevented {
            turn,
            path: rel_path.clone(),
            hash: hold.hash,
            range: hold.range,
            at_ms,
        })
        .is_err()
    {
        return false;
    }
    journal_prevented(
        git_dir,
        work_tree,
        raw,
        &rel_path,
        &file.blob_oid,
        hold,
        effective,
    )
}

/// Journal what one prevented read owes the worker, and say whether it left
/// the worker anything.
///
/// `lines` is the announced run, or `None` where the announcement was about
/// the whole file — the two are the same distinction
/// [`rift_agent::window_journal::Interval::is_whole_file`] draws, and the whole
/// file is priced as the whole file rather than as four billion lines.
fn journal_prevented(
    git_dir: &Path,
    work_tree: &Path,
    raw: &rift_shadow::hooks::RawHook,
    rel_path: &str,
    blob_oid: &str,
    hold: rift_agent::window_journal::StandingHold,
    effective: &rift_recall_policy::Effective,
) -> bool {
    use crate::agent_queue::{AuditBasis, AuditLines, ServeAudit, ServeAuditKind};
    let decision = if effective.read_deny == rift_recall_policy::Mode::Enforce {
        ServeAuditKind::ReadPrevented
    } else {
        ServeAuditKind::ReadPreventedObserved
    };
    crate::agent_queue::enqueue(
        git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: raw.occurred_at_ms,
            attempts: 0,
            kind: crate::agent_queue::JobKind::ServeAudit(ServeAudit {
                call_id: {
                    use rift_core::UlidSource as _;
                    AUDIT_IDS.next_ulid().to_hex()
                },
                decision,
                agent: raw.agent.as_str().to_owned(),
                basis: Some(AuditBasis {
                    rel_path: rel_path.to_owned(),
                    content_sha256: hold.hash.to_hex(),
                    blob_oid: blob_oid.to_owned(),
                    lines: (!hold.range.is_whole_file()).then_some(AuditLines {
                        lo: hold.range.lo,
                        hi: hold.range.hi,
                    }),
                }),
                // Nothing was put in front of the model: the frame that made
                // this read unnecessary is charged where it was injected.
                frame: String::new(),
                tag: None,
                total: None,
                declared_avoided: None,
                shape: None,
            }),
        },
    )
    .is_ok()
}

/// What one hook's read gate concluded: the answer the agent is owed, and
/// whether the detached worker was left something to write.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct GateOutcome {
    /// An accounting row was journaled, so the worker is spawned for it too.
    journaled: bool,
    /// The rendered explanation this call is declined with — `None` for the
    /// overwhelming majority of calls, which run exactly as they were written.
    deny_reason: Option<String>,
}

/// A source of per-decision accounting ids, process-wide for the reason
/// [`TURN_IDS`](super::manifest::TURN_IDS) is: a fresh source inside one millisecond re-emits the same
/// id, and two decisions that book identical figures are still two decisions.
pub(crate) static AUDIT_IDS: std::sync::LazyLock<rift_core::MonotonicUlidSource> =
    std::sync::LazyLock::new(rift_core::MonotonicUlidSource::new);

/// The accounting row a read decision owes, or `None` when it owes none.
///
/// An allowed read owes nothing: nothing was decided, and the journal line is
/// the whole record. A decline owes the row that carries what it saved and
/// what its reason cost; a decline that was only observed owes the row that
/// sizes the pool without claiming any of it. `frame` is the text the model
/// was handed, which is empty until a family is being declined on.
///
/// `booked` says that a **prevented** row of this same window has already
/// taken this slice. The decline itself is unchanged — the model is still
/// declined, still pointed at what it holds, and the reason it was sent is
/// still charged — but the slice is worth one read, so this row declares a
/// zero under the tag that says which row has it. The mirror order needs
/// nothing: a decline answers the announcement, so no booking follows it.
fn read_decision_audit(
    agent: &rift_core::AgentId,
    read: &rift_agent::window_journal::ResolvedRead,
    blob_oid: &str,
    decision: &rift_agent::window_journal::Decision,
    frame: &str,
    booked: bool,
    call_id: String,
) -> Option<crate::agent_queue::ServeAudit> {
    use crate::agent_queue::{AuditBasis, AuditLines, ServeAudit, ServeAuditKind};
    use rift_agent::window_journal::Decision;
    let declined = matches!(decision, Decision::Deny(_));
    let decision = match decision {
        Decision::Allow => return None,
        Decision::Deny(_) => ServeAuditKind::ReadIntercepted,
        Decision::Observe(_) => ServeAuditKind::ReadRepeatObserved,
    };
    let twice = declined && booked;
    Some(ServeAudit {
        call_id,
        decision,
        agent: agent.as_str().to_owned(),
        basis: Some(AuditBasis {
            rel_path: read.path.clone(),
            content_sha256: read.hash.to_hex(),
            blob_oid: blob_oid.to_owned(),
            lines: Some(AuditLines {
                lo: read.range.lo,
                hi: read.range.hi,
            }),
        }),
        frame: frame.to_owned(),
        tag: twice.then(|| rift_recall_serve::TAG_PREVENTED_FIRST.to_owned()),
        total: None,
        declared_avoided: twice.then_some(0),
        // Not a name search: nothing here stood in for a listing.
        shape: None,
    })
}

/// The decision as it is finally answered, the sentence carrying it, and the
/// reason to send back — which is the sentence only where one is actually sent.
///
/// The sentence is written by the one renderer allowed to write one. Composed
/// only where a decline travels: a decision that was merely recorded cost the
/// model nothing, and a row claiming otherwise would put a frame nobody read
/// on the paying side of the honesty gate.
///
/// A shape with nothing exact left to say has no frame, and a decline with no
/// frame is an unexplained tool failure — so the call runs instead, and the
/// journal records the lines it will have delivered.
fn gate_answer(
    held: &rift_agent::window_journal::Journal,
    read: &rift_agent::window_journal::ResolvedRead,
    decision: rift_agent::window_journal::Decision,
    context_lines: u32,
) -> (
    rift_agent::window_journal::Decision,
    Option<String>,
    Option<String>,
) {
    use rift_agent::window_journal::Decision;
    let frame = gate_frame(held, read, &decision, context_lines);
    let decision = match (&decision, &frame) {
        (Decision::Deny(_), None) => Decision::Allow,
        _ => decision,
    };
    let deny_reason = match (&decision, &frame) {
        (Decision::Deny(_), Some(frame)) => Some(frame.clone()),
        _ => None,
    };
    (decision, frame, deny_reason)
}

/// The file a request names and the request resolved against it, or `None`
/// where this checkout has nothing to say about either.
///
/// Two refusals in one place because they are one question — *which lines of
/// which revision is this call about* — and every way of failing to answer it
/// leaves the call alone and records nothing.
fn gate_resolve(
    work_tree: &Path,
    request: rift_agent::ReadRequest,
    cwd: Option<&Path>,
    effective: &rift_recall_policy::Effective,
) -> Option<(GateFile, rift_agent::window_journal::ResolvedRead)> {
    let file = gate_file(
        work_tree,
        &request.path,
        cwd,
        effective.max_inline_hash_bytes,
    )?;
    let read = rift_agent::window_journal::ResolvedRead::new(
        &rift_agent::ReadRequest {
            path: file.rel_path.clone(),
            ..request
        },
        file.hash,
        file.total_lines,
    )?;
    Some((file, read))
}

/// The turn a line is recorded in, derived without opening anything the gate
/// path may not open.
///
/// The wire's own ordinal when it carried one — an envelope states its turn and
/// is trusted verbatim, exactly as the ingest layer trusts it. Every alpha
/// family's NATIVE hook shape carries none, and before this they all recorded
/// turn 0, which flattened both figures that are counted per turn: a retry
/// inside one turn and what a window held before it lost it.
///
/// The store-free source is the transient session cache the ingest layer
/// already keeps beside the git dir (`<git_dir>/rift-sessions/<session>.json`):
/// a small JSON file whose `current_turn_ordinal` a prompt submission advances
/// at the turn boundary, well before any tool call of that turn reaches this
/// gate. Reading it is one `read` call — no store, no tokenizer, no
/// projection, which is what GOAL §2.5 asks of everything on this path.
///
/// A missing, unreadable or not-yet-written cache is turn 0, the number every
/// line carried before this: the direction of the miss is a lost distinction
/// between two turns, never a decision, because nothing declines on a turn.
///
/// A subagent's window shares its parent's session id and therefore its turn.
/// That is the honest answer: the turn is a property of the conversation, and
/// the window it belongs to is already keyed separately by the journal itself.
pub(crate) fn gate_turn(raw: &rift_shadow::hooks::RawHook, git_dir: &Path) -> u32 {
    if let Some(turn) = raw.turn {
        return turn;
    }
    rift_shadow::hooks::session_cache::read_cache(git_dir, &raw.session)
        .ok()
        .flatten()
        .map_or(0, |cache| cache.current_turn_ordinal)
}

/// Whether a pre-call slice fits every verified interception boundary.
///
/// **Unchanged by the client's own statement about a delivery, because at this
/// boundary there is no delivery to state.** A call that has not run has
/// answered nothing, so a slice past the size density can vouch for still runs
/// untouched and still creates no coverage here; the coverage arrives one hook
/// later, from the result, through
/// [`rift_agent::window_journal::reconcile_delivery`]. What that costs is a
/// parallel identical read issued in the same turn: both calls reach this
/// boundary before either result does, so neither can be declined on the
/// other. That is a saving not taken, which is the direction every rule on
/// this path leans.
fn gate_delivery_is_actionable(
    read: &rift_agent::window_journal::ResolvedRead,
    slice_bytes: usize,
    limits: rift_agent::window_journal::Limits,
) -> bool {
    slice_bytes <= limits.max_slice_bytes && read.delivery_is_complete(slice_bytes, limits)
}

/// The same reconciliation against what this call was credited with before its
/// result arrived, and nothing at all where the result stated nothing.
fn reconcile_stated_delivery(
    journal: &crate::window_journal::WindowJournal,
    read: &rift_agent::window_journal::ResolvedRead,
    stated: Option<rift_agent::window_journal::StatedDelivery>,
    slice_bytes: usize,
    limits: rift_agent::window_journal::Limits,
    turn: u32,
    at_ms: u64,
) {
    if let Some(delivered) = stated {
        reconcile_delivered_lines(
            journal,
            read,
            read.range_delivered(slice_bytes, limits),
            delivered,
            turn,
            at_ms,
        );
    }
}

/// The gate itself, which the guard above is what keeps off the ingest's back.
///
/// Returns whether it journaled an accounting row, so the one worker this
/// hook spawns is spawned for that too.
pub(super) fn read_gate(
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) -> GateOutcome {
    let quiet = GateOutcome::default();
    // A lever nobody is holding decides nothing and records nothing: an
    // unreadable policy and an explicit `off` both land here, and the journal
    // this repository never asked for is never written.
    if effective.read_deny == rift_recall_policy::Mode::Off || raw.session.is_empty() {
        return quiet;
    }
    let Some(adapter) = registry.get(&raw.agent) else {
        return quiet;
    };
    let Some(step) = gate_step(&raw.agent, &raw.verb, &raw.payload) else {
        return quiet;
    };
    // The context window, not the session: a subagent's hooks arrive under its
    // parent's session id and carry their own agent id, and the two windows
    // hold different bytes. Read once, because the directory a relative name
    // is resolved against comes off the same mapping of the same payload.
    let facts = adapter.facts(&raw.verb, &raw.payload);
    let cwd = payload_cwd(&facts);
    let window = facts.child_session;
    let journal =
        crate::window_journal::WindowJournal::open(work_tree, &raw.session, window.as_deref());
    let turn = gate_turn(raw, git_dir);
    let at_ms = u64::try_from(raw.occurred_at_ms).unwrap_or_default();
    if let GateStep::Evict(reason) = step {
        // The lines before it are kept: they stop being coverage and become
        // the map of what the window has just stopped holding.
        let _ = journal.append(&rift_agent::window_journal::Line::Evict {
            turn,
            reason,
            at_ms,
        });
        return quiet;
    }
    // What the result says about its own delivery, read before the journal is
    // consulted because it is three field lookups and it decides whether a
    // result with no decline behind it is work at all. A statement either way
    // is work: one that names less than the record claims takes lines back,
    // and one that names the call answered in full is the only evidence a
    // delivery too large for the size rule can ever have.
    let stated = match step {
        GateStep::CrossCheck => stated_delivery(&raw.agent, &raw.payload),
        _ => None,
    };
    let held = journal.load().journal;
    if matches!(step, GateStep::CrossCheck)
        && stated.is_none()
        && !held
            .live()
            .iter()
            .any(|line| matches!(line, rift_agent::window_journal::Line::Deny { .. }))
    {
        // Nothing was declined, so a result cannot contradict a decline, and
        // the read behind it was recorded before the call ran. Answered from
        // the journal alone: the check must not cost a hash it cannot use.
        return quiet;
    }
    // Every read the call performs, not just its first. One shell command
    // line is routinely several — `cd <dir> && cat a && cat b` is one call
    // and three facts — and a member left out of this list is a read the
    // window is never credited with.
    let requests = adapter.read_requests(&raw.verb, &raw.payload);
    if requests.is_empty() {
        return quiet;
    }
    let pass = GatePass {
        step,
        held: &held,
        journal: &journal,
        stated,
        limits: gate_limits(effective),
        turn,
        at_ms,
    };
    // Phase one: decide every member against ONE snapshot of the journal,
    // deciding nothing about the call as a whole yet.
    let mut members =
        gate_decide_members(raw, work_tree, cwd.as_deref(), effective, &pass, &requests);
    // Phase two: the call is one act, so it is declined only when declining
    // every part of it would be right. One member the window does not hold —
    // or one nobody here could resolve at all, which is why the count of
    // members is compared with the count of requests — and the call runs,
    // because a decline answers the whole command line and the agent would
    // lose the reads that were never in question.
    let wholly_declined = members.len() == requests.len()
        && members
            .iter()
            .all(|m| matches!(m.decision, rift_agent::window_journal::Decision::Deny(_)));
    if !wholly_declined {
        for member in &mut members {
            if matches!(
                member.decision,
                rift_agent::window_journal::Decision::Deny(_)
            ) {
                member.decision = rift_agent::window_journal::Decision::Allow;
                member.frame = None;
            }
        }
    }
    // The sentence the call is answered with: each member's own frame, from
    // the one renderer allowed to write one, one to a line. Nothing is
    // composed here — the join is a line break between two rendered facts.
    let deny_reason = wholly_declined
        .then(|| {
            members
                .iter()
                .filter_map(|m| m.frame.clone())
                .collect::<Vec<_>>()
        })
        .filter(|frames| frames.len() == members.len())
        .map(|frames| frames.join("\n"));
    GateOutcome {
        // Phase three: record what each member decided, and journal what each
        // one owes.
        journaled: gate_record_members(git_dir, work_tree, raw, &members, &pass),
        deny_reason,
    }
}

/// The facts every member of one call is decided and recorded against.
///
/// Gathered once because they are properties of the *call*, not of the member:
/// the step it arrived on, the journal snapshot every member is judged against,
/// and the moment it is all being written at. Passing them as one value is
/// also what keeps the two halves below from drifting apart — a member decided
/// against one snapshot and recorded against another would be a decision about
/// a window that had already moved.
struct GatePass<'a> {
    /// Which step of the gate this call is at.
    step: GateStep,
    /// The window's lines as they stood when the call arrived.
    held: &'a rift_agent::window_journal::Journal,
    /// The journal those lines are appended to.
    journal: &'a crate::window_journal::WindowJournal,
    /// What the result said about its own delivery, on the step that has a
    /// result at all.
    stated: Option<rift_agent::window_journal::StatedDelivery>,
    /// The policy ceilings in force.
    limits: rift_agent::window_journal::Limits,
    /// The turn every line written here carries.
    turn: u32,
    /// The moment it carries.
    at_ms: u64,
}

/// One read of a call that may perform several, decided and not yet recorded.
///
/// The phase split is the whole reason this type exists: a member's decision
/// is reached against the journal as it stood when the call arrived, and only
/// once every member has one does the call as a whole get to say whether a
/// decline travels. Recording inside the first pass would write declines that
/// the second pass then has to take back.
struct GateMember {
    /// The file this member named, as this checkout spells it.
    file: GateFile,
    /// The member's request resolved against that file.
    read: rift_agent::window_journal::ResolvedRead,
    /// How many bytes the run of lines it asked for holds.
    slice_bytes: usize,
    /// What was decided about it.
    decision: rift_agent::window_journal::Decision,
    /// The rendered sentence that decision would be answered with, where
    /// there is one.
    frame: Option<String>,
}

/// Phase one: every member of the call, resolved and decided, and nothing
/// written.
///
/// A member missing from the answer is a part of the call nobody here can
/// answer for — a path this checkout has no name for, a file past the inline
/// hash cap, a slice larger than the client's own result channel, or a
/// cross-checked read with no decline behind it. The caller counts what comes
/// back against what went in, which is how one unanswerable member keeps the
/// whole line from being declined.
fn gate_decide_members(
    raw: &rift_shadow::hooks::RawHook,
    work_tree: &Path,
    cwd: Option<&Path>,
    effective: &rift_recall_policy::Effective,
    pass: &GatePass<'_>,
    requests: &[rift_agent::ReadRequest],
) -> Vec<GateMember> {
    let mut members: Vec<GateMember> = Vec::new();
    for request in requests {
        let Some((file, read)) = gate_resolve(work_tree, request.clone(), cwd, effective) else {
            continue;
        };
        let slice_bytes = file.slice_bytes(read.range);
        // A request larger than the client's verified inline result channel —
        // including one pathological long line — runs untouched and creates no
        // coverage. A structural replacement belongs to the response planner;
        // this line gate must not repeatedly paginate or invent delivery.
        if matches!(pass.step, GateStep::Decide)
            && !gate_delivery_is_actionable(&read, slice_bytes, pass.limits)
        {
            continue;
        }
        let decision = if matches!(pass.step, GateStep::CrossCheck) {
            // The result of a call this window declined. The bytes reached the
            // model however the decision was answered, so the honest record is
            // that the window holds them now; the decline standing just before
            // that read is the pair a diagnostic counts.
            if !crate::window_journal::deny_stands(pass.held, &read.deny_key()) {
                // No decline to contradict, so there is no decision to reach
                // and nothing to journal — but a result that states its own
                // delivery still moves this window's coverage: back, where it
                // says the call was cut short, and forward, where it says the
                // call was answered in full and the slice was too large for the
                // size rule to have credited it before the call ran.
                reconcile_stated_delivery(
                    pass.journal,
                    &read,
                    pass.stated,
                    slice_bytes,
                    pass.limits,
                    pass.turn,
                    pass.at_ms,
                );
                continue;
            }
            rift_agent::window_journal::Decision::Allow
        } else {
            rift_agent::window_journal::decide(
                &read,
                pass.held,
                gate_mode(effective.read_deny),
                deny_channel(&raw.agent, &raw.verb),
                gate_edit_echo(effective),
                slice_bytes,
                pass.limits,
            )
        };
        let (decision, frame, _) =
            gate_answer(pass.held, &read, decision, pass.limits.context_lines);
        members.push(GateMember {
            file,
            read,
            slice_bytes,
            decision,
            frame,
        });
    }
    members
}

/// Phase three: write each member's decision into the window's journal and
/// journal the accounting it owes, and say whether the worker was left
/// anything at all.
///
/// A decline that lost its frame in phase two is an allowed read by the time
/// it arrives here, so nothing books an avoidance the model was never told
/// about.
fn gate_record_members(
    git_dir: &Path,
    work_tree: &Path,
    raw: &rift_shadow::hooks::RawHook,
    members: &[GateMember],
    pass: &GatePass<'_>,
) -> bool {
    let mut journaled = false;
    for member in members {
        // Whether this window has already booked any of these bytes as a read
        // that never happened. Asked of the journal this gate already has open
        // — no second file, no store, no tokenizer — and only where a decline
        // is actually being made, so the ordinary allowed call pays nothing
        // for it.
        //
        // Any of them, not all of them: this decline books its whole run or
        // none of it, so a booking that already holds part of the run is a
        // booking this claim would double.
        let booked = matches!(
            member.decision,
            rift_agent::window_journal::Decision::Deny(_)
        ) && pass.held.prevented_intersects(
            &member.read.path,
            member.read.hash,
            member.read.range,
        );
        let _ = pass.journal.append(&rift_agent::window_journal::record(
            &member.read,
            &member.decision,
            member.slice_bytes,
            pass.limits,
            pass.turn,
            pass.at_ms,
        ));
        // A cross-checked read whose own result states its delivery: the line
        // just written credits the window with what the size rule could vouch
        // for, and this moves it to what the client says actually arrived.
        reconcile_stated_delivery(
            pass.journal,
            &member.read,
            pass.stated,
            member.slice_bytes,
            pass.limits,
            pass.turn,
            pass.at_ms,
        );
        // The decision is recorded in the window's own journal either way; what
        // is journaled here is the accounting the worker owes for it, the
        // frame included — a row that books what a decline avoided and says
        // nothing about what its reason cost is a row that flatters itself.
        journaled |= journal_decision(
            git_dir,
            work_tree,
            raw,
            &member.read,
            &member.file.blob_oid,
            &member.decision,
            member.frame.as_deref().unwrap_or_default(),
            booked,
        );
    }
    journaled
}

/// Journal the accounting one decision owes to the detached worker, and say
/// whether it left the worker anything at all.
///
/// Best-effort by design: a journal that cannot be written costs a row and
/// never the agent's call, which has already been answered by the time this
/// runs.
fn journal_decision(
    git_dir: &Path,
    work_tree: &Path,
    raw: &rift_shadow::hooks::RawHook,
    read: &rift_agent::window_journal::ResolvedRead,
    blob_oid: &str,
    decision: &rift_agent::window_journal::Decision,
    frame: &str,
    booked: bool,
) -> bool {
    let Some(audit) = read_decision_audit(&raw.agent, read, blob_oid, decision, frame, booked, {
        use rift_core::UlidSource as _;
        AUDIT_IDS.next_ulid().to_hex()
    }) else {
        return false;
    };
    crate::agent_queue::enqueue(
        git_dir,
        &crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: raw.occurred_at_ms,
            attempts: 0,
            kind: crate::agent_queue::JobKind::ServeAudit(audit),
        },
    )
    .is_ok()
}

/// The turn this window was last handed — or last authored — the bytes it is
/// now asking for again.
///
/// The frame says "unchanged since turn N", and N is that moment, not the turn
/// the repeat is being made in. Read off the live lines rather than tracked
/// separately, because the journal is already the record of when a window was
/// given what.
fn held_since(
    held: &rift_agent::window_journal::Journal,
    read: &rift_agent::window_journal::ResolvedRead,
) -> Option<u64> {
    use rift_agent::window_journal::Line;
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

/// The explanation a declined call is answered with, or `None` when a decline
/// has nothing exact left to say and so must not be made.
///
/// Both frames come from the sealed renderers beside the manifest's: this
/// function chooses which and supplies the numbers, and composes no text of
/// its own. The delta renderer clamps the changed places to the file's own
/// length and derives the window it suggests, which is why the shape's own
/// window is not passed on — one clamp, in the place that also does the
/// rendering.
///
/// Only a decline has a frame. A decision that was recorded and not acted on
/// sent the model nothing, and a rendered sentence nobody read is not a cost
/// the accounting may book against it.
fn gate_frame(
    held: &rift_agent::window_journal::Journal,
    read: &rift_agent::window_journal::ResolvedRead,
    decision: &rift_agent::window_journal::Decision,
    context_lines: u32,
) -> Option<String> {
    use rift_agent::window_journal::{Decision, DenyShape};
    let Decision::Deny(shape) = decision else {
        return None;
    };
    let hash = read.hash.to_hex();
    let since = held_since(held, read);
    match shape {
        DenyShape::Plain => Some(rift_recall_serve::render_deny_frame(
            &read.path,
            &hash,
            read.range.lo,
            read.range.hi,
            since,
        )),
        DenyShape::Delta { sites, shift, .. } => {
            let sites: Vec<(u32, u32)> = sites.iter().map(|site| (site.lo, site.hi)).collect();
            rift_recall_serve::render_delta_frame(
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

#[cfg(test)]
mod read_gate_tests {
    use rift_agent::window_journal::{Interval, Line, StatedDelivery};
    use rift_shadow::hooks::RawHook;

    use super::*;
    use crate::hooks::audit::serve_audit_event;
    #[cfg(feature = "doc")]
    use crate::hooks::doc_gate::{doc_gate_in, DocOutcome};
    use crate::hooks::ingest::owe_basis_snapshots;
    #[cfg(feature = "search")]
    use crate::hooks::worker::rebuild_search_index_job;
    use crate::hooks::worker::{owe_search_index, snapshot_basis_job};
    use rift_core::Result;

    /// A hook as the gate reads one: the family, the verb, the window it
    /// belongs to and the payload it carried.
    fn hook(agent: &str, verb: &str, session: &str, payload: serde_json::Value) -> RawHook {
        RawHook {
            agent: rift_core::AgentId::new(agent),
            verb: verb.to_owned(),
            session: session.to_owned(),
            turn: Some(4),
            occurred_at_ms: 1_700_000_000_000,
            payload,
        }
    }

    /// The same hook as a family whose native shape carries no turn sends it.
    ///
    /// Every alpha family is this shape: the ordinal is not on the wire, and
    /// the gate has to find it somewhere that costs no store.
    fn native_hook(agent: &str, verb: &str, session: &str, payload: serde_json::Value) -> RawHook {
        RawHook {
            turn: None,
            ..hook(agent, verb, session, payload)
        }
    }

    /// Open turn `ordinal` for `session` the way a prompt submission does:
    /// through the transient session cache the ingest layer keeps beside the
    /// git dir, which is the one place the gate may read a turn from.
    fn open_turn(work_tree: &Path, session: &str, ordinal: u32) {
        let git_dir = work_tree.join(".git");
        let mut cache = rift_shadow::hooks::session_cache::SessionCache::new(
            session,
            "claude-code",
            "active",
            1_700_000_000_000,
        );
        cache.current_turn_ordinal = ordinal;
        cache.turn_open = true;
        rift_shadow::hooks::session_cache::write_cache(&git_dir, &cache)
            .expect("the boundary is recorded");
    }

    /// A `Read` of `path`, as the pinned fields spell one.
    fn read_of(path: &Path, offset: Option<u32>, limit: Option<u32>) -> serde_json::Value {
        let mut input = serde_json::Map::new();
        input.insert(
            "file_path".to_owned(),
            serde_json::json!(path.display().to_string()),
        );
        if let Some(offset) = offset {
            input.insert("offset".to_owned(), serde_json::json!(offset));
        }
        if let Some(limit) = limit {
            input.insert("limit".to_owned(), serde_json::json!(limit));
        }
        serde_json::json!({ "tool_name": "Read", "tool_input": input })
    }

    /// A file of `lines` numbered lines.
    fn numbered(lines: u32) -> String {
        (1..=lines).fold(String::new(), |mut body, n| {
            use std::fmt::Write as _;
            let _ = writeln!(body, "line {n}");
            body
        })
    }

    /// A work tree with one file of `lines` numbered lines in it.
    fn checkout(lines: u32) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().expect("a work tree");
        let file = tmp.path().join("src.rs");
        std::fs::write(&file, numbered(lines)).expect("the file");
        (tmp, file)
    }

    /// Another file of `lines` numbered lines beside the first.
    fn beside(work_tree: &Path, name: &str, lines: u32) -> PathBuf {
        let file = work_tree.join(name);
        std::fs::write(&file, numbered(lines)).expect("the file");
        file
    }

    fn lines_of(work_tree: &Path, session: &str, window: Option<&str>) -> Vec<Line> {
        crate::window_journal::WindowJournal::open(work_tree, session, window)
            .load()
            .journal
            .lines()
            .to_vec()
    }

    fn lines_journal(work_tree: &Path, session: &str) -> rift_agent::window_journal::Journal {
        crate::window_journal::WindowJournal::open(work_tree, session, None)
            .load()
            .journal
    }

    /// The levers as this repository ships them, resolved for one family:
    /// observe everywhere, with the defaults. Built through the policy crate's
    /// own `resolve` rather than by hand, so a test cannot grant itself a mode
    /// the resolver would have capped.
    fn levers(family: &str, mode: rift_recall_policy::Mode) -> rift_recall_policy::Effective {
        let mut policy = rift_recall_policy::ServePolicy::default();
        policy.interception.mode = mode;
        // The line floor is a policy trade with its own tests, and these
        // fixtures are a handful of lines each: held at one so a walk here
        // exercises the gate it is written about rather than the floor.
        policy.interception.min_slice_lines = 1;
        // Named, not repository-wide: a family the measured caveats hold back
        // is reached only by being named, and a test that skipped the naming
        // would be testing the caveat rather than the lever.
        policy.interception.agents = rift_recall_policy::AgentModes {
            claude_code: (family == "claude-code").then_some(mode),
            codex: (family == "codex").then_some(mode),
            antigravity: (family == "antigravity").then_some(mode),
        };
        rift_recall_policy::resolve(
            &policy,
            family,
            crate::recall::family_capability(&rift_core::AgentId::new(family)),
        )
    }

    fn observing() -> rift_recall_policy::Effective {
        levers("claude-code", rift_recall_policy::Mode::Observe)
    }

    fn enforcing() -> rift_recall_policy::Effective {
        levers("claude-code", rift_recall_policy::Mode::Enforce)
    }

    fn gate(raw: &RawHook, work_tree: &Path) -> GateOutcome {
        gate_under(raw, work_tree, &observing())
    }

    fn gate_under(
        raw: &RawHook,
        work_tree: &Path,
        effective: &rift_recall_policy::Effective,
    ) -> GateOutcome {
        read_gate(
            &AdapterRegistry::with_builtin(),
            raw,
            &work_tree.join(".git"),
            work_tree,
            effective,
        )
    }

    // ------------------------------------------------- the document gate ---

    /// A converter that answers whatever this test says, and counts how often
    /// it was asked.
    ///
    /// The counting is half the claim in two of the tests below: "decided
    /// against once, for the whole family" and "the family's answer is linked,
    /// not re-parsed" are both statements about how many times a parser ran.
    #[cfg(feature = "doc")]
    struct StandInConverter {
        answer: std::result::Result<rift_recall_doc::Markdown, rift_recall_doc::DocError>,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[cfg(feature = "doc")]
    impl StandInConverter {
        fn markdown(text: &str) -> Self {
            Self::answering(Ok(rift_recall_doc::Markdown {
                text: text.to_owned(),
                ..rift_recall_doc::Markdown::default()
            }))
        }

        fn answering(
            answer: std::result::Result<rift_recall_doc::Markdown, rift_recall_doc::DocError>,
        ) -> Self {
            Self {
                answer,
                calls: std::sync::atomic::AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[cfg(feature = "doc")]
    impl rift_recall_doc::DocConverter for StandInConverter {
        fn convert(
            &self,
            _bytes: &[u8],
            _ext: &str,
            _caps: &rift_recall_doc::ConvertCaps,
        ) -> std::result::Result<rift_recall_doc::Markdown, rift_recall_doc::DocError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.answer.clone()
        }
    }

    /// A checkout holding one document, and the family store beside it.
    #[cfg(feature = "doc")]
    fn documents(
        name: &str,
        bytes: &[u8],
    ) -> (tempfile::TempDir, PathBuf, crate::doc_store::DocStore) {
        let tmp = tempfile::tempdir().expect("a work tree");
        let work_tree = tmp.path().join("main");
        let common_dir = tmp.path().join("common");
        std::fs::create_dir_all(&work_tree).expect("the checkout");
        std::fs::create_dir_all(&common_dir).expect("the common dir");
        let document = work_tree.join(name);
        std::fs::write(&document, bytes).expect("the document");
        let store = crate::doc_store::DocStore::for_member(&common_dir, &work_tree);
        (tmp, document, store)
    }

    /// The document levers as this repository ships them, for one family.
    #[cfg(feature = "doc")]
    fn document_levers() -> (
        rift_recall_policy::ServePolicy,
        rift_recall_policy::Effective,
    ) {
        let policy = rift_recall_policy::ServePolicy::default();
        let effective = rift_recall_policy::resolve(
            &policy,
            "claude-code",
            crate::recall::family_capability(&rift_core::AgentId::CLAUDE_CODE),
        );
        (policy, effective)
    }

    /// One document read through the gate, against a store and a converter the
    /// test owns.
    #[cfg(feature = "doc")]
    fn doc_gate_for(
        raw: &RawHook,
        work_tree: &Path,
        store: &crate::doc_store::DocStore,
        converter: Option<&dyn rift_recall_doc::DocConverter>,
    ) -> DocOutcome {
        let (policy, effective) = document_levers();
        doc_gate_in(
            &crate::hooktime::HookTimer::new("test", false),
            &AdapterRegistry::with_builtin(),
            raw,
            &work_tree.join(".git"),
            work_tree,
            &policy,
            &effective,
            store,
            converter,
        )
    }

    /// ★ A read of a document is answered with the Markdown Rift holds for it:
    /// the family store gains the answer, this member gains its view, and the
    /// response the client actually receives names that view in the one field
    /// the pinned channel allows to be amended.
    #[cfg(feature = "doc")]
    #[test]
    fn a_read_of_a_docx_is_rewritten_to_its_sidecar() {
        let (tmp, document, store) = documents("spec.docx", b"PK\x03\x04 a document");
        let work_tree = tmp.path().join("main");
        let converter = StandInConverter::markdown("# Spec\n\nThe body.\n");
        let call = hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_of(&document, None, None),
        );

        let outcome = doc_gate_for(&call, &work_tree, &store, Some(&converter));
        let Some(DocAnswer::Rewrite {
            tool_input,
            sidecar,
        }) = outcome.answer.clone()
        else {
            panic!("a document read is answered with its sidecar: {outcome:?}");
        };
        assert_eq!(converter.calls(), 1, "converted once");
        assert!(
            Path::new(&sidecar).starts_with(work_tree.join(".rift/state/doc")),
            "the agent is pointed at a path inside its own checkout: {sidecar}"
        );
        assert!(
            std::fs::read_to_string(&sidecar)
                .expect("this member's view")
                .contains("The body."),
            "the view carries the conversion"
        );

        // The whole response, composed where every response is composed: the
        // one amended field is the path, and nothing else of the call moved.
        let response = rift_agent::respond::gate(
            &rift_core::AgentId::CLAUDE_CODE,
            "PreToolUse",
            &rift_agent::respond::GateDecision::Rewrite(
                rift_agent::respond::Rewrite::DocumentSidecar {
                    tool_input: &tool_input,
                    file_path: &sidecar,
                },
            ),
        )
        .expect("the amended call");
        assert_eq!(
            response.pointer("/hookSpecificOutput/updatedInput/file_path"),
            Some(&serde_json::Value::from(sidecar.as_str()))
        );
        assert_eq!(
            response.pointer("/hookSpecificOutput/permissionDecision"),
            Some(&serde_json::Value::from("allow")),
            "a substitution declines nothing"
        );
        assert!(
            !response.to_string().contains("The body."),
            "a hook response carries a path and never the bytes at it"
        );

        // A second read of the same document is one `stat` and a link that is
        // already there: the family paid the parser once.
        let again = doc_gate_for(&call, &work_tree, &store, Some(&converter));
        assert!(matches!(again.answer, Some(DocAnswer::Rewrite { .. })));
        assert_eq!(converter.calls(), 1, "the family's answer is not re-parsed");
    }

    /// ★ Two documents no substitution may touch. A PDF whose conversion is
    /// page images rather than text is decided against once for the whole
    /// family and never parsed again; a read that addresses `pages` is not a
    /// read of the document at all, and carries a range the sidecar has no
    /// answer for.
    #[cfg(feature = "doc")]
    #[test]
    fn a_read_of_a_scanned_pdf_or_with_pages_is_left_alone() {
        let (tmp, document, store) = documents("scan.pdf", b"%PDF-1.7 pages of pictures");
        let work_tree = tmp.path().join("main");
        // Four pages, and the text a scanner's OCR-less extract leaves.
        let scanned = StandInConverter::answering(Ok(rift_recall_doc::Markdown {
            text: "1\n2\n3\n4\n".to_owned(),
            pages: Some(4),
            thin_pages: vec![1, 2, 3, 4],
            ..rift_recall_doc::Markdown::default()
        }));
        let call = hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_of(&document, None, None),
        );

        let outcome = doc_gate_for(&call, &work_tree, &store, Some(&scanned));
        assert!(
            outcome.answer.is_none(),
            "a page image is not text, and Markdown that says so is worse than the page"
        );
        assert_eq!(scanned.calls(), 1);
        let outcome = doc_gate_for(&call, &work_tree, &store, Some(&scanned));
        assert!(outcome.answer.is_none());
        assert_eq!(
            scanned.calls(),
            1,
            "the decision against it is the family's, and it is not re-parsed"
        );

        // The same file, converted to real text, read with `pages`.
        let (tmp, document, store) = documents("report.pdf", b"%PDF-1.7 words");
        let work_tree = tmp.path().join("main");
        let converter = StandInConverter::markdown("# Report\n\nWords.\n");
        let mut input = serde_json::Map::new();
        input.insert(
            "file_path".to_owned(),
            serde_json::json!(document.display().to_string()),
        );
        input.insert("pages".to_owned(), serde_json::json!("1-5"));
        let paged = hook(
            "claude-code",
            "PreToolUse",
            "s-2",
            serde_json::json!({ "tool_name": "Read", "tool_input": input }),
        );
        let outcome = doc_gate_for(&paged, &work_tree, &store, Some(&converter));
        assert!(
            outcome.answer.is_none(),
            "a call that addresses pages is not a call the sidecar can answer"
        );
        assert_eq!(converter.calls(), 0, "and it is not even parsed");
    }

    /// A conversion that does not finish inside the budget leaves the read
    /// exactly as the agent wrote it and hands the work to the detached
    /// worker: the deadline is a bound on what an agent waits for, never a
    /// reason to answer with something that is not ready.
    #[cfg(feature = "doc")]
    #[test]
    fn a_conversion_over_the_deadline_leaves_the_read_alone_and_journals() {
        let (tmp, document, store) = documents("slow.docx", b"PK\x03\x04 a slow document");
        let work_tree = tmp.path().join("main");
        let timed_out = StandInConverter::answering(Err(rift_recall_doc::DocError::Engine {
            code: "deadline",
        }));
        let call = hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_of(&document, None, None),
        );

        let outcome = doc_gate_for(&call, &work_tree, &store, Some(&timed_out));
        assert!(outcome.answer.is_none(), "the read runs as it was written");
        assert!(
            outcome.journaled,
            "and the worker is spawned for what is owed"
        );
        let sha = rift_shadow::sha256_hex(&[std::fs::read(&document).unwrap().as_slice()]);
        let jobs = journaled(&work_tree);
        let owed: Vec<_> = jobs
            .iter()
            .filter_map(|job| match &job.kind {
                crate::agent_queue::JobKind::DocConvert {
                    rel_path,
                    content_sha256,
                } => Some((rel_path.clone(), content_sha256.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            owed,
            vec![("slow.docx".to_owned(), sha.clone())],
            "the conversion is journaled by the bytes it is about: {jobs:?}"
        );
        let booked: Vec<_> = jobs
            .iter()
            .filter_map(|job| match &job.kind {
                crate::agent_queue::JobKind::ServeAudit(audit) => Some(audit),
                _ => None,
            })
            .collect();
        assert_eq!(booked.len(), 1, "{jobs:?}");
        assert_eq!(
            booked[0].decision,
            crate::agent_queue::ServeAuditKind::DocDeferred
        );
        assert_eq!(booked[0].tag.as_deref(), Some("deadline"));
        assert!(
            booked[0].frame.is_empty(),
            "nothing was put in front of the model, so nothing cost"
        );
        assert_eq!(
            booked[0].basis.as_ref().map(|b| b.content_sha256.clone()),
            Some(sha),
            "the row names the document it is about"
        );
        // Nothing was decided against: the deadline says nothing about the
        // document, so the next pass converts it.
        assert!(!store.is_skipped(
            &crate::doc_store::sidecar_key(
                &booked[0].basis.as_ref().unwrap().content_sha256,
                &rift_recall_policy::DocPolicy::default()
            )
            .unwrap()
        ));
    }

    /// ★ A substitution that did not happen. The client applies the last
    /// amendment it is handed for one event, so a sibling hook amending the
    /// same call wins and Rift's path never reaches the tool. The read's own
    /// result is where that is found out — it carries the input the tool
    /// really ran with — and from there this window is answered by the
    /// decline, which nothing can overwrite: once, with the sidecar named, and
    /// the repeat allowed.
    #[cfg(feature = "doc")]
    #[test]
    fn a_dropped_rewrite_flips_the_session_to_redirect_and_the_next_document_read_is_redirected_once(
    ) {
        let (tmp, document, store) = documents("plan.docx", b"PK\x03\x04 the plan");
        let work_tree = tmp.path().join("main");
        let converter = StandInConverter::markdown("# Plan\n\nSteps.\n");
        let call = hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_of(&document, None, None),
        );

        let Some(DocAnswer::Rewrite { sidecar, .. }) =
            doc_gate_for(&call, &work_tree, &store, Some(&converter)).answer
        else {
            panic!("the first read is amended");
        };

        // The result comes back naming the document, not the sidecar: some
        // other hook's amendment is the one the client kept.
        let dropped = hook(
            "claude-code",
            "PostToolUse",
            "s-1",
            read_of(&document, None, None),
        );
        let verified = doc_gate_for(&dropped, &work_tree, &store, Some(&converter));
        assert!(verified.answer.is_none(), "a result is not answered");
        assert!(verified.journaled, "and the drop is recorded");
        let dropped_rows: Vec<_> = journaled(&work_tree)
            .into_iter()
            .filter_map(|job| match job.kind {
                crate::agent_queue::JobKind::ServeAudit(audit)
                    if audit.tag.as_deref() == Some("rewrite_dropped") =>
                {
                    Some(audit)
                }
                _ => None,
            })
            .collect();
        assert_eq!(dropped_rows.len(), 1, "recorded once");
        assert_eq!(
            dropped_rows[0].decision,
            crate::agent_queue::ServeAuditKind::DocDeferred
        );

        // The next document read of this window is declined, once, and the
        // sentence names where to read instead — and carries no content.
        let second = hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_of(&document, None, None),
        );
        let redirected = doc_gate_for(&second, &work_tree, &store, Some(&converter));
        let Some(DocAnswer::Redirect { reason }) = redirected.answer.clone() else {
            panic!("the window is answered by the decline now: {redirected:?}");
        };
        // Named the way this checkout names it, in full: a span inside a
        // pushed frame is bounded, and half a path is a wrong path.
        let here = sidecar_here(&work_tree, &sidecar);
        assert!(
            reason.contains(&here),
            "the frame names the sidecar: {reason}"
        );
        assert!(!reason.contains("Steps."), "and never the document's words");
        let response = rift_agent::respond::gate(
            &rift_core::AgentId::CLAUDE_CODE,
            "PreToolUse",
            &rift_agent::respond::GateDecision::Deny { reason: &reason },
        )
        .expect("the declined call");
        assert_eq!(
            response.pointer("/hookSpecificOutput/permissionDecision"),
            Some(&serde_json::Value::from("deny"))
        );
        assert_eq!(
            response.pointer("/hookSpecificOutput/permissionDecisionReason"),
            Some(&serde_json::Value::from(reason.as_str()))
        );
        assert!(
            response
                .pointer("/hookSpecificOutput/updatedInput")
                .is_none(),
            "a decline never carries an amendment"
        );

        // The repeat is the escape hatch the frame itself promises.
        let again = doc_gate_for(&second, &work_tree, &store, Some(&converter));
        assert!(
            again.answer.is_none(),
            "one decline per sidecar, and the repeated call runs"
        );

        // And nothing this window journaled asks for a declaration. The agent
        // read the container in full, so every row about it claims the zero it
        // avoided — the drop, and the decline that followed it.
        let rows = audits(&work_tree);
        assert!(
            rows.iter().all(|row| row.declared_avoided == Some(0)),
            "a document the agent really read is claimed by nobody: {rows:?}"
        );
    }

    /// ★ A substitution is claimed where it is proved, and only there.
    ///
    /// An amendment is a request to the client, not an outcome — the client
    /// applies the last one it is handed for an event, and a sibling hook can
    /// take the call — so the decision itself books nothing. The read's own
    /// result carries the input the tool really ran with: when that input
    /// names the sidecar, the read the agent wrote never happened and the one
    /// figure this channel may claim is journaled, once, over the document it
    /// stood in for. A second result for the same amendment, and a sidecar
    /// this window never armed, claim nothing.
    #[cfg(feature = "doc")]
    #[test]
    fn a_substitution_is_booked_only_when_the_executed_call_proves_it_and_only_once() {
        let (tmp, document, store) = documents("spec.docx", b"PK\x03\x04 the spec");
        let work_tree = tmp.path().join("main");
        let converter = StandInConverter::markdown("# Spec\n\nThe body.\n");
        let sha = rift_shadow::sha256_hex(&[&std::fs::read(&document).expect("the document")]);
        let call = hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_of(&document, None, None),
        );

        let decided = doc_gate_for(&call, &work_tree, &store, Some(&converter));
        let Some(DocAnswer::Rewrite { sidecar, .. }) = decided.answer else {
            panic!("the read is amended");
        };
        assert!(
            !decided.journaled && audits(&work_tree).is_empty(),
            "a decision the client may still overrule claims nothing"
        );

        // The result names the sidecar: the amendment survived to the tool.
        let executed = hook(
            "claude-code",
            "PostToolUse",
            "s-1",
            read_of(Path::new(&sidecar), None, None),
        );
        let verified = doc_gate_for(&executed, &work_tree, &store, Some(&converter));
        assert!(verified.answer.is_none(), "a result is not answered");
        assert!(verified.journaled, "and the substitution is booked here");
        let booked = audits(&work_tree);
        assert_eq!(booked.len(), 1, "{booked:?}");
        assert_eq!(
            booked[0].decision,
            crate::agent_queue::ServeAuditKind::DocRewritten
        );
        assert_eq!(
            booked[0].tag.as_deref(),
            Some(rift_recall_serve::TAG_DECLARED_BASELINE)
        );
        assert_eq!(
            booked[0].declared_avoided, None,
            "the figure is the worker's to count: a hook counts no tokens"
        );
        assert_eq!(
            booked[0]
                .basis
                .as_ref()
                .map(|b| (b.rel_path.clone(), b.content_sha256.clone())),
            Some(("spec.docx".to_owned(), sha)),
            "the row is about the document, under the bytes the amendment was \
             made over — never about the sidecar"
        );
        assert!(
            booked[0].frame.is_empty(),
            "a substitution puts no sentence in front of a model"
        );

        // The same result again, and the same sidecar read by a window that
        // was never amended: neither is a second read of the document.
        doc_gate_for(&executed, &work_tree, &store, Some(&converter));
        let stranger = hook(
            "claude-code",
            "PostToolUse",
            "s-2",
            read_of(Path::new(&sidecar), None, None),
        );
        doc_gate_for(&stranger, &work_tree, &store, Some(&converter));
        assert_eq!(
            audits(&work_tree).len(),
            1,
            "one amendment, one claim: {:?}",
            audits(&work_tree)
        );
    }

    /// The accounting rows a gate left the worker, in the order it wrote them.
    fn audits(work_tree: &Path) -> Vec<crate::agent_queue::ServeAudit> {
        journaled(work_tree)
            .into_iter()
            .filter_map(|job| match job.kind {
                crate::agent_queue::JobKind::ServeAudit(audit) => Some(audit),
                _ => None,
            })
            .collect()
    }

    /// A repository that would rather be told than amended is told, without
    /// anything having to be dropped first.
    #[cfg(feature = "doc")]
    #[test]
    fn a_repository_that_chose_the_redirect_is_never_amended() {
        let (tmp, document, store) = documents("notes.odt", b"PK\x03\x04 notes");
        let work_tree = tmp.path().join("main");
        let converter = StandInConverter::markdown("# Notes\n");
        let mut policy = rift_recall_policy::ServePolicy::default();
        policy.doc.channel = rift_recall_policy::DocChannel::Redirect;
        let effective = rift_recall_policy::resolve(
            &policy,
            "claude-code",
            crate::recall::family_capability(&rift_core::AgentId::CLAUDE_CODE),
        );
        assert_eq!(effective.doc_rewrite, rift_recall_policy::Mode::Off);
        let call = hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_of(&document, None, None),
        );
        let outcome = doc_gate_in(
            &crate::hooktime::HookTimer::new("test", false),
            &AdapterRegistry::with_builtin(),
            &call,
            &work_tree.join(".git"),
            &work_tree,
            &policy,
            &effective,
            &store,
            Some(&converter),
        );
        assert!(
            matches!(outcome.answer, Some(DocAnswer::Redirect { .. })),
            "{outcome:?}"
        );
    }

    /// The document levers as this repository ships them, for one family,
    /// with that family's own pinned capability resolved into them.
    #[cfg(feature = "doc")]
    fn document_levers_for(family: &str) -> rift_recall_policy::Effective {
        rift_recall_policy::resolve(
            &rift_recall_policy::ServePolicy::default(),
            family,
            crate::recall::family_capability(&rift_core::AgentId::new(family)),
        )
    }

    /// One document read through the gate under levers the test states.
    #[cfg(feature = "doc")]
    fn doc_gate_under(
        raw: &RawHook,
        work_tree: &Path,
        store: &crate::doc_store::DocStore,
        converter: Option<&dyn rift_recall_doc::DocConverter>,
        effective: &rift_recall_policy::Effective,
    ) -> DocOutcome {
        doc_gate_in(
            &crate::hooktime::HookTimer::new("test", false),
            &AdapterRegistry::with_builtin(),
            raw,
            &work_tree.join(".git"),
            work_tree,
            &rift_recall_policy::ServePolicy::default(),
            effective,
            store,
            converter,
        )
    }

    /// An Antigravity `view_file` of a whole file, nested the way that family
    /// nests arguments and carrying the two envelope keys its recorded runs
    /// put on every call.
    #[cfg(feature = "doc")]
    fn view_of_whole(path: &Path) -> serde_json::Value {
        serde_json::json!({"toolCall": {"name": "view_file", "args": {
            "AbsolutePath": path.display().to_string(),
            "toolAction": "Viewing the document",
            "toolSummary": "View the document",
        }}})
    }

    /// This member's own spelling of a sidecar, which is how a frame names it.
    #[cfg(feature = "doc")]
    fn sidecar_here(work_tree: &Path, sidecar: &str) -> String {
        rift_recall_substrate::worktree::repo_relative_slash(work_tree, Path::new(sidecar))
            .expect("the view is inside this member")
    }

    /// ★ **A Codex `cat` of a document is declined once, with the sidecar
    /// named.** This family's calls cannot be amended by anybody — its own
    /// published surface says so — so the channel that answers it is the
    /// decline its pinned row documents and a recorded run confirmed. The
    /// sentence names where the Markdown is, the response is the object that
    /// family is pinned to, and the repeat runs.
    #[cfg(feature = "doc")]
    #[test]
    fn a_codex_cat_of_a_docx_is_redirected_once_to_the_sidecar_when_confirmed() {
        let (tmp, document, store) = documents("spec.docx", b"PK\x03\x04 a document");
        let work_tree = tmp.path().join("main");
        let converter = StandInConverter::markdown("# Spec\n\nThe body.\n");
        let effective = document_levers_for("codex");
        assert_eq!(
            effective.doc_rewrite,
            rift_recall_policy::Mode::Off,
            "nothing amends a call this family will not have amended"
        );
        assert_eq!(effective.doc_redirect, rift_recall_policy::Mode::Enforce);
        let call = hook(
            "codex",
            "PreToolUse",
            "s-1",
            shell_of(&format!("cat {}", shell_spelling(&document))),
        );

        let outcome = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
        let Some(DocAnswer::Redirect { reason }) = outcome.answer.clone() else {
            panic!("a document read this family makes is declined: {outcome:?}");
        };
        assert_eq!(converter.calls(), 1, "converted once, for the family");
        let sidecar = std::fs::read_dir(work_tree.join(".rift/state/doc"))
            .expect("this member's views")
            .flatten()
            .find(|entry| entry.path().extension().is_some_and(|x| x == "md"))
            .expect("one view");
        let here = sidecar_here(&work_tree, &sidecar.path().to_string_lossy());
        assert!(
            reason.contains(&here),
            "the sentence names where to read instead: {reason}"
        );
        assert!(
            !reason.contains("The body."),
            "and never the document's own words: {reason}"
        );

        // The response, composed where every response is composed: this
        // family's own declined envelope, and no amendment in it.
        let response = rift_agent::respond::gate(
            &rift_core::AgentId::CODEX,
            "PreToolUse",
            &rift_agent::respond::GateDecision::Deny { reason: &reason },
        )
        .expect("the run recorded in this family's fixture confirmed the channel");
        assert_eq!(
            response.pointer("/hookSpecificOutput/permissionDecision"),
            Some(&serde_json::Value::from("deny"))
        );
        assert_eq!(
            response.pointer("/hookSpecificOutput/permissionDecisionReason"),
            Some(&serde_json::Value::from(reason.as_str()))
        );
        assert!(
            response
                .pointer("/hookSpecificOutput/updatedInput")
                .is_none(),
            "a decline never carries an amendment: {response}"
        );

        // One decline per sidecar: the repeat is the escape hatch the sentence
        // itself promises, and it costs no second conversion.
        let again = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
        assert!(again.answer.is_none(), "the repeated call runs: {again:?}");
        assert_eq!(converter.calls(), 1);
    }

    /// ★ **An Antigravity `view_file` of a document is declined once, with the
    /// sidecar named.** The same channel and the same once-per-sidecar rule as
    /// the family beside it, over a call whose arguments are nested and carry
    /// the client's own envelope keys — which are read past, because they say
    /// nothing about which part of the file was asked for.
    #[cfg(feature = "doc")]
    #[test]
    fn a_view_file_of_a_docx_is_redirected_once_to_the_sidecar_when_confirmed() {
        let (tmp, document, store) = documents("brief.docx", b"PK\x03\x04 a brief");
        let work_tree = tmp.path().join("main");
        let converter = StandInConverter::markdown("# Brief\n\nThe brief.\n");
        let effective = document_levers_for("antigravity");
        assert_eq!(effective.doc_rewrite, rift_recall_policy::Mode::Off);
        assert_eq!(effective.doc_redirect, rift_recall_policy::Mode::Enforce);
        let call = hook("antigravity", "PreToolUse", "s-1", view_of_whole(&document));

        let outcome = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
        let Some(DocAnswer::Redirect { reason }) = outcome.answer.clone() else {
            panic!("a document read this family makes is declined: {outcome:?}");
        };
        let sidecar = std::fs::read_dir(work_tree.join(".rift/state/doc"))
            .expect("this member's views")
            .flatten()
            .find(|entry| entry.path().extension().is_some_and(|x| x == "md"))
            .expect("one view");
        assert!(
            reason.contains(&sidecar_here(&work_tree, &sidecar.path().to_string_lossy())),
            "the sentence names where to read instead: {reason}"
        );
        let response = rift_agent::respond::gate(
            &rift_core::AgentId::ANTIGRAVITY,
            "PreToolUse",
            &rift_agent::respond::GateDecision::Deny { reason: &reason },
        )
        .expect("the runs recorded in this family's fixture confirmed the channel");
        assert_eq!(
            response.get("decision"),
            Some(&serde_json::Value::from("deny"))
        );
        assert_eq!(
            response.get("reason"),
            Some(&serde_json::Value::from(reason.as_str()))
        );
        assert!(
            response.get("updatedInput").is_none(),
            "a decline never carries an amendment: {response}"
        );

        // A view that addresses lines is not a whole-document read, so the
        // sidecar cannot answer it and the call is left alone.
        let mut ranged = view_of_whole(&document);
        ranged["toolCall"]["args"]["StartLine"] = serde_json::json!(1);
        let ranged = hook("antigravity", "PreToolUse", "s-2", ranged);
        let outcome = doc_gate_under(&ranged, &work_tree, &store, Some(&converter), &effective);
        assert!(
            outcome.answer.is_none(),
            "a call that addresses lines is not a call the sidecar can answer: {outcome:?}"
        );
    }

    /// **A family whose reason channel no recorded run has confirmed is never
    /// declined on — and still gets its sidecars.** The two halves are one
    /// point: conversion is what makes the answer exist for every reader of
    /// the family, and speaking to a model is a separate permission that this
    /// family has not earned. A decline it could not tell from a tool failure
    /// is one it would be right to retry.
    #[cfg(feature = "doc")]
    #[test]
    fn an_unconfirmed_family_is_never_denied_and_still_has_sidecars() {
        let (tmp, document, store) = documents("draft.docx", b"PK\x03\x04 a draft");
        let work_tree = tmp.path().join("main");
        let converter = StandInConverter::markdown("# Draft\n\nThe draft.\n");
        // The capability an unconfirmed row resolves to, held against the
        // pinned one so this states the difference rather than a whole table.
        let unconfirmed = rift_recall_policy::FamilyCapability {
            confirmed_reason_channel: false,
            ..crate::recall::family_capability(&rift_core::AgentId::CODEX)
        };
        let effective = rift_recall_policy::resolve(
            &rift_recall_policy::ServePolicy::default(),
            "codex",
            unconfirmed,
        );
        assert_eq!(
            effective.doc_redirect,
            rift_recall_policy::Mode::Observe,
            "the lever is capped by the channel, not by the repository"
        );
        assert_eq!(
            effective.doc_redirect_cap,
            Some(rift_recall_policy::CapReason::UnconfirmedReasonChannel)
        );
        let call = hook(
            "codex",
            "PreToolUse",
            "s-1",
            shell_of(&format!("cat {}", shell_spelling(&document))),
        );

        let outcome = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
        assert!(
            outcome.answer.is_none(),
            "nothing is declined on a channel nobody has seen a model read: {outcome:?}"
        );
        // And the answer exists anyway: the family store holds it and this
        // member has its view, so the pull verb and every other member are
        // served by this read having happened.
        assert_eq!(converter.calls(), 1);
        let views: Vec<_> = std::fs::read_dir(work_tree.join(".rift/state/doc"))
            .expect("this member's views")
            .flatten()
            .filter(|entry| entry.path().extension().is_some_and(|x| x == "md"))
            .collect();
        assert_eq!(views.len(), 1, "the sidecar is here");
        assert!(
            std::fs::read_to_string(views[0].path())
                .expect("the view")
                .contains("The draft."),
            "and it is the document as Markdown"
        );
    }

    /// **A lever in `observe` decides and records; it never declines.** The
    /// row it books is the size of the pool the lever would work on — one per
    /// document, not one per read of it — and it carries no frame, because
    /// nothing was put in front of a model.
    #[cfg(feature = "doc")]
    #[test]
    fn an_observed_document_channel_records_the_decline_it_did_not_make() {
        let (tmp, document, store) = documents("plan.odt", b"PK\x03\x04 a plan");
        let work_tree = tmp.path().join("main");
        let converter = StandInConverter::markdown("# Plan\n\nSteps.\n");
        let mut policy = rift_recall_policy::ServePolicy::default();
        policy.doc.mode = rift_recall_policy::Mode::Observe;
        let effective = rift_recall_policy::resolve(
            &policy,
            "codex",
            crate::recall::family_capability(&rift_core::AgentId::CODEX),
        );
        assert_eq!(effective.doc_redirect, rift_recall_policy::Mode::Observe);
        let call = hook(
            "codex",
            "PreToolUse",
            "s-1",
            shell_of(&format!("cat {}", shell_spelling(&document))),
        );

        let outcome = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
        assert!(outcome.answer.is_none(), "observe declines nothing");
        assert!(outcome.journaled, "and records what it decided");
        let observed: Vec<_> = journaled(&work_tree)
            .into_iter()
            .filter_map(|job| match job.kind {
                crate::agent_queue::JobKind::ServeAudit(audit)
                    if audit.tag.as_deref() == Some("redirect_observed") =>
                {
                    Some(audit)
                }
                _ => None,
            })
            .collect();
        assert_eq!(observed.len(), 1);
        assert!(
            observed[0].frame.is_empty(),
            "a sentence nobody read costs nothing"
        );
        assert_eq!(observed[0].declared_avoided, Some(0));

        // The pool is documents, not reads: the same document read again in
        // the same window is the same one decision.
        let again = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
        assert!(again.answer.is_none());
        assert!(!again.journaled, "one row per sidecar, as in enforce");
    }

    /// **A command line the strict grammar does not spell is not a document
    /// read.** A pipeline is not a single command; `sed` and `head` deliver
    /// part of a file, which is exactly what a whole-document answer cannot
    /// stand in for; and a command that reads names has read no file at all.
    /// Each of them leaves the call as the agent wrote it.
    #[cfg(feature = "doc")]
    #[test]
    fn a_command_line_outside_the_grammar_is_never_a_document_read() {
        let (tmp, document, store) = documents("sheet.csv", b"a,b\n1,2\n");
        let work_tree = tmp.path().join("main");
        let converter = StandInConverter::markdown("# Sheet\n");
        let effective = document_levers_for("codex");
        let named = document.display().to_string();
        for command in [
            format!("cat {named} | head -n 5"),
            format!("cat {named} > copy.csv"),
            format!("head -n 5 {named}"),
            format!("sed -n '1,5p' {named}"),
            format!("tail {named}"),
            format!("nl {named}"),
            format!("cat {named} {named}"),
            format!("ls {}", work_tree.display()),
        ] {
            let call = hook("codex", "PreToolUse", "s-1", shell_of(&command));
            let outcome = doc_gate_under(&call, &work_tree, &store, Some(&converter), &effective);
            assert!(
                outcome.answer.is_none(),
                "`{command}` is not a whole-document read: {outcome:?}"
            );
        }
        assert_eq!(
            converter.calls(),
            0,
            "and none of them spent the conversion budget"
        );
    }

    /// A document too large to hash while an agent waits is not keyed on this
    /// path at all: the read runs as it was written and the bounded pass is
    /// asked for, so the answer exists for the next reader without this hook
    /// having spent the budget to produce it.
    ///
    /// The ceiling is the read gate's own — what a hook may read and hash
    /// inline — and not the converter's, which is what will be parsed at all.
    #[cfg(feature = "doc")]
    #[test]
    fn a_document_too_large_to_hash_on_the_hook_path_is_left_to_the_worker() {
        let (policy, effective) = document_levers();
        let over = usize::try_from(effective.max_inline_hash_bytes).unwrap() + 1;
        assert!(
            u64::try_from(over).unwrap() <= effective.max_input_bytes,
            "the fixture is inside what the converter would parse, so the only \
             thing refusing it is the hook's own hashing ceiling"
        );
        let (tmp, document, store) = documents("huge.docx", &vec![b'z'; over]);
        let work_tree = tmp.path().join("main");
        let converter = StandInConverter::markdown("# Huge\n");
        let call = hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_of(&document, None, None),
        );

        let outcome = doc_gate_in(
            &crate::hooktime::HookTimer::new("test", false),
            &AdapterRegistry::with_builtin(),
            &call,
            &work_tree.join(".git"),
            &work_tree,
            &policy,
            &effective,
            &store,
            Some(&converter),
        );
        assert!(outcome.answer.is_none(), "the read runs as it was written");
        assert!(outcome.journaled, "and the pass is asked for");
        assert_eq!(converter.calls(), 0, "nothing was parsed on this path");
        let asked: Vec<_> = journaled(&work_tree)
            .into_iter()
            .filter(|job| matches!(job.kind, crate::agent_queue::JobKind::DocSweep))
            .collect();
        assert_eq!(asked.len(), 1, "one pass, asked for once");
    }

    // -------------------------------------------- the basis snapshot ---

    /// A work tree with a real git dir and one file in it.
    fn snapshot_fixture(rel_path: &str, body: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        rift_git::test_support::fake_repo(root).unwrap();
        if let Some(parent) = Path::new(rel_path).parent() {
            std::fs::create_dir_all(root.join(parent)).unwrap();
        }
        std::fs::write(root.join(rel_path), body).unwrap();
        tmp
    }

    fn snapshot_job(work_tree: &Path, rel_path: &str, body: &str) -> crate::agent_queue::AgentJob {
        crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: 1_700_000_000_000,
            attempts: 0,
            kind: crate::agent_queue::JobKind::BasisSnapshot {
                rel_path: rel_path.to_owned(),
                content_sha256: rift_recall_substrate::worktree::sha256_hex(body.as_bytes()),
                blob_oid: rift_recall_substrate::worktree::git_blob_oid(body.as_bytes()),
            },
        }
    }

    fn basis_store_of(work_tree: &Path) -> rift_recall_serve::basis_store::BasisStore {
        let family = crate::family::try_at(work_tree).expect("a minted family");
        rift_recall_serve::basis_store::BasisStore::for_family(&family.common_dir)
    }

    fn run_snapshot(job: &crate::agent_queue::AgentJob) -> Result<()> {
        let crate::agent_queue::JobKind::BasisSnapshot {
            rel_path,
            content_sha256,
            blob_oid,
        } = &job.kind
        else {
            panic!("a snapshot job");
        };
        snapshot_basis_job(
            &crate::hooktime::HookTimer::from_env("test"),
            job,
            rel_path,
            content_sha256,
            blob_oid,
        )
    }

    /// ★ The worker keeps the bytes a read recorded, under the read's own
    /// identity and in the family's store — which is what gives an
    /// **uncommitted** revision a basis at all.
    #[test]
    fn the_worker_keeps_a_revision_a_read_recorded() {
        let body = "alpha\nbravo\ncharlie\n";
        let tmp = snapshot_fixture("src/a.rs", body);
        let work_tree = tmp.path();
        let job = snapshot_job(work_tree, "src/a.rs", body);
        run_snapshot(&job).expect("the snapshot lands");

        let store = basis_store_of(work_tree);
        let key = rift_recall_substrate::worktree::sha256_hex(body.as_bytes());
        let held = store.get_for_path("src/a.rs", &key).expect("the snapshot");
        assert_eq!(held.bytes, body.as_bytes());
        assert!(held.redacted_lines.is_empty(), "nothing was masked");

        // The working file moves on — the revision is now reachable nowhere
        // else, which is the whole case this store exists for.
        std::fs::write(work_tree.join("src/a.rs"), "alpha\nBRAVO\ncharlie\n").unwrap();
        assert!(
            store.get_for_path("src/a.rs", &key).is_some(),
            "the basis outlives the edit that made it interesting"
        );
    }

    /// ★ Nothing durable here skipped the gate: a secret in the bytes a read
    /// delivered is masked in the snapshot, and the line it was on is
    /// recorded so no diff can name it.
    #[test]
    fn a_secret_in_a_recorded_read_is_masked_before_it_is_kept() {
        let secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyzAB";
        let body = format!("fn main() {{}}\nconst TOKEN: &str = \"{secret}\";\nfn other() {{}}\n");
        let tmp = snapshot_fixture("src/a.rs", &body);
        let work_tree = tmp.path();
        run_snapshot(&snapshot_job(work_tree, "src/a.rs", &body)).expect("the snapshot lands");

        let store = basis_store_of(work_tree);
        let key = rift_recall_substrate::worktree::sha256_hex(body.as_bytes());
        let held = store.get_for_path("src/a.rs", &key).expect("the snapshot");
        let text = String::from_utf8(held.bytes).expect("text");
        assert!(!text.contains(secret), "the raw secret is not durable");
        assert_eq!(
            held.redacted_lines,
            vec![(2, 2)],
            "the masked line is recorded so nothing names it as a change"
        );
        assert!(!held.policy_fingerprint.is_empty(), "under a named policy");
    }

    /// A path the repository calls sensitive is never re-encoded here, exactly
    /// as it is never converted into a document sidecar. The job succeeds and
    /// records nothing: a saving forgone, never a failure.
    #[test]
    fn a_sensitive_path_is_never_snapshotted() {
        let body = "KEY=1\n";
        let tmp = snapshot_fixture("secrets/.env", body);
        let work_tree = tmp.path();
        std::fs::create_dir_all(work_tree.join(".rift")).unwrap();
        std::fs::write(
            work_tree.join(".rift/config.toml"),
            "schema_version = 1\n[project]\nname = \"t\"\n",
        )
        .unwrap();
        run_snapshot(&snapshot_job(work_tree, "secrets/.env", body)).expect("no failure");
        assert_eq!(basis_store_of(work_tree).survey().snapshots, 0);
    }

    /// Bytes that match neither the working file nor a blob are not kept under
    /// a guessed identity — the same ladder, and the same refusal, the
    /// deferred token count lives by.
    #[test]
    fn bytes_that_match_no_source_are_not_kept() {
        let tmp = snapshot_fixture("src/a.rs", "alpha\n");
        let work_tree = tmp.path();
        // A job naming a revision nothing here has.
        let job = snapshot_job(work_tree, "src/a.rs", "a revision nobody wrote\n");
        assert!(
            run_snapshot(&job).is_err(),
            "said once, retried, never faked"
        );
        assert_eq!(basis_store_of(work_tree).survey().snapshots, 0);
    }

    /// One revision read many times owes one snapshot, keyed by the revision
    /// rather than by the read.
    #[test]
    fn many_reads_of_one_revision_owe_one_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        let read = |call: &str, sha: &str| rift_shadow::RecordedRead {
            tool_call_id: call.to_owned(),
            rel_path: "src/a.rs".into(),
            blob_oid: "ee".repeat(20),
            content_sha256: sha.to_owned(),
            tokens_recorded: true,
            lines: None,
            schema_version: 1,
        };
        let same = "aa".repeat(32);
        let other = "bb".repeat(32);
        let owed = owe_basis_snapshots(
            &git_dir,
            tmp.path(),
            &[
                read(&"11".repeat(32), &same),
                read(&"22".repeat(32), &same),
                read(&"33".repeat(32), &other),
            ],
        );
        assert_eq!(owed, 2, "two revisions, three reads");
        let queued = crate::agent_queue::jobs(&git_dir)
            .into_iter()
            .filter(|(_, job)| {
                matches!(job.kind, crate::agent_queue::JobKind::BasisSnapshot { .. })
            })
            .count();
        assert_eq!(queued, 2);
    }

    // ------------------------------------------------ the search index ---

    /// A work tree with a real git dir, a source file and a secret.
    #[cfg(feature = "search")]
    fn indexable(mode: &str) -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        rift_git::test_support::fake_repo(root).unwrap();
        std::fs::create_dir_all(root.join(".rift/policies")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/bus.rs"), "fn main() {}\n").unwrap();
        std::fs::write(root.join(".gitignore"), "ignored\n").unwrap();
        std::fs::write(root.join("ignored"), "not a path to answer with\n").unwrap();
        std::fs::write(
            root.join(".rift/config.toml"),
            "schema_version = 1\n[project]\nname = \"t\"\n[paths]\nsensitive_paths = [\"src/secret.rs\"]\n",
        )
        .unwrap();
        std::fs::write(root.join("src/secret.rs"), "const KEY: &str = \"x\";\n").unwrap();
        std::fs::write(
            root.join(".rift/policies/serve.toml"),
            format!("[search]\nmode = \"{mode}\"\n"),
        )
        .unwrap();
        tmp
    }

    #[cfg(feature = "search")]
    fn index_job(work_tree: &Path) -> crate::agent_queue::AgentJob {
        crate::agent_queue::AgentJob {
            schema_version: 1,
            work_tree: work_tree.to_path_buf(),
            queued_at_ms: 0,
            attempts: 0,
            kind: crate::agent_queue::JobKind::SearchIndex,
        }
    }

    /// The worker's pass writes the index the next query loads, over the paths
    /// git would list and never over one the repository calls sensitive.
    #[cfg(feature = "search")]
    #[test]
    fn the_worker_builds_an_index_of_the_paths_this_checkout_may_answer_with() {
        let tmp = indexable("enforce");
        let root = tmp.path();
        rebuild_search_index_job(
            &crate::hooktime::HookTimer::new("test", false),
            &index_job(root),
        )
        .unwrap();

        let index = rift_recall_search::PathIndex::load(
            &rift_recall_search::index_path(root),
            &rift_recall_search::walk::current_key(root, &rift_recall_search::head_oid(root)),
        )
        .expect("the pass left an index the next query can load");
        let paths: Vec<&str> = index.entries().iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"src/bus.rs"));
        assert!(!paths.contains(&"src/secret.rs"), "sensitive");
        assert!(!paths.contains(&"ignored"), "gitignored");
        assert!(
            !paths.iter().any(|path| path.starts_with(".rift/state")),
            "rift's own run state is not searchable: {paths:?}"
        );
    }

    /// A repository that turned the lever off gets no index written: the pass
    /// is journaled and drained, and it does nothing.
    #[cfg(feature = "search")]
    #[test]
    fn a_repository_with_search_off_is_not_indexed() {
        let tmp = indexable("off");
        rebuild_search_index_job(
            &crate::hooktime::HookTimer::new("test", false),
            &index_job(tmp.path()),
        )
        .unwrap();
        assert!(!rift_recall_search::index_path(tmp.path()).exists());
    }

    /// However many hooks ask for a rebuild, one rebuild is owed.
    #[test]
    fn a_rebuild_asked_for_by_three_hooks_is_one_job() {
        let tmp = tempfile::tempdir().unwrap();
        let git_dir = tmp.path().join(".git");
        std::fs::create_dir_all(&git_dir).unwrap();
        for _ in 0..3 {
            assert!(owe_search_index(&git_dir, tmp.path()));
        }
        let asked: Vec<_> = crate::agent_queue::jobs(&git_dir)
            .into_iter()
            .filter(|(_, job)| matches!(job.kind, crate::agent_queue::JobKind::SearchIndex))
            .collect();
        assert_eq!(asked.len(), 1);
    }

    /// The ordinary case: a bounded read of a file in this checkout leaves one
    /// line saying which revision, which lines, and how long the file is.
    #[test]
    fn a_bounded_read_of_this_checkout_is_recorded_as_the_lines_it_delivers() {
        let (tmp, file) = checkout(120);
        gate(
            &hook(
                "claude-code",
                "PreToolUse",
                "s-1",
                read_of(&file, Some(10), Some(20)),
            ),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-1", None);
        let [Line::Read {
            turn,
            path,
            range,
            total_lines,
            ..
        }] = lines.as_slice()
        else {
            panic!("one read line, got {lines:?}");
        };
        assert_eq!(*turn, 4);
        assert_eq!(
            path, "src.rs",
            "the checkout's own spelling, not the payload's"
        );
        assert_eq!(*range, Interval::new(10, 29).expect("a run"));
        assert_eq!(*total_lines, 120);
    }

    /// The pin this family's truncation reading rests on, included at compile
    /// time so a renamed or deleted fixture is a build failure rather than a
    /// test that quietly stops checking anything.
    const CLAUDE_CODE_TOOL_FIELDS: &str = include_str!(
        "../../../../crates/rift-agent-claude-code/fixtures/claude-code-tool-field-surface.md"
    );

    /// **Every string this gate reads a delivery out of is a string the pin
    /// quotes.** A client that renames the field or rewords the notice moves
    /// the fixture first, and the fixture moving without the code — or the
    /// code without the fixture — reds here.
    #[test]
    fn the_delivery_reading_is_spelled_the_way_the_pin_spells_it() {
        for quoted in [
            TRUNCATED_BY_TOKEN_CAP,
            DELIVERY_START_LINE,
            DELIVERY_NUM_LINES,
            DELIVERY_TOTAL_LINES,
            TRUNCATION_NOTICE,
            TRUNCATION_NOTICE_LINES,
            PERSISTED_OUTPUT_PATH,
            PERSISTED_OUTPUT_MARKER,
        ] {
            assert!(
                CLAUDE_CODE_TOOL_FIELDS.contains(quoted),
                "the pin does not carry {quoted:?}; re-pin the surface before \
                 reading it, never the other way round"
            );
        }
    }

    /// A `PostToolUse` result for a read, with the truncation the pinned
    /// object states and the delivery it names.
    fn result_of(
        path: &Path,
        offset: u32,
        limit: u32,
        file: serde_json::Value,
    ) -> serde_json::Value {
        let mut payload = read_of(path, Some(offset), Some(limit));
        payload["tool_response"] = serde_json::json!({"type": "text", "file": file});
        payload
    }

    /// ★ **A truncated read grants no coverage past what was delivered.**
    ///
    /// The call asked for the whole file and the client handed back forty
    /// lines of it and said so. The window is credited with the forty and with
    /// nothing after them, so the range it did not receive is a range a later
    /// decline cannot rest on — which is the one direction of error a decline
    /// does not survive.
    #[test]
    fn a_truncated_read_grants_no_coverage_past_what_was_delivered() {
        let (tmp, file) = checkout(120);
        let asked = hook(
            "claude-code",
            "PreToolUse",
            "s-1",
            read_of(&file, Some(1), Some(120)),
        );
        gate(&asked, tmp.path());
        assert_eq!(
            lines_of(tmp.path(), "s-1", None).first().map(Line::kind),
            Some("read"),
            "the call is recorded before it runs, at the range it asked for"
        );

        gate(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-1",
                result_of(
                    &file,
                    1,
                    120,
                    serde_json::json!({
                        "filePath": file.display().to_string(),
                        "startLine": 1,
                        "numLines": 40,
                        "totalLines": 120,
                        "truncatedByTokenCap": true,
                    }),
                ),
            ),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-1", None);
        assert_eq!(
            lines.iter().map(Line::kind).collect::<Vec<_>>(),
            vec!["read", "evict", "read"],
            "the claim is taken back and the delivery written again: {lines:?}"
        );
        let journal = lines_journal(tmp.path(), "s-1");
        let [Line::Read { path, hash, .. }] = journal.live() else {
            panic!("one live read, got {:?}", journal.live());
        };
        let coverage = journal.coverage(path, *hash);
        assert!(
            coverage.contains(Interval::new(1, 40).expect("a run")),
            "the lines the result says arrived are held"
        );
        assert!(
            !coverage.contains(Interval::new(41, 41).expect("a run")),
            "the first line past the delivery is not"
        );
        assert!(
            !coverage.contains(Interval::new(1, 41).expect("a run")),
            "and neither is any run reaching past it"
        );
    }

    /// A result that says nothing about being cut short says nothing at all:
    /// absence is silence, never a statement that the whole range arrived, and
    /// a journal that took a line back on silence would forgo savings on every
    /// ordinary read.
    #[test]
    fn a_result_that_states_no_truncation_takes_nothing_back() {
        let (tmp, file) = checkout(120);
        gate(
            &hook(
                "claude-code",
                "PreToolUse",
                "s-1",
                read_of(&file, Some(1), Some(120)),
            ),
            tmp.path(),
        );
        gate(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-1",
                result_of(
                    &file,
                    1,
                    120,
                    serde_json::json!({
                        "filePath": file.display().to_string(),
                        "startLine": 1,
                        "numLines": 40,
                        "totalLines": 120,
                    }),
                ),
            ),
            tmp.path(),
        );
        assert_eq!(
            lines_of(tmp.path(), "s-1", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read"],
            "no flag, no notice, nothing to correct"
        );
    }

    /// A truncation that names no delivery leaves the read holding the one
    /// line it started on — the same answer an unpinned default window gets,
    /// and the smallest claim there is.
    #[test]
    fn a_truncation_with_no_stated_delivery_keeps_only_the_line_it_started_on() {
        let (tmp, file) = checkout(120);
        gate(
            &hook(
                "claude-code",
                "PreToolUse",
                "s-1",
                read_of(&file, Some(10), Some(60)),
            ),
            tmp.path(),
        );
        gate(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-1",
                result_of(
                    &file,
                    10,
                    60,
                    serde_json::json!({
                        "filePath": file.display().to_string(),
                        "truncatedByTokenCap": true,
                    }),
                ),
            ),
            tmp.path(),
        );
        let journal = lines_journal(tmp.path(), "s-1");
        let [Line::Read { range, .. }] = journal.live() else {
            panic!("one live read, got {:?}", journal.live());
        };
        assert_eq!(*range, Interval::new(10, 10).expect("a run"));
    }

    /// The notice is read where a result reaches the hook as text rather than
    /// as the object the pin quotes — the older client's shape, and the one a
    /// reader looking only for the flag would miss.
    #[test]
    fn the_truncation_notice_is_read_when_the_object_is_not() {
        let (tmp, file) = checkout(120);
        gate(
            &hook(
                "claude-code",
                "PreToolUse",
                "s-1",
                read_of(&file, Some(1), Some(120)),
            ),
            tmp.path(),
        );
        let mut payload = read_of(&file, Some(1), Some(120));
        payload["tool_response"] = serde_json::json!(
            "<system-reminder>[Truncated: PARTIAL view — showing lines 1-12 of 120 total \
             (27006 tokens, cap 25000). Call Read with offset=13 limit=12 for the next page, \
             or Grep to find a specific section.]</system-reminder>\n\n1\tline 1\n"
        );
        gate(
            &hook("claude-code", "PostToolUse", "s-1", payload),
            tmp.path(),
        );
        let journal = lines_journal(tmp.path(), "s-1");
        let [Line::Read { range, .. }] = journal.live() else {
            panic!("one live read, got {:?}", journal.live());
        };
        assert_eq!(
            *range,
            Interval::new(1, 12).expect("a run"),
            "the sentence names the delivery and the delivery is what is kept"
        );
    }

    /// A command result the client kept in a file takes back everything past
    /// the line the read started on, because none of the output arrived as
    /// output — the model was handed a path and a preview.
    ///
    /// Driven at a slice the size rule cannot catch: the file is small enough
    /// that a shell read of it is recorded whole, and the client says it
    /// spilled anyway. That is the case this backstop exists for; the size
    /// rule alone would have left the claim standing.
    #[test]
    fn a_command_result_kept_in_a_file_delivers_no_lines_at_all() {
        let (tmp, file) = checkout(120);
        let command = |response: Option<serde_json::Value>| {
            let mut payload = serde_json::json!({
                "tool_name": "Bash",
                "tool_input": {"command": format!("head -n 120 {}", shell_spelling(&file))},
            });
            if let Some(response) = response {
                payload["tool_response"] = response;
            }
            payload
        };
        gate(
            &hook("claude-code", "PreToolUse", "s-1", command(None)),
            tmp.path(),
        );
        let before = lines_journal(tmp.path(), "s-1");
        let [Line::Read { range, .. }] = before.live() else {
            panic!("one live read, got {:?}", before.live());
        };
        assert_eq!(
            *range,
            Interval::new(1, 120).expect("a run"),
            "a 120-line command output is under every ceiling, so it is held whole"
        );
        gate(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-1",
                command(Some(serde_json::json!({
                    "stdout": "line 1\n",
                    "stderr": "",
                    "interrupted": false,
                    "isImage": false,
                    "persistedOutputPath": "/tmp/tool-results/bawxzim8j.txt",
                    "persistedOutputSize": 31_000,
                }))),
            ),
            tmp.path(),
        );
        let journal = lines_journal(tmp.path(), "s-1");
        let [Line::Read { range, .. }] = journal.live() else {
            panic!(
                "one live read after the take-back, got {:?}",
                journal.live()
            );
        };
        assert_eq!(
            *range,
            Interval::new(1, 1).expect("a run"),
            "a pointer is not the lines, however many the command named"
        );
    }

    /// The same statement as the envelope the model reads, for a result that
    /// arrives as text — and a command that merely *prints* the word is not
    /// the client having said it.
    #[test]
    fn the_persisted_envelope_is_read_at_the_front_of_a_text_result_only() {
        let file = std::path::Path::new("/nowhere/src.rs");
        let text = |body: &str| {
            let mut payload = read_of(file, Some(1), Some(120));
            payload["tool_response"] = serde_json::json!(body);
            payload
        };
        assert_eq!(
            stated_delivery(
                &rift_core::AgentId::CLAUDE_CODE,
                &text("<persisted-output>\nOutput too large (30.3KB). Full output saved to: /t")
            ),
            Some(StatedDelivery::Unstated)
        );
        assert_eq!(
            stated_delivery(
                &rift_core::AgentId::CLAUDE_CODE,
                &text("the sentinel <persisted-output> appears in this file's own text")
            ),
            None,
            "a result that quotes the word said nothing about itself"
        );
    }

    /// **A family whose result shape is not pinned is not read for one.** The
    /// same object on another family's hook states nothing this gate knows how
    /// to read, and nothing about that family's recording changes.
    #[test]
    fn a_family_with_no_pinned_result_shape_is_left_as_it_was() {
        let file = std::path::Path::new("/nowhere/src.rs");
        let object = serde_json::json!({
            "type": "text",
            "file": {"startLine": 1, "numLines": 40, "truncatedByTokenCap": true},
        });
        for family in ["codex", "antigravity"] {
            let mut payload = read_of(file, Some(1), Some(120));
            payload["tool_response"] = object.clone();
            assert_eq!(
                stated_delivery(&rift_core::AgentId::new(family), &payload),
                None,
                "{family} publishes no truncation statement any run has shown"
            );
        }
        let mut payload = read_of(file, Some(1), Some(120));
        payload["tool_response"] = object;
        assert_eq!(
            stated_delivery(&rift_core::AgentId::CLAUDE_CODE, &payload),
            Some(StatedDelivery::Truncated(
                Interval::new(1, 40).expect("a run")
            )),
            "and the family whose shape is pinned is read"
        );
    }

    /// **The three readings of a result's own delivery, and the absence that
    /// is none of them.**
    ///
    /// Two fields and a count, read exactly as O5 and O7 pin them: the flag
    /// set is a truncation, the flag cleared is the client's word that the
    /// call was answered in full, and a run reaching the client's own
    /// `totalLines` is the same word in the shape O7 actually records — an
    /// untruncated result carries no flag at all. A run stopping short of
    /// `totalLines` with no flag beside it says nothing, and is read as
    /// nothing rather than as either.
    #[test]
    fn a_delivery_statement_is_read_as_the_pin_states_it_and_absence_as_nothing() {
        let path = std::path::Path::new("/nowhere/src.rs");
        let reading = |file: serde_json::Value| {
            let mut payload = read_of(path, None, None);
            payload["tool_response"] = serde_json::json!({"type": "text", "file": file});
            stated_delivery(&rift_core::AgentId::CLAUDE_CODE, &payload)
        };
        let run = |lo, hi| Interval::new(lo, hi).expect("a run");

        assert_eq!(
            reading(serde_json::json!({
                "startLine": 1, "numLines": 40, "totalLines": 900,
                "truncatedByTokenCap": true,
            })),
            Some(StatedDelivery::Truncated(run(1, 40))),
            "the flag set names how far the delivery got"
        );
        assert_eq!(
            reading(serde_json::json!({
                "startLine": 1, "numLines": 40, "totalLines": 900,
                "truncatedByTokenCap": false,
            })),
            Some(StatedDelivery::Complete(run(1, 40))),
            "the flag cleared is the client's own word for a full answer"
        );
        assert_eq!(
            reading(serde_json::json!({
                "startLine": 1, "numLines": 901, "totalLines": 901,
            })),
            Some(StatedDelivery::Complete(run(1, 901))),
            "and so is a run reaching the file's own length with no flag at all"
        );
        assert_eq!(
            reading(serde_json::json!({
                "startLine": 1, "numLines": 40, "totalLines": 900,
            })),
            None,
            "a run stopping short of it with no flag beside it says nothing"
        );
        assert_eq!(
            reading(serde_json::json!({"filePath": "/nowhere/src.rs"})),
            None,
            "and a result carrying none of the fields says nothing either"
        );
    }

    /// A `PostToolUse` result for a read that named no range at all, carrying
    /// the three fields the pinned object states about its own delivery.
    ///
    /// `flag` is what `truncatedByTokenCap` is set to, or nothing at all —
    /// which is the shape O7 records for an untruncated read, and the reason
    /// the delivered run has to be read together with `totalLines` rather than
    /// the flag alone.
    fn whole_result_of(
        path: &Path,
        delivered: u32,
        total: u32,
        flag: Option<bool>,
    ) -> serde_json::Value {
        let mut file = serde_json::json!({
            "filePath": path.display().to_string(),
            "startLine": 1,
            "numLines": delivered,
            "totalLines": total,
        });
        if let Some(flag) = flag {
            file["truncatedByTokenCap"] = serde_json::json!(flag);
        }
        let mut payload = read_of(path, None, None);
        payload["tool_response"] = serde_json::json!({"type": "text", "file": file});
        payload
    }

    /// ★ **A delivery too large for density to vouch for becomes coverage from
    /// the client's own statement, one hook later.**
    ///
    /// The file here is about 59 KB — past `max_delivered_bytes` and therefore
    /// recorded as nothing before the call runs, which is the state in which
    /// every re-read of a repository's largest and most-read files has started
    /// until now. The result object then states `numLines` reaching the
    /// client's own `totalLines` with no truncation flag beside it, which is
    /// the client saying it handed the file over, and a later read of two
    /// hundred lines inside it is a read the window provably holds.
    ///
    /// The second half is the same walk with the flag set: the client says it
    /// stopped at line forty, the credit is exactly those forty lines, and the
    /// sub-range past them is not declined. One rule, read from both sides.
    #[test]
    fn a_stated_complete_delivery_credits_a_slice_the_size_rule_refused() {
        let (tmp, file) = checkout(6_000);
        let bytes = std::fs::metadata(&file).expect("the fixture").len();
        assert!(
            bytes > rift_agent::window_journal::DEFAULT_MAX_DELIVERED_BYTES as u64,
            "the walk is only about a slice past the bound: {bytes} bytes"
        );
        let whole = read_of(&file, None, None);
        let inside = read_of(&file, Some(100), Some(200));
        let enforce = enforcing();

        // Nothing is credited before the call has run, whatever it will
        // deliver: a decline cannot rest on a delivery that has not happened.
        assert!(gate_under(
            &hook("claude-code", "PreToolUse", "s-1", whole.clone()),
            tmp.path(),
            &enforce
        )
        .deny_reason
        .is_none());
        assert!(
            lines_of(tmp.path(), "s-1", None).is_empty(),
            "the size rule credited nothing, exactly as before"
        );

        gate_under(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-1",
                whole_result_of(&file, 6_001, 6_001, None),
            ),
            tmp.path(),
            &enforce,
        );
        let journal = lines_journal(tmp.path(), "s-1");
        let [Line::Read { range, .. }] = journal.live() else {
            panic!("the statement credits one read, got {:?}", journal.live());
        };
        assert_eq!(
            *range,
            Interval::new(1, 6_000).expect("a run"),
            "the client's own extent, clamped to the line count of these bytes"
        );
        assert!(
            gate_under(
                &hook("claude-code", "PreToolUse", "s-1", inside.clone()),
                tmp.path(),
                &enforce
            )
            .deny_reason
            .is_some(),
            "a sub-range of a delivery the client vouched for is declinable"
        );

        // The same walk in another window, with the client saying it stopped
        // at line forty.
        gate_under(
            &hook("claude-code", "PreToolUse", "s-2", whole.clone()),
            tmp.path(),
            &enforce,
        );
        gate_under(
            &hook(
                "claude-code",
                "PostToolUse",
                "s-2",
                whole_result_of(&file, 40, 6_001, Some(true)),
            ),
            tmp.path(),
            &enforce,
        );
        let cut = lines_journal(tmp.path(), "s-2");
        let [Line::Read { range, .. }] = cut.live() else {
            panic!("the statement credits one read, got {:?}", cut.live());
        };
        assert_eq!(
            *range,
            Interval::new(1, 40).expect("a run"),
            "exactly the run the result names, and not a line more"
        );
        assert!(
            gate_under(
                &hook("claude-code", "PreToolUse", "s-2", inside),
                tmp.path(),
                &enforce
            )
            .deny_reason
            .is_none(),
            "the lines past the truncation were never handed over"
        );
    }

    /// A slice too large to have survived the client's own cap runs without
    /// coverage before any result comes back — the truncation statement is
    /// only the backstop for a smaller call whose result still says partial.
    ///
    /// Both sides of the boundary are driven, so this reds on a raise as well
    /// as on a drop.
    #[test]
    fn a_slice_too_large_to_have_survived_the_cap_records_nothing() {
        // "line NNNN\n" is ten bytes from line 1000 on, so five thousand lines
        // are about 50 KB: over the bound, with a prefix of it under.
        let (tmp, file) = checkout(6_000);
        let bound = rift_agent::window_journal::DEFAULT_MAX_DELIVERED_BYTES;
        let bytes = std::fs::read(&file).expect("the fixture");
        let upto = |lines: u32| -> usize {
            bytes
                .split_inclusive(|byte| *byte == b'\n')
                .take(lines as usize)
                .map(<[u8]>::len)
                .sum()
        };
        let mut under = 1;
        while upto(under + 1) <= bound {
            under += 1;
        }
        assert!(
            upto(under) <= bound && upto(under + 1) > bound,
            "the boundary"
        );

        gate(
            &hook(
                "claude-code",
                "PreToolUse",
                "s-1",
                read_of(&file, Some(1), Some(under)),
            ),
            tmp.path(),
        );
        gate(
            &hook(
                "claude-code",
                "PreToolUse",
                "s-2",
                read_of(&file, Some(1), Some(under + 1)),
            ),
            tmp.path(),
        );
        let inside = lines_of(tmp.path(), "s-1", None);
        let [Line::Read { range, .. }] = inside.as_slice() else {
            panic!("one read line, got {inside:?}");
        };
        assert_eq!(
            *range,
            Interval::new(1, under).expect("a run"),
            "a slice inside the bound is credited in full"
        );
        let outside = lines_of(tmp.path(), "s-2", None);
        assert!(outside.is_empty(), "one byte over creates no coverage");
    }

    /// A read of lines this window was already handed, at the same revision,
    /// is what a decline would be for — and while nothing is enforced it is
    /// recorded as such and let through.
    #[test]
    fn a_read_of_lines_already_held_is_observed_and_not_declined() {
        let (tmp, file) = checkout(120);
        let payload = read_of(&file, Some(10), Some(20));
        let call = hook("claude-code", "PreToolUse", "s-1", payload);
        gate(&call, tmp.path());
        gate(&call, tmp.path());

        let lines = lines_of(tmp.path(), "s-1", None);
        assert_eq!(lines.len(), 2, "{lines:?}");
        assert_eq!(lines.first().map(Line::kind), Some("read"));
        assert_eq!(
            lines.get(1).map(Line::kind),
            Some("observe"),
            "the repeat is what a decline would be for, and nothing was declined"
        );
    }

    // ------------------------------------------- reads that never happened ---

    /// Announce a hold the way an injection does: Rift has told this window
    /// it holds these lines of the revision the checkout has now.
    fn announce(work_tree: &Path, session: &str, rel: &str, lo: u32, hi: u32) {
        let (blob, _) = rift_recall_substrate::worktree::hash_blob_bytes(work_tree, rel)
            .expect("the fixture file");
        crate::window_journal::WindowJournal::open(work_tree, session, None)
            .append(&Line::Hold {
                turn: 4,
                path: rel.to_owned(),
                hash: rift_core::Sha256Id::parse_hex(&blob.content_sha256).expect("a revision"),
                range: Interval::new(lo, hi).expect("a run"),
                at_ms: 1_700_000_000_000,
            })
            .expect("the announcement is recorded");
    }

    /// An `Edit` of `path`, as the pinned fields spell one.
    fn edit_call(path: &Path) -> serde_json::Value {
        serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {
                "file_path": path.display().to_string(),
                "old_string": "line 1\n",
                "new_string": "line one\n",
            }
        })
    }

    /// A content search over one named file, as the pinned shell grammar
    /// spells one.
    fn grep_call(path: &Path) -> serde_json::Value {
        serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": format!("grep needle {}", shell_spelling(path)) }
        })
    }

    fn prevented(
        raw: &RawHook,
        work_tree: &Path,
        effective: &rift_recall_policy::Effective,
    ) -> bool {
        prevented_record(
            &AdapterRegistry::with_builtin(),
            raw,
            &work_tree.join(".git"),
            work_tree,
            effective,
        )
    }

    /// **The lever's whole claim**: Rift said the window holds the file, the
    /// window changed the file, and the read it would have made first never
    /// happened. One booking, naming the announced lines and no content.
    #[test]
    fn an_edit_of_a_held_file_without_a_reread_books_one_prevented_read() {
        let (tmp, file) = checkout(120);
        let here = tmp.path();
        announce(here, "s-p1", "src.rs", 1, 120);
        let call = hook("claude-code", "PreToolUse", "s-p1", edit_call(&file));
        assert!(prevented(&call, here, &enforcing()));

        let rows = audits(here);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].decision,
            crate::agent_queue::ServeAuditKind::ReadPrevented
        );
        assert!(rows[0].frame.is_empty(), "nothing was put in front of it");
        let basis = rows[0].basis.as_ref().expect("the bytes to price");
        assert_eq!(basis.rel_path, "src.rs");
        assert_eq!(
            basis.lines,
            Some(crate::agent_queue::AuditLines { lo: 1, hi: 120 })
        );
        assert!(!basis.blob_oid.is_empty(), "the fallback source is named");
        let lines = lines_of(here, "s-p1", None);
        assert_eq!(
            lines.iter().filter(|l| l.kind() == "prevented").count(),
            1,
            "{lines:?}"
        );
    }

    /// A content search inside an announced file is the same act: the window
    /// is handed the matches, never the file, so the read it was told it did
    /// not need is a read that did not happen.
    #[test]
    fn a_grep_naming_a_held_file_books_one_prevented_read() {
        let (tmp, file) = checkout(120);
        let here = tmp.path();
        announce(here, "s-p2", "src.rs", 1, 120);
        let call = hook("claude-code", "PreToolUse", "s-p2", grep_call(&file));
        assert!(prevented(&call, here, &enforcing()));
        let rows = audits(here);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].decision,
            crate::agent_queue::ServeAuditKind::ReadPrevented
        );
    }

    /// One announcement is worth one read, however many times the window acts
    /// on the file afterwards: the second edit finds the booking and books
    /// nothing more.
    #[test]
    fn a_second_action_on_the_same_hold_in_one_window_books_nothing_more() {
        let (tmp, file) = checkout(120);
        let here = tmp.path();
        announce(here, "s-p3", "src.rs", 1, 120);
        let call = hook("claude-code", "PreToolUse", "s-p3", edit_call(&file));
        assert!(prevented(&call, here, &enforcing()));
        assert!(
            !prevented(&call, here, &enforcing()),
            "the announcement has already been booked"
        );
        assert_eq!(audits(here).len(), 1);
    }

    /// **Never twice for one slice.** A read of the announced file that was
    /// declined is already booked by the decline; the edit that follows adds
    /// nothing, because the decline answered the announcement.
    #[test]
    fn a_file_intercepted_and_edited_in_one_window_books_the_intercept_only() {
        let (tmp, file) = checkout(120);
        let here = tmp.path();
        let enforce = enforcing();
        let read = hook(
            "claude-code",
            "PreToolUse",
            "s-p4",
            read_of(&file, None, None),
        );
        gate_under(&read, here, &enforce);
        announce(here, "s-p4", "src.rs", 1, 120);
        assert!(
            gate_under(&read, here, &enforce).deny_reason.is_some(),
            "the repeat is declined, and that decline books the slice"
        );
        let edit = hook("claude-code", "PreToolUse", "s-p4", edit_call(&file));
        assert!(!prevented(&edit, here, &enforce));

        let rows = audits(here);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].decision,
            crate::agent_queue::ServeAuditKind::ReadIntercepted
        );
    }

    /// ★ **An observed re-read is a read, and answers the announcement.**
    /// Under the shipped line floor a repeat of a short slice is decided
    /// against and let through: the call RUNS and the bytes arrive in the
    /// window. The edit that follows has replaced no read, so it books
    /// nothing — the `observe` in the journal names what Rift did, never what
    /// the tool delivered.
    #[test]
    fn an_observed_reread_of_a_held_file_leaves_the_edit_nothing_to_book() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let here = tmp.path();
        let file = here.join("src.rs");
        std::fs::write(&file, "a\nb\nc\n").expect("the file");
        let mut enforce = levers("claude-code", rift_recall_policy::Mode::Enforce);
        enforce.min_slice_lines = rift_recall_policy::DEFAULT_MIN_SLICE_LINES;
        let read = hook("claude-code", "PreToolUse", "s-p7", whole(&file));
        gate_under(&read, here, &enforce);
        announce(here, "s-p7", "src.rs", 1, 3);
        assert_eq!(
            gate_under(&read, here, &enforce).deny_reason,
            None,
            "three lines is under the floor, so the repeat runs and delivers"
        );

        let edit = hook("claude-code", "PreToolUse", "s-p7", edit_call(&file));
        assert!(
            !prevented(&edit, here, &enforce),
            "the window was handed the bytes; no read was prevented"
        );
        assert_eq!(
            lines_of(here, "s-p7", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "hold", "observe"],
            "and no booking joined them"
        );
        let rows = audits(here);
        assert!(
            rows.iter().all(|row| !matches!(
                row.decision,
                crate::agent_queue::ServeAuditKind::ReadPrevented
                    | crate::agent_queue::ServeAuditKind::ReadPreventedObserved
            )),
            "{rows:?}"
        );
    }

    /// ★ **Never twice for one slice, in the other order either.** The
    /// window searches the announced file first, which books the read that
    /// never happened; the read it makes afterwards is still declined — the
    /// model is still told what it holds, and the reason it was sent is still
    /// charged — but the slice is already booked, so the decline claims a
    /// zero under the tag that says which row has it.
    #[test]
    fn a_decline_after_a_prevented_booking_books_nothing_more() {
        assert_eq!(
            rift_tokens::provenance(),
            rift_tokens::TOKENIZER,
            "these figures are only claims if the real tokenizer is missing"
        );
        let (tmp, file) = checkout(120);
        let here = tmp.path();
        let enforce = enforcing();
        let read = hook(
            "claude-code",
            "PreToolUse",
            "s-p8",
            read_of(&file, None, None),
        );
        gate_under(&read, here, &enforce);
        announce(here, "s-p8", "src.rs", 1, 120);
        let grep = hook("claude-code", "PreToolUse", "s-p8", grep_call(&file));
        assert!(prevented(&grep, here, &enforce), "the booking comes first");
        assert!(
            gate_under(&read, here, &enforce).deny_reason.is_some(),
            "and the read is still declined and still pointed"
        );
        assert_eq!(
            lines_of(here, "s-p8", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "hold", "prevented", "deny"],
            "the once-per-key decline is untouched"
        );

        let rows = audits(here);
        assert_eq!(rows.len(), 2, "{rows:?}");
        let figures = |kind: crate::agent_queue::ServeAuditKind| {
            let row = rows
                .iter()
                .find(|row| row.decision == kind)
                .expect("the row that kind owes");
            match serve_audit_event(here, row) {
                rift_recall_serve::MemdEvent::ReadPrevented {
                    reason,
                    estimated_tokens_avoided,
                    ..
                } => (reason, 0, estimated_tokens_avoided),
                rift_recall_serve::MemdEvent::ReadIntercepted {
                    reason,
                    estimated_tokens_returned,
                    estimated_tokens_avoided,
                    ..
                } => (reason, estimated_tokens_returned, estimated_tokens_avoided),
                other => panic!("neither of the two rows: {other:?}"),
            }
        };
        let whole_file = rift_tokens::count(
            &std::fs::read_to_string(&file).expect("the fixture the booking priced"),
        );
        assert!(whole_file > 0, "a file worth reading");
        let (reason, _, avoided) = figures(crate::agent_queue::ServeAuditKind::ReadPrevented);
        assert_eq!(reason, rift_recall_serve::TAG_PREVENTED);
        assert_eq!(avoided, whole_file, "the booking has the slice");
        let (reason, returned, avoided) =
            figures(crate::agent_queue::ServeAuditKind::ReadIntercepted);
        assert_eq!(reason, rift_recall_serve::TAG_PREVENTED_FIRST);
        assert_eq!(
            avoided, 0,
            "and the decline claims none of it a second time"
        );
        assert!(returned > 0, "while what its reason cost is still charged");
    }

    /// ★ **A decline that reaches past a booking still claims nothing.** The
    /// announcement was about part of a file, and the booking took that part;
    /// the read that is declined later asks for the whole of it. A decline
    /// books the run it was asked for and nothing smaller, so booking it
    /// beside the slice already taken would charge one revision for one and a
    /// half reads.
    ///
    /// Driven exactly as it was found: a slice is read and announced, a
    /// search books it, the whole file is read (and allowed, because the
    /// window did not hold it), and the read after that is declined. The
    /// figure the window ends on is the slice's, once.
    #[test]
    fn a_wider_decline_over_a_prevented_slice_books_nothing_more() {
        assert_eq!(
            rift_tokens::provenance(),
            rift_tokens::TOKENIZER,
            "these figures are only claims if the real tokenizer is missing"
        );
        let (tmp, file) = checkout(120);
        let here = tmp.path();
        let enforce = enforcing();
        let session = "s-p9";
        let slice = hook(
            "claude-code",
            "PreToolUse",
            session,
            read_of(&file, Some(1), Some(20)),
        );
        gate_under(&slice, here, &enforce);
        announce(here, session, "src.rs", 1, 20);
        let grep = hook("claude-code", "PreToolUse", session, grep_call(&file));
        assert!(prevented(&grep, here, &enforce), "the booking comes first");

        let whole = hook(
            "claude-code",
            "PreToolUse",
            session,
            read_of(&file, None, None),
        );
        assert!(
            gate_under(&whole, here, &enforce).deny_reason.is_none(),
            "the window held twenty lines, not the file"
        );
        assert!(
            gate_under(&whole, here, &enforce).deny_reason.is_some(),
            "and now it does, so the read after it is declined"
        );
        assert_eq!(
            lines_of(here, session, None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "hold", "prevented", "read", "deny"],
            "the once-per-key decline is untouched"
        );

        let rows = audits(here);
        assert_eq!(rows.len(), 2, "{rows:?}");
        let figures = |kind: crate::agent_queue::ServeAuditKind| {
            let row = rows
                .iter()
                .find(|row| row.decision == kind)
                .expect("the row that kind owes");
            match serve_audit_event(here, row) {
                rift_recall_serve::MemdEvent::ReadPrevented {
                    reason,
                    estimated_tokens_avoided,
                    ..
                }
                | rift_recall_serve::MemdEvent::ReadIntercepted {
                    reason,
                    estimated_tokens_avoided,
                    ..
                } => (reason, estimated_tokens_avoided),
                other => panic!("neither of the two rows: {other:?}"),
            }
        };
        let body = std::fs::read_to_string(&file).expect("the fixture the booking priced");
        let slice_cost = rift_tokens::count(&body.lines().take(20).fold(
            String::new(),
            |mut text, line| {
                text.push_str(line);
                text.push('\n');
                text
            },
        ));
        assert!(slice_cost > 0, "a slice worth reading");
        assert!(
            rift_tokens::count(&body) > slice_cost,
            "and a file worth more than the slice"
        );
        let (reason, booked) = figures(crate::agent_queue::ServeAuditKind::ReadPrevented);
        assert_eq!(reason, rift_recall_serve::TAG_PREVENTED);
        assert_eq!(booked, slice_cost, "the booking has the slice");
        let (reason, avoided) = figures(crate::agent_queue::ServeAuditKind::ReadIntercepted);
        assert_eq!(reason, rift_recall_serve::TAG_PREVENTED_FIRST);
        assert_eq!(avoided, 0, "the wider decline claims none of it again");
        assert_eq!(
            booked + avoided,
            slice_cost,
            "one slice, one saving, and nothing added for reaching past it"
        );
    }

    /// Nothing was announced, so nothing was avoided. This is the ordinary
    /// case, and it is the one that must cost nothing and claim nothing.
    #[test]
    fn an_action_on_a_file_never_announced_books_nothing() {
        let (tmp, file) = checkout(120);
        let here = tmp.path();
        for payload in [edit_call(&file), grep_call(&file)] {
            let call = hook("claude-code", "PreToolUse", "s-p5", payload);
            assert!(!prevented(&call, here, &enforcing()));
        }
        assert!(audits(here).is_empty());
        assert!(lines_of(here, "s-p5", None).is_empty());
    }

    /// Observe records the decision and claims none of it: the row names the
    /// file and the lines, so the class can be sized, and its avoided figure
    /// is a zero under a tag that says why.
    #[test]
    fn observe_records_a_prevented_read_with_nothing_avoided() {
        let (tmp, file) = checkout(120);
        let here = tmp.path();
        announce(here, "s-p6", "src.rs", 1, 120);
        let call = hook("claude-code", "PreToolUse", "s-p6", edit_call(&file));
        assert!(prevented(&call, here, &observing()));

        let rows = audits(here);
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!(
            rows[0].decision,
            crate::agent_queue::ServeAuditKind::ReadPreventedObserved
        );
        let event = serve_audit_event(here, &rows[0]);
        let rift_recall_serve::MemdEvent::ReadPrevented {
            reason,
            estimated_tokens_avoided,
            handle,
        } = event
        else {
            panic!("an observed prevented read is still a prevented read: {event:?}");
        };
        assert_eq!(estimated_tokens_avoided, 0);
        assert_eq!(reason, rift_recall_serve::TAG_PREVENTED_OBSERVED);
        assert!(handle.starts_with("file:src.rs@sha256:"), "{handle}");
    }

    /// The journaled jobs a work tree's queue holds, in listing order.
    fn journaled(work_tree: &Path) -> Vec<crate::agent_queue::AgentJob> {
        crate::agent_queue::jobs(&work_tree.join(".git"))
            .into_iter()
            .map(|(_, job)| job)
            .collect()
    }

    /// The row a repeat owes is journaled, and only for the repeat: the first
    /// read decided nothing, so it owes nothing. What is written names the
    /// bytes (path, revision, blob, lines) and no content, because the worker
    /// must be able to count exactly those bytes or none.
    #[test]
    fn the_repeat_the_gate_observes_journals_the_row_it_owes() {
        let (tmp, file) = checkout(120);
        let here = tmp.path();
        let payload = read_of(&file, Some(10), Some(20));
        let call = hook("claude-code", "PreToolUse", "s-8", payload);
        assert!(
            !gate(&call, here).journaled,
            "an allowed read owes no accounting"
        );
        assert!(journaled(here).is_empty());

        assert!(
            gate(&call, here).journaled,
            "the repeat is what sizes the pool"
        );
        let jobs = journaled(here);
        assert_eq!(jobs.len(), 1, "{jobs:?}");
        let crate::agent_queue::JobKind::ServeAudit(audit) = &jobs[0].kind else {
            panic!("the gate journals an accounting row: {:?}", jobs[0].kind);
        };
        assert_eq!(
            audit.decision,
            crate::agent_queue::ServeAuditKind::ReadRepeatObserved
        );
        assert_eq!(audit.agent, "claude-code");
        assert!(audit.frame.is_empty(), "nothing was sent, so nothing cost");
        let basis = audit.basis.as_ref().expect("the bytes to count");
        assert_eq!(basis.rel_path, "src.rs");
        assert_eq!(
            basis.lines,
            Some(crate::agent_queue::AuditLines { lo: 10, hi: 29 })
        );
        assert_eq!(basis.content_sha256, revision(here, "src.rs").to_hex());
        assert!(!basis.blob_oid.is_empty(), "the fallback source is named");
        assert_eq!(jobs[0].queued_at_ms, 1_700_000_000_000, "the hook\'s clock");
        assert_eq!(
            jobs[0].work_tree, here,
            "the checkout the worker resolves the family from"
        );
    }

    /// The decline arm of the same mapping, reached through the pure function
    /// because no family is declined on while every lever is in observe. A
    /// decline books what it declined AND what its reason cost; an allowed
    /// read books nothing at all.
    #[test]
    fn a_decline_books_its_frame_and_an_allowed_read_books_nothing() {
        use rift_agent::window_journal::{Decision, DenyShape, ResolvedRead};
        let read = ResolvedRead::new(
            &rift_agent::ReadRequest {
                path: "src.rs".to_owned(),
                span: rift_agent::ReadSpan::lines(1, rift_agent::HiKind::Explicit(10)),
                channel: rift_agent::ReadChannel::Tool,
            },
            rift_core::Sha256Id::parse_hex(&"ab".repeat(32)).expect("a revision"),
            80,
        )
        .expect("a resolved read");
        let agent = rift_core::AgentId::CLAUDE_CODE;
        let id = || "0123456789abcdef0123456789abcdef".to_owned();
        assert!(
            read_decision_audit(&agent, &read, "oid", &Decision::Allow, "", false, id()).is_none(),
            "nothing was decided, so nothing is owed"
        );
        let denied = read_decision_audit(
            &agent,
            &read,
            "oid",
            &Decision::Deny(DenyShape::Plain),
            "you already hold these lines",
            false,
            id(),
        )
        .expect("the row a decline owes");
        assert_eq!(
            denied.decision,
            crate::agent_queue::ServeAuditKind::ReadIntercepted
        );
        assert_eq!(denied.frame, "you already hold these lines");
        assert_eq!(denied.call_id, id());
    }

    /// A tool call that FAILED is not a result this window holds. The family
    /// publishes its own verb for one, and it is not a step of this gate, so
    /// no line is written and no coverage is credited — which is what stops a
    /// later decline resting on bytes the model never received.
    #[test]
    fn a_failed_tool_call_leaves_the_window_untouched() {
        let (tmp, file) = checkout(40);
        let here = tmp.path();
        let payload = read_of(&file, None, Some(10));
        gate(
            &hook("claude-code", "PostToolUseFailure", "s-9", payload),
            here,
        );
        assert!(
            lines_of(here, "s-9", None).is_empty(),
            "a failed call hands the window nothing"
        );
        assert!(journaled(here).is_empty());
    }

    /// One call the gate refuses to record anything about.
    ///
    /// Every refusal in the gate leans the same way, because a journal holding
    /// less than really happened costs a saving, where one claiming bytes the
    /// window never had costs the agent its content.
    fn refused(work_tree: &Path, why: &str, agent: &str, payload: serde_json::Value) {
        let call = hook(agent, "PreToolUse", "s-2", payload);
        gate(&call, work_tree);
        assert!(lines_of(work_tree, "s-2", None).is_empty(), "{why}");
    }

    /// A path the gate cannot stand behind: one it cannot resolve to a file of
    /// this checkout, or one whose bytes it will not spend an agent's budget
    /// hashing.
    #[test]
    fn a_path_the_gate_cannot_resolve_here_records_nothing() {
        let (tmp, file) = checkout(4);
        let here = tmp.path();
        let outside = tempfile::tempdir().expect("another tree");
        let elsewhere = outside.path().join("elsewhere.rs");
        std::fs::write(&elsewhere, b"x\n").expect("the file");
        let big = here.join("big.bin");
        let past_the_cap = usize::try_from(observing().max_inline_hash_bytes).expect("a size") + 1;
        std::fs::write(&big, vec![b'a'; past_the_cap]).expect("a large file");
        let binary = here.join("binary.bin");
        std::fs::write(&binary, b"one\0two\n").expect("bytes with a zero");

        let call = hook(
            "claude-code",
            "PreToolUse",
            "",
            read_of(&file, None, Some(2)),
        );
        gate(&call, here);
        assert!(
            lines_of(here, "", None).is_empty(),
            "a hook naming no window has no journal to write to"
        );
        refused(
            here,
            "a relative path is relative to a directory no payload states",
            "claude-code",
            serde_json::json!({
                "tool_name": "Read",
                "tool_input": { "file_path": "src.rs" },
            }),
        );
        for (why, path) in [
            (
                "a file of another tree is of no checkout this journal describes",
                &elsewhere,
            ),
            (
                "a file past the inline cap is not hashed on an agent's budget",
                &big,
            ),
            ("bytes holding a zero are not lines", &binary),
            (
                "a file that is not there has no revision to record",
                &here.join("gone.rs"),
            ),
        ] {
            refused(here, why, "claude-code", read_of(path, None, Some(1)));
        }
    }

    /// A call that is not the read shape the pins describe. An argument
    /// outside the pinned set takes the whole call out of the shape rather
    /// than being ignored, because a range derived from arguments nothing
    /// pinned is a range nothing bounds.
    #[test]
    fn a_call_outside_the_pinned_read_shape_records_nothing() {
        let (tmp, file) = checkout(4);
        let here = tmp.path();
        let mut with_pages = read_of(&file, None, None);
        with_pages["tool_input"]["pages"] = serde_json::json!([1]);
        refused(
            here,
            "an argument outside the pinned set takes the call out of the shape",
            "claude-code",
            with_pages,
        );
        refused(
            here,
            "a command line outside the published grammar is not a read",
            "claude-code",
            serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": "cat src.rs | head -n 2" },
            }),
        );
        refused(
            here,
            "a command line naming its file relatively names it in a \
             directory this step does not resolve against",
            "claude-code",
            serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": "cat src.rs" },
            }),
        );
        refused(
            here,
            "a family whose reads travel through a shell states no range",
            "codex",
            read_of(&file, None, Some(2)),
        );
    }

    /// **A read run through a terminal is a read**, once the command line is
    /// one the published grammar spells and the file it names is one of this
    /// checkout's.
    ///
    /// The path is spelled absolutely here; the relative spelling, which is
    /// how a person and an agent actually write a shell read, is the twin
    /// below.
    #[test]
    fn a_shell_read_of_this_checkout_is_recorded_as_the_lines_it_delivers() {
        let (tmp, file) = checkout(120);
        let command = format!("sed -n '10,29p' {}", shell_spelling(&file));
        gate(
            &hook(
                "claude-code",
                "PreToolUse",
                "s-9",
                serde_json::json!({
                    "tool_name": "Bash",
                    "tool_input": { "command": command },
                }),
            ),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-9", None);
        let [Line::Read {
            path,
            range,
            total_lines,
            ..
        }] = lines.as_slice()
        else {
            panic!("one read line, got {lines:?}");
        };
        assert_eq!(path, "src.rs");
        assert_eq!(*range, Interval::new(10, 29).expect("a run of lines"));
        assert_eq!(*total_lines, 120);
    }

    /// ★ A shell read written the way shell reads are written — a relative
    /// path — is recorded, against the directory the payload itself named.
    ///
    /// `sed -n '10,29p' src.rs` is the same read as the absolute spelling
    /// above and used to leave no line at all, because the resolution refused
    /// every relative name outright. What makes the name resolvable is the
    /// `cwd` the family puts on every payload: a stated fact, not the hook
    /// process's own directory, which says only where the client launched it.
    #[test]
    fn a_shell_read_named_relatively_is_recorded_against_the_directory_the_payload_states() {
        let (tmp, _file) = checkout(120);
        let here = tmp.path().canonicalize().expect("a canonical work tree");
        gate(
            &hook(
                "claude-code",
                "PreToolUse",
                "s-rel",
                serde_json::json!({
                    "tool_name": "Bash",
                    "tool_input": { "command": "sed -n '10,29p' src.rs" },
                    "cwd": here.display().to_string(),
                }),
            ),
            tmp.path(),
        );

        let lines = lines_of(tmp.path(), "s-rel", None);
        let [Line::Read { path, range, .. }] = lines.as_slice() else {
            panic!("one read line, got {lines:?}");
        };
        assert_eq!(path, "src.rs", "the checkout's own spelling either way");
        assert_eq!(*range, Interval::new(10, 29).expect("a run of lines"));
    }

    /// ★ The same relative read, with nothing stating where the agent stood,
    /// still records nothing.
    ///
    /// Three ways a directory can fail to be stated, and every one of them
    /// leans the same way: no `cwd` at all; a `cwd` that is itself relative
    /// and so names no place; and a `cwd` outside this checkout, whose file
    /// this journal has no name for. A guess in any of the three would put a
    /// window's coverage on bytes some other file holds — which is the one
    /// error that costs an agent its content rather than a saving.
    #[test]
    fn a_relative_read_with_no_stated_directory_records_nothing() {
        let (tmp, _file) = checkout(120);
        let outside = tempfile::tempdir().expect("another tree");
        std::fs::write(outside.path().join("src.rs"), "elsewhere\n").expect("its own file");
        let elsewhere = outside
            .path()
            .canonicalize()
            .expect("a canonical other tree");
        for (why, cwd) in [
            ("no working directory is stated at all", None),
            (
                "a relative working directory names no place to resolve against",
                Some("./sub".to_owned()),
            ),
            (
                "a directory outside this checkout names a file of no checkout this journal \
                 describes",
                Some(elsewhere.display().to_string()),
            ),
        ] {
            let mut payload = serde_json::json!({
                "tool_name": "Bash",
                "tool_input": { "command": "sed -n '10,29p' src.rs" },
            });
            if let Some(cwd) = cwd {
                payload["cwd"] = serde_json::json!(cwd);
            }
            refused(tmp.path(), why, "claude-code", payload);
        }
    }

    /// A subagent's hooks arrive under its parent's session id and carry their
    /// own agent id. Two windows, two journals: the parent must not be told it
    /// holds what only the child was handed.
    #[test]
    fn a_subagent_records_under_its_own_context_window() {
        let (tmp, file) = checkout(50);
        let mut payload = read_of(&file, None, Some(10));
        payload["agent_id"] = serde_json::json!("child-1");
        gate(
            &hook("claude-code", "PreToolUse", "s-3", payload),
            tmp.path(),
        );

        assert!(
            lines_of(tmp.path(), "s-3", None).is_empty(),
            "the parent window holds nothing"
        );
        assert_eq!(
            lines_of(tmp.path(), "s-3", Some("child-1")).len(),
            1,
            "the child's own window holds the read"
        );
    }

    /// ★ **Two reads on either side of a turn boundary are recorded in
    /// different turns**, on a family whose wire carries no turn at all.
    ///
    /// The ordinal comes off the session cache the ingest layer advances at
    /// the boundary, which is a file read and not a store — the only kind of
    /// lookup this path is allowed. Before this, every line of every alpha
    /// family said turn 0, and the two figures counted per turn (a retry
    /// inside one turn, and what a window held before it lost it) were flat.
    #[test]
    fn two_reads_across_a_prompt_boundary_are_recorded_in_different_turns() {
        let (tmp, file) = checkout(40);
        let here = tmp.path();

        open_turn(here, "s-turn", 1);
        gate(
            &native_hook(
                "claude-code",
                "PreToolUse",
                "s-turn",
                read_of(&file, Some(1), Some(5)),
            ),
            here,
        );
        // The next prompt opens the next ordinal, and the read after it is a
        // read of that turn.
        open_turn(here, "s-turn", 2);
        gate(
            &native_hook(
                "claude-code",
                "PreToolUse",
                "s-turn",
                read_of(&file, Some(10), Some(5)),
            ),
            here,
        );

        let turns: Vec<u32> = lines_of(here, "s-turn", None)
            .iter()
            .map(Line::turn)
            .collect();
        assert_eq!(
            turns,
            vec![1, 2],
            "each read is recorded in the turn it was made in"
        );
    }

    /// And a window with no cache to read from is turn 0 — the number every
    /// line carried before the boundary was consulted at all. A decision is
    /// never made of a turn, so the miss costs a distinction and nothing else.
    #[test]
    fn a_window_with_no_boundary_on_record_is_recorded_in_turn_zero() {
        let (tmp, file) = checkout(40);
        let here = tmp.path();
        gate(
            &native_hook(
                "claude-code",
                "PreToolUse",
                "s-turnless",
                read_of(&file, Some(1), Some(5)),
            ),
            here,
        );
        let lines = lines_of(here, "s-turnless", None);
        let [line] = lines.as_slice() else {
            panic!("one read, one line");
        };
        assert_eq!(line.turn(), 0);
    }

    /// An envelope that states its own turn is trusted verbatim, and the cache
    /// beside it is never consulted: the wire is the more specific fact.
    #[test]
    fn a_stated_turn_wins_over_the_boundary_on_record() {
        let (tmp, file) = checkout(40);
        let here = tmp.path();
        open_turn(here, "s-stated", 9);
        gate(
            &hook(
                "claude-code",
                "PreToolUse",
                "s-stated",
                read_of(&file, Some(1), Some(5)),
            ),
            here,
        );
        let lines = lines_of(here, "s-stated", None);
        let [line] = lines.as_slice() else {
            panic!("one read, one line");
        };
        assert_eq!(line.turn(), 4, "the envelope's ordinal, not the cache's");
    }

    /// The revision a file of this checkout is at, the way the gate reads it.
    fn revision(work_tree: &Path, rel: &str) -> rift_core::Sha256Id {
        let (blob, _) = rift_recall_substrate::worktree::hash_blob_bytes(work_tree, rel)
            .expect("the file is there");
        rift_core::Sha256Id::parse_hex(&blob.content_sha256).expect("a revision")
    }

    /// Each signal a family documents for "the context stopped holding what it
    /// was given" empties the window, and nothing else does. Antigravity
    /// publishes none at all, which is the fact that keeps it in observe.
    #[test]
    fn each_documented_eviction_signal_empties_the_window_and_no_other_does() {
        let evicting: Vec<(&str, &str, serde_json::Value, &str)> = vec![
            (
                "claude-code",
                "PreCompact",
                serde_json::json!({}),
                "pre_compact",
            ),
            (
                "claude-code",
                "SessionStart",
                serde_json::json!({"source": "compact"}),
                "compact",
            ),
            (
                "claude-code",
                "SessionStart",
                serde_json::json!({"source": "clear"}),
                "clear",
            ),
            (
                "claude-code",
                "SessionStart",
                serde_json::json!({"source": "resume"}),
                "resume",
            ),
            ("codex", "PreCompact", serde_json::json!({}), "pre_compact"),
            (
                "codex",
                "PostCompact",
                serde_json::json!({}),
                "post_compact",
            ),
        ];
        for (agent, verb, payload, reason) in evicting {
            let (tmp, _file) = checkout(20);
            let here = tmp.path();
            let held = revision(here, "src.rs");
            // Seeded rather than read through the gate, because which calls a
            // family records a read through is not what this is about: Codex
            // reads through its shell, and its signals answer for whatever its
            // window was handed.
            let window = crate::window_journal::WindowJournal::open(here, "s-4", None);
            window
                .append(&Line::Read {
                    turn: 1,
                    path: "src.rs".to_owned(),
                    hash: held,
                    range: Interval::new(1, 5).expect("a run"),
                    total_lines: 20,
                    at_ms: 1,
                })
                .expect("the window holds a read");
            assert!(
                !window.load().journal.coverage("src.rs", held).is_empty(),
                "{agent}: it holds the read before the signal arrives"
            );

            gate(&hook(agent, verb, "s-4", payload), here);

            let lines = lines_of(here, "s-4", None);
            let last = lines.last().expect("a line");
            assert_eq!(last.kind(), "evict", "{agent} {verb}: {lines:?}");
            assert!(
                last.to_json_line().contains(reason),
                "{agent} {verb}: the signal is named, not merged into one token"
            );
            assert!(
                window.load().journal.coverage("src.rs", held).is_empty(),
                "{agent} {verb}: and what was read before it is no longer held"
            );
            assert!(
                !window
                    .load()
                    .journal
                    .recorded_coverage("src.rs", held)
                    .is_empty(),
                "{agent} {verb}: while staying on the record, as the map of what it lost"
            );
        }
    }

    /// And nothing else does. A signal a family does not publish is not one
    /// this reads into it: an eviction that never happened empties a window
    /// that is still holding what it read, which costs every saving after it.
    #[test]
    fn a_signal_a_family_does_not_publish_empties_nothing() {
        let quiet: Vec<(&str, &str, serde_json::Value)> = vec![
            (
                "claude-code",
                "SessionStart",
                serde_json::json!({"source": "startup"}),
            ),
            (
                "claude-code",
                "SessionStart",
                serde_json::json!({"source": "fork"}),
            ),
            ("claude-code", "SessionStart", serde_json::json!({})),
            ("claude-code", "PostCompact", serde_json::json!({})),
            ("antigravity", "PreCompact", serde_json::json!({})),
            (
                "antigravity",
                "SessionStart",
                serde_json::json!({"source": "clear"}),
            ),
        ];
        for (agent, verb, payload) in quiet {
            let tmp = tempfile::tempdir().expect("a work tree");
            gate(&hook(agent, verb, "s-5", payload), tmp.path());
            assert!(
                lines_of(tmp.path(), "s-5", None).is_empty(),
                "{agent} {verb}: no signal this family documents, no line"
            );
        }
    }

    /// A client too old to act on the channel a decision was sent on runs the
    /// call anyway, and its *result* arrives for a call the journal declined.
    /// The bytes reached the model, so the window is recorded as holding them
    /// — and the pair of lines is what says the decision went unheeded.
    #[test]
    fn a_result_for_a_declined_read_records_the_read_and_leaves_the_pair() {
        let (tmp, file) = checkout(40);
        let here = tmp.path();
        let held = revision(here, "src.rs");
        let window = crate::window_journal::WindowJournal::open(here, "s-6", None);
        window
            .append(&Line::Deny {
                turn: 1,
                path: "src.rs".to_owned(),
                hash: held,
                range: Interval::new(1, 10).expect("a run"),
                at_ms: 1,
            })
            .expect("a decline");

        let mut payload = read_of(&file, None, Some(10));
        payload["tool_response"] = serde_json::json!({ "type": "text" });
        gate(
            &hook("claude-code", "PostToolUse", "s-6", payload.clone()),
            here,
        );

        let lines = lines_of(here, "s-6", None);
        assert_eq!(
            lines.iter().map(Line::kind).collect::<Vec<_>>(),
            vec!["deny", "read"],
            "the decline stands and the read that happened is beside it"
        );
        assert_eq!(
            crate::window_journal::ineffective_denies(&window.load().journal),
            1,
            "and the pair is what a diagnostic counts"
        );

        // A second result for the same lines adds nothing: the decline has
        // been answered, so there is no longer a decline to contradict.
        gate(&hook("claude-code", "PostToolUse", "s-6", payload), here);
        assert_eq!(lines_of(here, "s-6", None).len(), 2);
    }

    /// The ordinary result — of a read nobody declined — leaves the journal
    /// alone. The read was recorded before the call ran, and recording it
    /// again would say the window was handed the lines twice.
    #[test]
    fn a_result_for_a_read_nobody_declined_records_nothing() {
        let (tmp, file) = checkout(40);
        let here = tmp.path();
        let payload = read_of(&file, None, Some(10));
        gate(
            &hook("claude-code", "PreToolUse", "s-7", payload.clone()),
            here,
        );
        gate(&hook("claude-code", "PostToolUse", "s-7", payload), here);

        let lines = lines_of(here, "s-7", None);
        assert_eq!(
            lines.iter().map(Line::kind).collect::<Vec<_>>(),
            vec!["read"]
        );
        assert_eq!(
            crate::window_journal::ineffective_denies(&lines_journal(here, "s-7")),
            0
        );
    }

    /// Whatever a family's channel says, the lever is what decides. A
    /// confirmed channel is permission to be understood, not permission to
    /// act: while nothing has read a setting, the mode here is observe and a
    /// read of lines the window already holds is recorded and let through.
    #[test]
    fn a_confirmed_channel_declines_nothing_while_the_lever_is_in_observe() {
        assert_eq!(
            observing().read_deny,
            rift_recall_policy::Mode::Observe,
            "the mode a repository that has written no setting is left in"
        );
        let confirmed = |agent: &str| {
            matches!(
                deny_channel(&rift_core::AgentId::new(agent), "PreToolUse"),
                Some(rift_agent::respond::Channel::DenyWithReason {
                    confirmed: true,
                    ..
                })
            )
        };
        assert!(
            confirmed("claude-code"),
            "a family whose channel is confirmed is what makes this test say something"
        );

        let (tmp, file) = checkout(60);
        let call = hook(
            "claude-code",
            "PreToolUse",
            "s-8",
            read_of(&file, Some(1), Some(30)),
        );
        gate(&call, tmp.path());
        gate(&call, tmp.path());
        assert_eq!(
            lines_of(tmp.path(), "s-8", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "observe"],
            "the repeat is recorded as what a decline would be for, and allowed"
        );

        for agent in ["claude-code", "codex", "antigravity"] {
            assert!(
                deny_channel(&rift_core::AgentId::new(agent), "PostToolUse").is_none(),
                "{agent}: a decline is a thing said before a call, not after it"
            );
        }
    }

    // --- declining, once, with a reason -------------------------------------

    /// An `Edit` as the pinned fields spell one.
    fn edit_of(path: &Path, old: &str, new: &str) -> serde_json::Value {
        serde_json::json!({
            "tool_name": "Edit",
            "tool_input": {
                "file_path": path.display().to_string(),
                "old_string": old,
                "new_string": new,
                "replace_all": false,
            },
        })
    }

    /// A read of the whole file, as a client asks for one.
    fn whole(path: &Path) -> serde_json::Value {
        read_of(path, Some(1), Some(2_000))
    }

    /// The window reads a file in full, edits it, and the file on disk becomes
    /// what the edit left behind — the state every test below starts from.
    fn read_then_edit(
        here: &Path,
        session: &str,
        file: &Path,
        before: &str,
        after: &str,
        edit: (&str, &str),
        levers: &rift_recall_policy::Effective,
    ) {
        std::fs::write(file, before).expect("the file");
        gate_under(
            &hook("claude-code", "PreToolUse", session, whole(file)),
            here,
            levers,
        );
        std::fs::write(file, after).expect("the edit lands");
        edit_record(
            &AdapterRegistry::with_builtin(),
            &hook(
                "claude-code",
                "PostToolUse",
                session,
                edit_of(file, edit.0, edit.1),
            ),
            &here.join(".git"),
            here,
            levers,
        );
    }

    /// The largest pool this whole mechanism exists for: the agent read a file,
    /// edited it, and reads it back to see what it just wrote. The bytes it is
    /// asking for came out of its own payload, so the honest answer names the
    /// places that changed, in the numbering it is about to see, and lets the
    /// call run the moment it asks again.
    #[test]
    fn a_verification_read_after_a_full_read_and_an_edit_is_denied_once_with_exact_sites() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let here = tmp.path();
        let file = here.join("src.rs");
        let enforce = levers("claude-code", rift_recall_policy::Mode::Enforce);
        read_then_edit(
            here,
            "s-delta",
            &file,
            "a\nold\nc\n",
            "a\nnew\nnewer\nc\n",
            ("old\n", "new\nnewer\n"),
            &enforce,
        );

        let verify = hook("claude-code", "PreToolUse", "s-delta", whole(&file));
        let declined = gate_under(&verify, here, &enforce);
        let reason = declined.deny_reason.expect("a decline with a reason");
        assert!(
            reason.contains("changed only at lines 2-3"),
            "the places, in the numbering of the bytes it is about to see: {reason}"
        );
        assert!(
            reason.contains("(+1 lines)"),
            "and by how much the file moved: {reason}"
        );
        assert!(
            reason.contains("Read lines 1-4"),
            "and a window worth reading instead, inside the file: {reason}"
        );
        assert!(
            !reason.contains("newer"),
            "identifiers and numbers, never a byte of the file: {reason}"
        );

        // Once. The model that answers a decline by asking again is answered.
        let again = gate_under(&verify, here, &enforce);
        assert_eq!(again.deny_reason, None, "the repeat is honoured");
        assert_eq!(
            lines_of(here, "s-delta", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "edit", "deny", "read"],
            "declined once, then handed the bytes and recorded as holding them"
        );
    }

    /// Reading exactly the lines that changed is the right thing to do, so it
    /// is never the thing declined — the narrow read is the behaviour the
    /// frame asks for, and it must not be answered with another decline.
    #[test]
    fn a_read_inside_the_edited_site_is_allowed() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let here = tmp.path();
        let file = here.join("src.rs");
        let enforce = levers("claude-code", rift_recall_policy::Mode::Enforce);
        read_then_edit(
            here,
            "s-narrow",
            &file,
            "a\nold\nc\n",
            "a\nnew\nnewer\nc\n",
            ("old\n", "new\nnewer\n"),
            &enforce,
        );

        let narrow = gate_under(
            &hook(
                "claude-code",
                "PreToolUse",
                "s-narrow",
                read_of(&file, Some(2), Some(2)),
            ),
            here,
            &enforce,
        );
        assert_eq!(narrow.deny_reason, None, "the changed lines themselves");
    }

    /// ★ The floor the policy sets is the floor the gate uses.
    ///
    /// Every other walk in this module holds it at one so it does not stand in
    /// the way of the property under test; this one leaves the shipped value
    /// alone and re-reads a three-line file, which is far under it. The repeat
    /// is covered, the window holds it and enforcing is on — so the only
    /// reason the call runs is the trade, and the observation is still on the
    /// record. A wiring that dropped the floor between the policy and the
    /// decision would decline here.
    #[test]
    fn a_repeat_under_the_shipped_line_floor_is_observed_and_runs() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let here = tmp.path();
        let file = here.join("src.rs");
        let mut enforce = levers("claude-code", rift_recall_policy::Mode::Enforce);
        enforce.min_slice_lines = rift_recall_policy::DEFAULT_MIN_SLICE_LINES;
        std::fs::write(&file, "a\nb\nc\n").expect("the file");
        gate_under(
            &hook("claude-code", "PreToolUse", "s-floor", whole(&file)),
            here,
            &enforce,
        );

        let repeat = gate_under(
            &hook("claude-code", "PreToolUse", "s-floor", whole(&file)),
            here,
            &enforce,
        );
        assert_eq!(
            repeat.deny_reason, None,
            "three lines is under the floor, so the repeat runs"
        );
        assert_eq!(
            lines_of(here, "s-floor", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "observe"],
            "and the decision the floor held back is still recorded"
        );
    }

    /// The gate has one basis for a delta and it is the agent's own edit line.
    /// A change whose places do not follow from the payload leaves no such
    /// line, and reaching for a stored one would open a store on the path that
    /// must not — so the read is simply allowed.
    #[test]
    fn an_unreachable_basis_allows() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let here = tmp.path();
        let file = here.join("src.rs");
        let enforce = levers("claude-code", rift_recall_policy::Mode::Enforce);
        std::fs::write(&file, "a\nold\nc\n").expect("the file");
        gate_under(
            &hook("claude-code", "PreToolUse", "s-basis", whole(&file)),
            here,
            &enforce,
        );
        // Something other than this window changed the file: a rebase, a
        // formatter, another agent. No edit line, and no basis.
        std::fs::write(&file, "a\nsomething else\nc\n").expect("the change");

        let after = gate_under(
            &hook("claude-code", "PreToolUse", "s-basis", whole(&file)),
            here,
            &enforce,
        );
        assert_eq!(
            after.deny_reason, None,
            "nothing exact to say, so nothing said"
        );
        assert_eq!(
            lines_of(here, "s-basis", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "read"]
        );
    }

    /// The invariant the whole design rests on, asserted where it can actually
    /// be broken: a decline — the most expensive thing this path does, frame
    /// and all — is composed in a checkout that has no store of any kind, and
    /// leaves none behind.
    #[test]
    fn the_gate_path_never_opens_a_store_for_a_basis() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let here = tmp.path();
        let file = here.join("src.rs");
        let enforce = levers("claude-code", rift_recall_policy::Mode::Enforce);
        read_then_edit(
            here,
            "s-store",
            &file,
            "a\nold\nc\n",
            "a\nnew\nnewer\nc\n",
            ("old\n", "new\nnewer\n"),
            &enforce,
        );
        let declined = gate_under(
            &hook("claude-code", "PreToolUse", "s-store", whole(&file)),
            here,
            &enforce,
        );
        assert!(declined.deny_reason.is_some(), "the decline still happens");

        let mut found: Vec<String> = Vec::new();
        fn walk(dir: &Path, found: &mut Vec<String>) {
            for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, found);
                } else if path.extension().is_some_and(|ext| ext == "db") {
                    found.push(path.display().to_string());
                }
            }
        }
        walk(here, &mut found);
        assert!(
            found.is_empty(),
            "a decline that needed a database would have made one: {found:?}"
        );
    }

    /// A `view_file` as the pinned fields spell one.
    fn view_of(path: &Path, start: u32, end: u32) -> serde_json::Value {
        serde_json::json!({
            "toolCall": {
                "name": "view_file",
                "args": {
                    "AbsolutePath": path.display().to_string(),
                    "StartLine": start,
                    "EndLine": end,
                },
            },
        })
    }

    /// Retry telemetry never demotes Antigravity below repository-wide
    /// enforcement once its reason channel is pinned.
    #[test]
    fn repository_wide_enforce_reaches_antigravity_despite_retry_telemetry() {
        let (tmp, file) = checkout(60);
        let here = tmp.path();
        let call = hook("antigravity", "PreToolUse", "s-agy", view_of(&file, 0, 30));

        let mut wide = rift_recall_policy::ServePolicy::default();
        wide.interception.mode = rift_recall_policy::Mode::Enforce;
        let wide = rift_recall_policy::resolve(
            &wide,
            "antigravity",
            crate::recall::family_capability(&rift_core::AgentId::ANTIGRAVITY),
        );
        assert_eq!(wide.read_deny, rift_recall_policy::Mode::Enforce);
        assert_eq!(wide.read_deny_cap, None);
        gate_under(&call, here, &wide);
        assert!(
            gate_under(&call, here, &wide).deny_reason.is_some(),
            "the unchanged repeat is declined despite its retry history"
        );
    }

    /// A command line, as the family that has no read tool at all writes one.
    fn shell_of(command: &str) -> serde_json::Value {
        serde_json::json!({
            "tool_name": "Bash",
            "tool_input": { "command": command },
        })
    }

    /// A path as a POSIX command line spells it: the verbatim prefix dropped
    /// and `/` throughout. The shell-read grammar refuses `\` (a shell
    /// metacharacter) and `?` (a glob) by design, so a Windows `display()`
    /// would never be a read; `C:/Users/…` is still absolute to `Path` and
    /// canonicalises to the same file.
    fn shell_spelling(path: &Path) -> String {
        let spelled = path.display().to_string();
        let plain = spelled.strip_prefix(r"\\?\").unwrap_or(&spelled);
        plain.replace('\\', "/")
    }

    /// ★ **A read that arrives as a command line is declined once, and the
    /// second identical command line runs.**
    ///
    /// This family has no structured read tool, so every saving it can offer
    /// runs through the strict grammar: what the journal holds came off a
    /// command line, and so does the request matched against it. The walk is
    /// the whole rule in one session — the first read is the saving, the
    /// identical repeat is the decline, the repeat after that is honoured, and
    /// a narrower run of lines the window already holds is a new key declined
    /// on its own account.
    ///
    /// The spans are written the way the grammar pins them, which is why a
    /// command line stating its own range is what this walk reads with: a
    /// whole-file spelling ends where the client's result cap ends, not where
    /// the file does, so it produces no line coverage. That clause is asserted
    /// at the bottom rather than assumed away.
    #[test]
    fn a_shell_read_of_lines_the_window_holds_is_declined_once() {
        let (tmp, file) = checkout(40);
        let here = tmp.path();
        let enforce = levers("codex", rift_recall_policy::Mode::Enforce);
        assert_eq!(
            enforce.read_deny,
            rift_recall_policy::Mode::Enforce,
            "named deliberately, which is the only way this family is reached"
        );
        let ranged = hook(
            "codex",
            "PreToolUse",
            "s-codex",
            shell_of(&format!("sed -n \'1,40p\' {}", shell_spelling(&file))),
        );

        assert_eq!(
            gate_under(&ranged, here, &enforce).deny_reason,
            None,
            "the first read is the saving and is never declined"
        );
        let reason = gate_under(&ranged, here, &enforce)
            .deny_reason
            .expect("the identical repeat is declined");
        assert!(
            reason.contains("lines 1-40"),
            "the reason names the lines it is about: {reason}"
        );
        assert!(
            reason.contains("Repeat this exact call"),
            "and the way out of it: {reason}"
        );
        assert!(
            !reason.contains("line 4"),
            "identifiers and numbers, never a byte of the file: {reason}"
        );
        assert_eq!(
            gate_under(&ranged, here, &enforce).deny_reason,
            None,
            "one decline per key: the model that asks again is answered"
        );

        let narrower = hook(
            "codex",
            "PreToolUse",
            "s-codex",
            shell_of(&format!("sed -n \'5,20p\' {}", shell_spelling(&file))),
        );
        assert!(
            gate_under(&narrower, here, &enforce)
                .deny_reason
                .is_some_and(|reason| reason.contains("lines 5-20")),
            "a narrower run of lines the window already holds is its own key"
        );
        assert_eq!(
            lines_of(here, "s-codex", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "deny", "read", "deny"],
            "the record the walk left behind"
        );

        // Plain `cat` has byte-exact whole-file delivery semantics. It can
        // therefore establish complete coverage, be declined once when
        // unchanged, and then take the universal identical-retry escape.
        let whole_file = hook(
            "codex",
            "PreToolUse",
            "s-cat",
            shell_of(&format!("cat {}", shell_spelling(&file))),
        );
        assert_eq!(gate_under(&whole_file, here, &enforce).deny_reason, None);
        assert!(gate_under(&whole_file, here, &enforce)
            .deny_reason
            .is_some());
        assert_eq!(gate_under(&whole_file, here, &enforce).deny_reason, None);
        assert_eq!(
            lines_of(here, "s-cat", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "deny", "read"]
        );
    }

    /// ★ **A command line that reads several files is one act, and is
    /// declined only when declining every part of it would be right.**
    ///
    /// This is the rule the composed grammar made necessary. A decline answers
    /// the whole call, so a chain with one member the window does not hold has
    /// to run: declining it would cost the agent reads that were never in
    /// question, and amending the line to drop only the held members would
    /// mean rewriting a command Rift did not write. When every member *is*
    /// held, the reason is each member's own sentence from the one renderer
    /// allowed to write one, a line apiece.
    ///
    /// Both halves are walked, because a gate that declined nothing would pass
    /// the first on its own.
    #[test]
    fn a_command_line_of_several_reads_is_declined_only_when_every_part_is_held() {
        let (tmp, first) = checkout(40);
        let here = tmp.path();
        let second = beside(here, "other.rs", 40);
        let third = beside(here, "third.rs", 40);
        let enforce = levers("codex", rift_recall_policy::Mode::Enforce);
        let chain = |a: &Path, b: &Path| {
            format!(
                "cd {} && sed -n '1,40p' {} && sed -n '1,40p' {}",
                shell_spelling(here),
                a.file_name().expect("a name").to_string_lossy(),
                b.file_name().expect("a name").to_string_lossy(),
            )
        };
        let pair = hook(
            "codex",
            "PreToolUse",
            "s-chain",
            shell_of(&chain(&first, &second)),
        );

        assert_eq!(
            gate_under(&pair, here, &enforce).deny_reason,
            None,
            "the first pass is the saving and is never declined"
        );
        assert_eq!(
            lines_of(here, "s-chain", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "read"],
            "every member's read is recorded, not only the first"
        );

        let reason = gate_under(&pair, here, &enforce)
            .deny_reason
            .expect("a repeat whose every member is held is declined");
        let sentences: Vec<&str> = reason.lines().collect();
        assert_eq!(sentences.len(), 2, "one member to a line: {reason}");
        assert!(
            sentences[0].contains("other.rs") || sentences[1].contains("other.rs"),
            "the reason names every member it is about: {reason}"
        );
        assert!(
            sentences.iter().all(|line| line.contains("lines 1-40")),
            "each line carries its own member's range: {reason}"
        );

        // The escape hatch is per member, so the identical repeat runs — the
        // same one-decline-per-key rule a single read gets.
        assert_eq!(gate_under(&pair, here, &enforce).deny_reason, None);

        // …and a chain reaching one file the window has never been handed runs
        // whole, however much of the rest of it is held.
        let mixed = hook(
            "codex",
            "PreToolUse",
            "s-chain",
            shell_of(&chain(&first, &third)),
        );
        assert_eq!(
            gate_under(&mixed, here, &enforce).deny_reason,
            None,
            "one unheld member and the whole line runs"
        );
        assert!(
            lines_of(here, "s-chain", None)
                .iter()
                .filter(|line| line.kind() == "deny")
                .count()
                == 2,
            "and the members that would have been declined on their own are \
             recorded as the reads they became"
        );
    }

    /// ★ **A captioned chain is decided on its reads and on nothing else.**
    ///
    /// The `echo` lines an agent writes between two reads open no file, so
    /// they reach this gate as nothing at all: the same two reads are
    /// recorded, the same decline is reached when both are held, and the
    /// reason has one sentence per **read** rather than one per member. That
    /// last assertion is the one that would catch a label leaking into the
    /// request list — it would show up as a third line nobody can write, or as
    /// a member the resolution step silently dropped, which is the shape that
    /// would keep the whole line from ever being declined.
    #[test]
    fn a_captioned_chain_is_decided_on_its_reads_and_not_its_captions() {
        let (tmp, first) = checkout(40);
        let here = tmp.path();
        let second = beside(here, "other.rs", 40);
        let enforce = levers("codex", rift_recall_policy::Mode::Enforce);
        let captioned = hook(
            "codex",
            "PreToolUse",
            "s-caption",
            shell_of(&format!(
                "cd {} && echo \"=== one ===\" && sed -n '1,40p' {} && echo && \
                 sed -n '1,40p' {}",
                shell_spelling(here),
                first.file_name().expect("a name").to_string_lossy(),
                second.file_name().expect("a name").to_string_lossy(),
            )),
        );

        assert_eq!(
            gate_under(&captioned, here, &enforce).deny_reason,
            None,
            "the first pass is the saving"
        );
        assert_eq!(
            lines_of(here, "s-caption", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "read"],
            "two reads and two captions are two reads"
        );
        let reason = gate_under(&captioned, here, &enforce)
            .deny_reason
            .expect("a repeat whose every read is held is declined");
        assert_eq!(
            reason.lines().count(),
            2,
            "one sentence per read, and none for a caption: {reason}"
        );
        // A line that is captions and nothing else read nothing, so this gate
        // has nothing to resolve and nothing to record about it.
        let only_captions = hook(
            "codex",
            "PreToolUse",
            "s-caption",
            shell_of("echo \"=== one ===\" && echo done"),
        );
        assert_eq!(gate_under(&only_captions, here, &enforce).deny_reason, None);
        assert_eq!(
            lines_of(here, "s-caption", None)
                .iter()
                .filter(|line| line.kind() == "read")
                .count(),
            2,
            "no third read arrived from a line that read nothing"
        );
    }

    /// ★ Repository-wide enforcement reaches Codex and Claude Code equally;
    /// observed retry behavior changes telemetry, not effective policy.
    #[test]
    fn repository_wide_enforce_reaches_codex_despite_retry_telemetry() {
        let (tmp, file) = checkout(40);
        let here = tmp.path();
        let mut wide = rift_recall_policy::ServePolicy::default();
        wide.interception.mode = rift_recall_policy::Mode::Enforce;
        wide.interception.min_slice_lines = 1;
        let for_family = |family: &str| {
            rift_recall_policy::resolve(
                &wide,
                family,
                crate::recall::family_capability(&rift_core::AgentId::new(family)),
            )
        };

        let levers = for_family("codex");
        assert_eq!(levers.read_deny, rift_recall_policy::Mode::Enforce);
        assert_eq!(levers.read_deny_cap, None);
        let bounded = hook(
            "codex",
            "PreToolUse",
            "s-wide",
            shell_of(&format!("sed -n '1,40p' {}", shell_spelling(&file))),
        );
        gate_under(&bounded, here, &levers);
        assert!(
            gate_under(&bounded, here, &levers).deny_reason.is_some(),
            "the unchanged repeat is declined despite its retry history"
        );
        assert_eq!(
            lines_of(here, "s-wide", None)
                .iter()
                .map(Line::kind)
                .collect::<Vec<_>>(),
            vec!["read", "deny"],
            "the repeat is declined once"
        );

        let claude = for_family("claude-code");
        assert_eq!(claude.read_deny, rift_recall_policy::Mode::Enforce);
        let read = hook("claude-code", "PreToolUse", "s-wide-cc", whole(&file));
        gate_under(&read, here, &claude);
        assert!(
            gate_under(&read, here, &claude).deny_reason.is_some(),
            "the same file asks for the same thing and gets it here"
        );
    }

    /// ★ A read that verifies an edit is never declined on the family whose
    /// edit tool says nothing back.
    ///
    /// On the one family whose artifact quotes an edit result telling the model
    /// what it wrote, that verification read is the largest pool there is. Here
    /// nothing pins what the patch tool returns, so the read the agent makes to
    /// see its own change is the only sight of that change it has, and
    /// declining it would remove a verification rather than a repetition. The
    /// call runs, and it runs in `enforce` — this is a property of the pin, not
    /// of the mode.
    #[test]
    fn a_read_after_a_patch_is_allowed_where_nothing_pins_what_the_patch_returned() {
        let tmp = tempfile::tempdir().expect("a work tree");
        let here = tmp.path();
        let file = here.join("src.rs");
        let enforce = levers("codex", rift_recall_policy::Mode::Enforce);
        assert!(
            !enforce.pinned_edit_echo,
            "the fact this walk is about, read off the pinned table"
        );
        std::fs::write(&file, "a\nold\nc\n").expect("the file");

        let read = hook(
            "codex",
            "PreToolUse",
            "s-patch",
            shell_of(&format!("cat {}", shell_spelling(&file))),
        );
        gate_under(&read, here, &enforce);

        let patch = format!(
            "*** Begin Patch\n*** Update File: {}\n@@\n-old\n+new\n*** End Patch\n",
            file.display()
        );
        std::fs::write(&file, "a\nnew\nc\n").expect("the edit lands");
        edit_record(
            &AdapterRegistry::with_builtin(),
            &hook(
                "codex",
                "PostToolUse",
                "s-patch",
                serde_json::json!({
                    "tool_name": "apply_patch",
                    "tool_input": { "command": patch },
                }),
            ),
            &here.join(".git"),
            here,
            &enforce,
        );

        assert_eq!(
            gate_under(&read, here, &enforce).deny_reason,
            None,
            "the agent's only sight of what it just wrote is not taken away"
        );
    }

    /// A lever nobody is holding writes nothing at all. An unreadable policy
    /// resolves here, which is why this is the state a repository lands in when
    /// its file cannot be parsed rather than the state it has to ask for.
    #[test]
    fn a_lever_that_is_off_leaves_no_journal() {
        let (tmp, file) = checkout(60);
        let here = tmp.path();
        let off = levers("claude-code", rift_recall_policy::Mode::Off);
        let call = hook(
            "claude-code",
            "PreToolUse",
            "s-off",
            read_of(&file, Some(1), Some(30)),
        );
        gate_under(&call, here, &off);
        gate_under(&call, here, &off);
        assert!(lines_of(here, "s-off", None).is_empty());
    }
}
