//! The agent each session of a shard was captured from, read back from the
//! shard's `sessions.jsonl`, and the session rows it lets a rebuild write.
//!
//! A module of its own because the artifact has a grammar of its own, and
//! because every other step of the walk asks it one question — which agent
//! keys this native session — before it mints a session id. Without the
//! artifact the answer is the walk's default agent, which is what every shard
//! written before it gets, so such a shard projects exactly what it did.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_recall_substrate::parse::{
    IssueKind, Locator, ParseIssue, MAX_ARTIFACT_BYTES, MAX_JSONL_LINE_BYTES,
};
use rift_recall_substrate::SubstrateReader;
use rift_recall_types::checkpoint::CheckpointMetadata;
use rift_store::{
    record_capture_session_identity, record_session, CanonicalStore, CaptureSessionIdentityRow,
    SessionRow,
};
use serde_json::{Map, Value};

use crate::backend::SESSIONS_SCHEMA_VERSION;
use crate::ids;

use super::created_at::created_at_for;
use super::report::{note, BridgeReport};
use super::IngestCtx;

/// The artifact this module reads.
const ARTIFACT: &str = "sessions.jsonl";

/// Every member a line declares; a line carrying any other is refused.
const MEMBERS: [&str; 3] = ["agent", "schema_version", "session"];

/// The row schema the live projector stamps on the rows it writes.
const ROW_SCHEMA_VERSION: i64 = 1;

/// The agent of each session a shard names in `sessions.jsonl`, by native
/// session id. Empty for a shard without the artifact.
#[derive(Debug, Default)]
pub(super) struct SessionAgents(BTreeMap<String, String>);

impl SessionAgents {
    /// The agent that keys `native`: its own when the shard names one, else
    /// the walk's `default_agent`.
    pub(super) fn agent_of<'a>(&'a self, native: &str, default_agent: &'a str) -> &'a str {
        match self.0.get(native) {
            Some(agent) => agent,
            None => default_agent,
        }
    }
}

/// Read and parse the shard's `sessions.jsonl`. A refused line is counted and
/// named in the report, and its session keeps the walk's default agent.
pub(super) fn read_session_agents<S, R: SubstrateReader>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    id: &str,
) -> Result<SessionAgents> {
    let Some(bytes) = ctx.sub.read_artifact(id, ARTIFACT)? else {
        return Ok(SessionAgents::default());
    };
    let (agents, issues) = parse_sessions(&bytes, id);
    report.parse_issues += issues.len();
    for issue in &issues {
        note(report, issue_note(issue));
    }
    Ok(agents)
}

/// The session row and its native identity for every session the shard names
/// an agent for, as the live projector writes the pair.
///
/// Written after the usage lines, so a session a usage line already opened
/// keeps that line's model and earliest instant: every write is
/// insert-or-ignore on the live key. A session with no usage line names no
/// model and is dated by the shard's own capture time, the earliest instant
/// the shard records for it.
pub(super) async fn record_named_sessions<S: CanonicalStore, R>(
    ctx: &IngestCtx<'_, S, R>,
    agents: &SessionAgents,
    metadata: &CheckpointMetadata,
) -> Result<()> {
    let started_at = created_at_for(metadata);
    for (native, agent) in &agents.0 {
        let session_id = ids::session_row_id(agent, native);
        record_session(
            ctx.store,
            &SessionRow {
                session_id: session_id.clone(),
                agent: agent.clone(),
                model_primary: String::new(),
                repo_id: ctx.repo_id.to_owned(),
                started_at,
                schema_version: ROW_SCHEMA_VERSION,
            },
        )
        .await?;
        record_capture_session_identity(
            ctx.store,
            &CaptureSessionIdentityRow {
                session_id,
                agent: agent.clone(),
                native_session_id: native.clone(),
                schema_version: ROW_SCHEMA_VERSION,
            },
        )
        .await?;
    }
    Ok(())
}

/// Parse `sessions.jsonl`: one entry per line that declares exactly the three
/// members, a known schema version and an agent from the first-party
/// vocabulary. A refused line is one [`ParseIssue`] naming the shard and the
/// line number, never a byte of the line.
fn parse_sessions(bytes: &[u8], checkpoint_id: &str) -> (SessionAgents, Vec<ParseIssue>) {
    let issue = |locator: Locator, kind: IssueKind, detail: String| ParseIssue {
        checkpoint_id: checkpoint_id.to_owned(),
        artifact: ARTIFACT,
        locator,
        kind,
        detail,
    };
    let mut agents = SessionAgents::default();
    if bytes.len() > MAX_ARTIFACT_BYTES {
        let detail = format!("{} bytes over cap", bytes.len());
        let refused = issue(Locator::WholeFile, IssueKind::OversizeSkipped, detail);
        return (agents, vec![refused]);
    }
    let mut issues = Vec::new();
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
        match session_line(line) {
            Ok((session, agent)) => {
                agents.0.insert(session, agent);
            }
            Err(detail) => issues.push(issue(locator, IssueKind::Corrupt, detail)),
        }
    }
    (agents, issues)
}

/// One line under the declared grammar, or the reason it is refused.
fn session_line(line: &[u8]) -> std::result::Result<(String, String), String> {
    let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(line) else {
        return Err("not a JSON object".to_owned());
    };
    if obj.keys().any(|key| !MEMBERS.contains(&key.as_str())) {
        return Err("carries an undeclared member".to_owned());
    }
    let version = obj.get("schema_version").and_then(Value::as_i64);
    if version != Some(i64::from(SESSIONS_SCHEMA_VERSION)) {
        return Err("declares a schema_version this reader does not know".to_owned());
    }
    let session = string_member(&obj, "session")?;
    let agent = string_member(&obj, "agent")?;
    // The writer freezes an agent outside the vocabulary as a placeholder; a
    // session keyed by the placeholder would be a session no live path wrote.
    let known = rift_core::AgentId::FIRST_PARTY
        .iter()
        .any(|id| id.as_str() == agent);
    if !known {
        return Err("member `agent` is not a first-party agent".to_owned());
    }
    Ok((session, agent))
}

fn string_member(obj: &Map<String, Value>, name: &str) -> std::result::Result<String, String> {
    match obj.get(name).and_then(Value::as_str) {
        Some(value) if !value.is_empty() => Ok(value.to_owned()),
        Some(_) => Err(format!("member `{name}` is empty")),
        None => Err(format!("member `{name}` is missing or not a string")),
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
