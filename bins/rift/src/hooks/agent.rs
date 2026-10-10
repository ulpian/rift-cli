//! `rift hooks <agent> <verb>`: the stdin-JSON capture contract and the gate
//! dispatch that never fails the agent, plus the session-pointer injection
//! (tier a) and the pending-replacement handshake. Separate because this is
//! the process boundary: it reads stdin, decides, answers, and exits 0.

use std::io::Read as _;
use std::path::{Path, PathBuf};

use rift_core::Result;
use rift_shadow::hooks::AdapterRegistry;

use super::coordinator::{coordinator_section, Opening};
use super::doc_gate::DocAnswer;
use super::gate::{gate_file, AUDIT_IDS};
use super::ingest::try_ingest_agent_hook;
use super::manifest::{announce_now, held_now, turn_manifest};
use super::now_ms;
use super::recovery::{hook_number, recovery_map};

#[cfg(test)]
pub(super) use injection_tests::{drop_projection_store, set_up_here, AsRoot};

/// The document gate's conclusion as the decision it is, or `None` when it
/// reached none — which is the answer for almost every call.
///
/// Read out here rather than in the match below so that the composition of the
/// response reads the same in a build with the document seam and in one
/// without: this is where the two builds differ, and it is the whole of the
/// difference.
#[cfg(feature = "doc")]
fn document_decision(
    document: &Option<DocAnswer>,
) -> Option<rift_agent::respond::GateDecision<'_>> {
    match document {
        Some(DocAnswer::Rewrite {
            tool_input,
            sidecar,
        }) => Some(rift_agent::respond::GateDecision::Rewrite(
            rift_agent::respond::Rewrite::DocumentSidecar {
                tool_input,
                file_path: sidecar,
            },
        )),
        Some(DocAnswer::Redirect { reason }) => {
            Some(rift_agent::respond::GateDecision::Deny { reason })
        }
        None => None,
    }
}

/// The same read in a build without the document seam: the gate answers
/// nothing, so there is nothing to read.
#[cfg(not(feature = "doc"))]
fn document_decision(
    _document: &Option<DocAnswer>,
) -> Option<rift_agent::respond::GateDecision<'_>> {
    None
}

/// `rift hooks <agent> <verb>`: ingest one stdin-JSON hook and answer the
/// family's native contract. Diverges: always exit 0 — a capture error is
/// swallowed as "deferred", and an injection error means *no injection*, never
/// a stalled agent.
///
/// stdin is read exactly once, here: a hook gets one shot at it, and both the
/// capture pass and the serving decision need the same bytes. Each of those is
/// separately guarded, so a panic in either cannot take the other down or
/// escape into the agent's turn.
pub(super) fn run_agent_hook(agent: &str, args: &[String]) -> ! {
    let verb = args.first().map(String::as_str).unwrap_or_default();
    let mut body = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut body);
    record_hook_health(agent, Some("hook execution did not complete"));

    let mut decline: Option<String> = None;
    let mut document: Option<DocAnswer> = None;
    let mut search: Option<crate::search_gate::SearchAnswer> = None;
    let ingest = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        try_ingest_agent_hook(agent, verb, &body, &mut decline, &mut document, &mut search)
    }));
    let planned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        session_context_plan(agent, verb, &body, decline.as_deref())
    }));
    let mut failure = match (&ingest, &planned) {
        (Err(_), _) => Some("capture hook panicked"),
        (Ok(Err(_)), _) => Some("capture hook failed"),
        (_, Err(_)) => Some("context planning panicked"),
        _ => None,
    };
    let context = planned
        .as_ref()
        .map_or(rift_recall_types::ContextPlan::None, Clone::clone);

    let id = rift_core::AgentId::new(agent);
    // One object on stdout, composed in the one place a decision about a tool
    // call becomes JSON. A family without a confirmed reason channel answers
    // its neutral response there and the call runs, whatever this decided.
    let decision = if failure.is_none() {
        tool_call_decision(&document, decline.as_deref(), search.as_ref())
    } else {
        rift_agent::respond::GateDecision::Allow
    };
    let encoded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rift_agent::respond::respond_with_plan(&id, verb, &context, &decision)
    }));
    if encoded.is_err() {
        failure = Some("response encoding panicked");
    }
    if let Ok(Some(response)) = encoded {
        // `process::exit` below does not run destructors. Flush the native
        // response before best-effort bookkeeping so the client receives it
        // even when that bookkeeping fails.
        if write_response(&response).is_err() {
            failure = Some("hook response write failed");
        }
        let replacement_delivered = context.kind()
            == Some(rift_recall_types::ContextKind::ReadReplacement)
            && rift_agent::respond::delivers_read_replacement(&response);
        if matches!(decision, rift_agent::respond::GateDecision::Allow) || replacement_delivered {
            let bookkeeping = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                record_context_delivery(agent, verb, &body, &context);
                complete_pending_replacement(agent, &body, &context);
            }));
            if bookkeeping.is_err() && failure.is_none() {
                failure = Some("hook completion bookkeeping panicked");
            }
        }
    }
    record_hook_health(agent, failure);
    std::process::exit(0);
}

/// What the gates decided about this tool call, in the one shape the
/// response is composed from.
///
/// A document read is answered about the call the agent is making; a decline
/// of an unchanged re-read is about lines of a file that can be read as lines.
/// Only one of the two can ever be about the same call, and the document
/// answer is the one that knows the bytes are a container.
fn tool_call_decision<'a>(
    document: &'a Option<DocAnswer>,
    decline: Option<&'a str>,
    search: Option<&'a crate::search_gate::SearchAnswer>,
) -> rift_agent::respond::GateDecision<'a> {
    use crate::search_gate::SearchAnswer;
    use rift_agent::respond::{GateDecision, Rewrite};

    if let Some(answered) = document_decision(document) {
        return answered;
    }
    // A declined call has no input to amend, so the decline outranks the
    // bound — and the two can never be about one call anyway: a content
    // search delivers no lines of a file to hold.
    match (decline, search) {
        (Some(reason), _) => GateDecision::Deny { reason },
        // A name search is answered instead of run; a content search runs
        // with a bound it lacked. One call is never both, and neither is ever
        // the other kind of thing: an answered listing carries no amendment,
        // and a bounded search declines nothing.
        (None, Some(SearchAnswer::Listing(reason))) => GateDecision::Deny { reason },
        (None, Some(SearchAnswer::Cap(cap))) => GateDecision::Rewrite(Rewrite::GrepHeadLimit {
            tool_input: &cap.tool_input,
            head_limit: cap.head_limit,
        }),
        (None, None) => GateDecision::Allow,
    }
}

