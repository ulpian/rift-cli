//! The tool-call relations: a tool call, the files it read or edited, the
//! tokens its result and its reads put in front of the model, the captured
//! read facts a checkpoint shard carries, and the `content_sha256 → blob_oid`
//! map resolvers consult.

use rift_core::Result;

use crate::{CanonicalStore, Value};

/// A `tool_call` row: one tool invocation within a turn.
#[derive(Debug, Clone)]
pub struct ToolCallRow {
    /// Content-addressed tool-call id.
    pub tool_call_id: String,
    /// The turn.
    pub turn_id: String,
    /// The tool, agent-namespaced.
    pub tool_name: String,
    /// Valid-time (epoch ms).
    pub started_at: i64,
    /// Row schema version.
    pub schema_version: i64,
}

/// Record a `tool_call` (idempotent on its content-addressed key).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_tool_call(store: &impl CanonicalStore, r: &ToolCallRow) -> Result<bool> {
    store
        .insert_or_ignore(
            "tool_call",
            &[
                "tool_call_id",
                "turn_id",
                "tool_name",
                "started_at",
                "schema_version",
            ],
            &[
                Value::text(r.tool_call_id.clone()),
                Value::text(r.turn_id.clone()),
                Value::text(r.tool_name.clone()),
                Value::Int(r.started_at),
                Value::Int(r.schema_version),
            ],
        )
        .await
}

/// Record a `tool_reads_file` link (M:N, its own relation — 4NF).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_tool_reads_file(
    store: &impl CanonicalStore,
    tool_call_id: &str,
    file_revision_id: &str,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "tool_reads_file",
            &["tool_call_id", "file_revision_id"],
            &[
                Value::text(tool_call_id.to_owned()),
                Value::text(file_revision_id.to_owned()),
            ],
        )
        .await
}

/// Record a `tool_result_tokens` measurement: what one tool result put in
/// front of the model, on whichever channel it arrived.
///
/// The row that makes the savings denominator **repository content**, not
/// read-shaped calls: a search returns repository content too, resolves to no
/// file, and therefore has no `file_read_tokens` key at all. Keyed by the
/// result's own content-addressed id, so a redelivered job writes no second
/// row.
///
/// `basis` names how the figure was arrived at
/// ([`crate::schema::TOOL_RESULT_BASES`]); an `unmeasured` row carries zero
/// tokens and exists to be counted rather than summed.
///
/// # Errors
/// [`rift_core::RiftError::ConstraintRefused`] if `channel` or `basis` is
/// outside its closed vocabulary — permanent, so a queued job that meets it
/// is dropped, not retried; [`rift_core::RiftError::Storage`] on any other
/// non-conflict insert failure.
#[allow(clippy::too_many_arguments)] // one row, one argument per column it fills
pub async fn record_tool_result_tokens(
    store: &impl CanonicalStore,
    tool_result_id: &str,
    session_id: &str,
    tool_name: &str,
    channel: &str,
    output_tokens: u32,
    tokenizer: &str,
    basis: &str,
    recorded_at: i64,
    schema_version: i64,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "tool_result_tokens",
            &[
                "tool_result_id",
                "session_id",
                "tool_name",
                "channel",
                "output_tokens",
                "tokenizer",
                "basis",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(tool_result_id.to_owned()),
                Value::text(session_id.to_owned()),
                Value::text(tool_name.to_owned()),
                Value::text(channel.to_owned()),
                Value::Int(i64::from(output_tokens)),
                Value::text(tokenizer.to_owned()),
                Value::text(basis.to_owned()),
                Value::Int(recorded_at),
                Value::Int(schema_version),
            ],
        )
        .await
}

/// Record a `file_read_tokens` measurement: the token count of one
/// read's bytes, measured at read time — the only moment a modified working
/// file's content is certainly in hand. Keyed by the read's own tool call, so
/// it lands atomically with the fact it describes and is idempotent on replay.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_file_read_tokens(
    store: &impl CanonicalStore,
    tool_call_id: &str,
    input_tokens: u32,
    tokenizer: &str,
    schema_version: i64,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "file_read_tokens",
            &[
                "tool_call_id",
                "input_tokens",
                "tokenizer",
                "schema_version",
            ],
            &[
                Value::text(tool_call_id.to_owned()),
                Value::Int(i64::from(input_tokens)),
                Value::text(tokenizer.to_owned()),
                Value::Int(schema_version),
            ],
        )
        .await
}

