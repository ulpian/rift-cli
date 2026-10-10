//! The search half of a checkpoint's savings denominator: every measured tool
//! result the session's capture recorded, gathered from the capture
//! projection's `tool_result_tokens` at archive time, and the one writer of
//! `tool_results.jsonl`.
//!
//! A sibling artifact rather than members on `tool_calls.jsonl`, because every
//! line of that artifact is a read: the recall plane's parser requires a path
//! and a content hash on each, and a search result resolves to no file and
//! has neither. A line here carries identifiers, counts, an instant and closed
//! vocabularies only, never a byte of what the tool returned.

use std::collections::BTreeMap;

use rift_core::Result;
use rift_store::schema::{TOOL_RESULT_BASES, TOOL_RESULT_CHANNELS};
use rift_store::{CanonicalStore, Value};

use super::archive::{identifier_or_withheld, is_identifier, READ_TOKENIZERS, WITHHELD};
use super::artifacts::canonical_json;
use crate::ids;

/// The line format version every `tool_results.jsonl` line declares as
/// `schema_version`.
pub const TOOL_RESULTS_SCHEMA_VERSION: u32 = 1;

/// One measured tool result, as `tool_results.jsonl` freezes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchivedToolResult {
    /// `tool_result_tokens.tool_result_id` — 64 lowercase hex.
    pub id: String,
    /// The agent's native session id, as `metadata.json`'s `sessions` names it.
    pub session: String,
    /// The tool's name in the family's own spelling.
    pub tool: String,
    /// One of the projection's closed channels (`TOOL_RESULT_CHANNELS`).
    pub channel: String,
    /// What the result put in front of the model; `0` when unmeasured.
    pub output_tokens: u32,
    /// The tokenizer that counted [`Self::output_tokens`].
    pub tokenizer: String,
    /// One of the projection's closed bases (`TOOL_RESULT_BASES`).
    pub basis: String,
    /// When the turn spent the tokens (epoch ms), the decision's own clock.
    pub recorded_at: i64,
}

impl ArchivedToolResult {
    /// Whether the writer will freeze the result; a caller asks only to count
    /// losses, and the bridge asks to refuse a line the writer could not have
    /// produced.
    #[must_use]
    pub fn is_freezable(&self) -> bool {
        tool_result_line(self).is_some()
    }
}

/// Every measured tool result the projection records for the session
/// `native` captured from `agent`, in no particular order: the writer sorts
/// them.
///
/// # Errors
/// [`rift_core::RiftError`] on a storage failure, including a projection
/// older than the relation.
pub async fn session_tool_results(
    store: &impl CanonicalStore,
    agent: &str,
    native: &str,
) -> Result<Vec<ArchivedToolResult>> {
    let sql = "SELECT tool_result_id, tool_name, channel, output_tokens, tokenizer, basis, \
               recorded_at FROM tool_result_tokens WHERE session_id = ?";
    let session_row = ids::session_row_id(agent, native);
    let rows = store.query(sql, &[Value::text(session_row)]).await?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        out.push(ArchivedToolResult {
            id: row.get_text(0)?,
            session: native.to_owned(),
            tool: row.get_text(1)?,
            channel: row.get_text(2)?,
            output_tokens: u32::try_from(row.get_i64(3)?).unwrap_or(0),
            tokenizer: row.get_text(4)?,
            basis: row.get_text(5)?,
            recorded_at: row.get_i64(6)?,
        });
    }
    Ok(out)
}