/// Write the native response and flush it.
///
/// `process::exit` does not run destructors, so the flush is explicit: the
/// response reaches the client before the best-effort bookkeeping that follows
/// it, even when that bookkeeping fails.
fn write_response(response: &serde_json::Value) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{response}")?;
    stdout.flush()
}

pub(super) fn hook_health_path(repo: &Path, agent: &str) -> PathBuf {
    repo.join(".rift/state/support").join(format!(
        "{}.json",
        rift_hashchain::leaf_hash(agent.as_bytes()).to_hex()
    ))
}

/// Persist only the latest transient hook failure. Writing an in-progress
/// marker before work begins also turns a client timeout/process kill into a
/// degradation the next status call can see. A completed hook clears it.
fn record_hook_health(agent: &str, failure: Option<&str>) {
    let Some(repo) = std::env::current_dir()
        .ok()
        .map(|cwd| crate::worktree_root(&cwd))
    else {
        return;
    };
    // The same "set up on this machine" the commands gate on: a hook firing
    // in a clone whose `.rift/` arrived with the repository has no family
    // projection to be healthy against, and must not scaffold state to say so.
    if !crate::family::provisioned_here(&repo) {
        return;
    }
    record_hook_health_in(&repo, agent, failure);
}

pub(super) fn record_hook_health_in(repo: &Path, agent: &str, failure: Option<&str>) {
    let path = hook_health_path(repo, agent);
    if let Some(reason) = failure {
        let Some(parent) = path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let temp = parent.join(format!(".health-{}", std::process::id()));
        let bytes = serde_json::json!({"schema_version": 1, "reason": reason})
            .to_string()
            .into_bytes();
        if std::fs::write(&temp, bytes).is_ok() && std::fs::rename(&temp, path).is_err() {
            let _ = std::fs::remove_file(temp);
        }
    } else {
        let _ = std::fs::remove_file(path);
    }
}

pub(crate) fn hook_degradation(repo: &Path, agent: &str) -> Option<String> {
    let path = hook_health_path(repo, agent);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some("hook health state is unreadable".to_owned()),
    };
    if !metadata.file_type().is_file() || metadata.len() > 4_096 {
        return Some("hook health state is invalid".to_owned());
    }
    let Ok(bytes) = std::fs::read(path) else {
        return Some("hook health state is unreadable".to_owned());
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Some("hook health state is malformed".to_owned());
    };
    match (
        value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64),
        value.get("reason").and_then(serde_json::Value::as_str),
    ) {
        (Some(1), Some(reason)) if !reason.is_empty() => Some(reason.to_owned()),
        _ => Some("hook health state is malformed".to_owned()),
    }
}

/// Plan the one context payload this boundary may deliver. The dispatcher,
/// not the native adapter, applies priority and measures the selected text.
fn session_context_plan(
    agent: &str,
    verb: &str,
    body: &[u8],
    decline: Option<&str>,
) -> rift_recall_types::ContextPlan {
    let id = rift_core::AgentId::new(agent);
    let direct_replacement = (id == rift_core::AgentId::CODEX && verb == "PreToolUse")
        .then_some(decline)
        .flatten()
        .filter(|reason| !reason.is_empty())
        .map(|reason| rift_recall_types::ContextCandidate {
            kind: rift_recall_types::ContextKind::ReadReplacement,
            text: reason.to_owned(),
            avoided_tokens: 0,
            fallback: false,
        });
    if !rift_agent::respond::accepts_injection(&id, verb) {
        return rift_recall_serve::ServeDispatcher::plan_context(direct_replacement);
    }
    let Some(repo) = std::env::current_dir()
        .ok()
        .map(|cwd| crate::worktree_root(&cwd))
    else {
        return rift_recall_types::ContextPlan::None;
    };
    let replacement = pending_replacement_candidate(&repo, &id, verb, body);
    let boundary = boundary_is_open(&id, verb, body)
        .then(|| session_context_candidate_in(&repo, &id, verb, body))
        .flatten();
    rift_recall_serve::ServeDispatcher::plan_context(
        direct_replacement
            .into_iter()
            .chain(replacement)
            .chain(boundary),
    )
}

/// Test seam for the family-and-verb half of boundary planning, with the
/// repository it would speak about supplied **lazily**.
///
/// The laziness is the contract, not a convenience: a family or verb with no
/// verified channel is refused without the root ever being asked for, so the
/// refusal cannot depend on — or write into — whatever checkout the process
/// happens to stand in. That also lets a test state the refusal over a
/// repository of its own and assert that it was never even opened.
#[cfg(test)]
fn session_pointer_rooted<F>(agent: &str, verb: &str, body: &[u8], root: F) -> Option<String>
where
    F: FnOnce() -> Option<std::path::PathBuf>,
{
    let id = rift_core::AgentId::new(agent);
    if !rift_agent::respond::accepts_injection(&id, verb) {
        return None;
    }
    if !boundary_is_open(&id, verb, body) {
        return None;
    }
    session_pointer_in(&root()?, &id, verb, body)
}

/// Whether this delivery of a boundary hook is one Rift may speak on — the
/// per-payload half of the family-and-verb question `session_pointer_rooted` has
/// already answered.
///
/// One family needs it. Antigravity's boundary hook fires before **every**
/// model invocation, not once a turn: a channel that ignored the counter would
/// repeat the same pointer through a whole tool loop, which is the one way an
/// injection stops being a saving and becomes a wall of text. Its own payload
/// numbers the invocations from zero, and the first is the execution boundary —
/// the same place the family's implicit session start is read from.
///
/// A payload that does not name the counter is not a first invocation. The
/// direction is the conservative one everywhere: an unreadable or unnumbered
/// delivery costs a pointer, never a repetition.
pub(super) fn boundary_is_open(agent: &rift_core::AgentId, verb: &str, body: &[u8]) -> bool {
    if *agent != rift_core::AgentId::ANTIGRAVITY || verb != "PreInvocation" {
        return true;
    }
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(body) else {
        return false;
    };
    hook_number(&payload, "invocationNum") == Some(0)
}

/// The per-window pending replacement leaf. Session text is never a path
/// component; the same screened digest convention is used by injection state.
fn pending_replacement_path(repo: &Path, session: &str) -> PathBuf {
    repo.join(".rift/state/replacements").join(format!(
        "{}.json",
        rift_hashchain::leaf_hash(session.as_bytes()).to_hex()
    ))
}

