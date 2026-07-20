//! P1 S4 — the artifact bridge: the **authoritative** canonical-event source
//! (ADR-0002).
//!
//! Walks the redacted checkpoint branch through the [`SubstrateReader`] seam,
//! **refuses any shard without a valid redaction report**, re-derives canonical
//! events from `full.jsonl`, projects them through exactly the same writers and
//! id derivations the live hook path uses (`crate::project` + `crate::ids`),
//! and advances the append-only ingest watermark. Because every row id is
//! content-addressed, ingest is `INSERT … OR IGNORE`: re-running the bridge —
//! after a crash, over a hook-fed projection, or from scratch — is a set-union
//! no-op. The bridge never sees the raw shadow branch: the reader it is handed
//! opens only the checkpoint ref, and a non-local reader is refused outright.

use rift_core::{CanonicalEvent, EventKind, Result, RiftError, Sha256Id};
use rift_recall_substrate::parse;
use rift_recall_substrate::{CheckpointShard, SubstrateReader};
use rift_recall_types::checkpoint::{CheckpointMetadata, FullEventRecord};
use rift_shadow_hooks::{idempotency_key, AdapterRegistry, AgentId};
use rift_store::{
    record_captured_event, record_checkpoint_if_new, record_ingest_watermark,
    record_session_in_checkpoint, CanonicalStore, CapturedEventRow, CheckpointRecord,
    IngestWatermarkRow, SessionInCheckpointRow,
};

use crate::ids;
use crate::project::{
    payload_facts, project_event, project_main_context, project_usage, ProjectionCtx,
};

/// The checkpoint branch the bridge ingests.
pub const CHECKPOINT_BRANCH: &str = "rift/checkpoints/v1";

/// Why a shard was refused for projection (report + audit; never a panic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefusedShard {
    /// The shard's checkpoint id.
    pub checkpoint_id: String,
    /// The refusal reason (category only, never payload bytes).
    pub reason: String,
}

/// What one bridge run did — the crash-resume + idempotency evidence.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeReport {
    /// The branch walked.
    pub branch: String,
    /// The watermark token recorded (the highest shard id ingested), if any.
    pub tip: Option<String>,
    /// Shards enumerated on the branch.
    pub shards_seen: usize,
    /// Shards projected (valid redaction report).
    pub shards_projected: usize,
    /// Shards refused, with reasons.
    pub refused: Vec<RefusedShard>,
    /// Canonical events re-derived from artifacts.
    pub events_derived: usize,
    /// `captured_event` rows actually inserted (0 on a re-run or over a
    /// hook-fed projection — the dual-source set-union no-op, ADR-0002).
    pub events_inserted: usize,
    /// Lenient-parse issues encountered (enumerated, not fatal).
    pub parse_issues: usize,
}

impl BridgeReport {
    /// Render the report as the stable, human-readable summary (snapshot-locked
    /// in tests; brand-free).
    #[must_use]
    pub fn render(&self) -> String {
        use std::fmt::Write as _;
        let mut s = String::new();
        let _ = writeln!(s, "bridge: {}", self.branch);
        let _ = writeln!(
            s,
            "  shards: {} seen, {} projected, {} refused",
            self.shards_seen,
            self.shards_projected,
            self.refused.len()
        );
        for r in &self.refused {
            let _ = writeln!(s, "  refused {}: {}", r.checkpoint_id, r.reason);
        }
        let _ = writeln!(
            s,
            "  events: {} derived, {} inserted",
            self.events_derived, self.events_inserted
        );
        let _ = writeln!(s, "  parse issues: {}", self.parse_issues);
        match &self.tip {
            Some(t) => {
                let _ = writeln!(s, "  watermark: {t}");
            }
            None => {
                let _ = writeln!(s, "  watermark: (unchanged)");
            }
        }
        s
    }
}

