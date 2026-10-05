//! What one hook's ingest leaves owed to the detached worker: the token count
//! of a read, the bytes of the revision it named, the measurement of a
//! search, and the attribution of a serve a `rift recall` command printed.
//!
//! Separate because every function here writes a journal entry of identifiers
//! and nothing else. None of the work itself fits inside an agent's hook, and
//! keeping the four debts together is what keeps that boundary visible.

use std::path::Path;

use rift_shadow::hooks::AdapterRegistry;

use super::cli_serve::owe_cli_serve_attribution;
use crate::hooks::gate::{gate_step, gate_turn, GateStep};
use crate::hooks::now_ms;

/// Journal a token-count job for every read fact this hook recorded without
/// a count. Returns how many were journaled.
fn owe_read_tokens(git_dir: &Path, work_tree: &Path, reads: &[rift_shadow::RecordedRead]) -> usize {
    let now = now_ms();
    reads
        .iter()
        .filter(|r| !r.tokens_recorded)
        .filter(|r| {
            crate::agent_queue::enqueue(
                git_dir,
                &crate::agent_queue::AgentJob {
                    schema_version: 1,
                    work_tree: work_tree.to_path_buf(),
                    queued_at_ms: now,
                    attempts: 0,
                    kind: crate::agent_queue::JobKind::ReadTokens {
                        tool_call_id: r.tool_call_id.clone(),
                        rel_path: r.rel_path.clone(),
                        content_sha256: r.content_sha256.clone(),
                        blob_oid: r.blob_oid.clone(),
                        lines: r.lines,
                        schema_version: r.schema_version,
                    },
                },
            )
            .is_ok()
        })
        .count()
}

/// Journal a snapshot job for every revision this hook's read facts named.
/// Returns how many were journaled.
///
/// Every recorded read, not only the unmeasured ones: the count a read owes
/// and the bytes a *later* read will be diffed against are different debts,
/// and a revision whose tokens were counted inline is exactly as likely to be
/// the one an agent still holds. The journal keys by the revision, so a file
/// read ten times in a turn owes one snapshot.
pub(in crate::hooks) fn owe_basis_snapshots(
    git_dir: &Path,
    work_tree: &Path,
    reads: &[rift_shadow::RecordedRead],
) -> usize {
    let now = now_ms();
    let mut seen = std::collections::BTreeSet::new();
    reads
        .iter()
        .filter(|r| seen.insert(r.content_sha256.clone()))
        .filter(|r| {
            crate::agent_queue::enqueue(
                git_dir,
                &crate::agent_queue::AgentJob {
                    schema_version: 1,
                    work_tree: work_tree.to_path_buf(),
                    queued_at_ms: now,
                    attempts: 0,
                    kind: crate::agent_queue::JobKind::BasisSnapshot {
                        rel_path: r.rel_path.clone(),
                        content_sha256: r.content_sha256.clone(),
                        blob_oid: r.blob_oid.clone(),
                    },
                },
            )
            .is_ok()
        })
        .count()
}

/// Everything this hook's ingest owes a later, off-path writer, journaled in
/// one place and counted as one number.
///
/// Four debts, all of them identifiers only and all of them drained by the
/// same worker. **The token count** of each read fact, which needs the
/// tokenizer. **The bytes** of each revision a read recorded, so the delta
/// against it survives the edit that follows — which needs the redaction gate.
/// **The measurement of a search**, which needs a process. And **the
/// attribution of a serve** a `rift recall` command printed the keys of,
/// which needs the recall projection. Not one of them
/// fits inside a hook, and every one of them is best-effort: a journal that
/// cannot be written leaves a figure unmeasured, never the hook failed.
pub(super) fn owe_deferred_measurement(
    t: &crate::hooktime::HookTimer,
    git_dir: &Path,
    work_tree: &Path,
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    outcome: &rift_shadow::IngestOutcome,
) -> usize {
    let reads = t.phase("owe_tokens", || {
        owe_read_tokens(git_dir, work_tree, &outcome.read_calls)
    });
    let bases = t.phase("owe_basis", || {
        owe_basis_snapshots(git_dir, work_tree, &outcome.read_calls)
    });
    let results = t.phase("owe_result_tokens", || {
        owe_tool_result_tokens(
            git_dir,
            work_tree,
            registry,
            raw,
            outcome.owner_agent.as_str(),
        )
    });
    reads.saturating_add(bases).saturating_add(results)
}

/// What one tool result owes: the attribution of the serve a `rift recall`
/// command printed, and the measurement of a search.
///
/// The call is read off the payload once, through the family's own adapter,
/// and both debts are decided from that one reading: the window a serve is
/// attributed to comes off the same facts the search is classified from.
fn owe_tool_result_tokens(
    git_dir: &Path,
    work_tree: &Path,
    registry: &AdapterRegistry,
    raw: &rift_shadow::hooks::RawHook,
    owner_agent: &str,
) -> usize {
    let Some(call) = cross_checked_call(registry, raw) else {
        return 0;
    };
    let attributed = match call.command() {
        Some(command) => owe_cli_serve_attribution(git_dir, work_tree, raw, &call.facts, command),
        None => 0,
    };
    attributed.saturating_add(owe_search_measurement(
        git_dir,
        work_tree,
        raw,
        owner_agent,
        &call,
    ))
}

/// One tool result, as the family's pinned row reads its call.
struct CrossCheckedCall<'a> {
    facts: rift_agent::HookFacts,
    tool: String,
    row: rift_agent::tool_fields::ToolFieldRow,
    args: &'a serde_json::Map<String, serde_json::Value>,
}