/// Build a bounded structural alternative after the installed Antigravity
/// client chooses its unpinned `ContentOffset` pagination. The raw call still
/// runs untouched; the next model invocation receives this outline through
/// the pinned persistent-message channel instead of being left to probe more
/// offsets with no trustworthy coverage semantics.
fn antigravity_content_offset_plan(
    work_tree: &Path,
    raw: &rift_shadow::hooks::RawHook,
) -> Option<String> {
    if raw.agent != rift_core::AgentId::ANTIGRAVITY || raw.verb != "PreToolUse" {
        return None;
    }
    let args = raw.payload.pointer("/toolCall/args")?.as_object()?;
    args.get("ContentOffset")?.as_u64()?;
    let path = args.get("AbsolutePath")?.as_str()?;
    let file = gate_file(
        work_tree,
        path,
        None,
        rift_recall_policy::DEFAULT_MAX_INLINE_HASH_BYTES,
    )?;
    let text = String::from_utf8_lossy(&file.bytes);
    let mut landmarks = text
        .lines()
        .enumerate()
        .filter(|(_, line)| line.trim_start().starts_with('#'))
        .map(|(index, line)| {
            let heading: String = line.chars().take(120).collect();
            format!("L{} {heading}", index + 1)
        })
        .take(32)
        .collect::<Vec<_>>();
    if landmarks.is_empty() {
        let mut lo = 1_u32;
        while lo <= file.total_lines && landmarks.len() < 8 {
            let hi = lo.saturating_add(119).min(file.total_lines);
            landmarks.push(format!("L{lo}-L{hi}"));
            lo = hi.saturating_add(1);
        }
    }
    Some(format!(
        "[rift] Bounded structural read plan for `{}`. ContentOffset request/result semantics are unverified, so the raw call ran untouched. Use explicit line spans around these landmarks instead of probing more offsets:\n{}",
        file.rel_path,
        landmarks.join("\n")
    ))
}

/// Arm Antigravity's documented persistent-message fallback. The preferred
/// native tool-call step is intentionally not used: execution and
/// result delivery are not pinned. This bounded state is consumed only after
/// a native response was actually encoded; if anything fails, the existing
/// one-deny guard allows the identical raw call once.
fn arm_pending_replacement(repo: &Path, raw: &rift_shadow::hooks::RawHook, message: &str) {
    if raw.agent != rift_core::AgentId::ANTIGRAVITY || raw.session.is_empty() {
        return;
    }
    let message: String = message.chars().take(4_096).collect();
    let path = pending_replacement_path(repo, &raw.session);
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let temp = parent.join(format!(
        ".pending-{}",
        rift_hashchain::leaf_hash(format!("{}\0{message}", raw.session).as_bytes()).to_hex()
    ));
    let payload = serde_json::json!({
        "schema_version": 1,
        "message": message,
        "created_at_ms": raw.occurred_at_ms,
    });
    let wrote = (|| -> std::io::Result<()> {
        use std::io::Write as _;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)?;
        file.write_all(payload.to_string().as_bytes())
    })();
    if wrote.is_ok() {
        let _ = std::fs::rename(&temp, path);
    } else {
        let _ = std::fs::remove_file(temp);
    }
}

pub(super) fn arm_agent_replacement(
    work_tree: &Path,
    raw: &rift_shadow::hooks::RawHook,
    decline: Option<&str>,
) {
    if let Some(reason) = decline {
        arm_pending_replacement(
            work_tree,
            raw,
            &format!("[rift] Prepared result for the declined unchanged read:\n{reason}"),
        );
    } else if let Some(plan) = antigravity_content_offset_plan(work_tree, raw) {
        arm_pending_replacement(work_tree, raw, &plan);
    }
}

/// Read, but do not consume, the prepared persistent-message fallback.
fn pending_replacement_candidate(
    repo: &Path,
    agent: &rift_core::AgentId,
    verb: &str,
    body: &[u8],
) -> Option<rift_recall_types::ContextCandidate> {
    use rift_recall_types::{ContextCandidate, ContextKind};

    if *agent != rift_core::AgentId::ANTIGRAVITY || verb != "PreInvocation" {
        return None;
    }
    let session = payload_session_id(body)?;
    let bytes = std::fs::read(pending_replacement_path(repo, &session)).ok()?;
    if bytes.len() > 8_192 {
        return None;
    }
    let payload: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    if payload
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        != Some(1)
    {
        return None;
    }
    let message = payload.get("message")?.as_str()?.trim();
    if message.is_empty() {
        return None;
    }
    Some(ContextCandidate {
        kind: ContextKind::ReadReplacement,
        text: message.to_owned(),
        avoided_tokens: 0,
        fallback: true,
    })
}

/// Consume a pending replacement only after its native response exists.
fn complete_pending_replacement(agent: &str, body: &[u8], plan: &rift_recall_types::ContextPlan) {
    if agent != rift_core::AgentId::ANTIGRAVITY.as_str()
        || plan.kind() != Some(rift_recall_types::ContextKind::ReadReplacement)
    {
        return;
    }
    let Some(session) = payload_session_id(body) else {
        return;
    };
    let Some(repo) = std::env::current_dir()
        .ok()
        .map(|cwd| crate::worktree_root(&cwd))
    else {
        return;
    };
    complete_pending_replacement_in(&repo, &session, plan);
}

fn complete_pending_replacement_in(
    repo: &Path,
    session: &str,
    plan: &rift_recall_types::ContextPlan,
) {
    if plan.kind() == Some(rift_recall_types::ContextKind::ReadReplacement) {
        let _ = std::fs::remove_file(pending_replacement_path(repo, session));
    }
}

/// The repo-rooted half of `session_pointer_rooted`, once the family and verb have
/// been cleared: everything from here on is a fact about *this repository's*
/// state, so it is expressed against an explicit root rather than the process
/// cwd — which is also what lets a test drive the whole chain (claim → spend)
/// instead of its primitives.
///
/// Both injection tiers leave through here, and the claim is
/// what routes between them. Winning it means this is the session's first
/// prompt, which gets the pointer (tier a). Losing it means the session already
/// has the pointer, so the turn is a candidate for the per-turn delta manifest
/// (tier b, [`turn_manifest`]). One entry point, because the family/verb gate
/// and the Recall gate above are the same for both, and a second entry point
/// would be a second place to forget one of them.
#[cfg(test)]
pub(super) fn session_pointer_in(
    repo: &std::path::Path,
    agent: &rift_core::AgentId,
    verb: &str,
    body: &[u8],
) -> Option<String> {
    session_context_candidate_in(repo, agent, verb, body).map(|candidate| candidate.text)
}

