//! The three delta verbs and the one bounded dispatch behind them.
//!
//! Each verb SERVES a person standing in one checkout: the answer is contained
//! to that checkout's line and says what it held back. The family-wide
//! measurements live in the sibling modules, which read stores rather than
//! serve them.

use std::borrow::Cow;
use std::collections::BTreeSet;
use std::path::Path;

use rift_config::EffectiveConfig;
use rift_core::{Result, RiftError, Timestamp};
use rift_recall_policy::{PolicyEngine, ServePolicy, TurnBudget};
use rift_recall_serve::{
    RecallContext, ServeDispatcher, ToolError, ToolName, ToolRequest, ToolResponse,
};
use rift_recall_substrate::{worktree, GixSubstrate, MemorySubstrate, SubstrateReader};
use rift_recall_types::{AncestryOracle, NoProvenance, ProvenanceOracle, Scope};
use rift_store::{CanonicalStore, SqliteStore, Value};

use crate::cli::{exit, Outcome};

/// Dispatch one tool call through the shared bounded surface: the same policy
/// gates (`.rift/policies/serve.toml` overlay, fail-closed on a malformed
/// config) and per-turn budget every serving channel applies. Shared with the
/// agent-facing `rift recall <verb>` skill surface, so both CLI
/// spellings are callers of one dispatcher, never two implementations.
///
/// The [`ToolError`] survives here: the skill surface reports the dispatcher's
/// own refusal tag and exit class, and only this human-facing wrapper flattens
/// it into a generic storage error.
///
/// Before dispatch, a verb whose answer can cite a turn is handed the turns the
/// calling window's own journal records ([`caller_window`],
/// [`with_seen_turns`]). The push tier's `changed_since` reaches this function
/// from a hook, and asks `diff_since`, which cites none — so on that path no
/// variable is read and no journal is opened.
pub(crate) async fn tool_call_raw(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
    provenance: &dyn ProvenanceOracle,
    tool: ToolName,
    req: &ToolRequest,
) -> std::result::Result<ToolResponse, ToolError> {
    let window = caller_window(tool, &|name| std::env::var(name).ok());
    let req = with_seen_turns(repo, window.as_deref(), req);
    serve_raw(store, repo, scope, ancestry, provenance, tool, &req).await
}

/// [`tool_call_raw`] once the request carries everything the binary resolves
/// for it.
async fn serve_raw(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
    provenance: &dyn ProvenanceOracle,
    tool: ToolName,
    req: &ToolRequest,
) -> std::result::Result<ToolResponse, ToolError> {
    let rift_dir = repo.join(".rift");
    let config = match EffectiveConfig::load(&rift_dir) {
        Ok(c) => c,
        Err(_) => {
            EffectiveConfig::secure_default().map_err(|e| ToolError::Internal(e.to_string()))?
        }
    };
    let policy = PolicyEngine::new(&config).with_serve_policy(ServePolicy::load(&rift_dir));
    let server = ServeDispatcher::new(policy);
    // The ladder's second rung, opened for the same verbs and under the same
    // rule as the reader below: the family's own store, reached from the common
    // directory every member shares, so a revision a sibling's read recorded
    // answers here too. A checkout whose family cannot be proven from here
    // simply has the rung it always had.
    let snapshots = tool
        .may_read_the_substrate()
        .then(|| crate::hooks::basis_snapshots_at(repo))
        .flatten();
    let at = ServeAt {
        store,
        scope,
        ancestry,
        provenance,
        snapshots: snapshots.as_ref(),
    };
    match repository_reader(tool, repo) {
        Some(reader) => dispatch_over(&server, &at, &reader, tool, req).await,
        None => dispatch_over(&server, &at, &MemorySubstrate::local(), tool, req).await,
    }
}

/// Everything one dispatch needs except the reader it reads bytes through.
///
/// Five borrows that always travel together and are never stored: a context
/// struct keeps the call sites of [`dispatch_over`] readable where the reader
/// is the only thing that differs between them.
struct ServeAt<'a> {
    store: &'a SqliteStore,
    scope: &'a Scope,
    ancestry: &'a dyn AncestryOracle,
    provenance: &'a dyn ProvenanceOracle,
    snapshots: Option<&'a rift_recall_serve::basis_store::BasisStore>,
}