/// Reconstruct a [`CanonicalEvent`] from one archived `full.jsonl` record.
/// Unknown kind tokens are skipped (reported), never guessed.
fn event_from_record(rec: &FullEventRecord) -> Option<CanonicalEvent> {
    let kind = EventKind::from_token(&rec.kind)?;
    let payload_sha256 = rec
        .payload_sha256
        .as_deref()
        .and_then(|h| Sha256Id::parse_hex(h).ok())
        .unwrap_or_else(|| {
            // The archived record carries no explicit address: derive it from
            // the (redacted) payload bytes exactly the way ingest does.
            use sha2::{Digest as _, Sha256};
            let bytes = serde_json::to_vec(&rec.payload).unwrap_or_default();
            let mut h = Sha256::new();
            h.update(&bytes);
            let mut b = [0u8; 32];
            b.copy_from_slice(&h.finalize());
            Sha256Id::from_bytes(b)
        });
    Some(CanonicalEvent {
        schema_version: 1,
        session: rec.session.clone(),
        turn_ordinal: rec.turn_ordinal,
        event_ordinal: rec.event_ordinal,
        kind,
        trust: kind.default_trust(),
        occurred_at_ms: rec.occurred_at_ms,
        payload_sha256,
    })
}

/// Ingest everything on the checkpoint branch into the projection.
/// Crash-resumable and idempotent; refuses non-local readers and unredacted
/// shards. `default_agent` names the adapter used for usage extraction when a
/// record does not carry its own agent token.
///
/// # Errors
/// [`rift_core::RiftError`] on a reader or storage failure. A malformed shard
/// is **not** an error — it is refused and reported.
#[allow(clippy::too_many_lines)] // one linear shard-walk; splitting would obscure the strict gate
pub async fn ingest_from_substrate(
    store: &impl CanonicalStore,
    sub: &impl SubstrateReader,
    repo_id: &str,
    default_agent: &str,
    now_ms: i64,
) -> Result<BridgeReport> {
    if !sub.is_local_only() {
        return Err(RiftError::PolicyDenied(
            "the artifact bridge only ingests from a local substrate".into(),
        ));
    }
    let mut shards: Vec<CheckpointShard> = sub.checkpoints()?;
    shards.sort_by(|a, b| {
        a.ordinal
            .cmp(&b.ordinal)
            .then(a.checkpoint_id.cmp(&b.checkpoint_id))
    });

    let mut report = BridgeReport {
        branch: CHECKPOINT_BRANCH.to_owned(),
        shards_seen: shards.len(),
        ..BridgeReport::default()
    };
    let registry = AdapterRegistry::with_builtin();

    for shard in &shards {
        let id = shard.checkpoint_id.as_str();

        // STRICT LAYER (ADR-0002): a shard without a valid redaction report is
        // never projected. The lenient parser enumerates; we judge.
        let Some(report_bytes) = sub.read_artifact(id, "redaction_report.json")? else {
            report.refused.push(RefusedShard {
                checkpoint_id: id.to_owned(),
                reason: "missing redaction report".into(),
            });
            continue;
        };
        let parsed_report = parse::parse_redaction_report(&report_bytes, id);
        report.parse_issues += parsed_report.issues.len();
        if !parsed_report.value.is_valid() {
            report.refused.push(RefusedShard {
                checkpoint_id: id.to_owned(),
                reason: "invalid redaction report".into(),
            });
            continue;
        }

        // Checkpoint header → chain row + session links (idempotent).
        let metadata = match sub.read_artifact(id, "metadata.json")? {
            Some(bytes) => {
                let out = parse::parse_metadata(&bytes, id);
                report.parse_issues += out.issues.len();
                out.value
            }
            None => CheckpointMetadata::default(),
        };
        let merkle_root = metadata
            .merkle_root
            .as_deref()
            .and_then(|h| Sha256Id::parse_hex(h).ok());
        if let (Ok(ckpt_id), Some(root)) = (Sha256Id::parse_hex(id), merkle_root) {
            let prev = metadata
                .prev_hash
                .as_deref()
                .and_then(|h| Sha256Id::parse_hex(h).ok());
            record_checkpoint_if_new(
                store,
                &CheckpointRecord {
                    checkpoint_id: ckpt_id,
                    strategy: metadata
                        .strategy
                        .clone()
                        .unwrap_or_else(|| "manual-commit".into()),
                    created_at: now_ms,
                    merkle_root: root,
                    prev,
                    schema_version: 1,
                },
            )
            .await?;
        }
        for (ordinal, session) in metadata.sessions.iter().enumerate() {
            record_session_in_checkpoint(
                store,
                &SessionInCheckpointRow {
                    checkpoint_id: id.to_owned(),
                    session_id: ids::session_row_id(default_agent, session),
                    ordinal: i64::try_from(ordinal).unwrap_or(i64::MAX),
                    schema_version: 1,
                },
            )
            .await?;
        }

        // The archived canonical-event stream → the SAME projector the live
        // hook path runs (identical ids ⇒ the union is a set-union no-op).
        let Some(full_bytes) = sub.read_artifact(id, "full.jsonl")? else {
            report.shards_projected += 1;
            continue; // header-only shard: chain facts landed, no events
        };
        let full = parse::parse_full_events(&full_bytes, id);
        report.parse_issues += full.issues.len();
        for rec in &full.value {
            let Some(ev) = event_from_record(rec) else {
                report.parse_issues += 1;
                continue;
            };
            report.events_derived += 1;
            let agent_token = rec
                .agent
                .clone()
                .unwrap_or_else(|| default_agent.to_owned());
            let agent_id = AgentId::new(&agent_token);

            let key = idempotency_key(
                &agent_id,
                &ev.session,
                &rec.verb,
                ev.turn_ordinal,
                ev.event_ordinal,
                &ev.payload_sha256,
            );
            let inserted = record_captured_event(
                store,
                &CapturedEventRow {
                    idempotency_key: key.to_hex(),
                    session_id: ev.session.clone(),
                    turn_ordinal: i64::from(ev.turn_ordinal),
                    event_ordinal: i64::from(ev.event_ordinal),
                    kind: ev.kind.as_str().to_owned(),
                    trust: ev.trust.as_str().to_owned(),
                    payload_sha256: ev.payload_sha256.to_hex(),
                    occurred_at_ms: ev.occurred_at_ms,
                    schema_version: i64::from(ev.schema_version),
                },
            )
            .await?;
            if inserted {
                report.events_inserted += 1;
            }

            let facts = payload_facts(&rec.payload);
            let ctx = ProjectionCtx {
                agent: &agent_token,
                repo_id,
            };
            project_event(store, &ev, &facts, ctx).await?;
            if let Some(usage) = registry
                .get(&agent_id)
                .and_then(|a| a.usage(&rec.verb, &rec.payload))
            {
                project_usage(store, &ev, &facts, ctx, &usage, now_ms).await?;
            }
            if let Some(usage) = registry
                .get(&agent_id)
                .and_then(|a| a.main_context_usage(&rec.verb, &rec.payload))
            {
                project_main_context(store, &ev, &rec.verb, &facts, ctx, &usage).await?;
            }
        }
        report.shards_projected += 1;
    }

    // Advance the append-only watermark to the branch's high-water shard.
    // Written LAST: a crash before this line re-runs as a pure no-op.
    if let Some(tip) = shards.last().map(|s| s.checkpoint_id.clone()) {
        record_ingest_watermark(
            store,
            &IngestWatermarkRow {
                watermark_id: ids::watermark_id(CHECKPOINT_BRANCH, &tip),
                branch: CHECKPOINT_BRANCH.to_owned(),
                branch_tip: tip.clone(),
                ingested_at: now_ms,
                schema_version: 1,
            },
        )
        .await?;
        report.tip = Some(tip);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rift_recall_substrate::MemorySubstrate;
    use rift_store::SqliteStore;

    fn store() -> SqliteStore {
        let s = SqliteStore::open_in_memory().unwrap();
        s.create_canonical_schema().unwrap();
        s
    }

    fn valid_report() -> String {
        format!(
            r#"{{"total_spans":1,"rule_pack_versions":["builtin@1"],"input_sha256":"{}"}}"#,
            "c".repeat(64)
        )
    }

    fn full_jsonl() -> String {
        [
            r#"{"session_id":"native-1","turn":0,"event":0,"kind":"session_start","verb":"SessionStart","occurred_at_ms":10,"payload":{"model":"claude-sonnet-4-5"}}"#,
            r#"{"session_id":"native-1","turn":1,"event":0,"kind":"tool_call","verb":"PreToolUse","occurred_at_ms":11,"payload":{"tool_name":"Bash"}}"#,
            r#"{"session_id":"native-1","turn":1,"event":0,"kind":"turn_end","verb":"Stop","occurred_at_ms":12,"payload":{}}"#,
            r#"{"session_id":"native-1","turn":2,"event":0,"kind":"session_end","verb":"SessionEnd","occurred_at_ms":13,"payload":{}}"#,
        ]
        .join("\n")
    }

    fn shard_id(n: u8) -> String {
        format!("{n:02x}").repeat(32)
    }

    fn shard(id: &str, ordinal: u64) -> CheckpointShard {
        CheckpointShard {
            checkpoint_id: id.to_owned(),
            dialect: rift_recall_substrate::Dialect::Rift,
            prev: None,
            ordinal,
        }
    }

    fn substrate_with_shard(id: &str, redaction: Option<&str>) -> MemorySubstrate {
        let mut sub = MemorySubstrate::local().with_shard(shard(id, 0));
        if let Some(r) = redaction {
            sub = sub.with_artifact(id, "redaction_report.json", r.as_bytes());
        }
        sub.with_artifact(
            id,
            "metadata.json",
            format!(
                r#"{{"strategy":"manual-commit","merkle_root":"{}","sessions":["native-1"]}}"#,
                "d".repeat(64)
            )
            .as_bytes(),
        )
        .with_artifact(id, "full.jsonl", full_jsonl().as_bytes())
    }

    #[tokio::test]
    async fn bridge_projects_valid_shard_and_rerun_is_noop() {
        let s = store();
        let id = shard_id(0xab);
        let sub = substrate_with_shard(&id, Some(&valid_report()));

        let r1 = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 100)
            .await
            .unwrap();
        assert_eq!(r1.shards_projected, 1);
        assert!(r1.refused.is_empty());
        assert_eq!(r1.events_derived, 4);
        assert_eq!(r1.events_inserted, 4);
        // Normalized rows landed through the shared projector.
        assert_eq!(s.count("session").await.unwrap(), 1);
        assert_eq!(s.count("session_event").await.unwrap(), 2);
        assert_eq!(s.count("tool_call").await.unwrap(), 1);
        assert_eq!(s.count("checkpoint").await.unwrap(), 1);
        assert_eq!(s.count("session_in_checkpoint").await.unwrap(), 1);
        assert_eq!(s.count("ingest_watermark").await.unwrap(), 1);

        // Re-run: zero inserts anywhere (crash-resume / idempotency).
        let r2 = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 200)
            .await
            .unwrap();
        assert_eq!(r2.events_inserted, 0, "re-ingest is a set-union no-op");
        assert_eq!(s.count("captured_event").await.unwrap(), 4);
        assert_eq!(s.count("ingest_watermark").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn shard_without_valid_redaction_report_is_refused() {
        let s = store();
        let missing = shard_id(0x01);
        let invalid = shard_id(0x02);
        let sub = MemorySubstrate::local()
            .with_shard(shard(&missing, 0))
            .with_artifact(&missing, "full.jsonl", full_jsonl().as_bytes())
            .with_shard(shard(&invalid, 1))
            .with_artifact(&invalid, "redaction_report.json", b"{}")
            .with_artifact(&invalid, "full.jsonl", full_jsonl().as_bytes());

        let r = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 5)
            .await
            .unwrap();
        assert_eq!(r.shards_projected, 0);
        assert_eq!(r.refused.len(), 2);
        assert_eq!(
            s.count("captured_event").await.unwrap(),
            0,
            "a shard without a valid redaction report is NEVER projected"
        );
    }

    #[tokio::test]
    async fn non_local_reader_is_refused_outright() {
        struct RemoteReader(MemorySubstrate);
        impl SubstrateReader for RemoteReader {
            fn is_local_only(&self) -> bool {
                false
            }
            fn checkpoints(&self) -> rift_core::Result<Vec<CheckpointShard>> {
                self.0.checkpoints()
            }
            fn blob(&self, id: &str) -> rift_core::Result<Option<Vec<u8>>> {
                self.0.blob(id)
            }
            fn read_artifact(&self, c: &str, rel: &str) -> rift_core::Result<Option<Vec<u8>>> {
                self.0.read_artifact(c, rel)
            }
        }
        let s = store();
        let sub = RemoteReader(MemorySubstrate::local());
        let out = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 1).await;
        assert!(matches!(out, Err(RiftError::PolicyDenied(_))));
    }

    #[tokio::test]
    async fn bridge_report_snapshot() {
        let s = store();
        let ok = shard_id(0xab);
        let bad = shard_id(0xcd);
        // one refused shard alongside the good one
        let sub = substrate_with_shard(&ok, Some(&valid_report()))
            .with_shard(shard(&bad, 1))
            .with_artifact(&bad, "full.jsonl", full_jsonl().as_bytes());
        let r = ingest_from_substrate(&s, &sub, "repo-1", "claude-code", 42)
            .await
            .unwrap();
        insta::assert_snapshot!("bridge_report", r.render());
    }
}