/// Build at most one semantic candidate. Native response shapes never enter
/// this function; the dispatcher turns this value into the sole ContextPlan.
fn session_context_candidate_in(
    repo: &std::path::Path,
    agent: &rift_core::AgentId,
    verb: &str,
    body: &[u8],
) -> Option<rift_recall_types::ContextCandidate> {
    use rift_recall_types::{ContextCandidate, ContextKind};

    let cfg = rift_config::EffectiveConfig::load(&repo.join(".rift")).ok()?;
    // Two questions, one gate: is Recall on here, and is the repository set
    // up on this machine. The second is the same file every command asks —
    // Recall is on unless declared off, so without it a hook fired in a
    // directory that belongs to no checkout, or in a clone whose `.rift/`
    // arrived with the repository, would claim a pointer and write the
    // marker for it where there is no projection to serve from.
    if !cfg.recall_enabled() || !crate::family::provisioned_here(repo) {
        return None;
    }
    let session = payload_session_id(body)?;
    // The context window, not the session — the same key the gate writes the
    // window journal under, so a subagent claims its own pointer and its own
    // baseline instead of sharing its parent's.
    let window = context_window(agent, verb, body);
    // Tier (c) is asked first, because it is the one turn where the answer is
    // not about what changed but about what the context stopped holding: a
    // session that has just been compacted needs its map more than it needs the
    // pointer it was already given, and one hook writes one object.
    if let Some(map) = recovery_map(repo, &cfg, agent, verb, body) {
        return Some(ContextCandidate {
            kind: ContextKind::RecoveryMap,
            text: map,
            avoided_tokens: 0,
            fallback: false,
        });
    }
    if let Some(pointer) = claimed_pointer(repo, agent, verb, &session, window.as_deref()) {
        return Some(pointer);
    }
    // A window that opens with nothing read has no delta to be told about, so
    // the manifest tier does not run at the verb that opens one: what such a
    // window is told about its coordinator rode with its pointer, and a
    // subagent whose pointer is already claimed (a retried hook) is answered
    // with silence rather than with its parent's news twice.
    if window_opens_here(agent, verb) {
        return None;
    }
    turn_manifest(
        repo,
        &session,
        window.as_deref(),
        boundary_ceiling(agent, verb),
        &|| held_now(repo, agent, verb, body),
        &|named| announce_now(repo, agent, verb, body, named),
    )
    .map(|text| ContextCandidate {
        kind: ContextKind::DeltaManifest,
        text,
        avoided_tokens: 0,
        fallback: false,
    })
}

/// The session pointer, when this window has not been handed one yet and this
/// process wins the claim to hand it.
///
/// The claim is once per window, so whatever rides with the pointer is said
/// once too: a retried hook finds the claim taken and says nothing at all.
fn claimed_pointer(
    repo: &Path,
    agent: &rift_core::AgentId,
    verb: &str,
    session: &str,
    window: Option<&str>,
) -> Option<rift_recall_types::ContextCandidate> {
    let claim = claim_injection(repo, session, window)?;
    let text = opening_text(repo, agent, verb, session, window);
    // The pointer is spend: measure it with the real tokenizer and record
    // it against the same savings ledger the serves land in, keyed by the
    // session that paid for it, so the estimate stays honest about what
    // injection costs: no token class is exempt from accounting. The
    // coordinator section is measured in the same string, so its tokens are
    // spend on the same row.
    let attribution = super::audit::serve_attribution(
        Some(session),
        Some(&crate::window_journal::key(session, window)),
    );
    record_injection_spend(
        repo,
        rift_tokens::count(&text),
        &claim,
        attribution.as_ref(),
    );
    Some(rift_recall_types::ContextCandidate {
        kind: rift_recall_types::ContextKind::SessionPointer,
        text,
        avoided_tokens: 0,
        fallback: false,
    })
}

/// The pointer, and after it, at the boundary that opens a subagent's window,
/// the section naming what its coordinator held at that instant.
///
/// Only there: the main thread's window has no coordinator, and every later
/// boundary of a subagent's window is the manifest's, which speaks about what
/// that window has read itself.
fn opening_text(
    repo: &Path,
    agent: &rift_core::AgentId,
    verb: &str,
    session: &str,
    window: Option<&str>,
) -> String {
    let pointer = crate::agent_help::session_pointer();
    let (true, Some(window)) = (window_opens_here(agent, verb), window) else {
        return pointer;
    };
    let opening = Opening {
        repo,
        agent,
        verb,
        session,
        window,
    };
    match coordinator_section(&opening, &pointer) {
        Some(section) => format!("{pointer}\n{section}"),
        None => pointer,
    }
}

/// Whether this boundary is the one that **opens** a context window rather than
/// one that punctuates a running one.
///
/// Only one verb is, and only on one family: Claude Code's `SubagentStart`
/// fires once, at the moment a Task-spawned window exists and has read
/// nothing. The pointer belongs there — it is the only boundary that window
/// ever gets — and the delta manifest does not, because a manifest asserts
/// that files *this window has read* changed, and this window has read none.
fn window_opens_here(agent: &rift_core::AgentId, verb: &str) -> bool {
    *agent == rift_core::AgentId::CLAUDE_CODE && verb == "SubagentStart"
}

/// The context window a boundary payload belongs to, or `None` for an ordinary
/// main-thread boundary.
///
/// Read through the family's own `facts`, exactly the way the read gate reads
/// it ([`read_gate`](super::gate::read_gate)), so the window a pointer is claimed for and the window a
/// read is decided against are one key rather than two spellings of one. A
/// family whose payload cannot tell a child from its parent answers `None`
/// here and keeps the single main-thread window it always had.
fn context_window(agent: &rift_core::AgentId, verb: &str, body: &[u8]) -> Option<String> {
    let raw = rift_shadow::hooks::build_raw_hook(agent.as_str(), verb, body, now_ms()).ok()?;
    AdapterRegistry::with_builtin()
        .get(agent)
        .map(|adapter| adapter.facts(verb, &raw.payload))
        .and_then(|facts| facts.child_session)
}

/// The token ceiling one boundary response must fit inside, for a family that
/// bounds its context channel by a number Rift declared when it installed the
/// hooks — `None` where the family bounds it by nothing.
///
/// This is the whole of the family-specific part, and it is read off the
/// capability table rather than kept here, so the number the hook fits to is
/// the number the installer wrote and there is nowhere for a second one to
/// live.
pub(super) fn boundary_ceiling(agent: &rift_core::AgentId, verb: &str) -> Option<u32> {
    rift_agent::respond::declared_context_limit(agent, verb)
}

/// The budget one boundary response is actually rendered against: Rift's own,
/// lowered to the family's declared ceiling where there is one.
///
/// **Lowered, never raised.** The operator's budget is a ceiling on spend and
/// the family's is a ceiling on the channel; a response has to be under both,
/// and the fit that follows drops whole entries to get there. What it must
/// never do is leave the text over the family's number and let the client
/// deal with it: the client keeps a head and a tail, spills the middle to a
/// file and says so, which hands the model a frame cut in two places and a
/// path it will not open (recorded on codex-cli 0.153.1; the family's fixture
/// carries the run).
pub(super) fn boundary_budget(budget: u32, ceiling: Option<u32>) -> u32 {
    ceiling.map_or(budget, |ceiling| budget.min(ceiling))
}