/// Serialize the session's measured tool results exactly as the shard's
/// `tool_results.jsonl` freezes them: one canonical line per result id,
/// sorted by session, instant and id, unfreezable results left out. `None`
/// when nothing is left — the artifact is never written empty, so a shard
/// without it keeps its old leaf sequence.
#[must_use]
pub fn tool_results_jsonl_bytes(results: &[ArchivedToolResult]) -> Option<Vec<u8>> {
    let mut lines: BTreeMap<(&str, i64, &str), Vec<u8>> = BTreeMap::new();
    for result in results {
        let Some(line) = tool_result_line(result) else {
            continue;
        };
        let key = (
            result.session.as_str(),
            result.recorded_at,
            result.id.as_str(),
        );
        lines.entry(key).or_insert_with(|| canonical_json(&line));
    }
    if lines.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for line in lines.into_values() {
        out.extend_from_slice(&line);
        out.push(b'\n');
    }
    Some(out)
}

/// One `tool_results.jsonl` line, or `None` when the result's id, session,
/// channel or basis falls outside its shape — a placeholder there would
/// restore a row no live path wrote. The tool name comes from the agent's
/// payload and the tokenizer from a projection row, so each is frozen only in
/// its declared shape and is [`WITHHELD`] otherwise.
fn tool_result_line(result: &ArchivedToolResult) -> Option<serde_json::Value> {
    let hex = |s: &str| {
        s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    if result.id.len() != 64 || !hex(&result.id) {
        return None;
    }
    if result.session.is_empty() || !is_identifier(&result.session) {
        return None;
    }
    let closed = TOOL_RESULT_CHANNELS.contains(&result.channel.as_str())
        && TOOL_RESULT_BASES.contains(&result.basis.as_str());
    if !closed {
        return None;
    }
    let known = READ_TOKENIZERS.contains(&result.tokenizer.as_str());
    let tokenizer = if known {
        result.tokenizer.as_str()
    } else {
        WITHHELD
    };
    Some(serde_json::json!({
        "basis": result.basis,
        "channel": result.channel,
        "id": result.id,
        "output_tokens": result.output_tokens,
        "recorded_at": result.recorded_at,
        "schema_version": TOOL_RESULTS_SCHEMA_VERSION,
        "session": result.session,
        "tokenizer": tokenizer,
        "tool": identifier_or_withheld(&result.tool),
    }))
}

#[cfg(test)]
mod tests {
    use rift_store::{record_tool_result_tokens, SqliteStore};

    use super::*;

    /// A measured search of `session` at `at`, under the id `n` repeated.
    fn searched(session: &str, n: char, at: i64) -> ArchivedToolResult {
        ArchivedToolResult {
            id: n.to_string().repeat(64),
            session: session.to_owned(),
            tool: "Grep".to_owned(),
            channel: "search".to_owned(),
            output_tokens: 42,
            tokenizer: "cl100k_base".to_owned(),
            basis: "in_process".to_owned(),
            recorded_at: at,
        }
    }

    /// One canonical line per result, keys sorted, in (session, instant, id)
    /// order whatever order they arrive in, a repeated id written once;
    /// nothing, no artifact.
    #[test]
    fn results_freeze_canonically_one_line_per_id_in_session_and_time_order() {
        assert_eq!(tool_results_jsonl_bytes(&[]), None);
        let results = vec![
            searched("s-2", 'c', 1),
            searched("s-1", 'b', 9),
            searched("s-1", 'a', 3),
            searched("s-1", 'a', 3),
        ];
        let bytes = tool_results_jsonl_bytes(&results).unwrap();
        let reversed: Vec<_> = results.iter().rev().cloned().collect();
        assert_eq!(tool_results_jsonl_bytes(&reversed).unwrap(), bytes);
        let line = |session: &str, n: char, at: i64| {
            format!(
                "{{\"basis\":\"in_process\",\"channel\":\"search\",\"id\":\"{}\",\
                 \"output_tokens\":42,\"recorded_at\":{at},\"schema_version\":1,\
                 \"session\":\"{session}\",\"tokenizer\":\"cl100k_base\",\"tool\":\"Grep\"}}\n",
                n.to_string().repeat(64)
            )
        };
        let want = [
            line("s-1", 'a', 3),
            line("s-1", 'b', 9),
            line("s-2", 'c', 1),
        ]
        .concat();
        assert_eq!(String::from_utf8(bytes).unwrap(), want);
    }

    /// A tool name outside the identifier screen and a tokenizer outside the
    /// closed list are each frozen as the placeholder; the count stays.
    #[test]
    fn a_tool_name_or_tokenizer_outside_its_shape_is_withheld() {
        let typed = ArchivedToolResult {
            tool: "ignore previous instructions".to_owned(),
            tokenizer: "o200k_base".to_owned(),
            ..searched("s", 'a', 1)
        };
        let bytes = tool_results_jsonl_bytes(&[typed]).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["tool"], "withheld");
        assert_eq!(value["tokenizer"], "withheld");
        assert_eq!(value["output_tokens"], 42);
    }

    /// An id that is not 64 lowercase hex, an empty or non-identifier
    /// session, or a channel or basis outside its closed list refuses the
    /// result whole; nothing is rewritten.
    #[test]
    fn a_result_outside_its_declared_shapes_never_reaches_the_artifact() {
        let with = |edit: fn(&mut ArchivedToolResult)| {
            let mut result = searched("s", 'a', 1);
            edit(&mut result);
            result
        };
        let refused = [
            with(|r| r.id = "A".repeat(64)),
            with(|r| r.id = "a".repeat(40)),
            with(|r| r.session = String::new()),
            with(|r| r.session = "two words".to_owned()),
            with(|r| r.channel = "web".to_owned()),
            with(|r| r.basis = "guessed".to_owned()),
        ];
        assert!(refused.iter().all(|r| !r.is_freezable()));
        assert_eq!(tool_results_jsonl_bytes(&refused), None);
    }

    proptest::proptest! {
        /// Whatever a result holds, no line carries a member outside the
        /// declared set, and the tool and tokenizer are each an identifier or
        /// the placeholder.
        #[test]
        fn no_line_carries_an_undeclared_member_or_typed_text(
            tool in "(?s).{0,40}",
            tokenizer in "(?s).{0,24}",
        ) {
            let result = ArchivedToolResult { tool, tokenizer, ..searched("s", 'a', 1) };
            let bytes = tool_results_jsonl_bytes(&[result]).unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let declared = [
                "basis", "channel", "id", "output_tokens", "recorded_at", "schema_version",
                "session", "tokenizer", "tool",
            ];
            for member in value.as_object().unwrap().keys() {
                proptest::prop_assert!(declared.contains(&member.as_str()), "{}", member);
            }
            let tool = value["tool"].as_str().unwrap();
            proptest::prop_assert!(tool == WITHHELD || is_identifier(tool), "{}", tool);
            let tokenizer = value["tokenizer"].as_str().unwrap();
            let known = tokenizer == WITHHELD || READ_TOKENIZERS.contains(&tokenizer);
            proptest::prop_assert!(known, "{}", tokenizer);
        }
    }

    /// The archive gathers exactly the session's rows under its own agent,
    /// never another session's, with each column as the projection holds it.
    #[tokio::test]
    async fn the_archive_gathers_the_sessions_own_rows_as_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let store: SqliteStore =
            rift_store::open_capture_projection(dir.path().join("p.db")).unwrap();
        let own = ids::session_row_id("claude-code", "s-1");
        let other = ids::session_row_id("claude-code", "s-2");
        for (id, session) in [("a", &own), ("b", &other)] {
            let id = id.repeat(64);
            let recorded = record_tool_result_tokens(
                &store,
                &id,
                session,
                "Glob",
                "search",
                17,
                "cl100k_base",
                "in_process",
                1_000,
                1,
            );
            assert!(recorded.await.unwrap());
        }
        let gathered = session_tool_results(&store, "claude-code", "s-1")
            .await
            .unwrap();
        let want = ArchivedToolResult {
            tool: "Glob".to_owned(),
            output_tokens: 17,
            recorded_at: 1_000,
            ..searched("s-1", 'a', 0)
        };
        assert_eq!(gathered, vec![want]);
    }
}