/// **The reader is the repository**, for the verbs that can need bytes.
///
/// A verb that serves a delta against the revision its caller holds has to
/// fetch that revision's blob and read the named file as it is now; an empty
/// in-memory substrate can do neither, so every such answer would be
/// `basis_unavailable` however much the projection knew.
///
/// NOTHING IS OPENED ON A HOOK PATH. [`tool_call_raw`] has two callers: the CLI
/// surface (`rift memory <verb>` / `rift recall <verb>`), and the push tier's
/// `changed_since`, which asks exactly one verb — `diff_since`. That verb is
/// answered from the projection alone and says so (`may_read_the_substrate`),
/// so the hook path gets `None` here and opens no repository. Nothing here is
/// reachable from the `PreToolUse` gate at all.
///
/// What the reader opens is git, and git's objects for a linked worktree live
/// in the family's common directory — the same store every member shares — so
/// the basis a sibling member committed is reachable here. No identity, cache
/// or durable key is derived from the checkout path; the projection this
/// dispatch reads was already resolved family-first by the caller. The checkout
/// matters for exactly one thing, which is what "the file as it is now" means
/// to the caller who named it.
///
/// Conservative when it cannot: a path that is not a readable repository is
/// `None`, the in-memory reader stands in, and the verb answers
/// `basis_unavailable` rather than failing the call.
fn repository_reader(tool: ToolName, repo: &Path) -> Option<GixSubstrate> {
    tool.may_read_the_substrate()
        .then(|| GixSubstrate::open(repo).ok())
        .flatten()
}

/// Serve one call through `reader`, on a budget of its own.
async fn dispatch_over<R: SubstrateReader>(
    server: &ServeDispatcher<'_>,
    at: &ServeAt<'_>,
    reader: &R,
    tool: ToolName,
    req: &ToolRequest,
) -> std::result::Result<ToolResponse, ToolError> {
    let ctx = RecallContext {
        store: at.store,
        reader,
        snapshots: at.snapshots,
        scope: at.scope.clone(),
        ancestry: at.ancestry,
        provenance: at.provenance,
        now: Timestamp::now(),
    };
    let mut budget = TurnBudget::default();
    server.dispatch(tool, req, &ctx, &mut budget).await
}

/// The context window a pull comes from, spelled as the session of a window
/// with no agent id — or `None` whenever the client's shell does not name the
/// whole window, which is the only answer that is never a guess.
///
/// A window is a session **and** the agent inside it
/// ([`crate::window_journal::key`]), and a client's hooks say both, but a
/// pull is a shell command, and what a client exports to its shell is all
/// this process can see:
///
/// * **Claude Code** exports `CLAUDE_CODE_SESSION_ID` and nothing naming the
///   agent, and a subagent's shell carries its parent's session id. A pull from
///   a subagent and one from the main thread look the same, so neither is
///   attributed. Its markers are checked first because they are inherited: a
///   client started from inside another would otherwise read as the outer one.
/// * **Codex** exports `CODEX_THREAD_ID`, the thread running the command, and
///   its main thread's hooks carry no agent id. Where that id is the session
///   its hooks report, the main thread's journal is found; where it is not —
///   a child thread, whose hooks name an agent — the key names no journal and
///   the pull cites nothing. A mismatch lands on no turn, never on another
///   window's.
/// * **Antigravity** exports `ANTIGRAVITY_CONVERSATION_ID`, the id its hooks
///   report as the session, and its hooks name no agent inside it.
///
/// Two named at once is an ambiguity, not a choice, and is answered with
/// `None`. Asked only for a verb whose answer can cite a turn; every other
/// verb — `diff_since`, the one a hook asks — returns before a variable is
/// read.
fn caller_window(tool: ToolName, env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    if !matches!(tool, ToolName::KnownState | ToolName::ContextPack) {
        return None;
    }
    let named = |name: &str| env(name).filter(|value| !value.is_empty());
    if named("CLAUDECODE").is_some() || named("CLAUDE_CODE_SESSION_ID").is_some() {
        return None;
    }
    match (
        named("CODEX_THREAD_ID"),
        named("ANTIGRAVITY_CONVERSATION_ID"),
    ) {
        (Some(session), None) | (None, Some(session)) => Some(session),
        _ => None,
    }
}

/// `req` with the turns the window's own journal records, read once per pull
/// through the bounded journal walk and the pure fold; `req` unchanged when
/// there is no window or the journal records no read it still holds.
fn with_seen_turns<'a>(
    repo: &Path,
    window: Option<&str>,
    req: &'a ToolRequest,
) -> Cow<'a, ToolRequest> {
    let Some(session) = window else {
        return Cow::Borrowed(req);
    };
    let journal = crate::window_journal::WindowJournal::open(repo, session, None)
        .load()
        .journal;
    let seen_turns = crate::window_journal::last_read_turns(&journal);
    if seen_turns.is_empty() {
        return Cow::Borrowed(req);
    }
    Cow::Owned(ToolRequest {
        seen_turns,
        ..req.clone()
    })
}

