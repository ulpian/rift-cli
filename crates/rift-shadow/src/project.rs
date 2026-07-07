//! The normalized projector (P1 S4 · P2 S3): canonical events → the decomposed
//! session/turn/model-call/tool-call model, plus token usage and cost facts.
//!
//! This is the **single** projection function both event sources run through:
//! the live agent-hook path calls it as events arrive, and the artifact bridge
//! calls it while re-deriving events from checkpoint shards (ADR-0002). Row ids
//! come exclusively from [`crate::ids`], so the two sources' row sets union to
//! a no-op. Every write is `INSERT OR IGNORE` on a content-addressed key.
//!
//! Payload-derived facts (model, tool name, child-session id) are extracted
//! defensively: an absent field projects an empty/synthetic value, never an
//! error — projection must not be able to fail an agent's hook.

use rift_core::{CanonicalEvent, EventKind, Result};
use rift_shadow_cost::{call_cost_micros, ClassPrices, CounterMethod, TokenUsage};
use rift_store::{
    effective_prices, record_cost_fact, record_model_call, record_session, record_session_event,
    record_subagent_session, record_token_usage, record_turn, CanonicalStore, CostFact,
    ModelCallRow, SessionEventRow, SessionRow, SubagentSessionRow, TokenUsageRow, ToolCallRow,
    TurnRow,
};

use crate::ids;

/// Payload-derived facts for one hook/artifact record, extracted once and shared
/// by every event the record fanned out to.
#[derive(Debug, Clone, Default)]
pub struct PayloadFacts {
    /// The model named by the payload (`model`), if any.
    pub model: Option<String>,
    /// The tool named by the payload (`tool_name`), if any.
    pub tool_name: Option<String>,
    /// A child-session id for subagent events, if the payload names one.
    pub child_session: Option<String>,
    /// The path a **read-shaped** tool touched (`tool_input.file_path`), raw
    /// from the payload — the capture-side read fact the recall plane's whole
    /// delta model rests on (M5-06).
    pub read_path: Option<String>,
    /// The resolved read-time content identity for `read_path` — repo-relative
    /// path + hashes, resolved by the caller's injected worktree hasher, never
    /// by this crate (no filesystem here; tests stay deterministic).
    pub read_content: Option<ReadContent>,
}

/// The read-time content identity of one read-shaped tool call: the
/// repo-relative path plus the current bytes' git blob oid and SHA-256 —
/// exactly the triple `tool_calls.jsonl` (and so the recall ledger) carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadContent {
    /// Repo-relative path (the caller relativized any absolute payload path).
    pub rel_path: String,
    /// Git blob object id of the bytes at read time.
    pub blob_oid: String,
    /// Plain SHA-256 (hex) of the bytes at read time.
    pub content_sha256: String,
}

/// Extract [`PayloadFacts`] from a native payload. Total: unknown shapes yield
/// defaults. Only scalar identity fields are read — never content bytes.
#[must_use]
pub fn payload_facts(payload: &serde_json::Value) -> PayloadFacts {
    let str_at = |keys: &[&str]| -> Option<String> {
        keys.iter()
            .find_map(|k| payload.get(k).and_then(|v| v.as_str()))
            .map(str::to_owned)
    };
    let tool_name = str_at(&["tool_name"]);
    // A read-shaped call names its file in `tool_input.file_path` (a scalar
    // identity field — the CONTENT of the read never leaves the agent here).
    let read_path = tool_name
        .as_deref()
        .filter(|n| n.eq_ignore_ascii_case("read"))
        .and_then(|_| {
            payload
                .get("tool_input")
                .and_then(|t| t.get("file_path"))
                .and_then(|v| v.as_str())
                .map(str::to_owned)
        });
    PayloadFacts {
        model: str_at(&["model"]),
        tool_name,
        child_session: str_at(&[
            "subagent_session_id",
            "child_session_id",
            "agent_session_id",
        ]),
        read_path,
        read_content: None,
    }
}

/// The projection context: which agent and repo these events belong to.
#[derive(Debug, Clone, Copy)]
pub struct ProjectionCtx<'a> {
    /// The agent token (`claude-code` | …).
    pub agent: &'a str,
    /// The repo id the session runs in.
    pub repo_id: &'a str,
}

