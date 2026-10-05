//! The one writer of the serve relations: it turns a `memd.*` serve envelope
//! into its recall rows.
//!
//! A separate module in this crate, not in the binary, because three callers
//! write the same rows — the agent hook's deferred worker, the skill CLI, and
//! the rebuild ingest that re-derives the rows from frozen envelopes — and a
//! second copy of the writer is a second opinion about the key. With one
//! function they mint the same key from the same envelope and leave one row.

use rift_core::Result;
use rift_recall_events::PullClaimBasis;

/// The pull claims of a window, summed: how many held revisions callers
/// named, how many of them this family had recorded reading, and how many no
/// recorded read accounts for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct PullClaimTotals {
    /// Revisions callers said they held.
    pub stated: u64,
    /// Of those, the ones this family recorded a read of.
    pub recorded: u64,
    /// Of those, the ones no recorded read accounts for.
    pub unrecorded: u64,
}

/// What a serve decision's own payload says, for its row in the legacy
/// `memd_audit_event` relation.
struct DecisionPayload {
    /// The address served: `handle` on content events, `target` on refusals.
    handle: Option<String>,
    /// Why, when the decision carries a reason.
    reason: Option<String>,
    /// The payload's estimate of what was returned; the figure a v1 row books.
    tokens_returned: i64,
    /// The payload's estimate of what was avoided; the figure a v1 row books.
    tokens_avoided: i64,
}

impl DecisionPayload {
    /// Read the payload out of an envelope. An absent payload reads as empty,
    /// so a decision that carries none still books its row.
    fn of(env: &serde_json::Value) -> Self {
        let payload = env
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let str_field = |k: &str| {
            payload
                .get(k)
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        };
        let i64_field = |k: &str| {
            payload
                .get(k)
                .and_then(serde_json::Value::as_i64)
                .unwrap_or(0)
        };
        Self {
            // `handle` on content events; `target` on refusals.
            handle: str_field("handle").or_else(|| str_field("target")),
            reason: str_field("reason"),
            tokens_returned: i64_field("estimated_tokens_returned"),
            tokens_avoided: i64_field("estimated_tokens_avoided"),
        }
    }
}

/// Parse a `memd.*` hook envelope and append its neutral audit facts. Schema v1
/// writes the legacy serve-decision relation. Schema v2 additionally writes one
/// invocation-addressed `memd_call_event`; successful decisions also retain the
/// legacy savings row. A malformed envelope defers (no write).
///
/// # Errors
///
/// The store's own error when an append fails; an envelope this function
/// declines to read is never an error.
pub async fn append_memd_audit(
    store: &impl rift_store::CanonicalStore,
    envelope: &[u8],
    now: i64,
) -> Result<()> {
    let Ok(env) = serde_json::from_slice::<serde_json::Value>(envelope) else {
        return Ok(()); // malformed envelope → deferred (no write)
    };
    let (Some(audit_id), Some(verb)) = (
        env.get("idempotency_key")
            .and_then(serde_json::Value::as_str),
        env.get("verb").and_then(serde_json::Value::as_str),
    ) else {
        return Ok(());
    };
    let schema = env
        .get("schema")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(1);
    match schema {
        3 => append_connection_lifecycle(store, &env, audit_id).await,
        4 => append_serve_call(store, &env, audit_id).await,
        1 | 2 => append_serve_decision(store, &env, audit_id, verb, schema, now).await,
        _ => Ok(()),
    }
}

