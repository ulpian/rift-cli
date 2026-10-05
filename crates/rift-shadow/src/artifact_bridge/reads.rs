//! The reads a shard froze into `tool_calls.jsonl`, read back into the capture
//! projection as the rows the savings denominator joins: the tool call, the
//! file revision it read, the link between them and the read's token count.
//!
//! A module of its own because only a line carrying the read's instant, its
//! turn and its blob oid can stand for those rows, and every line written
//! before those members existed carries none of them. Such a line projects
//! nothing here and is not a parse issue: the recall plane reads the same
//! artifact for its own purpose, and an old line is a complete line for it.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_recall_substrate::parse::{MAX_ARTIFACT_BYTES, MAX_JSONL_LINE_BYTES};
use rift_recall_substrate::SubstrateReader;
use rift_recall_types::checkpoint::CheckpointMetadata;
use rift_store::{
    record_file_read_tokens, record_file_revision, record_tool_call, record_tool_reads_file,
    record_turn, CanonicalStore, FileRevisionRow, ToolCallRow, TurnRow,
};
use serde_json::{Map, Value};

use crate::ids;

use super::sessions::SessionAgents;
use super::IngestCtx;

/// The artifact this module reads.
const ARTIFACT: &str = "tool_calls.jsonl";

/// The tokenizer tags a frozen count may carry (the writer's closed list); a
/// `withheld` tag is not one of them, so its count is not restored.
const TOKENIZERS: [&str; 2] = ["cl100k_base", "chars/4+1"];

/// The row schema the live projector stamps on the rows it writes from a
/// canonical event.
const ROW_SCHEMA_VERSION: i64 = 1;

/// One read, as a `tool_calls.jsonl` line carrying the denominator's members.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReadLine {
    call_id: String,
    tool: String,
    path: String,
    sha256: String,
    at: i64,
    turn: u32,
    blob_oid: String,
    tokens: Option<(u32, String)>,
}

/// Project the shard's restorable reads, when it carries any. Only a shard
/// that names exactly one session can say whose reads they are, because a
/// line names no session; the condense path archives one session per shard.
pub(super) async fn project_read_lines<S: CanonicalStore, R: SubstrateReader>(
    ctx: &IngestCtx<'_, S, R>,
    id: &str,
    metadata: &CheckpointMetadata,
    agents: &SessionAgents,
) -> Result<()> {
    let [session] = metadata.sessions.as_slice() else {
        return Ok(());
    };
    let Some(bytes) = ctx.sub.read_artifact(id, ARTIFACT)? else {
        return Ok(());
    };
    let lines = restorable_lines(&bytes);
    let session_row = ids::session_row_id(agents.agent_of(session, ctx.default_agent), session);
    let mut turn_starts: BTreeMap<u32, i64> = BTreeMap::new();
    for line in &lines {
        let start = turn_starts.entry(line.turn).or_insert(line.at);
        *start = (*start).min(line.at);
    }
    for (turn, started_at) in &turn_starts {
        record_turn(
            ctx.store,
            &TurnRow {
                turn_id: ids::turn_row_id(&session_row, *turn),
                session_id: session_row.clone(),
                ordinal: i64::from(*turn),
                started_at: *started_at,
                schema_version: ROW_SCHEMA_VERSION,
            },
        )
        .await?;
    }
    for line in &lines {
        project_line(ctx.store, &session_row, line).await?;
    }
    Ok(())
}

/// The rows one read stands for, keyed exactly as the live projector keys
/// them (`project::project_read`): the call under its frozen id, the revision
/// under its path-and-oid id, the link, and the count when the line has one.
/// Every write is insert-or-ignore, so a row the live path wrote wins.
async fn project_line(
    store: &impl CanonicalStore,
    session_row: &str,
    line: &ReadLine,
) -> Result<()> {
    record_tool_call(
        store,
        &ToolCallRow {
            tool_call_id: line.call_id.clone(),
            turn_id: ids::turn_row_id(session_row, line.turn),
            tool_name: line.tool.clone(),
            started_at: line.at,
            schema_version: ROW_SCHEMA_VERSION,
        },
    )
    .await?;
    let revision_id =
        rift_shadow_attribution::file_revision_id(&line.path, line.blob_oid.as_bytes())
            .sha256()
            .to_hex();
    record_file_revision(
        store,
        &FileRevisionRow {
            file_revision_id: revision_id.clone(),
            path: line.path.clone(),
            blob_oid: line.blob_oid.clone(),
            content_sha256: line.sha256.clone(),
            schema_version: ROW_SCHEMA_VERSION,
        },
    )
    .await?;
    record_tool_reads_file(store, &line.call_id, &revision_id).await?;
    if let Some((input_tokens, tokenizer)) = &line.tokens {
        record_file_read_tokens(
            store,
            &line.call_id,
            *input_tokens,
            tokenizer,
            ROW_SCHEMA_VERSION,
        )
        .await?;
    }
    Ok(())
}

/// Every line that carries what the rows need. A line that does not (an old
/// line, a withheld oid, a malformed one) is passed over: it is still the
/// recall plane's read fact, and it names no row this reader may write.
fn restorable_lines(bytes: &[u8]) -> Vec<ReadLine> {
    if bytes.len() > MAX_ARTIFACT_BYTES {
        return Vec::new();
    }
    bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty() && line.len() <= MAX_JSONL_LINE_BYTES)
        .filter_map(|line| match serde_json::from_slice::<Value>(line) {
            Ok(Value::Object(obj)) => read_line(&obj),
            _ => None,
        })
        .collect()
}

/// One line's rows, or `None` when it lacks a member they need.
fn read_line(obj: &Map<String, Value>) -> Option<ReadLine> {
    let text = |name: &str| obj.get(name).and_then(Value::as_str).map(str::to_owned);
    let blob_oid = text("blob_oid").filter(|oid| is_object_id(oid))?;
    let turn = obj.get("turn").and_then(Value::as_u64)?;
    Some(ReadLine {
        call_id: text("id")?,
        tool: text("tool")?,
        path: text("path")?,
        sha256: text("sha256")?,
        at: obj.get("at").and_then(Value::as_i64)?,
        turn: u32::try_from(turn).ok()?,
        blob_oid,
        tokens: read_tokens(obj),
    })
}

/// The count and its tokenizer, when the line carries both and the tokenizer
/// is one the writer may freeze.
fn read_tokens(obj: &Map<String, Value>) -> Option<(u32, String)> {
    let count = obj.get("input_tokens").and_then(Value::as_u64)?;
    let tokenizer = obj.get("tokenizer").and_then(Value::as_str)?;
    if !TOKENIZERS.contains(&tokenizer) {
        return None;
    }
    Some((u32::try_from(count).ok()?, tokenizer.to_owned()))
}

/// Whether `oid` has a git object id's shape (40 or 64 lowercase hex digits),
/// the only shape the writer freezes.
fn is_object_id(oid: &str) -> bool {
    matches!(oid.len(), 40 | 64)
        && oid
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
