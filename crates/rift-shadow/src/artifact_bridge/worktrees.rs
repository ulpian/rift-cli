//! The worktree observations a shard froze in `session_worktrees.jsonl`, read
//! back into the capture projection's two observation relations.
//!
//! A module of its own because the artifact has a grammar of its own and one
//! fact no other artifact needs: the checkout's absolute path is never
//! frozen, so a rebuild resolves each frozen worktree token to this machine's
//! checkout of the family. `HEAD` is not frozen either, so a restored row
//! carries none, and the rebuild writes a session's observations only where
//! the projection holds none for it: over a live projection it writes
//! nothing. Only an agent hook's observations are frozen, so a session known
//! only through `commit`, `attach` or `import` restores none. A shard without the artifact restores no observation, which is
//! what every shard written before it gets.

use std::collections::BTreeMap;
use std::path::Path;

use rift_core::Result;
use rift_recall_substrate::parse::{
    IssueKind, Locator, ParseIssue, MAX_ARTIFACT_BYTES, MAX_JSONL_LINE_BYTES,
};
use rift_recall_substrate::SubstrateReader;
use rift_store::{record_session_worktree, CanonicalStore, SessionWorktreeRow};
use serde_json::{Map, Value};

use crate::backend::{ArchivedWorktreeObservation, SESSION_WORKTREES_SCHEMA_VERSION};
use crate::ids;

use super::report::{note, BridgeReport};
use super::sessions::SessionAgents;
use super::IngestCtx;

/// The artifact this module reads.
const ARTIFACT: &str = "session_worktrees.jsonl";

/// Every member a line may declare; a line carrying any other is refused.
const MEMBERS: [&str; 9] = [
    "at",
    "basis",
    "branch",
    "linked",
    "name",
    "schema_version",
    "session",
    "subagent",
    "token",
];

/// The row schema the live writers stamp on the rows they write.
const ROW_SCHEMA_VERSION: i64 = 1;

/// Restore the observations the shard's `session_worktrees.jsonl` froze for
/// every session the projection does not already place, each under the
/// session's own agent. A session with any observation in either relation is
/// left as it is — the usage rows' rule, that a rebuild only fills what is
/// absent — so levelling into the live projection writes nothing, and a
/// restored row never sits beside a live one that carries its head. A
/// refused line is counted and named in the report, and skipped.
///
/// Written after the shard's sessions, so the session row an observation
/// hangs off exists when it lands.
pub(super) async fn restore_worktree_observations<S: CanonicalStore, R: SubstrateReader>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    id: &str,
    agents: &SessionAgents,
) -> Result<()> {
    let Some(bytes) = ctx.sub.read_artifact(id, ARTIFACT)? else {
        return Ok(());
    };
    let (observations, issues) = parse_observations(&bytes, id);
    report.parse_issues += issues.len();
    for issue in &issues {
        note(report, issue_note(issue));
    }
    if observations.is_empty() {
        return Ok(());
    }
    let checkouts = member_checkouts(ctx.sub.worktree_root().as_deref());
    let mut placed: BTreeMap<String, bool> = BTreeMap::new();
    for seen in &observations {
        let agent = agents.agent_of(&seen.session, ctx.default_agent);
        let session_id = ids::session_row_id(agent, &seen.session);
        let already = match placed.get(&session_id) {
            Some(already) => *already,
            None => is_placed(ctx.store, &session_id).await?,
        };
        placed.insert(session_id.clone(), already);
        if already {
            continue;
        }
        let row = observation_row(seen, session_id, &checkouts);
        record_session_worktree(ctx.store, &row).await?;
    }
    Ok(())
}

/// Whether the projection holds any observation of `session_id`, in either
/// relation, before this shard's lines are written.
async fn is_placed(store: &impl CanonicalStore, session_id: &str) -> Result<bool> {
    let relation = SessionWorktreeRow::relation(store).await;
    let sql = format!("SELECT 1 FROM {relation} WHERE session_id = ? LIMIT 1");
    let rows = store
        .query(&sql, &[rift_store::Value::text(session_id)])
        .await?;
    Ok(!rows.is_empty())
}

/// The projection row one frozen observation restores. The id is derived
/// as the live writer derives it, from the state and, for a `shell_cd`
/// observation, its basis, with the head absent because the shard carries
/// none; `head_oid` is NULL for the same reason.
///
/// `work_tree` is this machine's checkout of the member the token names, or
/// the empty string when no member here has that token: no reader spells an
/// unknown path otherwise, and another machine's path would name nothing here.
fn observation_row(
    seen: &ArchivedWorktreeObservation,
    session_id: String,
    checkouts: &BTreeMap<String, String>,
) -> SessionWorktreeRow {
    let state_id = ids::session_worktree_observation_id(
        &session_id,
        &seen.token,
        seen.branch.as_deref(),
        None,
        seen.subagent.as_deref(),
    );
    SessionWorktreeRow {
        observation_id: ids::session_worktree_filed_id(&state_id, &seen.basis),
        session_id,
        worktree_token: seen.token.clone(),
        worktree_name: seen.name.clone(),
        is_linked: i64::from(seen.linked),
        work_tree: checkouts.get(&seen.token).cloned().unwrap_or_default(),
        branch: seen.branch.clone(),
        head_oid: None,
        subagent_id: seen.subagent.clone(),
        basis: seen.basis.clone(),
        first_observed_at: seen.at,
        schema_version: ROW_SCHEMA_VERSION,
    }
}