/// Validate and append the schema-v2 invocation fact. `None` means the
/// envelope was partial or internally inconsistent and must be deferred.
async fn append_memd_call(
    store: &impl rift_store::CanonicalStore,
    env: &serde_json::Value,
    audit_id: &str,
    verb: &str,
) -> Result<Option<(i64, i64, i64)>> {
    use rift_store::Value;

    let Some(call) = env.get("call") else {
        return Ok(None);
    };
    let (Some(call_id), Some(thread_id), Some(tool_name), Some(recorded_at)) = (
        call.get("call_id").and_then(serde_json::Value::as_str),
        call.get("thread_id").and_then(serde_json::Value::as_str),
        call.get("tool_name").and_then(serde_json::Value::as_str),
        call.get("recorded_at").and_then(serde_json::Value::as_i64),
    ) else {
        return Ok(None);
    };
    if call_id != audit_id {
        return Ok(None);
    }
    let returned = token_count(call, "tokens_returned");
    let avoided = token_count(call, "tokens_avoided");
    store
        .insert_or_ignore(
            "memd_call_event",
            &[
                "call_id",
                "thread_id",
                "tool_name",
                "terminal_verb",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(call_id),
                Value::text(thread_id),
                Value::text(tool_name),
                Value::text(verb),
                Value::Int(returned),
                Value::Int(avoided),
                Value::Int(recorded_at),
                Value::Int(2),
            ],
        )
        .await?;
    Ok(Some((recorded_at, returned, avoided)))
}