/// The capture projection's whole `content_sha256 → blob_oid` mapping (the
/// historical token resolution: hash → oid → object-store bytes). One query —
/// resolvers consult it per read, and `content_sha256` is unindexed, so
/// per-hash lookups would table-scan per read. First row wins per hash; the
/// mapping is content-addressed, so every row for a hash names the same oid.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn content_blob_oid_map(
    store: &impl CanonicalStore,
) -> Result<std::collections::BTreeMap<String, String>> {
    let rows = store
        .query(
            "SELECT content_sha256, blob_oid FROM file_revision \
             ORDER BY file_revision_id ASC",
            &[],
        )
        .await?;
    let mut out = std::collections::BTreeMap::new();
    for r in &rows {
        out.entry(r.get_text(0)?).or_insert(r.get_text(1)?);
    }
    Ok(out)
}

/// Record a `tool_edits_file` link (distinct from reads — they vary independently).
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a non-conflict insert failure.
pub async fn record_tool_edits_file(
    store: &impl CanonicalStore,
    tool_call_id: &str,
    file_revision_id: &str,
) -> Result<bool> {
    store
        .insert_or_ignore(
            "tool_edits_file",
            &["tool_call_id", "file_revision_id"],
            &[
                Value::text(tool_call_id.to_owned()),
                Value::text(file_revision_id.to_owned()),
            ],
        )
        .await
}

/// One captured read fact for a session: the tool call that read `path` whose
/// bytes hashed to `content_sha256` at read time — exactly the triple
/// the checkpoint shard's `tool_calls.jsonl` line carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionReadFact {
    /// The tool-call row id anchoring the read.
    pub tool_call_id: String,
    /// The tool name (`Read`-shaped).
    pub tool_name: String,
    /// Repo-relative path.
    pub path: String,
    /// SHA-256 (hex) of the bytes at read time.
    pub content_sha256: String,
    /// Token estimate over the read-time bytes, when one was captured
    /// (facts recorded before the v6 migration have none).
    pub input_tokens: Option<u32>,
    /// The tokenizer that counted [`Self::input_tokens`], beside it.
    pub tokenizer: Option<String>,
    /// When the read's tool call was made (`tool_call.started_at`).
    pub started_at: i64,
    /// The ordinal of the read's turn (`turn.ordinal`).
    pub turn_ordinal: i64,
    /// The git blob oid of the revision read (`file_revision.blob_oid`).
    pub blob_oid: String,
}

/// Every read fact captured for `session_row_id` (the projected session id),
/// oldest first — the condense-time source of the shard's `tool_calls.jsonl`.
/// It reads the same rows the savings denominator joins (the call's instant,
/// its turn, the revision's blob oid, the count's tokenizer), so a rebuild
/// can write them back.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on a query failure.
pub async fn session_read_facts(
    store: &impl CanonicalStore,
    session_row_id: &str,
) -> Result<Vec<SessionReadFact>> {
    let rows = store
        .query(
            "SELECT tc.tool_call_id, tc.tool_name, fr.path, fr.content_sha256, frt.input_tokens, \
             frt.tokenizer, tc.started_at, t.ordinal, fr.blob_oid \
             FROM tool_reads_file trf \
             JOIN tool_call tc ON tc.tool_call_id = trf.tool_call_id \
             JOIN turn t ON t.turn_id = tc.turn_id \
             JOIN file_revision fr ON fr.file_revision_id = trf.file_revision_id \
             LEFT JOIN file_read_tokens frt ON frt.tool_call_id = trf.tool_call_id \
             WHERE t.session_id = ?1 \
             ORDER BY tc.started_at ASC, tc.tool_call_id ASC",
            &[Value::text(session_row_id.to_owned())],
        )
        .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        out.push(SessionReadFact {
            tool_call_id: r.get_text(0)?,
            tool_name: r.get_text(1)?,
            path: r.get_text(2)?,
            content_sha256: r.get_text(3)?,
            input_tokens: r.get_opt_i64(4)?.and_then(|t| u32::try_from(t).ok()),
            tokenizer: r.get_opt_text(5)?,
            started_at: r.get_i64(6)?,
            turn_ordinal: r.get_i64(7)?,
            blob_oid: r.get_text(8)?,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::fixtures::store;

    #[tokio::test]
    async fn edits_and_reads_are_independent_links_over_the_same_call() {
        // Two relations, deliberately not one (4NF: a tool may read a file it
        // never edits and edit one it never read). Recording one must not
        // satisfy the other.
        let s = store();
        assert!(record_tool_edits_file(&s, "call1", "rev1").await.unwrap());
        assert!(
            !record_tool_edits_file(&s, "call1", "rev1").await.unwrap(),
            "re-linking the same (call, revision) is a no-op"
        );
        assert!(record_tool_edits_file(&s, "call1", "rev2").await.unwrap());
        assert_eq!(s.count("tool_edits_file").await.unwrap(), 2);
        assert_eq!(
            s.count("tool_reads_file").await.unwrap(),
            0,
            "an edit link must not populate the read relation"
        );
    }
}