/// [`tool_call_raw`] for the human surface: a dispatcher refusal becomes a
/// generic command failure.
async fn tool_call(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
    tool: ToolName,
    req: &ToolRequest,
) -> Result<ToolResponse> {
    tool_call_raw(store, repo, scope, ancestry, &NoProvenance, tool, req)
        .await
        .map_err(|e| RiftError::storage(format!("memory tool failed: {e:?}")))
}

/// `rift memory diff-since <point>`: everything that changed since a checkpoint
/// — the known set is what had been read *by* that checkpoint, so the answer is
/// exactly the delta an agent (or human) still needs to look at.
pub(super) async fn diff_since(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
    point: &str,
) -> Result<Outcome> {
    // Resolve the point to a checkpoint (id or unique prefix).
    let rows = store
        .query(
            "SELECT node_id, ordinal FROM n_checkpoint WHERE node_id LIKE ?1 || '%' \
             ORDER BY node_id LIMIT 2",
            &[Value::text(point)],
        )
        .await?;
    let (ckpt, ordinal) = match rows.as_slice() {
        [only] => (only.get_text(0)?, only.get_i64(1)?),
        [] => {
            return Ok(Outcome::coded(
                exit::FAILURE,
                format!("no checkpoint matches '{point}'"),
                serde_json::json!({ "error": "unknown_checkpoint", "point": point }),
            ))
        }
        _ => {
            return Ok(Outcome::coded(
                exit::FAILURE,
                format!("'{point}' is ambiguous (matches multiple checkpoints)"),
                serde_json::json!({ "error": "ambiguous_checkpoint", "point": point }),
            ))
        }
    };

    // The hashes known as of that checkpoint (reads at ordinal ≤ point's).
    let known_rows = store
        .query(
            "SELECT DISTINCT fr.content_sha256 FROM n_file_read r \
             JOIN n_file_revision fr ON fr.node_id = r.file_revision \
             JOIN n_checkpoint c ON c.node_id = r.checkpoint_id \
             WHERE c.ordinal <= ?1",
            &[Value::Int(ordinal)],
        )
        .await?;
    let known_hashes: Vec<String> = known_rows
        .iter()
        .filter_map(|r| r.get_text(0).ok())
        .collect();

    let req = ToolRequest {
        known_hashes,
        ..ToolRequest::default()
    };
    let resp = tool_call(store, repo, scope, ancestry, ToolName::DiffSince, &req).await?;
    let changed = resp.payload.get("changed").cloned().unwrap_or_default();
    let unchanged = resp.payload.get("unchanged").cloned().unwrap_or_default();
    let n_changed = changed.as_array().map_or(0, Vec::len);
    let n_unchanged = unchanged.as_array().map_or(0, Vec::len);

    let mut human = format!("since {ckpt}: {n_changed} changed, {n_unchanged} unchanged\n");
    for c in changed.as_array().into_iter().flatten().take(50) {
        let path = c.get("path").and_then(|v| v.as_str()).unwrap_or("?");
        human.push_str(&format!("  changed   {path}\n"));
    }
    for u in unchanged.as_array().into_iter().flatten().take(50) {
        human.push_str(&format!("  unchanged {}\n", u.as_str().unwrap_or("?")));
    }
    Ok(Outcome::ok(
        human,
        serde_json::json!({
            "since": ckpt,
            "changed": changed,
            "unchanged": unchanged,
            "estimated_tokens_avoided": resp.estimated_tokens_avoided,
        }),
    ))
}

/// `rift memory file-if-changed <path>`: does the worktree file differ from
/// what was last captured? The current worktree hash is the "known" hash — an
/// `unchanged` answer means a re-read would be pure waste.
pub(super) async fn file_if_changed(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
    path: &str,
) -> Result<Outcome> {
    let mut one = BTreeSet::new();
    one.insert(path.to_string());
    let current = worktree::hash_paths(repo, &one);
    let req = ToolRequest {
        target: Some(path.to_string()),
        known_hashes: current.values().cloned().collect(),
        ..ToolRequest::default()
    };
    let resp = tool_call(store, repo, scope, ancestry, ToolName::FileIfChanged, &req).await?;
    let result = resp.payload.get("result").cloned().unwrap_or_default();
    let state = result
        .get("state")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let human = match state.as_str() {
        "unchanged" => format!("{path}: unchanged since last captured read (re-read avoidable)"),
        "changed" => format!("{path}: CHANGED since last captured read"),
        _ => format!("{path}: {state} (no captured read of this path)"),
    };
    Ok(Outcome::ok(
        human,
        serde_json::json!({ "path": path, "result": result }),
    ))
}