/// Project one canonical event into the normalized relations. Idempotent; safe
/// to call from both event sources in any order.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on an insert failure (callers at the hook
/// boundary swallow it — capture never blocks the agent).
#[allow(clippy::too_many_lines)] // one linear kind→relation mapping; a match arm per kind
pub async fn project_event(
    store: &impl CanonicalStore,
    ev: &CanonicalEvent,
    facts: &PayloadFacts,
    ctx: ProjectionCtx<'_>,
) -> Result<()> {
    let session_row = ids::session_row_id(ctx.agent, &ev.session);

    // Every event implies its session and turn exist. First writer wins on
    // `started_at`/`model_primary`; a later SessionStart never rewrites them —
    // corrections are the bridge's job, as superseding rows.
    record_session(
        store,
        &SessionRow {
            session_id: session_row.clone(),
            agent: ctx.agent.to_owned(),
            model_primary: facts.model.clone().unwrap_or_default(),
            repo_id: ctx.repo_id.to_owned(),
            started_at: ev.occurred_at_ms,
            schema_version: i64::from(ev.schema_version),
        },
    )
    .await?;
    record_turn(
        store,
        &TurnRow {
            turn_id: ids::turn_row_id(&session_row, ev.turn_ordinal),
            session_id: session_row.clone(),
            ordinal: i64::from(ev.turn_ordinal),
            started_at: ev.occurred_at_ms,
            schema_version: i64::from(ev.schema_version),
        },
    )
    .await?;

    match ev.kind {
        EventKind::SessionStart | EventKind::SessionEnd | EventKind::SessionCompaction => {
            let kind = match ev.kind {
                EventKind::SessionStart => "start",
                EventKind::SessionEnd => "end",
                _ => "compaction",
            };
            record_session_event(
                store,
                &SessionEventRow {
                    event_id: ids::session_event_id(
                        &session_row,
                        kind,
                        ev.turn_ordinal,
                        ev.event_ordinal,
                    ),
                    session_id: session_row,
                    kind: kind.to_owned(),
                    occurred_at: ev.occurred_at_ms,
                    schema_version: i64::from(ev.schema_version),
                },
            )
            .await?;
        }
        EventKind::PromptResponse => {
            record_model_call(
                store,
                &ModelCallRow {
                    call_id: ids::model_call_id(&session_row, ev.turn_ordinal, ev.event_ordinal),
                    turn_id: ids::turn_row_id(&session_row, ev.turn_ordinal),
                    model: facts.model.clone().unwrap_or_default(),
                    started_at: ev.occurred_at_ms,
                    schema_version: i64::from(ev.schema_version),
                },
            )
            .await?;
        }
        EventKind::ToolCall => {
            rift_store::record_tool_call(
                store,
                &ToolCallRow {
                    tool_call_id: ids::tool_call_row_id(
                        &session_row,
                        ev.turn_ordinal,
                        ev.event_ordinal,
                    ),
                    turn_id: ids::turn_row_id(&session_row, ev.turn_ordinal),
                    tool_name: facts.tool_name.clone().unwrap_or_default(),
                    started_at: ev.occurred_at_ms,
                    schema_version: i64::from(ev.schema_version),
                },
            )
            .await?;
        }
        EventKind::SubagentStart => {
            let child = facts.child_session.clone().unwrap_or_else(|| {
                ids::subagent_child_id(&session_row, ev.turn_ordinal, ev.event_ordinal)
            });
            record_subagent_session(
                store,
                &SubagentSessionRow {
                    child_session_id: child,
                    parent_turn_id: ids::turn_row_id(&session_row, ev.turn_ordinal),
                    schema_version: i64::from(ev.schema_version),
                },
            )
            .await?;
        }
        // A completed read-shaped tool call (PostToolUse fan-out) is the one
        // durable "the agent read path P at content H" fact the recall plane
        // ingests — without it memory is empty by construction (M5-06). The
        // read anchors to its own idempotent tool_call row; the revision id is
        // content-addressed like the commit-capture writer's.
        EventKind::ToolResult => {
            if let Some(rc) = &facts.read_content {
                // The read's OWN row id is content-addressed over the fact
                // (session, turn, path, read-time hash) — never the shared
                // (turn, event_ordinal) slot, whose ordinals restart per hook
                // and collide across tools within a turn.
                let call_id = crate::sha256_hex(&[
                    b"rift.read-call.v1\0",
                    session_row.as_bytes(),
                    b"\0",
                    ev.turn_ordinal.to_string().as_bytes(),
                    b"\0",
                    rc.rel_path.as_bytes(),
                    b"\0",
                    rc.content_sha256.as_bytes(),
                ]);
                rift_store::record_tool_call(
                    store,
                    &ToolCallRow {
                        tool_call_id: call_id.clone(),
                        turn_id: ids::turn_row_id(&session_row, ev.turn_ordinal),
                        tool_name: facts.tool_name.clone().unwrap_or_default(),
                        started_at: ev.occurred_at_ms,
                        schema_version: i64::from(ev.schema_version),
                    },
                )
                .await?;
                let revision_id =
                    rift_shadow_attribution::file_revision_id(&rc.rel_path, rc.blob_oid.as_bytes())
                        .sha256()
                        .to_hex();
                rift_store::record_file_revision(
                    store,
                    &rift_store::FileRevisionRow {
                        file_revision_id: revision_id.clone(),
                        path: rc.rel_path.clone(),
                        blob_oid: rc.blob_oid.clone(),
                        content_sha256: rc.content_sha256.clone(),
                        schema_version: i64::from(ev.schema_version),
                    },
                )
                .await?;
                rift_store::record_tool_reads_file(store, &call_id, &revision_id).await?;
            }
        }
        // Turn boundaries drive the state machine and the boundary snapshotter;
        // prompt-submit / file-edit facts are event-grain only at this layer
        // (file links land with revisions at boundaries/commit).
        EventKind::TurnStart
        | EventKind::TurnEnd
        | EventKind::PromptSubmit
        | EventKind::FileEdit
        | EventKind::SubagentEnd => {}
    }
    Ok(())
}

