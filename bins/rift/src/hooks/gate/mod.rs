//! The read gate: the silence precondition a read must pass, the journal step
//! that decides it, the audit row and journal entry a decision leaves, and the
//! frame a denied read is answered with. Separate because this is the
//! decision path the intercept gate walks: nothing reachable from here opens a
//! store or loads the tokenizer.
//!
//! This file is the step itself — what a hook does about the window journal,
//! and the pure readings of the repository's policy it does it under. The
//! concerns it leans on are one module each: what a call names
//! ([`mod@file`]), what a result says it delivered ([`delivery`]), how the
//! several reads of one call are decided and recorded ([`members`]), the
//! sentence and the accounting row a decision leaves ([`answer`]), and the
//! read an announcement made unnecessary ([`prevented`]).

mod answer;
mod delivery;
mod family;
mod file;
mod first_contact;
mod members;
mod outline_cache;
mod prevented;

use std::path::Path;

use rift_shadow::hooks::AdapterRegistry;

use self::delivery::stated_delivery;
use self::members::{gate_decide_members, gate_record_members, settle_whole_call, GatePass};
use self::prevented::prevented_record;
use super::doc_gate::{doc_gate, DocAnswer};
use super::ingest::edit_record;

pub(in crate::hooks) use self::answer::GateOutcome;
pub(crate) use self::answer::AUDIT_IDS;
pub(crate) use self::file::payload_cwd;
pub(in crate::hooks) use self::file::{gate_file, gate_rel_path};
pub(crate) use self::outline_cache::sweep_outline_cache;
pub(in crate::hooks) use self::outline_cache::{
    write_outline_cache, CachedDefinition, CachedOutline,
};

#[cfg(test)]
pub(in crate::hooks) use self::outline_cache::outline_cache_dir;

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