/// `rift memory cost-hotspots`: where the context tokens go.
pub(super) async fn cost_hotspots(
    store: &SqliteStore,
    repo: &Path,
    scope: &Scope,
    ancestry: &dyn AncestryOracle,
    top: u32,
) -> Result<Outcome> {
    let req = ToolRequest {
        top_n: Some(top),
        ..ToolRequest::default()
    };
    let resp = tool_call(store, repo, scope, ancestry, ToolName::CostHotspots, &req).await?;
    let hotspots = resp.payload.get("hotspots").cloned().unwrap_or_default();
    let mut human = format!("top {top} context-token hotspots:\n");
    for h in hotspots.as_array().into_iter().flatten() {
        let area = h.get("area").and_then(|v| v.as_str()).unwrap_or("?");
        let tokens = h
            .get("est_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let share = h
            .get("share")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0);
        human.push_str(&format!(
            "  {tokens:>10} tok  {:>5.1}%  {area}\n",
            share * 100.0
        ));
    }
    if hotspots.as_array().is_none_or(Vec::is_empty) {
        human.push_str(&format!("  {}\n", empty_reads_diagnosis(repo, store).await));
    }
    Ok(Outcome::ok(
        human,
        serde_json::json!({ "hotspots": hotspots }),
    ))
}

/// Name the broken memory-chain link behind an empty read set: no checkpoints → not ingested → genuinely no reads — each with
/// its next command. An empty panel is a diagnosis, not a shrug.
async fn empty_reads_diagnosis(repo: &Path, store: &SqliteStore) -> String {
    let checkpoints = GixSubstrate::open(repo)
        .and_then(|sub| sub.checkpoints())
        .map_or(0, |c| c.len());
    if checkpoints == 0 {
        return "(no captured reads yet — this repository has no checkpoints; \
                commit with a live session, or see `rift doctor`)"
            .to_owned();
    }
    let ingested = store
        .query("SELECT COUNT(*) FROM n_file_read", &[])
        .await
        .ok()
        .and_then(|rows| rows.first().and_then(|r| r.get_i64(0).ok()))
        .unwrap_or(0);
    if ingested == 0 {
        return format!(
            "(no captured reads yet — {checkpoints} checkpoint(s) exist but carry no \
             read facts; sessions before the read-fact capture landed have none, \
             or see `rift doctor`)"
        );
    }
    "(no reads in scope)".to_owned()
}

#[cfg(test)]
mod tests {
    use rift_agent::window_journal::{Interval, Line};
    use rift_recall_types::{NoAncestry, NoProvenance};

    use super::*;