/// Project a provider-reported usage figure for one usage-bearing event into a
/// `model_call` + its 1:1 `token_usage`, and — when the seeded, versioned price
/// catalog covers the model — a bitemporal `cost_fact` computed from it.
///
/// The adapter's optional classes map onto the four DDL classes verbatim
/// (`input→uncached_input`, `cache_read→cached_input`,
/// `cache_write→cache_creation`); absent classes persist as 0 with the
/// provenance still `provider_reported` — a cache split is never fabricated
/// (P6 S1). Models without a full price on file produce **no** cost fact.
///
/// # Errors
/// [`rift_core::RiftError::Storage`] on an insert/query failure.
pub async fn project_usage(
    store: &impl CanonicalStore,
    ev: &CanonicalEvent,
    facts: &PayloadFacts,
    ctx: ProjectionCtx<'_>,
    adapter_usage: &rift_shadow_hooks::AdapterTokenUsage,
    now_ms: i64,
) -> Result<()> {
    let session_row = ids::session_row_id(ctx.agent, &ev.session);
    let call_id = ids::model_call_id(&session_row, ev.turn_ordinal, ev.event_ordinal);
    let model = facts.model.clone().unwrap_or_default();

    // The usage-bearing event anchors a model call (hooks see usage at result/
    // turn grain; each usage report is one call-grain fact).
    record_model_call(
        store,
        &ModelCallRow {
            call_id: call_id.clone(),
            turn_id: ids::turn_row_id(&session_row, ev.turn_ordinal),
            model: model.clone(),
            started_at: ev.occurred_at_ms,
            schema_version: i64::from(ev.schema_version),
        },
    )
    .await?;

    let usage = TokenUsage {
        uncached_input: adapter_usage.input.unwrap_or(0),
        cached_input: adapter_usage.cache_read.unwrap_or(0),
        cache_creation: adapter_usage.cache_write.unwrap_or(0),
        output: adapter_usage.output.unwrap_or(0),
    };
    let clamp = |v: u64| i64::try_from(v).unwrap_or(i64::MAX);
    record_token_usage(
        store,
        &TokenUsageRow {
            call_id: call_id.clone(),
            uncached_input: clamp(usage.uncached_input),
            cached_input: clamp(usage.cached_input),
            cache_creation: clamp(usage.cache_creation),
            output: clamp(usage.output),
            counter_method: CounterMethod::ProviderReported.as_str().to_owned(),
            schema_version: i64::from(ev.schema_version),
        },
    )
    .await?;

    // Cost, only against a complete price row set (honest absence otherwise).
    // Cost facts are strict-append (bitemporal): an identical recomputation
    // records nothing; a changed belief appends a superseding fact.
    if let Some(p) = effective_prices(store, &model, ev.occurred_at_ms, now_ms).await? {
        let amount = call_cost_micros(
            &usage,
            &ClassPrices {
                uncached_input_per_mtok: p.uncached_input,
                cached_input_per_mtok: p.cached_input,
                cache_write_per_mtok: p.cache_creation,
                output_per_mtok: p.output,
            },
            0,
        );
        if rift_store::current_cost_micros(store, &call_id).await? != Some(amount) {
            record_cost_fact(
                store,
                &CostFact {
                    cost_fact_id: crate::sha256_hex(&[
                        b"cost_fact",
                        b"\0",
                        call_id.as_bytes(),
                        b"\0",
                        amount.to_string().as_bytes(),
                        b"\0",
                        now_ms.to_string().as_bytes(),
                    ]),
                    call_id,
                    amount_micros: amount,
                    valid_from: ev.occurred_at_ms,
                    valid_to: None,
                    recorded_at: now_ms,
                    supersedes_id: None,
                    schema_version: 1,
                },
            )
            .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_core::Trust;
    use rift_store::SqliteStore;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    fn ev(kind: EventKind, turn: u32, ordinal: u32) -> CanonicalEvent {
        CanonicalEvent {
            schema_version: 1,
            session: "native-1".into(),
            turn_ordinal: turn,
            event_ordinal: ordinal,
            kind,
            trust: Trust::TranscriptUntrusted,
            occurred_at_ms: 42,
            payload_sha256: rift_core::Sha256Id::from_bytes([7u8; 32]),
        }
    }

    const CTX: ProjectionCtx<'static> = ProjectionCtx {
        agent: "claude-code",
        repo_id: "repo-1",
    };

    #[tokio::test]
    async fn projection_decomposes_and_is_idempotent() {
        let s = store();
        let facts = payload_facts(&serde_json::json!({
            "model": "claude-sonnet-4-5", "tool_name": "Bash"
        }));
        for _ in 0..2 {
            project_event(&s, &ev(EventKind::SessionStart, 0, 0), &facts, CTX)
                .await
                .unwrap();
            project_event(&s, &ev(EventKind::ToolCall, 1, 0), &facts, CTX)
                .await
                .unwrap();
            project_event(&s, &ev(EventKind::PromptResponse, 1, 1), &facts, CTX)
                .await
                .unwrap();
            project_event(&s, &ev(EventKind::SubagentStart, 1, 2), &facts, CTX)
                .await
                .unwrap();
            project_event(&s, &ev(EventKind::SessionEnd, 2, 0), &facts, CTX)
                .await
                .unwrap();
        }
        assert_eq!(s.count("session").await.unwrap(), 1);
        assert_eq!(s.count("session_event").await.unwrap(), 2); // start + end
        assert_eq!(s.count("turn").await.unwrap(), 3); // ordinals 0,1,2
        assert_eq!(s.count("tool_call").await.unwrap(), 1);
        assert_eq!(s.count("model_call").await.unwrap(), 1);
        assert_eq!(s.count("subagent_session").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn usage_writes_call_usage_and_seeded_cost() {
        // Use the migration-opened store so the M1 price seed is present.
        let dir = tempfile::tempdir().unwrap();
        let s = rift_store::open_capture_projection(dir.path().join("projection.db")).unwrap();
        let facts = payload_facts(&serde_json::json!({ "model": "claude-sonnet-4-5" }));
        let usage = rift_shadow_hooks::AdapterTokenUsage {
            input: Some(1_000_000),
            output: Some(1_000_000),
            cache_read: Some(1_000_000),
            cache_write: None,
        };
        // `as_of` must be ≥ the seed's transaction time (2026-07-01) — the
        // bitemporal filter refuses to apply a price we hadn't yet recorded.
        const NOW: i64 = 1_790_000_000_000;
        let e = ev(EventKind::TurnEnd, 1, 3);
        project_usage(&s, &e, &facts, CTX, &usage, NOW)
            .await
            .unwrap();
        // idempotent replay
        project_usage(&s, &e, &facts, CTX, &usage, NOW + 1)
            .await
            .unwrap();

        assert_eq!(s.count("model_call").await.unwrap(), 1);
        assert_eq!(s.count("token_usage").await.unwrap(), 1);
        // seeded sonnet-4-5: 1M uncached ($3) + 1M cached ($0.30) + 1M output ($15)
        // = $18.30 = 18_300_000 micro-USD.
        let session_row = ids::session_row_id("claude-code", "native-1");
        let call_id = ids::model_call_id(&session_row, 1, 3);
        assert_eq!(
            rift_store::current_cost_micros(&s, &call_id).await.unwrap(),
            Some(18_300_000)
        );
    }

    #[tokio::test]
    async fn unpriced_model_writes_usage_but_no_cost_fact() {
        let dir = tempfile::tempdir().unwrap();
        let s = rift_store::open_capture_projection(dir.path().join("projection.db")).unwrap();
        let facts = payload_facts(&serde_json::json!({ "model": "mystery-model-9" }));
        let usage = rift_shadow_hooks::AdapterTokenUsage {
            input: Some(10),
            output: Some(10),
            cache_read: None,
            cache_write: None,
        };
        project_usage(&s, &ev(EventKind::TurnEnd, 0, 0), &facts, CTX, &usage, 5)
            .await
            .unwrap();
        assert_eq!(s.count("token_usage").await.unwrap(), 1);
        assert_eq!(
            s.count("cost_fact").await.unwrap(),
            0,
            "no price on file → no fabricated cost"
        );
    }
}