/// Validate and append a schema-v3 client connection lifecycle fact. Client
/// and peer metadata are short identifiers only; arguments/transcripts never
/// enter this relation.
///
/// Compatibility path: the transport that opened and closed these connections
/// is retired, so nothing produces schema-3 envelopes any more. The reader
/// stays functional because `mcp_connection_event` is append-only and its
/// historical rows must keep decoding — the relation name is frozen history,
/// not live vocabulary.
async fn append_connection_lifecycle(
    store: &impl rift_store::CanonicalStore,
    env: &serde_json::Value,
    event_id: &str,
) -> Result<()> {
    use rift_store::Value;

    let Some(connection) = env.get("connection") else {
        return Ok(());
    };
    let (Some(thread_id), Some(client), Some(phase), Some(occurred_at)) = (
        connection
            .get("thread_id")
            .and_then(serde_json::Value::as_str),
        connection.get("client").and_then(serde_json::Value::as_str),
        connection.get("phase").and_then(serde_json::Value::as_str),
        connection
            .get("occurred_at")
            .and_then(serde_json::Value::as_i64),
    ) else {
        return Ok(());
    };
    if !matches!(client, "claude-code" | "codex" | "gemini" | "antigravity")
        || !matches!(phase, "opened" | "closed")
    {
        return Ok(());
    }
    let short = |key: &str| {
        connection
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| value.len() <= 128)
    };
    store
        .insert_or_ignore(
            "mcp_connection_event",
            &[
                "event_id",
                "thread_id",
                "client",
                "peer_name",
                "peer_version",
                "phase",
                "occurred_at",
                "schema_version",
            ],
            &[
                Value::text(event_id),
                Value::text(thread_id),
                Value::text(client),
                short("peer_name").map_or(Value::Null, Value::text),
                short("peer_version").map_or(Value::Null, Value::text),
                Value::text(phase),
                Value::Int(occurred_at),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(())
}

/// Schema v4: a `ServeChannel`-attributed call, written to
/// `serve_call_event` (mirrors `append_memd_call`'s shape and validation —
/// no `thread_id`, a `channel` in its place) and, the first time this
/// channel is ever seen, one `serve_session_event` row. Neither writer
/// touches the historical `memd_call_event`/`mcp_connection_event` relations:
/// the two families are independent, not a migration of one into the other.
async fn append_serve_call(
    store: &impl rift_store::CanonicalStore,
    env: &serde_json::Value,
    audit_id: &str,
) -> Result<()> {
    use rift_store::Value;

    let Some(call) = env.get("serve_call") else {
        return Ok(());
    };
    let (Some(call_id), Some(channel_token), Some(tool_name), Some(recorded_at)) = (
        call.get("call_id").and_then(serde_json::Value::as_str),
        call.get("channel").and_then(serde_json::Value::as_str),
        call.get("tool_name").and_then(serde_json::Value::as_str),
        call.get("recorded_at").and_then(serde_json::Value::as_i64),
    ) else {
        return Ok(());
    };
    if call_id != audit_id {
        return Ok(());
    }
    // The closed vocabulary, checked here rather than left to the CHECK
    // constraint: a row this crate refuses to build is a row that never
    // reaches the append-only guard, and the reader gets a typed channel
    // rather than a string that merely happens to be one of two values.
    let Some(channel) = rift_recall_events::ServeChannel::parse(channel_token) else {
        return Ok(());
    };
    let verb = env
        .get("verb")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let returned = call
        .get("tokens_returned")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
        .max(0);
    let avoided = call
        .get("tokens_avoided")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
        .max(0);
    store
        .insert_or_ignore(
            "serve_call_event",
            &[
                "call_id",
                "channel",
                "tool_name",
                "terminal_verb",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(call_id),
                Value::text(channel.token()),
                Value::text(tool_name),
                Value::text(verb),
                Value::Int(returned),
                Value::Int(avoided),
                Value::Int(recorded_at),
                Value::Int(1),
            ],
        )
        .await?;
    record_first_served(store, channel, recorded_at).await
}

/// A call's token figure: absent reads as none, and a negative one — which no
/// count can be — is clamped to none rather than booked.
fn token_count(call: &serde_json::Value, key: &str) -> i64 {
    call.get(key)
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(0)
        .max(0)
}

/// First-observed-per-channel, EVER: `INSERT OR IGNORE` on the channel PK
/// means every call after the first is silently a no-op here, which is
/// exactly right — `first_served_at` names when the channel was FIRST seen,
/// not when it was last used.
async fn record_first_served(
    store: &impl rift_store::CanonicalStore,
    channel: rift_recall_events::ServeChannel,
    recorded_at: i64,
) -> Result<()> {
    use rift_store::Value;

    store
        .insert_or_ignore(
            "serve_session_event",
            &["channel", "first_served_at", "schema_version"],
            &[
                Value::text(channel.token()),
                Value::Int(recorded_at),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(())
}

/// Schemas v1 and v2: the serve decision's row in the legacy
/// `memd_audit_event` relation, preceded on v2 by the invocation-addressed
/// `memd_call_event` whose figures and clock the decision row then carries.
async fn append_serve_decision(
    store: &impl rift_store::CanonicalStore,
    env: &serde_json::Value,
    audit_id: &str,
    verb: &str,
    schema: u64,
    now: i64,
) -> Result<()> {
    use rift_store::Value;

    let payload = DecisionPayload::of(env);
    // V2 is the per-invocation contract. All fields are neutral and required;
    // a partial envelope is deferred rather than inventing attribution.
    let call_fields = if schema == 2 {
        let Some(fields) = append_memd_call(store, env, audit_id, verb).await? else {
            return Ok(());
        };
        Some(fields)
    } else {
        None
    };

    // Internal errors are calls but not serve decisions and therefore carry no
    // savings fact in the legacy relation.
    if verb == "memd.internal_error" {
        return Ok(());
    }
    let (recorded_at, tokens_returned, tokens_avoided) =
        call_fields.unwrap_or((now, payload.tokens_returned, payload.tokens_avoided));

    store
        .insert_or_ignore(
            "memd_audit_event",
            &[
                "audit_id",
                "verb",
                "handle",
                "reason",
                "tokens_returned",
                "tokens_avoided",
                "recorded_at",
                "schema_version",
            ],
            &[
                Value::text(audit_id),
                Value::text(verb),
                payload.handle.as_deref().map_or(Value::Null, Value::text),
                payload.reason.as_deref().map_or(Value::Null, Value::text),
                Value::Int(tokens_returned),
                Value::Int(tokens_avoided),
                Value::Int(recorded_at),
                Value::Int(1),
            ],
        )
        .await?;

    // Two proper columns beside the composed string, derived from
    // it exactly once — at this, the one place a `memd_audit_event` row is
    // ever written — rather than re-parsed by every future query.
    record_audit_citation_if_any(store, audit_id, payload.handle.as_deref()).await?;
    record_pull_claim_if_any(store, audit_id, env).await
}

/// The companion write for those columns: a row lands ONLY when `handle` is a
/// citation identity (`retrieve_by_citation`'s route, whether it served
/// bytes or refused); every other verb's `handle` is a plain
/// path/summary/log identity and never matches, so the relation stays
/// sparse by construction rather than by a second condition at the call
/// site. Split out of `append_memd_audit` to keep that function's line
/// count under the workspace lint ceiling, not because this needed its own
/// name.
async fn record_audit_citation_if_any(
    store: &impl rift_store::CanonicalStore,
    audit_id: &str,
    handle: Option<&str>,
) -> Result<()> {
    use rift_store::Value;
    let Some((pack_key, ordinal)) = handle.and_then(citation_identity_of) else {
        return Ok(());
    };
    store
        .insert_or_ignore(
            "memd_audit_citation",
            &["audit_id", "pack_key", "ordinal", "schema_version"],
            &[
                Value::text(audit_id),
                Value::text(pack_key),
                Value::Int(i64::from(ordinal)),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(())
}

/// The companion write for a pull's claim: a row lands only when the payload
/// carries one, which only a successful pull that stated revisions does.
///
/// Written after its audit row, not in one transaction with it: the store
/// seam has none. A stop between the two loses this row and nothing else.
/// The key is the audit row's own, read off the same envelope, so a later
/// delivery writes the row once, insert-or-ignore, and never under another
/// serve's key.
async fn record_pull_claim_if_any(
    store: &impl rift_store::CanonicalStore,
    audit_id: &str,
    env: &serde_json::Value,
) -> Result<()> {
    use rift_store::Value;
    let Some((stated, recorded, basis)) = env
        .get("payload")
        .and_then(|payload| payload.get("pull_claim"))
        .and_then(pull_claim_of)
    else {
        return Ok(());
    };
    store
        .insert_or_ignore(
            "memd_audit_pull_claim",
            &[
                "audit_id",
                "stated_count",
                "recorded_count",
                "unrecorded_count",
                "basis",
                "schema_version",
            ],
            &[
                Value::text(audit_id),
                Value::Int(stated),
                Value::Int(recorded),
                Value::Int(stated - recorded),
                Value::text(basis.as_str()),
                Value::Int(1),
            ],
        )
        .await?;
    Ok(())
}

/// The `(stated, recorded, basis)` of a claim member, or `None` for one that
/// cannot describe a pull: no stated revision, a recorded count past the
/// stated one, a count that is not a count, or a rule outside the two a
/// stated hash leads to. Refused here rather than left to the relation's
/// CHECK, so a malformed claim never reaches the append-only guard.
fn pull_claim_of(claim: &serde_json::Value) -> Option<(i64, i64, PullClaimBasis)> {
    let count = |key: &str| {
        claim
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .map(i64::from)
    };
    let (Some(stated), Some(recorded)) = (count("stated"), count("recorded")) else {
        return None;
    };
    let basis = match claim.get("basis").and_then(serde_json::Value::as_str)? {
        "stated_recorded" => PullClaimBasis::StatedRecorded,
        "unverified" => PullClaimBasis::Unverified,
        _ => return None,
    };
    if stated == 0 || stated < recorded {
        return None;
    }
    Some((stated, recorded, basis))
}

/// The `(pack_key, ordinal)` a `memd_audit_event.handle`/`target` string
/// names, if it is a citation identity — `pack:<key>/evidence:<n>`,
/// optionally followed by `=file:<path>@sha256:<hash>` once the ordinal
/// resolved (`rift_recall_serve`'s `citation_audit_name`, the ONE function
/// that composes this string).
///
/// Reuses `rift_recall_pack::CitationTarget::parse` — the same
/// strict, no-lenient-arm grammar the serve layer itself uses to accept a
/// citation — rather than re-deriving hex/digit validation, which would be a
/// second grammar free to drift from the first. Splitting on the first `=` recovers exactly
/// `CitationTarget`'s own shape, because its `Display` never emits one (an
/// `@sha256:` restatement, when present, follows the ordinal directly with
/// no `=` in between).
fn citation_identity_of(handle: &str) -> Option<(String, u32)> {
    let head = handle.split_once('=').map_or(handle, |(h, _)| h);
    let citation = rift_recall_pack::CitationTarget::parse(head)?;
    Some((citation.pack_key, citation.ordinal))
}

/// Sum the pull claims booked at or after `since_ms` (every claim when
/// `None`), on the audit row's clock, the clock the savings numerator is cut
/// on.
///
/// The reader sits beside the one writer so the relation's shape is known in
/// one module.
///
/// # Errors
///
/// The store's own error, including on a projection that predates the
/// relation; the caller says the figure is not recorded there.
pub async fn pull_claim_totals(
    store: &impl rift_store::CanonicalStore,
    since_ms: Option<i64>,
) -> Result<PullClaimTotals> {
    use rift_store::Value;
    let rows = store
        .query(
            "SELECT CAST(COALESCE(SUM(c.stated_count), 0) AS INTEGER), \
                    CAST(COALESCE(SUM(c.recorded_count), 0) AS INTEGER), \
                    CAST(COALESCE(SUM(c.unrecorded_count), 0) AS INTEGER) \
             FROM memd_audit_pull_claim c \
             JOIN memd_audit_event e ON e.audit_id = c.audit_id \
             WHERE e.recorded_at >= ?1",
            &[Value::Int(since_ms.unwrap_or(i64::MIN))],
        )
        .await?;
    let count = |idx: usize| {
        rows.first()
            .and_then(|row| row.get_i64(idx).ok())
            .map_or(0, |n| u64::try_from(n).unwrap_or(0))
    };
    Ok(PullClaimTotals {
        stated: count(0),
        recorded: count(1),
        unrecorded: count(2),
    })
}

#[cfg(test)]
mod tests {
    use rift_recall_events::{MemdEvent, ServeChannel};
    use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore};

    use super::*;

    /// A migrated, empty recall store and the runtime its async seam runs on.
    fn recall_store() -> (SqliteStore, tokio::runtime::Runtime) {
        let store = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&store, &recall_migrations()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        (store, rt)
    }

    /// Append one envelope `times` times, as a redelivered job would.
    fn append_repeatedly(
        store: &SqliteStore,
        rt: &tokio::runtime::Runtime,
        envelope: &str,
        times: usize,
    ) {
        for _ in 0..times {
            rt.block_on(append_memd_audit(
                store,
                envelope.as_bytes(),
                1_700_000_000_000,
            ))
            .unwrap();
        }
    }

    fn row_count(store: &SqliteStore, rt: &tokio::runtime::Runtime, relation: &str) -> i64 {
        let rows = rt
            .block_on(store.query(&format!("SELECT COUNT(*) FROM {relation}"), &[]))
            .unwrap();
        rows[0].get_i64(0).unwrap()
    }

    fn read_intercepted(handle: &str) -> MemdEvent {
        MemdEvent::ReadIntercepted {
            handle: handle.to_owned(),
            reason: "already_held".to_owned(),
            estimated_tokens_returned: 12,
            estimated_tokens_avoided: 340,
        }
    }

    #[test]
    fn a_schema_one_envelope_delivered_twice_is_one_audit_row() {
        let (store, rt) = recall_store();
        let envelope = read_intercepted("file:src/a.rs@sha256:ab").to_hook_json_keyed("call-1");
        append_repeatedly(&store, &rt, &envelope, 2);
        assert_eq!(row_count(&store, &rt, "memd_audit_event"), 1);
        let rows = rt
            .block_on(store.query(
                "SELECT tokens_returned, tokens_avoided, recorded_at FROM memd_audit_event",
                &[],
            ))
            .unwrap();
        assert_eq!(rows[0].get_i64(0).unwrap(), 12);
        assert_eq!(rows[0].get_i64(1).unwrap(), 340);
        assert_eq!(rows[0].get_i64(2).unwrap(), 1_700_000_000_000);
    }

    #[test]
    fn a_schema_four_envelope_delivered_twice_is_one_call_one_session_and_one_audit_row() {
        let (store, rt) = recall_store();
        let event = read_intercepted("file:src/a.rs@sha256:ab");
        let audited = event.to_hook_json_keyed("call-4");
        let called = event.to_serve_call_json(
            ServeChannel::HookInject,
            "call-4",
            "claude-code:read-gate",
            rift_core::Timestamp::from_millis(1_600_000_000_000),
        );
        append_repeatedly(&store, &rt, &audited, 2);
        append_repeatedly(&store, &rt, &called, 2);
        assert_eq!(row_count(&store, &rt, "serve_call_event"), 1);
        assert_eq!(row_count(&store, &rt, "serve_session_event"), 1);
        assert_eq!(row_count(&store, &rt, "memd_audit_event"), 1);
        let rows = rt
            .block_on(store.query(
                "SELECT channel, tokens_returned, tokens_avoided, recorded_at \
                 FROM serve_call_event",
                &[],
            ))
            .unwrap();
        assert_eq!(rows[0].get_text(0).unwrap(), "hook_inject");
        assert_eq!(rows[0].get_i64(1).unwrap(), 12);
        assert_eq!(rows[0].get_i64(2).unwrap(), 340);
        assert_eq!(rows[0].get_i64(3).unwrap(), 1_600_000_000_000);
    }

    #[test]
    fn a_citation_handle_books_one_citation_row_however_often_it_arrives() {
        let (store, rt) = recall_store();
        let key = "a".repeat(64);
        let handle = format!("pack:{key}/evidence:3");
        let envelope = read_intercepted(&handle).to_hook_json_keyed("call-c");
        append_repeatedly(&store, &rt, &envelope, 2);
        let rows = rt
            .block_on(store.query("SELECT pack_key, ordinal FROM memd_audit_citation", &[]))
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].get_text(0).unwrap(), key);
        assert_eq!(rows[0].get_i64(1).unwrap(), 3);
    }

    #[test]
    fn a_malformed_envelope_writes_nothing_and_is_not_an_error() {
        let (store, rt) = recall_store();
        for envelope in ["not json", "{}", r#"{"schema":4,"verb":"memd.x"}"#] {
            rt.block_on(append_memd_audit(&store, envelope.as_bytes(), 1))
                .unwrap();
        }
        for relation in [
            "memd_audit_event",
            "serve_call_event",
            "serve_session_event",
            "memd_audit_citation",
        ] {
            assert_eq!(row_count(&store, &rt, relation), 0, "{relation}");
        }
    }

    #[test]
    fn citation_identity_of_parses_both_shapes_and_refuses_everything_else() {
        let key = "a".repeat(64);
        let sha = "b".repeat(64);
        assert_eq!(
            citation_identity_of(&format!("pack:{key}/evidence:3")),
            Some((key.clone(), 3))
        );
        assert_eq!(
            citation_identity_of(&format!("pack:{key}/evidence:3=file:src/a.rs@sha256:{sha}")),
            Some((key.clone(), 3)),
            "the resolved-address suffix must not stop the head from parsing"
        );
        assert_eq!(
            citation_identity_of(&format!("pack:{key}/evidence:3@sha256:{sha}")),
            Some((key, 3)),
            "a caller-restated address (no `=`) is still the same head"
        );
        for not_a_citation in [
            "file:src/a.rs@sha256:deadbeef",
            "summary:some/path",
            "log:tool-use-1",
            "",
        ] {
            assert_eq!(
                citation_identity_of(not_a_citation),
                None,
                "{not_a_citation} is not a citation"
            );
        }
    }
}

#[cfg(test)]
mod pull_claim_tests {
    use rift_recall_events::{MemdEvent, PullClaim, PullClaimBasis};
    use rift_store::{apply_migrations, recall_migrations, CanonicalStore, SqliteStore, Store};

    use super::*;

    fn recall_store() -> (SqliteStore, tokio::runtime::Runtime) {
        let store = SqliteStore::open_in_memory().unwrap();
        apply_migrations(&store, &recall_migrations()).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        (store, rt)
    }

    fn pull(stated: u32, recorded: u32, basis: PullClaimBasis) -> MemdEvent {
        MemdEvent::ManifestServed {
            estimated_tokens_returned: 7,
            estimated_tokens_avoided: 40,
            pull_claim: None,
        }
        .with_pull_claim(PullClaim::new(stated, recorded, basis))
    }

    fn append(store: &SqliteStore, rt: &tokio::runtime::Runtime, envelope: &str, at: i64) {
        rt.block_on(append_memd_audit(store, envelope.as_bytes(), at))
            .unwrap();
    }

    fn claims(store: &SqliteStore, rt: &tokio::runtime::Runtime) -> Vec<(String, i64, i64, i64)> {
        let rows = rt
            .block_on(store.query(
                "SELECT audit_id, stated_count, recorded_count, unrecorded_count \
                 FROM memd_audit_pull_claim ORDER BY audit_id",
                &[],
            ))
            .unwrap();
        rows.iter()
            .map(|r| {
                (
                    r.get_text(0).unwrap(),
                    r.get_i64(1).unwrap(),
                    r.get_i64(2).unwrap(),
                    r.get_i64(3).unwrap(),
                )
            })
            .collect()
    }

    fn key(envelope: &str) -> String {
        let doc: serde_json::Value = serde_json::from_str(envelope).unwrap();
        doc["idempotency_key"].as_str().unwrap().to_owned()
    }

    /// Make every claim write fail from here on, as a stop between the audit
    /// row and its claim would: a connection-local trigger, so the guard
    /// triggers the migration installed stay as they are.
    fn fail_claim_writes(store: &SqliteStore, rt: &tokio::runtime::Runtime) {
        batch(
            store,
            rt,
            "CREATE TEMP TRIGGER claim_write_fails BEFORE INSERT ON main.memd_audit_pull_claim \
             BEGIN SELECT RAISE(ABORT, 'stopped between the two writes'); END;",
        );
    }

    fn restore_claim_writes(store: &SqliteStore, rt: &tokio::runtime::Runtime) {
        batch(store, rt, "DROP TRIGGER temp.claim_write_fails;");
    }

    fn batch(store: &SqliteStore, rt: &tokio::runtime::Runtime, sql: &str) {
        rt.block_on(async { store.conn().await?.execute_batch(sql).await })
            .unwrap();
    }

    #[test]
    fn a_pull_that_stated_revisions_books_its_counts_beside_its_audit_row() {
        let (store, rt) = recall_store();
        let envelope = pull(3, 2, PullClaimBasis::StatedRecorded).to_hook_json_keyed("call-1");
        append(&store, &rt, &envelope, 10);
        assert_eq!(claims(&store, &rt), vec![(key(&envelope), 3, 2, 1)]);
        let basis = rt
            .block_on(store.query("SELECT basis FROM memd_audit_pull_claim", &[]))
            .unwrap();
        assert_eq!(basis[0].get_text(0).unwrap(), "stated_recorded");
        let audit = rt
            .block_on(store.query("SELECT audit_id FROM memd_audit_event", &[]))
            .unwrap();
        assert_eq!(audit[0].get_text(0).unwrap(), key(&envelope));
    }

    #[test]
    fn a_refusal_a_pull_with_no_claim_and_a_hook_decision_book_no_claim() {
        let (store, rt) = recall_store();
        let refused = MemdEvent::Refused {
            target: "src/a.rs".to_owned(),
            reason: "path_not_found".to_owned(),
            estimated_tokens_returned: 5,
        };
        let unclaimed = MemdEvent::ManifestServed {
            estimated_tokens_returned: 5,
            estimated_tokens_avoided: 0,
            pull_claim: None,
        };
        let intercepted = MemdEvent::ReadIntercepted {
            handle: "file:src/a.rs@sha256:ab".to_owned(),
            reason: "already_held".to_owned(),
            estimated_tokens_returned: 1,
            estimated_tokens_avoided: 9,
        };
        for event in [refused, unclaimed, intercepted] {
            append(&store, &rt, &event.to_hook_json_keyed("k"), 10);
        }
        assert_eq!(rt.block_on(store.count("memd_audit_event")).unwrap(), 3);
        assert!(claims(&store, &rt).is_empty());
    }

    /// A stop between the audit row and its claim loses the claim and nothing
    /// else; a later delivery of the same envelope writes it once, under the
    /// audit row's own key, and never under another serve's.
    #[test]
    fn a_crash_between_the_two_writes_can_only_lose_the_claim() {
        let (store, rt) = recall_store();
        let first = pull(4, 1, PullClaimBasis::StatedRecorded).to_hook_json_keyed("call-a");
        let second = pull(2, 2, PullClaimBasis::Unverified).to_hook_json_keyed("call-b");
        fail_claim_writes(&store, &rt);
        let stopped = rt.block_on(append_memd_audit(&store, first.as_bytes(), 10));
        assert!(stopped.is_err());
        assert_eq!(rt.block_on(store.count("memd_audit_event")).unwrap(), 1);
        assert!(claims(&store, &rt).is_empty());
        restore_claim_writes(&store, &rt);
        // Another serve lands before the stopped one is delivered again: its
        // claim is its own, and the lost one is never pinned to it.
        append(&store, &rt, &second, 12);
        assert_eq!(claims(&store, &rt), vec![(key(&second), 2, 2, 0)]);
        for _ in 0..3 {
            append(&store, &rt, &first, 11);
        }
        let mut want = vec![(key(&first), 4, 1, 3), (key(&second), 2, 2, 0)];
        want.sort();
        assert_eq!(claims(&store, &rt), want);
        assert_eq!(rt.block_on(store.count("memd_audit_event")).unwrap(), 2);
        let orphans = rt
            .block_on(store.query(
                "SELECT COUNT(*) FROM memd_audit_pull_claim c \
                 WHERE NOT EXISTS (SELECT 1 FROM memd_audit_event e WHERE e.audit_id = c.audit_id)",
                &[],
            ))
            .unwrap();
        assert_eq!(orphans[0].get_i64(0).unwrap(), 0);
    }

    #[test]
    fn a_claim_that_does_not_add_up_or_names_no_known_rule_books_nothing() {
        let (store, rt) = recall_store();
        let good = pull(3, 1, PullClaimBasis::StatedRecorded).to_hook_json_keyed("k");
        let mut doc: serde_json::Value = serde_json::from_str(&good).unwrap();
        for (n, (member, value)) in [
            ("recorded", serde_json::json!(5)),
            ("stated", serde_json::json!(-1)),
            ("basis", serde_json::json!("none_stated")),
            ("stated", serde_json::json!(0)),
        ]
        .into_iter()
        .enumerate()
        {
            let mut bad = doc.clone();
            bad["payload"]["pull_claim"][member] = value;
            bad["idempotency_key"] = serde_json::json!(format!("bad-{n}"));
            append(&store, &rt, &bad.to_string(), 10);
        }
        assert!(claims(&store, &rt).is_empty());
        doc["idempotency_key"] = serde_json::json!("good");
        append(&store, &rt, &doc.to_string(), 10);
        assert_eq!(claims(&store, &rt).len(), 1);
    }

    #[test]
    fn the_totals_sum_every_claim_in_the_window_and_split_the_stated_count() {
        let (store, rt) = recall_store();
        let early = pull(5, 5, PullClaimBasis::StatedRecorded).to_hook_json_keyed("early");
        let late_a = pull(3, 1, PullClaimBasis::StatedRecorded).to_hook_json_keyed("a");
        let late_b = pull(2, 0, PullClaimBasis::Unverified).to_hook_json_keyed("b");
        append(&store, &rt, &early, 100);
        append(&store, &rt, &late_a, 200);
        append(&store, &rt, &late_b, 300);
        let whole = rt.block_on(pull_claim_totals(&store, None)).unwrap();
        assert_eq!((whole.stated, whole.recorded, whole.unrecorded), (10, 6, 4));
        let window = rt.block_on(pull_claim_totals(&store, Some(150))).unwrap();
        assert_eq!(
            (window.stated, window.recorded, window.unrecorded),
            (5, 1, 4)
        );
    }
}
