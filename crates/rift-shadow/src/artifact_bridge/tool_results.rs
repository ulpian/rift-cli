//! The measured tool results a shard froze in `tool_results.jsonl`, read back
//! into the capture projection's `tool_result_tokens`: the search half of the
//! savings denominator.
//!
//! A module of its own because the artifact has a grammar of its own. Each
//! row is written under its frozen id, its session keyed under the agent the
//! shard's `sessions.jsonl` names (the walk's default agent otherwise), with
//! the instant the shard froze rather than the rebuild's clock. Every write is
//! insert-or-ignore on the live key, so a row the live path wrote wins. A
//! line whose tokenizer was withheld restores nothing: a count nobody can say
//! how was taken is not summed. A shard without the artifact restores no
//! result, which is what every shard written before it gets.

use rift_core::Result;
use rift_recall_substrate::parse::{
    IssueKind, Locator, ParseIssue, MAX_ARTIFACT_BYTES, MAX_JSONL_LINE_BYTES,
};
use rift_recall_substrate::SubstrateReader;
use rift_store::{record_tool_result_tokens, CanonicalStore};
use serde_json::{Map, Value};

use crate::backend::{ArchivedToolResult, TOOL_RESULTS_SCHEMA_VERSION};
use crate::ids;

use super::report::{note, BridgeReport};
use super::sessions::SessionAgents;
use super::IngestCtx;

/// The artifact this module reads.
const ARTIFACT: &str = "tool_results.jsonl";

/// Every member a line declares; a line carrying any other is refused.
const MEMBERS: [&str; 9] = [
    "basis",
    "channel",
    "id",
    "output_tokens",
    "recorded_at",
    "schema_version",
    "session",
    "tokenizer",
    "tool",
];

/// The placeholder the writer freezes a tokenizer outside its list as.
const WITHHELD: &str = "withheld";

/// The row schema the live worker stamps on the rows it writes.
const ROW_SCHEMA_VERSION: i64 = 1;

/// Restore every measured tool result the shard's `tool_results.jsonl`
/// froze. A refused line is counted and named in the report, and skipped.
///
/// Written after the shard's sessions, so the session row a result hangs off
/// exists when it lands.
pub(super) async fn restore_tool_results<S: CanonicalStore, R: SubstrateReader>(
    ctx: &IngestCtx<'_, S, R>,
    report: &mut BridgeReport,
    id: &str,
    agents: &SessionAgents,
) -> Result<()> {
    let Some(bytes) = ctx.sub.read_artifact(id, ARTIFACT)? else {
        return Ok(());
    };
    let (results, issues) = parse_tool_results(&bytes, id);
    report.parse_issues += issues.len();
    for issue in &issues {
        note(report, issue_note(issue));
    }
    for result in results.iter().filter(|r| r.tokenizer != WITHHELD) {
        let agent = agents.agent_of(&result.session, ctx.default_agent);
        let session_row = ids::session_row_id(agent, &result.session);
        record_tool_result_tokens(
            ctx.store,
            &result.id,
            &session_row,
            &result.tool,
            &result.channel,
            result.output_tokens,
            &result.tokenizer,
            &result.basis,
            result.recorded_at,
            ROW_SCHEMA_VERSION,
        )
        .await?;
    }
    Ok(())
}

/// Parse `tool_results.jsonl`: one result per line that declares exactly the
/// declared members, a known schema version, and values the writer could
/// have frozen. A refused line is one [`ParseIssue`] naming the shard and the
/// line number, never a byte of the line.
fn parse_tool_results(
    bytes: &[u8],
    checkpoint_id: &str,
) -> (Vec<ArchivedToolResult>, Vec<ParseIssue>) {
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
    let (mut results, mut issues) = (Vec::new(), Vec::new());
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
        match tool_result_line(line) {
            Ok(result) => results.push(result),
            Err(detail) => issues.push(issue(locator, IssueKind::Corrupt, detail)),
        }
    }
    (results, issues)
}

/// One line under the declared grammar, or the reason it is refused.
fn tool_result_line(line: &[u8]) -> std::result::Result<ArchivedToolResult, String> {
    let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(line) else {
        return Err("not a JSON object".to_owned());
    };
    if obj.len() != MEMBERS.len() || obj.keys().any(|key| !MEMBERS.contains(&key.as_str())) {
        return Err("does not carry exactly the declared members".to_owned());
    }
    let version = obj.get("schema_version").and_then(Value::as_i64);
    if version != Some(i64::from(TOOL_RESULTS_SCHEMA_VERSION)) {
        return Err("declares a schema_version this reader does not know".to_owned());
    }
    let tokens = obj.get("output_tokens").and_then(Value::as_u64);
    let (Some(output_tokens), Some(recorded_at)) = (
        tokens.and_then(|n| u32::try_from(n).ok()),
        obj.get("recorded_at").and_then(Value::as_i64),
    ) else {
        return Err("member `output_tokens` or `recorded_at` is missing or mistyped".to_owned());
    };
    let result = ArchivedToolResult {
        id: string_member(&obj, "id")?,
        session: string_member(&obj, "session")?,
        tool: string_member(&obj, "tool")?,
        channel: string_member(&obj, "channel")?,
        output_tokens,
        tokenizer: string_member(&obj, "tokenizer")?,
        basis: string_member(&obj, "basis")?,
        recorded_at,
    };
    // The writer's own screen: a line it could not have frozen restores a
    // row no live path wrote.
    if !result.is_freezable() {
        return Err("a member falls outside its declared shape".to_owned());
    }
    Ok(result)
}

fn string_member(obj: &Map<String, Value>, name: &str) -> std::result::Result<String, String> {
    match obj.get(name).and_then(Value::as_str) {
        Some(value) => Ok(value.to_owned()),
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