    /// A client's shell, as a table of the variables it exports.
    fn shell<'a>(vars: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| (*value).to_owned())
        }
    }

    fn window_of(tool: ToolName, vars: &[(&str, &str)]) -> Option<String> {
        caller_window(tool, &shell(vars))
    }

    #[test]
    fn a_pull_is_attributed_only_where_the_shell_names_the_whole_window() {
        let codex = [("CODEX_THREAD_ID", "thread-1")];
        let antigravity = [("ANTIGRAVITY_CONVERSATION_ID", "conv-1")];
        assert_eq!(
            window_of(ToolName::KnownState, &codex).as_deref(),
            Some("thread-1")
        );
        assert_eq!(
            window_of(ToolName::ContextPack, &antigravity).as_deref(),
            Some("conv-1")
        );
        // Claude Code names the session but not the agent inside it.
        let claude = [("CLAUDECODE", "1"), ("CLAUDE_CODE_SESSION_ID", "s-1")];
        assert_eq!(window_of(ToolName::KnownState, &claude), None);
        // Inherited markers: a client started inside another is not the outer one.
        let nested = [("CLAUDECODE", "1"), ("CODEX_THREAD_ID", "thread-1")];
        assert_eq!(window_of(ToolName::KnownState, &nested), None);
        let both = [
            ("CODEX_THREAD_ID", "t"),
            ("ANTIGRAVITY_CONVERSATION_ID", "c"),
        ];
        assert_eq!(window_of(ToolName::KnownState, &both), None);
        assert_eq!(
            window_of(ToolName::KnownState, &[("CODEX_THREAD_ID", "")]),
            None
        );
        assert_eq!(window_of(ToolName::KnownState, &[]), None);
    }

    /// `diff_since` is the verb a hook asks through this function; it cites no
    /// turn, so it never reads the environment, let alone a journal.
    #[test]
    fn a_verb_that_cites_no_turn_reads_no_variable() {
        let read = std::cell::Cell::new(0usize);
        let env = |_: &str| {
            read.set(read.get() + 1);
            Some("thread-1".to_owned())
        };
        assert_eq!(caller_window(ToolName::DiffSince, &env), None);
        assert_eq!(read.get(), 0);
    }

    const PATH: &str = "src/a.rs";

    fn revision() -> rift_core::Sha256Id {
        rift_hashchain::leaf_hash(b"the bytes of src/a.rs")
    }

    /// A projection holding one read of [`PATH`] at [`revision`].
    fn projection() -> SqliteStore {
        let store = SqliteStore::open_in_memory().unwrap();
        rift_store::apply_migrations(&store, &rift_store::recall_migrations()).unwrap();
        let sha = revision().to_hex();
        let rows: [(&str, &[&str], Vec<Value>); 3] = [
            (
                "n_checkpoint",
                &[
                    "node_id",
                    "dialect",
                    "ordinal",
                    "prev_checkpoint",
                    "strategy",
                    "merkle_root",
                    "developer",
                ],
                vec![
                    Value::text("c1"),
                    Value::text("rift"),
                    Value::Int(1),
                    Value::Null,
                    Value::Null,
                    Value::Null,
                    Value::Null,
                ],
            ),
            (
                "n_file_revision",
                &["node_id", "path", "blob_oid", "content_sha256", "size"],
                vec![
                    Value::text("rev_a"),
                    Value::text(PATH),
                    Value::text(&sha),
                    Value::text(&sha),
                    Value::Int(10),
                ],
            ),
            (
                "n_file_read",
                &[
                    "node_id",
                    "checkpoint_id",
                    "tool_call_id",
                    "file_revision",
                    "input_tokens",
                    "read_ordinal",
                    "byte_range_lo",
                    "byte_range_hi",
                ],
                vec![
                    Value::text("read_a"),
                    Value::text("c1"),
                    Value::text("tc_a"),
                    Value::text("rev_a"),
                    Value::Int(400),
                    Value::Int(1),
                    Value::Null,
                    Value::Null,
                ],
            ),
        ];
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        for (table, columns, values) in rows {
            rt.block_on(store.insert_or_ignore(table, columns, &values))
                .unwrap();
        }
        store
    }

    fn journal_read(turn: u32) -> Line {
        Line::Read {
            turn,
            path: PATH.to_owned(),
            hash: revision(),
            range: Interval::new(1, 40).unwrap(),
            total_lines: 40,
            at_ms: u64::from(turn),
        }
    }

    /// The manifest entry the pull serves for [`PATH`], through the same path
    /// `tool_call_raw` takes once the window is resolved.
    fn pulled_entry(repo: &Path, window: Option<&str>) -> serde_json::Value {
        let store = projection();
        let req = ToolRequest {
            task: Some("fix a.rs".into()),
            files_in_focus: vec![PATH.into()],
            known_hashes: vec![revision().to_hex()],
            token_budget: 2000,
            ..ToolRequest::default()
        };
        let req = with_seen_turns(repo, window, &req);
        let scope = Scope::unbounded("repo");
        let pull = serve_raw(
            &store,
            repo,
            &scope,
            &NoAncestry,
            &NoProvenance,
            ToolName::KnownState,
            &req,
        );
        let resp = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(pull)
            .unwrap();
        resp.payload["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["path"] == PATH)
            .cloned()
            .unwrap()
    }

    #[test]
    fn a_manifest_pull_cites_the_turn_its_window_last_read_the_revision() {
        let repo = tempfile::tempdir().unwrap();
        let journal = crate::window_journal::WindowJournal::open(repo.path(), "thread-1", None);
        journal.append(&journal_read(3)).unwrap();
        journal.append(&journal_read(7)).unwrap();
        let entry = pulled_entry(repo.path(), Some("thread-1"));
        assert_eq!(entry["state"], "known_unchanged");
        assert_eq!(entry["last_seen_turn"], 7);
    }

    #[test]
    fn a_pull_with_no_window_cites_no_turn() {
        let repo = tempfile::tempdir().unwrap();
        let journal = crate::window_journal::WindowJournal::open(repo.path(), "thread-1", None);
        journal.append(&journal_read(3)).unwrap();
        let entry = pulled_entry(repo.path(), None);
        assert_eq!(
            entry["state"], "known_unchanged",
            "the claim does not depend on a turn"
        );
        assert!(entry.get("last_seen_turn").is_none());
        // Another window's journal is not this one's evidence either.
        let elsewhere = pulled_entry(repo.path(), Some("thread-2"));
        assert!(elsewhere.get("last_seen_turn").is_none());
    }
}