impl CrossCheckedCall<'_> {
    /// The command line, when the call is a shell call that names one.
    fn command(&self) -> Option<&str> {
        if self.row.shape != rift_agent::tool_fields::ToolShape::Shell {
            return None;
        }
        self.args
            .get(self.row.fact("command").unwrap_or("command"))
            .and_then(serde_json::Value::as_str)
    }
}

/// The call a tool result answers, read through the family's adapter and its
/// pinned row; `None` for anything that is not a result, or a tool the row
/// table does not name.
fn cross_checked_call<'a>(
    registry: &AdapterRegistry,
    raw: &'a rift_shadow::hooks::RawHook,
) -> Option<CrossCheckedCall<'a>> {
    if !matches!(
        gate_step(&raw.agent, &raw.verb, &raw.payload),
        Some(GateStep::CrossCheck)
    ) {
        return None;
    }
    let facts = registry.get(&raw.agent)?.facts(&raw.verb, &raw.payload);
    let tool = facts.tool_name.clone()?;
    let row = rift_agent::tool_fields::tool_field_row(&raw.agent, &tool)?;
    let args = search_result_args(row, &raw.payload)?;
    Some(CrossCheckedCall {
        facts,
        tool,
        row,
        args,
    })
}

/// Journal the measurement one **search result** owes, and say whether the
/// worker was left anything.
///
/// The denominator's other half. A read resolves to a file and is measured
/// through `file_read_tokens`; a search resolves to no file, has no key there
/// and has never been measured at all — while returning repository content on
/// every call. This is the row that makes the pool the whole pool.
///
/// The hook does none of the work: it classifies the call from the arguments
/// the family's pinned row names, reads the tree key out of the saved index's
/// header for one small read, and writes a file. The re-execution, the
/// tokenizer and the projection all belong to the worker.
///
/// Two calls are deliberately **not** measured here. A shell read — `cat`,
/// `sed -n`, a `grep` over one named path — is already in the read
/// denominator through its own read fact, and counting it again would double
/// it; the recogniser is asked, and a command line it reads as a read is left
/// alone. And a command whose head word is not an allow-listed verb is not a
/// search Rift can see at all, so nothing is claimed about it.
fn owe_search_measurement(
    git_dir: &Path,
    work_tree: &Path,
    raw: &rift_shadow::hooks::RawHook,
    owner_agent: &str,
    call: &CrossCheckedCall<'_>,
) -> usize {
    let (row, args) = (call.row, call.args);
    let (channel, search) = match row.shape {
        rift_agent::tool_fields::ToolShape::Search => (
            "search",
            crate::search_measure::classify_tool(
                row.fact("kind").unwrap_or_default(),
                args,
                row.fact("pattern").unwrap_or("pattern"),
                row.fact("dir").unwrap_or("path"),
            ),
        ),
        rift_agent::tool_fields::ToolShape::Shell => {
            let command = call.command().unwrap_or_default();
            // A command line the read grammar reads as a READ is already in
            // the read denominator; measuring it here would count it twice.
            if matches!(
                rift_agent::shell_read::recognise(command),
                Some(rift_agent::shell_read::ShellCall::Read(_))
            ) {
                return 0;
            }
            ("search", crate::search_measure::classify_command(command))
        }
        _ => return 0,
    };
    let Some(search) = search else { return 0 };
    let payload_sha256 = rift_shadow::sha256_hex(&[serde_json::Value::Object(args.clone())
        .to_string()
        .as_bytes()]);
    let session_row = rift_shadow::ids::session_row_id(owner_agent, &raw.session);
    let job = crate::agent_queue::AgentJob {
        schema_version: 1,
        work_tree: work_tree.to_path_buf(),
        queued_at_ms: raw.occurred_at_ms,
        attempts: 0,
        kind: crate::agent_queue::JobKind::ToolResultTokens {
            tool_result_id: rift_shadow::ids::tool_result_row_id(
                &session_row,
                gate_turn(raw, git_dir),
                &call.tool,
                &payload_sha256,
            ),
            session_id: session_row,
            tool_name: call.tool.clone(),
            channel: channel.to_owned(),
            search,
            tree_key: saved_tree_key(work_tree),
        },
    };
    usize::from(crate::agent_queue::enqueue(git_dir, &job).is_ok())
}

/// The arguments object a family's pinned row says a call carries them in.
///
/// Read off the row rather than known here, exactly as the search gate reads
/// it: two families put them flat under the envelope field beside `tool_name`
/// and one nests them, and the row is where that fact is already pinned to an
/// artifact.
pub(super) fn search_result_args(
    row: rift_agent::tool_fields::ToolFieldRow,
    payload: &serde_json::Value,
) -> Option<&serde_json::Map<String, serde_json::Value>> {
    let mut node = payload;
    for segment in row.fact("args").unwrap_or("tool_input").split('.') {
        node = node.get(segment)?;
    }
    node.as_object()
}

/// The tree state this member's path index was last taken at, read from the
/// saved index's header alone.
///
/// One small read and no walk: a hook may not stat a tree's skeleton, and the
/// index's own key is the closest honest answer to "what state was this
/// checkout in when the search ran". `None` where the index is cold, which
/// leaves the worker with its own bracket and nothing earlier to compare.
#[cfg(feature = "search")]
fn saved_tree_key(work_tree: &Path) -> Option<crate::agent_queue::TreeKey> {
    rift_recall_search::PathIndex::saved_key(&rift_recall_search::index_path(work_tree))
        .as_ref()
        .map(crate::agent_queue::TreeKey::of)
}

/// A build with no search seam keeps no index and therefore no key.
#[cfg(not(feature = "search"))]
fn saved_tree_key(_work_tree: &Path) -> Option<crate::agent_queue::TreeKey> {
    None
}