/// The native session identifier a hook payload names, if any. Total: an
/// unparseable or session-less payload simply yields no pointer.
///
/// The spellings are the **wire parser's own table, in its order**, rather
/// than a second list kept here: the claim below has to be keyed by the id
/// capture bound this session under, or a family that spells its
/// conversation differently would be introduced again at every boundary. One
/// key is added at the end — `session` is the envelope's, not any family's,
/// and it is asked last for exactly that reason. A tool-use id names no
/// session on any family, and is refused here as it is there.
fn payload_session_id(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    for key in rift_shadow::hooks::SESSION_ID_FIELDS
        .iter()
        .copied()
        .chain(std::iter::once("session"))
    {
        if let Some(s) = v.get(key).and_then(serde_json::Value::as_str) {
            if !s.is_empty() && !s.starts_with(rift_shadow::hooks::TOOL_USE_ID_PREFIX) {
                return Some(s.to_owned());
            }
        }
    }
    None
}

/// Win the once-per-**window** right to inject, atomically. `Some(id)` means
/// this process created the marker and no other ever will, and hands back the
/// hashed window key that keys both the marker and the spend row; `None` means
/// the marker already existed (or could not be persisted), and both are
/// answered with silence.
///
/// **Keyed by the window, not the session**, through
/// [`crate::window_journal::key`] itself rather than a second statement of its
/// rule: a subagent's hooks arrive under the parent's session id and carry
/// their own agent id, so a marker keyed by the session alone is one marker
/// for two contexts — the parent takes the pointer and the subagent, which has
/// no other boundary at all, is never introduced. The agent id's absence (an
/// ordinary main-thread boundary) is a distinct, empty part of the key rather
/// than a missing one, which is why every main-thread marker written before
/// this change has a different name now: the cost is one duplicate pointer per
/// live session at rollout, and nothing else.
///
/// The marker is local run state under `.rift/state/`, not a canonical fact:
/// deleting it costs one duplicate pointer, never a correctness problem.
fn claim_injection(
    repo: &std::path::Path,
    session: &str,
    agent_id: Option<&str>,
) -> Option<String> {
    let dir = repo.join(".rift").join("state").join("injection");
    std::fs::create_dir_all(&dir).ok()?;
    // The session and agent ids are agent-supplied text; their hash is the
    // filename so no payload string ever becomes a path component.
    let name = crate::window_journal::key(session, agent_id);
    let path = dir.join(format!("{name}.json"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .ok()?;
    use std::io::Write as _;
    let _ = file.write_all(
        serde_json::json!({
            "schema_version": 1,
            "session": session,
            "agent_id": agent_id,
            "decided_at_ms": now_ms(),
        })
        .to_string()
        .as_bytes(),
    );
    Some(name)
}

/// Append the injected pointer's cost to the serve-audit relation, attributed
/// to the session and window that were handed it. Best-effort and
/// never-block: an unavailable projection costs an audit row, not a turn.
fn record_injection_spend(
    repo: &std::path::Path,
    tokens: u32,
    occurrence: &str,
    attribution: Option<&rift_recall_ledger::attribution::ServeAttribution>,
) {
    let envelope = rift_recall_serve::injection_audit_envelope(tokens, occurrence);
    let _ = (|| -> Result<()> {
        let db = crate::family::recall_db(repo)?;
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())?;
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(rift_core::RiftError::from)?;
        rt.block_on(rift_recall_ledger::attribution::append_attributed_serve(
            &store,
            envelope.as_bytes(),
            now_ms(),
            attribution,
        ))
    })();
}

/// Append the channel-attributed half of a boundary delivery after a native
/// encoder has actually produced a response. Pointer and delta spend already
/// have their historical `memd.manifest_injected` row; this adds the existing
/// `serve_call_event` sibling needed to prove which agent received them.
/// Recovery maps already write both relations in `record_recovery_injection`.
///
/// The row is attributed to the session the payload names and the window
/// inside it. Read here, after the response is already on its way, because
/// this is bookkeeping and never part of what the hook decided.
fn record_context_delivery(
    agent: &str,
    verb: &str,
    body: &[u8],
    plan: &rift_recall_types::ContextPlan,
) {
    let Some(repo) = std::env::current_dir()
        .ok()
        .map(|cwd| crate::worktree_root(&cwd))
    else {
        return;
    };
    if !matches!(plan, rift_recall_types::ContextPlan::Deliver { .. }) {
        return;
    }
    let attribution = payload_session_id(body).and_then(|session| {
        let window = context_window(&rift_core::AgentId::new(agent), verb, body);
        let key = crate::window_journal::key(&session, window.as_deref());
        super::audit::serve_attribution(Some(&session), Some(&key))
    });
    record_context_delivery_in(&repo, agent, plan, attribution.as_ref());
}

fn record_context_delivery_in(
    repo: &Path,
    agent: &str,
    plan: &rift_recall_types::ContextPlan,
    attribution: Option<&rift_recall_ledger::attribution::ServeAttribution>,
) {
    use rift_core::UlidSource as _;
    use rift_recall_types::{ContextKind, ContextPlan};

    let ContextPlan::Deliver {
        kind,
        delivered_tokens,
        fallback,
        ..
    } = plan
    else {
        return;
    };
    if *kind == ContextKind::RecoveryMap {
        return;
    }
    let step = match kind {
        ContextKind::SessionPointer => "session-pointer",
        ContextKind::DeltaManifest => "delta-manifest",
        ContextKind::RecoveryMap => return,
        ContextKind::ReadReplacement if *fallback => "read-replacement-fallback",
        ContextKind::ReadReplacement => "read-replacement",
    };
    let _ = (|| -> Result<()> {
        let db = crate::family::recall_db(repo)?;
        let store = rift_store::open_migrated(&db, &rift_store::recall_migrations())?;
        let call_id = AUDIT_IDS.next_ulid().to_hex();
        let at = now_ms();
        let event = rift_recall_serve::MemdEvent::ManifestInjected {
            estimated_tokens_returned: *delivered_tokens,
        };
        let called = event.to_serve_call_json(
            rift_recall_serve::ServeChannel::HookInject,
            &call_id,
            &format!("{agent}:{step}"),
            rift_core::Timestamp::from_millis(at),
        );
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .map_err(rift_core::RiftError::from)?;
        rt.block_on(rift_recall_ledger::attribution::append_attributed_serve(
            &store,
            called.as_bytes(),
            at,
            attribution,
        ))
    })();
}

#[cfg(test)]
mod injection_tests {
    use super::*;

    /// Set a fixture up **on this machine**: its family's projection under
    /// the cache root, the one fact `provisioned_here` reads. Recall on in
    /// the file is not enough — the injector asks both questions, so a
    /// fixture without a projection exercises the "records nothing" arm.
    pub(in crate::hooks) fn set_up_here(repo: &std::path::Path) {
        crate::projection::open_projection(repo).expect("the fixture's family projection opens");
    }

    /// Remove the projection `set_up_here` created under the machine cache.
    pub(in crate::hooks) fn drop_projection_store(repo: &std::path::Path) {
        if let Ok(db) = crate::family::projection_db(repo) {
            if let Some(dir) = db.parent() {
                let _ = std::fs::remove_dir_all(dir);
            }
        }
    }

    /// A Recall-enabled repository set up on this machine, and the path of
    /// its family's recall store.
    ///
    /// A real gitdir, because injection books its spend in the FAMILY's recall
    /// store and a family belongs to a repository. The projection lives in the
    /// machine cache, keyed by this temp repo's family (which adopts the main
    /// work tree's legacy ids, so it is this path's) — so it is the test's to
    /// create and, through [`drop_recall_repo`], the test's to remove.
    fn recall_repo() -> (tempfile::TempDir, PathBuf) {
        let repo = tempfile::tempdir().unwrap();
        rift_git::test_support::fake_repo(repo.path()).unwrap();
        std::fs::create_dir_all(repo.path().join(".rift")).unwrap();
        std::fs::write(
            repo.path().join(".rift").join("config.toml"),
            "schema_version = 1\n[project]\nname = \"t\"\n[features]\ntotal_recall = true\n",
        )
        .unwrap();
        let db = crate::family::recall_db(repo.path()).unwrap();
        set_up_here(repo.path());
        (repo, db)
    }

    /// Remove what [`recall_repo`] created under the machine cache.
    fn drop_recall_repo(repo: &tempfile::TempDir, db: &Path) {
        if let Some(dir) = db.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
        drop_projection_store(repo.as_root());
    }

    /// The fixture's root for a cleanup, whether the test holds its `repo`
    /// as a `TempDir` or as a path — the tests spell it both ways.
    pub(in crate::hooks) trait AsRoot {
        fn as_root(&self) -> &std::path::Path;
    }
    impl AsRoot for tempfile::TempDir {
        fn as_root(&self) -> &std::path::Path {
            self.path()
        }
    }
    impl AsRoot for std::path::Path {
        fn as_root(&self) -> &std::path::Path {
            self
        }
    }
    impl AsRoot for std::path::PathBuf {
        fn as_root(&self) -> &std::path::Path {
            self
        }
    }

    /// The family and the verb the injection chain is driven on here, spelled
    /// once: a pointer is due at any of this family's boundaries, and the
    /// prompt is the one every turn has.
    const CC: rift_core::AgentId = rift_core::AgentId::CLAUDE_CODE;
    const PROMPT: &str = "UserPromptSubmit";
    /// The prompt body the positive path is driven with.
    const FIRST_PROMPT: &[u8] = br#"{"session_id":"s-1"}"#;

    /// The dedup property this surface guarantees: **at most one pointer per
    /// context window**, whatever the ids look like and however many times a
    /// hook is retried, crashed, or run concurrently. The claim is the whole
    /// mechanism, so the property is stated over the claim.
    ///
    /// The agent id is swept alongside the session id because that is the axis
    /// the claim gained: a subagent's hooks arrive under its parent's session,
    /// so "once per session" would have been a latch that silenced every
    /// window but the first. `None` — the ordinary main thread — is one of the
    /// windows swept, and is distinct from every named one.
    #[test]
    fn at_most_one_pointer_is_ever_claimed_per_context_window() {
        use proptest::prelude::*;

        proptest!(|(ids in proptest::collection::vec("\\PC{0,64}", 1..4),
                    agents in proptest::collection::vec(proptest::option::of("\\PC{0,32}"), 1..4),
                    retries in 1usize..6)| {
            let repo = tempfile::tempdir().expect("tempdir");
            // The generator may draw one pair twice; "once per window" is a
            // claim about *distinct* windows, and re-presenting a pair is the
            // retry axis `retries` already sweeps — so state it over the set.
            let windows: std::collections::BTreeSet<(&str, Option<&str>)> = ids
                .iter()
                .map(String::as_str)
                .filter(|id| !id.is_empty())
                .flat_map(|id| {
                    // An empty agent id and no agent id are ONE window, by the
                    // key's own rule: the absent half is spelled as an empty
                    // part rather than left out, so the two cannot be told
                    // apart and must not be counted apart here either.
                    agents.iter().map(move |agent| {
                        (id, agent.as_deref().filter(|agent| !agent.is_empty()))
                    })
                })
                .collect();
            for (id, agent) in windows {
                let wins = (0..retries)
                    .filter(|_| claim_injection(repo.path(), id, agent).is_some())
                    .count();
                prop_assert_eq!(wins, 1, "window {:?}/{:?} injected {} times", id, agent, wins);
            }
            // A distinct window is a distinct claim: dedup must not be a
            // global latch that silences every later window.
            let fresh = claim_injection(repo.path(), "a-session-never-seen-before", None);
            prop_assert!(fresh.is_some());
        });
    }

    /// A subagent shares its parent's session id and is still introduced
    /// exactly once — and introducing it costs the parent nothing.
    ///
    /// Stated over the claim because the claim is where the two windows used
    /// to collide: keyed by the session alone, whichever of the two arrived
    /// first took the marker and the other was answered with silence forever.
    #[test]
    fn a_subagent_claims_its_own_pointer_under_its_parents_session() {
        let repo = tempfile::tempdir().unwrap();
        let parent = claim_injection(repo.path(), "s-shared", None);
        let child = claim_injection(repo.path(), "s-shared", Some("a0e79504"));
        let sibling = claim_injection(repo.path(), "s-shared", Some("a2f52a50"));
        assert!(parent.is_some(), "the main thread is introduced");
        assert!(child.is_some(), "and so is the window inside it");
        assert!(sibling.is_some(), "and so is the second one");
        assert_eq!(
            claim_injection(repo.path(), "s-shared", Some("a0e79504")),
            None,
            "the same window twice is one pointer"
        );
        let mut names = [parent, child, sibling]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), 3, "three windows, three occurrences");
        // The occurrence a spend row is keyed by is the window journal's key,
        // and not a second rule that happens to agree with it today.
        assert!(names.contains(&crate::window_journal::key("s-shared", Some("a0e79504"))));
    }

    /// A session id is agent-supplied text: it names the claim but never
    /// becomes a path component, so a traversal-shaped or newline-bearing id
    /// cannot escape `.rift/state/injection/`. The agent id is agent-supplied
    /// text too, and is screened by the same hash.
    #[test]
    fn a_hostile_session_id_never_becomes_a_path() {
        let repo = tempfile::tempdir().unwrap();
        let dir = repo.path().join(".rift/state/injection");
        for hostile in [
            "../../../etc/passwd",
            "/absolute/elsewhere",
            "with\nnewline",
            "with/slash",
        ] {
            assert!(claim_injection(repo.path(), hostile, None).is_some());
            assert!(claim_injection(repo.path(), "s-1", Some(hostile)).is_some());
        }
        let entries: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(std::result::Result::ok)
            .collect();
        assert_eq!(entries.len(), 8, "one flat marker per window");
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            let stem = name
                .strip_suffix(".json")
                .unwrap_or_else(|| panic!("{name} is not a marker file"));
            assert!(
                stem.chars().all(|c| c.is_ascii_hexdigit()),
                "{name} is not a hashed marker name"
            );
        }
    }

    #[test]
    fn antigravity_receives_a_pending_read_replacement_before_the_next_model_call() {
        let repo = tempfile::tempdir().unwrap();
        let raw = rift_shadow::hooks::RawHook {
            agent: rift_core::AgentId::ANTIGRAVITY,
            verb: "PreToolUse".to_owned(),
            session: "s-replacement".to_owned(),
            turn: None,
            occurred_at_ms: 10,
            payload: serde_json::json!({}),
        };
        arm_pending_replacement(repo.path(), &raw, "[rift] lines 4-20 are unchanged");
        let body = br#"{"session_id":"s-replacement","invocationNum":7}"#;
        let candidate = pending_replacement_candidate(
            repo.path(),
            &rift_core::AgentId::ANTIGRAVITY,
            "PreInvocation",
            body,
        )
        .expect("a pending result bypasses the first-invocation-only boundary gate");
        assert_eq!(
            candidate.kind,
            rift_recall_types::ContextKind::ReadReplacement
        );
        assert!(candidate.fallback);
        let plan = rift_recall_serve::ServeDispatcher::plan_context([candidate]);
        let response = rift_agent::respond::respond_with_plan(
            &rift_core::AgentId::ANTIGRAVITY,
            "PreInvocation",
            &plan,
            &rift_agent::respond::GateDecision::Allow,
        )
        .expect("the pinned persistent userMessage channel encodes the plan");
        assert!(response["injectSteps"][0]["userMessage"]
            .as_str()
            .is_some_and(|text| text.contains("lines 4-20 are unchanged")));
        let marker = pending_replacement_path(repo.path(), "s-replacement");
        assert!(marker.exists(), "planning alone does not consume delivery");
        complete_pending_replacement_in(repo.path(), "s-replacement", &plan);
        assert!(!marker.exists(), "successful encoding consumes it once");
    }

    #[test]
    fn content_offset_runs_raw_but_prepares_a_bounded_structural_outline() {
        let repo = tempfile::tempdir().unwrap();
        let ledger = repo.path().join("LEDGER.md");
        std::fs::write(
            &ledger,
            "# Goal\nbody\n## First gate\nbody\n### Acceptance\nbody\n",
        )
        .unwrap();
        let raw = rift_shadow::hooks::RawHook {
            agent: rift_core::AgentId::ANTIGRAVITY,
            verb: "PreToolUse".to_owned(),
            session: "s-offset".to_owned(),
            turn: None,
            occurred_at_ms: 10,
            payload: serde_json::json!({
                "toolCall": {
                    "name": "view_file",
                    "args": {
                        "AbsolutePath": ledger,
                        "ContentOffset": 230_000,
                        "StartLine": 270,
                        "EndLine": 339,
                    }
                }
            }),
        };
        let plan = antigravity_content_offset_plan(repo.path(), &raw).expect("a bounded plan");
        assert!(plan.contains("L1 # Goal"));
        assert!(plan.contains("L3 ## First gate"));
        assert!(plan.contains("L5 ### Acceptance"));
        assert!(plan.contains("raw call ran untouched"));
        assert!(plan.len() < 4_096);
    }

    /// Silence is the default: a payload that names no session yields no
    /// pointer, so a hook Rift cannot attribute never injects.
    #[test]
    fn a_payload_without_a_session_yields_no_pointer() {
        assert_eq!(payload_session_id(b"not json at all"), None);
        assert_eq!(payload_session_id(b"{}"), None);
        assert_eq!(payload_session_id(br#"{"session_id":""}"#), None);
        assert_eq!(payload_session_id(br#"{"session_id":123}"#), None);
        assert_eq!(
            payload_session_id(br#"{"session_id":"s-1"}"#).as_deref(),
            Some("s-1")
        );
    }

    /// A family with no verified context channel is never offered a pointer,
    /// and no verb other than the family's injection points is either — while
    /// the family whose channel *is* pinned gets it, on both of its boundaries
    /// and in the shape the pinned row names.
    ///
    /// "The decision is made before any repository state is touched" is
    /// asserted here rather than described: every refusal runs against a
    /// repository this test owns, and the root is handed over through a
    /// closure that records being asked, so a refusal that reached for a
    /// repository at all reddens the test. Nothing in this suite may read or
    /// write the checkout it happens to be running in — the developer's own
    /// hooks run out of that tree, and a claim marker written there would make
    /// every later run of this test pass for the wrong reason.
    #[test]
    fn only_the_verified_channel_and_verb_can_inject() {
        // Recall on and a projection of its own, so a refusal below is a fact
        // about the family and the verb — never about a repository that could
        // not have answered anyway.
        let (repo, db) = recall_repo();

        for (agent, verb) in [
            ("claude-code", "PostToolUse"),
            ("claude-code", "PreCompact"),
            // The family above has four context channels — its two turn
            // boundaries, the one that opens a subagent's window, and nothing
            // else; the family below has its two. The gate verb carries a
            // reason for one call, not the facts a turn holds, and the end of
            // a turn is not a row either family has at all.
            ("claude-code", "SubagentStop"),
            ("codex", "PreToolUse"),
            ("codex", "Stop"),
            ("gemini", "BeforeModel"),
            ("antigravity", "PreToolUse"),
            ("some-stranger", "UserPromptSubmit"),
        ] {
            let asked = std::cell::Cell::new(false);
            assert_eq!(
                session_pointer_rooted(agent, verb, br#"{"session_id":"s-1"}"#, || {
                    asked.set(true);
                    Some(repo.path().to_path_buf())
                }),
                None,
                "{agent} {verb} must not inject"
            );
            assert!(
                !asked.get(),
                "{agent} {verb} is refused before any repository is reached for"
            );
        }

        for (verb, session) in [
            ("UserPromptSubmit", "s-codex-prompt"),
            ("SessionStart", "s-codex-start"),
        ] {
            let body = format!(r#"{{"session_id":"{session}"}}"#);
            let text = session_pointer_rooted("codex", verb, body.as_bytes(), || {
                Some(repo.path().to_path_buf())
            })
            .unwrap_or_else(|| panic!("codex {verb} is a pinned boundary and injects"));
            let response =
                rift_agent::respond::respond(&rift_core::AgentId::CODEX, verb, Some(&text))
                    .unwrap_or_else(|| panic!("codex {verb} has a pinned response shape"));
            assert_eq!(
                response["hookSpecificOutput"]["additionalContext"]
                    .as_str()
                    .unwrap_or_default(),
                text,
                "codex {verb} travels on the channel its pinned row names"
            );
        }

        drop_recall_repo(&repo, &db);
    }

    /// The positive path, end to end: on a Recall-enabled repository the first
    /// prompt of a session gets the pointer, the session's next prompt gets
    /// silence, and the tokens the first one cost are booked as spend. Stated
    /// over the whole chain (config gate → claim → tokenizer → audit row)
    /// because the primitives were already green while the chain was what a
    /// caller actually gets; the two booking tests below drive the same chain
    /// through [`delivered_first_pointer`].
    #[test]
    fn the_first_prompt_of_a_session_injects_once() {
        let (repo, db) = recall_repo();
        let pointer = session_pointer_in(repo.path(), &CC, PROMPT, FIRST_PROMPT)
            .expect("the first prompt injects");
        assert!(
            pointer.contains("rift recall"),
            "the pointer must teach the skill surface: {pointer}"
        );
        assert_eq!(
            session_pointer_in(repo.path(), &CC, PROMPT, FIRST_PROMPT),
            None,
            "the same session's next prompt is silent"
        );
        drop_recall_repo(&repo, &db);
    }

    #[test]
    fn a_delivered_pointer_books_its_tokens_as_spend_and_claims_no_savings() {
        use rift_store::CanonicalStore as _;

        let (repo, db) = recall_repo();
        let (pointer, rt, store) = delivered_first_pointer(repo.path(), &db);
        let rows = rt
            .block_on(store.query(
                "SELECT tokens_returned, tokens_avoided FROM memd_audit_event \
                 WHERE verb = 'memd.manifest_injected'",
                &[],
            ))
            .unwrap();
        assert_eq!(rows.len(), 1, "one injection, one spend row");
        assert_eq!(
            rt.block_on(store.count("memd_audit_event")).unwrap(),
            1,
            "and the injection is the only row: it is audited under its own \
             verb, never doubled into the pull side's"
        );
        assert_eq!(
            u32::try_from(rows[0].get_i64(0).unwrap()).unwrap(),
            rift_tokens::count(&pointer),
            "spend is the real tokenizer's count of the injected text"
        );
        assert_eq!(
            rows[0].get_i64(1).unwrap(),
            0,
            "injection claims no savings"
        );
        drop(store);
        drop_recall_repo(&repo, &db);
    }

    #[test]
    fn a_delivered_pointer_leaves_one_hook_channel_receipt() {
        use rift_store::CanonicalStore as _;

        let (repo, db) = recall_repo();
        let (pointer, rt, store) = delivered_first_pointer(repo.path(), &db);
        let calls = rt
            .block_on(store.query(
                "SELECT channel, tool_name, tokens_returned FROM serve_call_event",
                &[],
            ))
            .unwrap();
        assert_eq!(calls.len(), 1, "one delivered pointer, one channel receipt");
        assert_eq!(calls[0].get_text(0).unwrap(), "hook_inject");
        assert_eq!(calls[0].get_text(1).unwrap(), "claude-code:session-pointer");
        assert_eq!(
            u32::try_from(calls[0].get_i64(2).unwrap()).unwrap(),
            rift_tokens::count(&pointer)
        );
        drop(store);
        drop_recall_repo(&repo, &db);
    }

    /// The first prompt's pointer injected, the next prompt's silence met, and
    /// the pointer delivered — then the family's recall store opened over what
    /// the delivery booked, with the runtime to read it on.
    fn delivered_first_pointer(
        repo: &Path,
        db: &Path,
    ) -> (String, tokio::runtime::Runtime, rift_store::SqliteStore) {
        let pointer =
            session_pointer_in(repo, &CC, PROMPT, FIRST_PROMPT).expect("the first prompt injects");
        assert_eq!(
            session_pointer_in(repo, &CC, PROMPT, FIRST_PROMPT),
            None,
            "the same session's next prompt is silent"
        );
        let plan = rift_recall_serve::ServeDispatcher::plan_context([
            rift_recall_types::ContextCandidate {
                kind: rift_recall_types::ContextKind::SessionPointer,
                text: pointer.clone(),
                avoided_tokens: 0,
                fallback: false,
            },
        ]);
        record_context_delivery_in(repo, CC.as_str(), &plan, None);
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let store = rift_store::open_migrated(db, &rift_store::recall_migrations()).unwrap();
        (pointer, rt, store)
    }

    /// The window a subagent gets, through the same chain: the verb that opens
    /// it is a boundary, the pointer is what it carries, and the parent's own
    /// introduction is untouched by it.
    ///
    /// The last assertion is the one that could not be made before: the two
    /// windows share a session id, so a claim keyed by the session alone would
    /// make this pair of calls one pointer and one silence — in whichever
    /// order they happened to arrive.
    #[test]
    fn a_subagents_window_is_introduced_at_the_verb_that_opens_it() {
        let (repo, db) = recall_repo();

        let started = serde_json::json!({
            "session_id": "s-sub",
            "hook_event_name": "SubagentStart",
            "agent_id": "a0e79504",
            "agent_type": "general-purpose",
        })
        .to_string();
        let pointer = session_pointer_in(repo.path(), &CC, "SubagentStart", started.as_bytes())
            .expect("the verb that opens a window introduces it");
        assert!(
            pointer.contains("rift recall"),
            "a subagent is handed the same sealed pointer: {pointer}"
        );
        assert_eq!(
            session_pointer_in(repo.path(), &CC, "SubagentStart", started.as_bytes()),
            None,
            "the same window twice is one pointer, and no manifest follows it"
        );
        // A second subagent of the same turn is a second window.
        let sibling = serde_json::json!({
            "session_id": "s-sub",
            "hook_event_name": "SubagentStart",
            "agent_id": "a2f52a50",
            "agent_type": "general-purpose",
        })
        .to_string();
        assert!(
            session_pointer_in(repo.path(), &CC, "SubagentStart", sibling.as_bytes()).is_some(),
            "a sibling window is introduced too"
        );
        // …and the parent has not been introduced by any of it.
        assert!(
            session_pointer_in(repo.path(), &CC, PROMPT, br#"{"session_id":"s-sub"}"#).is_some(),
            "the main thread's own introduction is still owed"
        );

        drop_recall_repo(&repo, &db);
    }
}