/// The window journal's side of one agent hook: what this context window is
/// being handed, what it has stopped holding, and what it did instead of
/// reading. Hands back the adapter registry it built along the way.
///
/// Runs before the ingest and opens no store: the journal is a file under this
/// checkout's own run state, the decision is pure, and the whole step is worth
/// a hash of at most a megabyte plus one read and one append. Every failure
/// leaves the journal with less than really happened, which costs a saving and
/// can never produce a decline of lines the window does not hold.
///
/// The registry comes back because one registry for the whole hook is the
/// point: the gate, the ingest and the worktree observation read the SAME
/// adapter's facts, so the `cwd`, the subagent and the read a row records can
/// never come from two different mappings of one payload.
///
/// The gate runs **before** the ingest, because the ingest is not on its
/// critical path, and before anything opens a store, because it never needs
/// one.
pub(in crate::hooks) fn window_journal_step(
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
    // lines is not a decision worth reaching.
    let documents = guarded_phase(t, "doc_gate", || {
        doc_gate(t, &registry, raw, git_dir, work_tree, &policy, &effective)
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
        guarded_phase(t, "read_gate", || {
            read_gate(&registry, raw, git_dir, work_tree, &effective)
        })
    };
    // What the window WROTE is coverage of a revision nobody read: the bytes
    // came out of the agent's own payload.
    guarded_phase(t, "edit_record", || {
        edit_record(&registry, raw, git_dir, work_tree, &effective);
    });
    // WHAT THE WINDOW DID **INSTEAD** OF READING: an act on a file Rift had
    // already told it that it holds. After the two above, because it is
    // decided over the lines they have just written — the read that ran, or
    // the edit that landed, is what answers an announcement, and a booking
    // taken before them would claim a read that is happening as one that did
    // not.
    let prevented = guarded_phase(t, "prevented_record", || {
        prevented_record(&registry, raw, git_dir, work_tree, &effective)
    });
    // A SPAWN, from either end: the spawning call's line in the session's main
    // journal, or the opening subagent's claim on it and what it inherits.
    guarded_phase(t, "spawn_record", || {
        spawn_record(&registry, raw, git_dir, work_tree, &effective);
    });
    // A SEARCH is decided apart from both: it names no lines of a file and no
    // document, so nothing above it can have answered it.
    let searches = guarded_phase(t, "search_gate", || {
        crate::search_gate::search_gate(t, &registry, raw, git_dir, work_tree, &policy, &effective)
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

/// One phase of the step, timed and behind its own panic guard.
///
/// The guard is per phase rather than around the step because each phase
/// writes a different kind of line: a panic in one must cost that line and
/// nothing else, so a phase that panicked answers with its own default — no
/// document, no decline, nothing journaled — and its neighbours still run.
fn guarded_phase<T: Default>(
    t: &crate::hooktime::HookTimer,
    name: &'static str,
    work: impl FnOnce() -> T,
) -> T {
    t.phase(name, || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_default()
    })
}

/// Either end of a spawn, under the same lever every journal line is: a
/// repository that switched reading off writes no spawn and no claim.
fn spawn_record(
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) {
    if effective.read_deny == rift_recall_policy::Mode::Off {
        return;
    }
    let window = registry
        .get(&raw.agent)
        .and_then(|adapter| adapter.facts(&raw.verb, &raw.payload).child_session);
    super::coordinator::spawn_step(&super::coordinator::SpawnHook {
        raw,
        work_tree,
        window: window.as_deref(),
        by_name: effective.inherit_by_name,
        turn: gate_turn(raw, git_dir),
        at_ms: u64::try_from(raw.occurred_at_ms).unwrap_or_default(),
    });
}

/// The gate itself, which the guard above is what keeps off the ingest's back.
///
/// Returns whether it journaled an accounting row, so the one worker this
/// hook spawns is spawned for that too.
pub(in crate::hooks) fn read_gate(
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    git_dir: &Path,
    work_tree: &Path,
    effective: &rift_recall_policy::Effective,
) -> GateOutcome {
    let quiet = GateOutcome::default();
    // A lever nobody is holding decides and records nothing: an unreadable
    // policy and an explicit `off` both land here, and no journal is written.
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
    let journal = crate::window_journal::WindowJournal::open(
        work_tree,
        &raw.session,
        facts.child_session.as_deref(),
    );
    let turn = gate_turn(raw, git_dir);
    let at_ms = u64::try_from(raw.occurred_at_ms).unwrap_or_default();
    if let GateStep::Evict(reason) = step {
        journal_eviction(&journal, reason, turn, at_ms);
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
    if cross_check_is_idle(&step, stated, &held) {
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
    let window_key = crate::window_journal::key(&raw.session, facts.child_session.as_deref());
    let pass = GatePass {
        step,
        git_dir,
        held: &held,
        journal: &journal,
        window_key: &window_key,
        stated,
        limits: gate_limits(effective),
        turn,
        at_ms,
    };
    // Phase one decides every member against ONE snapshot of the journal,
    // phase two decides the call as a whole, and phase three records both.
    let mut members =
        gate_decide_members(raw, work_tree, cwd.as_deref(), effective, &pass, &requests);
    let deny_reason = settle_whole_call(&mut members, requests.len());
    GateOutcome {
        journaled: gate_record_members(git_dir, work_tree, raw, &members, &pass),
        deny_reason,
    }
}

/// Write the line that says this window has stopped holding what it was given.
///
/// The lines before it are kept: they stop being coverage and become the map
/// of what the window has just lost.
fn journal_eviction(
    journal: &crate::window_journal::WindowJournal,
    reason: rift_agent::window_journal::EvictReason,
    turn: u32,
    at_ms: u64,
) {
    let _ = journal.append(&rift_agent::window_journal::Line::Evict {
        turn,
        reason,
        at_ms,
    });
}

/// Whether a cross-check has nothing to answer: no decline for the result to
/// contradict, and no statement about its own delivery to reconcile.
///
/// Answered from the journal alone, because the check must not cost a hash it
/// cannot use. The read behind such a result was recorded before the call ran.
fn cross_check_is_idle(
    step: &GateStep,
    stated: Option<rift_agent::window_journal::StatedDelivery>,
    held: &rift_agent::window_journal::Journal,
) -> bool {
    matches!(step, GateStep::CrossCheck)
        && stated.is_none()
        && !held
            .live()
            .iter()
            .any(|line| matches!(line, rift_agent::window_journal::Line::Deny { .. }))
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
pub(super) const fn gate_mode(mode: rift_recall_policy::Mode) -> rift_agent::window_journal::Mode {
    use rift_agent::window_journal::Mode as Journal;
    match mode {
        rift_recall_policy::Mode::Off => Journal::Off,
        rift_recall_policy::Mode::Observe => Journal::Observe,
        rift_recall_policy::Mode::Enforce => Journal::Enforce,
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
pub(super) const fn gate_edit_echo(
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
        inherit: rift_agent::window_journal::InheritModes {
            by_name: gate_mode(effective.inherit_by_name),
            ..rift_agent::window_journal::InheritModes::default()
        },
        ..rift_agent::window_journal::Limits::default()
    }
}

#[cfg(test)]
mod tests;