/// Every member of the family the reader's checkout belongs to, by worktree
/// token: the canonical path of each checkout present on this machine, the
/// spelling the live writer stores. Empty when the reader names no checkout.
///
/// A token hashes the checkout's path as it was spelled when discovered, so
/// each member is keyed under every spelling this side knows — the reader's
/// own, the one git records and the canonical one. A token frozen from a
/// spelling none of them matches resolves to nothing.
fn member_checkouts(root: Option<&Path>) -> BTreeMap<String, String> {
    let Some(root) = root else {
        return BTreeMap::new();
    };
    let Ok(paths) = rift_git::discover(root) else {
        return BTreeMap::new();
    };
    let mut spellings = vec![root.to_path_buf()];
    for member in rift_git::family_members(&paths) {
        if member.prunable {
            continue;
        }
        if let Ok(canonical) = std::fs::canonicalize(&member.work_tree) {
            spellings.push(canonical);
        }
        spellings.push(member.work_tree);
    }
    let mut out = BTreeMap::new();
    for spelling in spellings {
        let Ok(found) = rift_git::discover(&spelling) else {
            continue;
        };
        let address = std::fs::canonicalize(&found.work_tree)
            .unwrap_or_else(|_| found.work_tree.clone())
            .to_string_lossy()
            .into_owned();
        out.insert(found.worktree_token(), address);
    }
    out
}

/// Parse `session_worktrees.jsonl`: one observation per line that declares
/// only the declared members, a known schema version, and values the writer
/// could have frozen. A refused line is one [`ParseIssue`] naming the shard
/// and the line number, never a byte of the line.
fn parse_observations(
    bytes: &[u8],
    checkpoint_id: &str,
) -> (Vec<ArchivedWorktreeObservation>, Vec<ParseIssue>) {
    let issue = |locator: Locator, kind: IssueKind, detail: String| ParseIssue {
        checkpoint_id: checkpoint_id.to_owned(),
        artifact: ARTIFACT,
        locator,
        kind,
        detail,
    };
    if bytes.len() > MAX_ARTIFACT_BYTES {
        let detail = format!("{} bytes over cap", bytes.len());
        let refused = issue(Locator::WholeFile, IssueKind::OversizeSkipped, detail);
        return (Vec::new(), vec![refused]);
    }
    let (mut observations, mut issues) = (Vec::new(), Vec::new());
    for (n, line) in bytes.split(|&b| b == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let locator = Locator::Line(n + 1);
        if line.len() > MAX_JSONL_LINE_BYTES {
            let detail = format!("{} bytes over cap", line.len());
            issues.push(issue(locator, IssueKind::OversizeSkipped, detail));
            continue;
        }
        match observation_line(line) {
            Ok(seen) => observations.push(seen),
            Err(detail) => issues.push(issue(locator, IssueKind::Corrupt, detail)),
        }
    }
    (observations, issues)
}

/// One line under the declared grammar, or the reason it is refused.
fn observation_line(line: &[u8]) -> std::result::Result<ArchivedWorktreeObservation, String> {
    let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(line) else {
        return Err("not a JSON object".to_owned());
    };
    if obj.keys().any(|key| !MEMBERS.contains(&key.as_str())) {
        return Err("carries an undeclared member".to_owned());
    }
    let version = obj.get("schema_version").and_then(Value::as_i64);
    if version != Some(i64::from(SESSION_WORKTREES_SCHEMA_VERSION)) {
        return Err("declares a schema_version this reader does not know".to_owned());
    }
    let (Some(at), Some(linked)) = (
        obj.get("at").and_then(Value::as_i64),
        obj.get("linked").and_then(Value::as_bool),
    ) else {
        return Err("member `at` or `linked` is missing or mistyped".to_owned());
    };
    let seen = ArchivedWorktreeObservation {
        session: string_member(&obj, "session")?,
        token: string_member(&obj, "token")?,
        name: string_member(&obj, "name")?,
        linked,
        branch: optional_member(&obj, "branch")?,
        subagent: optional_member(&obj, "subagent")?,
        basis: string_member(&obj, "basis")?,
        at,
    };
    // The writer's own screen: a line it could not have frozen restores a
    // row no live path wrote.
    if !seen.is_freezable() {
        return Err("a member falls outside its declared shape".to_owned());
    }
    Ok(seen)
}

fn string_member(obj: &Map<String, Value>, name: &str) -> std::result::Result<String, String> {
    match obj.get(name).and_then(Value::as_str) {
        Some(value) => Ok(value.to_owned()),
        None => Err(format!("member `{name}` is missing or not a string")),
    }
}

fn optional_member(
    obj: &Map<String, Value>,
    name: &str,
) -> std::result::Result<Option<String>, String> {
    match obj.get(name) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(format!("member `{name}` is not a string")),
    }
}

/// The report line for one refused line: the shard, the artifact, where, why.
fn issue_note(issue: &ParseIssue) -> String {
    let at = match issue.locator {
        Locator::WholeFile => "the whole file".to_owned(),
        Locator::Line(n) => format!("line {n}"),
    };
    format!(
        "{}: {} {at} skipped: {}",
        issue.checkpoint_id, issue.artifact, issue.detail
    )
}
